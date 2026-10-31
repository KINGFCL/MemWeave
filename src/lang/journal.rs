//! 写入日志：修改记忆的脚本先记到磁盘上再执行，进程重启后重放就能恢复。
//!
//! 每行一条 JSON，只追加不修改，所以它同时也是修改历史。写到一半崩溃时，最后一行不完整，
//! 那段脚本还没有开始执行，重放时跳过它是安全的。

use std::fs::{self, File, TryLockError};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{Mode, Session};
use crate::durable;
use crate::error::Error;
use crate::memory::Memory;
use crate::parse::Parser;
use crate::source::{SourceStore, Trusted};
use crate::timeline;

/// 知识库的写入权。写脚本是基于加载时的状态检查的，加载之后到写完日志之间不能有别的进程写入。
/// 文件关闭时自动释放，进程崩溃也一样。
pub struct WriteLock {
    _file: File,
}

impl WriteLock {
    /// 取得写入权。别的进程正在写时立即报错，不会等待：常驻的服务可能一直不释放。
    pub fn acquire(knowledge_base: &Path) -> Result<Self, Error> {
        let path = knowledge_base.join("journal.lock");
        let failed =
            |e: std::io::Error| Error::Save(format!("无法获取写入锁 {}：{e}", path.display()));
        let file = File::create(&path).map_err(failed)?;
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file }),
            Err(TryLockError::WouldBlock) => Err(Error::Save(format!(
                "另一个进程正在写 {}，可能是 memweave mcp。这张图只属于一个进程，别的进程要用，就导出一份",
                knowledge_base.display()
            ))),
            Err(TryLockError::Error(e)) => Err(failed(e)),
        }
    }
}

/// 读出知识库的当前状态：先解析快照，再重放日志。只调用 [`Memory::load`] 得到的是最初的状态。
///
/// 原文缺失或损坏时照常加载，读到它时才报错。
pub fn load_current(
    parser: &dyn Parser,
    sources: &dyn SourceStore,
    knowledge_base: &Path,
) -> Result<Memory, Error> {
    // 少一份原文不影响别的记忆，读到它时才报错，check 也会列出来。
    let mut memory = Memory::load(parser, &Trusted(sources), knowledge_base)?;
    let events = timeline::read(knowledge_base);
    if events.skipped > 0 {
        eprintln!("警告：时间线有 {} 行损坏，已跳过", events.skipped);
    }
    for event in events.events {
        memory.record(event);
    }
    Journal::new(knowledge_base).replay(&mut memory, sources)?;
    Ok(memory)
}

/// 日志里的脚本用第几版语言写的。不兼容地改语言时要加一。旧版本遇到更新的日志会拒绝，不会读错。
pub const LANGUAGE_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
pub(crate) struct Entry {
    pub(crate) script: String,
    /// 这段脚本执行的时间。旧日志没有这一项。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) at: Option<u64>,
    /// 语言版本。没有这一项的是第 1 版。
    #[serde(default, rename = "v", skip_serializing_if = "Option::is_none")]
    version: Option<u32>,
}

/// 知识库目录下的 `journal.jsonl`。
pub struct Journal {
    path: PathBuf,
}

impl Journal {
    pub fn new(knowledge_base: &Path) -> Self {
        Self {
            path: knowledge_base.join("journal.jsonl"),
        }
    }

    /// 追加一段脚本，并确保已经落到磁盘上。
    ///
    /// 上次写到一半留下的半行会先被去掉。这次写失败时，已经写进去的部分也会去掉，所以日志里永远
    /// 只有完整的行。
    pub fn append(&self, script: &str, at: Option<u64>) -> Result<(), Error> {
        let mut line = serde_json::to_string(&Entry {
            script: script.to_owned(),
            at,
            version: Some(LANGUAGE_VERSION),
        })
        .expect("字符串总是可以序列化");
        line.push('\n');

        let failed = |e: std::io::Error| Error::Save(format!("{}：{e}", self.path.display()));
        let created = !self.path.exists();
        let mut file = durable::private_options()
            .create(true)
            .read(true)
            .append(true)
            .open(&self.path)
            .map_err(failed)?;
        drop_incomplete_tail(&mut file).map_err(failed)?;
        let before = file.metadata().map_err(failed)?.len();
        if let Err(error) = file
            .write_all(line.as_bytes())
            .and_then(|()| file.sync_all())
        {
            // 比如磁盘满了。留着半行的话，下一次写入会接在它后面，日志就坏了。
            let _ = file.set_len(before);
            let _ = file.sync_all();
            return Err(failed(error));
        }
        if created {
            durable::sync_dir(self.path.parent().unwrap_or(Path::new(""))).map_err(failed)?;
        }
        Ok(())
    }

    /// 把日志里的脚本依次重放到 `memory` 上。没有日志时什么也不做。
    ///
    /// 最后没有写完的一行会被跳过：崩溃发生在追加日志时，这段脚本没有执行过，也没有告诉过任何人
    /// 写入成功。这里按字节处理，因为半行可能切在一个汉字中间。
    pub fn replay(&self, memory: &mut Memory, sources: &dyn SourceStore) -> Result<(), Error> {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(Error::Save(format!("{}：{e}", self.path.display()))),
        };
        for (index, line) in complete_lines(&bytes).enumerate() {
            let entry = parse_entry(line).map_err(|e| {
                Error::Save(format!(
                    "{} 第 {} 行损坏：{e}",
                    self.path.display(),
                    index + 1
                ))
            })?;
            let output =
                Session::new(memory, sources, Mode::ReadWrite).replay(&entry.script, entry.at);
            if !output.ok {
                return Err(Error::Save(format!(
                    "{} 第 {} 行重放失败：\n{}",
                    self.path.display(),
                    index + 1,
                    output.text
                )));
            }
        }
        Ok(())
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

/// 日志里完整的行，不含最后没写完的部分。
pub(crate) fn complete_lines(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    let complete = match bytes.iter().rposition(|byte| *byte == b'\n') {
        Some(end) => &bytes[..end],
        None => &[],
    };
    complete
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
}

pub(crate) fn parse_entry(line: &[u8]) -> Result<Entry, String> {
    let entry: Entry = serde_json::from_slice(line).map_err(|e| e.to_string())?;
    if entry.version.unwrap_or(LANGUAGE_VERSION) > LANGUAGE_VERSION {
        return Err(format!(
            "这段脚本由更新的版本写入，当前版本只支持第 {LANGUAGE_VERSION} 版语言"
        ));
    }
    Ok(entry)
}

/// 去掉文件末尾没写完的那一行。文件很大时也只读末尾。
fn drop_incomplete_tail(file: &mut File) -> std::io::Result<()> {
    const CHUNK: u64 = 64 * 1024;
    let length = file.metadata()?.len();
    let mut end = length;
    let mut buffer = vec![0; CHUNK as usize];
    while end > 0 {
        let start = end.saturating_sub(CHUNK);
        let size = usize::try_from(end - start).expect("不超过 CHUNK");
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut buffer[..size])?;
        if let Some(position) = buffer[..size].iter().rposition(|byte| *byte == b'\n') {
            let keep = start + position as u64 + 1;
            if keep < length {
                file.set_len(keep)?;
                file.sync_all()?;
            }
            return Ok(());
        }
        end = start;
    }
    if length > 0 {
        file.set_len(0)?;
        file.sync_all()?;
    }
    Ok(())
}

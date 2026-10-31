//! 写入日志：修改记忆的脚本先记到磁盘上再执行，进程重启后重放就能恢复。
//!
//! 每行一条 JSON，只追加不修改，所以它同时也是修改历史。写到一半崩溃时，最后一行不完整，
//! 那段脚本还没有开始执行，重放时跳过它是安全的。

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{Mode, Session};
use crate::error::Error;
use crate::memory::Memory;
use crate::parse::Parser;
use crate::source::SourceStore;

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
pub fn load_current(
    parser: &dyn Parser,
    sources: &dyn SourceStore,
    knowledge_base: &Path,
) -> Result<Memory, Error> {
    let mut memory = Memory::load(parser, sources, knowledge_base)?;
    Journal::new(knowledge_base).replay(&mut memory, sources)?;
    Ok(memory)
}

#[derive(Serialize, Deserialize)]
struct Entry {
    script: String,
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
    pub fn append(&self, script: &str) -> Result<(), Error> {
        let mut line = serde_json::to_string(&Entry {
            script: script.to_owned(),
        })
        .expect("字符串总是可以序列化");
        line.push('\n');

        let failed = |e: std::io::Error| Error::Save(format!("{}：{e}", self.path.display()));
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(failed)?;
        file.write_all(line.as_bytes()).map_err(failed)?;
        file.sync_all().map_err(failed)
    }

    /// 把日志里的脚本依次重放到 `memory` 上。没有日志时什么也不做。
    pub fn replay(&self, memory: &mut Memory, sources: &dyn SourceStore) -> Result<(), Error> {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(Error::Save(format!("{}：{e}", self.path.display()))),
        };
        let complete = text.ends_with('\n');
        let lines: Vec<&str> = text.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            let entry: Entry = match serde_json::from_str(line) {
                Ok(entry) => entry,
                // 最后一行没写完：崩溃发生在追加日志时，这段脚本没有执行过。
                Err(_) if index + 1 == lines.len() && !complete => return Ok(()),
                Err(e) => {
                    return Err(Error::Save(format!(
                        "{} 第 {} 行损坏：{e}",
                        self.path.display(),
                        index + 1
                    )));
                }
            };
            let output = Session::new(memory, sources, Mode::ReadWrite).replay(&entry.script);
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
}

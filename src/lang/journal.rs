//! 写入日志：保存草稿时把脚本记到磁盘上，进程重启后重放就能恢复。
//!
//! 每行一条 JSON，只追加不修改，所以它同时也是修改历史。一次保存只写一行，写到一半崩溃时最后一行
//! 不完整，这次保存没有告诉过任何人成功，重放时跳过它是安全的。
//!
//! 没保存的脚本记在旁边的 `draft.jsonl` 里，不等落盘：进程退出后能接着用，断电丢了也只是没保存的
//! 修改。第一行记着开始时日志的长度，日志变长了就说明这份草稿已经保存过，只是没来得及删。

use std::fs::{self, File, TryLockError};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{Mode, Session};
use crate::durable;
use crate::error::Error;
use crate::memory::{DraftScript, Memory};
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
///
/// 第 2 版：读语句要写 `revision` 或 `origin`，一行里可以有多段脚本。第 1 版的日志还能读。
pub const LANGUAGE_VERSION: u32 = 2;

#[derive(Serialize, Deserialize)]
pub(crate) struct Entry {
    /// 只有一段脚本时写在这里。多段时是空的，但也要写出来：旧版本要求有这一项，缺了它会在看版本号
    /// 之前就解析失败，把新日志说成损坏，还会建议用户运行 doctor --fix。
    #[serde(default)]
    script: String,
    /// 这段脚本执行的时间。旧日志没有这一项。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    at: Option<u64>,
    /// 一次保存了多段脚本时写在这里，要么全部生效，要么都不生效。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    scripts: Vec<DraftScript>,
    /// 语言版本。没有这一项的是第 1 版。
    #[serde(default, rename = "v", skip_serializing_if = "Option::is_none")]
    version: Option<u32>,
}

impl Entry {
    fn new(scripts: &[DraftScript]) -> Self {
        let mut entry = Self {
            script: String::new(),
            at: None,
            scripts: Vec::new(),
            version: Some(LANGUAGE_VERSION),
        };
        if let [only] = scripts {
            entry.script.clone_from(&only.script);
            entry.at = only.at;
        } else {
            entry.scripts = scripts.to_vec();
        }
        entry
    }

    /// 这一行里的所有脚本，按执行的顺序。
    pub(crate) fn scripts(self) -> Vec<DraftScript> {
        if self.scripts.is_empty() {
            vec![DraftScript {
                script: self.script,
                at: self.at,
            }]
        } else {
            self.scripts
        }
    }
}

/// `draft.jsonl` 的第一行。
#[derive(Serialize, Deserialize)]
struct DraftHeader {
    /// 开始这份草稿时，日志里完整的行一共多少字节。
    base: u64,
}

/// 知识库目录下的 `journal.jsonl`。
pub struct Journal {
    path: PathBuf,
    draft_path: PathBuf,
}

impl Journal {
    pub fn new(knowledge_base: &Path) -> Self {
        Self {
            path: knowledge_base.join("journal.jsonl"),
            draft_path: knowledge_base.join("draft.jsonl"),
        }
    }

    /// 追加一段脚本，并确保已经落到磁盘上。
    pub fn append(&self, script: &str, at: Option<u64>) -> Result<(), Error> {
        self.append_scripts(&[DraftScript {
            script: script.to_owned(),
            at,
        }])
    }

    /// 把这些脚本写成一行，并确保已经落到磁盘上。崩溃时要么整行都在，要么都不算数。
    ///
    /// 上次写到一半留下的半行会先被去掉。这次写失败时，已经写进去的部分也会去掉，所以日志里永远
    /// 只有完整的行。
    pub fn append_scripts(&self, scripts: &[DraftScript]) -> Result<(), Error> {
        let mut line = serde_json::to_string(&Entry::new(scripts)).expect("字符串总是可以序列化");
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
            for part in entry.scripts() {
                let output =
                    Session::new(memory, sources, Mode::ReadWrite).replay(&part.script, part.at);
                if !output.ok {
                    return Err(Error::Save(format!(
                        "{} 第 {} 行重放失败：\n{}",
                        self.path.display(),
                        index + 1,
                        output.text
                    )));
                }
            }
        }
        Ok(())
    }

    /// 记下一段没保存的脚本，不等落盘。
    pub fn append_draft(&self, script: &str, at: Option<u64>) -> Result<(), Error> {
        let failed = |e: std::io::Error| Error::Save(format!("{}：{e}", self.draft_path.display()));
        let mut file = durable::private_options()
            .create(true)
            .read(true)
            .append(true)
            .open(&self.draft_path)
            .map_err(failed)?;
        drop_incomplete_tail(&mut file).map_err(failed)?;
        let before = file.metadata().map_err(failed)?.len();
        let mut bytes = Vec::new();
        if before == 0 {
            let base = complete_length(&self.path).map_err(failed)?;
            bytes = serde_json::to_vec(&DraftHeader { base }).expect("数字总是可以序列化");
            bytes.push(b'\n');
        }
        serde_json::to_writer(
            &mut bytes,
            &Entry::new(&[DraftScript {
                script: script.to_owned(),
                at,
            }]),
        )
        .expect("字符串总是可以序列化");
        bytes.push(b'\n');
        file.write_all(&bytes).map_err(|error| {
            // 比如磁盘满了。这段脚本不会执行，草稿里不能留下它的半行。
            let _ = file.set_len(before);
            failed(error)
        })
    }

    /// 删除草稿文件，没有时什么也不做。
    pub fn clear_draft(&self) -> Result<(), Error> {
        match fs::remove_file(&self.draft_path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
            Err(e) => Err(Error::Save(format!("{}：{e}", self.draft_path.display()))),
        }
    }

    /// 草稿文件里有多少段还没保存的脚本。不修改任何文件。
    pub fn pending_draft(&self) -> usize {
        match self.read_draft() {
            DraftState::Pending(scripts) => scripts.len(),
            DraftState::Damaged(_) => 1,
            DraftState::Nothing | DraftState::Saved => 0,
        }
    }

    /// 有未保存的修改时报错。压缩和清除会换掉整个目录，不能带着草稿做。
    pub fn refuse_unsaved(&self, knowledge_base: &Path) -> Result<(), Error> {
        match self.pending_draft() {
            0 => Ok(()),
            count => Err(Error::Save(format!(
                "有 {count} 段未保存的修改，先处理它们：memweave run --write {0} save，或者 memweave run --write {0} discard",
                knowledge_base.display()
            ))),
        }
    }

    /// 接着上次没保存的草稿。`writable` 时会清理用不上的草稿文件，只读的进程不碰它们。
    ///
    /// 草稿读不出来或者接不上时不影响打开知识库：已保存的记忆是完好的。文件改名留着，内容不会丢。
    pub fn restore_draft(&self, memory: &mut Memory, sources: &dyn SourceStore, writable: bool) {
        let scripts = match self.read_draft() {
            DraftState::Nothing => return,
            DraftState::Saved => {
                if writable {
                    let _ = self.clear_draft();
                }
                return;
            }
            DraftState::Damaged(reason) => {
                self.set_aside(&reason, writable);
                return;
            }
            DraftState::Pending(scripts) => scripts,
        };
        for part in scripts {
            memory.begin_draft();
            let output =
                Session::new(memory, sources, Mode::ReadWrite).replay(&part.script, part.at);
            if !output.ok {
                memory.abort_draft();
                self.set_aside(&output.text, writable);
                return;
            }
            memory.note_draft_script(part);
        }
    }

    fn set_aside(&self, reason: &str, writable: bool) {
        let display = self.draft_path.display();
        eprintln!("警告：草稿 {display} 接不上，已保存的记忆不受影响：{reason}");
        if writable {
            let aside = self
                .draft_path
                .with_extension(format!("damaged-{}.jsonl", timeline::now().unwrap_or(0)));
            match fs::rename(&self.draft_path, &aside) {
                Ok(()) => eprintln!("草稿改名为 {}", aside.display()),
                Err(error) => eprintln!("草稿没能改名：{error}"),
            }
        }
    }

    fn read_draft(&self) -> DraftState {
        let bytes = match fs::read(&self.draft_path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == ErrorKind::NotFound => return DraftState::Nothing,
            Err(e) => return DraftState::Damaged(e.to_string()),
        };
        let mut lines = complete_lines(&bytes);
        let Some(first) = lines.next() else {
            return DraftState::Nothing;
        };
        let header: DraftHeader = match serde_json::from_slice(first) {
            Ok(header) => header,
            Err(e) => return DraftState::Damaged(format!("第一行：{e}")),
        };
        match complete_length(&self.path) {
            Ok(length) if length != header.base => return DraftState::Saved,
            Ok(_) => {}
            Err(e) => return DraftState::Damaged(e.to_string()),
        }
        let mut scripts = Vec::new();
        for line in lines {
            match parse_entry(line) {
                Ok(entry) => scripts.extend(entry.scripts()),
                Err(e) => return DraftState::Damaged(e),
            }
        }
        if scripts.is_empty() {
            DraftState::Saved
        } else {
            DraftState::Pending(scripts)
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

/// 草稿文件的状态。
enum DraftState {
    /// 没有草稿文件。
    Nothing,
    /// 已经保存过，只是文件没来得及删。
    Saved,
    Pending(Vec<DraftScript>),
    Damaged(String),
}

/// 文件里完整的行一共多少字节，不存在时是 0。文件很大时也只读末尾。
fn complete_length(path: &Path) -> std::io::Result<u64> {
    const CHUNK: u64 = 64 * 1024;
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    let mut end = file.metadata()?.len();
    let mut buffer = vec![0; CHUNK as usize];
    while end > 0 {
        let start = end.saturating_sub(CHUNK);
        let size = usize::try_from(end - start).expect("不超过 CHUNK");
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut buffer[..size])?;
        if let Some(position) = buffer[..size].iter().rposition(|byte| *byte == b'\n') {
            return Ok(start + position as u64 + 1);
        }
        end = start;
    }
    Ok(0)
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

use std::cell::RefCell;
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{ErrorKind, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::{SourceStore, check_range, decode, hash_part, source_id};
use crate::durable;
use crate::error::Error;
use crate::knowledge::{Locator, SourceId};

/// 知识库目录下 `sources/<哈希>` 里的来源。
///
/// 读取时只读需要的范围，每份来源在一个进程里只校验一次哈希：引用很多段的大文件，读一段不用再把
/// 整个文件算一遍。所以进程运行期间文件被改动，要到下次启动才能发现。
pub struct FileSources {
    dir: PathBuf,
    verified: RefCell<HashSet<SourceId>>,
}

impl FileSources {
    /// `knowledge_base` 是知识库目录。
    pub fn new(knowledge_base: &Path) -> Self {
        Self {
            dir: knowledge_base.join("sources"),
            verified: RefCell::new(HashSet::new()),
        }
    }

    fn path(&self, id: &str) -> Result<PathBuf, Error> {
        let hash = hash_part(id).ok_or_else(|| Error::Source(format!("来源地址 {id:?} 不合法")))?;
        Ok(self.dir.join(hash))
    }

    fn open(&self, id: &str) -> Result<File, Error> {
        File::open(self.path(id)?).map_err(|e| Error::Source(format!("来源 {id}：{e}")))
    }

    /// 确认内容和地址一致，每份来源只算一次。边读边算，大文件也不用全读进内存。
    fn verify_once(&self, id: &str) -> Result<(), Error> {
        if self.verified.borrow().contains(id) {
            return Ok(());
        }
        let failed = |e: std::io::Error| Error::Source(format!("来源 {id}：{e}"));
        let mut file = self.open(id)?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0; 1024 * 1024];
        loop {
            let size = file.read(&mut buffer).map_err(failed)?;
            if size == 0 {
                break;
            }
            hasher.update(&buffer[..size]);
        }
        let hex: String = hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        if hash_part(id) != Some(hex.as_str()) {
            return Err(Error::Source(format!("来源 {id} 的内容已被改动")));
        }
        self.verified.borrow_mut().insert(id.to_owned());
        Ok(())
    }

    fn read_range(&self, id: &str, start: u64, end: u64) -> Result<Vec<u8>, Error> {
        let failed = |e: std::io::Error| Error::Source(format!("来源 {id}：{e}"));
        let mut file = self.open(id)?;
        file.seek(SeekFrom::Start(start)).map_err(failed)?;
        let mut bytes = Vec::new();
        file.take(end.saturating_sub(start))
            .read_to_end(&mut bytes)
            .map_err(failed)?;
        Ok(bytes)
    }

    /// 写入来源，不等落盘。导出很多文件时用，之后统一落盘。
    pub(crate) fn put_without_sync(&self, bytes: &[u8]) -> Result<SourceId, Error> {
        let id = source_id(bytes);
        let path = self.path(&id)?;
        durable::create_private_dir(&self.dir)
            .and_then(|()| durable::write_private(&path, bytes))
            .map_err(|e| Error::Save(format!("{}：{e}", path.display())))?;
        Ok(id)
    }
}

impl SourceStore for FileSources {
    fn size(&self, id: &str) -> Result<Option<u64>, Error> {
        match fs::metadata(self.path(id)?) {
            Ok(metadata) => Ok(Some(metadata.len())),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(Error::Source(format!("来源 {id}：{e}"))),
        }
    }

    fn read(&self, id: &str, locator: &Locator) -> Result<String, Error> {
        self.verify_once(id)?;
        let total = self
            .size(id)?
            .ok_or_else(|| Error::Source(format!("来源 {id} 不存在")))?;
        let (start, end) = check_range(total, id, locator)?;
        decode(
            self.read_range(id, start as u64, end as u64)?,
            id,
            start,
            end,
        )
    }

    /// 总是重新校验：导出靠它保证带走的内容是对的。
    fn read_all(&self, id: &str) -> Result<Vec<u8>, Error> {
        let bytes =
            fs::read(self.path(id)?).map_err(|e| Error::Source(format!("来源 {id}：{e}")))?;
        if source_id(&bytes) != id {
            return Err(Error::Source(format!("来源 {id} 的内容已被改动")));
        }
        Ok(bytes)
    }

    fn read_bytes(&self, id: &str, start: u64, end: u64) -> Result<Vec<u8>, Error> {
        self.verify_once(id)?;
        self.read_range(id, start, end)
    }

    fn put(&self, bytes: &[u8]) -> Result<SourceId, Error> {
        let id = source_id(bytes);
        let path = self.path(&id)?;
        // 文件在但内容不对时不能跳过：断电可能留下空文件，这时写入就是修复。
        if fs::read(&path).is_ok_and(|existing| existing == bytes) {
            return Ok(id);
        }
        let failed = |e: std::io::Error| Error::Save(format!("{}：{e}", path.display()));
        let created = !self.dir.exists();
        durable::create_private_dir(&self.dir).map_err(failed)?;
        durable::write_atomic(&path, bytes).map_err(failed)?;
        if created && let Some(parent) = self.dir.parent() {
            durable::sync_dir(parent).map_err(failed)?;
        }
        self.verified.borrow_mut().remove(&id);
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    const WHOLE: Locator = Locator::TextRange { start: 0, end: 6 };

    #[test]
    fn stores_and_reads_back() {
        let dir = TempDir::new().unwrap();
        let sources = FileSources::new(&dir.path().join("新知识库"));

        let id = sources.put("一二".as_bytes()).unwrap();

        assert_eq!(sources.size(&id).unwrap(), Some(6));
        assert_eq!(sources.read(&id, &WHOLE).unwrap(), "一二");
        // 同样的内容写两次只有一份，也不留临时文件。
        assert_eq!(sources.put("一二".as_bytes()).unwrap(), id);
        assert_eq!(fs::read_dir(&sources.dir).unwrap().count(), 1);
    }

    #[test]
    fn detects_tampering() {
        let dir = TempDir::new().unwrap();
        let sources = FileSources::new(dir.path());
        let id = sources.put("一二".as_bytes()).unwrap();
        fs::write(sources.path(&id).unwrap(), "三四").unwrap();

        // 大小没变，只有读取时校验哈希才能发现。
        assert_eq!(sources.size(&id).unwrap(), Some(6));
        let message = sources.read(&id, &WHOLE).unwrap_err().to_string();
        assert!(message.contains("已被改动"), "{message}");
    }

    #[test]
    fn missing_source_is_none_when_sizing_and_an_error_when_reading() {
        let dir = TempDir::new().unwrap();
        let sources = FileSources::new(dir.path());
        let id = source_id(b"x");

        assert_eq!(sources.size(&id).unwrap(), None);
        assert!(matches!(sources.read(&id, &WHOLE), Err(Error::Source(_))));
    }

    #[test]
    fn rejects_ids_that_could_escape_the_directory() {
        let dir = TempDir::new().unwrap();
        let sources = FileSources::new(dir.path());

        for id in ["../secret", "sha256:../../secret", "script.md"] {
            assert!(matches!(sources.size(id), Err(Error::Source(_))), "{id}");
            assert!(matches!(sources.read(id, &WHOLE), Err(Error::Source(_))));
        }
    }

    #[test]
    fn writing_repairs_a_damaged_file() {
        let dir = TempDir::new().unwrap();
        let sources = FileSources::new(dir.path());
        let id = sources.put("一二".as_bytes()).unwrap();

        // 断电可能留下空文件，或者长度对但内容错的文件。
        for damaged in ["", "三四"] {
            fs::write(sources.path(&id).unwrap(), damaged).unwrap();
            assert_eq!(sources.put("一二".as_bytes()).unwrap(), id);
            assert_eq!(sources.read(&id, &WHOLE).unwrap(), "一二");
        }
    }

    #[test]
    fn the_hash_is_checked_once_per_process() {
        let dir = TempDir::new().unwrap();
        let sources = FileSources::new(dir.path());
        let id = sources.put("一二".as_bytes()).unwrap();
        assert_eq!(sources.read(&id, &WHOLE).unwrap(), "一二");

        // 校验过之后改动文件，这个进程发现不了，下一个进程能发现。
        fs::write(sources.path(&id).unwrap(), "三四").unwrap();
        assert_eq!(sources.read(&id, &WHOLE).unwrap(), "三四");
        let next = FileSources::new(dir.path());
        assert!(next.read(&id, &WHOLE).is_err());
        // 读取全部内容总是重新校验，导出不会带走坏内容。
        assert!(sources.read_all(&id).is_err());
    }

    #[test]
    fn a_failed_check_is_not_remembered() {
        let dir = TempDir::new().unwrap();
        let sources = FileSources::new(dir.path());
        let id = sources.put("一二".as_bytes()).unwrap();
        fs::write(sources.path(&id).unwrap(), "三四").unwrap();
        assert!(sources.read(&id, &WHOLE).is_err());

        sources.put("一二".as_bytes()).unwrap();

        assert_eq!(sources.read(&id, &WHOLE).unwrap(), "一二");
    }

    #[test]
    fn reads_only_the_requested_range() {
        let dir = TempDir::new().unwrap();
        let sources = FileSources::new(dir.path());
        let id = sources.put("一二三".as_bytes()).unwrap();

        let locator = |start, end| Locator::TextRange { start, end };
        assert_eq!(sources.read(&id, &locator(3, 6)).unwrap(), "二");
        for bad in [locator(3, 3), locator(0, 10), locator(1, 3)] {
            assert!(sources.read(&id, &bad).is_err(), "{bad:?}");
        }
        assert_eq!(sources.read_bytes(&id, 6, 100).unwrap(), "三".as_bytes());
        assert!(sources.read_bytes(&id, 100, 200).unwrap().is_empty());
    }

    #[test]
    fn missing_source_cannot_be_read() {
        let dir = TempDir::new().unwrap();
        let sources = FileSources::new(dir.path());

        assert!(sources.read_bytes(&source_id(b"x"), 0, 1).is_err());
    }
}

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use super::{SourceStore, extract, hash_part, source_id};
use crate::error::Error;
use crate::knowledge::{Locator, SourceId};

/// 知识库目录下 `sources/<哈希>` 里的来源。
pub struct FileSources {
    dir: PathBuf,
}

impl FileSources {
    /// `knowledge_base` 是知识库目录。
    pub fn new(knowledge_base: &Path) -> Self {
        Self {
            dir: knowledge_base.join("sources"),
        }
    }

    fn path(&self, id: &str) -> Result<PathBuf, Error> {
        let hash = hash_part(id).ok_or_else(|| Error::Source(format!("来源地址 {id:?} 不合法")))?;
        Ok(self.dir.join(hash))
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
        extract(&self.read_all(id)?, id, locator)
    }

    fn read_all(&self, id: &str) -> Result<Vec<u8>, Error> {
        let bytes =
            fs::read(self.path(id)?).map_err(|e| Error::Source(format!("来源 {id}：{e}")))?;
        if source_id(&bytes) != id {
            return Err(Error::Source(format!("来源 {id} 的内容已被改动")));
        }
        Ok(bytes)
    }

    fn put(&self, bytes: &[u8]) -> Result<SourceId, Error> {
        let id = source_id(bytes);
        let path = self.path(&id)?;
        if path.exists() {
            return Ok(id);
        }
        fs::create_dir_all(&self.dir)
            .map_err(|e| Error::Save(format!("{}：{e}", self.dir.display())))?;
        // 先写临时文件再改名：中途失败不会留下内容和地址不符的来源。
        let temporary = path.with_extension("tmp");
        fs::write(&temporary, bytes)
            .and_then(|()| fs::rename(&temporary, &path))
            .map_err(|e| Error::Save(format!("{}：{e}", path.display())))?;
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
}

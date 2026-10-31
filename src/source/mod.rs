//! 原始数据：留在固态硬盘上、不可变的来源。
//!
//! 记忆只保存描述和证据索引，模型需要从底层确认时才按索引读取原文。来源按内容哈希寻址，
//! 永不修改，所以索引不会失效，内容被改动也能发现。存储方式实现 [`SourceStore`] 接口。

pub mod file;
pub mod in_memory;

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::error::Error;
use crate::knowledge::{Locator, NodeId, SourceId};

const ID_PREFIX: &str = "sha256:";
const HASH_HEX_LEN: usize = 64;

/// 按索引读取来源的接口。
pub trait SourceStore {
    /// 来源的字节数。来源不存在时返回 `None`。
    ///
    /// 校验知识库时对每条证据都会调用，实现不应该读取来源的全部内容。
    fn size(&self, id: &str) -> Result<Option<u64>, Error>;

    /// 读取来源中 `locator` 指向的内容。实现必须确认内容与地址里的哈希一致。
    fn read(&self, id: &str, locator: &Locator) -> Result<String, Error>;

    /// 读取来源的完整内容。实现必须确认内容与地址里的哈希一致。
    fn read_all(&self, id: &str) -> Result<Vec<u8>, Error>;

    /// 读取来源里 `start..end` 的字节，`end` 超出时只读到末尾。实现必须确认内容和地址里的哈希一致。
    fn read_bytes(&self, id: &str, start: u64, end: u64) -> Result<Vec<u8>, Error> {
        let bytes = self.read_all(id)?;
        let total = bytes.len() as u64;
        let (start, end) = (start.min(total) as usize, end.min(total) as usize);
        Ok(bytes[start..end.max(start)].to_vec())
    }

    /// 写入一份新来源，返回它的地址。内容相同的来源只保存一份。地址由内容算出，所以文件内容不对时，
    /// 比如断电后留下的空文件，写入就是修复。
    fn put(&self, _bytes: &[u8]) -> Result<SourceId, Error> {
        Err(Error::Source("这里不能写入新来源".to_owned()))
    }
}

/// 不检查原文的来源：`size` 总是说够大，`read` 不读文件。
///
/// 加载和重放时用它。这些内容当时已经检查过了，原文后来丢了，不能让整份记忆都打不开。
pub(crate) struct Trusted<'a>(pub(crate) &'a dyn SourceStore);

impl SourceStore for Trusted<'_> {
    fn size(&self, _id: &str) -> Result<Option<u64>, Error> {
        Ok(Some(u64::MAX))
    }

    fn read(&self, _id: &str, _locator: &Locator) -> Result<String, Error> {
        Ok(String::new())
    }

    fn read_all(&self, id: &str) -> Result<Vec<u8>, Error> {
        self.0.read_all(id)
    }

    fn put(&self, bytes: &[u8]) -> Result<SourceId, Error> {
        self.0.put(bytes)
    }
}

/// 读取出来的一条证据。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LoadedEvidence {
    Source {
        source: SourceId,
        locator: Locator,
        text: String,
    },
    Node {
        id: NodeId,
    },
}

/// 计算内容的来源地址。
pub fn source_id(bytes: &[u8]) -> SourceId {
    let hex: String = Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("{ID_PREFIX}{hex}")
}

/// 来源地址中的哈希部分。格式不对时返回 `None`。
fn hash_part(id: &str) -> Option<&str> {
    id.strip_prefix(ID_PREFIX).filter(|hash| {
        hash.len() == HASH_HEX_LEN
            && hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

pub fn is_valid_source_id(id: &str) -> bool {
    hash_part(id).is_some()
}

/// 确认范围有效，返回起始和结束。
fn check_range(total: u64, id: &str, locator: &Locator) -> Result<(usize, usize), Error> {
    let Locator::TextRange { start, end } = *locator;
    if start < end && end as u64 <= total {
        Ok((start, end))
    } else {
        Err(Error::Source(format!(
            "来源 {id} 的范围 {start}..{end} 无效（共 {total} 字节）"
        )))
    }
}

fn decode(bytes: Vec<u8>, id: &str, start: usize, end: usize) -> Result<String, Error> {
    String::from_utf8(bytes)
        .map_err(|_| Error::Source(format!("来源 {id} 的范围 {start}..{end} 不是有效的文本")))
}

/// 从来源的完整内容中取出 `locator` 指向的部分。
fn extract(bytes: &[u8], id: &str, locator: &Locator) -> Result<String, Error> {
    let (start, end) = check_range(bytes.len() as u64, id, locator)?;
    decode(bytes[start..end].to_vec(), id, start, end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_id_is_the_content_hash() {
        // "abc" 的 SHA-256 是公开的标准测试向量。
        assert_eq!(
            source_id(b"abc"),
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(is_valid_source_id(&source_id(b"abc")));
    }

    #[test]
    fn rejects_malformed_ids() {
        let hash = "a".repeat(64);
        for id in [
            String::new(),
            hash.clone(),
            format!("md5:{hash}"),
            format!("sha256:{}", "a".repeat(63)),
            format!("sha256:{}", "A".repeat(64)),
            format!("sha256:../{}", "a".repeat(61)),
        ] {
            assert!(!is_valid_source_id(&id), "{id}");
        }
    }

    #[test]
    fn extracts_text_ranges() {
        let bytes = "一二三".as_bytes();
        let locator = |start, end| Locator::TextRange { start, end };

        assert_eq!(extract(bytes, "s", &locator(3, 6)).unwrap(), "二");
        for bad in [locator(3, 3), locator(6, 3), locator(0, 10), locator(1, 3)] {
            assert!(matches!(extract(bytes, "s", &bad), Err(Error::Source(_))));
        }
    }
}

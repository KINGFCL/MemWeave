use std::collections::HashMap;

use super::{SourceStore, extract, source_id};
use crate::error::Error;
use crate::knowledge::{Locator, SourceId};

/// 暂存在内存中的来源：Weaver 生成的来源在写入固态硬盘之前放在这里。
#[derive(Debug, Clone, Default)]
pub struct InMemorySources {
    sources: HashMap<SourceId, Vec<u8>>,
}

impl InMemorySources {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, bytes: Vec<u8>) -> SourceId {
        let id = source_id(&bytes);
        self.sources.insert(id.clone(), bytes);
        id
    }

    /// 所有来源的内容，顺序不固定。
    pub fn contents(&self) -> impl Iterator<Item = &[u8]> {
        self.sources.values().map(Vec::as_slice)
    }
}

impl SourceStore for InMemorySources {
    fn size(&self, id: &str) -> Result<Option<u64>, Error> {
        Ok(self.sources.get(id).map(|bytes| bytes.len() as u64))
    }

    fn read(&self, id: &str, locator: &Locator) -> Result<String, Error> {
        let bytes = self
            .sources
            .get(id)
            .ok_or_else(|| Error::Source(format!("来源 {id} 不存在")))?;
        extract(bytes, id, locator)
    }
}

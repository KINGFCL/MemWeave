use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;

use serde::de::DeserializeOwned;

use crate::error::Error;
use crate::knowledge::{Catalog, Node};

/// 知识库的读取接口。
pub trait Store {
    fn catalog(&self) -> Result<Catalog, Error>;

    /// 节点不存在时返回 `None`，由调用方结合上下文报错。
    fn node(&self, id: &str) -> Result<Option<Node>, Error>;
}

/// 基于目录的知识库：`catalog.json` 加 `nodes/<id>.json`。
pub struct FsStore {
    root: PathBuf,
}

impl FsStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

impl Store for FsStore {
    fn catalog(&self) -> Result<Catalog, Error> {
        let path = self.root.join("catalog.json");
        let text = fs::read_to_string(&path)
            .map_err(|e| Error::Store(format!("{}：{e}", path.display())))?;
        parse(&path, &text)
    }

    fn node(&self, id: &str) -> Result<Option<Node>, Error> {
        // 地址会拼进文件路径，不能让它逃出 nodes 目录。
        if id.is_empty() || id == "." || id == ".." || id.contains(['/', '\\', '\0']) {
            return Err(Error::Store(format!("非法的节点地址：{id:?}")));
        }
        let path = self.root.join("nodes").join(format!("{id}.json"));
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(Error::Store(format!("{}：{e}", path.display()))),
        };
        let node: Node = parse(&path, &text)?;
        if node.id != id {
            return Err(Error::Store(format!(
                "{} 里的 id 是 {:?}，与地址 {id:?} 不一致",
                path.display(),
                node.id
            )));
        }
        Ok(Some(node))
    }
}

fn parse<T: DeserializeOwned>(path: &std::path::Path, text: &str) -> Result<T, Error> {
    serde_json::from_str(text).map_err(|e| Error::Store(format!("{}：{e}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn demo() -> FsStore {
        FsStore::new(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/demo-kb"))
    }

    #[test]
    fn demo_kb_is_consistent() {
        let store = demo();
        let catalog = store.catalog().unwrap();
        assert!(!catalog.sections.is_empty());
        let mut pending: Vec<String> = catalog
            .sections
            .iter()
            .flat_map(|s| s.nodes.iter().map(|e| e.id.clone()))
            .collect();
        let mut seen = std::collections::HashSet::new();
        while let Some(id) = pending.pop() {
            if seen.insert(id.clone()) {
                let node = store.node(&id).unwrap().unwrap();
                pending.extend(node.related);
            }
        }
    }

    #[test]
    fn missing_node_is_none() {
        assert_eq!(demo().node("不存在").unwrap(), None);
    }

    #[test]
    fn rejects_path_escape() {
        for id in ["../catalog", "a/b", "a\\b", "..", "."] {
            assert!(matches!(demo().node(id), Err(Error::Store(_))), "{id}");
        }
    }
}

use serde::{Deserialize, Serialize};

/// 节点地址，在整个知识库内唯一。
pub type NodeId = String;

/// 来源地址，由内容哈希得出，形如 `sha256:<哈希>`。
pub type SourceId = String;

/// 一条知识。节点只保存描述和证据，原始数据留在来源里，需要时再按证据读取。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    pub title: String,
    /// 模型自己的描述，是记忆的主体。
    pub description: String,
    /// 这条知识靠什么得出。
    pub evidence: Vec<Evidence>,
    /// 相关节点的地址。
    #[serde(default)]
    pub related: Vec<NodeId>,
}

/// 一条知识的依据。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Evidence {
    /// 来自原始数据。
    Source { source: SourceId, locator: Locator },
    /// 由另一条知识推出。那条知识被修改时，这个节点需要复查。
    Node { id: NodeId },
}

/// 来源中的一个位置。其他数据类型（PDF 的页码、图片的区域等）在这里增加新的种类。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Locator {
    /// 文本的字节范围，含 `start`，不含 `end`。
    TextRange { start: usize, end: usize },
}

/// 总表：知识库的目录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Catalog {
    pub sections: Vec<Section>,
}

/// 总表中的一个板块。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Section {
    /// 名字，在总表内唯一。
    pub name: String,
    /// 这个领域包含什么，模型靠它判断查询是否属于这个领域。
    #[serde(default)]
    pub description: String,
    pub nodes: Vec<CatalogEntry>,
}

/// 板块下指向节点的一项。标题让模型不打开节点也能在总表上做选择。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub id: NodeId,
    pub title: String,
}

/// 一次查询的结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KnowledgeTree {
    pub query: String,
    pub roots: Vec<TreeNode>,
    /// 读过但被模型丢弃的节点。
    pub rejected: Vec<NodeId>,
}

/// 知识树上的一个节点。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TreeNode {
    pub id: NodeId,
    pub title: String,
    pub description: String,
    pub evidence: Vec<Evidence>,
    pub children: Vec<TreeNode>,
    /// 同样被保留、但挂在树上其他位置的相关节点。
    pub references: Vec<NodeId>,
}

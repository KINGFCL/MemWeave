use serde::{Deserialize, Serialize};

/// 节点地址，在整个知识库内唯一。
pub type NodeId = String;

/// 一条知识。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    pub title: String,
    /// 知识的原始信息。
    pub raw: String,
    /// 对这条知识的理解。
    pub understanding: String,
    /// 相关节点的地址。
    #[serde(default)]
    pub related: Vec<NodeId>,
}

/// 总表：知识库的目录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Catalog {
    pub sections: Vec<Section>,
}

/// 总表中的一个板块。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Section {
    pub name: String,
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
    pub raw: String,
    pub understanding: String,
    pub children: Vec<TreeNode>,
    /// 同样被保留、但挂在树上其他位置的相关节点。
    pub references: Vec<NodeId>,
}

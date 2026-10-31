use std::fmt;

use thiserror::Error;

use crate::knowledge::NodeId;

/// 查询进行到的阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Catalog,
    Layer(usize),
}

impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Stage::Catalog => write!(f, "总表"),
            Stage::Layer(layer) => write!(f, "第 {layer} 层"),
        }
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("{stage}超出模型上下文：{tokens} token，上限 {limit}，超出 {}", .tokens - .limit)]
    ContextOverflow {
        stage: Stage,
        tokens: usize,
        limit: usize,
    },

    #[error("节点 {referrer} 关联的节点 {id} 不存在")]
    BrokenLink { id: NodeId, referrer: NodeId },

    #[error("总表中的节点 {0} 不存在")]
    MissingEntry(NodeId),

    #[error("{stage}：模型选择了候选之外的节点 {id}")]
    InvalidChoice { stage: Stage, id: NodeId },

    #[error("知识库读取失败：{0}")]
    Store(String),

    #[error("模型调用失败：{0}")]
    Model(String),
}

use std::fmt::{self, Display};

use thiserror::Error;

use crate::knowledge::NodeId;
use crate::memory::Problem;

/// 查询进行到的阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Catalog,
    Layer(usize),
    Weave,
}

impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Stage::Catalog => write!(f, "总表"),
            Stage::Layer(layer) => write!(f, "第 {layer} 层"),
            Stage::Weave => write!(f, "输入内容"),
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

    #[error("{stage}：模型选择了候选之外的节点 {id}")]
    InvalidChoice { stage: Stage, id: NodeId },

    #[error("知识库解析失败，共 {} 个问题：\n{}", .0.len(), bullet_list(.0))]
    Parse(Vec<String>),

    #[error("知识库校验失败，共 {} 个问题：\n{}", .0.len(), bullet_list(.0))]
    InvalidMemory(Vec<Problem>),

    #[error("记忆生成失败，共 {} 个问题：\n{}", .0.len(), bullet_list(.0))]
    Weave(Vec<String>),

    #[error("知识库写入失败：{0}")]
    Save(String),

    #[error("来源读取失败：{0}")]
    Source(String),

    #[error("节点 {0} 不存在")]
    UnknownNode(NodeId),

    #[error("内存不足")]
    OutOfMemory,

    #[error("模型调用失败：{0}")]
    Model(String),
}

fn bullet_list(items: &[impl Display]) -> String {
    let lines: Vec<String> = items.iter().map(|item| format!("- {item}")).collect();
    lines.join("\n")
}

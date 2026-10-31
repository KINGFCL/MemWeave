pub mod anthropic;
pub mod deepseek;
mod messages;

use crate::error::Error;
use crate::knowledge::NodeId;

/// 发给模型的一次请求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub system: &'static str,
    pub user: String,
}

/// 查询流程需要模型提供的能力。每家厂商在各自的子模块中实现。
pub trait Model {
    /// 单次请求可容纳的输入 token 数。
    fn context_limit(&self) -> usize;

    fn count_tokens(&self, prompt: &Prompt) -> Result<usize, Error>;

    /// 让模型做一次选择，返回它选中的节点地址。
    fn choose(&self, prompt: &Prompt) -> Result<Vec<NodeId>, Error>;
}

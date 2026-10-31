//! 记忆生成：Weaver（织者）调用模型，把原始数据组织成知识。
//!
//! 每种数据类型是一个子模块，实现同一个 [`Weaver`] 接口。生成的节点同样由
//! [`Memory`](crate::memory::Memory) 统一校验。

pub mod text;

use std::path::Path;

use crate::error::Error;
use crate::model::Model;
use crate::parse::ParsedKnowledge;
use crate::source::in_memory::InMemorySources;

/// Weaver 的结果：组织好的知识，以及证据指向的来源。
#[derive(Debug, Clone)]
pub struct Woven {
    pub knowledge: ParsedKnowledge,
    pub sources: InMemorySources,
}

/// 某一类原始数据的记忆生成器。
pub trait Weaver {
    /// 读取 `source` 处的数据，调用 `model` 组织成知识。
    ///
    /// 结果必须能通过 [`Memory::validate`](crate::memory::Memory::validate)。
    /// 证据的位置必须由实现根据原始数据算出，不能让模型改写或抄写原始数据。
    fn weave(&self, source: &Path, model: &dyn Model) -> Result<Woven, Error>;
}

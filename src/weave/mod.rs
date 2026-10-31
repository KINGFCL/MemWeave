//! 记忆生成：Weaver（织者）调用模型，把原始数据组织成知识。
//!
//! 每种数据类型是一个子模块，实现同一个 [`Weaver`] 接口。结果和解析器一样是
//! [`ParsedKnowledge`]，同样由 [`Memory`](crate::memory::Memory) 统一校验。

pub mod text;

use std::path::Path;

use crate::error::Error;
use crate::model::Model;
use crate::parse::ParsedKnowledge;

/// 某一类原始数据的记忆生成器。
pub trait Weaver {
    /// 读取 `source` 处的数据，调用 `model` 组织成知识。
    ///
    /// 返回的结果必须能通过 [`Memory::validate`](crate::memory::Memory::validate)。
    /// 节点的 `raw` 必须忠于原始数据，不能是模型改写的内容。
    fn weave(&self, source: &Path, model: &dyn Model) -> Result<ParsedKnowledge, Error>;
}

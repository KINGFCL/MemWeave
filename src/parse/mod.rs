//! 解析：把固态硬盘上某种格式的知识库读进内存。
//!
//! 每种格式是一个子模块，实现同一个 [`Parser`] 接口。解析器只负责读取和格式本身的检查，
//! 节点之间是否一致由 [`Memory`](crate::memory::Memory) 统一校验，新格式不需要重复实现。

pub mod json;

use std::path::Path;

use crate::error::Error;
use crate::knowledge::{Catalog, Node};

/// 解析结果：还没有校验过的总表和节点。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedKnowledge {
    pub catalog: Catalog,
    pub nodes: Vec<Node>,
}

/// 知识库格式的解析器。
pub trait Parser {
    /// 读取 `source` 处的知识库。
    ///
    /// 格式问题应该尽量一次报告完，通过 [`Error::Parse`] 返回。
    /// 节点的顺序决定校验报告的顺序，实现时应该保证结果是确定的。
    fn parse(&self, source: &Path) -> Result<ParsedKnowledge, Error>;
}

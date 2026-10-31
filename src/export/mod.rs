//! 导出：把内存里的记忆写回固态硬盘，方向和 [`parse`](crate::parse) 相反。
//!
//! 每种格式是一个子模块，实现同一个 [`Exporter`] 接口。导出的结果必须能被同一格式的解析器读回来。

pub mod json;

use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use crate::error::Error;
use crate::memory::Memory;
use crate::source::SourceStore;

/// 导出了多少内容。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exported {
    pub nodes: usize,
    pub sections: usize,
    pub sources: usize,
}

/// 知识库格式的导出器。
pub trait Exporter {
    /// 把 `memory` 的当前状态和节点引用到的来源写入 `target`。没有被引用的来源不会导出。
    ///
    /// `target` 必须不存在或是空目录。有节点从总表走不到时拒绝导出，因为这种状态解析不回来。
    /// 失败时 `target` 保持原样。
    fn export(
        &self,
        memory: &Memory,
        sources: &dyn SourceStore,
        target: &Path,
    ) -> Result<Exported, Error>;
}

/// `dir` 为什么不能作为导出目标。它不存在或是空目录时返回 `None`。
pub fn target_problem(dir: &Path) -> Option<String> {
    match fs::read_dir(dir) {
        Ok(mut entries) => entries
            .next()
            .map(|_| format!("{} 已有内容，不会覆盖", dir.display())),
        Err(e) if e.kind() == ErrorKind::NotFound => None,
        Err(e) => Some(format!("{}：{e}", dir.display())),
    }
}

/// 确认可以写入 `dir`，不会覆盖已有内容。
pub fn check_target(dir: &Path) -> Result<(), Error> {
    target_problem(dir).map_or(Ok(()), |problem| Err(Error::Save(problem)))
}

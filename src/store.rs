use crate::error::Error;
use crate::knowledge::{Catalog, Node};

/// 查询流程读取知识的接口。
pub trait Store {
    fn catalog(&self) -> Result<Catalog, Error>;

    /// 节点不存在时返回 `None`，由调用方结合上下文报错。
    fn node(&self, id: &str) -> Result<Option<Node>, Error>;
}

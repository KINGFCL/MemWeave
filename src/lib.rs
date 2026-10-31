//! 由模型驱动的知识图谱检索：从目录出发沿节点关联逐层查询，返回一棵知识树。
//!
//! **这是不稳定的实验版本，接口和数据格式随时可能发生不兼容的变化。**

pub mod error;
pub mod knowledge;
pub mod model;
mod prompt;
pub mod retrieve;
pub mod store;

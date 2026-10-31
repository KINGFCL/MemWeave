//! C++ 记忆层的接口。内存布局和 `cpp/include/memweave/graph.hpp` 共享。

use std::os::raw::c_void;
use std::sync::atomic::AtomicPtr;

/// 和 C++ 的 `Bytes` 布局相同。
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct RawBytes {
    pub(crate) data: *const u8,
    pub(crate) len: usize,
}

impl RawBytes {
    pub(crate) fn from_slice(bytes: &[u8]) -> Self {
        Self {
            data: bytes.as_ptr(),
            len: bytes.len(),
        }
    }
}

/// 和 C++ 的 `RelatedList` 布局相同，后面紧跟 `count` 个 `*const RawNode`。
#[repr(C)]
pub(crate) struct RawRelatedList {
    pub(crate) count: usize,
}

/// 和 C++ 的 `Node` 布局相同。
#[repr(C, align(64))]
pub(crate) struct RawNode {
    pub(crate) id: RawBytes,
    pub(crate) title: RawBytes,
    pub(crate) description: RawBytes,
    pub(crate) evidence: RawBytes,
    pub(crate) related: AtomicPtr<RawRelatedList>,
}

#[repr(C)]
pub(crate) struct RawPool {
    _opaque: [u8; 0],
}

#[repr(C)]
pub(crate) struct RawGraph {
    _opaque: [u8; 0],
}

#[cfg(test)]
#[repr(C)]
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RawLayout {
    pub(crate) node_size: usize,
    pub(crate) node_align: usize,
    pub(crate) node_id: usize,
    pub(crate) node_title: usize,
    pub(crate) node_description: usize,
    pub(crate) node_evidence: usize,
    pub(crate) node_related: usize,
    pub(crate) related_list_header: usize,
}

unsafe extern "C" {
    #[cfg(test)]
    pub(crate) fn mw_layout() -> RawLayout;

    pub(crate) fn mw_pool_new() -> *mut RawPool;
    pub(crate) fn mw_pool_free(pool: *mut RawPool);
    pub(crate) fn mw_pool_allocate(pool: *mut RawPool, size: usize) -> *mut c_void;
    pub(crate) fn mw_pool_deallocate(pool: *mut RawPool, pointer: *mut c_void, size: usize);

    pub(crate) fn mw_graph_new() -> *mut RawGraph;
    pub(crate) fn mw_graph_free(graph: *mut RawGraph);
    pub(crate) fn mw_graph_add_node(
        graph: *mut RawGraph,
        id: RawBytes,
        title: RawBytes,
        description: RawBytes,
        evidence: RawBytes,
    ) -> *mut RawNode;
    pub(crate) fn mw_graph_set_related(
        graph: *mut RawGraph,
        node: *mut RawNode,
        related: *const *mut RawNode,
        count: usize,
    ) -> bool;
}

#[cfg(test)]
mod tests {
    use std::mem::{align_of, offset_of, size_of};

    use super::*;

    /// 两边的布局不一致时，Rust 直接解引用会读错内存，必须在测试里拦住。
    #[test]
    fn layout_matches_cpp() {
        let expected = RawLayout {
            node_size: size_of::<RawNode>(),
            node_align: align_of::<RawNode>(),
            node_id: offset_of!(RawNode, id),
            node_title: offset_of!(RawNode, title),
            node_description: offset_of!(RawNode, description),
            node_evidence: offset_of!(RawNode, evidence),
            node_related: offset_of!(RawNode, related),
            related_list_header: size_of::<RawRelatedList>(),
        };

        assert_eq!(unsafe { mw_layout() }, expected);
    }
}

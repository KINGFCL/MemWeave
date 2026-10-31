//! C++ 记忆层的接口。内存布局和 `cpp/include/memweave/graph.hpp` 共享。

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

/// 和 C++ 的 `NodeList` 布局相同，后面紧跟 `count` 个 `*const RawNode`。
#[repr(C)]
pub(crate) struct RawNodeList {
    pub(crate) count: usize,
}

/// 和 C++ 的 `EvidenceKind` 取值相同。
pub(crate) const EVIDENCE_TEXT_RANGE: u32 = 0;
pub(crate) const EVIDENCE_NODE: u32 = 1;

/// 和 C++ 的 `Evidence` 布局相同。
#[repr(C)]
pub(crate) struct RawEvidence {
    pub(crate) kind: u32,
    pub(crate) id_len: u32,
    pub(crate) id: *const u8,
    pub(crate) node: *mut RawNode,
    pub(crate) start: u64,
    pub(crate) end: u64,
}

/// 和 C++ 的 `EvidenceList` 布局相同，后面紧跟 `count` 条 `RawEvidence`。
#[repr(C)]
pub(crate) struct RawEvidenceList {
    pub(crate) count: usize,
}

/// 和 C++ 的 `Node` 布局相同，正好占一条缓存行。
#[repr(C, align(64))]
pub(crate) struct RawNode {
    pub(crate) related: *const RawNodeList,
    pub(crate) id: *const u8,
    pub(crate) title: *const u8,
    pub(crate) description: *const u8,
    pub(crate) evidence: *const RawEvidenceList,
    pub(crate) dependents: *const RawNodeList,
    pub(crate) id_len: u32,
    pub(crate) title_len: u32,
    pub(crate) description_len: u32,
}

#[cfg(test)]
#[repr(C)]
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RawLayout {
    pub(crate) node_size: usize,
    pub(crate) node_align: usize,
    pub(crate) node_related: usize,
    pub(crate) node_id: usize,
    pub(crate) node_title: usize,
    pub(crate) node_description: usize,
    pub(crate) node_evidence: usize,
    pub(crate) node_dependents: usize,
    pub(crate) node_id_len: usize,
    pub(crate) node_title_len: usize,
    pub(crate) node_description_len: usize,
    pub(crate) node_list_header: usize,
    pub(crate) evidence_list_header: usize,
    pub(crate) evidence_size: usize,
    pub(crate) evidence_kind: usize,
    pub(crate) evidence_id_len: usize,
    pub(crate) evidence_id: usize,
    pub(crate) evidence_node: usize,
    pub(crate) evidence_start: usize,
    pub(crate) evidence_end: usize,
}

unsafe extern "C" {
    #[cfg(test)]
    pub(crate) fn mw_layout() -> RawLayout;

    pub(crate) fn mw_node_new(
        id: RawBytes,
        title: RawBytes,
        description: RawBytes,
        evidence: *const RawEvidence,
        evidence_count: usize,
        related_capacity: usize,
        dependents_capacity: usize,
    ) -> *mut RawNode;
    pub(crate) fn mw_node_set_related(
        node: *mut RawNode,
        related: *const *mut RawNode,
        count: usize,
    );
    pub(crate) fn mw_node_set_evidence_target(
        node: *mut RawNode,
        index: usize,
        target: *mut RawNode,
    );
    pub(crate) fn mw_node_set_dependents(
        node: *mut RawNode,
        dependents: *const *mut RawNode,
        count: usize,
    );
    pub(crate) fn mw_node_free(node: *mut RawNode);
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
            node_related: offset_of!(RawNode, related),
            node_id: offset_of!(RawNode, id),
            node_title: offset_of!(RawNode, title),
            node_description: offset_of!(RawNode, description),
            node_evidence: offset_of!(RawNode, evidence),
            node_dependents: offset_of!(RawNode, dependents),
            node_id_len: offset_of!(RawNode, id_len),
            node_title_len: offset_of!(RawNode, title_len),
            node_description_len: offset_of!(RawNode, description_len),
            node_list_header: size_of::<RawNodeList>(),
            evidence_list_header: size_of::<RawEvidenceList>(),
            evidence_size: size_of::<RawEvidence>(),
            evidence_kind: offset_of!(RawEvidence, kind),
            evidence_id_len: offset_of!(RawEvidence, id_len),
            evidence_id: offset_of!(RawEvidence, id),
            evidence_node: offset_of!(RawEvidence, node),
            evidence_start: offset_of!(RawEvidence, start),
            evidence_end: offset_of!(RawEvidence, end),
        };

        assert_eq!(unsafe { mw_layout() }, expected);
        assert_eq!(size_of::<RawNode>(), 64, "节点必须正好占一条缓存行");
    }
}

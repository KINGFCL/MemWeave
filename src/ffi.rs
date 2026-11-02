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

/// 和 C++ 的 `NodeList` 布局相同，后面紧跟 `capacity` 个 `*mut RawNode` 的位置。
#[repr(C)]
pub(crate) struct RawNodeList {
    pub(crate) count: u32,
    pub(crate) capacity: u32,
}

/// 和 C++ 的 `LinkKind` 取值相同。
pub(crate) const LINK_RELATED: u32 = 0;
pub(crate) const LINK_BASED_BY: u32 = 1;
pub(crate) const LINK_RELATED_BY: u32 = 2;

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

/// 和 C++ 的 `TextField` 取值相同。
pub(crate) const TEXT_TITLE: u32 = 0;
pub(crate) const TEXT_DESCRIPTION: u32 = 1;

/// C++ 的 `NodeIndex`：文本地址到节点的 set。
#[repr(C)]
pub(crate) struct RawIndex {
    _opaque: [u8; 0],
}

/// C++ 的 `NodeImage`：草稿里被改过的节点的原版。
#[repr(C)]
pub(crate) struct RawImage {
    _opaque: [u8; 0],
}

/// 和 C++ 的 `Backlinks` 布局相同。
#[repr(C)]
pub(crate) struct RawBacklinks {
    pub(crate) based_by: *const RawNodeList,
    pub(crate) related_by: *const RawNodeList,
    pub(crate) cold: *mut u8,
}

/// 和 C++ 的 `Node` 布局相同，正好占一条缓存行。
#[repr(C, align(64))]
pub(crate) struct RawNode {
    pub(crate) related: *const RawNodeList,
    pub(crate) id: *const u8,
    pub(crate) title: *const u8,
    pub(crate) description: *const u8,
    pub(crate) evidence: *const RawEvidenceList,
    pub(crate) backlinks: *const RawBacklinks,
    pub(crate) id_len: u32,
    pub(crate) title_len: u32,
    pub(crate) description_len: u32,
    pub(crate) flags: u32,
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
    pub(crate) node_backlinks: usize,
    pub(crate) node_id_len: usize,
    pub(crate) node_title_len: usize,
    pub(crate) node_description_len: usize,
    pub(crate) node_flags: usize,
    pub(crate) node_list_header: usize,
    pub(crate) node_list_count: usize,
    pub(crate) node_list_capacity: usize,
    pub(crate) evidence_list_header: usize,
    pub(crate) evidence_size: usize,
    pub(crate) evidence_kind: usize,
    pub(crate) evidence_id_len: usize,
    pub(crate) evidence_id: usize,
    pub(crate) evidence_node: usize,
    pub(crate) evidence_start: usize,
    pub(crate) evidence_end: usize,
    pub(crate) backlinks_size: usize,
    pub(crate) backlinks_based_by: usize,
    pub(crate) backlinks_related_by: usize,
    pub(crate) backlinks_cold: usize,
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
        based_by_capacity: usize,
        related_by_capacity: usize,
    ) -> *mut RawNode;
    pub(crate) fn mw_node_set_evidence_target(
        node: *mut RawNode,
        index: usize,
        target: *mut RawNode,
    );
    pub(crate) fn mw_node_set_links(
        node: *mut RawNode,
        kind: u32,
        nodes: *const *mut RawNode,
        count: usize,
    );
    pub(crate) fn mw_node_reserve_links(node: *mut RawNode, kind: u32, extra: usize) -> bool;
    pub(crate) fn mw_node_append_link(node: *mut RawNode, kind: u32, target: *mut RawNode);
    pub(crate) fn mw_node_detach(node: *mut RawNode);
    pub(crate) fn mw_node_free(node: *mut RawNode);
    pub(crate) fn mw_node_set_text(node: *mut RawNode, field: u32, text: RawBytes) -> bool;
    pub(crate) fn mw_node_link(from: *mut RawNode, to: *mut RawNode) -> bool;
    pub(crate) fn mw_node_unlink(from: *mut RawNode, to: *mut RawNode);
    pub(crate) fn mw_node_add_evidence(node: *mut RawNode, target: *mut RawNode) -> bool;
    pub(crate) fn mw_node_remove_evidence(node: *mut RawNode, id: RawBytes) -> bool;
    pub(crate) fn mw_node_add_source_evidence(
        node: *mut RawNode,
        source: RawBytes,
        start: u64,
        end: u64,
    ) -> bool;
    pub(crate) fn mw_node_remove_source_evidence(
        node: *mut RawNode,
        source: RawBytes,
        start: u64,
        end: u64,
    ) -> bool;

    pub(crate) fn mw_image_save(node: *mut RawNode) -> *mut RawImage;
    pub(crate) fn mw_image_own_list(node: *mut RawNode, kind: u32) -> bool;
    pub(crate) fn mw_image_swap(node: *mut RawNode, image: *mut RawImage);
    pub(crate) fn mw_image_keep_revision(node: *mut RawNode, image: *mut RawImage);
    pub(crate) fn mw_image_keep_origin(node: *mut RawNode, image: *mut RawImage);

    pub(crate) fn mw_index_new() -> *mut RawIndex;
    pub(crate) fn mw_index_free(index: *mut RawIndex);
    pub(crate) fn mw_index_size(index: *const RawIndex) -> usize;
    pub(crate) fn mw_index_find(index: *const RawIndex, id: RawBytes) -> *mut RawNode;
    pub(crate) fn mw_index_insert(index: *mut RawIndex, node: *mut RawNode) -> bool;
    pub(crate) fn mw_index_erase(index: *mut RawIndex, id: RawBytes);
    pub(crate) fn mw_index_next(index: *const RawIndex, cursor: *mut usize) -> *mut RawNode;
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
            node_backlinks: offset_of!(RawNode, backlinks),
            node_id_len: offset_of!(RawNode, id_len),
            node_title_len: offset_of!(RawNode, title_len),
            node_description_len: offset_of!(RawNode, description_len),
            node_flags: offset_of!(RawNode, flags),
            node_list_header: size_of::<RawNodeList>(),
            node_list_count: offset_of!(RawNodeList, count),
            node_list_capacity: offset_of!(RawNodeList, capacity),
            evidence_list_header: size_of::<RawEvidenceList>(),
            evidence_size: size_of::<RawEvidence>(),
            evidence_kind: offset_of!(RawEvidence, kind),
            evidence_id_len: offset_of!(RawEvidence, id_len),
            evidence_id: offset_of!(RawEvidence, id),
            evidence_node: offset_of!(RawEvidence, node),
            evidence_start: offset_of!(RawEvidence, start),
            evidence_end: offset_of!(RawEvidence, end),
            backlinks_size: size_of::<RawBacklinks>(),
            backlinks_based_by: offset_of!(RawBacklinks, based_by),
            backlinks_related_by: offset_of!(RawBacklinks, related_by),
            backlinks_cold: offset_of!(RawBacklinks, cold),
        };

        assert_eq!(unsafe { mw_layout() }, expected);
        assert_eq!(size_of::<RawNode>(), 64, "节点必须正好占一条缓存行");
    }
}

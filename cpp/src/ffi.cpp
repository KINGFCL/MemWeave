// 给 Rust 调用的 C 接口。所有函数都不抛异常，失败时返回空指针。
#include <cstddef>

#include "memweave/graph.hpp"

using memweave::Backlinks;
using memweave::Bytes;
using memweave::Evidence;
using memweave::EvidenceList;
using memweave::LinkKind;
using memweave::Node;
using memweave::NodeIndex;
using memweave::TextField;
using memweave::NodeList;

extern "C" {

struct MwLayout {
  std::size_t node_size;
  std::size_t node_align;
  std::size_t node_related;
  std::size_t node_id;
  std::size_t node_title;
  std::size_t node_description;
  std::size_t node_evidence;
  std::size_t node_backlinks;
  std::size_t node_id_len;
  std::size_t node_title_len;
  std::size_t node_description_len;
  std::size_t node_flags;
  std::size_t node_list_header;
  std::size_t node_list_count;
  std::size_t node_list_capacity;
  std::size_t evidence_list_header;
  std::size_t evidence_size;
  std::size_t evidence_kind;
  std::size_t evidence_id_len;
  std::size_t evidence_id;
  std::size_t evidence_node;
  std::size_t evidence_start;
  std::size_t evidence_end;
  std::size_t backlinks_size;
  std::size_t backlinks_based_by;
  std::size_t backlinks_related_by;
  std::size_t backlinks_cold;
};

MwLayout mw_layout() noexcept {
  return MwLayout{
      sizeof(Node),
      alignof(Node),
      offsetof(Node, related),
      offsetof(Node, id),
      offsetof(Node, title),
      offsetof(Node, description),
      offsetof(Node, evidence),
      offsetof(Node, backlinks),
      offsetof(Node, id_len),
      offsetof(Node, title_len),
      offsetof(Node, description_len),
      offsetof(Node, flags),
      sizeof(NodeList),
      offsetof(NodeList, count),
      offsetof(NodeList, capacity),
      sizeof(EvidenceList),
      sizeof(Evidence),
      offsetof(Evidence, kind),
      offsetof(Evidence, id_len),
      offsetof(Evidence, id),
      offsetof(Evidence, node),
      offsetof(Evidence, start),
      offsetof(Evidence, end),
      sizeof(Backlinks),
      offsetof(Backlinks, based_by),
      offsetof(Backlinks, related_by),
      offsetof(Backlinks, cold),
  };
}

Node* mw_node_new(Bytes id, Bytes title, Bytes description, const Evidence* evidence,
                  std::size_t evidence_count, std::size_t related_capacity,
                  std::size_t based_by_capacity, std::size_t related_by_capacity) noexcept {
  return memweave::new_node(id, title, description, evidence, evidence_count, related_capacity,
                            based_by_capacity, related_by_capacity);
}

void mw_node_set_evidence_target(Node* node, std::size_t index, Node* target) noexcept {
  memweave::set_evidence_target(node, index, target);
}

void mw_node_set_links(Node* node, LinkKind kind, Node* const* nodes, std::size_t count) noexcept {
  memweave::set_links(node, kind, nodes, count);
}

bool mw_node_reserve_links(Node* node, LinkKind kind, std::size_t extra) noexcept {
  return memweave::reserve_links(node, kind, extra);
}

void mw_node_append_link(Node* node, LinkKind kind, Node* target) noexcept {
  memweave::append_link(node, kind, target);
}

void mw_node_detach(Node* node) noexcept { memweave::detach_node(node); }

void mw_node_free(Node* node) noexcept { memweave::delete_node(node); }

bool mw_node_set_text(Node* node, TextField field, Bytes text) noexcept {
  return memweave::set_text(node, field, text);
}

bool mw_node_link(Node* from, Node* to) noexcept { return memweave::link_nodes(from, to); }

void mw_node_unlink(Node* from, Node* to) noexcept { memweave::unlink_nodes(from, to); }

bool mw_node_add_evidence(Node* node, Node* target) noexcept {
  return memweave::add_node_evidence(node, target);
}

bool mw_node_remove_evidence(Node* node, Bytes id) noexcept {
  return memweave::remove_node_evidence(node, id);
}

NodeIndex* mw_index_new() noexcept { return new (std::nothrow) NodeIndex(); }

void mw_index_free(NodeIndex* index) noexcept { delete index; }

std::size_t mw_index_size(const NodeIndex* index) noexcept { return index->size(); }

Node* mw_index_find(const NodeIndex* index, Bytes id) noexcept { return index->find(id); }

bool mw_index_insert(NodeIndex* index, Node* node) noexcept { return index->insert(node); }

void mw_index_erase(NodeIndex* index, Bytes id) noexcept { index->erase(id); }

Node* mw_index_next(const NodeIndex* index, std::size_t* cursor) noexcept {
  return index->next(cursor);
}

}  // extern "C"

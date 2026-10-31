// 给 Rust 调用的 C 接口。所有函数都不抛异常，失败时返回空指针。
#include <cstddef>

#include "memweave/graph.hpp"

using memweave::Backlinks;
using memweave::Bytes;
using memweave::Evidence;
using memweave::EvidenceList;
using memweave::Node;
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
  std::size_t node_list_header;
  std::size_t evidence_list_header;
  std::size_t evidence_size;
  std::size_t evidence_kind;
  std::size_t evidence_id_len;
  std::size_t evidence_id;
  std::size_t evidence_node;
  std::size_t evidence_start;
  std::size_t evidence_end;
  std::size_t backlinks_size;
  std::size_t backlinks_dependents;
  std::size_t backlinks_related_by;
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
      sizeof(NodeList),
      sizeof(EvidenceList),
      sizeof(Evidence),
      offsetof(Evidence, kind),
      offsetof(Evidence, id_len),
      offsetof(Evidence, id),
      offsetof(Evidence, node),
      offsetof(Evidence, start),
      offsetof(Evidence, end),
      sizeof(Backlinks),
      offsetof(Backlinks, dependents),
      offsetof(Backlinks, related_by),
  };
}

Node* mw_node_new(Bytes id, Bytes title, Bytes description, const Evidence* evidence,
                  std::size_t evidence_count, std::size_t related_capacity,
                  std::size_t dependents_capacity, std::size_t related_by_capacity) noexcept {
  return memweave::new_node(id, title, description, evidence, evidence_count, related_capacity,
                            dependents_capacity, related_by_capacity);
}

void mw_node_set_related(Node* node, Node* const* related, std::size_t count) noexcept {
  memweave::set_related(node, related, count);
}

void mw_node_set_evidence_target(Node* node, std::size_t index, Node* target) noexcept {
  memweave::set_evidence_target(node, index, target);
}

void mw_node_set_dependents(Node* node, Node* const* dependents, std::size_t count) noexcept {
  memweave::set_dependents(node, dependents, count);
}

void mw_node_set_related_by(Node* node, Node* const* related_by, std::size_t count) noexcept {
  memweave::set_related_by(node, related_by, count);
}

void mw_node_free(Node* node) noexcept { memweave::delete_node(node); }

}  // extern "C"

// 给 Rust 调用的 C 接口。所有函数都不抛异常，失败时返回空指针。
#include <cstddef>

#include "memweave/graph.hpp"

using memweave::Bytes;
using memweave::Node;
using memweave::RelatedList;

extern "C" {

struct MwLayout {
  std::size_t node_size;
  std::size_t node_align;
  std::size_t node_related;
  std::size_t node_id;
  std::size_t node_title;
  std::size_t node_description;
  std::size_t node_evidence;
  std::size_t node_id_len;
  std::size_t node_title_len;
  std::size_t node_description_len;
  std::size_t node_evidence_len;
  std::size_t related_list_header;
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
      offsetof(Node, id_len),
      offsetof(Node, title_len),
      offsetof(Node, description_len),
      offsetof(Node, evidence_len),
      sizeof(RelatedList),
  };
}

Node* mw_node_new(Bytes id, Bytes title, Bytes description, Bytes evidence,
                  std::size_t related_capacity) noexcept {
  return memweave::new_node(id, title, description, evidence, related_capacity);
}

void mw_node_set_related(Node* node, Node* const* related, std::size_t count) noexcept {
  memweave::set_related(node, related, count);
}

void mw_node_free(Node* node) noexcept { memweave::delete_node(node); }

}  // extern "C"

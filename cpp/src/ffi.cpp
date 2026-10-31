// 给 Rust 调用的 C 接口。所有函数都不抛异常，失败时返回空指针或 false。
#include <cstddef>
#include <new>

#include "memweave/graph.hpp"
#include "memweave/memory_pool.hpp"

using memweave::Bytes;
using memweave::Graph;
using memweave::MemoryPool;
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
  std::size_t node_inline_capacity;
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
      offsetof(Node, inline_capacity),
      sizeof(RelatedList),
  };
}

MemoryPool* mw_pool_new() noexcept { return new (std::nothrow) MemoryPool(); }

void mw_pool_free(MemoryPool* pool) noexcept { delete pool; }

void* mw_pool_allocate(MemoryPool* pool, std::size_t size) noexcept { return pool->allocate(size); }

void mw_pool_deallocate(MemoryPool* pool, void* pointer, std::size_t size) noexcept {
  pool->deallocate(pointer, size);
}

Graph* mw_graph_new() noexcept { return new (std::nothrow) Graph(); }

void mw_graph_free(Graph* graph) noexcept { delete graph; }

Node* mw_graph_add_node(Graph* graph, Bytes id, Bytes title, Bytes description, Bytes evidence,
                        std::size_t related_capacity) noexcept {
  return graph->add_node(id, title, description, evidence, related_capacity);
}

bool mw_graph_set_related(Graph* graph, Node* node, Node* const* related,
                          std::size_t count) noexcept {
  return graph->set_related(node, related, count);
}

}  // extern "C"

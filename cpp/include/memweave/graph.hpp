// 记忆图：节点和它们的关联都放在内存池里，节点之间直接用地址相连。
//
// 布局按遍历的访问顺序设计，目标是每跳尽量少的缓存未命中：
// - 遍历要读的字段都在节点的第一条缓存行里。
// - 关联列表紧跟在节点后面，在同一对相邻缓存行内，硬件预取会一起取回。
// - 文本和证据放在单独的池里，不占用遍历区域的缓存和 TLB。
#pragma once

#include <atomic>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <limits>
#include <new>

#include "memweave/memory_pool.hpp"

namespace memweave {

struct Node;

struct Bytes {
  const char* data;
  std::size_t len;
};

// 关联列表，后面紧跟 count 个 Node*。
struct RelatedList {
  std::size_t count;

  Node* const* nodes() const noexcept { return reinterpret_cast<Node* const*>(this + 1); }
  Node** nodes() noexcept { return reinterpret_cast<Node**>(this + 1); }
};

// 这个布局是和 Rust 共享的（src/ffi.rs），Rust 读取时直接解引用，不经过函数调用。
// 修改时两边要一起改，两边都有布局检查。
struct alignas(64) Node {
  std::atomic<const RelatedList*> related;
  const char* id;
  const char* title;
  const char* description;
  const char* evidence;  // 序列化后的证据，只在需要时才解析。
  std::uint32_t id_len;
  std::uint32_t title_len;
  std::uint32_t description_len;
  std::uint32_t evidence_len;
  std::uint32_t inline_capacity;  // 紧跟在节点后面的关联列表能放多少条。
};

static_assert(sizeof(Node) == 64, "节点必须正好占一条缓存行");
static_assert(sizeof(RelatedList) == sizeof(std::size_t));
static_assert(alignof(RelatedList) >= alignof(Node*));

class Graph {
 public:
  // 只能由分配线程调用。内存不足或文本超过 4 GiB 时返回 nullptr。
  // related_capacity 是预计的关联数量，在节点后面预留位置。
  Node* add_node(Bytes id, Bytes title, Bytes description, Bytes evidence,
                 std::size_t related_capacity) noexcept {
    constexpr std::size_t kMaxText = std::numeric_limits<std::uint32_t>::max();
    constexpr std::size_t kMaxInline = std::numeric_limits<std::uint32_t>::max();
    if (id.len > kMaxText || title.len > kMaxText || description.len > kMaxText ||
        evidence.len > kMaxText || related_capacity > kMaxInline ||
        related_capacity > (std::numeric_limits<std::size_t>::max() - sizeof(Node)) / sizeof(Node*) - 1) {
      return nullptr;
    }

    const char* id_copy = copy(id);
    const char* title_copy = copy(title);
    const char* description_copy = copy(description);
    const char* evidence_copy = copy(evidence);
    void* memory = hot_.allocate(sizeof(Node) + sizeof(RelatedList) + related_capacity * sizeof(Node*));
    if (id_copy == nullptr || title_copy == nullptr || description_copy == nullptr ||
        evidence_copy == nullptr || memory == nullptr) {
      return nullptr;  // 已分配的内存随图一起释放。
    }
    return new (memory) Node{{nullptr},
                             id_copy,
                             title_copy,
                             description_copy,
                             evidence_copy,
                             static_cast<std::uint32_t>(id.len),
                             static_cast<std::uint32_t>(title.len),
                             static_cast<std::uint32_t>(description.len),
                             static_cast<std::uint32_t>(evidence.len),
                             static_cast<std::uint32_t>(related_capacity)};
  }

  // 设置节点的关联。只能由分配线程调用。新列表用 release 发布，读取方看到的总是完整的列表。
  //
  // 第一次设置时优先用节点后面预留的位置。替换时分配新列表，旧列表不会被释放：可能还有读取方
  // 在用它，要等有了内存回收方案才能归还，目前随图一起释放。
  bool set_related(Node* node, Node* const* related, std::size_t count) noexcept {
    RelatedList* list = nullptr;
    if (node->related.load(std::memory_order_relaxed) == nullptr && count <= node->inline_capacity) {
      list = new (node + 1) RelatedList{count};
    } else {
      void* memory = hot_.allocate(sizeof(RelatedList) + count * sizeof(Node*));
      if (memory == nullptr) {
        return false;
      }
      list = new (memory) RelatedList{count};
    }
    if (count != 0) {
      std::memcpy(list->nodes(), related, count * sizeof(Node*));
    }
    node->related.store(list, std::memory_order_release);
    return true;
  }

 private:
  const char* copy(Bytes bytes) noexcept {
    // 空内容也分配一个块，保证指针不为空，Rust 可以直接构造切片。
    char* memory = static_cast<char*>(cold_.allocate(bytes.len == 0 ? 1 : bytes.len));
    if (memory != nullptr && bytes.len != 0) {
      std::memcpy(memory, bytes.data, bytes.len);
    }
    return memory;
  }

  MemoryPool hot_;   // 节点和关联列表，遍历时访问。
  MemoryPool cold_;  // 文本和证据。
};

}  // namespace memweave

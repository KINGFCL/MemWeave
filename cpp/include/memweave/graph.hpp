// 记忆图：节点和它们的关联都放在内存池里，节点之间直接用地址相连。
#pragma once

#include <atomic>
#include <cstddef>
#include <cstring>
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
  Bytes id;
  Bytes title;
  Bytes description;
  Bytes evidence;  // 序列化后的证据，只在需要时才解析。
  std::atomic<const RelatedList*> related;
};

static_assert(sizeof(RelatedList) == sizeof(std::size_t));
static_assert(alignof(RelatedList) >= alignof(Node*));

class Graph {
 public:
  // 只能由分配线程调用。内存不足时返回 nullptr。
  Node* add_node(Bytes id, Bytes title, Bytes description, Bytes evidence) noexcept {
    void* memory = pool_.allocate(sizeof(Node));
    if (memory == nullptr) {
      return nullptr;
    }
    const Bytes copies[] = {copy(id), copy(title), copy(description), copy(evidence)};
    for (const Bytes& bytes : copies) {
      if (bytes.data == nullptr) {
        return nullptr;  // 已分配的内存随图一起释放。
      }
    }
    return new (memory) Node{copies[0], copies[1], copies[2], copies[3], {nullptr}};
  }

  // 替换节点的关联。只能由分配线程调用。新列表用 release 发布，读取方看到的总是完整的列表。
  // 旧列表不会被释放：可能还有读取方在用它，要等有了内存回收方案才能归还，目前随图一起释放。
  bool set_related(Node* node, Node* const* related, std::size_t count) noexcept {
    void* memory = pool_.allocate(sizeof(RelatedList) + count * sizeof(Node*));
    if (memory == nullptr) {
      return false;
    }
    RelatedList* list = new (memory) RelatedList{count};
    if (count != 0) {
      std::memcpy(list->nodes(), related, count * sizeof(Node*));
    }
    node->related.store(list, std::memory_order_release);
    return true;
  }

 private:
  Bytes copy(Bytes bytes) noexcept {
    // 空内容也分配一个块，保证 data 不为空，Rust 可以直接构造切片。
    char* memory = static_cast<char*>(pool_.allocate(bytes.len == 0 ? 1 : bytes.len));
    if (memory != nullptr && bytes.len != 0) {
      std::memcpy(memory, bytes.data, bytes.len);
    }
    return Bytes{memory, bytes.len};
  }

  MemoryPool pool_;
};

}  // namespace memweave

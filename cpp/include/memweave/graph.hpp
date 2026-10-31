// 记忆图的节点：节点之间直接用地址相连，内存用 new 分配。
//
// 查询时只读不分配，所以分配方式不影响遍历的操作数，只影响节点在内存里的位置。
// 布局按遍历的访问顺序设计：
// - 节点占一条缓存行，遍历要读的字段都在里面。
// - 关联列表紧跟在节点后面，和节点一起分配。
// - 文本和证据单独分配，不占用遍历区域的缓存。
#pragma once

#include <atomic>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <limits>
#include <new>

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
};

static_assert(sizeof(Node) == 64, "节点必须正好占一条缓存行");
static_assert(sizeof(RelatedList) == sizeof(std::size_t));

// 按一对相邻缓存行对齐，硬件的相邻行预取通常会把节点和关联列表一起取回。
inline constexpr std::align_val_t kNodeAlignment{128};

// 创建节点，并在后面预留 related_capacity 条关联的位置。
// 内存不足或文本超过 4 GiB 时返回 nullptr。
inline Node* new_node(Bytes id, Bytes title, Bytes description, Bytes evidence,
                      std::size_t related_capacity) noexcept {
  constexpr std::size_t kMaxText = std::numeric_limits<std::uint32_t>::max();
  constexpr std::size_t kMaxCapacity =
      (std::numeric_limits<std::size_t>::max() - sizeof(Node) - sizeof(RelatedList)) / sizeof(Node*);
  if (id.len > kMaxText || title.len > kMaxText || description.len > kMaxText ||
      evidence.len > kMaxText || related_capacity > kMaxCapacity) {
    return nullptr;
  }

  // 四段文本放在同一块内存里，id 在最前面，释放时只需要释放它。
  char* text = new (std::nothrow) char[id.len + title.len + description.len + evidence.len];
  if (text == nullptr) {
    return nullptr;
  }
  void* memory = ::operator new(sizeof(Node) + sizeof(RelatedList) + related_capacity * sizeof(Node*),
                                kNodeAlignment, std::nothrow);
  if (memory == nullptr) {
    delete[] text;
    return nullptr;
  }

  auto append = [cursor = text](Bytes bytes) mutable {
    const char* start = cursor;
    if (bytes.len != 0) {
      std::memcpy(cursor, bytes.data, bytes.len);
    }
    cursor += bytes.len;
    return start;
  };
  const char* id_text = append(id);
  const char* title_text = append(title);
  const char* description_text = append(description);
  const char* evidence_text = append(evidence);
  return new (memory) Node{{nullptr},
                           id_text,
                           title_text,
                           description_text,
                           evidence_text,
                           static_cast<std::uint32_t>(id.len),
                           static_cast<std::uint32_t>(title.len),
                           static_cast<std::uint32_t>(description.len),
                           static_cast<std::uint32_t>(evidence.len)};
}

// 写入节点的关联。count 不能超过创建时预留的数量，每个节点只能设置一次。
inline void set_related(Node* node, Node* const* related, std::size_t count) noexcept {
  auto* list = new (node + 1) RelatedList{count};
  if (count != 0) {
    std::memcpy(list->nodes(), related, count * sizeof(Node*));
  }
  node->related.store(list, std::memory_order_release);
}

// 释放节点。调用时不能有其他线程在读这个节点。
inline void delete_node(Node* node) noexcept {
  delete[] node->id;
  node->~Node();
  ::operator delete(node, kNodeAlignment);
}

}  // namespace memweave

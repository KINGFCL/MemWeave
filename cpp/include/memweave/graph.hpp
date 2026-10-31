// 记忆图的节点：节点之间直接用地址相连，内存用 new 分配。
//
// 一张图只属于一个线程，这个线程又读又写，所以这里没有任何同步。别的线程要用就自己加载一份。
//
// 查询时只读不分配，所以分配方式不影响遍历的操作数，只影响节点在内存里的位置。
// 每个节点两块内存：
// - 热块：节点和关联列表。节点占一条缓存行，遍历要读的字段都在里面，关联列表紧跟在后面。
// - 冷块：证据列表、反向列表和所有文本。遍历用不到，不占用遍历区域的缓存。
#pragma once

#include <cstddef>
#include <cstdint>
#include <cstring>
#include <initializer_list>
#include <limits>
#include <new>

namespace memweave {

struct Node;

struct Bytes {
  const char* data;
  std::size_t len;
};

// 节点地址的列表，后面紧跟 capacity 个 Node* 的位置，其中前 count 个有效。
// 关联和两个反向关系都用它。这个布局也是和 Rust 共享的。
struct NodeList {
  std::uint32_t count;
  std::uint32_t capacity;

  Node* const* nodes() const noexcept { return reinterpret_cast<Node* const*>(this + 1); }
  Node** nodes() noexcept { return reinterpret_cast<Node**>(this + 1); }
};

// 证据的种类。其他数据类型（PDF 的页码、图片的区域等）在这里增加新的种类。
enum class EvidenceKind : std::uint32_t {
  kTextRange = 0,  // 来自原始数据：文本来源的字节范围。
  kNode = 1,       // 由另一条知识推出。
};

// 一条证据：这条知识靠什么得出。这个布局也是和 Rust 共享的。
struct Evidence {
  EvidenceKind kind;
  std::uint32_t id_len;
  // 依据的文本地址：kTextRange 是来源地址（sha256:...），kNode 是依据节点的 id。
  // 依据节点的 id 虽然也能从 node 读到，但存一份在这里，看证据时不用再跳转就能读懂。
  const char* id;
  Node* node;  // kNode 时是依据节点的内存地址，kTextRange 时为空。
  std::uint64_t start;  // kTextRange 时是字节范围，含 start，不含 end。kNode 时为 0。
  std::uint64_t end;
};

// 证据列表，后面紧跟 count 条 Evidence。
struct EvidenceList {
  std::size_t count;

  const Evidence* entries() const noexcept { return reinterpret_cast<const Evidence*>(this + 1); }
  Evidence* entries() noexcept { return reinterpret_cast<Evidence*>(this + 1); }
};

// 反向关系：谁指向我。修改和删除节点时靠它找到需要处理的节点，遍历时用不到。
// 它们都可以从 evidence 和 related 反推，所以不存到磁盘，加载时计算。这个布局也是和 Rust 共享的。
struct Backlinks {
  const NodeList* dependents;  // 谁依赖我：证据里引用了这个节点的节点。这个节点变了，它们要复查。
  const NodeList* related_by;  // 谁关联我：related 里有这个节点的节点。删除这个节点时要清理它们。
};

// 这个布局是和 Rust 共享的（src/ffi.rs），Rust 读取时直接解引用，不经过函数调用。
// 修改时两边要一起改，两边都有布局检查。
//
// 节点里没有文本本身：id、title、description 都是地址，文本放在冷块里。文本没有 \0 结尾，
// 所以每段都配一个长度（字节数）。原始数据也不在节点里，evidence 只记录它在哪，
// 原文留在固态硬盘的 sources/ 目录。
struct alignas(64) Node {
  // 关联列表的地址，列表紧跟在节点后面，存的是相关节点的内存地址。
  // 从一个节点走到下一个节点只读这个字段。检索时沿它往下爬。
  const NodeList* related;// 关联列表的地址，列表紧跟在节点后面，存的是相关节点的内存地址。
  const char* id;// 节点地址文本，如 "ownership"。它是节点的文本名字，也是 JSON 文件名，模型回答时用的就是它。
  const char* title;  // 标题，如 "所有权"。总表和“已保留节点”列表里会用到。
  const char* description;// 模型自己写的描述，是记忆的主体。查询时模型靠它判断是否保留节点。
  const EvidenceList* evidence;// 我依赖谁：这条知识靠什么得出。来源证据指向原始数据的位置，节点证据指向依据节点。
  const Backlinks* backlinks;// 反向关系：谁依赖我、谁关联我。
  std::uint32_t id_len;
  std::uint32_t title_len;
  std::uint32_t description_len;
  std::uint32_t flags;  // 哪些列表已经搬到单独分配的内存里，释放节点时要单独释放它们。
};

static_assert(sizeof(Node) == 64, "节点必须正好占一条缓存行");
static_assert(sizeof(NodeList) == sizeof(std::size_t));
static_assert(sizeof(EvidenceList) == sizeof(std::size_t));
static_assert(sizeof(Evidence) == 40);
static_assert(sizeof(Backlinks) == 2 * sizeof(void*));

// 按一对相邻缓存行对齐，硬件的相邻行预取通常会把节点和关联列表一起取回。
inline constexpr std::align_val_t kNodeAlignment{128};

// 节点的三个列表。
enum class LinkKind : std::uint32_t {
  kRelated = 0,
  kDependents = 1,
  kRelatedBy = 2,
};

inline const NodeList* link_list(const Node* node, LinkKind kind) noexcept {
  switch (kind) {
    case LinkKind::kRelated:
      return node->related;
    case LinkKind::kDependents:
      return node->backlinks->dependents;
    case LinkKind::kRelatedBy:
      return node->backlinks->related_by;
  }
  return nullptr;
}

inline void replace_link_list(Node* node, LinkKind kind, const NodeList* list) noexcept {
  auto* backlinks = const_cast<Backlinks*>(node->backlinks);
  switch (kind) {
    case LinkKind::kRelated:
      node->related = list;
      break;
    case LinkKind::kDependents:
      backlinks->dependents = list;
      break;
    case LinkKind::kRelatedBy:
      backlinks->related_by = list;
      break;
  }
}

inline std::uint32_t heap_flag(LinkKind kind) noexcept {
  return std::uint32_t{1} << static_cast<std::uint32_t>(kind);
}

// 创建节点，并预留 related_capacity 条关联，以及两个反向列表的位置。
// evidence 里的 id 会被复制，node 字段之后用 set_evidence_target 填写。
// 内存不足或文本超过 4 GiB 时返回 nullptr。
inline Node* new_node(Bytes id, Bytes title, Bytes description, const Evidence* evidence,
                      std::size_t evidence_count, std::size_t related_capacity,
                      std::size_t dependents_capacity, std::size_t related_by_capacity) noexcept {
  constexpr std::size_t kMax = std::numeric_limits<std::size_t>::max();
  constexpr std::size_t kMaxText = std::numeric_limits<std::uint32_t>::max();
  constexpr std::size_t kMaxCount = std::numeric_limits<std::uint32_t>::max();
  if (id.len > kMaxText || title.len > kMaxText || description.len > kMaxText ||
      evidence_count > kMaxCount || related_capacity > kMaxCount ||
      dependents_capacity > kMaxCount || related_by_capacity > kMaxCount) {
    return nullptr;
  }

  // 冷块依次放证据列表、反向列表和文本。证据列表在最前面，释放时只需要释放它。
  const std::size_t lists_size = sizeof(EvidenceList) + evidence_count * sizeof(Evidence) +
                                 sizeof(Backlinks) + sizeof(NodeList) +
                                 dependents_capacity * sizeof(Node*) + sizeof(NodeList) +
                                 related_by_capacity * sizeof(Node*);
  std::size_t text_size = id.len + title.len + description.len;
  for (std::size_t i = 0; i < evidence_count; ++i) {
    if (text_size > kMax - lists_size - evidence[i].id_len) {
      return nullptr;
    }
    text_size += evidence[i].id_len;
  }

  char* cold = new (std::nothrow) char[lists_size + text_size];
  if (cold == nullptr) {
    return nullptr;
  }
  void* hot = ::operator new(sizeof(Node) + sizeof(NodeList) + related_capacity * sizeof(Node*),
                             kNodeAlignment, std::nothrow);
  if (hot == nullptr) {
    delete[] cold;
    return nullptr;
  }

  auto* evidence_list = new (cold) EvidenceList{evidence_count};
  auto* backlinks = reinterpret_cast<Backlinks*>(evidence_list->entries() + evidence_count);
  auto* dependents =
      new (backlinks + 1) NodeList{0, static_cast<std::uint32_t>(dependents_capacity)};
  auto* related_by = new (dependents->nodes() + dependents_capacity)
      NodeList{0, static_cast<std::uint32_t>(related_by_capacity)};
  new (backlinks) Backlinks{dependents, related_by};

  char* cursor = cold + lists_size;
  auto append = [&cursor](const char* data, std::size_t len) {
    const char* start = cursor;
    if (len != 0) {
      std::memcpy(cursor, data, len);
    }
    cursor += len;
    return start;
  };
  for (std::size_t i = 0; i < evidence_count; ++i) {
    Evidence copied = evidence[i];
    copied.id = append(evidence[i].id, evidence[i].id_len);
    copied.node = nullptr;
    new (evidence_list->entries() + i) Evidence(copied);
  }
  const char* id_text = append(id.data, id.len);
  const char* title_text = append(title.data, title.len);
  const char* description_text = append(description.data, description.len);

  auto* node_memory = static_cast<Node*>(hot);
  auto* related = new (node_memory + 1) NodeList{0, static_cast<std::uint32_t>(related_capacity)};
  return new (hot) Node{related,
                        id_text,
                        title_text,
                        description_text,
                        evidence_list,
                        backlinks,
                        static_cast<std::uint32_t>(id.len),
                        static_cast<std::uint32_t>(title.len),
                        static_cast<std::uint32_t>(description.len),
                        0};
}

// 填写第 index 条节点证据指向的节点。
inline void set_evidence_target(Node* node, std::size_t index, Node* target) noexcept {
  const_cast<EvidenceList*>(node->evidence)->entries()[index].node = target;
}

// 整个写入一个列表。count 不能超过容量。
inline void set_links(Node* node, LinkKind kind, Node* const* nodes, std::size_t count) noexcept {
  auto* list = const_cast<NodeList*>(link_list(node, kind));
  list->count = static_cast<std::uint32_t>(count);
  if (count != 0) {
    std::memcpy(list->nodes(), nodes, count * sizeof(Node*));
  }
}

// 确保列表还能再放 extra 个节点，放不下就搬到更大的内存里。内存不足时返回 false，列表不变。
inline bool reserve_links(Node* node, LinkKind kind, std::size_t extra) noexcept {
  const NodeList* list = link_list(node, kind);
  if (extra <= list->capacity - list->count) {
    return true;
  }
  constexpr std::size_t kMaxCount = std::numeric_limits<std::uint32_t>::max();
  if (extra > kMaxCount - list->count) {
    return false;
  }
  std::size_t capacity = list->count + extra;
  if (capacity < std::size_t{list->capacity} * 2) {
    capacity = std::size_t{list->capacity} * 2;
  }
  if (capacity < 4) {
    capacity = 4;
  }
  if (capacity > kMaxCount) {
    capacity = kMaxCount;
  }

  char* memory = new (std::nothrow) char[sizeof(NodeList) + capacity * sizeof(Node*)];
  if (memory == nullptr) {
    return false;
  }
  auto* grown = new (memory) NodeList{list->count, static_cast<std::uint32_t>(capacity)};
  if (list->count != 0) {
    std::memcpy(grown->nodes(), list->nodes(), list->count * sizeof(Node*));
  }
  // 原来的列表如果在节点的内存块里，就随节点一起释放；只有单独分配的才要在这里释放。
  if ((node->flags & heap_flag(kind)) != 0) {
    delete[] reinterpret_cast<const char*>(list);
  }
  replace_link_list(node, kind, grown);
  node->flags |= heap_flag(kind);
  return true;
}

// 在列表末尾追加一个节点。调用前必须用 reserve_links 预留位置，所以不会失败。
inline void append_link(Node* node, LinkKind kind, Node* target) noexcept {
  auto* list = const_cast<NodeList*>(link_list(node, kind));
  list->nodes()[list->count] = target;
  ++list->count;
}

// 从列表里去掉指向 target 的所有地址，其余顺序不变。
inline void remove_link(Node* node, LinkKind kind, const Node* target) noexcept {
  auto* list = const_cast<NodeList*>(link_list(node, kind));
  std::uint32_t kept = 0;
  for (std::uint32_t i = 0; i < list->count; ++i) {
    if (list->nodes()[i] != target) {
      list->nodes()[kept] = list->nodes()[i];
      ++kept;
    }
  }
  list->count = kept;
}

// 清除其他节点里所有指向 node 的地址，之后释放它不会留下悬空指针。
//
// 这里只保证内存安全，不判断知识关系：依赖 node 的节点保留证据的 id，node 字段置空，
// 它们的结论是否还成立由模型决定。
inline void detach_node(Node* node) noexcept {
  const NodeList* related_by = link_list(node, LinkKind::kRelatedBy);
  for (std::uint32_t i = 0; i < related_by->count; ++i) {
    remove_link(related_by->nodes()[i], LinkKind::kRelated, node);
  }
  const NodeList* related = link_list(node, LinkKind::kRelated);
  for (std::uint32_t i = 0; i < related->count; ++i) {
    remove_link(related->nodes()[i], LinkKind::kRelatedBy, node);
  }
  const EvidenceList* evidence = node->evidence;
  for (std::size_t i = 0; i < evidence->count; ++i) {
    if (Node* target = evidence->entries()[i].node) {
      remove_link(target, LinkKind::kDependents, node);
    }
  }
  const NodeList* dependents = link_list(node, LinkKind::kDependents);
  for (std::uint32_t i = 0; i < dependents->count; ++i) {
    auto* entries = const_cast<EvidenceList*>(dependents->nodes()[i]->evidence);
    for (std::size_t e = 0; e < entries->count; ++e) {
      if (entries->entries()[e].node == node) {
        entries->entries()[e].node = nullptr;
      }
    }
  }
}

// 释放节点。释放前必须先调用 detach_node，除非整张图都在销毁。
inline void delete_node(Node* node) noexcept {
  for (LinkKind kind : {LinkKind::kRelated, LinkKind::kDependents, LinkKind::kRelatedBy}) {
    if ((node->flags & heap_flag(kind)) != 0) {
      delete[] reinterpret_cast<const char*>(link_list(node, kind));
    }
  }
  delete[] reinterpret_cast<const char*>(node->evidence);
  node->~Node();
  ::operator delete(node, kNodeAlignment);
}

}  // namespace memweave

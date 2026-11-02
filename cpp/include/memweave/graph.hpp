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
#include <utility>

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
  const NodeList* based_by;  // 谁依赖我：证据里引用了这个节点的节点。这个节点变了，它们要复查。
  const NodeList* related_by;  // 谁关联我：related 里有这个节点的节点。删除这个节点时要清理它们。
  char* cold;                  // 冷块的开头，释放节点时用。
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
  std::uint32_t flags;  // 哪些内容已经搬到单独分配的内存里，释放节点时要单独释放它们。
};

static_assert(sizeof(Node) == 64, "节点必须正好占一条缓存行");
static_assert(sizeof(NodeList) == sizeof(std::size_t));
static_assert(sizeof(EvidenceList) == sizeof(std::size_t));
static_assert(sizeof(Evidence) == 40);
static_assert(sizeof(Backlinks) == 3 * sizeof(void*));

// 按一对相邻缓存行对齐，硬件的相邻行预取通常会把节点和关联列表一起取回。
inline constexpr std::align_val_t kNodeAlignment{128};

// 节点的三个列表。
enum class LinkKind : std::uint32_t {
  kRelated = 0,
  kBasedBy = 1,
  kRelatedBy = 2,
};

inline const NodeList* link_list(const Node* node, LinkKind kind) noexcept {
  switch (kind) {
    case LinkKind::kRelated:
      return node->related;
    case LinkKind::kBasedBy:
      return node->backlinks->based_by;
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
    case LinkKind::kBasedBy:
      backlinks->based_by = list;
      break;
    case LinkKind::kRelatedBy:
      backlinks->related_by = list;
      break;
  }
}

inline std::uint32_t heap_flag(LinkKind kind) noexcept {
  return std::uint32_t{1} << static_cast<std::uint32_t>(kind);
}

// flags 里的其他位：修改过的内容不再放在冷块里。
inline constexpr std::uint32_t kTitleOnHeap = std::uint32_t{1} << 3;
inline constexpr std::uint32_t kDescriptionOnHeap = std::uint32_t{1} << 4;
inline constexpr std::uint32_t kEvidenceOnHeap = std::uint32_t{1} << 5;

// 创建节点，并预留 related_capacity 条关联，以及两个反向列表的位置。
// evidence 里的 id 会被复制，node 字段之后用 set_evidence_target 填写。
// 内存不足或文本超过 4 GiB 时返回 nullptr。
inline Node* new_node(Bytes id, Bytes title, Bytes description, const Evidence* evidence,
                      std::size_t evidence_count, std::size_t related_capacity,
                      std::size_t based_by_capacity, std::size_t related_by_capacity) noexcept {
  constexpr std::size_t kMax = std::numeric_limits<std::size_t>::max();
  constexpr std::size_t kMaxText = std::numeric_limits<std::uint32_t>::max();
  constexpr std::size_t kMaxCount = std::numeric_limits<std::uint32_t>::max();
  if (id.len > kMaxText || title.len > kMaxText || description.len > kMaxText ||
      evidence_count > kMaxCount || related_capacity > kMaxCount ||
      based_by_capacity > kMaxCount || related_by_capacity > kMaxCount) {
    return nullptr;
  }

  // 冷块依次放证据列表、反向列表和文本。证据列表在最前面，释放时只需要释放它。
  const std::size_t lists_size = sizeof(EvidenceList) + evidence_count * sizeof(Evidence) +
                                 sizeof(Backlinks) + sizeof(NodeList) +
                                 based_by_capacity * sizeof(Node*) + sizeof(NodeList) +
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
  auto* based_by =
      new (backlinks + 1) NodeList{0, static_cast<std::uint32_t>(based_by_capacity)};
  auto* related_by = new (based_by->nodes() + based_by_capacity)
      NodeList{0, static_cast<std::uint32_t>(related_by_capacity)};
  new (backlinks) Backlinks{based_by, related_by, cold};

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
// 调用前 node 的 based_by 必须是空的：还有结论依据它时不能删除，由调用方先处理。
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
      remove_link(target, LinkKind::kBasedBy, node);
    }
  }
}

// 节点的文本字段。id 不能修改，别的节点的证据里还存着它。
enum class TextField : std::uint32_t {
  kTitle = 0,
  kDescription = 1,
};

// 替换标题或描述。内存不足或文本超过 4 GiB 时返回 false，内容不变。
inline bool set_text(Node* node, TextField field, Bytes text) noexcept {
  if (text.len > std::numeric_limits<std::uint32_t>::max()) {
    return false;
  }
  char* memory = new (std::nothrow) char[text.len == 0 ? 1 : text.len];
  if (memory == nullptr) {
    return false;
  }
  if (text.len != 0) {
    std::memcpy(memory, text.data, text.len);
  }
  const bool is_title = field == TextField::kTitle;
  const char*& pointer = is_title ? node->title : node->description;
  std::uint32_t& length = is_title ? node->title_len : node->description_len;
  const std::uint32_t flag = is_title ? kTitleOnHeap : kDescriptionOnHeap;
  if ((node->flags & flag) != 0) {
    delete[] pointer;
  }
  pointer = memory;
  length = static_cast<std::uint32_t>(text.len);
  node->flags |= flag;
  return true;
}

inline bool list_contains(const NodeList* list, const Node* target) noexcept {
  for (std::uint32_t i = 0; i < list->count; ++i) {
    if (list->nodes()[i] == target) {
      return true;
    }
  }
  return false;
}

// 新增一条关联 from -> to，同时维护 to 的反向列表。内存不足时返回 false，图不变。
inline bool link_nodes(Node* from, Node* to) noexcept {
  const bool has_reverse = list_contains(link_list(to, LinkKind::kRelatedBy), from);
  if (!reserve_links(from, LinkKind::kRelated, 1) ||
      (!has_reverse && !reserve_links(to, LinkKind::kRelatedBy, 1))) {
    return false;
  }
  append_link(from, LinkKind::kRelated, to);
  if (!has_reverse) {
    append_link(to, LinkKind::kRelatedBy, from);
  }
  return true;
}

// 去掉关联 from -> to，同时维护 to 的反向列表。
inline void unlink_nodes(Node* from, Node* to) noexcept {
  remove_link(from, LinkKind::kRelated, to);
  remove_link(to, LinkKind::kRelatedBy, from);
}

// 重写证据列表：去掉 dropped 选中的证据，再追加 extra。
//
// 新列表和它用到的所有 id 文本放在同一块内存里，旧的那块可以直接释放。内存不足时返回 false，列表不变。
template <typename Dropped>
inline bool rewrite_evidence(Node* node, Dropped dropped, const Evidence* extra) noexcept {
  const EvidenceList* old = node->evidence;

  std::size_t count = extra == nullptr ? 0 : 1;
  std::size_t text_size = extra == nullptr ? 0 : extra->id_len;
  for (std::size_t i = 0; i < old->count; ++i) {
    if (!dropped(old->entries()[i])) {
      ++count;
      text_size += old->entries()[i].id_len;
    }
  }

  const std::size_t lists_size = sizeof(EvidenceList) + count * sizeof(Evidence);
  char* memory = new (std::nothrow) char[lists_size + text_size];
  if (memory == nullptr) {
    return false;
  }
  auto* list = new (memory) EvidenceList{count};
  char* cursor = memory + lists_size;
  std::size_t written = 0;
  const auto copy_entry = [&](const Evidence& entry) {
    Evidence copied = entry;
    if (entry.id_len != 0) {
      std::memcpy(cursor, entry.id, entry.id_len);
    }
    copied.id = cursor;
    cursor += entry.id_len;
    new (list->entries() + written) Evidence(copied);
    ++written;
  };
  for (std::size_t i = 0; i < old->count; ++i) {
    if (!dropped(old->entries()[i])) {
      copy_entry(old->entries()[i]);
    }
  }
  if (extra != nullptr) {
    copy_entry(*extra);
  }

  if ((node->flags & kEvidenceOnHeap) != 0) {
    delete[] reinterpret_cast<const char*>(old);
  }
  node->evidence = list;
  node->flags |= kEvidenceOnHeap;
  return true;
}

// 新增一条节点证据：node 依据 target，同时维护 target 的被依赖列表。内存不足时返回 false，图不变。
inline bool add_node_evidence(Node* node, Node* target) noexcept {
  const bool has_reverse = list_contains(link_list(target, LinkKind::kBasedBy), node);
  if (!has_reverse && !reserve_links(target, LinkKind::kBasedBy, 1)) {
    return false;
  }
  const Evidence extra{EvidenceKind::kNode, target->id_len, target->id, target, 0, 0};
  if (!rewrite_evidence(node, [](const Evidence&) { return false; }, &extra)) {
    return false;
  }
  if (!has_reverse) {
    append_link(target, LinkKind::kBasedBy, node);
  }
  return true;
}

// 去掉指向 id 的节点证据，同时维护对方的被依赖列表。
// 内存不足时返回 false，图不变。
inline bool remove_node_evidence(Node* node, Bytes id) noexcept {
  Node* target = nullptr;
  const EvidenceList* old = node->evidence;
  for (std::size_t i = 0; i < old->count; ++i) {
    const Evidence& entry = old->entries()[i];
    if (entry.kind == EvidenceKind::kNode && entry.id_len == id.len &&
        std::memcmp(entry.id, id.data, id.len) == 0 && entry.node != nullptr) {
      target = entry.node;
    }
  }
  const auto dropped = [&id](const Evidence& entry) {
    return entry.kind == EvidenceKind::kNode && entry.id_len == id.len &&
           std::memcmp(entry.id, id.data, id.len) == 0;
  };
  if (!rewrite_evidence(node, dropped, nullptr)) {
    return false;
  }
  if (target != nullptr) {
    remove_link(target, LinkKind::kBasedBy, node);
  }
  return true;
}

// 新增一条来源证据：node 引用 source 的 start..end。内存不足时返回 false，图不变。
inline bool add_source_evidence(Node* node, Bytes source, std::uint64_t start,
                                std::uint64_t end) noexcept {
  const Evidence extra{EvidenceKind::kTextRange, static_cast<std::uint32_t>(source.len), source.data,
                       nullptr, start, end};
  return rewrite_evidence(node, [](const Evidence&) { return false; }, &extra);
}

// 去掉 source 的 start..end 这条来源证据，同一来源的其他范围保留。内存不足时返回 false，图不变。
inline bool remove_source_evidence(Node* node, Bytes source, std::uint64_t start,
                                   std::uint64_t end) noexcept {
  const auto dropped = [&](const Evidence& entry) {
    return entry.kind == EvidenceKind::kTextRange && entry.id_len == source.len &&
           std::memcmp(entry.id, source.data, source.len) == 0 && entry.start == start &&
           entry.end == end;
  };
  return rewrite_evidence(node, dropped, nullptr);
}

// 释放节点。释放前必须先调用 detach_node，除非整张图都在销毁。
inline void delete_node(Node* node) noexcept {
  for (LinkKind kind : {LinkKind::kRelated, LinkKind::kBasedBy, LinkKind::kRelatedBy}) {
    if ((node->flags & heap_flag(kind)) != 0) {
      delete[] reinterpret_cast<const char*>(link_list(node, kind));
    }
  }
  if ((node->flags & kTitleOnHeap) != 0) {
    delete[] node->title;
  }
  if ((node->flags & kDescriptionOnHeap) != 0) {
    delete[] node->description;
  }
  if ((node->flags & kEvidenceOnHeap) != 0) {
    delete[] reinterpret_cast<const char*>(node->evidence);
  }
  // 反向列表的结构体在冷块里，所以冷块最后释放。
  delete[] node->backlinks->cold;
  node->~Node();
  ::operator delete(node, kNodeAlignment);
}

// 草稿里被改过的节点的原版：第一次改这个节点之前存下来。
//
// 没被改过的节点不存，两个版本共用。这里只存地址和长度，文本和列表本身不拷贝：
// - 文本和证据：修改函数总是分配新内容，旧内容原样留着，所以只要把释放它的责任交给原版。
// - 关联列表：修改函数会原地改，所以第一次改某个列表之前，用 own_list 给当前版本一份拷贝。
//
// 之后节点里的 flags 只表示当前版本自己分配的内容，原版分配的记在 header.flags 里。
struct NodeImage {
  Node header;
  const NodeList* based_by;
  const NodeList* related_by;
};

// 存下原版。内存不足时返回 nullptr，节点不变。
inline NodeImage* save_node(Node* node) noexcept {
  auto* image = new (std::nothrow)
      NodeImage{*node, node->backlinks->based_by, node->backlinks->related_by};
  if (image != nullptr) {
    node->flags = 0;
  }
  return image;
}

// 让当前版本拥有这个列表自己的拷贝，之后可以原地修改。内存不足时返回 false，节点不变。
inline bool own_list(Node* node, LinkKind kind) noexcept {
  if ((node->flags & heap_flag(kind)) != 0) {
    return true;
  }
  const NodeList* list = link_list(node, kind);
  std::size_t capacity = std::size_t{list->count} + 1;
  if (capacity < 4) {
    capacity = 4;
  }
  if (capacity > std::numeric_limits<std::uint32_t>::max()) {
    return false;
  }
  char* memory = new (std::nothrow) char[sizeof(NodeList) + capacity * sizeof(Node*)];
  if (memory == nullptr) {
    return false;
  }
  auto* copy = new (memory) NodeList{list->count, static_cast<std::uint32_t>(capacity)};
  if (list->count != 0) {
    std::memcpy(copy->nodes(), list->nodes(), list->count * sizeof(Node*));
  }
  replace_link_list(node, kind, copy);
  node->flags |= heap_flag(kind);
  return true;
}

// 交换节点和原版的内容，交换两次就回到原样。只换地址，所以不分配内存，也不会失败。
inline void swap_image(Node* node, NodeImage* image) noexcept {
  std::swap(*node, image->header);
  auto* backlinks = const_cast<Backlinks*>(node->backlinks);
  std::swap(backlinks->based_by, image->based_by);
  std::swap(backlinks->related_by, image->related_by);
}

inline const NodeList* image_list(const NodeImage* image, LinkKind kind) noexcept {
  switch (kind) {
    case LinkKind::kRelated:
      return image->header.related;
    case LinkKind::kBasedBy:
      return image->based_by;
    case LinkKind::kRelatedBy:
      return image->related_by;
  }
  return nullptr;
}

// 保存草稿：留下当前版本，释放原版独有的内容和 image。不会失败。
inline void keep_revision(Node* node, NodeImage* image) noexcept {
  const std::uint32_t saved_flags = image->header.flags;
  const auto settle = [&](const void* current, const void* saved, std::uint32_t flag) {
    if (current == saved) {
      // 这一项没改过，释放它的责任交回节点。
      node->flags |= saved_flags & flag;
    } else if ((saved_flags & flag) != 0) {
      delete[] static_cast<const char*>(saved);
    }
  };
  for (LinkKind kind : {LinkKind::kRelated, LinkKind::kBasedBy, LinkKind::kRelatedBy}) {
    const NodeList* current = link_list(node, kind);
    const NodeList* saved = image_list(image, kind);
    const std::uint32_t flag = heap_flag(kind);
    settle(current, saved, flag);
    // 放得下就搬回节点的内存块：关联列表紧贴节点，遍历时才有缓存优势。
    if (current != saved && (saved_flags & flag) == 0 && current->count <= saved->capacity) {
      auto* home = const_cast<NodeList*>(saved);
      home->count = current->count;
      if (current->count != 0) {
        std::memcpy(home->nodes(), current->nodes(), current->count * sizeof(Node*));
      }
      delete[] reinterpret_cast<const char*>(current);
      replace_link_list(node, kind, home);
      node->flags &= ~flag;
    }
  }
  settle(node->title, image->header.title, kTitleOnHeap);
  settle(node->description, image->header.description, kDescriptionOnHeap);
  settle(node->evidence, image->header.evidence, kEvidenceOnHeap);
  delete image;
}

// 放弃草稿：回到原版，释放当前版本独有的内容和 image。不会失败。
inline void keep_origin(Node* node, NodeImage* image) noexcept {
  const auto drop = [&](const void* current, const void* saved, std::uint32_t flag) {
    if (current != saved && (node->flags & flag) != 0) {
      delete[] static_cast<const char*>(current);
    }
  };
  for (LinkKind kind : {LinkKind::kRelated, LinkKind::kBasedBy, LinkKind::kRelatedBy}) {
    drop(link_list(node, kind), image_list(image, kind), heap_flag(kind));
  }
  drop(node->title, image->header.title, kTitleOnHeap);
  drop(node->description, image->header.description, kDescriptionOnHeap);
  drop(node->evidence, image->header.evidence, kEvidenceOnHeap);
  auto* backlinks = const_cast<Backlinks*>(node->backlinks);
  backlinks->based_by = image->based_by;
  backlinks->related_by = image->related_by;
  *node = image->header;
  delete image;
}

// 节点的 set：文本地址到内存地址。保证 agent 用文本地址访问的节点是存在的。
//
// 开放寻址，线性探测，删除时向前搬动后面的元素，所以没有墓碑。key 就是节点自己的 id，不额外存一份。
class NodeIndex {
 public:
  NodeIndex() noexcept = default;
  NodeIndex(const NodeIndex&) = delete;
  NodeIndex& operator=(const NodeIndex&) = delete;
  ~NodeIndex() { delete[] slots_; }

  std::size_t size() const noexcept { return size_; }

  Node* find(Bytes id) const noexcept {
    if (capacity_ == 0) {
      return nullptr;
    }
    const std::uint64_t hash = hash_of(id);
    for (std::size_t i = hash & (capacity_ - 1);; i = (i + 1) & (capacity_ - 1)) {
      const Slot& slot = slots_[i];
      if (slot.node == nullptr) {
        return nullptr;
      }
      if (slot.hash == hash && slot.node->id_len == id.len &&
          (id.len == 0 || std::memcmp(slot.node->id, id.data, id.len) == 0)) {
        return slot.node;
      }
    }
  }

  // 加入节点。id 不能已经存在。内存不足时返回 false，内容不变。
  bool insert(Node* node) noexcept {
    if ((size_ + 1) * 10 > capacity_ * 7 && !grow()) {
      return false;
    }
    place(node, hash_of(Bytes{node->id, node->id_len}));
    ++size_;
    return true;
  }

  // 去掉节点，节点不存在时什么也不做。
  void erase(Bytes id) noexcept {
    if (capacity_ == 0) {
      return;
    }
    const std::uint64_t hash = hash_of(id);
    const std::size_t mask = capacity_ - 1;
    std::size_t hole = hash & mask;
    while (true) {
      const Slot& slot = slots_[hole];
      if (slot.node == nullptr) {
        return;
      }
      if (slot.hash == hash && slot.node->id_len == id.len &&
          (id.len == 0 || std::memcmp(slot.node->id, id.data, id.len) == 0)) {
        break;
      }
      hole = (hole + 1) & mask;
    }
    // 把后面原本应该排在洞之前的元素搬过来，探测链才不会断。
    for (std::size_t next = (hole + 1) & mask; slots_[next].node != nullptr;
         next = (next + 1) & mask) {
      const std::size_t home = slots_[next].hash & mask;
      if (((next - home) & mask) >= ((next - hole) & mask)) {
        slots_[hole] = slots_[next];
        hole = next;
      }
    }
    slots_[hole] = Slot{nullptr, 0};
    --size_;
  }

  // 遍历：从 cursor 开始找下一个节点并移动 cursor，没有了返回 nullptr。顺序不固定。
  Node* next(std::size_t* cursor) const noexcept {
    while (*cursor < capacity_) {
      Node* node = slots_[*cursor].node;
      ++*cursor;
      if (node != nullptr) {
        return node;
      }
    }
    return nullptr;
  }

 private:
  struct Slot {
    Node* node;
    std::uint64_t hash;
  };

  // FNV-1a，最后再混合一次，让低位也足够均匀。
  static std::uint64_t hash_of(Bytes id) noexcept {
    std::uint64_t hash = 0xcbf29ce484222325ULL;
    for (std::size_t i = 0; i < id.len; ++i) {
      hash ^= static_cast<unsigned char>(id.data[i]);
      hash *= 0x100000001b3ULL;
    }
    hash ^= hash >> 32;
    return hash * 0x9e3779b97f4a7c15ULL;
  }

  void place(Node* node, std::uint64_t hash) noexcept {
    std::size_t i = hash & (capacity_ - 1);
    while (slots_[i].node != nullptr) {
      i = (i + 1) & (capacity_ - 1);
    }
    slots_[i] = Slot{node, hash};
  }

  bool grow() noexcept {
    const std::size_t capacity = capacity_ == 0 ? 16 : capacity_ * 2;
    Slot* slots = new (std::nothrow) Slot[capacity]();
    if (slots == nullptr) {
      return false;
    }
    Slot* old = slots_;
    const std::size_t old_capacity = capacity_;
    slots_ = slots;
    capacity_ = capacity;
    for (std::size_t i = 0; i < old_capacity; ++i) {
      if (old[i].node != nullptr) {
        place(old[i].node, old[i].hash);
      }
    }
    delete[] old;
    return true;
  }

  Slot* slots_ = nullptr;
  std::size_t capacity_ = 0;
  std::size_t size_ = 0;
};

}  // namespace memweave

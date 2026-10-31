// 记忆图的节点：节点之间直接用地址相连，内存用 new 分配。
//
// 查询时只读不分配，所以分配方式不影响遍历的操作数，只影响节点在内存里的位置。
// 每个节点两块内存：
// - 热块：节点和关联列表。节点占一条缓存行，遍历要读的字段都在里面，关联列表紧跟在后面。
// - 冷块：证据列表、被依赖列表和所有文本。遍历用不到，不占用遍历区域的缓存。
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

// 节点地址的列表，后面紧跟 count 个 Node*。关联和被依赖关系都用它。
struct NodeList {
  std::size_t count;

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

// 这个布局是和 Rust 共享的（src/ffi.rs），Rust 读取时直接解引用，不经过函数调用。
// 修改时两边要一起改，两边都有布局检查。
//
// 节点里没有文本本身：id、title、description 都是地址，文本放在冷块里。文本没有 \0 结尾，
// 所以每段都配一个长度（字节数）。原始数据也不在节点里，evidence 只记录它在哪，
// 原文留在固态硬盘的 sources/ 目录。
struct alignas(64) Node {
  // 关联列表的地址，列表紧跟在节点后面，存的是相关节点的内存地址。
  // 从一个节点走到下一个节点只读这个字段。检索时沿它往下爬。
  std::atomic<const NodeList*> related;
  // 节点地址文本，如 "ownership"。它是节点的文本名字，也是 JSON 文件名，模型回答时用的就是它。
  const char* id;
  // 标题，如 "所有权"。总表和“已保留节点”列表里会用到。
  const char* title;
  // 模型自己写的描述，是记忆的主体。查询时模型靠它判断是否保留节点。
  const char* description;
  // 我依赖谁：这条知识靠什么得出。来源证据指向原始数据的位置，节点证据指向依据节点。
  const EvidenceList* evidence;
  // 谁依赖我：证据里引用了这个节点的节点。纠错时顺着它找到受影响的结论。
  // 它可以从 evidence 反推，所以不存到磁盘，加载时计算。
  const NodeList* dependents;
  std::uint32_t id_len;
  std::uint32_t title_len;
  std::uint32_t description_len;
  // 还空 4 字节。
};

static_assert(sizeof(Node) == 64, "节点必须正好占一条缓存行");
static_assert(sizeof(NodeList) == sizeof(std::size_t));
static_assert(sizeof(EvidenceList) == sizeof(std::size_t));
static_assert(sizeof(Evidence) == 40);

// 按一对相邻缓存行对齐，硬件的相邻行预取通常会把节点和关联列表一起取回。
inline constexpr std::align_val_t kNodeAlignment{128};

// 创建节点，并预留 related_capacity 条关联和 dependents_capacity 条被依赖关系的位置。
// evidence 里的 id 会被复制，node 字段之后用 set_evidence_target 填写。
// 内存不足或文本超过 4 GiB 时返回 nullptr。
inline Node* new_node(Bytes id, Bytes title, Bytes description, const Evidence* evidence,
                      std::size_t evidence_count, std::size_t related_capacity,
                      std::size_t dependents_capacity) noexcept {
  constexpr std::size_t kMax = std::numeric_limits<std::size_t>::max();
  constexpr std::size_t kMaxText = std::numeric_limits<std::uint32_t>::max();
  constexpr std::size_t kMaxCount = kMax / 1024 / sizeof(Evidence);
  if (id.len > kMaxText || title.len > kMaxText || description.len > kMaxText ||
      evidence_count > kMaxCount || related_capacity > kMaxCount ||
      dependents_capacity > kMaxCount) {
    return nullptr;
  }

  // 冷块依次放证据列表、被依赖列表和文本。证据列表在最前面，释放时只需要释放它。
  const std::size_t lists_size = sizeof(EvidenceList) + evidence_count * sizeof(Evidence) +
                                 sizeof(NodeList) + dependents_capacity * sizeof(Node*);
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
  auto* dependents = new (evidence_list->entries() + evidence_count) NodeList{0};

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

  return new (hot) Node{{nullptr},
                        id_text,
                        title_text,
                        description_text,
                        evidence_list,
                        dependents,
                        static_cast<std::uint32_t>(id.len),
                        static_cast<std::uint32_t>(title.len),
                        static_cast<std::uint32_t>(description.len)};
}

// 下面三个函数只在建图时调用，这时还没有其他线程在读节点。

// 写入节点的关联。count 不能超过创建时预留的数量，每个节点只能设置一次。
inline void set_related(Node* node, Node* const* related, std::size_t count) noexcept {
  auto* list = new (node + 1) NodeList{count};
  if (count != 0) {
    std::memcpy(list->nodes(), related, count * sizeof(Node*));
  }
  node->related.store(list, std::memory_order_release);
}

// 填写第 index 条节点证据指向的节点。
inline void set_evidence_target(Node* node, std::size_t index, Node* target) noexcept {
  const_cast<EvidenceList*>(node->evidence)->entries()[index].node = target;
}

// 写入依赖这个节点的节点。count 不能超过创建时预留的数量。
inline void set_dependents(Node* node, Node* const* dependents, std::size_t count) noexcept {
  auto* list = const_cast<NodeList*>(node->dependents);
  list->count = count;
  if (count != 0) {
    std::memcpy(list->nodes(), dependents, count * sizeof(Node*));
  }
}

// 释放节点。调用时不能有其他线程在读这个节点。
inline void delete_node(Node* node) noexcept {
  delete[] reinterpret_cast<const char*>(node->evidence);
  node->~Node();
  ::operator delete(node, kNodeAlignment);
}

}  // namespace memweave

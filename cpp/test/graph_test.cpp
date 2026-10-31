// 记忆图增删的内存安全测试，用 AddressSanitizer 运行，它能发现悬空指针、越界和泄漏：
//
//   g++ -std=c++20 -g -fsanitize=address,undefined -Icpp/include cpp/test/graph_test.cpp -o /tmp/graph_test
//   /tmp/graph_test
//
// 随机增删改节点，并定期走遍所有活着的节点和它们的全部地址。只要留下一个悬空指针，
// 读取它就会被 AddressSanitizer 拦住。同时用 std::map 当参照，核对 NodeIndex 的结果。
#include <algorithm>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <map>
#include <random>
#include <string>
#include <vector>

#include "memweave/graph.hpp"

using namespace memweave;

namespace {

[[noreturn]] void fail(const char* message) {
  std::fprintf(stderr, "失败：%s\n", message);
  std::exit(1);
}

Bytes bytes(const std::string& text) { return Bytes{text.data(), text.size()}; }

bool contains(const NodeList* list, const Node* target) {
  return std::find(list->nodes(), list->nodes() + list->count, target) != list->nodes() + list->count;
}

template <typename T>
std::vector<T> unique(std::vector<T> items) {
  std::sort(items.begin(), items.end());
  items.erase(std::unique(items.begin(), items.end()), items.end());
  return items;
}

class Graph {
 public:
  ~Graph() {
    for (Node* node : live_) delete_node(node);
  }

  // 随机修改一个节点：文本、关联和证据。
  void change(std::mt19937& random) {
    if (live_.empty()) return;
    Node* node = live_[random() % live_.size()];
    Node* other = live_[random() % live_.size()];
    switch (random() % 6) {
      case 0: {
        const std::string text(random() % 200, 'x');
        if (!set_text(node, TextField::kDescription, bytes(text))) fail("分配失败");
        break;
      }
      case 1: {
        const std::string text(random() % 50, 'y');
        if (!set_text(node, TextField::kTitle, bytes(text))) fail("分配失败");
        break;
      }
      case 2:
        if (!link_nodes(node, other)) fail("分配失败");
        break;
      case 3:
        unlink_nodes(node, other);
        break;
      case 4:
        if (node != other && !has_evidence(node, other) && !has_evidence(other, node)) {
          if (!add_node_evidence(node, other)) fail("分配失败");
        }
        break;
      default: {
        // 既能去掉有效的证据，也能清除依据已被删除的证据。
        const EvidenceList* evidence = node->evidence;
        for (std::size_t i = 0; i < evidence->count; ++i) {
          const Evidence& entry = evidence->entries()[i];
          if (entry.kind == EvidenceKind::kNode) {
            const std::string id(entry.id, entry.id_len);
            if (!remove_node_evidence(node, bytes(id))) fail("分配失败");
            break;
          }
        }
      }
    }
  }

  std::size_t size() const { return live_.size(); }

  void add(std::mt19937& random) {
    std::vector<Node*> related = pick(random);
    std::vector<Node*> based_on = unique(pick(random));
    std::vector<Node*> incoming = unique(pick(random));

    const std::string id = "n" + std::to_string(next_id_++);
    const std::string source = "sha256:source";
    std::vector<Evidence> evidence{Evidence{EvidenceKind::kTextRange,
                                            static_cast<std::uint32_t>(source.size()), source.data(),
                                            nullptr, 0, 4}};
    std::vector<std::string> names;
    names.reserve(based_on.size());
    for (Node* target : based_on) {
      names.emplace_back(target->id, target->id_len);
      evidence.push_back(Evidence{EvidenceKind::kNode,
                                  static_cast<std::uint32_t>(names.back().size()),
                                  names.back().data(), nullptr, 0, 0});
    }

    Node* node = new_node(bytes(id), bytes("标题"), bytes("描述"), evidence.data(), evidence.size(),
                          related.size(), 0, incoming.size());
    if (node == nullptr) fail("分配失败");

    set_links(node, LinkKind::kRelated, related.data(), related.size());
    set_links(node, LinkKind::kRelatedBy, incoming.data(), incoming.size());
    for (std::size_t i = 0; i < based_on.size(); ++i) {
      set_evidence_target(node, i + 1, based_on[i]);
      if (!reserve_links(based_on[i], LinkKind::kBasedBy, 1)) fail("分配失败");
      append_link(based_on[i], LinkKind::kBasedBy, node);
    }
    for (Node* target : unique(related)) {
      if (!reserve_links(target, LinkKind::kRelatedBy, 1)) fail("分配失败");
      append_link(target, LinkKind::kRelatedBy, node);
    }
    for (Node* from : incoming) {
      if (!reserve_links(from, LinkKind::kRelated, 1)) fail("分配失败");
      append_link(from, LinkKind::kRelated, node);
    }
    if (!index_.insert(node)) fail("分配失败");
    expected_[id] = node;
    live_.push_back(node);
  }

  void remove(std::mt19937& random) {
    if (live_.empty()) return;
    const std::size_t index = random() % live_.size();
    Node* node = live_[index];
    live_[index] = live_.back();
    live_.pop_back();
    const std::string id(node->id, node->id_len);
    index_.erase(bytes(id));
    expected_.erase(id);
    detach_node(node);
    delete_node(node);
  }

  // 走遍所有地址：悬空指针会被 AddressSanitizer 拦住，关系不对称会被这里拦住。
  void check() const {
    if (index_.size() != expected_.size()) fail("NodeIndex 的数量不对");
    for (const auto& [id, node] : expected_) {
      if (index_.find(bytes(id)) != node) fail("NodeIndex 找到的节点不对");
    }
    const std::string gone = "n" + std::to_string(next_id_);
    if (index_.find(bytes(gone)) != nullptr) fail("NodeIndex 找到了不存在的节点");
    std::size_t cursor = 0;
    std::size_t visited = 0;
    while (Node* node = index_.next(&cursor)) {
      if (expected_.count(std::string(node->id, node->id_len)) == 0) fail("NodeIndex 遍历到了已删除的节点");
      ++visited;
    }
    if (visited != expected_.size()) fail("NodeIndex 遍历的数量不对");

    for (const Node* node : live_) {
      if (std::memchr(node->description, 0, node->description_len) != nullptr) fail("描述被破坏");
      const NodeList* related = link_list(node, LinkKind::kRelated);
      for (std::uint32_t i = 0; i < related->count; ++i) {
        if (!contains(link_list(related->nodes()[i], LinkKind::kRelatedBy), node)) {
          fail("related 没有对应的 related_by");
        }
      }
      const NodeList* related_by = link_list(node, LinkKind::kRelatedBy);
      for (std::uint32_t i = 0; i < related_by->count; ++i) {
        if (!contains(link_list(related_by->nodes()[i], LinkKind::kRelated), node)) {
          fail("related_by 没有对应的 related");
        }
      }
      const EvidenceList* evidence = node->evidence;
      for (std::size_t i = 0; i < evidence->count; ++i) {
        const Evidence& entry = evidence->entries()[i];
        if (std::memchr(entry.id, 0, entry.id_len) != nullptr) fail("证据的 id 被破坏");
        if (entry.node != nullptr && !contains(link_list(entry.node, LinkKind::kBasedBy), node)) {
          fail("证据没有对应的 based_by");
        }
      }
      const NodeList* based_by = link_list(node, LinkKind::kBasedBy);
      for (std::uint32_t i = 0; i < based_by->count; ++i) {
        const EvidenceList* entries = based_by->nodes()[i]->evidence;
        bool found = false;
        for (std::size_t e = 0; e < entries->count; ++e) {
          found = found || entries->entries()[e].node == node;
        }
        if (!found) fail("based_by 没有对应的证据");
      }
    }
  }

 private:
  static bool has_evidence(const Node* node, const Node* target) {
    const EvidenceList* evidence = node->evidence;
    for (std::size_t i = 0; i < evidence->count; ++i) {
      const Evidence& entry = evidence->entries()[i];
      if (entry.node == target ||
          (entry.kind == EvidenceKind::kNode && entry.id_len == target->id_len &&
           std::memcmp(entry.id, target->id, target->id_len) == 0)) {
        return true;
      }
    }
    return false;
  }

  // 选几个活着的节点。个数偶尔很大，用来触发列表搬家。
  std::vector<Node*> pick(std::mt19937& random) const {
    std::vector<Node*> nodes;
    if (live_.empty()) return nodes;
    const std::size_t count = random() % 10 == 0 ? random() % 32 : random() % 4;
    for (std::size_t i = 0; i < count; ++i) {
      nodes.push_back(live_[random() % live_.size()]);
    }
    return nodes;
  }

  std::vector<Node*> live_;
  NodeIndex index_;
  std::map<std::string, Node*> expected_;
  std::size_t next_id_ = 0;
};

}  // namespace

int main() {
  std::mt19937 random(20261031);
  Graph graph;
  std::size_t removed = 0;
  for (int step = 0; step < 20000; ++step) {
    // 节点数在 300 上下来回变化。
    const bool grow = graph.size() < 300 ? random() % 100 < 60 : random() % 100 < 40;
    if (random() % 3 == 0) {
      graph.change(random);
    } else if (grow) {
      graph.add(random);
    } else {
      graph.remove(random);
      ++removed;
    }
    if (step % 50 == 0) graph.check();
  }
  graph.check();
  std::printf("通过：删除 %zu 次，最后剩 %zu 个节点\n", removed, graph.size());
  return 0;
}

// 内存池的并发压力测试，用 ThreadSanitizer 运行：
//
//   g++ -std=c++20 -O1 -g -fsanitize=thread -Icpp/include cpp/test/pool_stress.cpp -o /tmp/pool_stress
//   /tmp/pool_stress
//
// 部分内核上 ThreadSanitizer 要关闭 ASLR 才能启动：setarch $(uname -m) -R /tmp/pool_stress
//
// 释放线程和分配线程之间，除了内存池本身没有任何同步。释放线程写入的标记如果能被分配线程
// 读到，且 ThreadSanitizer 不报告数据竞争，才说明池的内存序是对的。
#include <atomic>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <thread>
#include <unordered_set>
#include <vector>

#include "memweave/memory_pool.hpp"

namespace {

constexpr std::size_t kThreads = 8;
constexpr std::size_t kBlocksPerThread = 20000;
constexpr std::size_t kRounds = 5;
constexpr std::size_t kSizes[] = {16, 48, 128, 4096};

struct Header {
  void* next_slot;  // 释放后被池用作 next。
  std::uint64_t tag;
};

[[noreturn]] void fail(const char* message) {
  std::fprintf(stderr, "失败：%s\n", message);
  std::exit(1);
}

std::uint64_t tag_for(const void* block) {
  return reinterpret_cast<std::uintptr_t>(block) ^ 0x5bd1e995u;
}

void run_round(memweave::MemoryPool& pool, std::size_t size) {
  std::vector<std::vector<void*>> assigned(kThreads);
  std::unordered_set<void*> original;
  for (auto& blocks : assigned) {
    for (std::size_t i = 0; i < kBlocksPerThread; ++i) {
      void* block = pool.allocate(size);
      if (block == nullptr) fail("分配失败");
      if (!original.insert(block).second) fail("同一个块被分配了两次");
      static_cast<Header*>(block)->tag = 0;
      blocks.push_back(block);
    }
  }

  std::atomic<std::size_t> finished{0};
  std::vector<std::thread> threads;
  for (auto& blocks : assigned) {
    threads.emplace_back([&pool, &blocks, &finished, size] {
      for (void* block : blocks) {
        static_cast<Header*>(block)->tag = tag_for(block);
        pool.deallocate(block, size);
      }
      // 只用来判断何时结束，用 relaxed，不给分配线程带来额外的同步。
      finished.fetch_add(1, std::memory_order_relaxed);
    });
  }

  // 分配线程和释放线程同时运行。
  std::unordered_set<void*> reused;
  std::vector<void*> fresh;
  const std::size_t total = kThreads * kBlocksPerThread;
  while (reused.size() < total) {
    const bool all_freed = finished.load(std::memory_order_relaxed) == kThreads;
    void* block = pool.allocate(size);
    if (block == nullptr) fail("分配失败");
    auto* header = static_cast<Header*>(block);
    if (original.count(block) != 0) {
      if (header->tag != tag_for(block)) fail("读到的块内容不对");
      if (!reused.insert(block).second) fail("块在归还前被分配了两次");
      header->tag = 0;  // 写入：如果还有线程在用这个块，ThreadSanitizer 会报告。
    } else {
      // 栈暂时是空的，池从 chunk 切了新块。
      fresh.push_back(block);
      if (all_freed && fresh.size() > total) fail("有块丢失了");
    }
  }

  for (auto& thread : threads) thread.join();
  for (void* block : fresh) pool.deallocate(block, size);
  for (void* block : reused) pool.deallocate(block, size);
}

}  // namespace

int main() {
  memweave::MemoryPool pool;
  for (std::size_t round = 0; round < kRounds; ++round) {
    for (std::size_t size : kSizes) {
      run_round(pool, size);
    }
  }
  std::puts("通过");
  return 0;
}

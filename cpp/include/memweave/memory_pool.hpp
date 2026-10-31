// 内存池：一个线程分配，任意线程释放。
//
// 每个大小级别一个空闲栈（Treiber 栈），next 指针直接存放在空闲块里。
//
// 为什么不会有 ABA：只有分配线程会弹出。分配线程在读栈顶和 CAS 之间，栈顶的块不可能被
// 别的线程弹走再压回来，其他线程只会压入新块，这只会让 CAS 失败重试。所以单字 CAS 就够了，
// 不需要版本计数。这也保证了读 head->next 时，这个块还在栈上、内容不会变。
//
// 这个保证依赖调用方遵守约定：allocate 只能由同一时刻唯一的一个线程调用。
#pragma once

#include <atomic>
#include <bit>
#include <cstddef>
#include <cstdint>
#include <new>

#if defined(__linux__) && !defined(MEMWEAVE_NO_HUGEPAGES)
#include <sys/mman.h>
#endif

namespace memweave {

class MemoryPool {
 public:
  MemoryPool() noexcept = default;
  MemoryPool(const MemoryPool&) = delete;
  MemoryPool& operator=(const MemoryPool&) = delete;

  // 所有块都归还给操作系统。调用时不能有其他线程在使用这个池。
  ~MemoryPool() {
    Chunk* chunk = chunks_;
    while (chunk != nullptr) {
      Chunk* next = chunk->next;
      ::operator delete(chunk, std::align_val_t{kChunkSize});
      chunk = next;
    }
  }

  // 只能由分配线程调用。内存不足时返回 nullptr。
  // 返回的块按自身大小对齐，上限 128 字节，所以 64 字节的块独占一条缓存行，
  // 128 字节及以上的块从一对相邻缓存行的开头开始，硬件的相邻行预取会一起取回。
  [[nodiscard]] void* allocate(std::size_t size) noexcept {
    if (size > kMaxSize) {
      return nullptr;
    }
    const std::size_t index = class_index(size);
    FreeStack& stack = free_[index];

    // 这个 acquire 与 deallocate 的 release 配对，保证读到释放线程写入的 next。
    Block* head = stack.head.load(std::memory_order_acquire);
    while (head != nullptr) {
      if (stack.head.compare_exchange_weak(head, head->next, std::memory_order_acquire,
                                           std::memory_order_acquire)) {
        return head;
      }
    }
    return allocate_fresh(class_size(index));
  }

  // 任意线程都可以调用。size 必须与分配时相同。
  void deallocate(void* pointer, std::size_t size) noexcept {
    FreeStack& stack = free_[class_index(size)];
    Block* block = static_cast<Block*>(pointer);
    block->next = stack.head.load(std::memory_order_relaxed);
    while (!stack.head.compare_exchange_weak(block->next, block, std::memory_order_release,
                                             std::memory_order_relaxed)) {
    }
  }

 private:
  struct Block {
    Block* next;
  };

  struct Chunk {
    Chunk* next;
  };

  // 独占缓存行，避免不同大小级别的释放互相造成伪共享。
  struct alignas(64) FreeStack {
    std::atomic<Block*> head{nullptr};
  };

  static constexpr std::size_t kMaxBlockAlignment = 128;
  static constexpr std::size_t kChunkHeader = kMaxBlockAlignment;
  static constexpr std::size_t kChunkSize = std::size_t{2} * 1024 * 1024;
  // 超过这个大小的块单独申请一个 chunk，避免浪费 chunk 的剩余空间。
  static constexpr std::size_t kMaxBumpSize = kChunkSize / 4;
  static constexpr unsigned kMinShift = 4;  // 最小 16 字节，放得下 next 指针。
  static constexpr unsigned kMaxShift = 47;
  static constexpr std::size_t kMaxSize = std::size_t{1} << kMaxShift;
  static constexpr std::size_t kClassCount = kMaxShift - kMinShift + 1;

  static std::size_t class_index(std::size_t size) noexcept {
    const std::size_t rounded = size < (std::size_t{1} << kMinShift) ? (std::size_t{1} << kMinShift)
                                                                     : size;
    return static_cast<std::size_t>(std::bit_width(rounded - 1)) - kMinShift;
  }

  static std::size_t class_size(std::size_t index) noexcept {
    return std::size_t{1} << (index + kMinShift);
  }

  void* allocate_fresh(std::size_t size) noexcept {
    if (size > kMaxBumpSize) {
      char* chunk = new_chunk(size);
      return chunk == nullptr ? nullptr : chunk + kChunkHeader;
    }

    const std::size_t alignment = size < kMaxBlockAlignment ? size : kMaxBlockAlignment;
    std::uintptr_t start = align_up(reinterpret_cast<std::uintptr_t>(bump_), alignment);
    if (bump_ == nullptr || start + size > reinterpret_cast<std::uintptr_t>(bump_end_)) {
      char* chunk = new_chunk(kChunkSize - kChunkHeader);
      if (chunk == nullptr) {
        return nullptr;
      }
      bump_ = chunk + kChunkHeader;
      bump_end_ = chunk + kChunkSize;
      start = reinterpret_cast<std::uintptr_t>(bump_);
    }
    bump_ = reinterpret_cast<char*>(start + size);
    return reinterpret_cast<void*>(start);
  }

  char* new_chunk(std::size_t payload) noexcept {
    // 按大页对齐并申请大页：节点随机分布时，TLB 未命中的代价和缓存未命中一样高。
    const std::size_t total = payload + kChunkHeader;
    void* memory = ::operator new(total, std::align_val_t{kChunkSize}, std::nothrow);
    if (memory == nullptr) {
      return nullptr;
    }
#if defined(__linux__) && !defined(MEMWEAVE_NO_HUGEPAGES)
    // 只是优化建议，失败也不影响使用。
    (void)madvise(memory, total, MADV_HUGEPAGE);
#endif
    Chunk* chunk = new (memory) Chunk{chunks_};
    chunks_ = chunk;
    return static_cast<char*>(memory);
  }

  static std::uintptr_t align_up(std::uintptr_t value, std::size_t alignment) noexcept {
    return (value + alignment - 1) & ~(static_cast<std::uintptr_t>(alignment) - 1);
  }

  FreeStack free_[kClassCount];

  // 下面的字段只有分配线程访问。
  alignas(64) Chunk* chunks_ = nullptr;
  char* bump_ = nullptr;
  char* bump_end_ = nullptr;
};

}  // namespace memweave

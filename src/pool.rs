//! C++ 内存池的 Rust 接口：一个线程分配，任意线程释放。

use std::ptr::NonNull;
use std::sync::Arc;

use crate::error::Error;
use crate::ffi;

/// 创建内存池，返回分配句柄和释放句柄。
///
/// [`Allocator`] 不能克隆，分配要求 `&mut self`，所以同一时刻只有一个线程在分配，
/// 这正是无锁空闲栈不会出现 ABA 问题的前提，由类型系统保证。
/// [`Deallocator`] 可以克隆给任意多个线程。
pub fn memory_pool() -> Result<(Allocator, Deallocator), Error> {
    let raw = NonNull::new(unsafe { ffi::mw_pool_new() }).ok_or(Error::OutOfMemory)?;
    let pool = Arc::new(Pool { raw });
    Ok((Allocator { pool: pool.clone() }, Deallocator { pool }))
}

struct Pool {
    raw: NonNull<ffi::RawPool>,
}

// 分配只能经过唯一的 Allocator，释放在 C++ 里是线程安全的。
unsafe impl Send for Pool {}
unsafe impl Sync for Pool {}

impl Drop for Pool {
    fn drop(&mut self) {
        unsafe { ffi::mw_pool_free(self.raw.as_ptr()) }
    }
}

/// 分配句柄。可以移动到别的线程，但只有一个。
pub struct Allocator {
    pool: Arc<Pool>,
}

impl Allocator {
    /// 分配至少 `size` 字节，至少按 16 字节对齐。内存在所有句柄都释放后归还给操作系统。
    pub fn allocate(&mut self, size: usize) -> Result<NonNull<u8>, Error> {
        let pointer = unsafe { ffi::mw_pool_allocate(self.pool.raw.as_ptr(), size) };
        NonNull::new(pointer.cast()).ok_or(Error::OutOfMemory)
    }
}

/// 释放句柄。可以克隆给任意多个线程。
#[derive(Clone)]
pub struct Deallocator {
    pool: Arc<Pool>,
}

impl Deallocator {
    /// 归还一个块。
    ///
    /// # Safety
    ///
    /// `pointer` 必须来自同一个池的 [`Allocator::allocate`]，`size` 必须与分配时相同，
    /// 每个块只能归还一次，归还后不能再使用。
    pub unsafe fn deallocate(&self, pointer: NonNull<u8>, size: usize) {
        unsafe { ffi::mw_pool_deallocate(self.pool.raw.as_ptr(), pointer.as_ptr().cast(), size) }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::{Mutex, mpsc};
    use std::thread;

    use super::*;

    /// 块的地址，用来在线程间传递所有权。
    struct Block(NonNull<u8>);
    unsafe impl Send for Block {}

    #[test]
    fn reuses_freed_blocks() {
        let (mut allocator, deallocator) = memory_pool().unwrap();

        let block = allocator.allocate(100).unwrap();
        unsafe { deallocator.deallocate(block, 100) };

        assert_eq!(allocator.allocate(100).unwrap(), block);
    }

    #[test]
    fn size_classes_are_separate() {
        let (mut allocator, deallocator) = memory_pool().unwrap();

        let small = allocator.allocate(32).unwrap();
        unsafe { deallocator.deallocate(small, 32) };

        assert_ne!(allocator.allocate(64).unwrap(), small);
        assert_eq!(allocator.allocate(17).unwrap(), small);
    }

    #[test]
    fn blocks_are_aligned_and_do_not_overlap() {
        let (mut allocator, _deallocator) = memory_pool().unwrap();
        let mut blocks = Vec::new();
        for size in [0, 1, 16, 17, 64, 100, 4096, 1 << 20, 1 << 22] {
            for _ in 0..100 {
                let address = allocator.allocate(size).unwrap().as_ptr() as usize;
                assert_eq!(address % 16, 0, "{size}");
                if size >= 128 {
                    assert_eq!(address % 128, 0, "{size}");
                } else if size >= 64 {
                    assert_eq!(address % 64, 0, "{size}");
                }
                blocks.push((address, size.max(1)));
            }
        }

        blocks.sort_unstable();
        for pair in blocks.windows(2) {
            assert!(pair[0].0 + pair[0].1 <= pair[1].0, "{pair:?}");
        }
    }

    #[test]
    fn an_absurd_size_fails_instead_of_crashing() {
        let (mut allocator, _deallocator) = memory_pool().unwrap();

        assert!(matches!(
            allocator.allocate(usize::MAX),
            Err(Error::OutOfMemory)
        ));
    }

    #[test]
    fn memory_outlives_the_allocator_for_late_frees() {
        let (mut allocator, deallocator) = memory_pool().unwrap();
        let block = allocator.allocate(32).unwrap();

        drop(allocator);

        unsafe { deallocator.deallocate(block, 32) };
    }

    /// 一个线程分配，多个线程释放：归还的块要能重新分配，并且不会同时分给两方。
    #[test]
    fn one_allocator_many_freers() {
        const SIZE: usize = 128;
        const THREADS: usize = 8;
        const BLOCKS: usize = 200_000;

        let (mut allocator, deallocator) = memory_pool().unwrap();
        let (sender, receiver) = mpsc::sync_channel::<Block>(1024);
        let receiver = Mutex::new(receiver);

        let reused = thread::scope(|scope| {
            for _ in 0..THREADS {
                let (receiver, deallocator) = (&receiver, deallocator.clone());
                scope.spawn(move || {
                    loop {
                        let block = receiver.lock().unwrap().recv();
                        let Ok(Block(pointer)) = block else { break };
                        // 块的前 8 字节在空闲时用作 next，标记写在后面。
                        let tag = unsafe { pointer.cast::<u64>().as_ptr().add(1) };
                        assert_eq!(unsafe { tag.read() }, pointer.as_ptr() as u64);
                        unsafe { deallocator.deallocate(pointer, SIZE) };
                    }
                });
            }

            let mut seen = HashSet::new();
            let mut reused = 0;
            for _ in 0..BLOCKS {
                let pointer = allocator.allocate(SIZE).unwrap();
                if !seen.insert(pointer.as_ptr() as usize) {
                    reused += 1;
                }
                let tag = unsafe { pointer.cast::<u64>().as_ptr().add(1) };
                unsafe { tag.write(pointer.as_ptr() as u64) };
                sender.send(Block(pointer)).unwrap();
            }
            drop(sender);
            reused
        });

        assert!(reused > 0, "归还的块应该被重新分配");
    }
}

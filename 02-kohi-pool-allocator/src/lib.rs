//! A port of Kohi's `pool_allocator` (`kohi.core/src/memory/allocators/pool_allocator.c`).
//!
//! A pool allocator splits one block into `capacity` slots of the same size.
//! Allocating takes the first free slot in O(1); freeing puts the slot back.
//!
//! Like the C version, free slots are tracked in a separate array of nodes, and
//! the free list is kept sorted by address, so allocation always returns the
//! lowest free slot and freeing is O(n).
//!
//! Differences from the C version:
//! - A full pool returns `AllocError`. The C version only checks with a debug
//!   assertion and dereferences a null pointer in release builds.
//! - `Drop` frees the slots and node array. The C `pool_allocator_destroy` is empty.
//! - Slots respect the alignment of the element type.
//! - Freeing a slot that is already free aborts instead of corrupting the list.

use std::alloc::{AllocError, Allocator, Global, Layout};
use std::cell::Cell;
use std::ptr::NonNull;

/// Marks the end of the free list. C: a null `next` pointer.
const NONE: usize = usize::MAX;

/// A pool of `capacity` slots, each big enough for one `slot` layout.
pub struct PoolAllocator {
    /// The slots, one after another. C: `void *memory`.
    memory: NonNull<u8>,
    /// Layout of the whole slot block, needed to free it.
    block_layout: Layout,
    /// Size of one slot, padded to its alignment. C: `u64 element_size`.
    slot_size: usize,
    /// `next[i]` is the free slot after slot `i`, or `NONE`. Its length is
    /// the capacity (C: `u64 capacity`).
    /// C: the `free_list_nodes` array, which stores pointers and offsets.
    next: Box<[Cell<usize>]>,
    /// First (lowest) free slot, or `NONE`. C: `free_list_head`.
    head: Cell<usize>,
    /// Number of free slots. The C version walks the list to count them.
    free_count: Cell<usize>,
}

impl PoolAllocator {
    /// Creates a pool of `capacity` slots for values of `slot` layout.
    /// C: `pool_allocator_create(element_size, capacity)`.
    ///
    /// # Panics
    /// If `capacity` or `slot.size()` is 0, or the pool size overflows.
    pub fn new(slot: Layout, capacity: usize) -> Self {
        assert!(capacity > 0, "pool capacity must be nonzero");
        assert!(slot.size() > 0, "pool slots must have a nonzero size");
        // `repeat` pads each slot to its alignment and checks for overflow.
        let (block_layout, slot_size) = slot.repeat(capacity).expect("pool size overflows");
        let memory = Global
            .allocate(block_layout)
            .unwrap_or_else(|_| std::alloc::handle_alloc_error(block_layout))
            .cast();

        // Link every slot to the next one, as the C version does.
        let next = (0..capacity)
            .map(|i| Cell::new(if i + 1 < capacity { i + 1 } else { NONE }))
            .collect();

        PoolAllocator {
            memory,
            block_layout,
            slot_size,
            next,
            head: Cell::new(0),
            free_count: Cell::new(capacity),
        }
    }

    /// Creates a pool with slots sized and aligned for `T`.
    pub fn for_type<T>(capacity: usize) -> Self {
        Self::new(Layout::new::<T>(), capacity)
    }

    /// Takes the lowest free slot and returns it with its index, or `None` if the
    /// pool is full. C: `pool_allocator_allocate(&pool, &out_index)`.
    pub fn allocate_slot(&self) -> Option<(NonNull<u8>, usize)> {
        let index = self.head.get();
        if index == NONE {
            return None;
        }
        self.head.set(self.next[index].get());
        self.free_count.set(self.free_count.get() - 1);
        // SAFETY: `index < capacity`, so the offset is inside the block.
        let ptr = unsafe { self.memory.add(index * self.slot_size) };
        Some((ptr, index))
    }

    /// Returns the slot index of a pointer into this pool.
    /// Returns `None` if `ptr` is not the start of one of this pool's slots.
    pub fn index_of(&self, ptr: NonNull<u8>) -> Option<usize> {
        let offset = ptr.addr().get().checked_sub(self.memory.addr().get())?;
        let index = offset / self.slot_size;
        (offset % self.slot_size == 0 && index < self.capacity()).then_some(index)
    }

    /// Number of free slots. C: `pool_allocator_elements_free`.
    pub fn elements_free(&self) -> usize {
        self.free_count.get()
    }

    /// Free bytes. C: `pool_allocator_space_free`.
    pub fn space_free(&self) -> usize {
        self.free_count.get() * self.slot_size
    }

    /// Size of one slot in bytes.
    pub fn slot_size(&self) -> usize {
        self.slot_size
    }

    /// Number of slots.
    pub fn capacity(&self) -> usize {
        self.next.len()
    }

    /// Moves `value` into a slot, or gives it back if the pool is full.
    ///
    /// `Box::try_new_in` is still unstable (`allocator_ext`), so this builds the
    /// `Box` from a slot with the stable `Box::from_raw_in`.
    pub fn try_box<T>(&self, value: T) -> Result<Box<T, &Self>, T> {
        match self.allocate(Layout::new::<T>()) {
            Ok(slot) => {
                let ptr = slot.cast::<T>().as_ptr();
                // SAFETY: the slot is big enough and aligned for `T`, and was
                // allocated by `self`, which the `Box` will free it with.
                unsafe {
                    ptr.write(value);
                    Ok(Box::from_raw_in(ptr, self))
                }
            }
            Err(AllocError) => Err(value),
        }
    }
}

/// Allocator methods must not unwind, so errors that C reports with `KFATAL` or
/// `KASSERT` abort here instead of panicking.
fn fatal(message: &str) -> ! {
    eprintln!("kohi_pool: {message}");
    std::process::abort()
}

// SAFETY:
// - Each slot is handed out at most once until it is freed, so blocks never overlap.
// - Slot memory stays valid until `Drop`, which needs every borrower to be gone.
// - `allocate` rejects layouts that do not fit a slot, so every block respects
//   the requested size and alignment.
// - Nothing here panics.
unsafe impl Allocator for PoolAllocator {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        if layout.size() > self.slot_size || layout.align() > self.block_layout.align() {
            return Err(AllocError);
        }
        let (ptr, _) = self.allocate_slot().ok_or(AllocError)?;
        // The whole slot is usable, so report its full size.
        Ok(NonNull::slice_from_raw_parts(ptr, self.slot_size))
    }

    /// C: `pool_allocator_free`. Inserts the slot into the free list in address order.
    unsafe fn deallocate(&self, ptr: NonNull<u8>, _layout: Layout) {
        let Some(index) = self.index_of(ptr) else {
            fatal("freed a pointer that is not a slot of this pool");
        };

        // Find the first free slot after `index`, as the C version does.
        let mut prev = NONE;
        let mut cur = self.head.get();
        while cur != NONE && cur < index {
            prev = cur;
            cur = self.next[cur].get();
        }
        if cur == index {
            fatal("double free of a pool slot");
        }

        self.next[index].set(cur);
        if prev == NONE {
            self.head.set(index);
        } else {
            self.next[prev].set(index);
        }
        self.free_count.set(self.free_count.get() + 1);
    }
}

impl Drop for PoolAllocator {
    /// C: `pool_allocator_destroy`, which is empty in Kohi and leaks both blocks.
    fn drop(&mut self) {
        // SAFETY: `memory` was allocated from `Global` with `block_layout`.
        // `next` is a `Box` and frees itself.
        unsafe { Global.deallocate(self.memory, self.block_layout) };
    }
}

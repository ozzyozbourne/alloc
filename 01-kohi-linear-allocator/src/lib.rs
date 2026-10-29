//! A port of Kohi's `linear_allocator` (`kohi.core/src/memory/allocators/linear_allocator.c`).
//!
//! A linear allocator hands out memory from one block by moving an offset forward.
//! Single allocations are never freed; [`LinearAllocator::free_all`] resets the
//! whole block at once.
//!
//! Differences from the C version:
//! - Allocations are aligned to the `Layout` they ask for. The C version ignores
//!   alignment, so mixing sizes can return misaligned pointers.
//! - `free_all` takes `&mut self`, so the compiler rejects any call while a `Vec`
//!   or `Box` still points into the block:
//!
//! ```compile_fail,E0502
//! use kohi_linear::LinearAllocator;
//! let mut frame = LinearAllocator::new(1024);
//! let v: Vec<u32, &LinearAllocator> = Vec::with_capacity_in(4, &frame);
//! frame.free_all(false); // error: `frame` is still borrowed by `v`
//! drop(v);
//! ```

use std::alloc::{AllocError, Allocator, Global, Layout};
use std::cell::Cell;
use std::marker::PhantomData;
use std::mem::MaybeUninit;
use std::ptr::NonNull;

/// Alignment of the block the allocator allocates for itself in [`LinearAllocator::new`].
const OWNED_BLOCK_ALIGN: usize = 16;

/// A bump allocator over one fixed block of memory.
///
/// `'buf` is the lifetime of a caller-provided buffer (see [`LinearAllocator::from_buffer`]).
/// An allocator that owns its block is `LinearAllocator<'static>`.
pub struct LinearAllocator<'buf> {
    /// Start of the block. C: `void *memory`.
    memory: NonNull<u8>,
    /// C: `u64 total_size`.
    total_size: usize,
    /// Bytes used so far, including alignment padding. C: `u64 allocated`.
    /// A `Cell`, because `Allocator::allocate` only gets `&self`.
    allocated: Cell<usize>,
    /// `Some(layout)` if this allocator allocated `memory` itself and must free it.
    /// C: `b8 owns_memory`.
    owned: Option<Layout>,
    /// Ties a borrowed buffer's lifetime to the allocator.
    _buffer: PhantomData<&'buf mut [MaybeUninit<u8>]>,
}

impl LinearAllocator<'static> {
    /// Creates an allocator that owns a new `total_size`-byte block from the global
    /// allocator. C: `linear_allocator_create(total_size, 0, &alloc)`.
    pub fn new(total_size: usize) -> Self {
        let layout = Layout::from_size_align(total_size.max(1), OWNED_BLOCK_ALIGN)
            .expect("linear allocator size overflows isize");
        let memory = Global
            .allocate(layout)
            .unwrap_or_else(|_| std::alloc::handle_alloc_error(layout));
        LinearAllocator {
            memory: memory.cast(),
            total_size,
            allocated: Cell::new(0),
            owned: Some(layout),
            _buffer: PhantomData,
        }
    }
}

impl<'buf> LinearAllocator<'buf> {
    /// Creates an allocator over a buffer the caller owns, such as a stack array or
    /// part of a larger block. C: `linear_allocator_create(size, memory, &alloc)`.
    pub fn from_buffer(buffer: &'buf mut [MaybeUninit<u8>]) -> Self {
        LinearAllocator {
            total_size: buffer.len(),
            memory: NonNull::from(buffer).cast(),
            allocated: Cell::new(0),
            owned: None,
            _buffer: PhantomData,
        }
    }

    /// Total size of the block in bytes.
    pub fn total_size(&self) -> usize {
        self.total_size
    }

    /// Bytes used so far, including alignment padding.
    pub fn allocated(&self) -> usize {
        self.allocated.get()
    }

    /// Bytes left before the allocator is full.
    pub fn remaining(&self) -> usize {
        self.total_size - self.allocated.get()
    }

    /// Resets the allocator to empty, optionally zeroing the block.
    /// C: `linear_allocator_free_all(&alloc, clear)`.
    ///
    /// Taking `&mut self` is what makes this safe: nothing allocated from `self`
    /// can still be alive when this runs.
    pub fn free_all(&mut self, clear: bool) {
        self.allocated.set(0);
        if clear {
            // SAFETY: `memory` is valid for writes of `total_size` bytes, and no
            // allocation from it is alive because we hold `&mut self`.
            unsafe { self.memory.write_bytes(0, self.total_size) };
        }
    }
}

// SAFETY:
// - Each returned block lies inside `memory[..total_size]` and starts at or after
//   the end of the previous one, so blocks never overlap.
// - Blocks stay valid until `free_all` (needs `&mut self`) or `Drop` runs, which
//   are the two ways the `Allocator` contract allows memory to be invalidated.
// - Nothing here panics.
unsafe impl Allocator for LinearAllocator<'_> {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        // Align the *address*, not just the offset, so any buffer alignment works.
        let base = self.memory.addr().get();
        let start = (base + self.allocated.get())
            .checked_next_multiple_of(layout.align())
            .ok_or(AllocError)?
            - base;
        let end = start
            .checked_add(layout.size())
            .filter(|&end| end <= self.total_size)
            .ok_or(AllocError)?;
        self.allocated.set(end);
        // SAFETY: `start <= end <= total_size`, so the pointer stays inside the block.
        let ptr = unsafe { self.memory.add(start) };
        Ok(NonNull::slice_from_raw_parts(ptr, layout.size()))
    }

    unsafe fn deallocate(&self, _ptr: NonNull<u8>, _layout: Layout) {
        // Single allocations are not freed; `free_all` frees everything at once.
    }
}

impl Drop for LinearAllocator<'_> {
    /// C: `linear_allocator_destroy`. Frees the block only if this allocator owns it.
    fn drop(&mut self) {
        if let Some(layout) = self.owned {
            // SAFETY: `memory` was allocated from `Global` with exactly `layout`.
            unsafe { Global.deallocate(self.memory, layout) };
        }
    }
}

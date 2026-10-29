//! A port of the RAD Debugger's `Arena` (`src/base/base_arena.c`).
//!
//! An arena is a stack of bytes. Allocating pushes onto the stack; you free by
//! popping back to an earlier position, never one allocation at a time. Like
//! the C version, the arena:
//! - reserves a large range of address space (64 MiB by default) and commits
//!   pages in steps (64 KiB by default) as the position moves forward;
//! - stores its bookkeeping in a 128-byte header at the start of its own memory;
//! - when a block fills up, chains a new block and keeps counting positions
//!   across blocks, so the arena still behaves like one long stack;
//! - saves and restores positions with [`Temp`] scopes.
//!
//! Differences from the C version:
//! - Popping takes `&mut self` and [`Temp`] holds `&mut Arena`, so the compiler
//!   rejects code that keeps using memory after the pop that frees it.
//! - Failure returns `AllocError` from the `Allocator` methods instead of
//!   aborting. The C-style helpers ([`Arena::push`]) still abort.
//! - `deallocate`, `grow` and `shrink` work in place on the most recent
//!   allocation, so a `Vec` growing at the top of an arena does not copy.
//! - Not ported: large pages, `ARENA_FREE_LIST` block reuse, the global debug
//!   table, ASan poisoning and Telemetry plots.
//!
//! ```compile_fail,E0502
//! use rad_arena::Arena;
//! let mut arena = Arena::new();
//! let name = arena.push_str("dangling");
//! arena.clear(); // error: `arena` is still borrowed by `name`
//! println!("{name}");
//! ```

use std::alloc::{AllocError, Allocator, Layout};
use std::cell::Cell;
use std::ops::{Deref, DerefMut};
use std::ptr::NonNull;

mod os;

/// Bytes reserved at the start of every block for its header.
/// C: `ARENA_HEADER_SIZE`.
pub const HEADER_SIZE: usize = 128;

/// Block header, stored in the first bytes of each block. C: the `Arena`
/// struct, which is also the first thing in each block's memory.
#[repr(C)]
struct Block {
    /// Previous block in the chain. C: `Arena *prev`.
    prev: Option<NonNull<Block>>,
    /// Position of this block's byte 0 in the arena as a whole. C: `base_pos`.
    base_pos: usize,
    /// Next free byte, counted from the start of this block. C: `pos`.
    pos: usize,
    /// Bytes committed so far. C: `cmt`.
    cmt: usize,
    /// Bytes reserved. C: `res`.
    res: usize,
}

const _: () = assert!(size_of::<Block>() <= HEADER_SIZE);

/// Settings for [`Arena::with_params`]. C: `ArenaParams`.
#[derive(Clone, Copy, Debug)]
pub struct ArenaParams {
    /// Address space each block reserves. C: `reserve_size`, default `MB(64)`.
    pub reserve_size: usize,
    /// Step in which pages are committed. C: `commit_size`, default `KB(64)`.
    pub commit_size: usize,
    /// Whether to chain a new block when one is full. C: the inverse of
    /// `ArenaFlag_NoChain`.
    pub chain: bool,
}

impl Default for ArenaParams {
    fn default() -> Self {
        ArenaParams {
            reserve_size: 64 << 20,
            commit_size: 64 << 10,
            chain: true,
        }
    }
}

/// A chained, virtual-memory-backed bump arena.
pub struct Arena {
    /// The block allocations currently go into. C: `arena->current`.
    current: Cell<NonNull<Block>>,
    /// Page-rounded settings, used when chaining new blocks.
    params: ArenaParams,
}

// SAFETY: an `Arena` owns its blocks outright, so moving it to another thread
// is fine. It is not `Sync`: `allocate` mutates through `&self` without locks.
unsafe impl Send for Arena {}

/// A saved arena position. C: the `pos` field of `Temp`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pos(usize);

impl Pos {
    /// The position in bytes, counted across all blocks and including headers.
    pub fn get(self) -> usize {
        self.0
    }
}

impl Arena {
    /// Creates an arena with the default 64 MiB reserve and 64 KiB commit step.
    /// C: `arena_alloc()`.
    pub fn new() -> Self {
        Self::with_params(ArenaParams::default())
    }

    /// C: `arena_alloc(.reserve_size = ..., .commit_size = ..., .flags = ...)`.
    ///
    /// # Panics
    /// If the OS refuses the reservation. C aborts in that case.
    pub fn with_params(params: ArenaParams) -> Self {
        let page = os::page_size();
        let reserve_size = params
            .reserve_size
            .max(HEADER_SIZE)
            .checked_next_multiple_of(page)
            .expect("reserve size overflows");
        let commit_size = params
            .commit_size
            .max(1)
            .checked_next_multiple_of(page)
            .expect("commit size overflows")
            .min(reserve_size);
        let params = ArenaParams {
            reserve_size,
            commit_size,
            ..params
        };
        let first = Self::new_block(reserve_size, commit_size, 0, None)
            .expect("failed to reserve arena memory");
        Arena {
            current: Cell::new(first),
            params,
        }
    }

    /// Reserves a block, commits its first `cmt` bytes and writes its header.
    fn new_block(
        res: usize,
        cmt: usize,
        base_pos: usize,
        prev: Option<NonNull<Block>>,
    ) -> Option<NonNull<Block>> {
        let base = os::reserve(res)?;
        // SAFETY: `base` is a fresh reservation of `res >= cmt` bytes.
        if !unsafe { os::commit(base, cmt) } {
            // SAFETY: `base` is the reservation we just made.
            unsafe { os::release(base, res) };
            return None;
        }
        let block = base.cast::<Block>();
        // SAFETY: the first `HEADER_SIZE` bytes are committed and unused.
        unsafe {
            block.write(Block {
                prev,
                base_pos,
                pos: HEADER_SIZE,
                cmt,
                res,
            })
        };
        Some(block)
    }

    /// The current position. C: `arena_pos`.
    pub fn pos(&self) -> Pos {
        // SAFETY: `current` always points at a live block header.
        let b = unsafe { self.current.get().as_ref() };
        Pos(b.base_pos + b.pos)
    }

    /// Number of blocks in the chain.
    pub fn block_count(&self) -> usize {
        let mut n = 0;
        let mut cur = Some(self.current.get());
        while let Some(b) = cur {
            n += 1;
            // SAFETY: every block in the chain is live.
            cur = unsafe { b.as_ref().prev };
        }
        n
    }

    /// Bytes committed in the current block.
    pub fn committed(&self) -> usize {
        // SAFETY: `current` always points at a live block header.
        unsafe { self.current.get().as_ref().cmt }
    }

    /// The core push. C: `arena_push(arena, size, align, zero)` without `zero`.
    fn push_raw(&self, layout: Layout) -> Result<NonNull<u8>, AllocError> {
        let mut block = self.current.get();
        // SAFETY: `current` is a live block; we copy the header out and write
        // fields back through the raw pointer, never holding a reference.
        let (mut start, mut end) = unsafe { Self::fit(block, layout)? };

        // C: "chain, if needed".
        // SAFETY: as above.
        if end > unsafe { (*block.as_ptr()).res } {
            if !self.params.chain {
                return Err(AllocError);
            }
            let page = os::page_size();
            let need = HEADER_SIZE
                .checked_add(layout.size())
                .and_then(|n| n.checked_add(layout.align()))
                .and_then(|n| n.checked_next_multiple_of(page))
                .ok_or(AllocError)?;
            let res = self.params.reserve_size.max(need);
            let cmt = self.params.commit_size.max(need.min(res));
            // SAFETY: as above.
            let base_pos = unsafe { (*block.as_ptr()).base_pos + (*block.as_ptr()).res };
            block = Self::new_block(res, cmt, base_pos, Some(block)).ok_or(AllocError)?;
            self.current.set(block);
            // SAFETY: `block` is the live block we just made.
            (start, end) = unsafe { Self::fit(block, layout)? };
        }

        // SAFETY: `end <= res` (checked or guaranteed by sizing above).
        unsafe { Self::commit_to(block, end, self.params.commit_size)? };
        // SAFETY: as above.
        unsafe { (*block.as_ptr()).pos = end };
        // SAFETY: `start <= end <= res`, so this stays inside the block.
        Ok(unsafe { block.cast::<u8>().add(start) })
    }

    /// Where `layout` would go in `block`: `(start, end)` offsets.
    ///
    /// # Safety
    /// `block` must be a live block header.
    unsafe fn fit(block: NonNull<Block>, layout: Layout) -> Result<(usize, usize), AllocError> {
        let addr = block.addr().get();
        // SAFETY: forwarded from the caller.
        let pos = unsafe { (*block.as_ptr()).pos };
        let start = (addr + pos)
            .checked_next_multiple_of(layout.align())
            .ok_or(AllocError)?
            - addr;
        let end = start.checked_add(layout.size()).ok_or(AllocError)?;
        Ok((start, end))
    }

    /// Commits pages until at least `end` bytes of `block` are usable.
    /// C: "commit new pages, if needed".
    ///
    /// # Safety
    /// `block` must be a live block header and `end <= res`.
    unsafe fn commit_to(block: NonNull<Block>, end: usize, step: usize) -> Result<(), AllocError> {
        let b = block.as_ptr();
        // SAFETY: forwarded from the caller.
        unsafe {
            if (*b).cmt >= end {
                return Ok(());
            }
            let new_cmt = end
                .checked_next_multiple_of(step)
                .ok_or(AllocError)?
                .min((*b).res);
            let from = block.cast::<u8>().add((*b).cmt);
            if !os::commit(from, new_cmt - (*b).cmt) {
                return Err(AllocError);
            }
            (*b).cmt = new_cmt;
        }
        Ok(())
    }

    /// Pops back to `pos`, releasing whole blocks above it. C: `arena_pop_to`.
    ///
    /// `&mut self` guarantees nothing allocated after `pos` is still in use.
    ///
    /// # Panics
    /// If `pos` is above the current position.
    pub fn pop_to(&mut self, pos: Pos) {
        let target = pos.0.max(HEADER_SIZE);
        let mut block = self.current.get();
        // SAFETY: every block in the chain is live; the first has `base_pos`
        // 0 < `target`, so the loop always stops at a live block.
        unsafe {
            while (*block.as_ptr()).base_pos >= target {
                let prev = (*block.as_ptr())
                    .prev
                    .expect("the first block has base_pos 0");
                os::release(block.cast(), (*block.as_ptr()).res);
                block = prev;
            }
            let new_pos = target - (*block.as_ptr()).base_pos;
            assert!(
                new_pos <= (*block.as_ptr()).pos,
                "pop_to a position above the current one"
            );
            (*block.as_ptr()).pos = new_pos;
        }
        self.current.set(block);
    }

    /// Pops `amount` bytes. C: `arena_pop`.
    pub fn pop(&mut self, amount: usize) {
        let pos = self.pos().0.saturating_sub(amount);
        self.pop_to(Pos(pos));
    }

    /// Pops everything. C: `arena_clear`.
    pub fn clear(&mut self) {
        self.pop_to(Pos(0));
    }

    /// Starts a temporary scope that pops back to the current position when
    /// dropped. C: `temp_begin`; dropping the guard is `temp_end`.
    pub fn temp(&mut self) -> Temp<'_> {
        Temp {
            pos: self.pos(),
            arena: self,
        }
    }

    /// Moves `value` into the arena. Like C's `push_array`, the value is never
    /// dropped; its memory is reclaimed by popping. Aborts on failure, as C does.
    // Returning `&mut` from `&self` is sound: every push returns fresh,
    // unaliased memory, and popping needs `&mut self`.
    #[allow(clippy::mut_from_ref)]
    pub fn push<T>(&self, value: T) -> &mut T {
        let layout = Layout::new::<T>();
        let p = self
            .push_raw(layout)
            .unwrap_or_else(|_| std::alloc::handle_alloc_error(layout))
            .cast::<T>();
        // SAFETY: fresh, aligned, exclusively ours until the arena is popped,
        // which needs `&mut self` and so ends this borrow first.
        unsafe {
            p.write(value);
            &mut *p.as_ptr()
        }
    }

    /// Copies `items` into the arena. C: `push_array_no_zero` + `MemoryCopy`.
    // Returning `&mut` from `&self` is sound: every push returns fresh,
    // unaliased memory, and popping needs `&mut self`.
    #[allow(clippy::mut_from_ref)]
    pub fn push_slice<T: Copy>(&self, items: &[T]) -> &mut [T] {
        let layout = Layout::for_value(items);
        let p = self
            .push_raw(layout)
            .unwrap_or_else(|_| std::alloc::handle_alloc_error(layout))
            .cast::<T>();
        // SAFETY: as in `push`; `p` has room for `items.len()` values.
        unsafe {
            p.copy_from_nonoverlapping(NonNull::from(items).cast(), items.len());
            std::slice::from_raw_parts_mut(p.as_ptr(), items.len())
        }
    }

    /// Copies a string into the arena. C: `push_str8_copy`.
    // Returning `&mut` from `&self` is sound: every push returns fresh,
    // unaliased memory, and popping needs `&mut self`.
    #[allow(clippy::mut_from_ref)]
    pub fn push_str(&self, s: &str) -> &mut str {
        let bytes = self.push_slice(s.as_bytes());
        // SAFETY: the bytes were copied from a `str`.
        unsafe { std::str::from_utf8_unchecked_mut(bytes) }
    }

    /// True if `ptr..ptr+size` ends exactly at the top of the current block.
    fn is_top(&self, ptr: NonNull<u8>, size: usize) -> bool {
        let block = self.current.get();
        // SAFETY: `current` is a live block header.
        let pos = unsafe { (*block.as_ptr()).pos };
        ptr.addr().get() + size == block.addr().get() + pos
    }
}

impl Default for Arena {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Arena {
    /// C: `arena_release`. Releases every block in the chain.
    fn drop(&mut self) {
        let mut cur = Some(self.current.get());
        while let Some(block) = cur {
            // SAFETY: each block is live until released here, exactly once.
            unsafe {
                cur = (*block.as_ptr()).prev;
                os::release(block.cast(), (*block.as_ptr()).res);
            }
        }
    }
}

// SAFETY:
// - Blocks are carved from disjoint ranges between the old and new position.
// - Memory stays valid until a pop (`&mut self`) or `Drop`, the two ways the
//   contract allows; `deallocate`/`shrink` only move the top down over the
//   block being freed, and `grow` only moves it up over fresh bytes.
// - Nothing here panics.
unsafe impl Allocator for Arena {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        let p = self.push_raw(layout)?;
        Ok(NonNull::slice_from_raw_parts(p, layout.size()))
    }

    /// Frees only the most recent allocation (by moving the top back over it).
    /// Everything else waits for a pop.
    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        if self.is_top(ptr, layout.size()) {
            let block = self.current.get();
            // SAFETY: the block ends at the top, so the top moves back to its
            // start, which is inside this block.
            unsafe { (*block.as_ptr()).pos = ptr.addr().get() - block.addr().get() };
        }
    }

    /// Grows the most recent allocation in place when it fits in the current
    /// block; otherwise allocates and copies, as the default does.
    unsafe fn grow(
        &self,
        ptr: NonNull<u8>,
        old: Layout,
        new: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        let block = self.current.get();
        if self.is_top(ptr, old.size()) && ptr.addr().get().is_multiple_of(new.align()) {
            let end = ptr.addr().get() - block.addr().get() + new.size();
            // SAFETY: `block` is live; we only commit up to `res`.
            if end <= unsafe { (*block.as_ptr()).res } {
                // SAFETY: as above.
                unsafe {
                    Self::commit_to(block, end, self.params.commit_size)?;
                    (*block.as_ptr()).pos = end;
                }
                return Ok(NonNull::slice_from_raw_parts(ptr, new.size()));
            }
        }
        let fresh = self.push_raw(new)?;
        // SAFETY: `ptr` holds `old.size()` live bytes; `fresh` is a new,
        // disjoint block at least that big.
        unsafe { fresh.copy_from_nonoverlapping(ptr, old.size()) };
        Ok(NonNull::slice_from_raw_parts(fresh, new.size()))
    }

    /// Keeps the same pointer. Moves the top back if this is the most recent allocation.
    unsafe fn shrink(
        &self,
        ptr: NonNull<u8>,
        old: Layout,
        new: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        if !ptr.addr().get().is_multiple_of(new.align()) {
            return Err(AllocError);
        }
        if self.is_top(ptr, old.size()) {
            let block = self.current.get();
            // SAFETY: the new top is inside the old allocation.
            unsafe { (*block.as_ptr()).pos -= old.size() - new.size() };
        }
        Ok(NonNull::slice_from_raw_parts(ptr, new.size()))
    }
}

/// A temporary scope. Allocate from it like an arena (it derefs to one); when
/// it is dropped, the arena pops back to where the scope started.
/// C: `Temp`, `temp_begin` and `temp_end`.
pub struct Temp<'a> {
    arena: &'a mut Arena,
    pos: Pos,
}

impl Temp<'_> {
    /// Where this scope started.
    pub fn start(&self) -> Pos {
        self.pos
    }
}

impl Deref for Temp<'_> {
    type Target = Arena;
    fn deref(&self) -> &Arena {
        self.arena
    }
}

impl DerefMut for Temp<'_> {
    /// Lets scopes nest: `temp.temp()` starts an inner scope.
    fn deref_mut(&mut self) -> &mut Arena {
        self.arena
    }
}

impl Drop for Temp<'_> {
    fn drop(&mut self) {
        self.arena.pop_to(self.pos);
    }
}

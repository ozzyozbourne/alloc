//! A size-class pool where every block belongs to the thread that carved it.
//! C: `mem/pool_arena.c` (Pony's default pool), much simplified.
//!
//! Pony allocates actors, messages and heap chunks from this pool. Memory is
//! carved into size-aligned slabs, and each slab has an owner thread. The
//! owner allocates and frees without atomics. A block freed by any other
//! thread is not touched by that thread's lists: it is chained, and every
//! [`BATCH`] blocks the whole chain goes onto the owner's inbox with one
//! compare-and-swap. The owner takes its whole inbox with one swap when it
//! runs out of blocks.
//!
//! Simplifications compared with Pony:
//! - Classes are 32 B to 4 KiB ([`MIN_CLASS`]..=[`MAX_CLASS`]); Pony goes to
//!   1 MiB. Anything larger goes straight to `Global`.
//! - A slab is one 64 KiB block of one class, from `Global`, instead of units
//!   inside 8 MiB arenas inside 256 MiB regions.
//! - There is no per-thread cache in front of the slabs, and memory is never
//!   returned to the OS. Like Pony, slabs and owners outlive their thread,
//!   because other threads may still hold blocks from them.
use std::alloc::{AllocError, Allocator, Global, Layout};
use std::cell::RefCell;
use std::ptr::{self, NonNull};
use std::sync::Mutex;
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

/// Smallest block. C: `POOL_MIN` (32 B with the arena pool).
pub const MIN_CLASS: usize = 32;
/// Largest block served from slabs.
pub const MAX_CLASS: usize = 4096;
const CLASSES: usize = (MAX_CLASS / MIN_CLASS).trailing_zeros() as usize + 1;
/// Size and alignment of a slab, so `addr & !(SLAB_SIZE - 1)` finds its header.
/// C: `arena_of(p)`, which masks with `ARENA_MASK`.
pub const SLAB_SIZE: usize = 64 * 1024;
/// Foreign frees are sent to their owner in batches of this many. C: `BATCH_SIZE`.
pub const BATCH: usize = 32;

/// A thread's inbox for blocks other threads freed. C: `inbox_t`.
struct Owner {
    inbox: AtomicPtr<Block>,
}

/// At the start of every slab. Written once, before any block is handed out.
#[repr(C)]
struct SlabHeader {
    owner: &'static Owner,
    class: usize,
}

/// A free block's first word. C: `pool_item_t`.
struct Block {
    next: *mut Block,
}

struct SlabPtr(#[allow(dead_code)] *mut u8);
// SAFETY: only stored so the slabs stay reachable; never dereferenced.
unsafe impl Send for SlabPtr {}

/// Every slab and owner ever created. Pony never unmaps arenas or reuses
/// owner slots either; keeping them here also makes them reachable for Miri.
static SLABS: Mutex<Vec<SlabPtr>> = Mutex::new(Vec::new());
static OWNERS: Mutex<Vec<&'static Owner>> = Mutex::new(Vec::new());
static BATCHES_SENT: AtomicUsize = AtomicUsize::new(0);

/// Blocks freed on this thread that belong to `owner`. C: `chain_t`.
struct Chain {
    owner: &'static Owner,
    head: *mut Block,
    tail: *mut Block,
    count: usize,
}

/// One thread's pool state. C: `this_thread` plus the per-class slabs.
struct ThreadPool {
    me: &'static Owner,
    free: [*mut Block; CLASSES],
    /// Unused end of the current slab per class: (next, end).
    bump: [(*mut u8, *mut u8); CLASSES],
    chains: Vec<Chain>,
}

thread_local! {
    static THREAD: RefCell<ThreadPool> = RefCell::new(ThreadPool::new());
}

fn class_of(layout: Layout) -> Option<usize> {
    let size = layout
        .size()
        .max(layout.align())
        .max(MIN_CLASS)
        .next_power_of_two();
    (size <= MAX_CLASS).then(|| (size / MIN_CLASS).trailing_zeros() as usize)
}

fn class_size(class: usize) -> usize {
    MIN_CLASS << class
}

/// The header of the slab `p` is in. C: `arena_of(p)`.
fn header_of(p: *mut u8) -> &'static SlabHeader {
    let slab = ptr::with_exposed_provenance::<SlabHeader>(p.addr() & !(SLAB_SIZE - 1));
    // SAFETY: `p` came from a slab, which is `SLAB_SIZE`-aligned, starts with
    // an initialized header that never changes, is never freed, and had its
    // provenance exposed when it was created.
    unsafe { &*slab }
}

/// Pushes the list `head..=tail` onto `owner`'s inbox with one CAS.
/// C: the push at the end of `chain_flush`.
///
/// # Safety
/// `head..=tail` must be a linked list of free blocks nobody else references.
unsafe fn push_inbox(owner: &Owner, head: *mut Block, tail: *mut Block) {
    let mut old = owner.inbox.load(Ordering::Relaxed);
    loop {
        // SAFETY: the caller hands us `tail`, a free block we own until the CAS.
        unsafe { (*tail).next = old };
        match owner
            .inbox
            .compare_exchange_weak(old, head, Ordering::Release, Ordering::Relaxed)
        {
            Ok(_) => return,
            Err(now) => old = now,
        }
    }
}

impl ThreadPool {
    fn new() -> Self {
        let me: &'static Owner = Box::leak(Box::new(Owner {
            inbox: AtomicPtr::new(ptr::null_mut()),
        }));
        OWNERS.lock().unwrap().push(me);
        ThreadPool {
            me,
            free: [ptr::null_mut(); CLASSES],
            bump: [(ptr::null_mut(), ptr::null_mut()); CLASSES],
            chains: Vec::new(),
        }
    }

    /// C: `ponyint_pool_alloc`: free list, then the inbox, then the slab,
    /// then a new slab.
    fn alloc(&mut self, class: usize) -> Option<*mut u8> {
        if let Some(p) = self.pop_free(class) {
            return Some(p);
        }
        self.drain_inbox();
        if let Some(p) = self.pop_free(class) {
            return Some(p);
        }
        let size = class_size(class);
        let (mut next, mut end) = self.bump[class];
        if next.is_null() || next.addr() + size > end.addr() {
            let layout = Layout::from_size_align(SLAB_SIZE, SLAB_SIZE).unwrap();
            let slab = Global.allocate(layout).ok()?.cast::<u8>().as_ptr();
            // SAFETY: the slab is fresh, aligned, and big enough for a header.
            unsafe {
                slab.cast::<SlabHeader>().write(SlabHeader {
                    owner: self.me,
                    class,
                })
            };
            slab.expose_provenance();
            SLABS.lock().unwrap().push(SlabPtr(slab));
            // The first `size` bytes hold the header (16 B <= MIN_CLASS), so
            // every block is at a multiple of its size and so aligned to it.
            next = slab.wrapping_add(size);
            end = slab.wrapping_add(SLAB_SIZE);
        }
        self.bump[class] = (next.wrapping_add(size), end);
        Some(next)
    }

    fn pop_free(&mut self, class: usize) -> Option<*mut u8> {
        let b = self.free[class];
        if b.is_null() {
            return None;
        }
        // SAFETY: blocks on our free lists are free and ours.
        self.free[class] = unsafe { (*b).next };
        Some(b.cast())
    }

    fn push_free(&mut self, class: usize, p: *mut u8) {
        let b = p.cast::<Block>();
        // SAFETY: `p` is a free block of at least `MIN_CLASS` bytes.
        unsafe {
            b.write(Block {
                next: self.free[class],
            })
        };
        self.free[class] = b;
    }

    /// C: `ponyint_pool_free`: own blocks go back on the free list, foreign
    /// ones onto their owner's chain.
    fn free(&mut self, p: *mut u8, class: usize) {
        let header = header_of(p);
        debug_assert_eq!(header.class, class);
        if ptr::eq(header.owner, self.me) {
            self.push_free(class, p);
            return;
        }
        let b = p.cast::<Block>();
        let i = match self
            .chains
            .iter()
            .position(|c| ptr::eq(c.owner, header.owner))
        {
            Some(i) => i,
            None => {
                self.chains.push(Chain {
                    owner: header.owner,
                    head: ptr::null_mut(),
                    tail: b,
                    count: 0,
                });
                self.chains.len() - 1
            }
        };
        let chain = &mut self.chains[i];
        // SAFETY: `p` is a free block; the chain now owns it.
        unsafe { b.write(Block { next: chain.head }) };
        if chain.head.is_null() {
            chain.tail = b;
        }
        chain.head = b;
        chain.count += 1;
        if chain.count >= BATCH {
            Self::flush_chain(chain);
        }
    }

    fn flush_chain(chain: &mut Chain) {
        if chain.head.is_null() {
            return;
        }
        // SAFETY: the chain is a list of free blocks only this thread references.
        unsafe { push_inbox(chain.owner, chain.head, chain.tail) };
        BATCHES_SENT.fetch_add(1, Ordering::Relaxed);
        chain.head = ptr::null_mut();
        chain.count = 0;
    }

    /// C: `inbox_drain`: take everything other threads returned, in one swap.
    fn drain_inbox(&mut self) {
        let mut b = self.me.inbox.swap(ptr::null_mut(), Ordering::Acquire);
        while !b.is_null() {
            // SAFETY: the inbox held free blocks; the swap made them ours.
            let next = unsafe { (*b).next };
            let class = header_of(b.cast()).class;
            self.push_free(class, b.cast());
            b = next;
        }
    }
}

impl Drop for ThreadPool {
    /// Thread exit: hand every pending chain to its owner.
    fn drop(&mut self) {
        for chain in &mut self.chains {
            Self::flush_chain(chain);
        }
    }
}

/// The pool, as an allocator. Zero-sized, `Copy`, and usable from any thread.
#[derive(Clone, Copy, Debug, Default)]
pub struct Pool;

// SAFETY: blocks never overlap (each comes from one free list or one bump),
// are aligned to their class size, and stay valid until deallocated: slabs
// are never freed, and a block is reused only after it is deallocated.
unsafe impl Allocator for Pool {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        if layout.size() == 0 {
            let p = NonNull::new(ptr::without_provenance_mut(layout.align())).unwrap();
            return Ok(NonNull::slice_from_raw_parts(p, 0));
        }
        let Some(class) = class_of(layout) else {
            return Global.allocate(layout);
        };
        // During thread teardown there is no pool state to allocate from.
        let p = THREAD
            .try_with(|t| t.borrow_mut().alloc(class))
            .ok()
            .flatten()
            .ok_or(AllocError)?;
        Ok(NonNull::slice_from_raw_parts(
            NonNull::new(p).ok_or(AllocError)?,
            class_size(class),
        ))
    }

    unsafe fn deallocate(&self, p: NonNull<u8>, layout: Layout) {
        if layout.size() == 0 {
            return;
        }
        let Some(class) = class_of(layout) else {
            // SAFETY: blocks of this layout came from `Global`.
            return unsafe { Global.deallocate(p, layout) };
        };
        let p = p.as_ptr();
        if THREAD.try_with(|t| t.borrow_mut().free(p, class)).is_err() {
            // This thread's pool state is gone: return the block on its own.
            // SAFETY: the caller gave up the block, so it is free and ours.
            unsafe { push_inbox(header_of(p).owner, p.cast(), p.cast()) };
            BATCHES_SENT.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Sends this thread's pending foreign frees to their owners now, instead of
/// waiting for a full batch. Pony does this when a scheduler thread goes idle
/// (`ponyint_pool_thread_cleanup`, `suspend_flush`).
pub fn flush() {
    let _ = THREAD.try_with(|t| {
        for chain in &mut t.borrow_mut().chains {
            ThreadPool::flush_chain(chain);
        }
    });
}

/// Counters for the whole process.
#[derive(Clone, Copy, Debug)]
pub struct PoolStats {
    /// Slabs carved so far (never freed).
    pub slabs: usize,
    /// Threads that have used the pool.
    pub threads: usize,
    /// Batches of foreign frees pushed onto an owner's inbox.
    pub batches_sent: usize,
}

/// Current [`PoolStats`].
pub fn stats() -> PoolStats {
    PoolStats {
        slabs: SLABS.lock().unwrap().len(),
        threads: OWNERS.lock().unwrap().len(),
        batches_sent: BATCHES_SENT.load(Ordering::Relaxed),
    }
}

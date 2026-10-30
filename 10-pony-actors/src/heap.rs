//! A per-actor heap. C: `mem/heap.c`.
//!
//! In Pony every actor has its own heap, and only that actor ever frees from
//! it, during its own GC between behaviours. Objects it sends to other actors
//! stay in its heap; those actors' references are counted (ORCA), and the
//! owner frees an object once the count says nobody else holds it.
//!
//! Rust has no tracing GC, so here the owner learns about frees directly:
//! - [`Local`] is the owning actor's handle. It allocates and frees with
//!   plain (non-atomic) free lists. It is `!Send`, so it, and every
//!   `Box`/`Vec` using it, stays on the owner's thread.
//! - [`Remote`] is the handle that travels with sent objects
//!   ([`Iso`](crate::Iso), [`Val`](crate::Val)). It can only free: it pushes
//!   the block onto the heap's atomic remote list. This is how snmalloc,
//!   which came out of the Pony-inspired Verona project, returns frees.
//! - [`Local::collect`] takes the whole remote list in one swap and puts the
//!   blocks back on the free lists. The actor calls it between batches of
//!   behaviours, where Pony would run `ponyint_heap_startgc`/`endgc`.
//!
//! Layout follows Pony: small objects (32-512 B, `HEAP_MINBITS`..`HEAP_MAXBITS`)
//! are carved from 1 KiB chunks taken from the [`Pool`]; anything larger is a
//! pool block of its own. Pony tracks free slots with a bitmap per chunk for
//! marking; with no marking, a free list per size class does the same job.
use std::alloc::{AllocError, Allocator, Layout};
use std::cell::UnsafeCell;
use std::marker::PhantomData;
use std::ptr::{self, NonNull};
use std::sync::Arc;
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

use crate::pool::Pool;

/// Smallest object. C: `HEAP_MIN` (1 << `HEAP_MINBITS`).
pub const HEAP_MIN: usize = 32;
/// Largest small object. C: `HEAP_MAX` (1 << `HEAP_MAXBITS`).
pub const HEAP_MAX: usize = 512;
/// Small objects are carved from chunks of this size. C: `POOL_ALIGN`.
pub const CHUNK: usize = 1024;
const CLASSES: usize = (HEAP_MAX / HEAP_MIN).trailing_zeros() as usize + 1;

/// A free small block. The class is stored so remote frees can be sorted.
#[repr(C)]
struct Free {
    next: *mut Free,
    class: usize,
}

struct HeapInner {
    /// Owner-only state, touched only through `Local`.
    free: UnsafeCell<[*mut Free; CLASSES]>,
    chunks: UnsafeCell<Vec<NonNull<u8>, Pool>>,
    /// Blocks freed through `Remote`, waiting for `collect`.
    remote: AtomicPtr<Free>,
    live: AtomicUsize,
    remote_frees: AtomicUsize,
}

// SAFETY: `free` and `chunks` are only accessed through `Local`, which is
// neither `Send` nor `Sync`, so every `Local` for a heap is on the thread that
// created it and those accesses never race. `Remote` touches only atomics.
// `Drop` has exclusive access.
unsafe impl Sync for HeapInner {}
// SAFETY: as above; the raw pointers are into chunks this heap owns.
unsafe impl Send for HeapInner {}

impl Drop for HeapInner {
    /// Runs when the actor and every object it sent are gone.
    fn drop(&mut self) {
        let chunk = Layout::from_size_align(CHUNK, CHUNK).unwrap();
        for &c in self.chunks.get_mut().iter() {
            // SAFETY: every chunk came from `Pool` with this layout.
            unsafe { Pool.deallocate(c, chunk) };
        }
    }
}

fn class_of(layout: Layout) -> Option<usize> {
    let size = layout
        .size()
        .max(layout.align())
        .max(HEAP_MIN)
        .next_power_of_two();
    (size <= HEAP_MAX).then(|| (size / HEAP_MIN).trailing_zeros() as usize)
}

fn dangling(layout: Layout) -> NonNull<[u8]> {
    let p = NonNull::new(ptr::without_provenance_mut(layout.align())).unwrap();
    NonNull::slice_from_raw_parts(p, 0)
}

/// The owning actor's handle to its heap.
#[derive(Clone)]
pub struct Local {
    inner: Arc<HeapInner>,
    _not_send: PhantomData<*const ()>,
}

/// A free-only handle to an actor's heap, sent along with its objects.
#[derive(Clone)]
pub struct Remote {
    inner: Arc<HeapInner>,
}

/// What a heap currently holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeapStats {
    /// Bytes handed out and not yet freed, rounded up to their size class.
    pub live_bytes: usize,
    /// 1 KiB chunks taken from the pool for small objects.
    pub chunks: usize,
    /// Objects freed by other threads, ever.
    pub remote_frees: usize,
}

impl Local {
    /// A new, empty heap. C: `ponyint_heap_init`.
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Local {
            inner: Arc::new(HeapInner {
                free: UnsafeCell::new([ptr::null_mut(); CLASSES]),
                chunks: UnsafeCell::new(Vec::new_in(Pool)),
                remote: AtomicPtr::new(ptr::null_mut()),
                live: AtomicUsize::new(0),
                remote_frees: AtomicUsize::new(0),
            }),
            _not_send: PhantomData,
        }
    }

    /// A handle that can free this heap's blocks from any thread.
    pub fn remote(&self) -> Remote {
        Remote {
            inner: self.inner.clone(),
        }
    }

    /// Moves every block other threads freed back onto the free lists and
    /// returns how many there were. Call it between behaviours.
    /// C: the role of `ponyint_heap_startgc` / `ponyint_heap_endgc`.
    pub fn collect(&self) -> usize {
        let mut f = self.inner.remote.swap(ptr::null_mut(), Ordering::Acquire);
        let mut n = 0;
        while !f.is_null() {
            // SAFETY: the swap made these remotely freed blocks ours.
            let (next, class) = unsafe { ((*f).next, (*f).class) };
            self.push_free(class, f);
            f = next;
            n += 1;
        }
        n
    }

    /// Current [`HeapStats`].
    pub fn stats(&self) -> HeapStats {
        HeapStats {
            live_bytes: self.inner.live.load(Ordering::Relaxed),
            // SAFETY: we are on the owner's thread (`Local` is `!Send`).
            chunks: unsafe { (*self.inner.chunks.get()).len() },
            remote_frees: self.inner.remote_frees.load(Ordering::Relaxed),
        }
    }

    /// True if both handles are for the same heap.
    pub fn same_heap(&self, other: &Remote) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    fn lists(&self) -> *mut [*mut Free; CLASSES] {
        self.inner.free.get()
    }

    fn push_free(&self, class: usize, f: *mut Free) {
        // SAFETY: owner's thread only; `f` is a free block of this heap.
        unsafe {
            let lists = &mut *self.lists();
            f.write(Free {
                next: lists[class],
                class,
            });
            lists[class] = f;
        }
    }

    /// Carves a fresh chunk into blocks of `class`. C: the new-chunk path of
    /// `ponyint_heap_alloc_small`.
    fn refill(&self, class: usize) -> Result<(), AllocError> {
        let chunk = Pool
            .allocate(Layout::from_size_align(CHUNK, CHUNK).unwrap())?
            .cast::<u8>();
        // SAFETY: owner's thread only.
        unsafe { (*self.inner.chunks.get()).push(chunk) };
        let size = HEAP_MIN << class;
        for i in (0..CHUNK / size).rev() {
            self.push_free(class, chunk.as_ptr().wrapping_add(i * size).cast());
        }
        Ok(())
    }
}

// SAFETY: small blocks come from chunks owned by the heap and are handed out
// once per free; large blocks are `Pool` blocks. Everything stays valid while
// any `Local` or `Remote` (they share the `Arc`) is alive.
unsafe impl Allocator for Local {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        if layout.size() == 0 {
            return Ok(dangling(layout));
        }
        let Some(class) = class_of(layout) else {
            let block = Pool.allocate(layout)?;
            self.inner.live.fetch_add(layout.size(), Ordering::Relaxed);
            return Ok(block);
        };
        // SAFETY: owner's thread only.
        if unsafe { (*self.lists())[class].is_null() } {
            self.refill(class)?;
        }
        // SAFETY: owner's thread only; the list is non-empty.
        let f = unsafe {
            let lists = &mut *self.lists();
            let f = lists[class];
            lists[class] = (*f).next;
            f
        };
        let size = HEAP_MIN << class;
        self.inner.live.fetch_add(size, Ordering::Relaxed);
        Ok(NonNull::slice_from_raw_parts(
            NonNull::new(f.cast()).unwrap(),
            size,
        ))
    }

    unsafe fn deallocate(&self, p: NonNull<u8>, layout: Layout) {
        if layout.size() == 0 {
            return;
        }
        match class_of(layout) {
            Some(class) => {
                self.inner
                    .live
                    .fetch_sub(HEAP_MIN << class, Ordering::Relaxed);
                self.push_free(class, p.as_ptr().cast());
            }
            None => {
                self.inner.live.fetch_sub(layout.size(), Ordering::Relaxed);
                // SAFETY: large blocks came from `Pool` with this layout.
                unsafe { Pool.deallocate(p, layout) }
            }
        }
    }
}

// SAFETY: `Remote` never allocates. It accepts any block of the heap it
// shares with `Local` (same `Arc`), which is what `Iso`/`Val` rely on when
// they rebuild a `Box` with a `Remote`.
unsafe impl Allocator for Remote {
    fn allocate(&self, _: Layout) -> Result<NonNull<[u8]>, AllocError> {
        Err(AllocError)
    }

    unsafe fn deallocate(&self, p: NonNull<u8>, layout: Layout) {
        if layout.size() == 0 {
            return;
        }
        let Some(class) = class_of(layout) else {
            self.inner.live.fetch_sub(layout.size(), Ordering::Relaxed);
            // SAFETY: large blocks came from `Pool`, which takes frees from any thread.
            return unsafe { Pool.deallocate(p, layout) };
        };
        self.inner
            .live
            .fetch_sub(HEAP_MIN << class, Ordering::Relaxed);
        self.inner.remote_frees.fetch_add(1, Ordering::Relaxed);
        let f = p.as_ptr().cast::<Free>();
        let mut head = self.inner.remote.load(Ordering::Relaxed);
        loop {
            // SAFETY: the caller gave up the block (at least `HEAP_MIN` bytes).
            unsafe { f.write(Free { next: head, class }) };
            match self.inner.remote.compare_exchange_weak(
                head,
                f,
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(now) => head = now,
            }
        }
    }
}

//! A port of Kohi's `dynamic_allocator` (`kohi.core/src/memory/allocators/dynamic_allocator.c`)
//! and of the parts of `kmemory.c` that sit on top of it: memory tags, usage
//! stats, a mutex, and use as the engine-wide allocator.
//!
//! The allocator manages one block with a [`kohi_freelist::FreeList`] of free
//! byte ranges. Each allocation takes a range big enough for the user data, a
//! header, alignment padding and (in debug builds) two guard regions:
//!
//! ```text
//! C:    [padding][u32 size][guard][user data][guard][alloc_header]
//! Rust: [padding][Header        ][guard][user data][guard]
//! ```
//!
//! The C version stores the size before the user data because `kfree(ptr)` gets
//! only a pointer. Rust's `Allocator::deallocate(ptr, layout)` is always given
//! the size and alignment, so the Rust header only needs the start of the range
//! and the tag.
//!
//! Like the C version, the block holds its own freelist nodes in front of the
//! user memory (C's two-call `memory_requirement` pattern). Because of that, the
//! allocator never allocates from anywhere else, which is what lets
//! [`GlobalHeap`] be the program's `#[global_allocator]`.

use std::alloc::{AllocError, Allocator, GlobalAlloc, Layout, System};
use std::cell::UnsafeCell;
use std::fmt;
use std::marker::PhantomData;
use std::mem::MaybeUninit;
use std::panic::Location;
use std::ptr::NonNull;
use std::sync::{Mutex, MutexGuard, OnceLock};

use kohi_freelist::{FreeList, Node, node_count_for};

/// What an allocation is for, so usage can be reported per subsystem.
/// A subset of Kohi's `memory_tag` enum.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryTag {
    Unknown,
    Array,
    DArray,
    String,
    Texture,
    Renderer,
    Game,
    Scene,
}

impl MemoryTag {
    /// Number of tags.
    pub const COUNT: usize = 8;
    /// Every tag, in order.
    pub const ALL: [MemoryTag; Self::COUNT] = [
        MemoryTag::Unknown,
        MemoryTag::Array,
        MemoryTag::DArray,
        MemoryTag::String,
        MemoryTag::Texture,
        MemoryTag::Renderer,
        MemoryTag::Game,
        MemoryTag::Scene,
    ];

    /// The label Kohi prints in its memory report.
    pub fn name(self) -> &'static str {
        match self {
            MemoryTag::Unknown => "UNKNOWN",
            MemoryTag::Array => "ARRAY",
            MemoryTag::DArray => "DARRAY",
            MemoryTag::String => "STRING",
            MemoryTag::Texture => "TEXTURE",
            MemoryTag::Renderer => "RENDERER",
            MemoryTag::Game => "GAME",
            MemoryTag::Scene => "SCENE",
        }
    }
}

/// Every freelist range is a multiple of this, so headers stay 8-aligned.
const GRAIN: usize = 8;

/// Stored right before the user data (or before the front guard).
/// C: `alloc_header`, which is stored after the user data.
#[repr(C, align(8))]
struct Header {
    /// Offset of the freelist range this allocation came from. C: `void *start`.
    start: usize,
    tag: MemoryTag,
    /// Where the allocator handle was created. C: `file[256]` and `line`, debug only.
    #[cfg(debug_assertions)]
    site: Option<&'static Location<'static>>,
}

const HEADER_SIZE: usize = size_of::<Header>();

/// The guard pattern written on both sides of the user data in debug builds.
/// C: `MEM_GUARD_MAGIC_0` and `MEM_GUARD_MAGIC_1` under `MEM_DEBUG_TRACE`.
#[cfg(debug_assertions)]
const GUARD: [u8; 16] = {
    let mut g = [0u8; 16];
    let a = 0xF00D_F00D_F00D_F00Du64.to_ne_bytes();
    let b = 0xBAD0_BAD0_BAD0_BAD0u64.to_ne_bytes();
    let mut i = 0;
    while i < 8 {
        g[i] = a[i];
        g[i + 8] = b[i];
        i += 1;
    }
    g
};
#[cfg(debug_assertions)]
const GUARD_SIZE: usize = GUARD.len();
#[cfg(not(debug_assertions))]
const GUARD_SIZE: usize = 0;

/// Bytes of freelist range an allocation of `layout` needs. It depends only on
/// the layout, so `deallocate` can compute the same number again.
fn required_size(layout: Layout) -> Option<usize> {
    layout
        .size()
        .checked_next_multiple_of(GRAIN)?
        .checked_add(HEADER_SIZE + 2 * GUARD_SIZE)?
        // Worst-case padding to reach the alignment from an 8-aligned start.
        .checked_add(layout.align().max(GRAIN) - GRAIN)
}

/// Freelist storage that lives inside the managed block.
struct RawNodes(NonNull<[Node]>);

impl AsRef<[Node]> for RawNodes {
    fn as_ref(&self) -> &[Node] {
        // SAFETY: the nodes are initialized in `from_raw` and only reached
        // through the allocator's mutex.
        unsafe { self.0.as_ref() }
    }
}

impl AsMut<[Node]> for RawNodes {
    fn as_mut(&mut self) -> &mut [Node] {
        // SAFETY: as above, and `&mut self` means we hold the mutex.
        unsafe { self.0.as_mut() }
    }
}

// SAFETY: `RawNodes` is only used behind the allocator's mutex.
unsafe impl Send for RawNodes {}

struct Inner {
    list: FreeList<RawNodes>,
    tagged_bytes: [usize; MemoryTag::COUNT],
    live_allocations: usize,
}

/// A thread-safe general-purpose allocator over one fixed block.
pub struct DynamicAllocator<'m> {
    /// Start of the user region, 8-aligned. C: `state->memory_block`.
    memory: NonNull<u8>,
    total_size: usize,
    /// C: the freelist inside `dynamic_allocator_state`, plus `kmemory`'s
    /// `allocation_mutex` and stats.
    inner: Mutex<Inner>,
    /// The block to free on drop, if this allocator allocated it.
    owned: Option<(NonNull<u8>, Layout)>,
    _buffer: PhantomData<&'m mut [MaybeUninit<u8>]>,
}

// SAFETY: all shared state is behind `inner`'s mutex, and the raw pointers
// only point into the block this allocator manages.
unsafe impl Send for DynamicAllocator<'_> {}
// SAFETY: as above.
unsafe impl Sync for DynamicAllocator<'_> {}

/// Allocator methods must not unwind, so problems C reports with `KFATAL` or
/// `KASSERT` abort instead of panicking.
fn fatal(message: &dyn fmt::Display) -> ! {
    eprintln!("kohi_dynamic: {message}");
    std::process::abort()
}

impl DynamicAllocator<'static> {
    /// Creates an allocator that owns a new block with room for `total_size`
    /// bytes of allocations, taken from the system allocator.
    /// C: `memory_system_initialize`, which gets the block from `platform_allocate`.
    pub fn new(total_size: usize) -> Self {
        let layout = Layout::from_size_align(Self::memory_requirement(total_size), 16)
            .expect("allocator size overflows isize");
        let block = System
            .allocate(layout)
            .unwrap_or_else(|_| std::alloc::handle_alloc_error(layout))
            .cast();
        // SAFETY: `block` is valid for `layout.size()` bytes and owned by the result.
        unsafe { Self::from_raw(block, layout.size(), total_size, Some(layout)) }
            .expect("memory_requirement is large enough")
    }
}

impl<'m> DynamicAllocator<'m> {
    /// Bytes of buffer needed for `total_size` bytes of allocations: the
    /// freelist nodes, alignment slack and the user region.
    /// C: the first `dynamic_allocator_create` call, with `memory = 0`.
    pub fn memory_requirement(total_size: usize) -> usize {
        (align_of::<Node>() - 1)
            + node_count_for(total_size as u64) * size_of::<Node>()
            + (GRAIN - 1)
            + total_size
    }

    /// Creates an allocator inside `buffer`. Returns `None` if the buffer is
    /// smaller than [`memory_requirement`](Self::memory_requirement)`(total_size)`.
    /// C: the second `dynamic_allocator_create` call.
    pub fn from_buffer(total_size: usize, buffer: &'m mut [MaybeUninit<u8>]) -> Option<Self> {
        let len = buffer.len();
        // SAFETY: the buffer is valid for `len` bytes for `'m`, and the result
        // borrows it for `'m`.
        unsafe { Self::from_raw(NonNull::from(buffer).cast(), len, total_size, None) }
    }

    /// Lays out `[nodes][user region]` inside `block`, like the C version's
    /// `[state][freelist block][memory block]`.
    ///
    /// # Safety
    /// `block` must be valid for reads and writes of `len` bytes for as long as
    /// the result lives, and not be used by anything else.
    unsafe fn from_raw(
        block: NonNull<u8>,
        len: usize,
        total_size: usize,
        owned: Option<Layout>,
    ) -> Option<Self> {
        let node_count = node_count_for(total_size as u64);
        let base = block.addr().get();
        let nodes_offset = base.next_multiple_of(align_of::<Node>()) - base;
        let nodes_end = nodes_offset.checked_add(node_count.checked_mul(size_of::<Node>())?)?;
        let memory_offset = (base + nodes_end).next_multiple_of(GRAIN) - base;
        if memory_offset.checked_add(total_size)? > len {
            return None;
        }
        // SAFETY: every offset below is inside `block[..len]`, checked above.
        unsafe {
            let nodes = block.add(nodes_offset).cast::<Node>();
            for i in 0..node_count {
                nodes.add(i).write(Node::EMPTY);
            }
            let nodes = RawNodes(NonNull::slice_from_raw_parts(nodes, node_count));
            Some(DynamicAllocator {
                memory: block.add(memory_offset),
                total_size,
                inner: Mutex::new(Inner {
                    list: FreeList::with_storage(total_size as u64, nodes),
                    tagged_bytes: [0; MemoryTag::COUNT],
                    live_allocations: 0,
                }),
                owned: owned.map(|layout| (block, layout)),
                _buffer: PhantomData,
            })
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // Nothing panics while holding the lock, so poisoning cannot happen,
        // but recovering is still the right thing in an allocator.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Returns a handle that tags every allocation with `tag` and records the
    /// caller's location. C: the `tag`, `file` and `line` arguments of `kallocate`.
    #[track_caller]
    pub fn tagged(&self, tag: MemoryTag) -> Tagged<'_, 'm> {
        Tagged {
            heap: self,
            tag,
            site: Location::caller(),
        }
    }

    fn allocate_tagged(
        &self,
        layout: Layout,
        tag: MemoryTag,
        site: Option<&'static Location<'static>>,
    ) -> Result<NonNull<[u8]>, AllocError> {
        let required = required_size(layout).ok_or(AllocError)?;
        let mut inner = self.lock();
        let start = inner
            .list
            .allocate_block(required as u64)
            .ok_or(AllocError)? as usize;

        let base = self.memory.addr().get();
        let user = (base + start + HEADER_SIZE + GUARD_SIZE)
            .next_multiple_of(layout.align().max(GRAIN))
            - base;
        // SAFETY: `required_size` reserved room for the header, both guards, the
        // data and the padding, all inside `[start, start + required)`.
        let ptr = unsafe {
            let header = self
                .memory
                .add(user - GUARD_SIZE - HEADER_SIZE)
                .cast::<Header>();
            header.write(Header {
                start,
                tag,
                #[cfg(debug_assertions)]
                site,
            });
            #[cfg(debug_assertions)]
            {
                let guard = NonNull::from(&GUARD).cast::<u8>();
                guard.copy_to_nonoverlapping(self.memory.add(user - GUARD_SIZE), GUARD_SIZE);
                guard.copy_to_nonoverlapping(self.memory.add(user + layout.size()), GUARD_SIZE);
            }
            self.memory.add(user)
        };
        #[cfg(not(debug_assertions))]
        let _ = site;

        inner.tagged_bytes[tag as usize] += layout.size();
        inner.live_allocations += 1;
        Ok(NonNull::slice_from_raw_parts(ptr, layout.size()))
    }

    /// Offset of `ptr` in the user region, or abort if it is not inside it.
    fn offset_of(&self, ptr: NonNull<u8>) -> usize {
        match ptr.addr().get().checked_sub(self.memory.addr().get()) {
            Some(offset) if offset >= HEADER_SIZE + GUARD_SIZE && offset <= self.total_size => {
                offset
            }
            _ => fatal(&"freed a pointer outside this allocator's block"),
        }
    }

    /// # Safety
    /// `ptr` must be a live allocation from this allocator.
    unsafe fn header(&self, ptr: NonNull<u8>) -> &Header {
        let offset = self.offset_of(ptr);
        // SAFETY: the caller guarantees `ptr` came from `allocate_tagged`,
        // which wrote a `Header` at this offset.
        unsafe {
            self.memory
                .add(offset - GUARD_SIZE - HEADER_SIZE)
                .cast::<Header>()
                .as_ref()
        }
    }

    /// Returns false if the data was written past either end.
    /// C: `_validate_block`. Always true in release builds, which have no guards.
    ///
    /// # Safety
    /// `ptr` must be a live allocation from this allocator, made with `layout`.
    pub unsafe fn check_guards(&self, ptr: NonNull<u8>, layout: Layout) -> bool {
        #[cfg(debug_assertions)]
        {
            let offset = self.offset_of(ptr);
            // SAFETY: both guards are inside the allocation's range.
            unsafe {
                let front = self
                    .memory
                    .add(offset - GUARD_SIZE)
                    .cast::<[u8; GUARD_SIZE]>()
                    .read();
                let back = self
                    .memory
                    .add(offset + layout.size())
                    .cast::<[u8; GUARD_SIZE]>()
                    .read_unaligned();
                front == GUARD && back == GUARD
            }
        }
        #[cfg(not(debug_assertions))]
        {
            let _ = (ptr, layout);
            true
        }
    }

    /// The tag and (in debug builds) the location of the `tagged` call that
    /// allocated `ptr`. C: `dynamic_allocator_get_size_alignment`,
    /// `dynamic_allocator_get_file` and `dynamic_allocator_get_line`.
    ///
    /// # Safety
    /// `ptr` must be a live allocation from this allocator.
    pub unsafe fn info(&self, ptr: NonNull<u8>) -> AllocationInfo {
        // SAFETY: forwarded from the caller.
        let header = unsafe { self.header(ptr) };
        AllocationInfo {
            tag: header.tag,
            #[cfg(debug_assertions)]
            site: header.site,
            #[cfg(not(debug_assertions))]
            site: None,
        }
    }

    /// A snapshot of usage. C: `get_memory_usage_str` and
    /// `dynamic_allocator_free_space`.
    pub fn stats(&self) -> MemoryStats {
        let inner = self.lock();
        MemoryStats {
            total_size: self.total_size,
            free_space: inner.list.free_space() as usize,
            live_allocations: inner.live_allocations,
            tagged_bytes: inner.tagged_bytes,
        }
    }

    /// Bytes not yet handed out, including header overhead.
    /// C: `dynamic_allocator_free_space`.
    pub fn free_space(&self) -> usize {
        self.lock().list.free_space() as usize
    }

    /// C: `dynamic_allocator_total_space`.
    pub fn total_space(&self) -> usize {
        self.total_size
    }

    /// C: `dynamic_allocator_free_aligned`.
    ///
    /// # Safety
    /// `ptr` must be a live allocation from this allocator, made with `layout`.
    unsafe fn deallocate_impl(&self, ptr: NonNull<u8>, layout: Layout) {
        // SAFETY: forwarded from the caller.
        unsafe {
            if !self.check_guards(ptr, layout) {
                fatal(&"memory guard overwritten: something wrote past an allocation");
            }
        }
        // SAFETY: forwarded from the caller.
        let (start, tag) = unsafe {
            let header = self.header(ptr);
            (header.start, header.tag)
        };
        let required =
            required_size(layout).unwrap_or_else(|| fatal(&"deallocate with an impossible layout"));
        let mut inner = self.lock();
        if let Err(e) = inner.list.free_block(required as u64, start as u64) {
            fatal(&e);
        }
        inner.tagged_bytes[tag as usize] -= layout.size();
        inner.live_allocations -= 1;
    }
}

impl Drop for DynamicAllocator<'_> {
    fn drop(&mut self) {
        if let Some((block, layout)) = self.owned {
            // SAFETY: `block` came from `System.allocate(layout)` in `new`.
            unsafe { System.deallocate(block, layout) };
        }
    }
}

// SAFETY:
// - Each block is a distinct freelist range, so blocks never overlap.
// - Memory stays valid until `deallocate` or until the allocator is dropped.
// - `allocate_tagged` honours the layout's size and alignment.
// - Nothing here panics; errors return `AllocError` or abort.
unsafe impl Allocator for DynamicAllocator<'_> {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        self.allocate_tagged(layout, MemoryTag::Unknown, None)
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        // SAFETY: forwarded from the caller.
        unsafe { self.deallocate_impl(ptr, layout) }
    }
}

/// A [`DynamicAllocator`] handle that tags its allocations.
#[derive(Clone, Copy)]
pub struct Tagged<'a, 'm> {
    heap: &'a DynamicAllocator<'m>,
    tag: MemoryTag,
    site: &'static Location<'static>,
}

// SAFETY: forwards to the underlying allocator, which is the same for every
// copy of the handle, so copies are equivalent.
unsafe impl Allocator for Tagged<'_, '_> {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        self.heap.allocate_tagged(layout, self.tag, Some(self.site))
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        // SAFETY: forwarded from the caller.
        unsafe { self.heap.deallocate_impl(ptr, layout) }
    }
}

/// What [`DynamicAllocator::info`] returns.
#[derive(Clone, Copy, Debug)]
pub struct AllocationInfo {
    pub tag: MemoryTag,
    /// Where `tagged` was called. `None` for untagged allocations and in release builds.
    pub site: Option<&'static Location<'static>>,
}

/// What [`DynamicAllocator::stats`] returns.
#[derive(Clone, Copy, Debug)]
pub struct MemoryStats {
    pub total_size: usize,
    /// Includes the per-allocation overhead of headers, guards and padding.
    pub free_space: usize,
    pub live_allocations: usize,
    /// Requested bytes still allocated, per tag.
    pub tagged_bytes: [usize; MemoryTag::COUNT],
}

impl MemoryStats {
    /// Requested bytes still allocated with `tag`.
    pub fn bytes(&self, tag: MemoryTag) -> usize {
        self.tagged_bytes[tag as usize]
    }
}

fn human(bytes: usize) -> (f64, &'static str) {
    const UNITS: [(usize, &str); 3] = [(1 << 30, "GiB"), (1 << 20, "MiB"), (1 << 10, "KiB")];
    for (scale, unit) in UNITS {
        if bytes >= scale {
            return (bytes as f64 / scale as f64, unit);
        }
    }
    (bytes as f64, "B")
}

/// Prints the report the way Kohi's `get_memory_usage_str` does.
impl fmt::Display for MemoryStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "System memory use (tagged):")?;
        for tag in MemoryTag::ALL {
            let (amount, unit) = human(self.bytes(tag));
            writeln!(f, "  {:<10}: {amount:.2}{unit}", tag.name())?;
        }
        let used = self.total_size - self.free_space;
        let (used, used_unit) = human(used);
        let (total, total_unit) = human(self.total_size);
        write!(
            f,
            "Total: {used:.2}{used_unit} of {total:.2}{total_unit} used (incl. overhead), {} live allocations",
            self.live_allocations
        )
    }
}

/// A [`DynamicAllocator`] over a static buffer of `N` bytes, usable as the
/// program's `#[global_allocator]`, the way Kohi routes every `kallocate`
/// through its dynamic allocator.
///
/// ```no_run
/// use kohi_dynamic::GlobalHeap;
///
/// #[global_allocator]
/// static HEAP: GlobalHeap<{ 16 << 20 }> = GlobalHeap::new();
///
/// fn main() {
///     let v = vec![1, 2, 3]; // allocated in HEAP
///     println!("{v:?}\n{}", HEAP.heap().stats());
/// }
/// ```
pub struct GlobalHeap<const N: usize> {
    buffer: UnsafeCell<[MaybeUninit<u8>; N]>,
    heap: OnceLock<DynamicAllocator<'static>>,
}

// SAFETY: `buffer` is only reached through `heap`, which is created once and
// is itself `Sync`.
unsafe impl<const N: usize> Sync for GlobalHeap<N> {}

impl<const N: usize> GlobalHeap<N> {
    /// Creates the heap. It initializes itself on first use.
    pub const fn new() -> Self {
        GlobalHeap {
            buffer: UnsafeCell::new([const { MaybeUninit::uninit() }; N]),
            heap: OnceLock::new(),
        }
    }

    /// The allocator inside, for stats and tagged handles.
    pub fn heap(&self) -> &DynamicAllocator<'static> {
        self.heap.get_or_init(|| {
            // Leave room for the nodes of an `N`-byte region; the real region
            // is a bit smaller and needs no more nodes than that.
            let overhead = DynamicAllocator::memory_requirement(N) - N;
            let total = N
                .checked_sub(overhead)
                .unwrap_or_else(|| fatal(&"GlobalHeap is too small"));
            let block = NonNull::new(self.buffer.get().cast::<u8>()).expect("statics are not null");
            // SAFETY: the buffer is valid for `N` bytes for the whole program
            // and only ever used by this allocator.
            unsafe { DynamicAllocator::from_raw(block, N, total, None) }
                .unwrap_or_else(|| fatal(&"GlobalHeap is too small"))
        })
    }
}

impl<const N: usize> Default for GlobalHeap<N> {
    fn default() -> Self {
        Self::new()
    }
}

// SAFETY: forwards to `DynamicAllocator`, whose `Allocator` implementation
// meets the same requirements. Initialization does not allocate.
unsafe impl<const N: usize> GlobalAlloc for GlobalHeap<N> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.heap()
            .allocate(layout)
            .map_or(std::ptr::null_mut(), |p| p.as_ptr().cast())
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `GlobalAlloc` callers pass a pointer this allocator returned.
        unsafe { self.heap().deallocate(NonNull::new_unchecked(ptr), layout) }
    }
}

//! Reserve/commit/release, as in RAD's `linux_base.c` (`reserve_memory`,
//! `commit_memory`, `release_memory`).
//!
//! On Unix, reserving maps address space with `PROT_NONE` (no physical memory),
//! and committing makes pages readable and writable with `mprotect`, so the OS
//! only backs them when they are first touched. Other platforms (and Miri, which
//! cannot run `mmap`) fall back to allocating the whole reservation up front
//! from `System`.

use std::ptr::NonNull;

#[cfg(all(unix, not(miri)))]
pub fn page_size() -> usize {
    use std::sync::OnceLock;
    static PAGE: OnceLock<usize> = OnceLock::new();
    *PAGE.get_or_init(|| {
        // SAFETY: `sysconf` has no preconditions.
        let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        usize::try_from(size).unwrap_or(4096)
    })
}

/// Reserves `size` bytes of address space. C: `reserve_memory` (`mmap` with `PROT_NONE`).
#[cfg(all(unix, not(miri)))]
pub fn reserve(size: usize) -> Option<NonNull<u8>> {
    // SAFETY: an anonymous private mapping with no address hint has no preconditions.
    let p = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            size,
            libc::PROT_NONE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    if p == libc::MAP_FAILED {
        None
    } else {
        NonNull::new(p.cast())
    }
}

/// Makes `[ptr, ptr + size)` usable. C: `commit_memory` (`mprotect` to read/write).
///
/// # Safety
/// The range must be page-aligned and inside one reservation.
#[cfg(all(unix, not(miri)))]
pub unsafe fn commit(ptr: NonNull<u8>, size: usize) -> bool {
    // SAFETY: forwarded from the caller.
    unsafe {
        libc::mprotect(
            ptr.as_ptr().cast(),
            size,
            libc::PROT_READ | libc::PROT_WRITE,
        ) == 0
    }
}

/// Returns a whole reservation to the OS. C: `release_memory` (`munmap`).
///
/// # Safety
/// `ptr` and `size` must be exactly a reservation from [`reserve`], no longer in use.
#[cfg(all(unix, not(miri)))]
pub unsafe fn release(ptr: NonNull<u8>, size: usize) {
    // SAFETY: forwarded from the caller.
    unsafe { libc::munmap(ptr.as_ptr().cast(), size) };
}

#[cfg(any(not(unix), miri))]
mod fallback {
    use std::alloc::{Allocator, Layout, System};
    use std::ptr::NonNull;

    pub fn page_size() -> usize {
        4096
    }

    pub fn reserve(size: usize) -> Option<NonNull<u8>> {
        let layout = Layout::from_size_align(size, page_size()).ok()?;
        System.allocate_zeroed(layout).ok().map(|p| p.cast())
    }

    pub unsafe fn commit(_ptr: NonNull<u8>, _size: usize) -> bool {
        true
    }

    pub unsafe fn release(ptr: NonNull<u8>, size: usize) {
        let layout = Layout::from_size_align(size, page_size()).unwrap();
        // SAFETY: `ptr` came from `reserve(size)`.
        unsafe { System.deallocate(ptr, layout) };
    }
}

#[cfg(any(not(unix), miri))]
pub use fallback::*;

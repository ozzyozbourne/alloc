//! A port of the RAD Debugger's scratch arenas (`src/base/base_thread_context.c`).
//!
//! Every thread has a few arenas for temporary work. A function that needs
//! scratch memory borrows one, uses it, and pops it back when it is done. In C:
//!
//! ```c
//! Temp scratch = scratch_begin(&out_arena, 1);   // any arena except out_arena
//! ... push temporary data onto scratch.arena ...
//! ... push the result onto out_arena ...
//! scratch_end(scratch);
//! ```
//!
//! The `conflicts` list exists because popping a scratch arena that is also the
//! caller's output arena would free the result. C relies on every caller
//! passing the right list.
//!
//! In Rust, [`with_scratch`] lends an arena exclusively for the length of a
//! closure. An arena that is already lent out, such as one the caller is using
//! for its output, cannot be picked again, so there is no `conflicts` argument
//! to get wrong. And because the closure's return type cannot borrow the
//! scratch arena, scratch data cannot escape the scope:
//!
//! ```compile_fail
//! let leaked: &str = rad_scratch::with_scratch(|s| s.push_str("temporary"));
//! ```
//!
//! The cost: C can reuse the caller's own scratch arena for a nested scratch
//! when it is not a conflict; here each nesting level takes another arena.
//! RAD keeps two per thread; this version creates more when scopes nest deeper.

use std::cell::RefCell;

use rad_arena::Arena;

thread_local! {
    /// C: `TCTX::arenas[2]`. Boxed so each arena keeps its address while the
    /// `Vec` grows: `with_scratch` keeps a pointer to the arena while it runs.
    #[allow(clippy::vec_box)]
    static SCRATCH: RefCell<Vec<Box<RefCell<Arena>>>> = const { RefCell::new(Vec::new()) };
}

/// Number of scratch arenas this thread has created so far.
pub fn arena_count() -> usize {
    SCRATCH.with(|s| s.borrow().len())
}

/// Runs `f` with a scratch arena that no enclosing `with_scratch` on this
/// thread is using, then pops it back to where it was.
/// C: `scratch_begin(conflicts, count)` ... `scratch_end(scratch)`.
pub fn with_scratch<R>(f: impl FnOnce(&Arena) -> R) -> R {
    let cell: *const RefCell<Arena> = SCRATCH.with(|s| {
        let mut arenas = s.borrow_mut();
        // C: the loop in `tctx_get_scratch` that skips `conflicts`.
        match arenas.iter().find(|a| a.try_borrow_mut().is_ok()) {
            Some(free) => &**free as *const _,
            None => {
                arenas.push(Box::new(RefCell::new(Arena::new())));
                &**arenas.last().expect("just pushed") as *const _
            }
        }
    });
    // SAFETY: the boxes are never removed or moved, and thread-local storage
    // outlives any call running on its thread.
    let cell = unsafe { &*cell };
    let mut arena = cell.borrow_mut();
    let temp = arena.temp();
    f(&temp)
}

/// Runs `f` with two scratch arenas at once, for code that ping-pongs between
/// buffers.
pub fn with_scratch2<R>(f: impl FnOnce(&Arena, &Arena) -> R) -> R {
    with_scratch(|a| with_scratch(|b| f(a, b)))
}

//! Pony's sendable reference capabilities as Rust types.
//!
//! | Pony | Here | Checked by |
//! |---|---|---|
//! | `iso`: the only reference, readable and writable | [`Iso<T>`] | Rust moves: using it after a send is a compile error |
//! | `val`: immutable, shared by any number of actors | [`Val<T>`] | `Deref` only, and `T: Sync` |
//! | `tag`: identity only, can send messages | [`Addr`](crate::Addr) | it has no way to reach the actor's state |
//!
//! Both [`Iso`] and [`Val`] live in the heap of the actor that created them
//! and are never copied when sent. When the last holder drops one, the block
//! goes back to that heap through its [`Remote`] handle, the way Pony objects
//! are only ever freed by the actor that allocated them.
use std::fmt;
use std::ops::{Deref, DerefMut};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicUsize, Ordering, fence};

use crate::heap::{Local, Remote};

/// An object with exactly one owner that can be sent to another actor.
/// Pony: `T iso`, sent with `consume`.
///
/// Requires `T: Send`, so `T` cannot hold a `Box` or `Vec` of the sending
/// actor's heap ([`Local`] is not `Send`): what Pony calls a deep `iso`.
pub struct Iso<T> {
    b: Box<T, Remote>,
}

impl<T: Send> Iso<T> {
    /// Allocates `value` in `heap`. Pony: `recover iso ... end`.
    pub fn new_in(value: T, heap: &Local) -> Self {
        Self::from_box(Box::new_in(value, heap.clone()))
    }

    /// Turns an actor-local box into a sendable one without copying.
    pub fn from_box(b: Box<T, Local>) -> Self {
        let (p, local) = Box::into_raw_with_allocator(b);
        // SAFETY: `p` was allocated by `local`, and `Remote` frees any block
        // of the heap it shares with `local`.
        let b = unsafe { Box::from_raw_in(p, local.remote()) };
        Iso { b }
    }

    /// Takes the value out; the block goes back to its heap.
    pub fn into_inner(self) -> T {
        *self.b
    }

    /// The address of the object, to show that sends do not copy it.
    pub fn as_ptr(&self) -> *const T {
        &*self.b
    }

    /// The heap the object lives in.
    pub fn heap(&self) -> &Remote {
        Box::allocator(&self.b)
    }
}

impl<T> Deref for Iso<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.b
    }
}

impl<T> DerefMut for Iso<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.b
    }
}

impl<T: fmt::Debug> fmt::Debug for Iso<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Iso").field(&*self.b).finish()
    }
}

struct ValBox<T> {
    rc: AtomicUsize,
    value: T,
}

/// An immutable object shared by any number of actors. Pony: `T val`.
///
/// Like `Arc`, but allocated in an actor's heap: `Arc<T, A>` with a custom
/// allocator is not stable yet. The last clone to drop frees it back to the
/// owner's heap.
pub struct Val<T> {
    ptr: NonNull<ValBox<T>>,
    heap: Remote,
}

// SAFETY: `Val<T>` only gives out `&T`, and the count is atomic, so it is
// `Arc<T>`'s rule: sendable and shareable when `T: Send + Sync`.
unsafe impl<T: Send + Sync> Send for Val<T> {}
// SAFETY: as above.
unsafe impl<T: Send + Sync> Sync for Val<T> {}

impl<T: Send + Sync> Val<T> {
    /// Allocates `value` in `heap`. Pony: `recover val ... end`.
    pub fn new_in(value: T, heap: &Local) -> Self {
        let b = Box::new_in(
            ValBox {
                rc: AtomicUsize::new(1),
                value,
            },
            heap.clone(),
        );
        let (p, local) = Box::into_raw_with_allocator(b);
        Val {
            // SAFETY: `Box::into_raw_with_allocator` never returns null.
            ptr: unsafe { NonNull::new_unchecked(p) },
            heap: local.remote(),
        }
    }
}

impl<T> Val<T> {
    /// True if both point at the same object.
    pub fn ptr_eq(a: &Self, b: &Self) -> bool {
        a.ptr == b.ptr
    }

    /// Number of live clones.
    pub fn count(this: &Self) -> usize {
        this.inner().rc.load(Ordering::Acquire)
    }

    fn inner(&self) -> &ValBox<T> {
        // SAFETY: the box is alive while any clone (including `self`) is.
        unsafe { self.ptr.as_ref() }
    }
}

impl<T> Clone for Val<T> {
    fn clone(&self) -> Self {
        // Relaxed is enough to add a reference we already hold (as in `Arc`).
        if self.inner().rc.fetch_add(1, Ordering::Relaxed) > isize::MAX as usize {
            std::process::abort();
        }
        Val {
            ptr: self.ptr,
            heap: self.heap.clone(),
        }
    }
}

impl<T> Drop for Val<T> {
    fn drop(&mut self) {
        if self.inner().rc.fetch_sub(1, Ordering::Release) != 1 {
            return;
        }
        // Every other clone's use happens before the free (as in `Arc`).
        fence(Ordering::Acquire);
        // SAFETY: we held the last reference; the box was allocated in the
        // heap `self.heap` frees into.
        drop(unsafe { Box::from_raw_in(self.ptr.as_ptr(), self.heap.clone()) });
    }
}

impl<T> Deref for Val<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.inner().value
    }
}

impl<T: fmt::Debug> fmt::Debug for Val<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Val").field(&**self).finish()
    }
}

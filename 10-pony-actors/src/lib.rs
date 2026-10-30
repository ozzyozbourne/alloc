//! Pony's actor memory model, rebuilt on Rust's `Allocator` trait.
//!
//! The [Pony](https://www.ponylang.io) runtime (`ponyc/src/libponyrt`) gives
//! every actor its own heap and passes objects between actors by reference,
//! never copying them. This crate reproduces those allocation patterns:
//!
//! | Pony (C) | Here |
//! |---|---|
//! | `mem/pool_arena.c`: size-class pool, blocks owned by a thread, foreign frees batched to the owner's inbox | [`Pool`] |
//! | `mem/heap.c`: per-actor heap, freed only by its actor, between behaviours | [`Local`] / [`Remote`] |
//! | `iso`, `val`, `tag` | [`Iso`], [`Val`], [`Addr`] |
//! | `actor/messageq.c`: intrusive MPSC queue, empty flag in bit 0 of `head` | [`Mailbox`] |
//! | `actor/actor.c`: run a batch, GC, mark empty | [`System`], [`Actor`] |
//!
//! It does not reproduce ORCA (Pony's reference-counting GC across actors),
//! the cycle detector, work stealing or backpressure. Rust ownership takes the
//! place of ORCA: an [`Iso`] or [`Val`] is freed when its last holder drops
//! it, and the block goes back to the heap it came from.
//!
//! The compiler enforces what Pony's capabilities do. An actor-local box
//! cannot leave its thread:
//!
//! ```compile_fail,E0277
//! use pony_actors::Local;
//! let heap = Local::new();
//! let b = Box::new_in(5u64, heap.clone());
//! std::thread::spawn(move || drop(b)); // `Local` is not `Send`
//! ```
//!
//! An [`Iso`] can, but only once: sending it moves it, like `consume`:
//!
//! ```compile_fail,E0382
//! use pony_actors::{Iso, Local};
//! let heap = Local::new();
//! let buf = Iso::new_in(vec![1u8, 2, 3], &heap);
//! std::thread::spawn(move || drop(buf));
//! println!("{:?}", buf); // error: use of moved value
//! ```
//!
//! An [`Iso`] cannot hold memory of the sender's heap, since that would be
//! shared after the send (Pony: an `iso` must be isolated):
//!
//! ```compile_fail,E0277
//! use pony_actors::{Iso, Local};
//! let heap = Local::new();
//! let inner: Vec<u8, Local> = Vec::new_in(heap.clone());
//! let _ = Iso::new_in(inner, &heap);
//! ```
mod actor;
mod cap;
mod heap;
pub mod pool;
mod queue;

pub use actor::{Actor, Addr, BATCH, Context, System, WorkerStats};
pub use cap::{Iso, Val};
pub use heap::{CHUNK, HEAP_MAX, HEAP_MIN, HeapStats, Local, Remote};
pub use pool::Pool;
pub use queue::Mailbox;

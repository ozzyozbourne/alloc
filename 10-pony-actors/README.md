# Pony actor allocators

Rust port of the allocators behind [Pony](https://www.ponylang.io)'s actors, from `ponyc/src/libponyrt` (commit `cc2ec5d`), on `std::alloc::Allocator` (stable in Rust 1.100). No `#![feature]`.

| Pony (C) | Rust |
|---|---|
| `mem/pool_arena.c`: size-class pool, blocks owned by a thread, foreign frees batched to the owner's inbox | `Pool` |
| `mem/heap.c`: one heap per actor, freed only by its actor | `Local` (owner, `!Send`) and `Remote` (free-only, `Send`) |
| `iso`, `val`, `tag` | `Iso<T>`, `Val<T>`, `Addr<M>` |
| `actor/messageq.c`: intrusive MPSC queue, empty flag in bit 0 of `head` | `Mailbox<M>` |
| `actor/actor.c`: batch of 100, then GC, then mark empty | `System`, `Actor` |

Objects are sent between actors by pointer. When the last holder drops one, it goes back to the heap of the actor that made it, and that actor collects it between batches. ORCA (Pony's cross-actor GC), the cycle detector, work stealing and backpressure are not ported; the guide explains why.

**Open [`guide.html`](guide.html) for the full study guide.**

| Path | What |
|---|---|
| `src/` | The crate `pony_actors`: `pool.rs`, `heap.rs`, `cap.rs`, `queue.rs`, `actor.rs` |
| `original/` | The Pony C it copies (`pool_arena.c`, `POOL_ARENA.md`, `heap.{h,c}`, `messageq.{h,c}`, `actor.c`), with its licence |
| `tests/pony.rs` | 14 tests; the crate docs add 3 `compile_fail` tests |
| `examples/demo.rs` | Producer and consumer actors passing buffers without copying |

```sh
cargo test -p pony_actors
cargo run -p pony_actors --example demo
MIRIFLAGS=-Zmiri-many-seeds=0..4 cargo +nightly miri test -p pony_actors --test pony
```

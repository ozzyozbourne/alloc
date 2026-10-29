# C allocators in Rust

Seven custom allocators from the [Kohi game engine](https://github.com/travisvroman/kohi) and the [RAD Debugger](https://github.com/EpicGamesExt/raddebugger), rebuilt on Rust's `std::alloc::Allocator` trait, which is stable from Rust 1.100. This repository is meant as a study guide for writing allocators in Rust and comparing them with C.

**Start with [`index.html`](index.html)**, then open each folder's `guide.html`. Every guide shows the original C next to the Rust port, explains the differences (including bugs in the C that the Rust type system catches), shows how to use the allocator with `Vec::new_in` / `Box::new_in`, and walks through building it yourself.

| Folder | Copies | Rust API |
|---|---|---|
| [`01-kohi-linear-allocator`](01-kohi-linear-allocator) | Kohi linear (bump) allocator | `LinearAllocator: Allocator` |
| [`02-kohi-pool-allocator`](02-kohi-pool-allocator) | Kohi fixed-size pool | `PoolAllocator: Allocator` |
| [`03-kohi-freelist`](03-kohi-freelist) | Kohi first-fit freelist | `FreeList` (offsets, not an allocator) |
| [`04-kohi-dynamic-allocator`](04-kohi-dynamic-allocator) | Kohi general heap + `kmemory` tags | `DynamicAllocator: Allocator`, `GlobalHeap: GlobalAlloc` |
| [`05-rad-arena`](05-rad-arena) | RAD chained virtual-memory arena | `Arena: Allocator`, `Temp` |
| [`06-rad-scratch-arenas`](06-rad-scratch-arenas) | RAD per-thread scratch arenas | `with_scratch(\|arena\| ...)` |
| [`07-rad-thread-pool-arenas`](07-rad-thread-pool-arenas) | RAD linker per-worker arenas | `WorkerArenas` |

## Running

```sh
rustup toolchain install beta              # rust-toolchain.toml pins beta until 1.100.0 is stable (~2026-11-12)
cargo test --workspace
cargo run -p rad_arena --example demo      # every crate has examples/demo.rs
cargo +nightly miri test --workspace       # check the unsafe code for undefined behaviour
uv run --with pygments python tools/gen_guides.py   # regenerate the HTML guides
```

No `#![feature]` is used anywhere. Once 1.100.0 is on stable, change `rust-toolchain.toml` to `stable`.

## Sources and licences

The Rust code in this repository is original. Each `original-c/` folder holds unmodified copies (line endings normalised) of the C it is compared with, plus that project's licence:

- Kohi at commit `156b1c2c`, Apache-2.0, Copyright (c) Travis Vroman.
- RAD Debugger at commit `771f63d`, MIT, Copyright (c) Epic Games Tools.

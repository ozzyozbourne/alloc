# Verus-verified handle map

The handle map from [`08-verified-handle-map`](../08-verified-handle-map), proved with [Verus](https://github.com/verus-lang/verus) for **every capacity** and **every allocator**. Kani checked 08 at capacity 4. Verus proves the same properties for any capacity from a written invariant, with no bound.

Verus proves:

- The free list holds exactly the empty, non-retired slots, each once. It ends in `NONE` and has no cycle.
- Generations never decrease. A removed handle never resolves again, after any number of operations.
- `add` returns a handle that was never issued before. `add` and `remove` don't change any other handle's value.
- `add` fails only when every slot is occupied or retired.
- No arithmetic overflow and no out-of-bounds index, anywhere.

All 16 functions verify in about 3 seconds using about 320 MB of memory. Nine planted bugs, including slotmap's and Odin's generation wrapping, are each rejected. See [`VERUS_RESULTS.txt`](VERUS_RESULTS.txt).

The map stores its slots in a `Vec<Slot<T>, A>` for any `A: Allocator`, using `Option<T>` rather than `MaybeUninit<T>`. So this is a proof about the bookkeeping. The `unsafe` slot code is still covered by 08's Kani and Miri checks.

**Open [`guide.html`](guide.html) for the full study guide.**

| Path | What |
|---|---|
| `src/handle_map.rs` | The map, its spec and its proof, all inside `verus! { }` |
| `src/main.rs` | Two verified clients (a stale-handle demo, and churn for any number of rounds) and a run with a counting allocator |
| `VERUS_RESULTS.txt` | Verification result, demo output, planted-bug table |

This folder is not a Cargo workspace member. Verus runs its own rustc (Rust 1.99, so `#![feature(allocator_api)]` is still needed).

```sh
# Verus release 0.2026.10.26.d17fbd0 from https://github.com/verus-lang/verus/releases, and rustup toolchain 1.99.1
VERUS=/path/to/verus tools/run_verus.sh                       # verify
VERUS=/path/to/verus tools/run_verus.sh --compile -o /tmp/demo && /tmp/demo
VERUS=/path/to/verus python3 tools/verus_mutants.py           # plant bugs, expect each one caught
```

If the bundled `z3` needs a newer glibc than your system has, `pip download z3-solver==4.16.0.0` gives a manylinux wheel; unzip it and set `VERUS_Z3_PATH` to `z3_solver-4.16.0.0.data/data/bin/z3`.

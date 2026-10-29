# Verified handle map

A fixed-capacity handle map (index + generation handles, like Odin's `core:container/handle_map` and the `slotmap` crate) that owns its slot memory, with [Kani](https://github.com/model-checking/kani) proofs.

The proofs start from an arbitrary map that satisfies `check_invariant`, run one operation, and check the result. Because every operation keeps the invariant, the properties hold after any number of operations, at capacity 4. They prove: no undefined behaviour in the `unsafe` slot code, `add` never reissues an old handle, removed handles stay dead after reuse, operations don't touch other handles, and every value is dropped exactly once. See [`KANI_RESULTS.txt`](KANI_RESULTS.txt).

Unlike Odin and slotmap, generations never wrap: a slot whose counter is exhausted is retired. The `naive-generations` feature switches to wrapping, and Kani then finds the stale-handle bug.

**Open [`guide.html`](guide.html) for the full study guide.**

| Path | What |
|---|---|
| `src/lib.rs` | The map (crate `verified_handle_map`) |
| `src/proofs.rs` | Kani harnesses |
| `original/` | Odin `static_handle_map.odin` (commit `69b973e`) and slotmap `basic.rs` (v1.1.1, `eccedef`), with licences |
| `tests/` | Model test vs `HashMap`, differential test vs `slotmap`, wraparound test |
| `examples/demo.rs` | Runnable demo |

```sh
tools/run_kani.sh                       # needs: cargo install --locked kani-verifier && cargo kani setup
cargo test -p verified_handle_map
cargo run -p verified_handle_map --example demo
```

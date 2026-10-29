//! Kani proof harnesses. Run with `cargo kani -p verified_handle_map`, or one at
//! a time with `tools/run_kani.sh`.
//!
//! Every harness except `new_map_is_valid` starts from an *arbitrary* map: any
//! occupancy, generations, free list and length that satisfy
//! [`HandleMap::check_invariant`], with arbitrary values in the occupied slots.
//! It then runs one operation with arbitrary arguments and checks the result.
//!
//! `new()` satisfies the invariant, and every operation keeps it, so the
//! invariant holds after any number of operations. The per-operation checks
//! then hold for every reachable state, not just short sequences. The bound is
//! the capacity `N`, not the number of steps.
//!
//! Kani also checks every `unsafe` block for undefined behaviour on each path:
//! reading uninitialized slots, double reads, out-of-bounds indexing, overflow.

use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

impl<G: kani::Arbitrary> kani::Arbitrary for Handle<G> {
    fn any() -> Self {
        Handle {
            idx: kani::any(),
            generation: kani::any(),
        }
    }
}

impl<T: kani::Arbitrary, const N: usize, G: Generation + kani::Arbitrary> HandleMap<T, N, G> {
    /// Any map that satisfies the invariant.
    fn any_valid() -> Self {
        let mut m = HandleMap {
            values: [const { MaybeUninit::uninit() }; N],
            occupied: kani::any(),
            generation: kani::any(),
            next_free: kani::any(),
            free_head: kani::any(),
            len: kani::any(),
        };
        kani::assume(m.check_invariant());
        for i in 0..N {
            if m.occupied[i] {
                m.values[i].write(kani::any());
            }
        }
        m
    }
}

static DROPS: AtomicUsize = AtomicUsize::new(0);

/// A value that counts its drops.
struct Counted(#[allow(dead_code)] u8);

impl kani::Arbitrary for Counted {
    fn any() -> Self {
        Counted(kani::any())
    }
}

impl Drop for Counted {
    fn drop(&mut self) {
        DROPS.fetch_add(1, Ordering::Relaxed);
    }
}

macro_rules! harnesses {
    ($module:ident, $gen:ty, $n:expr) => {
        mod $module {
            use super::*;

            const N: usize = $n;
            type G = $gen;
            type Map = HandleMap<u8, N, G>;

            /// Base case: a new map is empty and satisfies the invariant.
            #[kani::proof]
            #[kani::unwind(10)]
            fn new_map_is_valid() {
                let m = Map::new();
                assert!(m.check_invariant());
                assert!(m.is_empty() && m.retired() == 0);
                let h: Handle<G> = kani::any();
                assert!(m.get(h).is_none());
            }

            /// `add` keeps the invariant, returns a handle that is fresh
            /// (no handle with that generation for that slot ever existed),
            /// stores the value under it, and leaves every other handle alone.
            /// It fails only when no slot can be reused, and then changes nothing.
            #[kani::proof]
            #[kani::unwind(10)]
            #[kani::solver(cadical)]
            fn add_is_correct() {
                let mut m = Map::any_valid();
                let generations = m.generation;
                let occupied = m.occupied;
                let len = m.len();
                let other: Handle<G> = kani::any();
                let other_before = m.get(other).copied();
                let v: u8 = kani::any();

                match m.add(v) {
                    Ok(h) => {
                        let i = h.index() as usize;
                        assert!(!occupied[i], "add reused an occupied slot");
                        assert!(
                            h.generation() > generations[i],
                            "add reissued an old generation"
                        );
                        assert!(m.get(h) == Some(&v));
                        assert!(m.len() == len + 1);
                        if other != h {
                            assert!(m.get(other).copied() == other_before);
                        }
                    }
                    Err(back) => {
                        assert!(back == v);
                        assert!(len + m.retired() == N, "add failed with a reusable slot");
                        assert!(m.get(other).copied() == other_before);
                    }
                }
                assert!(m.check_invariant());
                for i in 0..N {
                    assert!(
                        m.generation[i] >= generations[i],
                        "a generation went backwards"
                    );
                }
            }

            /// `remove` keeps the invariant, returns exactly what `get` would
            /// have, kills the handle, and leaves every other handle alone.
            #[kani::proof]
            #[kani::unwind(10)]
            #[kani::solver(cadical)]
            fn remove_is_correct() {
                let mut m = Map::any_valid();
                let generations = m.generation;
                let len = m.len();
                let h: Handle<G> = kani::any();
                let other: Handle<G> = kani::any();
                let h_before = m.get(h).copied();
                let other_before = m.get(other).copied();

                let removed = m.remove(h);
                assert!(removed == h_before);
                assert!(m.get(h).is_none());
                assert!(m.len() == len - usize::from(removed.is_some()));
                if other != h {
                    assert!(m.get(other).copied() == other_before);
                }
                assert!(m.check_invariant());
                for i in 0..N {
                    assert!(
                        m.generation[i] == generations[i],
                        "remove changed a generation"
                    );
                }
            }

            /// A removed handle stays dead after the slot is reused.
            #[kani::proof]
            #[kani::unwind(10)]
            #[kani::solver(cadical)]
            fn removed_handle_stays_dead_after_reuse() {
                let mut m = Map::any_valid();
                let h: Handle<G> = kani::any();
                kani::assume(m.contains(h));
                m.remove(h);
                let _ = m.add(kani::any());
                assert!(!m.contains(h));
            }

            /// `get_mut` changes only the value its handle refers to.
            #[kani::proof]
            #[kani::unwind(10)]
            #[kani::solver(cadical)]
            fn get_mut_writes_only_its_slot() {
                let mut m = Map::any_valid();
                let h: Handle<G> = kani::any();
                let other: Handle<G> = kani::any();
                let other_before = m.get(other).copied();
                let v: u8 = kani::any();
                let was_live = m.contains(h);
                if let Some(slot) = m.get_mut(h) {
                    *slot = v;
                }
                assert!(m.contains(h) == was_live);
                if was_live {
                    assert!(m.get(h) == Some(&v));
                }
                if other != h {
                    assert!(m.get(other).copied() == other_before);
                }
            }

            /// `iter` yields exactly `len` items, each under a live handle.
            #[kani::proof]
            #[kani::unwind(10)]
            #[kani::solver(cadical)]
            fn iter_yields_exactly_the_live_values() {
                let m = Map::any_valid();
                let mut count = 0;
                for (h, v) in m.iter() {
                    assert!(m.get(h) == Some(v));
                    count += 1;
                }
                assert!(count == m.len());
            }

            /// Guards against vacuous proofs: the arbitrary maps the other
            /// harnesses start from really include these states. Kani reports
            /// each `cover!` as satisfied or unsatisfiable.
            #[kani::proof]
            #[kani::unwind(10)]
            #[kani::solver(cadical)]
            fn arbitrary_maps_cover_interesting_states() {
                let m = Map::any_valid();
                kani::cover!(m.is_empty(), "empty map");
                kani::cover!(m.len() == N, "full map");
                kani::cover!(m.retired() > 0, "a retired slot");
                kani::cover!(m.len() == 1 && m.retired() == 0, "three-slot free list");
                kani::cover!(
                    m.free_head != 0 && m.free_head != NONE,
                    "free list not starting at slot 0"
                );
                kani::cover!(
                    m.generation.iter().any(|&g| g == G::MAX - 1),
                    "a slot one reuse from retiring"
                );
            }

            /// Every value is dropped exactly once: by the caller if removed,
            /// otherwise by the map's `Drop`.
            #[kani::proof]
            #[kani::unwind(10)]
            #[kani::solver(cadical)]
            fn values_are_dropped_exactly_once() {
                let mut m: HandleMap<Counted, N, G> = HandleMap::any_valid();
                let len = m.len();
                let h: Handle<G> = kani::any();
                DROPS.store(0, Ordering::Relaxed);
                drop(m.remove(h));
                drop(m);
                assert!(DROPS.load(Ordering::Relaxed) == len);
            }
        }
    };
}

// `u8` generations reach `MAX` with a short counter, so the retirement path is
// covered; the code is generic, and the `u32` run checks the default type.
harnesses!(u8_gen, u8, 4);
harnesses!(u32_gen, u32, 4);

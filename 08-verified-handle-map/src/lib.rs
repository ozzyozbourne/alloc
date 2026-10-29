//! A fixed-capacity handle map that owns its slot memory, with Kani proofs.
//!
//! A handle map stores values in slots and gives out [`Handle`]s: a slot index
//! plus a generation. When a value is removed, its slot's generation changes,
//! so old handles stop resolving even after the slot is reused. This is the
//! same idea as Odin's `core:container/handle_map` and the `slotmap` crate.
//!
//! This map is its own allocator: it holds `N` slots of `MaybeUninit<T>` and
//! decides itself which are in use, so no other allocator is involved. That
//! makes it small and fixed in size, which is what lets Kani prove it (see
//! `src/proofs.rs`).
//!
//! Differences from slotmap and Odin:
//! - Generations never wrap. A slot whose generation reaches `G::MAX` is
//!   retired: it stays empty forever instead of being reused. slotmap and Odin
//!   wrap the counter, so after enough reuses of one slot an old handle
//!   becomes valid again. The `naive-generations` feature switches to their
//!   behaviour, so the proofs can show that bug.
//! - Occupancy is a separate `bool` per slot rather than slotmap's odd/even
//!   version trick. It costs a byte per slot and keeps generations monotone.
//! - `add` returns `Err(value)` when the map is full; it never grows.

use std::fmt::Debug;
use std::mem::MaybeUninit;

#[cfg(kani)]
mod proofs;

/// Marks the end of the free list.
const NONE: u32 = u32::MAX;

/// A generation counter. Smaller types make handles smaller but retire slots
/// sooner: a `u8` slot can be reused 255 times.
pub trait Generation: Copy + Eq + Ord + Debug {
    /// Generation of a slot that has never held a value. No handle has it.
    const ZERO: Self;
    /// The last generation a slot can have.
    const MAX: Self;
    /// `self + 1`, wrapping to `ZERO` after `MAX`.
    fn wrapping_inc(self) -> Self;
}

macro_rules! impl_generation {
    ($($t:ty),*) => {$(
        impl Generation for $t {
            const ZERO: Self = 0;
            const MAX: Self = <$t>::MAX;
            fn wrapping_inc(self) -> Self { self.wrapping_add(1) }
        }
    )*};
}
impl_generation!(u8, u16, u32, u64);

/// Whether exhausted slots are retired (the default) or wrap around.
const RETIRE: bool = !cfg!(feature = "naive-generations");

/// A reference to a value in a [`HandleMap`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Handle<G = u32> {
    idx: u32,
    generation: G,
}

impl<G: Generation> Handle<G> {
    /// A handle that never resolves. Odin uses index 0 for this; here every
    /// slot is usable and `ZERO` is the generation no value ever has.
    pub const NONE: Self = Handle {
        idx: 0,
        generation: G::ZERO,
    };

    /// The slot index.
    pub fn index(self) -> u32 {
        self.idx
    }

    /// The slot's generation when this handle was made.
    pub fn generation(self) -> G {
        self.generation
    }
}

/// A map from [`Handle`] to `T` with room for `N` values.
pub struct HandleMap<T, const N: usize, G: Generation = u32> {
    /// Slot memory. `values[i]` is initialized exactly when `occupied[i]`.
    values: [MaybeUninit<T>; N],
    occupied: [bool; N],
    /// Current generation of each slot. Only ever increases.
    generation: [G; N],
    /// For a free slot, the next free slot, or `NONE`.
    next_free: [u32; N],
    /// First free slot, or `NONE` when the map is full.
    free_head: u32,
    /// Number of occupied slots.
    len: u32,
}

impl<T, const N: usize, G: Generation> HandleMap<T, N, G> {
    /// Compile-time check that slot indices fit in a `u32` and never equal `NONE`.
    const CAPACITY_OK: () = assert!(N < NONE as usize, "capacity must be below u32::MAX");

    /// Creates an empty map. All `N` slots start on the free list.
    pub fn new() -> Self {
        let () = Self::CAPACITY_OK;
        let next_free = std::array::from_fn(|i| if i + 1 < N { (i + 1) as u32 } else { NONE });
        HandleMap {
            values: [const { MaybeUninit::uninit() }; N],
            occupied: [false; N],
            generation: [G::ZERO; N],
            next_free,
            free_head: if N > 0 { 0 } else { NONE },
            len: 0,
        }
    }

    /// Number of values in the map.
    pub fn len(&self) -> usize {
        self.len as usize
    }

    /// True if the map holds no values.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Number of slots.
    pub const fn capacity(&self) -> usize {
        N
    }

    /// Number of slots retired because their generation ran out.
    pub fn retired(&self) -> usize {
        (0..N).filter(|&i| self.is_retired(i)).count()
    }

    fn is_retired(&self, i: usize) -> bool {
        RETIRE && !self.occupied[i] && self.generation[i] == G::MAX
    }

    /// Index of the slot `h` refers to, if `h` is live.
    fn live_index(&self, h: Handle<G>) -> Option<usize> {
        let i = h.idx as usize;
        (i < N && self.occupied[i] && self.generation[i] == h.generation).then_some(i)
    }

    /// True if `h` refers to a value in the map.
    pub fn contains(&self, h: Handle<G>) -> bool {
        self.live_index(h).is_some()
    }

    /// Moves `value` into a free slot and returns its handle, or gives the
    /// value back if every slot is taken or retired.
    pub fn add(&mut self, value: T) -> Result<Handle<G>, T> {
        let idx = self.free_head;
        if idx == NONE {
            return Err(value);
        }
        let i = idx as usize;
        self.free_head = self.next_free[i];
        // Free slots are below `MAX` (they would be retired otherwise), so
        // this never wraps unless `naive-generations` is on.
        let generation = self.generation[i].wrapping_inc();
        self.generation[i] = generation;
        self.values[i].write(value);
        self.occupied[i] = true;
        self.len += 1;
        Ok(Handle { idx, generation })
    }

    /// The value `h` refers to, if it is still in the map.
    pub fn get(&self, h: Handle<G>) -> Option<&T> {
        let i = self.live_index(h)?;
        // SAFETY: occupied slots are initialized.
        Some(unsafe { self.values[i].assume_init_ref() })
    }

    /// The value `h` refers to, mutably.
    pub fn get_mut(&mut self, h: Handle<G>) -> Option<&mut T> {
        let i = self.live_index(h)?;
        // SAFETY: occupied slots are initialized.
        Some(unsafe { self.values[i].assume_init_mut() })
    }

    /// Removes and returns the value `h` refers to. Afterwards `h`, and every
    /// other handle to that value, never resolves again.
    pub fn remove(&mut self, h: Handle<G>) -> Option<T> {
        let i = self.live_index(h)?;
        self.occupied[i] = false;
        self.len -= 1;
        // SAFETY: the slot was occupied, and is now marked free, so the value
        // is moved out exactly once.
        let value = unsafe { self.values[i].assume_init_read() };
        if !self.is_retired(i) {
            self.next_free[i] = self.free_head;
            self.free_head = h.idx;
        }
        Some(value)
    }

    /// Removes every value. Slot generations are kept, so old handles stay dead.
    pub fn clear(&mut self) {
        for i in 0..N {
            if self.occupied[i] {
                let generation = self.generation[i];
                self.remove(Handle {
                    idx: i as u32,
                    generation,
                });
            }
        }
    }

    /// Every value with its handle, in slot order.
    pub fn iter(&self) -> impl Iterator<Item = (Handle<G>, &T)> + '_ {
        (0..N).filter(|&i| self.occupied[i]).map(|i| {
            let h = Handle {
                idx: i as u32,
                generation: self.generation[i],
            };
            // SAFETY: occupied slots are initialized.
            (h, unsafe { self.values[i].assume_init_ref() })
        })
    }

    /// Checks the bookkeeping. Used by the proofs and by debug assertions in
    /// the tests; it never reads values.
    ///
    /// 1. `len` equals the number of occupied slots.
    /// 2. Occupied slots have a generation above `ZERO`.
    /// 3. The free list visits each non-retired empty slot exactly once,
    ///    visits nothing else, and ends.
    pub fn check_invariant(&self) -> bool {
        let mut occupied = 0;
        let mut reusable = 0;
        for i in 0..N {
            if self.occupied[i] {
                occupied += 1;
                if self.generation[i] == G::ZERO {
                    return false;
                }
            } else if !self.is_retired(i) {
                reusable += 1;
            }
        }
        if self.len as usize != occupied {
            return false;
        }
        let mut seen = [false; N];
        let mut count = 0;
        let mut cur = self.free_head;
        while cur != NONE {
            let i = cur as usize;
            if i >= N || seen[i] || self.occupied[i] || self.is_retired(i) || count == reusable {
                return false;
            }
            seen[i] = true;
            count += 1;
            cur = self.next_free[i];
        }
        count == reusable
    }
}

impl<T, const N: usize, G: Generation> Default for HandleMap<T, N, G> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, const N: usize, G: Generation> Drop for HandleMap<T, N, G> {
    fn drop(&mut self) {
        for i in 0..N {
            if self.occupied[i] {
                // SAFETY: occupied slots are initialized and dropped once.
                unsafe { self.values[i].assume_init_drop() }
            }
        }
    }
}

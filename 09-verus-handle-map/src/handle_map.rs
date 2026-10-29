//! A handle map proved correct with Verus for every capacity.
//!
//! Same design as `08-verified-handle-map`, which Kani checks at capacity 4:
//! - a handle is a slot index plus a generation;
//! - each slot has its own occupied state (`Option<T>` here) and a generation
//!   that only goes up;
//! - a slot whose generation reaches `MAX_GEN` is retired, never wrapped;
//! - free slots form an intrusive list through `next_free`, starting at
//!   `free_head`, ending at `NONE`.
//!
//! Differences from 08: slots live in a `Vec<Slot<T>, A>` from any
//! `Allocator` instead of a `[MaybeUninit<T>; N]`, values are `Option<T>`
//! rather than `MaybeUninit<T>` plus a flag, the capacity is a run-time
//! value, and the generation is a fixed `u32`.
//! Everything here is safe Rust, so the proof is about the bookkeeping, not
//! about memory safety. 08's Kani and Miri checks cover the `unsafe` version.
use std::alloc::Allocator;
use vstd::prelude::*;

verus! {

/// End of the free list.
pub const NONE: u32 = 0xffff_ffff;

/// The last generation a slot can have. A free slot at this generation is
/// retired.
pub const MAX_GEN: u32 = 0xffff_ffff;

/// A reference to a value in a [`HandleMap`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Handle {
    pub idx: u32,
    pub generation: u32,
}

pub struct Slot<T> {
    value: Option<T>,
    generation: u32,
    next_free: u32,
}

/// A map from [`Handle`] to `T` with a fixed number of slots, allocated from `A`.
#[verifier::reject_recursive_types(A)]
pub struct HandleMap<T, A: Allocator> {
    slots: Vec<Slot<T>, A>,
    free_head: u32,
    len: u32,
    /// Ghost: the free list as a sequence of slot indices, head first.
    free_list: Ghost<Seq<int>>,
    /// Ghost: the indices of occupied slots.
    occupied: Ghost<Set<int>>,
    /// Ghost: every handle `add` has ever returned.
    issued: Ghost<Set<Handle>>,
    /// Ghost: every handle `remove` has ever removed.
    dead: Ghost<Set<Handle>>,
}

impl<T, A: Allocator> HandleMap<T, A> {
    // ----- Specification: what callers can see -----
    /// Number of slots.
    pub closed spec fn cap(&self) -> nat {
        self.slots@.len()
    }

    /// Number of values in the map.
    pub closed spec fn count(&self) -> nat {
        self.len as nat
    }

    pub closed spec fn generation_of(&self, i: int) -> u32 {
        self.slots@[i].generation
    }

    pub closed spec fn is_occupied(&self, i: int) -> bool {
        self.slots@[i].value is Some
    }

    pub open spec fn is_retired(&self, i: int) -> bool {
        !self.is_occupied(i) && self.generation_of(i) == MAX_GEN
    }

    /// Empty and not retired: `add` may use it.
    pub open spec fn is_reusable(&self, i: int) -> bool {
        !self.is_occupied(i) && self.generation_of(i) < MAX_GEN
    }

    /// `h` resolves to a value.
    pub open spec fn live(&self, h: Handle) -> bool {
        &&& h.idx < self.cap()
        &&& self.is_occupied(h.idx as int)
        &&& self.generation_of(h.idx as int) == h.generation
    }

    /// The value a live handle resolves to.
    pub closed spec fn value_of(&self, h: Handle) -> T {
        self.slots@[h.idx as int].value->Some_0
    }

    pub closed spec fn issued(&self) -> Set<Handle> {
        self.issued@
    }

    pub closed spec fn dead(&self) -> Set<Handle> {
        self.dead@
    }

    // ----- The invariant -----
    /// Well-formedness. Every public operation needs it and keeps it.
    /// Split into opaque parts so each proof step only sees the part it needs.
    pub closed spec fn wf(&self) -> bool {
        // Slot indices fit in a u32 and never equal NONE.
        &&& self.slots@.len() < NONE
        &&& self.wf_free()
        &&& self.wf_count()
        &&& self.wf_handles()
    }

    /// The free list is exactly the reusable slots, each once, and the
    /// runtime links (`free_head`, `next_free`) spell out that sequence and
    /// end in `NONE`. So it has no duplicates and no cycle.
    #[verifier::opaque]
    closed spec fn wf_free(&self) -> bool {
        let n = self.slots@.len();
        let fl = self.free_list@;
        &&& fl.no_duplicates()
        &&& forall|k: int| 0 <= k < fl.len() ==> 0 <= #[trigger] fl[k] < n && self.is_reusable(fl[k])
        &&& forall|i: int| 0 <= i < n && #[trigger] self.is_reusable(i) ==> fl.contains(i)
        &&& self.free_head == (if fl.len() == 0 {
            NONE
        } else {
            fl[0] as u32
        })
        &&& forall|k: int|
            0 <= k < fl.len() ==> self.slots@[#[trigger] fl[k]].next_free == (if k + 1 < fl.len() {
                fl[k + 1] as u32
            } else {
                NONE
            })
    }

    /// `len` counts the occupied slots.
    #[verifier::opaque]
    closed spec fn wf_count(&self) -> bool {
        let n = self.slots@.len();
        &&& self.len as nat == self.occupied@.len()
        &&& forall|i: int| #[trigger]
            self.occupied@.contains(i) <==> (0 <= i < n && self.is_occupied(i))
        &&& self.len + self.free_list@.len() <= n
    }

    /// Every occupied slot's current handle was issued; issued handles point
    /// at a real slot with a generation in `1..=` the slot's; a removed
    /// handle's slot is empty or has moved past it.
    #[verifier::opaque]
    closed spec fn wf_handles(&self) -> bool {
        let n = self.slots@.len();
        &&& forall|i: int|
            0 <= i < n && #[trigger] self.is_occupied(i) ==> self.issued@.contains(
                Handle { idx: i as u32, generation: self.generation_of(i) },
            )
        &&& forall|h: Handle| #[trigger]
            self.issued@.contains(h) ==> h.idx < n && 0 < h.generation <= self.generation_of(
                h.idx as int,
            )
        &&& forall|h: Handle| #[trigger]
            self.dead@.contains(h) ==> self.issued@.contains(h) && (!self.is_occupied(h.idx as int)
                || self.generation_of(h.idx as int) > h.generation)
    }

    // ----- Lemmas callers can use -----
    /// A removed handle never resolves, whatever has happened since.
    pub proof fn lemma_dead_never_live(&self)
        requires
            self.wf(),
        ensures
            forall|h: Handle| #[trigger] self.dead().contains(h) ==> !self.live(h),
            self.count() <= self.cap(),
    {
        reveal(HandleMap::wf_handles);
        reveal(HandleMap::wf_count);
    }

    /// Every handle the map has given out is in range and has a generation
    /// above 0, so `Handle { idx: 0, generation: 0 }` is never issued.
    pub proof fn lemma_issued_in_range(&self)
        requires
            self.wf(),
        ensures
            forall|h: Handle| #[trigger]
                self.issued().contains(h) ==> h.idx < self.cap() && h.generation > 0,
    {
        reveal(HandleMap::wf_handles);
    }

    // ----- Operations -----
    /// Creates an empty map with `capacity` slots allocated from `alloc`.
    pub fn new_in(capacity: u32, alloc: A) -> (m: Self)
        requires
            capacity < NONE,
        ensures
            m.wf(),
            m.cap() == capacity,
            m.count() == 0,
            m.issued() == Set::<Handle>::empty(),
            m.dead() == Set::<Handle>::empty(),
            forall|i: int| 0 <= i < capacity ==> !m.is_occupied(i) && m.generation_of(i) == 0,
    {
        let mut slots: Vec<Slot<T>, A> = Vec::with_capacity_in(capacity as usize, alloc);
        let mut i: u32 = 0;
        while i < capacity
            invariant
                i <= capacity < NONE,
                slots@.len() == i,
                forall|j: int|
                    0 <= j < i ==> {
                        &&& (#[trigger] slots@[j]).value is None
                        &&& slots@[j].generation == 0
                        &&& slots@[j].next_free == (if j + 1 < capacity {
                            (j + 1) as u32
                        } else {
                            NONE
                        })
                    },
            decreases capacity - i,
        {
            let next = if i + 1 < capacity {
                i + 1
            } else {
                NONE
            };
            slots.push(Slot { value: None, generation: 0, next_free: next });
            i = i + 1;
        }
        let ghost fl = Seq::new(capacity as nat, |k: int| k);
        let m = HandleMap {
            slots,
            free_head: if capacity > 0 {
                0
            } else {
                NONE
            },
            len: 0,
            free_list: Ghost(fl),
            occupied: Ghost(Set::empty()),
            issued: Ghost(Set::empty()),
            dead: Ghost(Set::empty()),
        };
        assert(m.wf_free()) by {
            reveal(HandleMap::wf_free);
            assert forall|i: int|
                0 <= i < capacity && #[trigger] m.is_reusable(i) implies fl.contains(i) by {
                assert(fl[i] == i);
            }
        }
        assert(m.wf_count()) by {
            reveal(HandleMap::wf_count);
        }
        assert(m.wf_handles()) by {
            reveal(HandleMap::wf_handles);
        }
        m
    }

    /// Number of values in the map.
    pub fn len(&self) -> (r: u32)
        requires
            self.wf(),
        ensures
            r == self.count(),
    {
        self.len
    }

    /// Number of slots.
    pub fn capacity(&self) -> (r: usize)
        ensures
            r == self.cap(),
    {
        self.slots.len()
    }

    /// True if `h` resolves to a value.
    pub fn contains(&self, h: Handle) -> (r: bool)
        requires
            self.wf(),
        ensures
            r == self.live(h),
    {
        let i = h.idx as usize;
        i < self.slots.len() && self.slots[i].value.is_some() && self.slots[i].generation
            == h.generation
    }

    /// The value `h` refers to, if it is still in the map.
    pub fn get(&self, h: Handle) -> (r: Option<&T>)
        requires
            self.wf(),
        ensures
            match r {
                Some(v) => self.live(h) && *v == self.value_of(h),
                None => !self.live(h),
            },
    {
        let i = h.idx as usize;
        if i < self.slots.len() && self.slots[i].generation == h.generation {
            self.slots[i].value.as_ref()
        } else {
            None
        }
    }

    /// Moves `value` into a free slot and returns its handle, or gives the
    /// value back if every slot is occupied or retired.
    pub fn add(&mut self, value: T) -> (r: Result<Handle, T>)
        requires
            old(self).wf(),
        ensures
            final(self).wf(),
            final(self).cap() == old(self).cap(),
            final(self).dead() == old(self).dead(),
            forall|i: int|
                0 <= i < old(self).cap() ==> old(self).generation_of(i) <= final(self).generation_of(i),
            match r {
                Ok(h) => {
                    // The new handle uses a reusable slot, bumps its
                    // generation, and resolves to `value`...
                    &&& old(self).is_reusable(h.idx as int)
                    &&& h.generation == old(self).generation_of(h.idx as int) + 1
                    &&& final(self).live(h)
                    &&& final(self).value_of(h) == value
                    // ...was never handed out before...
                    &&& !old(self).issued().contains(h)
                    &&& final(self).issued() == old(self).issued().insert(h)
                    &&& final(self).count() == old(self).count() + 1
                    // ...and every other handle is unaffected.
                    &&& forall|g: Handle| #[trigger]
                        old(self).live(g) ==> final(self).live(g) && final(self).value_of(g)
                            == old(self).value_of(g)
                    &&& forall|g: Handle| #[trigger] final(self).live(g) ==> old(self).live(g) || g
                        == h
                },
                Err(v) => {
                    // Failure only when there is truly no room, and nothing changes.
                    &&& v == value
                    &&& *final(self) == *old(self)
                    &&& forall|i: int|
                        0 <= i < old(self).cap() ==> old(self).is_occupied(i) || old(self).is_retired(i)
                },
            },
    {
        let idx = self.free_head;
        let ghost old_map = *self;
        let ghost fl = self.free_list@;
        if idx == NONE {
            proof {
                assert forall|i: int| 0 <= i < self.slots@.len() implies self.is_occupied(i)
                    || self.is_retired(i) by {
                    reveal(HandleMap::wf_free);
                    if self.is_reusable(i) {
                        assert(fl.contains(i));
                    }
                }
            }
            return Err(value);
        }
        proof {
            assert(fl.len() > 0 && 0 <= fl[0] < self.slots@.len() && fl[0] == idx as int
                && self.is_reusable(idx as int)) by {
                reveal(HandleMap::wf_free);
            }
            assert(self.len + 1 <= self.slots@.len()) by {
                reveal(HandleMap::wf_count);
            }
        }
        let i = idx as usize;
        let generation = self.slots[i].generation + 1;
        let next = self.slots[i].next_free;
        self.slots[i] = Slot { value: Some(value), generation, next_free: next };
        self.free_head = next;
        self.len = self.len + 1;
        let h = Handle { idx, generation };
        self.free_list = Ghost(fl.drop_first());
        self.occupied = Ghost(self.occupied@.insert(i as int));
        self.issued = Ghost(self.issued@.insert(h));
        proof {
            let n = self.slots@.len();
            let nfl = self.free_list@;
            assert(self.slots@ == old_map.slots@.update(
                i as int,
                Slot { value: Some(value), generation, next_free: next },
            ));
            assert(self.wf_free()) by {
                reveal(HandleMap::wf_free);
                assert forall|k: int| 0 <= k < nfl.len() implies nfl[k] != i && 0
                    <= #[trigger] nfl[k] < n && self.is_reusable(nfl[k]) by {
                    assert(nfl[k] == fl[k + 1]);
                }
                assert(nfl.no_duplicates()) by {
                    assert forall|a: int, b: int|
                        0 <= a < b < nfl.len() implies nfl[a] != nfl[b] by {
                        assert(nfl[a] == fl[a + 1]);
                        assert(nfl[b] == fl[b + 1]);
                    }
                }
                assert forall|j: int| 0 <= j < n && #[trigger] self.is_reusable(j) implies nfl.contains(
                    j,
                ) by {
                    assert(old_map.is_reusable(j));
                    let k = choose|k: int| 0 <= k < fl.len() && fl[k] == j;
                    assert(nfl[k - 1] == j);
                }
                assert forall|k: int| 0 <= k < nfl.len() implies self.slots@[#[trigger] nfl[k]].next_free
                    == (if k + 1 < nfl.len() {
                    nfl[k + 1] as u32
                } else {
                    NONE
                }) by {
                    assert(nfl[k] == fl[k + 1]);
                    assert(old_map.slots@[fl[k + 1]].next_free == (if k + 2 < fl.len() {
                        fl[k + 2] as u32
                    } else {
                        NONE
                    }));
                }
                assert(old_map.slots@[fl[0]].next_free == (if 1 < fl.len() {
                    fl[1] as u32
                } else {
                    NONE
                }));
            }
            assert(self.wf_count()) by {
                reveal(HandleMap::wf_count);
                reveal(HandleMap::wf_free);
                assert(!old_map.occupied@.contains(i as int));
            }
            assert(self.wf_handles()) by {
                reveal(HandleMap::wf_handles);
                assert forall|j: int| 0 <= j < n && #[trigger] self.is_occupied(j) implies self.issued@.contains(
                    Handle { idx: j as u32, generation: self.generation_of(j) },
                ) by {
                    if j == i {
                        assert(Handle { idx: j as u32, generation: self.generation_of(j) } == h);
                    } else {
                        assert(old_map.is_occupied(j));
                    }
                }
                assert forall|g: Handle| #[trigger] self.issued@.contains(g) implies g.idx < n && 0
                    < g.generation <= self.generation_of(g.idx as int) by {
                    if g != h {
                        assert(old_map.issued@.contains(g));
                    }
                }
                assert forall|g: Handle| #[trigger] self.dead@.contains(g) implies self.issued@.contains(g)
                    && (!self.is_occupied(g.idx as int) || self.generation_of(g.idx as int)
                    > g.generation) by {
                    assert(old_map.dead@.contains(g));
                    assert(old_map.issued@.contains(g));
                }
            }
            assert forall|g: Handle| #[trigger] old_map.live(g) implies self.live(g)
                && self.value_of(g) == old_map.value_of(g) by {
                assert(g.idx != idx);
            }
            assert(!old_map.issued@.contains(h)) by {
                reveal(HandleMap::wf_handles);
            }
        }
        Ok(h)
    }

    /// Removes and returns the value `h` refers to. Afterwards `h`, and every
    /// other handle to that value, never resolves again.
    pub fn remove(&mut self, h: Handle) -> (r: Option<T>)
        requires
            old(self).wf(),
        ensures
            final(self).wf(),
            final(self).cap() == old(self).cap(),
            final(self).issued() == old(self).issued(),
            forall|i: int|
                0 <= i < old(self).cap() ==> old(self).generation_of(i) == final(self).generation_of(i),
            old(self).live(h) ==> {
                &&& r == Some(old(self).value_of(h))
                &&& !final(self).is_occupied(h.idx as int)
                &&& !final(self).live(h)
                &&& final(self).dead() == old(self).dead().insert(h)
                &&& final(self).count() == old(self).count() - 1
            },
            !old(self).live(h) ==> r is None && *final(self) == *old(self),
            forall|g: Handle| #[trigger]
                old(self).live(g) && g != h ==> final(self).live(g) && final(self).value_of(g)
                    == old(self).value_of(g),
            forall|g: Handle| #[trigger] final(self).live(g) ==> old(self).live(g),
    {
        let i = h.idx as usize;
        if !(i < self.slots.len() && self.slots[i].value.is_some() && self.slots[i].generation
            == h.generation) {
            return None;
        }
        let ghost old_map = *self;
        let ghost fl = self.free_list@;
        proof {
            assert(!fl.contains(i as int)) by {
                reveal(HandleMap::wf_free);
            }
            assert(self.len >= 1 && self.occupied@.remove(i as int).len() == self.len - 1) by {
                reveal(HandleMap::wf_count);
                assert(self.occupied@.contains(i as int));
            }
            assert(self.issued@.contains(h)) by {
                reveal(HandleMap::wf_handles);
                assert(self.is_occupied(i as int));
                assert(h == Handle { idx: (i as int) as u32, generation: self.generation_of(i as int) });
            }
        }
        let value = self.slots[i].value.take();
        self.slots[i].next_free = self.free_head;
        self.len = self.len - 1;
        if self.slots[i].generation != MAX_GEN {
            self.free_head = h.idx;
            self.free_list = Ghost(seq![i as int] + fl);
        }
        self.occupied = Ghost(self.occupied@.remove(i as int));
        self.dead = Ghost(self.dead@.insert(h));
        proof {
            let n = self.slots@.len();
            let nfl = self.free_list@;
            let old_slot = old_map.slots@[i as int];
            assert(self.slots@ =~= old_map.slots@.update(
                i as int,
                Slot { value: None, generation: old_slot.generation, next_free: old_map.free_head },
            ));
            assert(self.wf_free()) by {
                reveal(HandleMap::wf_free);
                if old_slot.generation != MAX_GEN {
                    assert forall|k: int| 0 <= k < nfl.len() implies 0 <= #[trigger] nfl[k] < n
                        && self.is_reusable(nfl[k]) by {
                        if k > 0 {
                            assert(nfl[k] == fl[k - 1]);
                        }
                    }
                    assert forall|j: int| 0 <= j < n && #[trigger] self.is_reusable(
                        j,
                    ) implies nfl.contains(j) by {
                        if j == i {
                            assert(nfl[0] == j);
                        } else {
                            assert(old_map.is_reusable(j));
                            let k = choose|k: int| 0 <= k < fl.len() && fl[k] == j;
                            assert(nfl[k + 1] == j);
                        }
                    }
                    assert forall|k: int| 0 <= k < nfl.len() implies self.slots@[#[trigger] nfl[k]].next_free
                        == (if k + 1 < nfl.len() {
                        nfl[k + 1] as u32
                    } else {
                        NONE
                    }) by {
                        if k > 0 {
                            assert(nfl[k] == fl[k - 1]);
                            assert(nfl[k] != i);
                        } else if fl.len() > 0 {
                            assert(nfl[1] == fl[0]);
                        }
                    }
                    assert(nfl.no_duplicates()) by {
                        assert forall|a: int, b: int|
                            0 <= a < b < nfl.len() implies nfl[a] != nfl[b] by {
                            if a > 0 {
                                assert(nfl[a] == fl[a - 1]);
                            }
                            assert(nfl[b] == fl[b - 1]);
                        }
                    }
                } else {
                    assert forall|k: int| 0 <= k < nfl.len() implies #[trigger] nfl[k] != i by {}
                    assert forall|j: int| 0 <= j < n && #[trigger] self.is_reusable(
                        j,
                    ) implies nfl.contains(j) by {
                        assert(old_map.is_reusable(j));
                    }
                }
            }
            assert(self.wf_count()) by {
                reveal(HandleMap::wf_count);
            }
            assert(self.wf_handles()) by {
                reveal(HandleMap::wf_handles);
                assert forall|j: int| 0 <= j < n && #[trigger] self.is_occupied(j) implies self.issued@.contains(
                    Handle { idx: j as u32, generation: self.generation_of(j) },
                ) by {
                    assert(old_map.is_occupied(j));
                }
                assert forall|g: Handle| #[trigger] self.issued@.contains(g) implies g.idx < n && 0
                    < g.generation <= self.generation_of(g.idx as int) by {
                    assert(old_map.issued@.contains(g));
                }
                assert forall|g: Handle| #[trigger] self.dead@.contains(g) implies self.issued@.contains(g)
                    && (!self.is_occupied(g.idx as int) || self.generation_of(g.idx as int)
                    > g.generation) by {
                    if g != h {
                        assert(old_map.dead@.contains(g));
                    }
                }
            }
            assert forall|g: Handle| #[trigger]
                old_map.live(g) && g != h implies self.live(g) && self.value_of(g)
                    == old_map.value_of(g) by {
                assert(g.idx != h.idx);
            }
        }
        value
    }
}

} // verus!

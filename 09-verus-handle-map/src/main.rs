// Not needed from Rust 1.100, where the allocator API is stable. Verus still
// builds on Rust 1.99.
#![feature(allocator_api)]

mod handle_map;

use handle_map::{Handle, HandleMap};
use std::alloc::{AllocError, Allocator, Global, Layout};
use std::cell::Cell;
use std::ptr::NonNull;
use vstd::prelude::*;

verus! {

/// A verified client. Verus checks these asserts for every allocator and
/// every value of `x`, using only the specs of `HandleMap`.
fn stale_handle_demo<A: Allocator>(alloc: A, x: u64) -> (r: (Handle, Handle))
    requires
        x < u64::MAX,
    ensures
        r.0 != r.1,
        r.0.idx == r.1.idx,
{
    let mut m: HandleMap<u64, A> = HandleMap::new_in(1, alloc);
    assert(m.is_reusable(0));
    let a = match m.add(x) {
        Ok(h) => h,
        Err(_) => {
            assert(m.is_occupied(0) || m.is_retired(0));
            // One empty slot, so `add` cannot fail. Verus proves this branch is dead.
            assert(false);
            return (Handle { idx: 0, generation: 0 }, Handle { idx: 0, generation: 0 });
        },
    };
    assert(m.live(a) && m.value_of(a) == x);
    let full = m.add(7);
    assert(full is Err);  // capacity 1
    let removed = m.remove(a);
    assert(removed == Some(x));
    let y = x + 1;
    assert(m.is_reusable(0));
    let b = match m.add(y) {
        Ok(h) => h,
        Err(_) => {
            assert(m.is_occupied(0) || m.is_retired(0));
            assert(false);
            return (a, a);
        },
    };
    proof {
        m.lemma_dead_never_live();
        m.lemma_issued_in_range();
    }
    // Same slot, new generation: the old handle does not see the new value.
    assert(a != b);
    assert(!m.live(a));
    assert(m.live(b) && m.value_of(b) == y);
    // The same facts through the exec API.
    let ga = m.get(a);
    let gb = m.get(b);
    assert(ga is None);
    assert(gb == Some(&y));
    (a, b)
}

/// Any number of add/remove pairs on a map of any size keep a removed
/// handle dead. The loop invariant only needs `wf` and the `dead` set.
fn churn<A: Allocator>(m: &mut HandleMap<u64, A>, stale: Handle, rounds: u32)
    requires
        old(m).wf(),
        old(m).dead().contains(stale),
    ensures
        final(m).wf(),
        !final(m).live(stale),
{
    let mut k: u32 = 0;
    while k < rounds
        invariant
            m.wf(),
            m.dead().contains(stale),
        decreases rounds - k,
    {
        if let Ok(h) = m.add(k as u64) {
            m.remove(h);
        }
        k = k + 1;
    }
    proof {
        m.lemma_dead_never_live();
    }
}

} // verus!

/// A custom allocator that counts live bytes, to show where the slots come from.
struct Counting {
    live: Cell<usize>,
}

unsafe impl Allocator for &Counting {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        self.live.set(self.live.get() + layout.size());
        Global.allocate(layout)
    }
    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        self.live.set(self.live.get() - layout.size());
        // SAFETY: `ptr` came from `Global.allocate` with this layout.
        unsafe { Global.deallocate(ptr, layout) }
    }
}

fn main() {
    let (a, b) = stale_handle_demo(Global, 41);
    println!("old handle {a:?}, new handle {b:?}");

    let counting = Counting { live: Cell::new(0) };
    {
        let mut m: HandleMap<String, &Counting> = HandleMap::new_in(1000, &counting);
        println!("1000 slots: {} bytes from the counting allocator", counting.live.get());
        let h = m.add("hello".to_string()).unwrap();
        println!("get(h) = {:?}", m.get(h));
        println!("remove(h) = {:?}", m.remove(h));
        let h2 = m.add("again".to_string()).unwrap();
        println!("after reuse: get(h) = {:?}, get(h2) = {:?}, len = {}", m.get(h), m.get(h2), m.len());
        println!("capacity = {}", m.capacity());
    }
    println!("after drop: {} bytes live", counting.live.get());
}

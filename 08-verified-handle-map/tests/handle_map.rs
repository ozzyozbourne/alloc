use std::collections::HashMap;

use verified_handle_map::{Handle, HandleMap};

/// Small xorshift so the tests are deterministic without extra crates.
fn rng(mut state: u64) -> impl FnMut() -> u64 {
    move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    }
}

#[test]
fn basic_add_get_remove() {
    let mut m: HandleMap<&str, 4> = HandleMap::new();
    let a = m.add("a").unwrap();
    let b = m.add("b").unwrap();
    assert_eq!((m.get(a), m.get(b)), (Some(&"a"), Some(&"b")));
    assert_eq!(m.remove(a), Some("a"));
    assert_eq!(m.get(a), None);
    let c = m.add("c").unwrap();
    assert_eq!(c.index(), a.index(), "the freed slot is reused");
    assert_ne!(c, a, "with a new generation");
    assert_eq!(m.get(a), None);
    assert_eq!(m.len(), 2);
    assert!(m.get(Handle::NONE).is_none());
}

#[test]
fn full_map_gives_the_value_back() {
    let mut m: HandleMap<String, 2> = HandleMap::new();
    m.add("x".into()).unwrap();
    m.add("y".into()).unwrap();
    assert_eq!(m.add("z".into()), Err("z".to_string()));
}

/// With `u8` generations a slot can be reused 255 times. The 256th removal
/// retires it, so a handle from the first use can never come back.
/// slotmap and Odin wrap instead; `naive-generations` shows that.
#[test]
fn exhausted_slots_are_retired_not_wrapped() {
    let mut m: HandleMap<u32, 1, u8> = HandleMap::new();
    let first = m.add(0).unwrap();
    m.remove(first);
    let mut revived = false;
    for i in 1..300u32 {
        match m.add(i) {
            Ok(h) => {
                revived |= m.contains(first);
                m.remove(h);
            }
            Err(_) => {
                assert_eq!(i, 255, "the slot retires after generation 255");
                break;
            }
        }
    }
    if cfg!(feature = "naive-generations") {
        assert!(revived, "wrapping generations revive the first handle");
    } else {
        assert!(!revived);
        assert_eq!(m.retired(), 1);
    }
}

#[test]
fn clear_keeps_old_handles_dead() {
    let mut m: HandleMap<u8, 8> = HandleMap::new();
    let hs: Vec<_> = (0..8).map(|i| m.add(i).unwrap()).collect();
    m.clear();
    assert!(m.is_empty());
    let new: Vec<_> = (0..8).map(|i| m.add(i).unwrap()).collect();
    assert!(hs.iter().all(|&h| !m.contains(h)));
    assert!(new.iter().all(|&h| m.contains(h)));
}

#[test]
fn drops_each_value_once() {
    use std::rc::Rc;
    let tracker = Rc::new(());
    {
        let mut m: HandleMap<Rc<()>, 8> = HandleMap::new();
        let hs: Vec<_> = (0..8).map(|_| m.add(tracker.clone()).unwrap()).collect();
        drop(m.remove(hs[3]));
        assert_eq!(Rc::strong_count(&tracker), 8);
    }
    assert_eq!(Rc::strong_count(&tracker), 1);
}

/// Random operations compared against a `HashMap<Handle, value>` model, with
/// the invariant checked after every step. Stale handles are kept and must
/// never resolve again.
///
/// Under `naive-generations` this fails by design: a `u8` generation wraps to
/// 0 on an occupied slot, which breaks the invariant (and lets `Handle::NONE`
/// resolve to a live value).
#[test]
#[cfg_attr(feature = "naive-generations", should_panic(expected = "step"))]
fn matches_a_hashmap_model() {
    const N: usize = if cfg!(miri) { 2 } else { 16 };
    let mut next = rng(0xC0FFEE);
    let mut map: HandleMap<u64, N, u8> = HandleMap::new();
    let mut model: HashMap<Handle<u8>, u64> = HashMap::new();
    let mut dead: Vec<Handle<u8>> = Vec::new();

    // Miri is ~1000x slower; fewer steps still reach slot retirement.
    let steps = if cfg!(miri) { 2_000 } else { 20_000 };
    for step in 0..steps {
        let roll = next() % 10;
        if roll < 5 {
            let v = next();
            match map.add(v) {
                Ok(h) => assert!(model.insert(h, v).is_none(), "step {step}: handle reissued"),
                Err(back) => {
                    assert_eq!(back, v);
                    assert_eq!(
                        model.len() + map.retired(),
                        N,
                        "step {step}: full too early"
                    );
                }
            }
        } else if roll < 9 && !model.is_empty() {
            let h = *model
                .keys()
                .nth((next() % model.len() as u64) as usize)
                .unwrap();
            assert_eq!(map.remove(h), model.remove(&h));
            dead.push(h);
        } else if let Some(&h) = dead.get((next() % (dead.len() as u64 + 1)) as usize) {
            assert_eq!(
                map.remove(h),
                None,
                "step {step}: stale handle removed something"
            );
        }

        assert!(map.check_invariant(), "step {step}");
        assert_eq!(map.len(), model.len());
        for (h, v) in &model {
            assert_eq!(map.get(*h), Some(v));
        }
        for h in dead.iter().rev().take(32) {
            assert!(!map.contains(*h), "step {step}: stale handle came back");
        }
    }
    // `u8` generations and 20k operations on 16 slots: some slots retired.
    assert!(map.retired() > 0);
}

/// The same operation sequence on this map and on `slotmap::SlotMap` (with the
/// map kept below capacity) must give the same values and the same liveness
/// for every handle, live or stale.
#[test]
fn behaves_like_slotmap() {
    const N: usize = 64;
    let mut next = rng(42);
    let mut ours: HandleMap<u64, N> = HandleMap::new();
    let mut theirs: slotmap::SlotMap<slotmap::DefaultKey, u64> = slotmap::SlotMap::new();
    let mut pairs: Vec<(Handle, slotmap::DefaultKey)> = Vec::new();

    for _ in 0..if cfg!(miri) { 500 } else { 10_000 } {
        if next().is_multiple_of(2) && ours.len() < N {
            let v = next();
            pairs.push((ours.add(v).unwrap(), theirs.insert(v)));
        } else if !pairs.is_empty() {
            let (h, k) = pairs[(next() % pairs.len() as u64) as usize];
            assert_eq!(ours.remove(h), theirs.remove(k));
        }
        assert_eq!(ours.len(), theirs.len());
    }
    for (h, k) in pairs {
        assert_eq!(ours.get(h), theirs.get(k));
    }
}

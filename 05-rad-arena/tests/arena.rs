#![allow(clippy::undocumented_unsafe_blocks)] // tests: each unsafe call is the thing under test

use std::alloc::{Allocator, Layout};

use rad_arena::{Arena, ArenaParams};

fn small() -> Arena {
    Arena::with_params(ArenaParams {
        reserve_size: 64 << 10,
        commit_size: 4 << 10,
        chain: true,
    })
}

#[test]
fn starts_after_the_header_and_aligns_every_push() {
    let arena = Arena::new();
    let a = arena.push(1u8);
    let a_addr = (a as *const u8).addr();
    let b = arena.push(2u64);
    assert_eq!((b as *const u64).addr() % 8, 0);
    assert_eq!(
        (b as *const u64).addr() - a_addr,
        8,
        "7 bytes of padding after the u8"
    );
    let s = arena.push_str("hello");
    assert_eq!(&*s, "hello");
    assert_eq!(*b, 2);
}

#[test]
fn temp_scopes_pop_back_and_nest() {
    let mut arena = small();
    let start = arena.pos();
    {
        let mut outer = arena.temp();
        outer.push([0u8; 100]);
        let mid = outer.pos();
        {
            let inner = outer.temp();
            inner.push([0u8; 1000]);
            assert!(inner.pos() > mid);
        }
        assert_eq!(outer.pos(), mid);
    }
    assert_eq!(arena.pos(), start);
}

#[test]
fn commits_pages_lazily() {
    let arena = small();
    let first = arena.committed();
    assert_eq!(first, 4 << 10);
    arena.push([0u8; 10_000]);
    // 128 + 10000 bytes, rounded up to the 4 KiB commit step.
    assert_eq!(arena.committed(), 12 << 10);
}

#[test]
fn chains_new_blocks_and_releases_them_on_pop() {
    let mut arena = small();
    let start = arena.pos();
    let mut pointers = Vec::new();
    for i in 0..10u32 {
        let block = arena.push([i as u8; 20_000]);
        pointers.push(block.as_ptr());
    }
    // 64 KiB blocks hold three 20,000-byte pushes each.
    assert_eq!(arena.block_count(), 4);
    for (i, p) in pointers.iter().enumerate() {
        assert_eq!(unsafe { **p }, i as u8, "block {i} was overwritten");
    }
    let mid = arena.pos();
    arena.push([0u8; 1]);
    arena.pop_to(mid);
    assert_eq!(arena.pos(), mid);
    arena.pop_to(start);
    assert_eq!((arena.block_count(), arena.pos()), (1, start));
}

#[test]
fn a_push_bigger_than_the_reserve_gets_its_own_block() {
    let arena = small();
    let big = arena.push_slice(&vec![7u8; 200_000]);
    assert_eq!(big.len(), 200_000);
    assert!(big.iter().all(|&b| b == 7));
    assert_eq!(arena.block_count(), 2);
}

#[test]
fn no_chain_reports_full() {
    let arena = Arena::with_params(ArenaParams {
        reserve_size: 8 << 10,
        commit_size: 4 << 10,
        chain: false,
    });
    let mut v: Vec<u8, &Arena> = Vec::new_in(&arena);
    assert!(v.try_reserve_exact(16 << 10).is_err());
    assert!(v.try_reserve_exact(4 << 10).is_ok());
}

#[test]
fn a_vec_at_the_top_grows_in_place() {
    let arena = small();
    let mut v: Vec<u64, &Arena> = Vec::with_capacity_in(4, &arena);
    v.push(0);
    let first = v.as_ptr();
    for i in 1..1000 {
        v.push(i);
    }
    assert_eq!(
        v.as_ptr(),
        first,
        "growing at the top must not move the buffer"
    );
    assert_eq!(v.iter().sum::<u64>(), 999 * 1000 / 2);
}

#[test]
fn a_vec_that_is_not_at_the_top_is_copied_when_it_grows() {
    let arena = small();
    let mut v: Vec<u32, &Arena> = Vec::with_capacity_in(2, &arena);
    v.extend([1, 2]);
    let before = v.as_ptr();
    let _blocker = arena.push(0u8);
    v.push(3);
    assert_ne!(v.as_ptr(), before);
    assert_eq!(v, [1, 2, 3]);
}

#[test]
fn freeing_the_last_allocation_moves_the_top_back() {
    let arena = small();
    let before = arena.pos();
    let b = Box::new_in([1u8; 64], &arena);
    assert!(arena.pos() > before);
    drop(b);
    assert_eq!(arena.pos(), before);

    let layout = Layout::from_size_align(64, 8).unwrap();
    let a = arena.allocate(layout).unwrap();
    let _b = arena.allocate(layout).unwrap();
    let pos = arena.pos();
    unsafe { arena.deallocate(a.cast(), layout) };
    assert_eq!(
        arena.pos(),
        pos,
        "only the most recent allocation can be freed"
    );
}

#[test]
fn arenas_can_move_between_threads() {
    let arena = small();
    let start = arena.pos();
    let arena = std::thread::spawn(move || {
        arena.push_str("from another thread");
        arena
    })
    .join()
    .unwrap();
    assert!(arena.pos() > start);
}

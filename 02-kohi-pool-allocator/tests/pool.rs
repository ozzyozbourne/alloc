#![allow(clippy::undocumented_unsafe_blocks)] // tests: each unsafe call is the thing under test

use std::alloc::{Allocator, Layout};

use kohi_pool::PoolAllocator;

#[test]
fn hands_out_slots_in_order_and_reports_full() {
    let pool = PoolAllocator::for_type::<u64>(3);
    let slots: Vec<_> = (0..3).map(|_| pool.allocate_slot().unwrap()).collect();
    assert_eq!(slots.iter().map(|&(_, i)| i).collect::<Vec<_>>(), [0, 1, 2]);
    assert_eq!(pool.elements_free(), 0);
    assert!(pool.allocate_slot().is_none());
    assert!(pool.allocate(Layout::new::<u64>()).is_err());
}

#[test]
fn free_list_stays_sorted_so_the_lowest_slot_is_reused_first() {
    let pool = PoolAllocator::for_type::<u32>(4);
    let slots: Vec<_> = (0..4).map(|_| pool.allocate_slot().unwrap().0).collect();
    unsafe {
        // Free slot 0, then slot 2. A LIFO free list would hand out 2 next.
        pool.deallocate(slots[0], Layout::new::<u32>());
        pool.deallocate(slots[2], Layout::new::<u32>());
    }
    assert_eq!(pool.allocate_slot().unwrap().1, 0);
    assert_eq!(pool.allocate_slot().unwrap().1, 2);
    assert!(pool.allocate_slot().is_none());
}

#[test]
fn boxes_return_their_slot_when_dropped() {
    let pool = PoolAllocator::for_type::<[f32; 4]>(2);
    let a = Box::new_in([1.0f32; 4], &pool);
    let b = Box::new_in([2.0f32; 4], &pool);
    assert_eq!(pool.elements_free(), 0);
    let a_addr = (&*a as *const [f32; 4]).addr();
    drop(a);
    assert_eq!(pool.elements_free(), 1);
    let c = Box::new_in([3.0f32; 4], &pool);
    assert_eq!((&*c as *const [f32; 4]).addr(), a_addr);
    assert_eq!((*b, *c), ([2.0; 4], [3.0; 4]));
}

#[test]
fn try_box_gives_the_value_back_when_full() {
    let pool = PoolAllocator::for_type::<String>(1);
    let first = pool.try_box(String::from("kept")).unwrap();
    let rejected = pool.try_box(String::from("rejected")).unwrap_err();
    assert_eq!((first.as_str(), rejected.as_str()), ("kept", "rejected"));
}

#[test]
fn rejects_layouts_that_do_not_fit_a_slot() {
    let pool = PoolAllocator::new(Layout::from_size_align(12, 4).unwrap(), 4);
    assert_eq!(pool.slot_size(), 12);
    assert!(
        pool.allocate(Layout::from_size_align(13, 4).unwrap())
            .is_err()
    );
    assert!(
        pool.allocate(Layout::from_size_align(8, 8).unwrap())
            .is_err()
    );
    assert!(
        pool.allocate(Layout::from_size_align(12, 4).unwrap())
            .is_ok()
    );
    assert_eq!(
        pool.elements_free(),
        3,
        "rejected requests must not use a slot"
    );
}

#[test]
fn slots_are_aligned_for_the_element_type() {
    #[repr(align(64))]
    #[allow(dead_code)]
    struct CacheLine([u8; 64]);
    let pool = PoolAllocator::for_type::<CacheLine>(3);
    for _ in 0..3 {
        let (ptr, _) = pool.allocate_slot().unwrap();
        assert_eq!(ptr.addr().get() % 64, 0);
    }
}

#[test]
fn index_of_rejects_foreign_and_interior_pointers() {
    let pool = PoolAllocator::for_type::<u64>(2);
    let (p, i) = pool.allocate_slot().unwrap();
    assert_eq!(pool.index_of(p), Some(i));
    assert_eq!(pool.index_of(unsafe { p.add(1) }), None);
    let other = 5u64;
    assert_eq!(pool.index_of(std::ptr::NonNull::from(&other).cast()), None);
}

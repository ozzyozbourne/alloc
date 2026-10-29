#![allow(clippy::undocumented_unsafe_blocks)] // tests: each unsafe call is the thing under test

use std::alloc::{Allocator, Layout};
use std::mem::MaybeUninit;

use kohi_linear::LinearAllocator;

#[test]
fn aligns_each_allocation_unlike_the_c_version() {
    let a = LinearAllocator::new(256);
    let byte = Box::new_in(7u8, &a);
    let word = Box::new_in(9u64, &a);
    // 1 byte, 7 bytes of padding, then 8 bytes for the u64.
    assert_eq!((&*word as *const u64).addr() % 8, 0);
    assert_eq!(a.allocated(), 16);
    assert_eq!((*byte, *word), (7, 9));
}

#[test]
fn aligns_addresses_even_when_the_buffer_is_misaligned() {
    let mut storage = [MaybeUninit::<u64>::uninit(); 8];
    // Start the buffer one byte past an 8-byte boundary.
    let bytes: &mut [MaybeUninit<u8>] = unsafe {
        std::slice::from_raw_parts_mut(storage.as_mut_ptr().cast::<MaybeUninit<u8>>().add(1), 63)
    };
    let a = LinearAllocator::from_buffer(bytes);
    let word = a.allocate(Layout::new::<u64>()).unwrap();
    assert_eq!(word.cast::<u8>().addr().get() % 8, 0);
    // 7 bytes of padding to reach the boundary, then 8 bytes.
    assert_eq!(a.allocated(), 15);
}

#[test]
fn reports_exhaustion_instead_of_overrunning() {
    let a = LinearAllocator::new(64);
    let mut v: Vec<u8, &LinearAllocator> = Vec::new_in(&a);
    assert!(v.try_reserve_exact(65).is_err());
    assert!(v.try_reserve_exact(64).is_ok());
    assert_eq!(a.remaining(), 0);
}

#[test]
fn free_all_reuses_the_block_and_can_clear_it() {
    let mut a = LinearAllocator::new(64);
    let first = {
        let mut b = Box::new_in([0xAAu8; 32], &a);
        b[0] = 0xBB;
        (&*b as *const [u8; 32]).addr()
    };
    assert_eq!(a.allocated(), 32);

    a.free_all(true);
    assert_eq!(a.allocated(), 0);
    let p = a.allocate(Layout::new::<[u8; 32]>()).unwrap();
    assert_eq!(
        p.cast::<u8>().addr().get(),
        first,
        "reset should reuse the start of the block"
    );
    // `clear = true` zeroed the old 0xAA/0xBB bytes.
    let bytes = unsafe { p.as_ref() };
    assert!(bytes.iter().all(|&b| b == 0));
}

#[test]
fn deallocate_does_not_give_memory_back() {
    let a = LinearAllocator::new(64);
    drop(Box::new_in(1u32, &a));
    drop(Box::new_in(2u32, &a));
    assert_eq!(a.allocated(), 8);
}

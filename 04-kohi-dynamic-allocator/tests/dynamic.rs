#![allow(clippy::undocumented_unsafe_blocks)] // tests: each unsafe call is the thing under test

use std::alloc::{Allocator, Layout};
use std::mem::MaybeUninit;

use kohi_dynamic::{DynamicAllocator, MemoryTag};

#[test]
fn honours_alignment_and_keeps_neighbours_intact() {
    let heap = DynamicAllocator::new(64 * 1024);
    let aligns = [1usize, 2, 8, 16, 64, 256, 4096];
    let blocks: Vec<_> = aligns
        .iter()
        .enumerate()
        .map(|(i, &align)| {
            let layout = Layout::from_size_align(37 + i, align).unwrap();
            let p = heap.allocate(layout).unwrap().cast::<u8>();
            assert_eq!(p.addr().get() % align, 0, "align {align}");
            unsafe { p.write_bytes(i as u8 + 1, layout.size()) };
            (p, layout, i as u8 + 1)
        })
        .collect();
    for &(p, layout, fill) in &blocks {
        let bytes = unsafe { std::slice::from_raw_parts(p.as_ptr(), layout.size()) };
        assert!(
            bytes.iter().all(|&b| b == fill),
            "block filled with {fill} was overwritten"
        );
        assert!(unsafe { heap.check_guards(p, layout) });
    }
    for (p, layout, _) in blocks {
        unsafe { heap.deallocate(p, layout) };
    }
}

/// `deallocate` recomputes each range's size from the `Layout`. If it got a
/// different number than `allocate`, the freelist would leak or double-free.
#[test]
fn freeing_everything_in_any_order_restores_all_free_space() {
    let heap = DynamicAllocator::new(256 * 1024);
    let empty = heap.free_space();
    assert_eq!(empty, 256 * 1024);

    let mut rng = 12345u64;
    let mut next = move || {
        rng = rng
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (rng >> 33) as usize
    };
    let mut live = Vec::new();
    for _ in 0..500 {
        let layout = Layout::from_size_align(1 + next() % 300, 1 << (next() % 7)).unwrap();
        live.push((heap.allocate(layout).unwrap().cast::<u8>(), layout));
        if next() % 3 == 0 {
            let (p, l) = live.swap_remove(next() % live.len());
            unsafe { heap.deallocate(p, l) };
        }
    }
    assert!(heap.free_space() < empty);
    while let Some((p, l)) = live.pop() {
        unsafe { heap.deallocate(p, l) };
    }
    assert_eq!(heap.free_space(), empty);
    assert_eq!(heap.stats().live_allocations, 0);
}

#[test]
fn tracks_bytes_per_tag_and_the_allocation_site() {
    let heap = DynamicAllocator::new(64 * 1024);
    let game = heap.tagged(MemoryTag::Game);
    let line = line!() - 1;
    let mut entities: Vec<u64, _> = Vec::with_capacity_in(10, game);
    entities.extend(0..10);
    let texture = Box::new_in([0u8; 100], heap.tagged(MemoryTag::Texture));

    let stats = heap.stats();
    assert_eq!(stats.bytes(MemoryTag::Game), 80);
    assert_eq!(stats.bytes(MemoryTag::Texture), 100);
    assert_eq!(stats.live_allocations, 2);

    let info = unsafe { heap.info(std::ptr::NonNull::from(&entities[0]).cast()) };
    assert_eq!(info.tag, MemoryTag::Game);
    if cfg!(debug_assertions) {
        let site = info.site.unwrap();
        assert_eq!((site.file(), site.line()), (file!(), line));
    }

    drop(texture);
    assert_eq!(heap.stats().bytes(MemoryTag::Texture), 0);
}

#[test]
fn returns_an_error_when_full() {
    let heap = DynamicAllocator::new(1024);
    let mut v: Vec<u8, &DynamicAllocator> = Vec::new_in(&heap);
    assert!(v.try_reserve_exact(4096).is_err());
    assert!(v.try_reserve_exact(512).is_ok());
}

#[test]
fn runs_inside_a_caller_provided_buffer() {
    let need = DynamicAllocator::memory_requirement(4096);
    // `need` includes worst-case alignment slack, so test with a buffer clearly too small.
    let mut too_small = vec![MaybeUninit::<u8>::uninit(); need - 64];
    assert!(DynamicAllocator::from_buffer(4096, &mut too_small).is_none());

    let mut buffer = vec![MaybeUninit::<u8>::uninit(); need];
    let range = buffer.as_ptr_range();
    let heap = DynamicAllocator::from_buffer(4096, &mut buffer).unwrap();
    let b = Box::new_in(42u32, &heap);
    let addr = (&*b as *const u32).cast::<MaybeUninit<u8>>();
    assert!(range.contains(&addr));
}

#[cfg(debug_assertions)]
#[test]
fn guards_detect_a_write_past_the_end() {
    let heap = DynamicAllocator::new(4096);
    let layout = Layout::array::<u8>(10).unwrap();
    let p = heap.allocate(layout).unwrap().cast::<u8>();
    assert!(unsafe { heap.check_guards(p, layout) });
    // An off-by-one write lands on the guard behind the data.
    unsafe { p.add(10).write(0) };
    assert!(!unsafe { heap.check_guards(p, layout) });
}

#[test]
fn can_be_shared_between_threads() {
    let heap = DynamicAllocator::new(1 << 20);
    let empty = heap.free_space();
    std::thread::scope(|s| {
        for t in 0..4u64 {
            let heap = &heap;
            s.spawn(move || {
                for i in 0..200 {
                    let mut v: Vec<u64, _> = Vec::new_in(heap.tagged(MemoryTag::Scene));
                    v.extend(0..(i % 17) + t);
                    assert_eq!(v.iter().sum::<u64>(), (0..(i % 17) + t).sum());
                }
            });
        }
    });
    assert_eq!(heap.free_space(), empty);
}

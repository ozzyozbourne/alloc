//! Tagged allocations, a memory report, and fragmentation in Kohi's heap.
use std::alloc::{Allocator, Layout};

use kohi_dynamic::{DynamicAllocator, MemoryTag};

#[allow(dead_code)]
struct Entity {
    position: [f32; 3],
    health: u32,
}

fn main() {
    let heap = DynamicAllocator::new(1 << 20);

    let mut entities: Vec<Entity, _> = Vec::with_capacity_in(100, heap.tagged(MemoryTag::Game));
    for i in 0..100 {
        entities.push(Entity {
            position: [i as f32; 3],
            health: 100,
        });
    }
    let texture = Box::new_in([0u8; 64 * 64 * 4], heap.tagged(MemoryTag::Texture));
    let mut draw_list: Vec<u32, _> = Vec::new_in(heap.tagged(MemoryTag::Renderer));
    draw_list.extend(0..1000);

    println!("{}\n", heap.stats());

    // SAFETY: `entities[0]` is a live allocation from `heap`.
    let info = unsafe { heap.info(std::ptr::NonNull::from(&entities[0]).cast()) };
    match info.site {
        Some(site) => println!("entities: tag {}, allocated via {site}", info.tag.name()),
        None => println!(
            "entities: tag {} (sites are recorded in debug builds)",
            info.tag.name()
        ),
    }

    drop(texture);
    drop(draw_list);
    println!(
        "\nafter dropping the texture and draw list:\n{}\n",
        heap.stats()
    );

    // Fragmentation: fill the heap with small blocks, free every other one,
    // then ask for one block bigger than any hole.
    let small = Layout::from_size_align(1000, 8).unwrap();
    let mut blocks = Vec::new();
    while let Ok(b) = heap.allocate(small) {
        blocks.push(b);
    }
    for b in blocks.iter().step_by(2) {
        // SAFETY: each block came from `heap` with `small` and is freed once.
        unsafe { heap.deallocate(b.cast(), small) };
    }
    let big = Layout::from_size_align(4000, 8).unwrap();
    let result = heap.allocate(big);
    println!(
        "filled the heap with {} blocks, freed every other one: {} bytes free, but a 4000-byte request {}",
        blocks.len(),
        heap.free_space(),
        if result.is_ok() {
            "fits"
        } else {
            "fails (no hole is big enough)"
        }
    );
    if let Ok(b) = result {
        // SAFETY: allocated just above with `big`.
        unsafe { heap.deallocate(b.cast(), big) };
    }
    for b in blocks.iter().skip(1).step_by(2) {
        // SAFETY: as above, the other half.
        unsafe { heap.deallocate(b.cast(), small) };
    }
    drop(entities);
    println!(
        "after freeing everything: {} of {} bytes free",
        heap.free_space(),
        heap.total_space()
    );
}

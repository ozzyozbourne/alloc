//! A per-frame allocator: build each frame's data in the linear allocator, then
//! reset it with `free_all` before the next frame.
use std::mem::MaybeUninit;

use kohi_linear::LinearAllocator;

#[derive(Debug)]
#[allow(dead_code)]
struct DrawCommand {
    mesh_id: u32,
    transform: [f32; 16],
}

fn main() {
    let mut frame = LinearAllocator::new(64 * 1024);

    for frame_number in 0..3 {
        {
            // Everything in this block borrows `frame`, so it all has to be
            // dropped before `free_all` below.
            let mut commands: Vec<DrawCommand, &LinearAllocator> = Vec::with_capacity_in(8, &frame);
            for mesh_id in 0..8 {
                commands.push(DrawCommand {
                    mesh_id,
                    transform: [1.0; 16],
                });
            }
            let label = Box::new_in(*b"frame", &frame);
            let tag = Box::new_in(1u8, &frame);
            let after_u8 = Box::new_in(0u64, &frame);

            println!(
                "frame {frame_number}: {} commands, label {:?}, used {} of {} bytes",
                commands.len(),
                std::str::from_utf8(&*label).unwrap(),
                frame.allocated(),
                frame.total_size()
            );
            println!(
                "  u8 at {:p}, next u64 at {:p} (aligned to 8: {})",
                &*tag,
                &*after_u8,
                (&*after_u8 as *const u64).addr().is_multiple_of(8)
            );
        }
        frame.free_all(false);
        println!("  after free_all: used {} bytes", frame.allocated());
    }

    // The same allocator over a stack buffer instead of a heap block.
    let mut stack = [MaybeUninit::<u8>::uninit(); 256];
    let small = LinearAllocator::from_buffer(&mut stack);
    let mut squares: Vec<u16, &LinearAllocator> = Vec::with_capacity_in(10, &small);
    squares.extend((1..=10).map(|x| x * x));
    println!(
        "stack buffer: {squares:?}, used {} of 256 bytes",
        small.allocated()
    );
}

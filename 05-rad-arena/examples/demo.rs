//! RAD-style arena use: long-lived data at the bottom, temporary work in
//! scopes above it, and std collections allocating straight into the arena.
use rad_arena::{Arena, ArenaParams};

struct Symbol<'a> {
    name: &'a str,
    address: u64,
}

fn main() {
    let mut arena = Arena::with_params(ArenaParams {
        reserve_size: 1 << 20,
        commit_size: 64 << 10,
        chain: true,
    });

    // C: `String8 name = push_str8_copy(arena, ...)` etc.
    let module = arena.push_str("raddbg.exe");
    let base = arena.push(0x1_4000_0000u64);
    println!(
        "module {module} at {:#x}, arena pos {:?}",
        *base,
        arena.pos()
    );

    {
        // C: `Temp scratch = temp_begin(arena); ... temp_end(scratch);`
        let scope = arena.temp();
        let mut symbols: Vec<Symbol, &Arena> = Vec::new_in(&scope);
        for i in 0..1000u64 {
            let name = scope.push_str(&format!("func_{i}"));
            symbols.push(Symbol {
                name,
                address: 0x1000 + i * 16,
            });
        }
        let hit = symbols
            .iter()
            .find(|s| s.address == 0x1000 + 500 * 16)
            .unwrap();
        println!("lookup 0x{:x} -> {}", hit.address, hit.name);
        println!(
            "inside the scope: pos {:?}, {} block(s)",
            scope.pos(),
            scope.block_count()
        );
    } // temp_end: everything since the scope began is popped

    println!(
        "after the scope:  pos {:?}, {} block(s)",
        arena.pos(),
        arena.block_count()
    );

    // Chaining: push more than one 1 MiB block holds.
    let start = arena.pos();
    for _ in 0..5 {
        arena.push([0u8; 300 << 10]);
    }
    println!("after 1.5 MiB of pushes: {} block(s)", arena.block_count());
    arena.pop_to(start);
    println!("after pop_to:          {} block(s)", arena.block_count());

    // A Vec at the top of the arena grows in place instead of copying.
    let mut grows = Vec::<u32, &Arena>::with_capacity_in(1, &arena);
    let first = grows.as_ptr();
    grows.extend(0..10_000);
    println!(
        "vec of {} grew without moving: {}",
        grows.len(),
        grows.as_ptr() == first
    );
}

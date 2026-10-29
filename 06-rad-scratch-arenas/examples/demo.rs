//! The RAD pattern: callers pass an output arena, callees use scratch arenas
//! for everything temporary.
use rad_arena::Arena;
use rad_scratch::{arena_count, with_scratch};

/// Parses `key=value;...` into pairs that live in `out`. All intermediate
/// data goes into a scratch arena and disappears when the function returns.
fn parse_config<'a>(out: &'a Arena, text: &str) -> &'a [(&'a str, &'a str)] {
    with_scratch(|scratch| {
        let start = scratch.pos();
        let mut pairs: Vec<(&str, &str), &Arena> = Vec::new_in(scratch);
        for entry in text.split(';').filter(|e| !e.is_empty()) {
            let (k, v) = entry.split_once('=').unwrap_or((entry, ""));
            // Normalize keys in scratch memory first.
            let key = scratch.push_str(k.trim());
            key.make_ascii_lowercase();
            pairs.push((out.push_str(key), out.push_str(v.trim())));
        }
        println!(
            "  parse_config used {} scratch bytes",
            scratch.pos().get() - start.get()
        );
        out.push_slice(&pairs)
    })
}

fn main() {
    let mut out = Arena::new();
    let config = parse_config(&out, "Width=1280; Height=720; VSync=on;");
    for (k, v) in config {
        println!("{k} = {v}");
    }
    println!("scratch arenas on this thread: {}", arena_count());

    // The output arena can itself be a scratch arena: the callee gets the
    // other one automatically, with no `conflicts` list.
    with_scratch(|frame| {
        let config = parse_config(frame, "Theme=dark;");
        println!("parsed into a scratch arena: {:?}", config);
    });
    println!("scratch arenas on this thread: {}", arena_count());
    out.clear();
}

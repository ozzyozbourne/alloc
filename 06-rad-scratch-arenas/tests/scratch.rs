use rad_arena::Arena;
use rad_scratch::{arena_count, with_scratch, with_scratch2};

/// Builds a string in `out`, using scratch memory for the pieces.
fn join<'a>(out: &'a Arena, parts: &[&str]) -> &'a str {
    with_scratch(|scratch| {
        let mut pieces: Vec<&str, &Arena> = Vec::new_in(scratch);
        for p in parts {
            pieces.push(scratch.push_str(&p.to_uppercase()));
        }
        let total: usize = pieces.iter().map(|p| p.len() + 1).sum();
        let mut buf: Vec<u8, &Arena> = Vec::with_capacity_in(total, scratch);
        for p in &pieces {
            buf.extend_from_slice(p.as_bytes());
            buf.push(b'/');
        }
        buf.pop();
        out.push_str(std::str::from_utf8(&buf).unwrap())
    })
}

#[test]
fn scratch_memory_is_popped_when_the_scope_ends() {
    std::thread::spawn(|| {
        let before = with_scratch(|s| s.pos());
        with_scratch(|s| {
            s.push([0u8; 4096]);
        });
        assert_eq!(with_scratch(|s| s.pos()), before);
    })
    .join()
    .unwrap();
}

#[test]
fn output_into_a_scratch_arena_survives_nested_scratch_use() {
    // `join` takes a scratch arena internally while the caller's output arena
    // is itself a scratch arena. It must not pick the same one, or popping it
    // would free the result. C needs `scratch_begin(&out, 1)` for this.
    std::thread::spawn(|| {
        with_scratch(|out| {
            let path = join(out, &["usr", "local", "bin"]);
            let again = join(out, &["etc"]);
            assert_eq!((path, again), ("USR/LOCAL/BIN", "ETC"));
        });
        assert_eq!(arena_count(), 2);
    })
    .join()
    .unwrap();
}

#[test]
fn nested_scopes_get_distinct_arenas_and_grow_the_pool() {
    std::thread::spawn(|| {
        with_scratch(|a| {
            with_scratch2(|b, c| {
                let ptrs = [a as *const Arena, b as *const Arena, c as *const Arena];
                assert!(ptrs[0] != ptrs[1] && ptrs[1] != ptrs[2] && ptrs[0] != ptrs[2]);
            })
        });
        assert_eq!(arena_count(), 3);
        // Arenas are reused once the scopes end.
        with_scratch2(|_, _| {});
        assert_eq!(arena_count(), 3);
    })
    .join()
    .unwrap();
}

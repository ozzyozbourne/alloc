//! A linker-style parallel pass: each task "parses" one object file into its
//! worker's arena, then the main thread merges the results.
use rad_tp_arena::WorkerArenas;

#[derive(Clone, Copy)]
struct Section<'a> {
    name: &'a str,
    size: u32,
}

fn main() {
    let objects = [
        "main.obj",
        "math.obj",
        "io.obj",
        "net.obj",
        "ui.obj",
        "gfx.obj",
        "audio.obj",
    ];
    let mut arenas = WorkerArenas::new(3);

    {
        let mut temp = arenas.temp();
        let workers = temp.workers();
        let parsed: Vec<&[Section]> = temp.for_parallel(objects.len(), |arena, i| {
            let obj = objects[i];
            let sections: Vec<Section> = [".text", ".data", ".rdata"]
                .iter()
                .enumerate()
                .map(|(k, s)| Section {
                    name: arena.push_str(&format!("{obj}{s}")),
                    size: (i as u32 + 1) * 100 + k as u32,
                })
                .collect();
            // Copy the sections into the arena; the temporary Vec is freed here.
            &*arena.push_slice(&sections)
        });

        let total: u32 = parsed.iter().flat_map(|s| s.iter()).map(|s| s.size).sum();
        println!("parsed {} objects on {workers} workers", parsed.len());
        println!(
            "first section: {} ({} bytes)",
            parsed[0][0].name, parsed[0][0].size
        );
        println!(
            "last section:  {} ({} bytes)",
            parsed[6][2].name, parsed[6][2].size
        );
        println!("total section bytes: {total}");
        // `parsed` borrows `temp` mutably until its last use above, so the
        // arenas can only be inspected from here on.
        for w in 0..temp.workers() {
            println!(
                "  worker {w} arena pos: {} bytes",
                temp.arena(w).pos().get()
            );
        }
    }
    for w in 0..arenas.workers() {
        println!(
            "after tp_temp_end, worker {w} arena pos: {} bytes",
            arenas.arena(w).pos().get()
        );
    }
}

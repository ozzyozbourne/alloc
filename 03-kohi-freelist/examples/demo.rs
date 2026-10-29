//! Sub-allocating a GPU-style buffer: the freelist hands out offsets into
//! memory the CPU never dereferences.
use kohi_freelist::FreeList;

fn show(label: &str, list: &FreeList) {
    let ranges: Vec<String> = list
        .free_ranges()
        .map(|(off, size)| format!("[{off}..{})", off + size))
        .collect();
    println!(
        "{label:<28} free {:>5} B: {}",
        list.free_space(),
        ranges.join(" ")
    );
}

fn main() {
    // Pretend this is a 4 KiB vertex buffer on the GPU.
    let mut vertex_buffer = FreeList::new(4096);
    show("new buffer", &vertex_buffer);

    let cube = vertex_buffer.allocate_block(1024).unwrap();
    let sphere = vertex_buffer.allocate_block(2048).unwrap();
    let quad = vertex_buffer.allocate_block(256).unwrap();
    println!("cube at {cube}, sphere at {sphere}, quad at {quad}");
    show("after 3 meshes", &vertex_buffer);

    vertex_buffer.free_block(1024, cube).unwrap();
    show("cube unloaded", &vertex_buffer);

    // First fit: 512 bytes go into the hole the cube left.
    let decal = vertex_buffer.allocate_block(512).unwrap();
    println!("decal at {decal}");
    show("decal loaded", &vertex_buffer);

    match vertex_buffer.free_block(1024, cube) {
        Ok(()) => println!("unexpected: double free accepted"),
        Err(e) => println!("freeing the cube again: {e}"),
    }

    vertex_buffer.free_block(2048, sphere).unwrap();
    vertex_buffer.free_block(512, decal).unwrap();
    vertex_buffer.free_block(256, quad).unwrap();
    show("everything unloaded", &vertex_buffer);

    let (grown, _old_nodes) =
        vertex_buffer.resize(8192, vec![Default::default(); 64].into_boxed_slice());
    show("resized to 8 KiB", &grown);
}

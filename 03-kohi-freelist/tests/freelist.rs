use kohi_freelist::{FreeError, FreeList, Node};

fn ranges(list: &FreeList<impl AsRef<[Node]> + AsMut<[Node]>>) -> Vec<(u64, u64)> {
    list.free_ranges().collect()
}

#[test]
fn first_fit_takes_the_front_of_the_first_big_enough_range() {
    let mut list = FreeList::new(100);
    let a = list.allocate_block(10).unwrap();
    let b = list.allocate_block(20).unwrap();
    let c = list.allocate_block(10).unwrap();
    assert_eq!((a, b, c), (0, 10, 30));
    // Free the 20-byte hole at 10. A 15-byte request fits it, not the tail at 40.
    list.free_block(20, b).unwrap();
    assert_eq!(list.allocate_block(15), Some(10));
    assert_eq!(ranges(&list), [(25, 5), (40, 60)]);
}

#[test]
fn freeing_the_middle_block_merges_three_ranges_into_one() {
    let mut list = FreeList::new(30);
    let a = list.allocate_block(10).unwrap();
    let b = list.allocate_block(10).unwrap();
    let c = list.allocate_block(10).unwrap();
    assert_eq!(list.free_space(), 0);
    list.free_block(10, a).unwrap();
    list.free_block(10, c).unwrap();
    assert_eq!(ranges(&list), [(0, 10), (20, 10)]);
    list.free_block(10, b).unwrap();
    assert_eq!(ranges(&list), [(0, 30)]);
}

#[test]
fn rejects_double_and_overlapping_frees() {
    let mut list = FreeList::new(64);
    let a = list.allocate_block(16).unwrap();
    list.free_block(16, a).unwrap();
    assert_eq!(list.free_block(16, a), Err(FreeError::AlreadyFree));
    let b = list.allocate_block(32).unwrap();
    assert_eq!(b, 0);
    // [16, 32) is still allocated, but [24, 40) overlaps the free tail at 32.
    assert_eq!(list.free_block(16, 24), Err(FreeError::AlreadyFree));
    assert_eq!(list.free_block(8, 56), Err(FreeError::AlreadyFree));
    assert_eq!(list.free_block(8, 64), Err(FreeError::OutOfRange));
    assert_eq!(list.free_block(0, 0), Err(FreeError::ZeroSize));
    // Failed frees must leave the list unchanged.
    assert_eq!(ranges(&list), [(32, 32)]);
}

#[test]
fn reports_running_out_of_nodes() {
    let mut storage = [Node::EMPTY; 2];
    let mut list = FreeList::with_storage(100, &mut storage[..]);
    let offsets: Vec<u64> = (0..10).map(|_| list.allocate_block(10).unwrap()).collect();
    // Free every other block: each one is a separate range and needs a node.
    list.free_block(10, offsets[0]).unwrap();
    list.free_block(10, offsets[2]).unwrap();
    assert_eq!(list.free_block(10, offsets[4]), Err(FreeError::OutOfNodes));
    // Freeing a neighbour needs no new node.
    list.free_block(10, offsets[1]).unwrap();
    assert_eq!(ranges(&list), [(0, 30)]);
}

#[test]
fn resize_extends_the_last_range_or_adds_one() {
    let mut list = FreeList::new(40);
    let a = list.allocate_block(30).unwrap();
    let (mut list, _old) = list.resize(60, vec![Node::EMPTY; 20]);
    // The free tail [30, 40) merges with the new space [40, 60).
    assert_eq!(ranges(&list), [(30, 30)]);

    list.allocate_block(30).unwrap();
    let (mut list, _old) = list.resize(80, vec![Node::EMPTY; 20]);
    assert_eq!(ranges(&list), [(60, 20)]);
    list.free_block(30, a).unwrap();
    assert_eq!(ranges(&list), [(0, 30), (60, 20)]);
}

/// Compares the freelist with a byte map after random operations: the free
/// ranges must be exactly the free bytes, sorted, disjoint and fully merged.
#[test]
fn matches_a_byte_map_under_random_operations() {
    const SIZE: usize = 4096;
    let mut rng = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };

    // Kohi sizes the node array for typical use (21 nodes for 4 KiB). Random
    // frees fragment far more than that, so give this test plenty of nodes.
    let mut list = FreeList::with_storage(SIZE as u64, vec![Node::EMPTY; SIZE].into_boxed_slice());
    let mut used = vec![false; SIZE];
    let mut live: Vec<(u64, u64)> = Vec::new();

    for step in 0..5000 {
        if live.is_empty() || next() % 3 != 0 {
            let size = 1 + next() % 200;
            if let Some(off) = list.allocate_block(size) {
                let range = off as usize..(off + size) as usize;
                assert!(
                    used[range.clone()].iter().all(|&u| !u),
                    "step {step}: handed out used bytes"
                );
                used[range].fill(true);
                live.push((off, size));
            } else {
                // Fails only if no free range is large enough.
                assert!(
                    list.free_ranges().all(|(_, s)| s < size),
                    "step {step}: missed a fit"
                );
            }
        } else {
            let (off, size) = live.swap_remove((next() % live.len() as u64) as usize);
            list.free_block(size, off).unwrap();
            used[off as usize..(off + size) as usize].fill(false);
        }

        let mut expected = Vec::new();
        let mut offset = 0;
        for run in used.chunk_by(|a, b| a == b) {
            if !run[0] {
                expected.push((offset as u64, run.len() as u64));
            }
            offset += run.len();
        }
        assert_eq!(ranges(&list), expected, "step {step}");
    }
}

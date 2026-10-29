//! A port of Kohi's `freelist` (`kohi.core/src/containers/freelist.c`).
//!
//! A freelist tracks which byte ranges of some region are free. It never touches
//! the region itself: it only hands out and takes back *offsets*. That is why
//! Kohi uses it both inside its heap ([`kohi_dynamic`]) and for sub-allocating
//! Vulkan buffers, whose memory the CPU cannot dereference.
//!
//! Because it works on offsets, a freelist is not an `Allocator`. Crate 04 wraps
//! it into one.
//!
//! Like the C version:
//! - Free ranges are kept in a singly linked list sorted by offset.
//! - Allocation is first-fit: it takes the front of the first range big enough.
//! - Freeing merges the range with its neighbours.
//! - List nodes come from a fixed array, so the list itself never allocates.
//!   You can supply that array yourself (C's two-call `memory_requirement`
//!   pattern), which is what lets crate 04 run as a global allocator.
//!
//! Differences from the C version:
//! - Errors are returned as [`FreeError`] instead of being logged. The C version
//!   detects only an exact double free; this one rejects any overlap with a free
//!   range, ranges outside the region, and running out of nodes (where C
//!   dereferences a null node).
//! - Freeing a range that touches the start of the next free range extends that
//!   node instead of taking a new one and merging.
//!
//! [`kohi_dynamic`]: ../kohi_dynamic/index.html

use std::fmt;

/// Marks "no node". C: a null `freelist_node *`.
const NONE: u32 = u32::MAX;

/// One entry of the node array. An unused node has `size == 0`, the same
/// convention the C version uses.
#[derive(Clone, Copy, Debug)]
pub struct Node {
    offset: u64,
    size: u64,
    next: u32,
}

impl Node {
    /// An unused node, for filling caller-provided storage.
    pub const EMPTY: Node = Node {
        offset: 0,
        size: 0,
        next: NONE,
    };
}

impl Default for Node {
    fn default() -> Self {
        Node::EMPTY
    }
}

/// How many nodes the C version reserves for a region of `total_size` bytes:
/// one per `sizeof(void*) * sizeof(freelist_node)` bytes, and at least 20.
///
/// The worst case is one free range per two allocations, so this bounds how
/// fragmented the region can get before [`FreeError::OutOfNodes`].
pub fn node_count_for(total_size: u64) -> usize {
    let per_node = (size_of::<usize>() * size_of::<Node>()) as u64;
    usize::try_from(total_size / per_node)
        .unwrap_or(usize::MAX)
        .max(20)
}

/// Why [`FreeList::free_block`] rejected a range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FreeError {
    /// The size was 0.
    ZeroSize,
    /// The range extends past the end of the region.
    OutOfRange,
    /// Part of the range is already free: a double free or a wrong size.
    AlreadyFree,
    /// Every node is in use, so a new free range cannot be recorded.
    OutOfNodes,
}

impl fmt::Display for FreeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            FreeError::ZeroSize => "freed a zero-sized range",
            FreeError::OutOfRange => "freed a range outside the region",
            FreeError::AlreadyFree => "freed a range that is already (partly) free",
            FreeError::OutOfNodes => "freelist ran out of nodes",
        })
    }
}

impl std::error::Error for FreeError {}

/// Tracks the free ranges of a `total_size`-byte region.
///
/// `S` is the node storage: a `Box<[Node]>` by default, or any `&mut [Node]`
/// when the nodes must live in memory you manage.
pub struct FreeList<S = Box<[Node]>> {
    nodes: S,
    total_size: u64,
    /// First free range, or `NONE` when the region is fully allocated.
    head: u32,
}

impl FreeList {
    /// Creates a freelist for `total_size` bytes with its own node array.
    pub fn new(total_size: u64) -> Self {
        let nodes = vec![Node::EMPTY; node_count_for(total_size)].into_boxed_slice();
        FreeList::with_storage(total_size, nodes)
    }
}

impl<S: AsRef<[Node]> + AsMut<[Node]>> FreeList<S> {
    /// Creates a freelist for `total_size` bytes using `nodes` as node storage.
    /// C: the second `freelist_create` call, with `memory` set.
    ///
    /// # Panics
    /// If `nodes` is empty or has more than `u32::MAX - 1` entries.
    pub fn with_storage(total_size: u64, mut nodes: S) -> Self {
        let slice = nodes.as_mut();
        assert!(!slice.is_empty(), "a freelist needs at least one node");
        assert!(slice.len() < NONE as usize, "too many freelist nodes");
        let mut list = FreeList {
            nodes,
            total_size,
            head: NONE,
        };
        list.clear();
        list
    }

    /// Size of the region in bytes.
    pub fn total_size(&self) -> u64 {
        self.total_size
    }

    /// Total free bytes. C: `freelist_free_space`.
    pub fn free_space(&self) -> u64 {
        self.free_ranges().map(|(_, size)| size).sum()
    }

    /// Free ranges as `(offset, size)`, in offset order.
    pub fn free_ranges(&self) -> impl Iterator<Item = (u64, u64)> + '_ {
        let nodes = self.nodes.as_ref();
        let mut cur = self.head;
        std::iter::from_fn(move || {
            let node = nodes.get(cur as usize)?;
            cur = node.next;
            Some((node.offset, node.size))
        })
    }

    /// Takes `size` bytes from the first free range big enough, and returns the
    /// offset. C: `freelist_allocate_block`.
    pub fn allocate_block(&mut self, size: u64) -> Option<u64> {
        if size == 0 {
            return None;
        }
        let nodes = self.nodes.as_mut();
        let mut prev = NONE;
        let mut cur = self.head;
        while cur != NONE {
            let node = nodes[cur as usize];
            if node.size == size {
                // Exact fit: unlink the node and mark it unused.
                if prev == NONE {
                    self.head = node.next;
                } else {
                    nodes[prev as usize].next = node.next;
                }
                nodes[cur as usize] = Node::EMPTY;
                return Some(node.offset);
            }
            if node.size > size {
                // Larger: take the front of the range.
                let n = &mut nodes[cur as usize];
                n.offset += size;
                n.size -= size;
                return Some(node.offset);
            }
            prev = cur;
            cur = node.next;
        }
        None
    }

    /// Marks `size` bytes at `offset` as free again, merging with neighbouring
    /// free ranges. C: `freelist_free_block`.
    pub fn free_block(&mut self, size: u64, offset: u64) -> Result<(), FreeError> {
        if size == 0 {
            return Err(FreeError::ZeroSize);
        }
        let end = offset.checked_add(size).ok_or(FreeError::OutOfRange)?;
        if end > self.total_size {
            return Err(FreeError::OutOfRange);
        }

        let nodes = self.nodes.as_mut();
        let mut prev = NONE;
        let mut cur = self.head;
        // Find the first free range that starts after `offset`.
        while cur != NONE && nodes[cur as usize].offset < offset {
            prev = cur;
            cur = nodes[cur as usize].next;
        }

        let prev_end =
            (prev != NONE).then(|| nodes[prev as usize].offset + nodes[prev as usize].size);
        let next_start = (cur != NONE).then(|| nodes[cur as usize].offset);
        if prev_end.is_some_and(|e| e > offset) || next_start.is_some_and(|s| s < end) {
            return Err(FreeError::AlreadyFree);
        }

        let joins_prev = prev_end == Some(offset);
        let joins_next = next_start == Some(end);
        match (joins_prev, joins_next) {
            (true, true) => {
                // Fills the gap between two ranges: merge all three into `prev`.
                let next = nodes[cur as usize];
                let p = &mut nodes[prev as usize];
                p.size += size + next.size;
                p.next = next.next;
                nodes[cur as usize] = Node::EMPTY;
            }
            (true, false) => nodes[prev as usize].size += size,
            (false, true) => {
                let n = &mut nodes[cur as usize];
                n.offset = offset;
                n.size += size;
            }
            (false, false) => {
                let new = nodes
                    .iter()
                    .position(|n| n.size == 0)
                    .ok_or(FreeError::OutOfNodes)? as u32;
                nodes[new as usize] = Node {
                    offset,
                    size,
                    next: cur,
                };
                if prev == NONE {
                    self.head = new;
                } else {
                    nodes[prev as usize].next = new;
                }
            }
        }
        Ok(())
    }

    /// Marks the whole region free again. C: `freelist_clear`.
    pub fn clear(&mut self) {
        let total = self.total_size;
        let nodes = self.nodes.as_mut();
        nodes.fill(Node::EMPTY);
        if total > 0 {
            nodes[0] = Node {
                offset: 0,
                size: total,
                next: NONE,
            };
            self.head = 0;
        } else {
            self.head = NONE;
        }
    }

    /// Grows the region to `new_size` bytes, moving the list into `new_nodes`.
    /// Returns the new list and the old storage, so the caller can free it.
    /// C: `freelist_resize`, which hands back the old block in `out_old_memory`.
    ///
    /// # Panics
    /// If `new_size` is smaller than the current size, or `new_nodes` cannot
    /// hold the current free ranges plus one.
    pub fn resize<S2: AsRef<[Node]> + AsMut<[Node]>>(
        self,
        new_size: u64,
        new_nodes: S2,
    ) -> (FreeList<S2>, S) {
        assert!(new_size >= self.total_size, "a freelist can only grow");
        let count = self.free_ranges().count();
        let mut list = FreeList::with_storage(new_size, new_nodes);
        // Rebuild the old free ranges, then free the new tail, which merges
        // with the last range if it ended at the old size.
        let nodes = list.nodes.as_mut();
        assert!(nodes.len() > count, "new freelist storage is too small");
        nodes[0] = Node::EMPTY; // `with_storage` put the whole region here
        for (i, (offset, size)) in self.free_ranges().enumerate() {
            let next = if i + 1 < count { i as u32 + 1 } else { NONE };
            nodes[i] = Node { offset, size, next };
        }
        list.head = if count == 0 { NONE } else { 0 };
        if new_size > self.total_size {
            list.free_block(new_size - self.total_size, self.total_size)
                .expect("the grown tail cannot overlap existing ranges");
        }
        (list, self.nodes)
    }
}

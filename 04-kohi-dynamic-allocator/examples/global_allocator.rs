//! Every allocation in this program, including std's own, goes through Kohi's
//! dynamic allocator, the way every `kallocate` in Kohi does.
use kohi_dynamic::GlobalHeap;

#[global_allocator]
static HEAP: GlobalHeap<{ 8 << 20 }> = GlobalHeap::new();

fn main() {
    let before = HEAP.heap().stats().live_allocations;

    let words: Vec<String> = "the quick brown fox jumps over the lazy dog"
        .split(' ')
        .map(String::from)
        .collect();
    let mut counts = std::collections::HashMap::new();
    for w in &words {
        *counts.entry(w.as_str()).or_insert(0) += 1;
    }
    let handle = std::thread::spawn(|| (0..1000u64).collect::<Vec<_>>().iter().sum::<u64>());
    let sum = handle.join().unwrap();

    let stats = HEAP.heap().stats();
    println!(
        "{} words, 'the' appears {} times, thread sum {sum}",
        words.len(),
        counts["the"]
    );
    println!(
        "live allocations in the global heap: {} before, {} now",
        before, stats.live_allocations
    );
    println!("{stats}");
}

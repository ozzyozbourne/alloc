use std::alloc::{Allocator, Layout};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;

use pony_actors::{Actor, Addr, Context, Iso, Local, Mailbox, Pool, System, Val, pool};

/// Fewer iterations under Miri, which is slow.
const SCALE: usize = if cfg!(miri) { 1 } else { 20 };

// ----- Pool -----

#[test]
fn pool_reuses_blocks_on_the_owning_thread() {
    let l = Layout::new::<[u64; 4]>();
    let a = Pool.allocate(l).unwrap().cast::<u8>();
    // SAFETY: allocated just above with `l`.
    unsafe { Pool.deallocate(a, l) };
    let b = Pool.allocate(l).unwrap().cast::<u8>();
    assert_eq!(a, b, "a freed block is the next one handed out");
    // SAFETY: as above.
    unsafe { Pool.deallocate(b, l) };
}

#[test]
fn pool_blocks_are_aligned_to_their_class() {
    for align in [8, 64, 512, 4096] {
        let l = Layout::from_size_align(24, align).unwrap();
        let p = Pool.allocate(l).unwrap().cast::<u8>();
        assert_eq!(p.addr().get() % align, 0);
        // SAFETY: allocated just above with `l`.
        unsafe { Pool.deallocate(p, l) };
    }
}

#[test]
fn foreign_frees_return_to_the_owner_in_batches() {
    // A class no other test uses, so the free list starts empty.
    let l = Layout::from_size_align(2048, 8).unwrap();
    let blocks: Vec<usize> = (0..pool::BATCH)
        .map(|_| {
            Pool.allocate(l)
                .unwrap()
                .cast::<u8>()
                .as_ptr()
                .expose_provenance()
        })
        .collect();
    let before = pool::stats().batches_sent;
    let to_free = blocks.clone();
    std::thread::spawn(move || {
        for p in to_free {
            let p = std::ptr::NonNull::new(std::ptr::with_exposed_provenance_mut::<u8>(p)).unwrap();
            // SAFETY: allocated above with `l`; the owner no longer uses it.
            unsafe { Pool.deallocate(p, l) };
        }
        // `BATCH` frees fill exactly one batch.
    })
    .join()
    .unwrap();
    assert!(pool::stats().batches_sent > before);
    // The owner gets them back from its inbox before carving new memory.
    let again: Vec<usize> = (0..pool::BATCH)
        .map(|_| Pool.allocate(l).unwrap().cast::<u8>().as_ptr().addr())
        .collect();
    let mut a = again.clone();
    let mut b = blocks;
    a.sort();
    b.sort();
    assert_eq!(a, b, "every block freed elsewhere came back to this thread");
    for p in again {
        let p = std::ptr::NonNull::new(std::ptr::with_exposed_provenance_mut::<u8>(p)).unwrap();
        // SAFETY: allocated above with `l`.
        unsafe { Pool.deallocate(p, l) };
    }
}

#[test]
fn pool_backs_std_collections() {
    let mut v: Vec<u32, Pool> = Vec::new_in(Pool);
    for i in 0..1000 * SCALE as u32 {
        v.push(i); // grows through every class and past MAX_CLASS
    }
    assert_eq!(v.iter().map(|&x| x as u64).sum::<u64>(), {
        let n = 1000 * SCALE as u64;
        n * (n - 1) / 2
    });
}

// ----- Heap, Iso, Val -----

#[test]
fn local_heap_reuses_and_counts() {
    let heap = Local::new();
    let a = Box::new_in(1u64, heap.clone());
    let p = &*a as *const u64;
    assert_eq!(
        heap.stats().live_bytes,
        32,
        "8 bytes round up to the 32 B class"
    );
    drop(a);
    assert_eq!(heap.stats().live_bytes, 0);
    let b = Box::new_in(2u64, heap.clone());
    assert_eq!(&*b as *const u64, p);
    let big: Vec<u8, Local> = Vec::with_capacity_in(10_000, heap.clone());
    assert_eq!(heap.stats().live_bytes, 32 + 10_000);
    drop(big);
    assert_eq!(heap.stats().chunks, 1);
}

#[test]
fn iso_sent_to_another_thread_is_freed_back_to_its_heap() {
    let heap = Local::new();
    let iso = Iso::new_in([7u8; 100], &heap);
    let addr = iso.as_ptr().addr();
    let seen = std::thread::spawn(move || {
        let seen = iso.as_ptr().addr();
        drop(iso); // a remote free
        seen
    })
    .join()
    .unwrap();
    assert_eq!(seen, addr, "sent by reference, not copied");
    assert_eq!(heap.stats().live_bytes, 0);
    assert_eq!(heap.stats().remote_frees, 1);
    assert_eq!(heap.collect(), 1);
    // Same class, so the block comes straight back.
    let again = Iso::new_in([0u8; 100], &heap);
    assert_eq!(again.as_ptr().addr(), addr);
}

#[test]
fn iso_into_inner_and_from_box() {
    let heap = Local::new();
    let b = Box::new_in(String::from("hello"), heap.clone());
    let iso = Iso::from_box(b);
    assert!(heap.same_heap(iso.heap()));
    let s = std::thread::spawn(move || iso.into_inner()).join().unwrap();
    assert_eq!(s, "hello");
    assert_eq!(heap.collect(), 1);
}

#[test]
fn val_is_shared_and_freed_once() {
    struct Counted(Arc<AtomicUsize>);
    impl Drop for Counted {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    let drops = Arc::new(AtomicUsize::new(0));
    let heap = Local::new();
    let v = Val::new_in(Counted(drops.clone()), &heap);
    let threads: Vec<_> = (0..4)
        .map(|_| {
            let v = v.clone();
            std::thread::spawn(move || {
                let w = v.clone();
                assert!(Val::ptr_eq(&v, &w));
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    assert_eq!(Val::count(&v), 1);
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(v);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert_eq!(heap.stats().live_bytes, 0);
}

#[test]
fn heap_outlives_its_owner_while_objects_are_out() {
    let heap = Local::new();
    let iso = Iso::new_in(vec![1, 2, 3], &heap);
    drop(heap); // the actor is gone
    assert_eq!(*iso, [1, 2, 3]);
    drop(iso); // frees the block, then the heap itself
}

// ----- Mailbox -----

#[test]
fn mailbox_reports_empty_only_to_the_first_sender() {
    let q = Mailbox::new();
    assert!(q.push(1), "a new mailbox is marked empty");
    assert!(!q.push(2), "already scheduled");
    // SAFETY: this thread is the only consumer.
    unsafe {
        assert_eq!(q.pop(), Some(1));
        assert!(!q.mark_empty(), "2 is still queued");
        assert_eq!(q.pop(), Some(2));
        assert_eq!(q.pop(), None);
        assert!(q.mark_empty());
    }
    assert!(q.push(3), "empty again, so the sender schedules");
}

#[test]
fn mailbox_keeps_each_senders_order() {
    let q = Arc::new(Mailbox::new());
    let per = 200 * SCALE;
    let senders: Vec<_> = (0..4)
        .map(|s| {
            let q = q.clone();
            std::thread::spawn(move || {
                for i in 0..per {
                    q.push((s, i));
                }
            })
        })
        .collect();
    let mut next = [0; 4];
    let mut got = 0;
    while got < 4 * per {
        // SAFETY: the main thread is the only consumer.
        if let Some((s, i)) = unsafe { q.pop() } {
            assert_eq!(i, next[s], "sender {s} out of order");
            next[s] += 1;
            got += 1;
        } else {
            std::thread::yield_now();
        }
    }
    for t in senders {
        t.join().unwrap();
    }
}

#[test]
fn mailbox_drops_unread_messages() {
    let rc = Arc::new(());
    let q = Mailbox::new();
    for _ in 0..3 {
        q.push(rc.clone());
    }
    // SAFETY: only consumer.
    drop(unsafe { q.pop() });
    assert_eq!(Arc::strong_count(&rc), 3);
    drop(q);
    assert_eq!(Arc::strong_count(&rc), 1);
}

// ----- Actors -----

/// Counts to `n` by sending itself messages, then reports.
struct Counter {
    left: usize,
    done: mpsc::Sender<usize>,
}

impl Actor for Counter {
    type Msg = usize;
    fn receive(&mut self, cx: &Context<'_, usize>, i: usize) {
        if self.left == 0 {
            self.done.send(i).unwrap();
        } else {
            self.left -= 1;
            cx.me().send(i + 1);
        }
    }
}

#[test]
fn an_actor_runs_more_than_one_batch() {
    let system = System::new(2);
    let (tx, rx) = mpsc::channel();
    let n = 25 * SCALE;
    let a = system.spawn(move |_| Counter { left: n, done: tx });
    a.send(0);
    assert_eq!(rx.recv().unwrap(), n);
    let stats = system.shutdown();
    let messages: usize = stats.iter().map(|s| s.messages).sum();
    assert_eq!(messages, n + 1);
}

/// Fills a buffer allocated in its own heap and sends it to the next actor.
struct Stage {
    next: Option<Addr<Iso<Vec<u32>>>>,
    seen: mpsc::Sender<(usize, u32)>,
}

impl Actor for Stage {
    type Msg = Iso<Vec<u32>>;
    fn receive(&mut self, _cx: &Context<'_, Self::Msg>, mut buf: Self::Msg) {
        let len = buf.len() as u32;
        buf.push(len);
        match &self.next {
            Some(next) => next.send(buf), // zero-copy hand-off
            None => {
                self.seen
                    .send((buf.as_ptr().addr(), buf.iter().sum()))
                    .unwrap();
                // Dropping `buf` frees it back to the first stage's heap.
            }
        }
    }
}

/// Allocates a fresh `Iso` in its own heap per request.
struct Source {
    first: Addr<Iso<Vec<u32>>>,
    sent: mpsc::Sender<usize>,
}

impl Actor for Source {
    type Msg = ();
    fn receive(&mut self, cx: &Context<'_, ()>, (): ()) {
        let buf = Iso::new_in(vec![0u32], cx.heap());
        self.sent.send(buf.as_ptr().addr()).unwrap();
        self.first.send(buf);
    }
}

#[test]
fn iso_messages_cross_actors_without_copying() {
    let system = System::new(3);
    let (seen_tx, seen_rx) = mpsc::channel();
    let last = system.spawn({
        let seen = seen_tx.clone();
        move |_| Stage { next: None, seen }
    });
    let middle = system.spawn(move |_| Stage {
        next: Some(last),
        seen: seen_tx,
    });
    let (sent_tx, sent_rx) = mpsc::channel();
    let source = system.spawn(move |_| Source {
        first: middle,
        sent: sent_tx,
    });
    let rounds = 5 * SCALE;
    for _ in 0..rounds {
        source.send(());
    }
    for _ in 0..rounds {
        let sent = sent_rx.recv().unwrap();
        let (seen, sum) = seen_rx.recv().unwrap();
        assert_eq!(
            seen, sent,
            "the Iso reached the last stage at the same address"
        );
        assert_eq!(sum, 1 + 2);
    }
    let stats = system.shutdown();
    assert!(stats.iter().map(|s| s.messages).sum::<usize>() >= 3 * rounds);
}

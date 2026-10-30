//! An actor's mailbox. C: `actor/messageq.c`.
//!
//! An intrusive, lock-free queue with many senders and one receiver (Dmitry
//! Vyukov's design). Each message is a node allocated from the [`Pool`],
//! with its link inside it. The queue always holds one already-consumed
//! node, the stub, so push and pop never touch the same node's link.
//!
//! Pony's trick is bit 0 of `head`: it is set when the queue is empty *and*
//! the actor is not scheduled. A push that finds it set clears it (the
//! exchange stores an untagged pointer) and must schedule the actor; every
//! other push leaves scheduling alone. The actor sets the bit again with
//! [`Mailbox::mark_empty`] when it has nothing left to do.
use std::cell::UnsafeCell;
use std::mem::MaybeUninit;
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering, fence};

use crate::pool::Pool;

/// C: `pony_msg_t`, with the arguments inline after the header.
struct Node<M> {
    next: AtomicPtr<Node<M>>,
    msg: MaybeUninit<M>,
}

fn alloc_node<M>(msg: MaybeUninit<M>) -> *mut Node<M> {
    let node = Box::new_in(
        Node {
            next: AtomicPtr::new(ptr::null_mut()),
            msg,
        },
        Pool,
    );
    Box::into_raw_with_allocator(node).0
}

/// # Safety
/// `node` came from `alloc_node`, and its message was moved out or never set.
unsafe fn free_node<M>(node: *mut Node<M>) {
    // SAFETY: per the contract; `MaybeUninit` drops nothing.
    drop(unsafe { Box::from_raw_in(node, Pool) });
}

/// C: `messageq_t`.
pub struct Mailbox<M> {
    /// Last node pushed, with bit 0 set when empty and unscheduled.
    head: AtomicPtr<Node<M>>,
    /// The stub: the node the consumer read last. Consumer only.
    tail: UnsafeCell<*mut Node<M>>,
}

// SAFETY: messages move from producers to the consumer, so `M: Send` is
// needed and enough; `tail` is only touched by the single consumer (the
// `unsafe` methods' contract) and by `Drop`.
unsafe impl<M: Send> Send for Mailbox<M> {}
// SAFETY: as above; producers only use `head`, atomically.
unsafe impl<M: Send> Sync for Mailbox<M> {}

impl<M> Default for Mailbox<M> {
    fn default() -> Self {
        Self::new()
    }
}

impl<M> Mailbox<M> {
    /// An empty mailbox, marked empty. C: `ponyint_messageq_init`.
    pub fn new() -> Self {
        let stub = alloc_node::<M>(MaybeUninit::uninit());
        Mailbox {
            head: AtomicPtr::new(stub.map_addr(|a| a | 1)),
            tail: UnsafeCell::new(stub),
        }
    }

    /// Adds a message. Returns true if the mailbox was marked empty, which
    /// means the caller must schedule the actor. C: `messageq_push`.
    pub fn push(&self, msg: M) -> bool {
        let node = alloc_node(MaybeUninit::new(msg));
        // Release publishes the node; acquire pairs with `mark_empty`.
        let prev = self.head.swap(node, Ordering::AcqRel);
        let was_empty = prev.addr() & 1 != 0;
        let prev = prev.map_addr(|a| a & !1);
        // SAFETY: `prev` stays allocated until the consumer moves past it,
        // which needs the link we are about to store.
        unsafe { (*prev).next.store(node, Ordering::Release) };
        was_empty
    }

    /// Takes the oldest message, freeing the previous stub.
    /// C: `ponyint_actor_messageq_pop`.
    ///
    /// # Safety
    /// Only one thread may act as the consumer (`pop`, `mark_empty`) at a time.
    pub unsafe fn pop(&self) -> Option<M> {
        // SAFETY: we are the only consumer.
        let tail = unsafe { *self.tail.get() };
        // SAFETY: the stub is ours and allocated.
        let next = unsafe { (*tail).next.load(Ordering::Acquire) };
        if next.is_null() {
            return None;
        }
        fence(Ordering::Acquire);
        // SAFETY: `next` is fully published (acquire above); it becomes the
        // new stub after we move its message out, and `tail` is no longer
        // reachable by producers.
        unsafe {
            *self.tail.get() = next;
            let msg = (*next).msg.assume_init_read();
            free_node(tail);
            Some(msg)
        }
    }

    /// Marks the mailbox empty if it is. Returns false if messages arrived,
    /// in which case the actor must stay scheduled.
    /// C: `ponyint_messageq_markempty`.
    ///
    /// # Safety
    /// As for [`pop`](Self::pop).
    pub unsafe fn mark_empty(&self) -> bool {
        // SAFETY: we are the only consumer.
        let tail = unsafe { *self.tail.get() };
        let head = self.head.load(Ordering::Acquire);
        if head.addr() & 1 != 0 {
            return true;
        }
        if head != tail {
            return false;
        }
        self.head
            .compare_exchange(
                tail,
                tail.map_addr(|a| a | 1),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }
}

impl<M> Drop for Mailbox<M> {
    /// Frees the stub and drops any messages nobody read.
    /// C: `ponyint_messageq_destroy`.
    fn drop(&mut self) {
        let mut node = *self.tail.get_mut();
        loop {
            // SAFETY: `&mut self`, so no producer or consumer is left; every
            // node from the stub on is ours, and only nodes after the stub
            // still hold a message.
            unsafe {
                let next = (*node).next.load(Ordering::Acquire);
                free_node(node);
                if next.is_null() {
                    return;
                }
                drop((*next).msg.assume_init_read());
                node = next;
            }
        }
    }
}

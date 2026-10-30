//! Actors, addresses and a small scheduler. C: `actor/actor.c` (`pony_create`,
//! `pony_sendv`, `ponyint_actor_run`) and, much reduced, `sched/scheduler.c`.
//!
//! Each actor has a state, a [`Local`] heap and a [`Mailbox`]. Sending pushes
//! a pool-allocated message; if the mailbox was marked empty, the sender
//! schedules the actor on its worker. A worker runs up to [`BATCH`] messages,
//! then collects the actor's remotely freed blocks (Pony would GC here), then
//! marks the mailbox empty or reschedules the actor.
//!
//! Differences from Pony, on purpose:
//! - Actors are pinned to the worker that created them; Pony's schedulers
//!   steal actors from each other. Pinning keeps [`Local`] on one thread, so
//!   the heap needs no `unsafe impl Send`.
//! - There is no actor GC or cycle detector. A [`System`] runs until it is
//!   shut down, and actors are dropped then.
//! - No backpressure (muting) and no ORCA reference counting: Rust ownership
//!   decides when sent objects are freed (see [`Iso`](crate::Iso)).
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::JoinHandle;

use crate::heap::Local;
use crate::pool;
use crate::queue::Mailbox;

/// Messages an actor handles per turn before yielding. C: `PONY_SCHED_BATCH`.
pub const BATCH: usize = 100;

/// An actor: state plus a behaviour per message.
pub trait Actor: 'static {
    /// The messages it accepts. Pony: its behaviours and their arguments.
    type Msg: Send + 'static;

    /// Handles one message. Pony: runs one behaviour to completion.
    fn receive(&mut self, cx: &Context<'_, Self::Msg>, msg: Self::Msg);
}

/// What a behaviour can reach besides the actor's state.
pub struct Context<'a, M> {
    heap: &'a Local,
    me: &'a Addr<M>,
}

impl<M: Send> Context<'_, M> {
    /// This actor's heap: allocate with `Box::new_in(x, cx.heap().clone())`,
    /// [`Iso::new_in`](crate::Iso::new_in) or [`Val::new_in`](crate::Val::new_in).
    pub fn heap(&self) -> &Local {
        self.heap
    }

    /// This actor's own address. Pony: `this` as a `tag`.
    pub fn me(&self) -> &Addr<M> {
        self.me
    }
}

/// The address of an actor: it can send messages and be compared, nothing
/// else. Pony: a `tag` reference to an actor.
pub struct Addr<M> {
    mailbox: Arc<Mailbox<M>>,
    worker: Sender<Cmd>,
    id: usize,
}

impl<M> Clone for Addr<M> {
    fn clone(&self) -> Self {
        Addr {
            mailbox: self.mailbox.clone(),
            worker: self.worker.clone(),
            id: self.id,
        }
    }
}

impl<M> PartialEq for Addr<M> {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.mailbox, &other.mailbox)
    }
}

impl<M> Eq for Addr<M> {}

impl<M: Send> Addr<M> {
    /// Queues `msg`, and schedules the actor if it was idle. Never blocks.
    /// C: `pony_sendv`.
    pub fn send(&self, msg: M) {
        if self.mailbox.push(msg) {
            // The worker is gone only after shutdown; the message is then
            // dropped with the mailbox.
            let _ = self.worker.send(Cmd::Run(self.id));
        }
    }
}

type Spawn = Box<dyn FnOnce() -> Box<dyn Runnable> + Send>;

enum Cmd {
    Spawn(usize, Spawn),
    Run(usize),
    Stop,
}

trait Runnable {
    /// Runs one turn; returns true if the actor must be scheduled again.
    fn run(&mut self, stats: &mut WorkerStats) -> bool;
}

/// C: `pony_actor_t`: the state, its heap and its queue together.
struct ActorCell<A: Actor> {
    actor: A,
    heap: Local,
    me: Addr<A::Msg>,
}

impl<A: Actor> Runnable for ActorCell<A> {
    /// C: `ponyint_actor_run`.
    fn run(&mut self, stats: &mut WorkerStats) -> bool {
        let cx = Context {
            heap: &self.heap,
            me: &self.me,
        };
        let mut n = 0;
        while n < BATCH {
            // SAFETY: this cell is its mailbox's only consumer, and a worker
            // runs one cell at a time.
            let Some(msg) = (unsafe { self.me.mailbox.pop() }) else {
                break;
            };
            self.actor.receive(&cx, msg);
            n += 1;
        }
        stats.messages += n;
        stats.turns += 1;
        stats.collected += self.heap.collect();
        if n == BATCH {
            return true;
        }
        // SAFETY: as above.
        !unsafe { self.me.mailbox.mark_empty() }
    }
}

/// What one worker thread did.
#[derive(Clone, Copy, Debug, Default)]
pub struct WorkerStats {
    /// Messages handled.
    pub messages: usize,
    /// Times an actor was run.
    pub turns: usize,
    /// Remotely freed blocks returned to their heaps by `collect`.
    pub collected: usize,
}

fn worker(rx: Receiver<Cmd>, me: Sender<Cmd>) -> WorkerStats {
    let mut actors: HashMap<usize, Box<dyn Runnable>> = HashMap::new();
    let mut stats = WorkerStats::default();
    loop {
        let cmd = match rx.try_recv() {
            Ok(cmd) => cmd,
            Err(TryRecvError::Empty) => {
                // About to sleep: hand pending foreign frees to their owners.
                pool::flush();
                match rx.recv() {
                    Ok(cmd) => cmd,
                    Err(_) => break,
                }
            }
            Err(TryRecvError::Disconnected) => break,
        };
        match cmd {
            Cmd::Spawn(id, make) => {
                actors.insert(id, make());
            }
            Cmd::Run(id) => {
                if let Some(actor) = actors.get_mut(&id)
                    && actor.run(&mut stats)
                {
                    let _ = me.send(Cmd::Run(id));
                }
            }
            Cmd::Stop => break,
        }
    }
    drop(actors);
    pool::flush();
    stats
}

/// A fixed set of worker threads that run actors.
pub struct System {
    workers: Vec<(Sender<Cmd>, JoinHandle<WorkerStats>)>,
    next_id: AtomicUsize,
}

impl System {
    /// Starts `threads` workers. Pony starts one scheduler per core.
    pub fn new(threads: usize) -> Self {
        assert!(threads > 0, "a system needs at least one worker");
        let workers = (0..threads)
            .map(|i| {
                let (tx, rx) = mpsc::channel();
                let me = tx.clone();
                let handle = std::thread::Builder::new()
                    .name(format!("pony-worker-{i}"))
                    .spawn(move || worker(rx, me))
                    .expect("failed to start a worker thread");
                (tx, handle)
            })
            .collect();
        System {
            workers,
            next_id: AtomicUsize::new(0),
        }
    }

    /// Creates an actor on the next worker, round robin. `init` runs on that
    /// worker, with the new actor's heap, so the state can allocate in it.
    /// C: `pony_create`.
    pub fn spawn<A, F>(&self, init: F) -> Addr<A::Msg>
    where
        A: Actor,
        F: FnOnce(&Local) -> A + Send + 'static,
    {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let worker = self.workers[id % self.workers.len()].0.clone();
        let addr = Addr {
            mailbox: Arc::new(Mailbox::new()),
            worker: worker.clone(),
            id,
        };
        let me = addr.clone();
        let make: Spawn = Box::new(move || {
            let heap = Local::new();
            let actor = init(&heap);
            Box::new(ActorCell { actor, heap, me })
        });
        // Sent before `addr` is returned, so it reaches the worker before any
        // `Run` for this actor.
        worker
            .send(Cmd::Spawn(id, make))
            .expect("worker thread is gone");
        addr
    }

    /// Stops every worker after the commands already queued for it, drops
    /// the actors, and returns each worker's stats. Messages still queued are
    /// dropped, so wait for the replies you need first.
    pub fn shutdown(mut self) -> Vec<WorkerStats> {
        self.stop()
    }

    fn stop(&mut self) -> Vec<WorkerStats> {
        for (tx, _) in &self.workers {
            let _ = tx.send(Cmd::Stop);
        }
        self.workers
            .drain(..)
            .map(|(_, h)| h.join().expect("a worker panicked"))
            .collect()
    }
}

impl Drop for System {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            self.stop();
        }
    }
}

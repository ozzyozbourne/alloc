//! A port of the RAD linker's `TP_Arena` (`src/linker/thread_pool/thread_pool.c`).
//!
//! An arena is not thread-safe: pushing moves its position without a lock. So
//! a parallel job gives each worker thread its own arena. Tasks push their
//! results into their worker's arena, and the main thread reads all results
//! after the job. In C:
//!
//! ```c
//! TP_Arena *arenas = tp_arena_alloc(pool);    // one Arena per worker
//! TP_Temp temp = tp_temp_begin(arenas);       // save every arena's position
//! tp_for_parallel(pool, arenas, task_count, task_func, data);
//! ... read results ...
//! tp_temp_end(temp);                          // pop every arena back
//! ```
//!
//! In Rust, [`WorkerArenas::for_parallel`] hands each worker thread `&mut` its
//! own arena, so two threads can never push to the same arena. The compiler
//! enforces the rest:
//! - Task results may borrow the worker arenas (for example `&[T]` from
//!   `push_slice`), and those borrows keep the arenas alive and un-popped.
//! - `Arena` is `Send` but not `Sync`, so a task cannot smuggle `&Arena` out to
//!   another thread.
//!
//! ```compile_fail,E0505
//! use rad_tp_arena::WorkerArenas;
//! let mut arenas = WorkerArenas::new(2);
//! let mut temp = arenas.temp();
//! let words = temp.for_parallel(4, |arena, i| arena.push_str(&i.to_string()) as &str);
//! drop(temp); // error: `temp` is still borrowed by `words`
//! println!("{words:?}");
//! ```
//!
//! Difference from the C version: RAD keeps a persistent pool of worker
//! threads synchronised with barriers. This port starts scoped threads for each
//! job, which is simpler but costs a thread spawn per worker per job.

use std::ops::{Deref, DerefMut};

use rad_arena::{Arena, Pos};

/// One arena per worker. C: `TP_Arena { U64 count; Arena **v; }`.
pub struct WorkerArenas {
    arenas: Vec<Arena>,
}

impl WorkerArenas {
    /// Creates `workers` arenas. C: `tp_arena_alloc(pool)`.
    ///
    /// # Panics
    /// If `workers` is 0.
    pub fn new(workers: usize) -> Self {
        assert!(workers > 0, "need at least one worker");
        WorkerArenas {
            arenas: (0..workers).map(|_| Arena::new()).collect(),
        }
    }

    /// Number of workers.
    pub fn workers(&self) -> usize {
        self.arenas.len()
    }

    /// The arena of worker `index`.
    pub fn arena(&self, index: usize) -> &Arena {
        &self.arenas[index]
    }

    /// Saves every arena's position; dropping the guard pops them all back.
    /// C: `tp_temp_begin` and `tp_temp_end`.
    pub fn temp(&mut self) -> WorkerTemp<'_> {
        let positions = self.arenas.iter().map(Arena::pos).collect();
        WorkerTemp {
            arenas: self,
            positions,
        }
    }

    /// Runs `task(arena, task_index)` for every index in `0..task_count`,
    /// spread over the workers, and returns the results in task order.
    /// Task `i` runs on worker `i % workers` and gets that worker's arena.
    /// C: `tp_for_parallel(pool, arenas, task_count, task_func, data)`.
    pub fn for_parallel<'a, R, F>(&'a mut self, task_count: usize, task: F) -> Vec<R>
    where
        F: Fn(&'a Arena, usize) -> R + Sync,
        R: Send,
    {
        let workers = self.arenas.len();
        let task = &task;
        let per_worker: Vec<Vec<R>> = std::thread::scope(|s| {
            let handles: Vec<_> = self
                .arenas
                .iter_mut()
                .enumerate()
                .map(|(w, arena)| {
                    // Each thread gets exclusive access to one arena.
                    s.spawn(move || {
                        let arena: &'a Arena = arena;
                        (w..task_count)
                            .step_by(workers)
                            .map(|i| task(arena, i))
                            .collect()
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().expect("a task panicked"))
                .collect()
        });

        // Task `i` was the `i / workers`-th result of worker `i % workers`.
        let mut per_worker: Vec<_> = per_worker.into_iter().map(Vec::into_iter).collect();
        (0..task_count)
            .map(|i| per_worker[i % workers].next().expect("every task ran"))
            .collect()
    }
}

/// Positions of every worker arena, restored on drop. C: `TP_Temp`.
pub struct WorkerTemp<'a> {
    arenas: &'a mut WorkerArenas,
    positions: Vec<Pos>,
}

impl Deref for WorkerTemp<'_> {
    type Target = WorkerArenas;
    fn deref(&self) -> &WorkerArenas {
        self.arenas
    }
}

impl DerefMut for WorkerTemp<'_> {
    fn deref_mut(&mut self) -> &mut WorkerArenas {
        self.arenas
    }
}

impl Drop for WorkerTemp<'_> {
    fn drop(&mut self) {
        for (arena, &pos) in self.arenas.arenas.iter_mut().zip(&self.positions) {
            arena.pop_to(pos);
        }
    }
}

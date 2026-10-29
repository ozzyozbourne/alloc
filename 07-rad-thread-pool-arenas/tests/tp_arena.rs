use rad_tp_arena::WorkerArenas;

#[test]
fn results_come_back_in_task_order_and_live_in_worker_arenas() {
    let mut arenas = WorkerArenas::new(3);
    let results = arenas.for_parallel(10, |arena, i| {
        let squares: Vec<u64> = (0..=i as u64).map(|x| x * x).collect();
        &*arena.push_slice(&squares)
    });
    assert_eq!(results.len(), 10);
    for (i, r) in results.iter().enumerate() {
        assert_eq!(r.len(), i + 1);
        assert_eq!(*r.last().unwrap(), (i * i) as u64);
    }
}

#[test]
fn task_i_runs_on_worker_i_mod_n() {
    let mut arenas = WorkerArenas::new(4);
    let addrs: Vec<usize> =
        arenas.for_parallel(8, |arena, _| (arena as *const _ as *const u8).addr());
    for i in 0..8 {
        assert_eq!(addrs[i], addrs[i % 4]);
    }
    let mut distinct = addrs[..4].to_vec();
    distinct.sort();
    distinct.dedup();
    assert_eq!(distinct.len(), 4);
}

#[test]
fn temp_pops_every_worker_arena() {
    let mut arenas = WorkerArenas::new(3);
    let before: Vec<_> = (0..3).map(|w| arenas.arena(w).pos()).collect();
    {
        let mut temp = arenas.temp();
        let total: usize = temp
            .for_parallel(9, |arena, i| {
                arena.push_slice(&vec![1u8; 1000 * (i + 1)]).len()
            })
            .into_iter()
            .sum();
        assert_eq!(total, 45_000);
        assert!((0..3).all(|w| temp.arena(w).pos() > before[w]));
    }
    let after: Vec<_> = (0..3).map(|w| arenas.arena(w).pos()).collect();
    assert_eq!(after, before);
}

#[test]
fn more_workers_than_tasks_is_fine() {
    let mut arenas = WorkerArenas::new(8);
    let r = arenas.for_parallel(2, |_, i| i * 10);
    assert_eq!(r, [0, 10]);
    assert!(arenas.for_parallel(0, |_, i| i).is_empty());
}

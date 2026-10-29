//! A particle system that keeps particles in a fixed-size pool.
use kohi_pool::PoolAllocator;

#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
struct Particle {
    position: [f32; 3],
    velocity: [f32; 3],
    life: f32,
}

fn spawn(id: u32) -> Particle {
    Particle {
        position: [id as f32, 0.0, 0.0],
        velocity: [0.0, 1.0, 0.0],
        life: 1.0,
    }
}

fn main() {
    let pool = PoolAllocator::for_type::<Particle>(4);
    println!(
        "pool: {} slots of {} bytes, {} free",
        pool.capacity(),
        pool.slot_size(),
        pool.elements_free()
    );

    let mut live: Vec<Box<Particle, &PoolAllocator>> = Vec::new();
    for id in 0..6 {
        match pool.try_box(spawn(id)) {
            Ok(p) => {
                let slot = pool.index_of(std::ptr::NonNull::from(&*p).cast()).unwrap();
                println!("spawned particle {id} in slot {slot}");
                live.push(p);
            }
            Err(p) => println!("pool full, dropped particle at x = {}", p.position[0]),
        }
    }

    // Kill particles 1 and 3. Their slots go back into the sorted free list.
    live.remove(3);
    live.remove(1);
    println!("after killing two: {} free", pool.elements_free());

    for id in 6..8 {
        let p = Box::new_in(spawn(id), &pool);
        let slot = pool.index_of(std::ptr::NonNull::from(&*p).cast()).unwrap();
        println!("spawned particle {id} in reused slot {slot}");
        live.push(p);
    }
    println!("live particles: {}", live.len());
    // `live` drops before `pool`, so every slot is returned before the pool's
    // memory is freed. Reversing that order is a compile error.
}

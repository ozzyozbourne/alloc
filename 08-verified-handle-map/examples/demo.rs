//! Game entities stored in a handle map. Systems keep handles, not pointers.
use verified_handle_map::{Handle, HandleMap};

#[derive(Debug)]
struct Entity {
    name: &'static str,
    pos: [f32; 2],
}

fn main() {
    let mut entities: HandleMap<Entity, 4> = HandleMap::new();
    let player = entities
        .add(Entity {
            name: "player",
            pos: [0.0, 0.0],
        })
        .unwrap();
    let enemy = entities
        .add(Entity {
            name: "enemy",
            pos: [5.0, 1.0],
        })
        .unwrap();
    println!("player {player:?}, enemy {enemy:?}");

    // An AI system stores the enemy's handle as its target.
    let target: Handle = enemy;

    // Another system kills the enemy and spawns a pickup in the same slot.
    entities.remove(enemy);
    let pickup = entities
        .add(Entity {
            name: "pickup",
            pos: [5.0, 1.0],
        })
        .unwrap();
    println!("pickup {pickup:?} reuses slot {}", pickup.index());

    // The stale target does not resolve to the pickup.
    match entities.get(target) {
        Some(e) => println!("target is {}", e.name),
        None => println!(
            "target is gone (generation {} != {})",
            target.generation(),
            pickup.generation()
        ),
    }

    if let Some(p) = entities.get_mut(player) {
        p.pos[0] += 1.0;
    }
    for (h, e) in entities.iter() {
        println!(
            "  slot {} gen {}: {} at {:?}",
            h.index(),
            h.generation(),
            e.name,
            e.pos
        );
    }

    // Fill the map: the fifth entity is handed back instead of overflowing.
    for name in ["a", "b", "c"] {
        match entities.add(Entity {
            name,
            pos: [0.0; 2],
        }) {
            Ok(h) => println!("added {name} in slot {}", h.index()),
            Err(e) => println!("map full, got {} back", e.name),
        }
    }
}

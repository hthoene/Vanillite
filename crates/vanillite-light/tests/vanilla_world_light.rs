//! Compares the light engine with the light a Vanilla server stored in a world (ignored by
//! default; it needs that world).
//!
//! Every compared chunk is lit from scratch from the Vanilla world's own blocks (its 3 × 3 area,
//! which holds every source that reaches it) and compared with the light Vanilla saved, in the
//! sections Vanilla keeps light data for. `VANILLITE_LIGHT_WORLD=<world>/dimensions/minecraft/overworld`,
//! `VANILLITE_LIGHT_AREA=x0,z0,x1,z1` (chunks, inclusive; one ring around must exist too),
//! `VANILLITE_LIGHT_VERBOSE=<n>` (print the first n differing cells). Run with
//! `cargo test --release -p vanillite-light --test vanilla_world_light -- --ignored --nocapture`.

mod common;

use std::collections::BTreeMap;
use std::path::PathBuf;

use vanillite_base::ChunkPos;

#[test]
#[ignore = "needs a Vanilla world (VANILLITE_LIGHT_WORLD)"]
fn compare_with_vanilla_world() {
    let Some(dir) = std::env::var_os("VANILLITE_LIGHT_WORLD").map(PathBuf::from) else {
        return;
    };
    let area: Vec<i32> = std::env::var("VANILLITE_LIGHT_AREA")
        .unwrap_or_else(|_| "-8,-8,7,7".to_owned())
        .split(',')
        .map(|v| v.trim().parse().unwrap())
        .collect();
    let verbose: usize = std::env::var("VANILLITE_LIGHT_VERBOSE")
        .ok()
        .map_or(0, |v| v.parse().unwrap());
    let world = common::World::open(&dir);
    let mut totals = common::Stats::default();
    let mut pairs: BTreeMap<(char, u8, u8), u64> = BTreeMap::new();
    let mut worst = Vec::new();
    let mut printed = 0;
    for z in area[1]..=area[3] {
        for x in area[0]..=area[2] {
            let pos = ChunkPos::new(x, z);
            let stats = world.compare(pos, common::Mode::Relight, |diff| {
                *pairs
                    .entry((diff.kind, diff.ours, diff.vanilla))
                    .or_default() += 1;
                if printed < verbose {
                    printed += 1;
                    println!("{diff}");
                }
            });
            if stats.differ() > 0 {
                worst.push((stats.differ(), pos));
            }
            totals.add(&stats);
        }
    }
    worst.sort_by(|a, b| b.cmp(a));
    println!("{totals}");
    println!("most different chunks: {:?}", &worst[..worst.len().min(20)]);
    let mut pairs: Vec<_> = pairs.into_iter().collect();
    pairs.sort_by_key(|p| std::cmp::Reverse(p.1));
    for ((kind, ours, vanilla), n) in pairs.iter().take(30) {
        println!("{n:10}  {kind} ours {ours:2} vanilla {vanilla:2}");
    }
}

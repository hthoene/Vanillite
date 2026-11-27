//! Light after block changes (`LightEngine::relight`, which only recomputes the cells a change
//! reaches) against the reference, the recomputation of the whole area from its blocks
//! (`BfsLightEngine::relight_area`): random terrain with caves, overhangs, transparent, shaped
//! and glowing blocks, random batches of changes, and every light level must be the same.
//!
//! `VANILLITE_LIGHT_ROUNDS` sets the number of random worlds (default 12).

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use vanillite_base::{BlockPos, ChunkPos};
use vanillite_chunk::light::LightNibbles;
use vanillite_chunk::slots::{ChunkStatus, LightEngine, Neighbours, ProtoChunk, WorldInfo};
use vanillite_data::block::{BlockId, BlockState};
use vanillite_light::BfsLightEngine;

const HEIGHT: i32 = 80;
/// The world is 7 × 7 chunks; blocks change in the 3 × 3 in its middle, whose light reaches the
/// 5 × 5 around it.
const RADIUS: i32 = 3;

/// CPU time of this thread (Linux), so a busy machine does not blur the comparison.
fn cpu_time() -> Duration {
    let stat = std::fs::read_to_string("/proc/thread-self/schedstat").unwrap_or_default();
    let nanos = stat.split(' ').next().and_then(|v| v.parse().ok());
    Duration::from_nanos(nanos.unwrap_or(0))
}

struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: i32) -> i32 {
        (self.next() % bound as u64) as i32
    }
}

fn state(name: &str, properties: &[(&str, &str)]) -> u16 {
    let mut state = BlockId::by_name(name)
        .unwrap_or_else(|| panic!("no block {name}"))
        .default_state();
    for (property, value) in properties {
        state = state
            .with(property, value)
            .unwrap_or_else(|| panic!("{name} has no {property}={value}"));
    }
    state.0
}

/// Blocks with every kind of light behaviour: opaque, clear, dampening, shaped, emitting.
fn palette() -> Vec<u16> {
    vec![
        BlockState::AIR.0,
        BlockState::AIR.0,
        state("minecraft:stone", &[]),
        state("minecraft:stone", &[]),
        state("minecraft:glass", &[]),
        state("minecraft:water", &[]),
        state("minecraft:oak_leaves", &[]),
        state("minecraft:oak_slab", &[]),
        state("minecraft:oak_slab", &[("type", "top")]),
        state("minecraft:stone_stairs", &[]),
        state("minecraft:oak_trapdoor", &[("half", "top")]),
        state("minecraft:torch", &[]),
        state("minecraft:glowstone", &[]),
        state("minecraft:lava", &[]),
        state("minecraft:magma_block", &[]),
        state("minecraft:oak_button", &[]),
    ]
}

fn world(random: &mut Random, info: &WorldInfo, palette: &[u16]) -> Vec<Arc<ProtoChunk>> {
    let stone = palette[2];
    let mut chunks = Vec::new();
    for cz in -RADIUS..=RADIUS {
        for cx in -RADIUS..=RADIUS {
            let mut chunk = ProtoChunk::empty(ChunkPos::new(cx, cz), info);
            chunk.status = ChunkStatus::Full;
            chunk.light_on = true;
            let base = 8 + random.below(30);
            for z in 0..16 {
                for x in 0..16 {
                    let top = base + random.below(4);
                    for y in 0..top {
                        // Caves and odd blocks inside the ground.
                        let block = match random.below(40) {
                            0..=5 => continue,
                            6 => palette[random.below(palette.len() as i32) as usize],
                            _ => stone,
                        };
                        chunk.chunk.set_block(x, y, z, block);
                    }
                    // Things above the ground: overhangs, floating lights, sometimes up in the
                    // highest section.
                    if random.below(12) == 0 {
                        let y = top + random.below(HEIGHT - top);
                        let block = palette[random.below(palette.len() as i32) as usize];
                        chunk.chunk.set_block(x, y, z, block);
                    }
                }
            }
            chunks.push(Arc::new(chunk));
        }
    }
    chunks
}

/// The reference light of the 5 × 5 in the middle.
fn reference(engine: &BfsLightEngine, chunks: &mut [Arc<ProtoChunk>]) {
    for cz in -1..=1 {
        for cx in -1..=1 {
            engine.relight_area(ChunkPos::new(cx, cz), &mut Neighbours::new(chunks));
        }
    }
}

fn compare(seed: u64, ours: &[Arc<ProtoChunk>], theirs: &[Arc<ProtoChunk>]) {
    for (a, b) in ours.iter().zip(theirs) {
        assert_eq!(a.pos, b.pos);
        if a.pos.x.abs() > 2 || a.pos.z.abs() > 2 {
            continue;
        }
        let (la, lb) = (a.chunk.light(), b.chunk.light());
        for layer in 1..la.len() - 1 {
            for (kind, x, y) in [
                ("sky", &la.sky()[layer], &lb.sky()[layer]),
                ("block", &la.block()[layer], &lb.block()[layer]),
            ] {
                assert_eq!(
                    *x == LightNibbles::Absent,
                    *y == LightNibbles::Absent,
                    "seed {seed}: {kind} layer {layer} of {:?} absent: ours {x:?}",
                    a.pos
                );
                if x == y {
                    continue;
                }
                for cell in 0..4096u32 {
                    let (cx, cy, cz) = (cell & 15, cell >> 8, (cell >> 4) & 15);
                    assert_eq!(
                        x.get(cx, cy, cz),
                        y.get(cx, cy, cz),
                        "seed {seed}: {kind} light at {} {} {} (ours, reference), block {:?}",
                        a.pos.x * 16 + cx as i32,
                        (layer as i32 - 1) * 16 + cy as i32,
                        a.pos.z * 16 + cz as i32,
                        BlockState(a.chunk.sections()[layer - 1].block(cx, cy, cz))
                            .block()
                            .name(),
                    );
                }
                // Same levels: both must also have chosen the same form.
                assert_eq!(
                    matches!(x, LightNibbles::Uniform(_)),
                    matches!(y, LightNibbles::Uniform(_)),
                    "seed {seed}: {kind} layer {layer} of {:?} uniform",
                    a.pos
                );
            }
        }
    }
}

#[test]
fn update_equals_recomputation() {
    common::install();
    let info = WorldInfo {
        min_y: 0,
        height: HEIGHT as u32,
        ..common::world_info(Path::new(""))
    };
    let palette = palette();
    let rounds: u64 = std::env::var("VANILLITE_LIGHT_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(12);
    let mut reference_time = Duration::ZERO;
    // Update time and batches: one change, a few, many close together, many anywhere.
    let mut update_time = [(Duration::ZERO, 0u32); 4];
    for seed in 1..=rounds {
        let mut random = Random(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let engine = BfsLightEngine::new(&WorldInfo {
            has_sky_light: seed % 5 != 0,
            ..info.clone()
        });
        let mut ours = world(&mut random, &info, &palette);
        reference(&engine, &mut ours);
        // Several batches on the same world: each update starts from the light of the last.
        for _ in 0..4 {
            let count = match random.below(4) {
                0 => 1,
                1 => 1 + random.below(4),
                _ => 1 + random.below(40),
            };
            // Usually close together (one build site, one explosion), sometimes anywhere.
            let near = random.below(4) != 0;
            let origin = [
                random.below(48) - 16,
                random.below(HEIGHT),
                random.below(48) - 16,
            ];
            let mut changes = Vec::new();
            for _ in 0..count {
                let pos = if near {
                    BlockPos::new(
                        (origin[0] + random.below(9) - 4).clamp(-16, 31),
                        (origin[1] + random.below(9) - 4).clamp(0, HEIGHT - 1),
                        (origin[2] + random.below(9) - 4).clamp(-16, 31),
                    )
                } else {
                    BlockPos::new(
                        random.below(48) - 16,
                        random.below(HEIGHT),
                        random.below(48) - 16,
                    )
                };
                let block = palette[random.below(palette.len() as i32) as usize];
                let chunk = ours
                    .iter_mut()
                    .find(|chunk| chunk.pos == pos.chunk())
                    .expect("chunk");
                Arc::make_mut(chunk).chunk.set_block(
                    (pos.x & 15) as u32,
                    pos.y,
                    (pos.z & 15) as u32,
                    block,
                );
                changes.push(pos);
            }
            let mut theirs = ours.clone();
            let start = cpu_time();
            reference(&engine, &mut theirs);
            reference_time += cpu_time() - start;

            let start = cpu_time();
            let light = engine.relight(&changes, &Neighbours::new(&mut ours));
            let kind = match (count, near) {
                (1, _) => 0,
                (2..=4, _) => 1,
                (_, true) => 2,
                (_, false) => 3,
            };
            update_time[kind].0 += cpu_time() - start;
            update_time[kind].1 += 1;
            for (pos, light) in light {
                let chunk = ours
                    .iter_mut()
                    .find(|chunk| chunk.pos == pos)
                    .expect("chunk");
                *Arc::make_mut(chunk).chunk.light_mut() = light;
            }
            compare(seed, &ours, &theirs);
        }
    }
    let batches: u32 = update_time.iter().map(|(_, count)| count).sum();
    let each =
        |(time, count): (Duration, u32)| time.as_secs_f64() * 1000.0 / f64::from(count.max(1));
    eprintln!(
        "{batches} batches. Update per batch: one change {:.3} ms, up to 4 changes {:.3} ms, up \
         to 40 close together {:.3} ms, up to 40 anywhere {:.3} ms. Recomputation of one 3 x 3 \
         area: {:.3} ms",
        each(update_time[0]),
        each(update_time[1]),
        each(update_time[2]),
        each(update_time[3]),
        each((reference_time, batches * 9)),
    );
}

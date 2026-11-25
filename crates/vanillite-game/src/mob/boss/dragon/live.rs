//! The pillars the fight rebuilds in the live world for the respawn (`EndSpikeFeature`): the
//! same blocks as the world generation, written through `setBlock` with neighbour updates.
//! The exit portal and the gateways are End portal blocks of [`crate::block::portal`].

use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Direction, Vec3};
use vanillite_data::block::{BlockId, BlockState, ids};
use vanillite_worldgen::feature::kinds::end::EndSpike;

use super::crystal::{Crystal, spawn_crystal};
use crate::level::Level;
use crate::survival::player::Env;

/// The horizontal directions in Vanilla's order (`Direction.Plane.HORIZONTAL`).
pub const HORIZONTAL: [Direction; 4] = [
    Direction::North,
    Direction::East,
    Direction::South,
    Direction::West,
];

fn state(block: BlockId) -> BlockState {
    block.default_state()
}

/// `EndSpikeFeature(List.of(spike), true, Optional.of(0, 128, 0)).placeSpike`: the pillar, its
/// cage and a new invulnerable crystal beaming at the dragon's spawn.
pub fn place_spike(level: &mut Level, env: &mut Env<'_>, spike: &EndSpike) {
    let radius = spike.radius;
    let (cx, cz) = (spike.center_x, spike.center_z);
    let limit = f64::from(radius * radius + 1);
    let obsidian = state(ids::OBSIDIAN);
    let min_y = level.min_y();
    for z in cz - radius..=cz + radius {
        for y in min_y..=spike.height + 10 {
            for x in cx - radius..=cx + radius {
                let pos = BlockPos::new(x, y, z);
                let dx = f64::from(x) - f64::from(cx);
                let dz = f64::from(z) - f64::from(cz);
                if dx * dx + dz * dz <= limit && y < spike.height {
                    level.set_block_and_update(pos, obsidian);
                } else if y > 65 {
                    level.set_block_and_update(pos, BlockState::AIR);
                }
            }
        }
    }
    if spike.guarded {
        for dx in -2_i32..=2 {
            for dz in -2_i32..=2 {
                for dy in 0..=3 {
                    let x_side = dx.abs() == 2;
                    let z_side = dz.abs() == 2;
                    let top = dy == 3;
                    if x_side || z_side || top {
                        let x_edge = dx == -2 || dx == 2 || top;
                        let z_edge = dz == -2 || dz == 2 || top;
                        let bars = state(ids::IRON_BARS);
                        let set = |s: BlockState, p: &str, v: bool| {
                            s.with(p, if v { "true" } else { "false" }).unwrap_or(s)
                        };
                        let bars = set(bars, "north", x_edge && dz != -2);
                        let bars = set(bars, "south", x_edge && dz != 2);
                        let bars = set(bars, "west", z_edge && dx != -2);
                        let bars = set(bars, "east", z_edge && dx != 2);
                        level.set_block_and_update(
                            BlockPos::new(cx + dx, spike.height + dy, cz + dz),
                            bars,
                        );
                    }
                }
            }
        }
    }
    let yaw = env.random.next_float() * 360.0;
    let crystal_pos = Vec3::new(
        f64::from(cx) + 0.5,
        f64::from(spike.height + 1),
        f64::from(cz) + 0.5,
    );
    let mut ents = std::mem::take(&mut level.entities);
    spawn_crystal(
        level,
        &mut ents,
        env,
        crystal_pos,
        yaw,
        Crystal {
            beam_target: Some(BlockPos::new(0, 128, 0)),
            invulnerable: true,
            ..Crystal::default()
        },
    );
    level.entities = ents;
    let at = BlockPos::new(cx, spike.height + 1, cz);
    level.set_block_and_update(at.offset(0, -1, 0), state(ids::BEDROCK));
    let fire = if crate::block::block_tags()
        .soul_fire_base_blocks
        .contains(level.block(at.offset(0, -1, 0)))
    {
        state(ids::SOUL_FIRE)
    } else {
        state(ids::FIRE)
    };
    level.set_block_and_update(at, fire);
}

//! The End features the fight places into the live world (`EndPodiumFeature`,
//! `EndGatewayFeature` with a delayed exit, `EndSpikeFeature` for the respawn): the same blocks
//! as the world generation, written through `setBlock` with neighbour updates.

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

/// `EndPodiumFeature.place` at `origin` (the portal layer's centre); `active` fills the
/// portal.
pub fn place_podium(level: &mut Level, origin: BlockPos, active: bool) {
    let bedrock = state(ids::BEDROCK);
    for z in origin.z - 4..=origin.z + 4 {
        for y in origin.y - 1..=origin.y + 32 {
            for x in origin.x - 4..=origin.x + 4 {
                let pos = BlockPos::new(x, y, z);
                let dist = pos.dist_sqr(origin);
                let inside_rim = dist < 2.5 * 2.5;
                if !inside_rim && dist >= 3.5 * 3.5 {
                    continue;
                }
                if y < origin.y {
                    if inside_rim {
                        level.set_block_and_update(pos, bedrock);
                    } else {
                        set_or_drop(level, pos, ids::END_STONE, active);
                    }
                } else if y > origin.y {
                    set_or_drop(level, pos, ids::AIR, active);
                } else if !inside_rim {
                    level.set_block_and_update(pos, bedrock);
                } else if active {
                    drop_previous_and_set(level, pos, ids::END_PORTAL);
                } else {
                    level.set_block_and_update(pos, BlockState::AIR);
                }
            }
        }
    }
    for y in 0..4 {
        level.set_block_and_update(origin.offset(0, y, 0), bedrock);
    }
    let pillar = origin.offset(0, 2, 0);
    for dir in HORIZONTAL {
        let facing = match dir {
            Direction::North => "north",
            Direction::East => "east",
            Direction::South => "south",
            _ => "west",
        };
        let torch = state(ids::WALL_TORCH)
            .with("facing", facing)
            .unwrap_or_else(|| state(ids::WALL_TORCH));
        level.set_block_and_update(pillar.relative(dir), torch);
    }
}

fn set_or_drop(level: &mut Level, pos: BlockPos, block: BlockId, active: bool) {
    if active {
        drop_previous_and_set(level, pos, block);
    } else {
        level.set_block_and_update(pos, state(block));
    }
}

/// `dropPreviousAndSetBlock`.
fn drop_previous_and_set(level: &mut Level, pos: BlockPos, block: BlockId) {
    if level.block(pos).block() != block {
        level.destroy_block(pos, true, None);
        level.set_block_and_update(pos, state(block));
    }
}

/// `EndGatewayFeature.place` with `END_GATEWAY_DELAYED` (no exit yet).
pub fn place_gateway(level: &mut Level, origin: BlockPos) {
    let bedrock = state(ids::BEDROCK);
    for z in origin.z - 1..=origin.z + 1 {
        for y in origin.y - 2..=origin.y + 2 {
            for x in origin.x - 1..=origin.x + 1 {
                let pos = BlockPos::new(x, y, z);
                let same_x = x == origin.x;
                let same_y = y == origin.y;
                let same_z = z == origin.z;
                let end = (y - origin.y).abs() == 2;
                let s = if same_x && same_y && same_z {
                    state(ids::END_GATEWAY)
                } else if same_y {
                    BlockState::AIR
                } else if (end && same_x && same_z) || ((same_x || same_z) && !end) {
                    bedrock
                } else {
                    BlockState::AIR
                };
                level.set_block_and_update(pos, s);
            }
        }
    }
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

/// The exit portal frame (`exitPortalPattern`) with its portal layer centred at `c`: the
/// bedrock pillar above, the ring with its centre, and the disc below.
pub fn portal_frame_at(level: &Level, c: BlockPos) -> bool {
    let bedrock = |p: BlockPos| level.block(p).block() == ids::BEDROCK;
    if !(1..=3).all(|dy| bedrock(c.offset(0, dy, 0))) || !bedrock(c) {
        return false;
    }
    const RING: [&str; 7] = [
        "  ###  ", " #   # ", "#     #", "#  #  #", "#     #", " #   # ", "  ###  ",
    ];
    const DISC: [&str; 7] = [
        "       ", "  ###  ", " ##### ", " ##### ", " ##### ", "  ###  ", "       ",
    ];
    for (layer, dy) in [(RING, 0), (DISC, -1)] {
        for (row, line) in layer.iter().enumerate() {
            for (col, ch) in line.bytes().enumerate() {
                if ch == b'#' {
                    #[allow(clippy::cast_possible_wrap, clippy::cast_possible_truncation)]
                    let p = c.offset(col as i32 - 3, dy, row as i32 - 3);
                    if !bedrock(p) {
                        return false;
                    }
                }
            }
        }
    }
    true
}

/// Every position of the frame's 7×5×7 box (for clearing it before a respawn).
pub fn portal_box(c: BlockPos) -> impl Iterator<Item = BlockPos> {
    (-3..=3).flat_map(move |x| (-1..=3).flat_map(move |y| (-3..=3).map(move |z| c.offset(x, y, z))))
}

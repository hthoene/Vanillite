//! Where players respawn: at their bed or respawn anchor (`ServerPlayer.findRespawnAndUseSpawnBlock`),
//! at a forced respawn position, or scattered around the world spawn (`PlayerSpawnFinder`), and
//! where a new world's spawn is (`PlayerSpawnFinder.getSpawnPosInChunk`).

use vanillite_base::random::{LegacyRandom, RandomSource};
use vanillite_base::{Aabb, BlockPos, ChunkPos, Vec3};
use vanillite_chunk::HeightmapKind;
use vanillite_data::block::{BlockState, ids};
use vanillite_world::entity::Dimensions;
use vanillite_worldgen::behaviour::StateExt;

use crate::block::bed;
use crate::level::Level;
use crate::survival::player::RespawnConfig;

fn player_dims() -> Dimensions {
    Dimensions::of(vanillite_data::entity::ids::PLAYER)
}

/// `RespawnPosAngle.calculateLookAtYaw`: the yaw from `position` towards the block.
fn look_at_yaw(position: Vec3, block: BlockPos) -> f32 {
    let look = Vec3::new(
        f64::from(block.x) + 0.5 - position.x,
        f64::from(block.y) - position.y,
        f64::from(block.z) + 0.5 - position.z,
    )
    .normalize();
    #[allow(clippy::cast_possible_truncation)]
    vanillite_base::math::wrap_degrees_f((crate::mob::atan2_deg(look.z, look.x) - 90.0) as f32)
}

/// Loads the chunks within `radius` blocks of `pos` (Vanilla reads them, which loads them).
fn load_around(level: &mut Level, pos: BlockPos, radius: i32) {
    for cx in (pos.x - radius) >> 4..=(pos.x + radius) >> 4 {
        for cz in (pos.z - radius) >> 4..=(pos.z + radius) >> 4 {
            level.load_chunk(ChunkPos::new(cx, cz));
        }
    }
}

/// `ServerPlayer.findRespawnAndUseSpawnBlock(level, config, consumeSpawnBlock)`: the position,
/// yaw and pitch at a charged anchor (one charge used when `consume`), a bed that allows
/// setting the spawn here, or a free forced position; `None` when the block is gone.
pub fn find_respawn_and_use_spawn_block(
    level: &mut Level,
    config: &RespawnConfig,
    consume: bool,
) -> Option<(Vec3, f32, f32)> {
    let pos = config.pos;
    load_around(level, pos, 2);
    let state = level.block(pos);
    let forced = config.forced;
    if state.block() == ids::RESPAWN_ANCHOR
        && (forced || state.int("charges").unwrap_or(0) > 0)
        && level.settings.respawn_anchor_works
    {
        let stand = bed::anchor_stand_up_position(level, pos, player_dims());
        if !forced && consume && stand.is_some() {
            let charges = state.int("charges").unwrap_or(0);
            level.set_block_and_update(pos, state.set_int("charges", charges - 1));
        }
        return stand.map(|p| (p, look_at_yaw(p, pos), 0.0));
    }
    if bed::is_bed(state)
        && bed::bed_rule(level, state)
            .can_set_spawn
            .test(bed::is_dark_outside(level))
    {
        return bed::stand_up_position(
            level,
            pos,
            bed::facing(state),
            config.yaw,
            player_dims(),
            true,
        )
        .map(|p| (p, look_at_yaw(p, pos), 0.0));
    }
    if !forced {
        return None;
    }
    let free = |s: BlockState| !crate::block::is_solid(s) && !crate::block::is_liquid(s);
    (free(state) && free(level.block(pos.offset(0, 1, 0)))).then(|| {
        (
            Vec3::new(
                f64::from(pos.x) + 0.5,
                f64::from(pos.y) + 0.1,
                f64::from(pos.z) + 0.5,
            ),
            config.yaw,
            config.pitch,
        )
    })
}

/// `PlayerSpawnFinder.noCollisionNoLiquid`: a player fits at the block's bottom centre without
/// touching blocks or liquids.
fn no_collision_no_liquid(level: &Level, pos: BlockPos) -> bool {
    let dims = player_dims();
    let half = f64::from(dims.width) / 2.0;
    let (x, y, z) = (
        f64::from(pos.x) + 0.5,
        f64::from(pos.y),
        f64::from(pos.z) + 0.5,
    );
    let aabb = Aabb::new(
        x - half,
        y,
        z - half,
        x + half,
        y + f64::from(dims.height),
        z + half,
    );
    let mut scratch = Vec::new();
    if !crate::mob::physics::no_collision(&level.view(), &aabb, &mut scratch) {
        return false;
    }
    let floor = |v: f64| vanillite_base::math::floor(v);
    for by in floor(aabb.min_y)..=floor(aabb.max_y - 1.0e-7) {
        for bz in floor(aabb.min_z)..=floor(aabb.max_z - 1.0e-7) {
            for bx in floor(aabb.min_x)..=floor(aabb.max_x - 1.0e-7) {
                if !level.fluid(BlockPos::new(bx, by, bz)).is_empty() {
                    return false;
                }
            }
        }
    }
    true
}

/// `Block.isFaceFull(collisionShape, UP)`.
fn top_face_full(state: BlockState) -> bool {
    if state.has(vanillite_data::block::flags::COLLISION_FULL_BLOCK) {
        return true;
    }
    state
        .collision_shape()
        .iter()
        .any(|b| b[4] >= 1.0 && b[0] <= 0.0 && b[3] >= 1.0 && b[2] <= 0.0 && b[5] >= 1.0)
}

/// `PlayerSpawnFinder.getLevelRespawnPos`: the block above the highest full top face of the
/// column, unless the column is under water or the top is a fluid.
fn level_respawn_pos(level: &Level, x: i32, z: i32, spawn_height: i32) -> Option<BlockPos> {
    let height = |kind| crate::portal::height_at(level, kind, x, z);
    let top = if level.spawner_env.env.has_ceiling {
        spawn_height
    } else {
        height(HeightmapKind::MotionBlocking)
    };
    if top < level.min_y() {
        return None;
    }
    let surface = height(HeightmapKind::WorldSurface);
    if surface <= top && surface > height(HeightmapKind::OceanFloor) {
        return None;
    }
    let mut y = top + 1;
    while y >= level.min_y() {
        let pos = BlockPos::new(x, y, z);
        let state = level.block(pos);
        if !state.fluid_state().is_empty() {
            return None;
        }
        if top_face_full(state) {
            return Some(pos.offset(0, 1, 0));
        }
        y -= 1;
    }
    None
}

/// `PlayerSpawnFinder.getSpawnPosInChunk`: the first column of the chunk with a respawn
/// position (`x`, then `z`).
pub fn spawn_pos_in_chunk(
    level: &mut Level,
    chunk: ChunkPos,
    spawn_height: i32,
) -> Option<BlockPos> {
    level.load_chunk(chunk);
    for x in chunk.min_block_x()..chunk.min_block_x() + 16 {
        for z in chunk.min_block_z()..chunk.min_block_z() + 16 {
            if let Some(pos) = level_respawn_pos(level, x, z, spawn_height) {
                return Some(pos);
            }
        }
    }
    None
}

/// `PlayerSpawnFinder.fixupSpawnHeight`: up out of blocks, then down onto the ground.
fn fixup_spawn_height(level: &Level, spawn: BlockPos) -> Vec3 {
    let mut pos = spawn;
    while !no_collision_no_liquid(level, pos) && pos.y < level.max_y() {
        pos = pos.offset(0, 1, 0);
    }
    pos = pos.offset(0, -1, 0);
    while no_collision_no_liquid(level, pos) && pos.y > level.min_y() {
        pos = pos.offset(0, -1, 0);
    }
    pos = pos.offset(0, 1, 0);
    Vec3::new(
        f64::from(pos.x) + 0.5,
        f64::from(pos.y),
        f64::from(pos.z) + 0.5,
    )
}

/// How far (in chunks) from the suggestion the spawn search loads chunks it needs; candidates
/// further out (respawn radii above 32) are only tried where their chunk is loaded.
const SEARCH_LOAD_CHUNKS: i32 = 2;

/// `PlayerSpawnFinder.findSpawn(level, suggestion)` with the `respawn_radius` rule: a random
/// free column within the radius (clamped to the world border), in adventure mode (the
/// server's default game mode) or when none is free the suggestion on the ground.
pub fn find_spawn(
    level: &mut Level,
    suggestion: BlockPos,
    respawn_radius: i32,
    adventure: bool,
) -> Vec3 {
    load_around(level, suggestion, 0);
    if adventure {
        return fixup_spawn_height(level, suggestion);
    }
    let mut radius = respawn_radius.max(0);
    #[allow(clippy::cast_possible_truncation)]
    let to_border = vanillite_base::math::floor(
        level
            .border
            .distance_to_border(f64::from(suggestion.x), f64::from(suggestion.z)),
    );
    if to_border < radius {
        radius = to_border;
    }
    if to_border <= 1 {
        radius = 1;
    }
    let side = i64::from(radius) * 2 + 1;
    let count = i32::try_from((side * side).min(1024)).unwrap_or(1024);
    let coprime = if count <= 16 { count - 1 } else { 17 };
    // `RandomSource.createThreadLocalInstance()`: an unseeded random.
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as i64);
    let offset = LegacyRandom::new(seed).next_int_bounded(count);
    let width = radius * 2 + 1;
    let center = suggestion.chunk();
    for index in 0..count {
        let value = (offset + coprime * index) % count;
        let x = suggestion.x + value % width - radius;
        let z = suggestion.z + value / width - radius;
        let chunk = BlockPos::new(x, 0, z).chunk();
        if (chunk.x - center.x).abs() <= SEARCH_LOAD_CHUNKS
            && (chunk.z - center.z).abs() <= SEARCH_LOAD_CHUNKS
        {
            level.load_chunk(chunk);
        } else if !level.is_loaded(BlockPos::new(x, level.min_y(), z)) {
            continue;
        }
        if let Some(pos) = level_respawn_pos(level, x, z, 64)
            && no_collision_no_liquid(level, pos)
        {
            return Vec3::new(
                f64::from(pos.x) + 0.5,
                f64::from(pos.y),
                f64::from(pos.z) + 0.5,
            );
        }
    }
    fixup_spawn_height(level, suggestion)
}

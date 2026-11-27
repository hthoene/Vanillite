//! Creaking hearts (`CreakingHeartBlock`, `CreakingHeartBlockEntity`): a heart between two
//! pale oak logs of its axis wakes up at night (`EnvironmentAttributes.CREAKING_ACTIVE`) and
//! spawns a creaking that protects it. The creaking cannot be hurt while its heart lives;
//! players hitting it make the heart grow resin clumps on the logs around it. Breaking the heart
//! kills the creaking, a heart that loses its creaking for too long forgets it.
//!
//! The creaking side (home position, damage transfer, tearing down) is in
//! [`crate::mob::fam::mount::creaking`].

use std::collections::VecDeque;

use vanillite_base::hash::FxHashSet;
use vanillite_base::random::RandomSource;
use vanillite_base::{Aabb, BlockPos, Direction, Vec3};
use vanillite_data::block::{BlockId, BlockState, ids};
use vanillite_nbt::Compound;
use vanillite_world::entity::EntityId;
use vanillite_worldgen::behaviour::StateExt;

use super::place::{tags, util};
use super::{BlockBehaviour, sound_id};
use crate::level::game_event::{GameEventContext, events};
use crate::level::{BlockEntity, Level, SOUND_BLOCKS};
use crate::mob::Kind;
use crate::mob::fam::mount::creaking::{self, HeartDeath};

/// `PLAYER_DETECTION_RANGE`.
const PLAYER_DETECTION_RANGE: f64 = 32.0;
/// `DISTANCE_CREAKING_TOO_FAR`.
const DISTANCE_CREAKING_TOO_FAR: f64 = 34.0;
/// `SPAWN_RANGE_XZ`.
const SPAWN_RANGE_XZ: i32 = 16;
/// `SPAWN_RANGE_Y`.
const SPAWN_RANGE_Y: i32 = 8;
/// `ATTEMPTS_PER_SPAWN`.
const ATTEMPTS_PER_SPAWN: i32 = 5;
/// `TICKS_GRACE_PERIOD`: how long an unresolved creaking UUID is kept.
const TICKS_GRACE_PERIOD: i64 = 30;
/// How far the heart looks for its creaking by UUID (`getEntity(uuid)` is level wide; a
/// creaking farther than [`DISTANCE_CREAKING_TOO_FAR`] is torn down anyway).
const SEARCH_RADIUS: f64 = 64.0;
/// `Creaking.CREAKING_ORANGE`.
const CREAKING_ORANGE: i32 = 16_545_810;
/// `Creaking.CREAKING_GRAY`.
const CREAKING_GRAY: i32 = 6_250_335;

/// The creaking a heart knows (`creakingInfo`): its UUID and, once resolved
/// (`Either.left`), the live entity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Link {
    pub uuid: u128,
    pub entity: Option<EntityId>,
}

/// The data of a `CreakingHeartBlockEntity`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CreakingHeart {
    /// `creakingInfo`.
    pub creaking: Option<Link>,
    /// `ticksExisted`.
    pub ticks_existed: i64,
    /// `ticker`: ticks until the next state and creaking check.
    pub ticker: i32,
    /// `emitter`: ticks left of the hurt effect.
    pub emitter: i32,
    /// `emitterTarget`.
    pub emitter_target: Option<Vec3>,
    /// `outputSignal`.
    pub output_signal: i32,
}

impl CreakingHeart {
    /// `loadAdditional`: the creaking's UUID, unresolved.
    pub fn load(&mut self, nbt: &Compound) {
        match nbt.get("creaking").and_then(super::trial_spawner::uuid_of) {
            Some(uuid) => self.set_uuid(uuid),
            None => self.creaking = None,
        }
    }

    /// `saveAdditional`.
    pub fn save(&self, nbt: &mut Compound) {
        if let Some(link) = self.creaking {
            nbt.insert("creaking", super::trial_spawner::uuid_tag(link.uuid));
        }
    }

    /// `setCreakingInfo(uuid)`.
    fn set_uuid(&mut self, uuid: u128) {
        self.creaking = Some(Link { uuid, entity: None });
        self.ticks_existed = 0;
    }
}

/// Fills the creaking heart behaviours.
pub(super) fn fill(block: BlockId, b: &mut BlockBehaviour) {
    if block == ids::CREAKING_HEART {
        b.tick = Some(tick);
        b.analog_output = Some(analog_output);
        b.affect_neighbors_after_removal = Some(super::dispenser::container_removed);
    }
}

fn heart(level: &Level, pos: BlockPos) -> Option<&CreakingHeart> {
    match level.block_entity(pos) {
        Some(BlockEntity::CreakingHeart(h)) => Some(h),
        _ => None,
    }
}

fn heart_mut(level: &mut Level, pos: BlockPos) -> Option<&mut CreakingHeart> {
    match level.block_entity_mut(pos) {
        Some(BlockEntity::CreakingHeart(h)) => Some(h),
        _ => None,
    }
}

fn heart_state(state: BlockState) -> &'static str {
    match state.value("creaking_heart_state") {
        Some("awake") => "awake",
        Some("dormant") => "dormant",
        _ => "uprooted",
    }
}

/// `CreakingHeartBlock.hasRequiredLogs`: pale oak logs of the heart's axis on both sides.
pub fn has_required_logs(level: &Level, state: BlockState, pos: BlockPos) -> bool {
    let Some(axis) = util::StateExt::get_axis(state, "axis") else {
        return false;
    };
    [util::from_axis(axis, false), util::from_axis(axis, true)]
        .into_iter()
        .all(|d| {
            let n = level.block(pos.relative(d));
            tags::PALE_OAK_LOGS.has(n) && util::StateExt::get_axis(n, "axis") == Some(axis)
        })
}

/// `EnvironmentAttributes.CREAKING_ACTIVE` at the heart (the overworld's night).
fn creaking_active(level: &Level) -> bool {
    level.spawner_env.env.creaking_active
}

/// `CreakingHeartBlock.updateState`: an uprooted heart between logs becomes awake or dormant.
fn update_state(level: &Level, state: BlockState, pos: BlockPos) -> BlockState {
    if has_required_logs(level, state, pos) && heart_state(state) == "uprooted" {
        state.set(
            "creaking_heart_state",
            if creaking_active(level) {
                "awake"
            } else {
                "dormant"
            },
        )
    } else {
        state
    }
}

/// `CreakingHeartBlock.getStateForPlacement` after the shape: awake at night.
pub fn placement_state(level: &Level, state: BlockState) -> BlockState {
    if state.is(ids::CREAKING_HEART) && heart_state(state) == "dormant" && creaking_active(level) {
        state.set("creaking_heart_state", "awake")
    } else {
        state
    }
}

/// `CreakingHeartBlock.tick` (scheduled by `updateShape`).
fn tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    let updated = update_state(level, state, pos);
    if updated != state {
        level.set_block_and_update(pos, updated);
    }
}

/// `CreakingHeartBlock.getAnalogOutputSignal`.
fn analog_output(level: &Level, state: BlockState, pos: BlockPos, _: Direction) -> i32 {
    if heart_state(state) == "uprooted" {
        return 0;
    }
    heart(level, pos).map_or(0, |h| h.output_signal)
}

/// Whether the creaking with `uuid` is the one the heart at `pos` protects (`isProtector`).
pub fn is_protector(level: &Level, pos: BlockPos, uuid: u128) -> bool {
    heart(level, pos).is_some_and(|h| h.creaking.is_some_and(|l| l.uuid == uuid))
}

/// The live creaking with `uuid` near `pos`.
fn find_creaking(level: &Level, pos: BlockPos, uuid: u128) -> Option<EntityId> {
    let c = super::spawner::center(pos);
    let r = SEARCH_RADIUS;
    let area = Aabb::new(c.x - r, c.y - r, c.z - r, c.x + r, c.y + r, c.z + r);
    let mut found = None;
    level.entities.store.for_each_in(&area, false, |id| {
        if found.is_none() && live_creaking(level, id, uuid) {
            found = Some(id);
        }
    });
    found
}

/// Whether `id` is the creaking with `uuid` and not removed.
fn live_creaking(level: &Level, id: EntityId, uuid: u128) -> bool {
    level
        .entities
        .store
        .get(id)
        .is_some_and(|e| e.uuid() == uuid)
        && level
            .entities
            .mob(id)
            .is_some_and(|m| m.kind == Kind::Creaking && !m.removed)
}

/// `getCreakingProtector`: the creaking, resolving its UUID; an unresolved UUID is forgotten
/// after the grace period.
fn protector(level: &mut Level, pos: BlockPos) -> Option<EntityId> {
    let link = heart(level, pos)?.creaking?;
    if let Some(id) = link.entity {
        if live_creaking(level, id, link.uuid) {
            return Some(id);
        }
        heart_mut(level, pos)?.set_uuid(link.uuid);
    }
    if let Some(id) = find_creaking(level, pos, link.uuid) {
        heart_mut(level, pos)?.creaking = Some(Link {
            uuid: link.uuid,
            entity: Some(id),
        });
        return Some(id);
    }
    let h = heart_mut(level, pos)?;
    if h.ticks_existed >= TICKS_GRACE_PERIOD {
        h.creaking = None;
        level.block_entities.touch(pos.chunk());
    }
    None
}

fn creaking_box(level: &Level, id: EntityId) -> Option<Aabb> {
    level.entities.store.get(id).map(|e| e.aabb())
}

fn box_center(b: &Aabb) -> Vec3 {
    Vec3::new(
        (b.min_x + b.max_x) / 2.0,
        (b.min_y + b.max_y) / 2.0,
        (b.min_z + b.max_z) / 2.0,
    )
}

/// `distanceToCreaking`: from the heart's bottom centre, 0 without a creaking.
fn distance_to_creaking(level: &mut Level, pos: BlockPos) -> f64 {
    let Some(id) = protector(level, pos) else {
        return 0.0;
    };
    let bottom = Vec3::new(
        f64::from(pos.x) + 0.5,
        f64::from(pos.y),
        f64::from(pos.z) + 0.5,
    );
    level
        .entities
        .store
        .get(id)
        .map_or(0.0, |e| e.pos().distance_to_sqr(bottom).sqrt())
}

/// `computeAnalogOutputSignal`: 15 next to the heart, down to 0 at 32 blocks.
fn compute_output(level: &mut Level, pos: BlockPos) -> i32 {
    if heart(level, pos).is_none_or(|h| h.creaking.is_none()) || protector(level, pos).is_none() {
        return 0;
    }
    let distance = distance_to_creaking(level, pos);
    let scaled = distance.clamp(0.0, 32.0) / 32.0;
    #[allow(clippy::cast_possible_truncation)]
    let floor = (scaled * 15.0).floor() as i32;
    15 - floor
}

/// `CreakingHeartBlockEntity.serverTick`. An uprooted heart has no ticker in Vanilla
/// (`getTicker`); here it skips the tick.
pub fn server_tick(level: &mut Level, pos: BlockPos, state: BlockState) {
    if heart_state(state) == "uprooted" {
        return;
    }
    let Some(h) = heart_mut(level, pos) else {
        return;
    };
    h.ticks_existed += 1;
    let computed = compute_output(level, pos);
    let Some(h) = heart_mut(level, pos) else {
        return;
    };
    if h.output_signal != computed {
        h.output_signal = computed;
        level.update_neighbour_for_output_signal(pos, ids::CREAKING_HEART);
    }
    tick_emitter(level, pos);
    let Some(h) = heart_mut(level, pos) else {
        return;
    };
    let ticker = h.ticker;
    h.ticker -= 1;
    if ticker >= 0 {
        return;
    }
    let next = level.random().next_int_bounded(5) + 20;
    if let Some(h) = heart_mut(level, pos) {
        h.ticker = next;
    }
    let updated = update_creaking_state(level, state, pos);
    if updated != state {
        level.set_block_and_update(pos, updated);
        if heart_state(updated) == "uprooted" {
            return;
        }
    }
    let has_info = heart(level, pos).is_some_and(|h| h.creaking.is_some());
    if !has_info {
        let env = level.spawner_env;
        if heart_state(updated) == "awake"
            && env.spawn_mobs
            && env.spawn_monsters
            && env.difficulty != crate::survival::rules::Difficulty::Peaceful
            && nearest_player_near(level, pos)
            && let Some(id) = spawn_protector(level, pos)
        {
            let uuid = level.entities.store.get(id).map_or(0, |e| e.uuid());
            if let Some(h) = heart_mut(level, pos) {
                h.creaking = Some(Link {
                    uuid,
                    entity: Some(id),
                });
            }
            level.block_entities.touch(pos.chunk());
            creaking_sound(level, id, "minecraft:entity.creaking.spawn");
            level.play_block_sound(
                None,
                pos,
                sound_id("block.creaking_heart.spawn"),
                SOUND_BLOCKS,
                1.0,
                1.0,
            );
        }
    } else if let Some(id) = protector(level, pos) {
        let persistent = level.entities.mob(id).is_some_and(|m| m.persistent);
        if (!creaking_active(level) && !persistent)
            || distance_to_creaking(level, pos) > DISTANCE_CREAKING_TOO_FAR
            || player_is_stuck_in(level, id)
        {
            remove_protector(level, pos, None);
        }
    }
}

/// The hurt effect of the last player hit: trail particles and the heart's sound wandering
/// from the creaking to the heart.
fn tick_emitter(level: &mut Level, pos: BlockPos) {
    let Some(h) = heart(level, pos) else {
        return;
    };
    let emitter = h.emitter;
    if emitter <= 0 {
        return;
    }
    if emitter > 50 {
        emit_particles(level, pos, 1, true);
        emit_particles(level, pos, 1, false);
    }
    if emitter % 10 == 0 && heart(level, pos).is_some_and(|h| h.emitter_target.is_some()) {
        if let Some(id) = protector(level, pos)
            && let Some(b) = creaking_box(level, id)
            && let Some(h) = heart_mut(level, pos)
        {
            h.emitter_target = Some(box_center(&b));
        }
        let Some(target) = heart(level, pos).and_then(|h| h.emitter_target) else {
            return;
        };
        let center = super::spawner::center(pos);
        #[allow(clippy::cast_precision_loss)]
        let progress = 0.2_f32 + 0.8_f32 * (100 - emitter) as f32 / 100.0;
        let p = f64::from(progress);
        let at = Vec3::new(
            (center.x - target.x) * p + target.x,
            (center.y - target.y) * p + target.y,
            (center.z - target.z) * p + target.z,
        );
        let sound_pos = BlockPos::containing(at.x, at.y, at.z);
        #[allow(clippy::cast_precision_loss)]
        let volume = emitter as f32 / 2.0 / 100.0 + 0.5;
        level.play_block_sound(
            None,
            sound_pos,
            sound_id("block.creaking_heart.hurt"),
            SOUND_BLOCKS,
            volume,
            1.0,
        );
    }
    if let Some(h) = heart_mut(level, pos) {
        h.emitter -= 1;
    }
}

/// `updateCreakingState`: uprooted without logs and creaking, else awake or dormant by the
/// time.
fn update_creaking_state(level: &Level, state: BlockState, pos: BlockPos) -> BlockState {
    let no_info = heart(level, pos).is_none_or(|h| h.creaking.is_none());
    if !has_required_logs(level, state, pos) && no_info {
        state.set("creaking_heart_state", "uprooted")
    } else {
        state.set(
            "creaking_heart_state",
            if creaking_active(level) {
                "awake"
            } else {
                "dormant"
            },
        )
    }
}

/// `level.getNearestPlayer(x, y, z, 32, false) != null`: a non-spectator player within 32
/// blocks of the heart's corner.
fn nearest_player_near(level: &Level, pos: BlockPos) -> bool {
    let at = Vec3::new(f64::from(pos.x), f64::from(pos.y), f64::from(pos.z));
    let range = PLAYER_DETECTION_RANGE * PLAYER_DETECTION_RANGE;
    level.entities.players.iter().any(|(id, p)| {
        p.game_mode != vanillite_config::GameMode::Spectator
            && level
                .entities
                .store
                .get(id)
                .is_some_and(|e| e.pos().distance_to_sqr(at) < range)
    })
}

/// `Creaking.playerIsStuckInYou`: a nearby player's eyes were inside the creaking for more
/// than 4 checks.
fn player_is_stuck_in(level: &mut Level, id: EntityId) -> bool {
    let Some(own) = creaking_box(level, id) else {
        return false;
    };
    let players: Vec<EntityId> = level
        .entities
        .mob(id)
        .and_then(|m| m.brain.as_deref())
        .map(|b| {
            b.entities(crate::mob::brain::MemoryType::NearestPlayers)
                .to_vec()
        })
        .unwrap_or_default();
    let inside = players.iter().any(|&p| {
        level.entities.store.get(p).is_some_and(|e| {
            let info = e.kind().info();
            let eye =
                f64::from(info.eye_height) * f64::from(e.dims().height / info.height.max(0.01));
            let at = e.pos();
            own.contains(at.x, at.y + eye, at.z)
        })
    });
    let Some(m) = level.entities.mob_mut(id) else {
        return false;
    };
    let c = creaking::creaking_mut(m);
    if players.is_empty() || !inside {
        c.player_stuck_counter = 0;
        return false;
    }
    c.player_stuck_counter += 1;
    c.player_stuck_counter > 4
}

/// `spawnProtector`: `SpawnUtil.trySpawnMob(CREAKING, SPAWNER, level, pos, 5, 16, 8,
/// ON_TOP_OF_COLLIDER_NO_LEAVES, true)`, then the creaking is bound to the heart.
fn spawn_protector(level: &mut Level, pos: BlockPos) -> Option<EntityId> {
    let type_id = Kind::Creaking.type_id();
    for _ in 0..ATTEMPTS_PER_SPAWN {
        let dx = level
            .random()
            .next_int_between_inclusive(-SPAWN_RANGE_XZ, SPAWN_RANGE_XZ);
        let dz = level
            .random()
            .next_int_between_inclusive(-SPAWN_RANGE_XZ, SPAWN_RANGE_XZ);
        let search = pos.offset(dx, SPAWN_RANGE_Y, dz);
        if !level
            .border
            .is_within_bounds(f64::from(search.x), f64::from(search.z))
        {
            continue;
        }
        let Some(at_block) = spawn_position(level, search) else {
            continue;
        };
        let at = Vec3::new(
            f64::from(at_block.x) + 0.5,
            f64::from(at_block.y),
            f64::from(at_block.z) + 0.5,
        );
        if !super::spawner::no_collision_at(level, type_id, at) {
            continue;
        }
        // `EntityType.create`: the yaw, then `finalizeSpawn(SPAWNER)`.
        let mut mob = crate::mob::MobData::new(Kind::Creaking, crate::mob::new_entity_seed());
        let y_rot = vanillite_base::math::wrap_degrees_f(level.random().next_float() * 360.0);
        let extra = super::spawner::finalize_spawner_mob(
            level,
            &mut mob,
            at,
            crate::mob::fam::SpawnReason::Spawner,
        );
        if !super::spawner::mob_spawn_checks(level, Kind::Creaking, at, true) {
            continue;
        }
        creaking::set_transient(&mut mob, pos);
        let id = crate::mob::spawn::add_mob_with(
            level,
            mob,
            at,
            y_rot,
            extra,
            crate::mob::fam::SpawnReason::Spawner,
        );
        // `playAmbientSound`.
        if let Some(sound) = level
            .entities
            .mob_mut(id)
            .and_then(crate::mob::kinds::ambient_sound)
        {
            creaking_sound(level, id, sound);
        }
        level.game_event(events().entity_place, at, GameEventContext::of(Some(id)));
        return Some(id);
    }
    None
}

/// `SpawnUtil.moveToPossibleSpawnPosition` with `ON_TOP_OF_COLLIDER_NO_LEAVES`: the first
/// block top below `search` with an empty collision above that is no leaves.
fn spawn_position(level: &Level, search: BlockPos) -> Option<BlockPos> {
    let mut pos = search;
    let mut above = level.block(pos);
    for _ in -SPAWN_RANGE_Y..=SPAWN_RANGE_Y {
        pos = pos.offset(0, -1, 0);
        let current = level.block(pos);
        if above.collision_shape().is_empty()
            && !tags::LEAVES.has(current)
            && util::face_full(current.collision_shape(), Direction::Up)
        {
            return Some(pos.offset(0, 1, 0));
        }
        above = current;
    }
    None
}

/// `creakingHurt`: a player hit the protecting creaking. Trail particles, and an awake heart
/// grows 2 or 3 resin clumps on its logs.
pub fn creaking_hurt(level: &mut Level, pos: BlockPos) {
    let Some(id) = protector(level, pos) else {
        return;
    };
    if heart(level, pos).is_none_or(|h| h.emitter > 0) {
        return;
    }
    emit_particles(level, pos, 20, false);
    let state = level.block(pos);
    if heart_state(state) == "awake" {
        let clumps = level.random().next_int_between_inclusive(2, 3);
        for _ in 0..clumps {
            if let Some(placed) = spread_resin(level, pos) {
                level.play_block_sound(
                    None,
                    placed,
                    sound_id("block.resin.place"),
                    SOUND_BLOCKS,
                    1.0,
                    1.0,
                );
                level.game_event_block(
                    events().block_place,
                    placed,
                    GameEventContext::with_state(None, state),
                );
            }
        }
    }
    let target = creaking_box(level, id).map(|b| box_center(&b));
    if let Some(h) = heart_mut(level, pos) {
        h.emitter = 100;
        h.emitter_target = target;
    }
}

/// `Util.shuffledCopy(Direction.values(), random)`.
fn shuffled_directions(level: &mut Level) -> [Direction; 6] {
    let mut dirs = Direction::ALL;
    for i in (2..=dirs.len()).rev() {
        #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
        let swap = level.random().next_int_bounded(i as i32) as usize;
        dirs.swap(i - 1, swap);
    }
    dirs
}

/// `spreadResin`: a breadth first walk over the pale oak logs (depth 2, 64 nodes) that puts a
/// resin clump face on the first free side of a log.
fn spread_resin(level: &mut Level, start: BlockPos) -> Option<BlockPos> {
    let mut nodes: VecDeque<(BlockPos, i32)> = VecDeque::new();
    let mut visited: FxHashSet<i64> = FxHashSet::default();
    nodes.push_back((start, 0));
    let mut count = 0;
    while let Some((current, depth)) = nodes.pop_front() {
        if !visited.insert(current.as_long()) {
            continue;
        }
        // The node processor.
        if tags::PALE_OAK_LOGS.has(level.block(current)) {
            for dir in shuffled_directions(level) {
                let n = current.relative(dir);
                let mut neighbour = level.block(n);
                let opposite = dir.opposite();
                if neighbour.is_air() {
                    neighbour = ids::RESIN_CLUMP.default_state();
                } else if neighbour.is(ids::WATER) && neighbour.int("level") == Some(0) {
                    neighbour = ids::RESIN_CLUMP
                        .default_state()
                        .set_flag("waterlogged", true);
                }
                let face = util::DirExt::name(opposite);
                if neighbour.is(ids::RESIN_CLUMP) && !neighbour.flag(face) {
                    level.set_block_and_update(n, neighbour.set_flag(face, true));
                    return Some(n);
                }
            }
        }
        count += 1;
        if count >= 64 {
            return None;
        }
        if depth < 2 {
            // The neighbour provider: the logs around, in a shuffled order.
            for dir in shuffled_directions(level) {
                let n = current.relative(dir);
                if tags::PALE_OAK_LOGS.has(level.block(n)) {
                    nodes.push_back((n, depth + 1));
                }
            }
        }
    }
    None
}

/// `emitParticles`: trail particles between the creaking and the heart (orange towards the
/// creaking, grey towards the heart).
fn emit_particles(level: &mut Level, pos: BlockPos, count: i32, towards_creaking: bool) {
    let Some(id) = protector(level, pos) else {
        return;
    };
    let Some(b) = creaking_box(level, id) else {
        return;
    };
    let color = if towards_creaking {
        CREAKING_ORANGE
    } else {
        CREAKING_GRAY
    };
    for _ in 0..count {
        let r = level.random();
        let mut source = Vec3::new(
            b.min_x + r.next_double() * (b.max_x - b.min_x),
            b.min_y + r.next_double() * (b.max_y - b.min_y),
            b.min_z + r.next_double() * (b.max_z - b.min_z),
        );
        let mut destination = Vec3::new(
            f64::from(pos.x) + r.next_double(),
            f64::from(pos.y) + r.next_double(),
            f64::from(pos.z) + r.next_double(),
        );
        if towards_creaking {
            std::mem::swap(&mut source, &mut destination);
        }
        let duration = r.next_int_bounded(40) + 10;
        // `TrailParticleOption(target, color, duration)`.
        let mut options = Vec::with_capacity(32);
        for v in [destination.x, destination.y, destination.z] {
            options.extend_from_slice(&v.to_be_bytes());
        }
        options.extend_from_slice(&color.to_be_bytes());
        vanillite_protocol::codec::write_varint(&mut options, duration);
        let particle = crate::level::particles::Particle {
            kind: i32::from(
                vanillite_data::registry::PARTICLE_TYPE
                    .id("minecraft:trail")
                    .unwrap_or(0),
            ),
            options,
        };
        let mut packet = crate::level::particles::packet(particle, source, 1, Vec3::ZERO, 0.0);
        packet.override_limiter = true;
        packet.always_show = true;
        level.send_particles_packet(packet);
    }
}

/// `removeProtector(damageSource)`: without a source the creaking crumbles at once
/// (`tearDown`); with one it dies (`creakingDeathEffects`) and twitches until it crumbles.
pub fn remove_protector(level: &mut Level, pos: BlockPos, death: Option<HeartDeath>) {
    let Some(id) = protector(level, pos) else {
        return;
    };
    match death {
        None => tear_down(level, id),
        Some(death) => {
            if let Some(m) = level.entities.mob_mut(id) {
                let c = creaking::creaking_mut(m);
                c.pending_death = Some(death);
                c.tearing_down = true;
                m.health = 0.0;
            }
            if let Some(mut e) = level.entities.store.get_mut(id) {
                e.mark_data_dirty();
            }
        }
    }
    if let Some(h) = heart_mut(level, pos) {
        h.creaking = None;
    }
    level.block_entities.touch(pos.chunk());
}

/// `Creaking.tearDown` from the level: the death sound, then the creaking is discarded.
fn tear_down(level: &mut Level, id: EntityId) {
    if let Some(sound) = level.entities.mob(id).map(crate::mob::kinds::death_sound) {
        creaking_sound(level, id, sound);
    }
    level.entities.despawn(id);
}

/// `Mob.makeSound(sound)` of a creaking from the level: the mob's voice pitch.
fn creaking_sound(level: &mut Level, id: EntityId, sound: &str) {
    let Some(pos) = level.entities.store.get(id).map(|e| e.pos()) else {
        return;
    };
    let Some(m) = level.entities.mob_mut(id) else {
        return;
    };
    if m.silent {
        return;
    }
    let volume = crate::mob::fam::mob_sound_volume(m);
    let a = m.random.next_float();
    let b = m.random.next_float();
    let pitch = (a - b) * 0.2 + if m.baby { 1.5 } else { 1.0 };
    let source = m.kind.sound_source() as u8;
    let name = sound.strip_prefix("minecraft:").unwrap_or(sound);
    level.play_sound(
        None,
        pos.x,
        pos.y,
        pos.z,
        sound_id(name),
        source,
        volume,
        pitch,
    );
}

/// `CreakingHeartBlock.tryAwardExperience`: 20 to 24 experience for a natural heart, unless
/// the player prevents drops or spectates.
fn try_award_experience(level: &mut Level, state: BlockState, pos: BlockPos, prevents: bool) {
    if prevents || !state.flag("natural") {
        return;
    }
    let amount = level.random().next_int_between_inclusive(20, 24);
    if level.settings.block_drops {
        level.award_experience(super::spawner::center(pos), amount);
    }
}

/// `CreakingHeartBlock.playerWillDestroy`: the creaking dies by the player's attack.
/// `prevents_drops` is `preventsBlockDrops() || isSpectator()`.
pub fn player_will_destroy(
    level: &mut Level,
    pos: BlockPos,
    state: BlockState,
    player: EntityId,
    prevents_drops: bool,
) {
    if heart(level, pos).is_none() {
        return;
    }
    remove_protector(level, pos, Some(HeartDeath::Player(player)));
    try_award_experience(level, state, pos, prevents_drops);
}

/// `CreakingHeartBlock.onExplosionHit` of a block breaking explosion: the creaking dies by the
/// explosion, a player who caused it gets the experience.
pub fn on_explosion_hit(
    level: &mut Level,
    pos: BlockPos,
    state: BlockState,
    source: Option<EntityId>,
    indirect: Option<EntityId>,
    center: Vec3,
) {
    if heart(level, pos).is_none() {
        return;
    }
    remove_protector(
        level,
        pos,
        Some(HeartDeath::Explosion {
            source,
            indirect,
            center,
        }),
    );
    if let Some(player) = indirect.and_then(|i| level.entities.player(i)) {
        let prevents = player.game_mode == vanillite_config::GameMode::Creative;
        try_award_experience(level, state, pos, prevents);
    }
}

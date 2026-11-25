//! Lightning: `ServerLevel.tickThunder` (where a strike lands) and the `LightningBolt` entity
//! (fire, lightning rods, copper, and the entities it hits).

use vanillite_base::random::{LegacyRandom, RandomSource};
use vanillite_base::{Aabb, BlockPos, ChunkPos, Vec3};
use vanillite_chunk::HeightmapKind;
use vanillite_data::entity::ids;
use vanillite_world::entity::{EntityId, NewEntity};
use vanillite_worldgen::behaviour::PosExt;

use crate::block::redstone::{is_lightning_rod, strike_lightning_rod};
use crate::block::{fire, place, weather};
use crate::entities::Entities;
use crate::level::Level;
use crate::level::game_event::{GameEventContext, events};
use crate::mob::{Kind, MobContext, combat};
use crate::survival::damage::DamageSource;
use crate::survival::player::{self, Env};
use crate::survival::rules::Difficulty;

/// `ServerLevel.tickThunder`: one strike in `1 / THUNDER_CHANCE` per spawning chunk and tick.
const THUNDER_CHANCE: i32 = 100_000;

/// `findLightningRod`: the search radius of the `lightning_rod` points of interest.
const ROD_RADIUS: i32 = 128;

/// `DimensionType.MOON_BRIGHTNESS_PER_PHASE`.
const MOON_BRIGHTNESS: [f32; 8] = [1.0, 0.75, 0.5, 0.25, 0.0, 0.25, 0.5, 0.75];

/// The kind data of a `lightning_bolt` (`LightningBolt`).
#[derive(Clone, Debug)]
pub struct LightningData {
    /// Ticks of the current flash left (`life`).
    pub life: i32,
    /// The seed of the bolt's shape on clients (`seed`).
    pub seed: i64,
    /// Flashes left after the current one.
    pub flashes: i32,
    /// Neither fire nor damage (the bolt of a skeleton trap).
    pub visual_only: bool,
    /// The entity's own random (`Entity.random`).
    pub random: LegacyRandom,
}

/// What a strike needs to know about the world besides the level.
#[derive(Clone, Copy, Debug)]
pub struct StrikeInput {
    /// The `spawn_mobs` game rule: strikes may be skeleton traps.
    pub spawn_mobs: bool,
    /// `getOverworldClockTime`: the overworld clock's ticks (local difficulty, moon phase).
    pub clock_time: i64,
}

/// `ServerLevel.tickThunder(chunk)` of one spawning chunk in entity ticking range.
pub fn tick_thunder(level: &mut Level, chunk: ChunkPos, input: StrikeInput) -> Option<EntityId> {
    let raining = level.is_raining();
    if !(raining && level.is_thundering() && level.random().next_int_bounded(THUNDER_CHANCE) == 0) {
        return None;
    }
    let column = level.block_random_pos(chunk.x * 16, 0, chunk.z * 16, 15);
    strike(level, column, input)
}

/// The strike of `tickThunder` in the column of `column` after its chance roll: the target
/// (`findLightningTargetAround`), `isRainingAt`, the skeleton trap and the bolt.
pub fn strike(level: &mut Level, column: BlockPos, input: StrikeInput) -> Option<EntityId> {
    let pos = find_target_around(level, column);
    if !level.is_raining_at(pos) {
        return None;
    }
    let trap = input.spawn_mobs
        && level.random().next_double()
            < f64::from(effective_difficulty(level, pos, input.clock_time)) * 0.01
        && !is_lightning_rod(level.block(pos.below()));
    if trap {
        // `SkeletonHorse` (`setTrap(true)`, `setAge(0)`) at the block corner. The horse has no
        // behaviour in Vanillite yet: it only takes its place in the entity order.
        let _ = level.new_entity_random();
        let uuid = level.new_uuid();
        let network_id = level.network_ids().next();
        let at = Vec3::new(f64::from(pos.x), f64::from(pos.y), f64::from(pos.z));
        level.entities.store.spawn(
            NewEntity::new(ids::SKELETON_HORSE, network_id, uuid, at),
            |_| 0,
        );
    }
    let at = Vec3::new(
        f64::from(pos.x) + 0.5,
        f64::from(pos.y),
        f64::from(pos.z) + 0.5,
    );
    Some(spawn(level, at, trap))
}

/// Adds a lightning bolt at `pos` (`EntityTypes.LIGHTNING_BOLT.create`): it draws its seed and
/// flashes from its own random.
pub fn spawn(level: &mut Level, pos: Vec3, visual_only: bool) -> EntityId {
    let mut random = level.new_entity_random();
    let seed = random.next_long();
    let flashes = random.next_int_bounded(3) + 1;
    let uuid = level.new_uuid();
    let network_id = level.network_ids().next();
    level.entities.spawn_bolt(
        NewEntity::new(ids::LIGHTNING_BOLT, network_id, uuid, pos),
        LightningData {
            life: 2,
            seed,
            flashes,
            visual_only,
            random,
        },
    )
}

/// `ServerLevel.findLightningTargetAround`: the nearest lightning rod on top of its column, else
/// a random living entity under the open sky above the column, else the column's top.
pub fn find_target_around(level: &mut Level, column: BlockPos) -> BlockPos {
    let mut center = BlockPos::new(
        column.x,
        level.heightmap_y(HeightmapKind::MotionBlocking, column),
        column.z,
    );
    if let Some(rod) = find_lightning_rod(level, center) {
        return rod.above();
    }
    // `AABB.encapsulatingFullBlocks(center, center.atY(maxY + 1)).inflate(3)`.
    let area = Aabb::new(
        f64::from(center.x) - 3.0,
        f64::from(center.y) - 3.0,
        f64::from(center.z) - 3.0,
        f64::from(center.x) + 4.0,
        f64::from(level.max_y()) + 2.0 + 3.0,
        f64::from(center.z) + 4.0,
    );
    let mut found = Vec::new();
    level
        .entities
        .store
        .for_each_in(&area, false, |id| found.push(id));
    found.retain(|&id| {
        living_alive(&level.entities, id)
            && level
                .entities
                .store
                .get(id)
                .is_some_and(|e| level.light(block_position(e.pos())).0 >= 15)
    });
    if !found.is_empty() {
        let pick = level
            .random()
            .next_int_bounded(i32::try_from(found.len()).unwrap_or(i32::MAX));
        let id = found[usize::try_from(pick).unwrap_or(0)];
        if let Some(e) = level.entities.store.get(id) {
            return block_position(e.pos());
        }
    }
    if center.y == level.min_y() - 1 {
        center = center.above().above();
    }
    center
}

/// `findLightningRod`: the closest lightning rod point of interest within 128 blocks that is
/// the top block of its column (`WORLD_SURFACE`). A rod can only pass that test on top of its
/// column, so the columns' tops of the loaded chunks are searched; ties go to the first in
/// Vanilla's chunk order (`ChunkPos.rangeClosed`: x fastest).
fn find_lightning_rod(level: &Level, center: BlockPos) -> Option<BlockPos> {
    let chunk_radius = ROD_RADIUS.div_euclid(16) + 1;
    let c = center.chunk();
    let mut best: Option<(i64, BlockPos)> = None;
    for cz in c.z - chunk_radius..=c.z + chunk_radius {
        for cx in c.x - chunk_radius..=c.x + chunk_radius {
            let Some(chunk) = level.chunks.chunk(ChunkPos::new(cx, cz)) else {
                continue;
            };
            let Some(heights) = chunk.chunk.heightmaps().get(HeightmapKind::WorldSurface) else {
                continue;
            };
            let min_y = chunk.chunk.min_y();
            for z in 0..16u32 {
                for x in 0..16u32 {
                    #[allow(clippy::cast_possible_wrap, clippy::as_conversions)]
                    let pos = BlockPos::new(
                        cx * 16 + x as i32,
                        heights.get(x, z) as i32 + min_y - 1,
                        cz * 16 + z as i32,
                    );
                    let (dx, dy, dz) = (
                        i64::from(pos.x - center.x),
                        i64::from(pos.y - center.y),
                        i64::from(pos.z - center.z),
                    );
                    let radius = i64::from(ROD_RADIUS);
                    if dx.abs() > radius || dz.abs() > radius {
                        continue;
                    }
                    let distance = dx * dx + dy * dy + dz * dz;
                    if distance > radius * radius
                        || best.is_some_and(|(d, _)| d <= distance)
                        || !is_lightning_rod(level.block(pos))
                    {
                        continue;
                    }
                    best = Some((distance, pos));
                }
            }
        }
    }
    best.map(|(_, pos)| pos)
}

/// `getCurrentDifficultyAt(pos).getEffectiveDifficulty()`.
fn effective_difficulty(level: &Level, pos: BlockPos, clock_time: i64) -> f32 {
    let (inhabited, moon) = level.chunks.chunk(pos.chunk()).map_or((0, 0.0), |c| {
        let phase = usize::try_from(clock_time.rem_euclid(8 * 24_000) / 24_000).unwrap_or(0);
        (c.inhabited_time, MOON_BRIGHTNESS[phase])
    });
    let base = level.settings.difficulty;
    if base == Difficulty::Peaceful {
        return 0.0;
    }
    let hard = base == Difficulty::Hard;
    #[allow(clippy::cast_precision_loss, clippy::as_conversions)]
    let global = ((clock_time as f32 + -72_000.0) / 1_440_000.0).clamp(0.0, 1.0) * 0.25;
    let mut scale = 0.75 + global;
    #[allow(clippy::cast_precision_loss, clippy::as_conversions)]
    let mut local =
        (inhabited as f32 / 3_600_000.0).clamp(0.0, 1.0) * if hard { 1.0 } else { 0.75 };
    local += (moon * 0.25).clamp(0.0, global);
    if base == Difficulty::Easy {
        local *= 0.5;
    }
    scale += local;
    #[allow(clippy::cast_precision_loss, clippy::as_conversions)]
    let id = base.id() as f32;
    id * scale
}

/// `Entity.blockPosition()`.
fn block_position(p: Vec3) -> BlockPos {
    BlockPos::containing(p.x, p.y, p.z)
}

/// `instanceof LivingEntity && isAlive()`.
fn living_alive(ents: &Entities, id: EntityId) -> bool {
    if let Some(p) = ents.player(id) {
        return p.survival.health > 0.0;
    }
    ents.mob(id).is_some_and(|m| m.is_alive())
}

/// `isAlive()` of the entities lightning can hit.
fn alive(ents: &Entities, id: EntityId) -> bool {
    if let Some(item) = ents.item(id) {
        return item.health > 0;
    }
    living_alive(ents, id) || ents.boat(id).is_some() || ents.minecart(id).is_some()
}

/// `LightningBolt.tick`. Fills `hits` with the entities to strike (`thunderHit`, see
/// [`thunder_hit`]) in Vanilla's order; the bolt removes itself when its last flash ends.
pub fn tick(level: &mut Level, id: EntityId, hits: &mut Vec<EntityId>) {
    hits.clear();
    let Some(pos) = level.entities.store.get(id).map(|e| e.pos()) else {
        return;
    };
    let Some(first) = level.entities.bolt_mut(id).map(|b| b.life == 2) else {
        return;
    };
    if first {
        if matches!(
            level.settings.difficulty,
            Difficulty::Normal | Difficulty::Hard
        ) {
            spawn_fire(level, id, pos, 4);
        }
        // `getStrikePosition`: the block the bolt stands on.
        let strike = BlockPos::containing(pos.x, pos.y - 1.0e-6, pos.z);
        strike_lightning_rod(level, strike);
        clear_copper(level, strike);
        level.game_event(
            events().lightning_strike,
            pos,
            GameEventContext::of(Some(id)),
        );
    }
    let Some(bolt) = level.entities.bolt_mut(id) else {
        return;
    };
    bolt.life -= 1;
    if bolt.life < 0 {
        if bolt.flashes == 0 {
            level.entities.despawn(id);
            return;
        }
        if bolt.life < -bolt.random.next_int_bounded(10) {
            bolt.flashes -= 1;
            bolt.life = 1;
            bolt.seed = bolt.random.next_long();
            spawn_fire(level, id, pos, 0);
        }
    }
    let Some(bolt) = level.entities.bolt_mut(id) else {
        return;
    };
    if bolt.life < 0 || bolt.visual_only {
        return;
    }
    let area = Aabb::new(
        pos.x - 3.0,
        pos.y - 3.0,
        pos.z - 3.0,
        pos.x + 3.0,
        pos.y + 6.0 + 3.0,
        pos.z + 3.0,
    );
    let ents = &level.entities;
    ents.store.for_each_in(&area, false, |other| {
        if other != id && alive(ents, other) {
            hits.push(other);
        }
    });
}

/// `LightningBolt.spawnFire(additionalSources)`: fire at the bolt and at random blocks around
/// it, drawn from the bolt's random.
fn spawn_fire(level: &mut Level, id: EntityId, pos: Vec3, additional: i32) {
    let Some(mut random) = level
        .entities
        .bolt_mut(id)
        .filter(|b| !b.visual_only)
        .map(|b| b.random.clone())
    else {
        return;
    };
    let at = block_position(pos);
    if !fire::can_spread_fire_around(level, at) {
        return;
    }
    let place_fire = |level: &mut Level, target: BlockPos| {
        let state = place::fire_state(&*level, target);
        if level.block(target).is_air() && level.can_survive(state, target) {
            level.set_block_and_update(target, state);
        }
    };
    place_fire(level, at);
    for _ in 0..additional {
        let x = random.next_int_bounded(3) - 1;
        let y = random.next_int_bounded(3) - 1;
        let z = random.next_int_bounded(3) - 1;
        place_fire(level, at.offset(x, y, z));
    }
    if let Some(bolt) = level.entities.bolt_mut(id) {
        bolt.random = random;
    }
}

/// `LightningBolt.clearCopperOnLightningStrike`: struck copper loses its oxidation, and random
/// walks from it (level random) scrape one stage off the copper they reach.
fn clear_copper(level: &mut Level, struck: BlockPos) {
    let block = level.block(struck).block();
    let weathering = weather::is_weathering_copper(block);
    if !weathering && !weather::is_waxed_copper(block) {
        return;
    }
    if weathering && let Some(first) = weather::copper_first(level.block(struck)) {
        level.set_block_and_update(struck, first);
    }
    let strikes = level.random().next_int_bounded(3) + 3;
    for _ in 0..strikes {
        let steps = level.random().next_int_bounded(8) + 1;
        let mut work = struck;
        for _ in 0..steps {
            match random_step_cleaning_copper(level, work) {
                Some(next) => work = next,
                None => break,
            }
        }
    }
}

/// `randomStepCleaningCopper`: up to 10 random blocks of the cube around `pos`
/// (`BlockPos.randomInCube`); the first copper one goes back one stage.
fn random_step_cleaning_copper(level: &mut Level, pos: BlockPos) -> Option<BlockPos> {
    for _ in 0..10 {
        let x = level.random().next_int_bounded(3) - 1;
        let y = level.random().next_int_bounded(3) - 1;
        let z = level.random().next_int_bounded(3) - 1;
        let candidate = pos.offset(x, y, z);
        let state = level.block(candidate);
        if weather::is_weathering_copper(state.block()) {
            if let Some(previous) = weather::copper_previous(state) {
                level.set_block_and_update(candidate, previous);
            }
            level.level_event(None, 3002, candidate, -1);
            return Some(candidate);
        }
    }
    None
}

/// `Entity.thunderHit` and its overrides: one more fire tick (eight seconds of fire when that
/// reached zero) and 5 lightning damage; creepers become charged.
///
/// Pigs and villagers would convert (zombified piglin, witch) and mooshrooms, turtles and
/// copper golems react on their own once those mobs exist; until then pigs take the plain hit.
pub fn thunder_hit(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    cx: &mut MobContext,
    target: EntityId,
) {
    if ents.player(target).is_some() {
        let Entities { store, players, .. } = ents;
        if let Some(mut entity) = store.get_mut(target) {
            let row = entity.as_ref().kind_row();
            if let Some(data) = players.get_mut(row) {
                player::thunder_hit(&mut entity, data, env);
            }
        }
        return;
    }
    let source = DamageSource::of(env.types.lightning_bolt);
    if let Some(d) = ents.item_mut(target) {
        d.health -= 5;
        if d.health <= 0 {
            cx.discarded.push(target);
        }
        return;
    }
    if ents.minecart(target).is_some() {
        crate::minecart::hurt(ents, env, cx, target, source, 5.0);
        return;
    }
    if ents.boat(target).is_some() {
        crate::vehicle::hurt_boat(ents, env, cx, target, source, 5.0);
        return;
    }
    let Some(m) = ents.mob_mut(target) else {
        return;
    };
    if !m.is_alive() {
        return;
    }
    m.fire_ticks += 1;
    if m.fire_ticks == 0 {
        m.fire_ticks = 160;
    }
    let creeper = m.kind == Kind::Creeper;
    if let Some(mut e) = ents.store.get_mut(target) {
        e.mark_data_dirty();
    }
    combat::hurt(level, ents, env, cx, target, source, 5.0);
    if creeper && let Some(m) = ents.mob_mut(target) {
        m.powered = true;
    }
}

//! `WanderingTraderSpawner`: every minute the spawn delay counts down; when it is over, a
//! trader with two llamas may appear near a random player's village bell (or the player).
//! Its delay and chance are the `minecraft:wandering_trader` saved data.

use vanillite_base::random::{LegacyRandom, RandomSource};
use vanillite_base::{BlockPos, Vec3};
use vanillite_data::entity::ids;
use vanillite_nbt::{Compound, Tag};

use super::super::state_mut;
use crate::level::Level;
use crate::mob::fam::SpawnReason;
use crate::mob::spawn::{self, SpawnTable};
use crate::mob::{Environment, Kind, MobData};
use crate::survival::rules::Difficulty;

/// `WanderingTraderData` (saved as `minecraft:wandering_trader`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TraderData {
    pub spawn_delay: i32,
    pub spawn_chance: i32,
}

impl Default for TraderData {
    fn default() -> Self {
        TraderData {
            spawn_delay: 24000,
            spawn_chance: 25,
        }
    }
}

impl TraderData {
    /// The saved data (`spawn_delay`, `spawn_chance`).
    pub fn save(&self) -> Compound {
        Compound::new()
            .with("spawn_delay", self.spawn_delay)
            .with("spawn_chance", self.spawn_chance)
    }

    /// Reads [`Self::save`]; missing fields take their defaults.
    pub fn load(tag: &Compound) -> TraderData {
        let int = |key: &str, default: i32| {
            tag.get(key)
                .and_then(Tag::as_i64)
                .and_then(|v| i32::try_from(v).ok())
                .unwrap_or(default)
        };
        TraderData {
            spawn_delay: int("spawn_delay", 24000),
            spawn_chance: int("spawn_chance", 25),
        }
    }
}

/// The state of a `WanderingTraderSpawner`.
#[derive(Clone, Debug)]
pub struct TraderSpawner {
    /// `RandomSource.create()`.
    pub random: LegacyRandom,
    pub tick_delay: i32,
    pub data: TraderData,
    /// The data changed since the last save (`setDirty`).
    pub dirty: bool,
}

impl TraderSpawner {
    /// A spawner with its saved data and a random seed.
    pub fn new(data: TraderData, seed: i64) -> TraderSpawner {
        TraderSpawner {
            random: LegacyRandom::new(seed),
            tick_delay: 1200,
            data,
            dirty: false,
        }
    }
}

/// What the spawner reads of the world this tick.
pub struct TraderInput<'a> {
    /// The `spawn_wandering_traders` game rule.
    pub enabled: bool,
    pub difficulty: Difficulty,
    pub env: Environment,
    pub table: Option<&'a SpawnTable>,
    /// The alive players (`getRandomPlayer`).
    pub players: &'a [Vec3],
}

impl std::fmt::Debug for TraderInput<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TraderInput")
            .field("enabled", &self.enabled)
            .finish_non_exhaustive()
    }
}

/// `WanderingTraderSpawner.tick`.
pub fn tick(level: &mut Level, spawner: &mut TraderSpawner, input: &TraderInput<'_>) {
    if !input.enabled {
        return;
    }
    spawner.tick_delay -= 1;
    if spawner.tick_delay > 0 {
        return;
    }
    spawner.tick_delay = 1200;
    let delay = spawner.data.spawn_delay - 1200;
    set_delay(spawner, delay);
    if delay > 0 {
        return;
    }
    set_delay(spawner, 24000);
    let chance = spawner.data.spawn_chance;
    set_chance(spawner, (chance + 25).clamp(25, 75));
    if spawner.random.next_int_bounded(100) <= chance && spawn(level, spawner, input) {
        set_chance(spawner, 25);
    }
}

fn set_delay(s: &mut TraderSpawner, v: i32) {
    if s.data.spawn_delay != v {
        s.data.spawn_delay = v;
        s.dirty = true;
    }
}

fn set_chance(s: &mut TraderSpawner, v: i32) {
    if s.data.spawn_chance != v {
        s.data.spawn_chance = v;
        s.dirty = true;
    }
}

/// `WanderingTraderSpawner.spawn`: `true` also without players (the chance resets).
fn spawn(level: &mut Level, s: &mut TraderSpawner, input: &TraderInput<'_>) -> bool {
    // `getRandomPlayer`: the level's random.
    if input.players.is_empty() {
        return true;
    }
    let n = i32::try_from(input.players.len()).unwrap_or(i32::MAX);
    let pick = usize::try_from(level.random().next_int_bounded(n)).unwrap_or(0);
    let p = input.players[pick];
    if s.random.next_int_bounded(10) != 0 {
        return false;
    }
    let player_pos = BlockPos::containing(p.x, p.y, p.z);
    level.flush_poi_changes();
    let reference = level
        .entities
        .poi
        .find(
            |t| t == crate::poi::PoiType::Meeting,
            |_| true,
            player_pos,
            48,
            crate::poi::Occupancy::Any,
        )
        .unwrap_or(player_pos);
    let Some(pos) = find_spawn_position(level, s, reference, 48) else {
        return false;
    };
    if !has_enough_space(level, pos) {
        return false;
    }
    let biome = input.table.and_then(|t| t.biome(level.biome(pos)));
    if biome.is_some_and(|b| b.is("minecraft:without_wandering_trader_spawns")) {
        return false;
    }
    let mut trader = MobData::new(Kind::WanderingTrader, crate::mob::new_entity_seed());
    let trader_id = spawn_event(level, input, &mut trader, pos);
    for _ in 0..2 {
        let at = level.entities.store.get(trader_id).map_or(pos, |e| {
            let p = e.pos();
            BlockPos::containing(p.x, p.y, p.z)
        });
        if let Some(lpos) = find_spawn_position(level, s, at, 4) {
            let mut llama = MobData::new(Kind::TraderLlama, crate::mob::new_entity_seed());
            let llama_id = spawn_event(level, input, &mut llama, lpos);
            // `setLeashedTo(trader, true)`.
            level.entities.leashes.insert(llama_id, trader_id);
        }
    }
    if let Some(m) = level.entities.mob_mut(trader_id) {
        let tr = &mut state_mut(m).trader;
        tr.despawn_delay = 48000;
        tr.wander_target = Some(reference);
        // `setHomeTo(reference, 16)`.
        m.restrict_center = reference;
        m.restrict_radius = 16;
    }
    true
}

/// `EntityType.spawn(level, pos, EVENT)`: centred on the block, a random yaw of the level,
/// `finalizeSpawn`, added.
fn spawn_event(
    level: &mut Level,
    input: &TraderInput<'_>,
    mob: &mut MobData,
    pos: BlockPos,
) -> vanillite_world::entity::EntityId {
    let at = Vec3::new(
        f64::from(pos.x) + 0.5,
        f64::from(pos.y),
        f64::from(pos.z) + 0.5,
    );
    let y_rot = wrap_degrees(level.random().next_float() * 360.0);
    let extra = spawn::with_level_random(level, |level, random| {
        spawn::finalize(
            level,
            input.table,
            &input.env,
            mob,
            pos,
            input.difficulty,
            0,
            SpawnReason::Event,
            random,
        )
    });
    let mob = std::mem::replace(mob, MobData::new(Kind::WanderingTrader, 0));
    spawn::add_mob_with(level, mob, at, y_rot, extra, SpawnReason::Event)
}

/// `Mth.wrapDegrees`.
fn wrap_degrees(d: f32) -> f32 {
    let mut d = d % 360.0;
    if d >= 180.0 {
        d -= 360.0;
    }
    if d < -180.0 {
        d += 360.0;
    }
    d
}

/// `findSpawnPositionNear(level, reference, radius)`: ten tries on the surface.
fn find_spawn_position(
    level: &Level,
    s: &mut TraderSpawner,
    reference: BlockPos,
    radius: i32,
) -> Option<BlockPos> {
    let (_, heightmap) = spawn::placement(ids::WANDERING_TRADER);
    for _ in 0..10 {
        let x = reference.x + s.random.next_int_bounded(radius * 2) - radius;
        let z = reference.z + s.random.next_int_bounded(radius * 2) - radius;
        let y = spawn::heightmap_y(level, heightmap, x, z);
        let pos = BlockPos::new(x, y, z);
        if spawn::spawn_position_ok(level, ids::WANDERING_TRADER, pos) {
            return Some(pos);
        }
    }
    None
}

/// `hasEnoughSpace`: no collision in the 2 x 3 x 2 blocks from the position.
fn has_enough_space(level: &Level, pos: BlockPos) -> bool {
    for x in pos.x..=pos.x + 1 {
        for y in pos.y..=pos.y + 2 {
            for z in pos.z..=pos.z + 1 {
                if !level
                    .block(BlockPos::new(x, y, z))
                    .collision_shape()
                    .is_empty()
                {
                    return false;
                }
            }
        }
    }
    true
}

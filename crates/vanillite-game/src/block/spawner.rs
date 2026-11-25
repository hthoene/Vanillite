//! Mob spawners (`SpawnerBlockEntity` with `BaseSpawner`) and what trial spawners share with
//! them: the spawn data (`SpawnData` with its custom spawn rules), weighted spawn potentials,
//! the player-in-range check and creating a mob from spawn data.
//!
//! Spawn data is kept as its NBT (`SpawnData.CODEC`) so stored spawners round-trip unchanged;
//! only entity types with a [`Kind`] can be spawned, other types make the spawner wait for its
//! next delay as if the entity could not be created.

use vanillite_base::random::{LegacyRandom, RandomSource};
use vanillite_base::{Aabb, BlockPos, Vec3};
use vanillite_data::block::{BlockId, BlockState, ids};
use vanillite_data::entity::EntityTypeId;
use vanillite_nbt::{Compound, List, Tag};
use vanillite_world::entity::{Dimensions, EntityId};

use super::BlockBehaviour;
use crate::level::Level;
use crate::level::block_entity::BlockEntity;
use crate::level::game_event::{GameEventContext, events};
use crate::mob::spawn::{self, Category};
use crate::mob::{Environment, Kind, MobData, kinds, physics};
use crate::survival::rules::Difficulty;

/// What spawner blocks read of the world's rules and sky this tick (the world sets it before
/// block entities tick).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpawnerEnv {
    /// Sky darkening, weather and the dimension's monster light rules.
    pub env: Environment,
    pub difficulty: Difficulty,
    /// `spawner_blocks_work`.
    pub spawner_blocks_work: bool,
    /// `spawn_mobs`.
    pub spawn_mobs: bool,
}

impl Default for SpawnerEnv {
    fn default() -> Self {
        SpawnerEnv {
            env: Environment::default(),
            difficulty: Difficulty::Normal,
            spawner_blocks_work: true,
            spawn_mobs: true,
        }
    }
}

/// `SpawnData`: the entity to spawn (`entity`), optional `custom_spawn_rules` and `equipment`,
/// kept as its NBT.
#[derive(Clone, Debug, PartialEq)]
pub struct SpawnData(pub Compound);

impl Default for SpawnData {
    /// `new SpawnData()`: an empty entity.
    fn default() -> Self {
        let mut c = Compound::new();
        c.insert("entity", Compound::new());
        SpawnData(c)
    }
}

/// `SpawnData.CustomSpawnRules`: inclusive light ranges.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CustomSpawnRules {
    pub block_light: (i32, i32),
    pub sky_light: (i32, i32),
}

/// `InclusiveRange.INT` (a single int or `min_inclusive`/`max_inclusive`), `0..=15` if absent.
fn light_range(tag: Option<&Tag>) -> (i32, i32) {
    let int = |t: Option<&Tag>| t.and_then(Tag::as_i64).and_then(|v| i32::try_from(v).ok());
    match tag {
        Some(Tag::Compound(c)) => {
            match (int(c.get("min_inclusive")), int(c.get("max_inclusive"))) {
                (Some(lo), Some(hi)) => (lo, hi),
                _ => (0, 15),
            }
        }
        Some(t) => int(Some(t)).map_or((0, 15), |v| (v, v)),
        None => (0, 15),
    }
}

impl SpawnData {
    /// Spawn data from its NBT; a missing `entity` becomes an empty one.
    pub fn from_nbt(mut c: Compound) -> SpawnData {
        if !matches!(c.get("entity"), Some(Tag::Compound(_))) {
            c.insert("entity", Compound::new());
        }
        SpawnData(c)
    }

    /// Spawn data of an entity type only (`withSpawning(type)`).
    pub fn of_type(name: &str) -> SpawnData {
        let mut entity = Compound::new();
        entity.insert("id", Tag::String(name.to_owned()));
        let mut c = Compound::new();
        c.insert("entity", entity);
        SpawnData(c)
    }

    /// `getEntityToSpawn`.
    pub fn entity(&self) -> Option<&Compound> {
        self.0.get("entity").and_then(Tag::as_compound)
    }

    /// The `id` of the entity to spawn.
    pub fn entity_name(&self) -> Option<&str> {
        self.entity()?.get("id").and_then(Tag::as_str)
    }

    /// The entity type to spawn (`EntityType.by`).
    pub fn entity_type(&self) -> Option<EntityTypeId> {
        crate::mob::nbt::type_of(self.entity()?)
    }

    /// `getCustomSpawnRules`.
    pub fn custom_spawn_rules(&self) -> Option<CustomSpawnRules> {
        let c = self.0.get("custom_spawn_rules")?.as_compound()?;
        Some(CustomSpawnRules {
            block_light: light_range(c.get("block_light_limit")),
            sky_light: light_range(c.get("sky_light_limit")),
        })
    }

    /// The entity compound holds only its `id`: the mob gets `finalizeSpawn`.
    pub fn has_no_configuration(&self) -> bool {
        self.entity()
            .is_some_and(|e| e.len() == 1 && e.get("id").and_then(Tag::as_str).is_some())
    }
}

/// `CustomSpawnRules.isValidPosition`: block light and effective sky light within the ranges.
pub fn custom_rules_valid(level: &Level, rules: CustomSpawnRules, pos: BlockPos) -> bool {
    let (sky, block) = level.light(pos);
    let sky = sky - level.spawner_env.env.sky_darken;
    let within = |(lo, hi): (i32, i32), v: i32| (lo..=hi).contains(&v);
    within(rules.block_light, block) && within(rules.sky_light, sky)
}

/// `WeightedList<SpawnData>`: entries with their weights.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SpawnPotentials(pub Vec<(SpawnData, i32)>);

impl SpawnPotentials {
    /// `isEmpty`.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// `getRandom(random)`: `nextInt(totalWeight)` picks an entry; empty for no weight.
    pub fn get_random(&self, random: &mut impl RandomSource) -> Option<&SpawnData> {
        let total: i32 = self.0.iter().map(|(_, w)| *w).sum();
        if total <= 0 {
            return None;
        }
        let mut roll = random.next_int_bounded(total);
        for (data, weight) in &self.0 {
            if roll < *weight {
                return Some(data);
            }
            roll -= weight;
        }
        None
    }

    /// `SpawnData.LIST_CODEC` from NBT: `[{data, weight}]`.
    pub fn from_nbt(tag: Option<&Tag>) -> Option<SpawnPotentials> {
        let Some(Tag::List(list)) = tag else {
            return None;
        };
        let mut out = Vec::new();
        for t in &list.0 {
            let Some(c) = t.as_compound() else {
                continue;
            };
            let Some(data) = c.get("data").and_then(Tag::as_compound) else {
                continue;
            };
            let weight = c
                .get("weight")
                .and_then(Tag::as_i64)
                .and_then(|v| i32::try_from(v).ok())
                .unwrap_or(1);
            if weight > 0 {
                out.push((SpawnData::from_nbt(data.clone()), weight));
            }
        }
        Some(SpawnPotentials(out))
    }

    /// The list as NBT.
    pub fn to_nbt(&self) -> Tag {
        Tag::List(List(
            self.0
                .iter()
                .map(|(d, w)| {
                    let mut c = Compound::new();
                    c.insert("data", d.0.clone());
                    c.insert("weight", *w);
                    Tag::Compound(c)
                })
                .collect(),
        ))
    }
}

/// `Level.hasNearbyAlivePlayer(x, y, z, range)`: a living non-spectator player closer than
/// `range` (any distance for a negative range).
pub fn has_nearby_alive_player(level: &Level, pos: Vec3, range: f64) -> bool {
    level.entities.players.iter().any(|(id, p)| {
        if p.game_mode == vanillite_config::GameMode::Spectator || p.survival.is_dead_or_dying() {
            return false;
        }
        let Some(e) = level.entities.store.get(id) else {
            return false;
        };
        range < 0.0 || e.pos().distance_to_sqr(pos) < range * range
    })
}

/// The block's centre (`Vec3.atCenterOf`).
pub fn center(pos: BlockPos) -> Vec3 {
    Vec3::new(
        f64::from(pos.x) + 0.5,
        f64::from(pos.y) + 0.5,
        f64::from(pos.z) + 0.5,
    )
}

/// `Level.noCollision(type.getSpawnAABB(x, y, z))` against blocks.
pub fn no_collision_at(level: &Level, type_id: EntityTypeId, pos: Vec3) -> bool {
    let aabb = Dimensions::of(type_id).bounding_box(pos);
    let mut scratch = Vec::new();
    physics::no_collision(level, &aabb, &mut scratch)
}

/// `SpawnPlacements.checkSpawnRules(type, level, SPAWNER or TRIAL_SPAWNER, pos, random)`:
/// monsters need darkness (not for trial spawners), animals grass and light; the floor of
/// monsters is not checked for spawners.
pub fn spawner_spawn_rules(level: &mut Level, kind: Kind, pos: BlockPos, trial: bool) -> bool {
    let env = level.spawner_env.env;
    let mut random = level.random().clone();
    let ok = if kind.is_monster() {
        trial || spawn::is_dark_enough_to_spawn(level, &env, pos, &mut random)
    } else {
        spawn::check_spawn_rules(
            level,
            &env,
            kind,
            pos,
            &mut random,
            if trial {
                crate::mob::fam::SpawnReason::TrialSpawner
            } else {
                crate::mob::fam::SpawnReason::Spawner
            },
        )
    };
    *level.random() = random;
    ok
}

/// Creates the mob of `data` at `pos` (`EntityType.loadEntityRecursive`): a fresh mob for an
/// entity with only its id, else the mob read from the entity NBT. `None` for types without
/// mob behaviour or broken data.
pub fn create_mob(level: &mut Level, data: &SpawnData, pos: Vec3) -> Option<(MobData, Vec3)> {
    let kind = Kind::of(data.entity_type()?)?;
    #[allow(clippy::cast_possible_truncation)]
    let seed = level.new_uuid() as i64;
    let entity = data.entity()?;
    if entity.len() <= 1 {
        return Some((MobData::new(kind, seed), pos));
    }
    let mut tag = entity.clone();
    if tag.get("Pos").is_none() {
        tag.insert(
            "Pos",
            List(vec![
                Tag::Double(pos.x),
                Tag::Double(pos.y),
                Tag::Double(pos.z),
            ]),
        );
    }
    let loaded = crate::mob::nbt::load_mob(&tag, seed)?;
    Some((loaded.data, loaded.pos))
}

/// `Mob.checkSpawnRules(level, SPAWNER)` (the walk target value is not negative) and
/// `checkSpawnObstruction` (no liquid in the box).
pub fn mob_spawn_checks(level: &Level, kind: Kind, pos: Vec3, check_rules: bool) -> bool {
    let block = BlockPos::containing(pos.x, pos.y, pos.z);
    if check_rules && kinds::walk_target_value(kind, level, &level.spawner_env.env, block) < 0.0 {
        return false;
    }
    let aabb = Dimensions::of(kind.type_id()).bounding_box(pos);
    !physics::contains_any_liquid(level, &aabb)
}

/// `level.gameEvent(entity, GameEvent.ENTITY_PLACE, pos)`.
pub fn entity_place_event(level: &mut Level, entity: EntityId, pos: BlockPos) {
    level.game_event_block(
        events().entity_place,
        pos,
        GameEventContext::of(Some(entity)),
    );
}

/// `BaseSpawner` of a `SpawnerBlockEntity`.
#[derive(Clone, Debug, PartialEq)]
pub struct MobSpawner {
    /// `spawnDelay`: ticks until the next attempt, `-1` to draw a new delay first.
    pub spawn_delay: i32,
    pub spawn_potentials: SpawnPotentials,
    pub next_spawn_data: Option<SpawnData>,
    pub min_spawn_delay: i32,
    pub max_spawn_delay: i32,
    pub spawn_count: i32,
    pub max_nearby_entities: i32,
    pub required_player_range: i32,
    pub spawn_range: i32,
}

impl Default for MobSpawner {
    fn default() -> Self {
        MobSpawner {
            spawn_delay: 20,
            spawn_potentials: SpawnPotentials::default(),
            next_spawn_data: None,
            min_spawn_delay: 200,
            max_spawn_delay: 800,
            spawn_count: 4,
            max_nearby_entities: 6,
            required_player_range: 16,
            spawn_range: 4,
        }
    }
}

fn short(c: &Compound, key: &str, default: i32) -> i32 {
    c.get(key)
        .and_then(Tag::as_i64)
        .and_then(|v| i16::try_from(v).ok())
        .map_or(default, i32::from)
}

fn int(c: &Compound, key: &str, default: i32) -> i32 {
    c.get(key)
        .and_then(Tag::as_i64)
        .and_then(|v| i32::try_from(v).ok())
        .unwrap_or(default)
}

#[allow(clippy::cast_possible_truncation)]
fn to_short(v: i32) -> i16 {
    v as i16
}

impl MobSpawner {
    /// `BaseSpawner.load`.
    pub fn load(nbt: &Compound) -> MobSpawner {
        let next_spawn_data = nbt
            .get("SpawnData")
            .and_then(Tag::as_compound)
            .map(|c| SpawnData::from_nbt(c.clone()));
        let spawn_potentials = SpawnPotentials::from_nbt(nbt.get("SpawnPotentials"))
            .unwrap_or_else(|| {
                SpawnPotentials(vec![(next_spawn_data.clone().unwrap_or_default(), 1)])
            });
        MobSpawner {
            spawn_delay: short(nbt, "Delay", 20),
            spawn_potentials,
            next_spawn_data,
            min_spawn_delay: int(nbt, "MinSpawnDelay", 200),
            max_spawn_delay: int(nbt, "MaxSpawnDelay", 800),
            spawn_count: int(nbt, "SpawnCount", 4),
            max_nearby_entities: int(nbt, "MaxNearbyEntities", 6),
            required_player_range: int(nbt, "RequiredPlayerRange", 16),
            spawn_range: int(nbt, "SpawnRange", 4),
        }
    }

    /// `BaseSpawner.save`; `with_potentials` is false for the update tag.
    pub fn save(&self, out: &mut Compound, with_potentials: bool) {
        out.insert("Delay", to_short(self.spawn_delay));
        out.insert("MinSpawnDelay", to_short(self.min_spawn_delay));
        out.insert("MaxSpawnDelay", to_short(self.max_spawn_delay));
        out.insert("SpawnCount", to_short(self.spawn_count));
        out.insert("MaxNearbyEntities", to_short(self.max_nearby_entities));
        out.insert("RequiredPlayerRange", to_short(self.required_player_range));
        out.insert("SpawnRange", to_short(self.spawn_range));
        if let Some(d) = &self.next_spawn_data {
            out.insert("SpawnData", d.0.clone());
        }
        if with_potentials {
            out.insert("SpawnPotentials", self.spawn_potentials.to_nbt());
        }
    }

    /// `isNearPlayer`.
    fn is_near_player(&self, level: &Level, pos: BlockPos) -> bool {
        has_nearby_alive_player(level, center(pos), f64::from(self.required_player_range))
    }
}

fn spawner_mut(level: &mut Level, pos: BlockPos) -> Option<&mut MobSpawner> {
    match level.block_entity_mut(pos) {
        Some(BlockEntity::Spawner(s)) => Some(s),
        _ => None,
    }
}

/// `setNextSpawnData`: the clients see the new display entity.
fn set_next_spawn_data(level: &mut Level, pos: BlockPos, data: SpawnData) {
    if let Some(s) = spawner_mut(level, pos) {
        s.next_spawn_data = Some(data);
    }
    level.send_block_entity_updated(pos);
}

/// `getOrCreateNextSpawnData`.
fn next_spawn_data(level: &mut Level, pos: BlockPos) -> Option<SpawnData> {
    let s = spawner_mut(level, pos)?;
    if let Some(d) = &s.next_spawn_data {
        return Some(d.clone());
    }
    let potentials = s.spawn_potentials.clone();
    let data = potentials
        .get_random(level.random())
        .cloned()
        .unwrap_or_default();
    set_next_spawn_data(level, pos, data.clone());
    Some(data)
}

/// `BaseSpawner.delay`: a new delay, the next spawn data and the block event that resets the
/// clients' spin.
fn delay(level: &mut Level, pos: BlockPos) {
    let Some(s) = spawner_mut(level, pos) else {
        return;
    };
    let (min, max) = (s.min_spawn_delay, s.max_spawn_delay);
    let potentials = s.spawn_potentials.clone();
    let delay = if max <= min {
        min
    } else {
        min + level.random().next_int_bounded(max - min)
    };
    if let Some(s) = spawner_mut(level, pos) {
        s.spawn_delay = delay;
    }
    if let Some(data) = potentials.get_random(level.random()).cloned() {
        set_next_spawn_data(level, pos, data);
    }
    level.block_event(pos, ids::SPAWNER, 1, 0);
}

/// `SpawnerBlockEntity.serverTick` (`BaseSpawner.serverTick`).
pub fn tick(level: &mut Level, pos: BlockPos, _state: BlockState) {
    let Some(s) = spawner_mut(level, pos) else {
        return;
    };
    let s = s.clone();
    if !s.is_near_player(level, pos) || !level.spawner_env.spawner_blocks_work {
        return;
    }
    if s.spawn_delay == -1 {
        delay(level, pos);
    }
    let Some(spawner) = spawner_mut(level, pos) else {
        return;
    };
    if spawner.spawn_delay > 0 {
        spawner.spawn_delay -= 1;
        return;
    }
    let (spawn_count, spawn_range, max_nearby) =
        (s.spawn_count, s.spawn_range, s.max_nearby_entities);
    let mut delayed = false;
    let Some(data) = next_spawn_data(level, pos) else {
        return;
    };
    for _ in 0..spawn_count {
        let Some(type_id) = data.entity_type() else {
            delay(level, pos);
            return;
        };
        let spawn_pos =
            fixed_pos(&data).unwrap_or_else(|| random_spawn_pos(level.random(), pos, spawn_range));
        if !no_collision_at(level, type_id, spawn_pos) {
            continue;
        }
        let block = BlockPos::containing(spawn_pos.x, spawn_pos.y, spawn_pos.z);
        let custom = data.custom_spawn_rules();
        let kind = Kind::of(type_id);
        if let Some(rules) = custom {
            let friendly = Category::of(type_id).is_some_and(Category::is_friendly);
            if !friendly && level.spawner_env.difficulty == Difficulty::Peaceful {
                continue;
            }
            if !custom_rules_valid(level, rules, block) {
                continue;
            }
        } else if let Some(kind) = kind
            && !spawner_spawn_rules(level, kind, block, false)
        {
            continue;
        }
        let Some((mob, at)) = create_mob(level, &data, spawn_pos) else {
            delay(level, pos);
            return;
        };
        // Entities of the same type around the spawner.
        let range = f64::from(spawn_range);
        let area = Aabb::new(
            f64::from(pos.x) - range,
            f64::from(pos.y) - range,
            f64::from(pos.z) - range,
            f64::from(pos.x + 1) + range,
            f64::from(pos.y + 1) + range,
            f64::from(pos.z + 1) + range,
        );
        let mut nearby = 0;
        level.entities.store.for_each_in(&area, false, |id| {
            if level
                .entities
                .mob(id)
                .is_some_and(|m| m.kind.type_id() == type_id)
            {
                nearby += 1;
            }
        });
        if nearby >= max_nearby {
            delay(level, pos);
            return;
        }
        let y_rot = level.random().next_float() * 360.0;
        if !mob_spawn_checks(level, mob.kind, at, custom.is_none()) {
            continue;
        }
        let mut mob = mob;
        let extra = if data.has_no_configuration() {
            finalize_spawner_mob(level, &mut mob, at, crate::mob::fam::SpawnReason::Spawner)
        } else {
            Vec::new()
        };
        let id = spawn::add_mob_with(level, mob, at, y_rot, extra);
        level.level_event(None, 2004, pos, 0);
        entity_place_event(level, id, block);
        delayed = true;
    }
    if delayed {
        delay(level, pos);
    }
}

/// `SpawnerBlockEntity.triggerEvent`: event 1 (the delay reset) goes to the clients.
fn trigger_event(_: &mut Level, _: BlockState, _: BlockPos, a: i32, _: i32) -> bool {
    a == 1
}

/// The behaviour of the spawner block.
pub fn fill(block: BlockId, b: &mut BlockBehaviour) {
    if block == ids::SPAWNER {
        b.trigger_event = Some(trigger_event);
    }
}

/// `getUpdateTag`: the spawner without its potentials.
pub fn update_tag(s: &MobSpawner) -> Compound {
    let mut c = Compound::new();
    s.save(&mut c, false);
    c
}

/// A random spawn position used by both spawners: `pos + (d - d) * range + 0.5` horizontally
/// and `nextInt(3) - 1` vertically.
pub fn random_spawn_pos(random: &mut LegacyRandom, pos: BlockPos, range: i32) -> Vec3 {
    let range = f64::from(range);
    let x = f64::from(pos.x) + (random.next_double() - random.next_double()) * range + 0.5;
    let y = f64::from(pos.y + random.next_int_bounded(3) - 1);
    let z = f64::from(pos.z) + (random.next_double() - random.next_double()) * range + 0.5;
    Vec3::new(x, y, z)
}

/// The `Pos` of the entity of `data`, if it has one.
pub fn fixed_pos(data: &SpawnData) -> Option<Vec3> {
    match data.entity()?.get("Pos") {
        Some(Tag::List(l)) if l.0.len() == 3 => {
            let d = |i: usize| match l.0.get(i) {
                Some(Tag::Double(v)) => Some(*v),
                _ => None,
            };
            Some(Vec3::new(d(0)?, d(1)?, d(2)?))
        }
        _ => None,
    }
}

/// `finalizeSpawn(level, difficulty, reason, null)` of a spawner's mob with the level random;
/// returns the mobs that spawn with it (`tryAddFreshEntityWithPassengers`).
pub(crate) fn finalize_spawner_mob(
    level: &mut Level,
    mob: &mut crate::mob::MobData,
    at: vanillite_base::Vec3,
    reason: crate::mob::fam::SpawnReason,
) -> Vec<crate::mob::fam::ExtraMob> {
    let env = level.spawner_env.env;
    let difficulty = level.spawner_env.difficulty;
    let block = BlockPos::containing(at.x, at.y, at.z);
    spawn::with_level_random(level, |level, random| {
        spawn::finalize(level, None, &env, mob, block, difficulty, 0, reason, random)
    })
}

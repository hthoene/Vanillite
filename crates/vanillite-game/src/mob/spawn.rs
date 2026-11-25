//! Natural spawning (`NaturalSpawner`): mob caps per category, the spawn attempts per chunk
//! near players, the spawn rules of each mob, the animals of freshly generated chunks
//! (`spawnMobsForChunkGeneration`) and spawning a mob by command.

use vanillite_base::hash::{FxHashMap, FxHashSet};
use vanillite_base::random::{LegacyRandom, RandomSource};
use vanillite_base::{BlockPos, ChunkPos, Vec3};
use vanillite_chunk::HeightmapKind;
use vanillite_data::block::{BlockState, flags, ids};
use vanillite_data::entity::EntityTypeId;
use vanillite_world::entity::{EntityId, Rotation};

use super::kinds::{self, FarmClimate, Kind};
use super::physics;
use super::{Environment, MobData, max_local_raw_brightness, random_uuid};
use crate::level::Level;
use crate::survival::rules::Difficulty;

/// `MobCategory` without `MISC`, in Vanilla's order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Category {
    Monster,
    Creature,
    Ambient,
    Axolotls,
    UndergroundWaterCreature,
    WaterCreature,
    WaterAmbient,
}

impl Category {
    pub const ALL: [Category; 7] = [
        Category::Monster,
        Category::Creature,
        Category::Ambient,
        Category::Axolotls,
        Category::UndergroundWaterCreature,
        Category::WaterCreature,
        Category::WaterAmbient,
    ];

    /// The category of an entity type, `None` for `misc`.
    pub fn of(t: EntityTypeId) -> Option<Category> {
        Category::by_name(t.info().category)
    }

    /// The category by its name in data packs (`monster`, `creature`, …).
    pub fn by_name(name: &str) -> Option<Category> {
        Some(match name {
            "monster" => Category::Monster,
            "creature" => Category::Creature,
            "ambient" => Category::Ambient,
            "axolotls" => Category::Axolotls,
            "underground_water_creature" => Category::UndergroundWaterCreature,
            "water_creature" => Category::WaterCreature,
            "water_ambient" => Category::WaterAmbient,
            _ => return None,
        })
    }

    fn index(self) -> usize {
        self as usize
    }

    /// `getMaxInstancesPerChunk`.
    pub fn max_per_chunk(self) -> i32 {
        match self {
            Category::Monster => 70,
            Category::Creature => 10,
            Category::Ambient => 15,
            Category::Axolotls | Category::UndergroundWaterCreature | Category::WaterCreature => 5,
            Category::WaterAmbient => 20,
        }
    }

    /// `isFriendly`.
    pub fn is_friendly(self) -> bool {
        self != Category::Monster
    }

    /// `isPersistent`.
    pub fn is_persistent(self) -> bool {
        self == Category::Creature
    }

    /// `getDespawnDistance`.
    pub fn despawn_distance(self) -> i32 {
        if self == Category::WaterAmbient {
            64
        } else {
            128
        }
    }
}

/// One `SpawnerData` of a biome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpawnEntry {
    pub type_id: EntityTypeId,
    pub weight: i32,
    pub min: i32,
    pub max: i32,
}

/// The spawn settings of one biome.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BiomeSpawns {
    pub by_category: [Vec<SpawnEntry>; 7],
    /// `creature_world_gen_spawn_probability`.
    pub creature_probability: f32,
    pub climate: FarmClimate,
}

/// The spawn settings of every biome, by protocol id.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SpawnTable {
    pub biomes: Vec<BiomeSpawns>,
}

impl SpawnTable {
    fn biome(&self, id: u16) -> Option<&BiomeSpawns> {
        self.biomes.get(usize::from(id))
    }
}

/// `WeightedList.getRandom`.
fn pick(list: &[SpawnEntry], random: &mut LegacyRandom) -> Option<SpawnEntry> {
    let total: i32 = list.iter().map(|e| e.weight).sum();
    if total <= 0 {
        return None;
    }
    let mut roll = random.next_int_bounded(total);
    for e in list {
        roll -= e.weight;
        if roll < 0 {
            return Some(*e);
        }
    }
    None
}

/// What the spawner needs to know about the world besides the level.
#[derive(Clone, Copy, Debug)]
pub struct SpawnInput<'a> {
    /// Non-spectator players with their position.
    pub players: &'a [(EntityId, Vec3)],
    pub spawn_point: Option<BlockPos>,
    /// `spawnEnemies`: monsters may spawn (difficulty and `spawn_monsters`).
    pub spawn_enemies: bool,
    pub difficulty: Difficulty,
    pub env: Environment,
}

/// Per-player mob counts (`LocalMobCapCalculator.MobCounts`).
#[derive(Debug, Default)]
struct LocalCaps {
    counts: Vec<[i32; 7]>,
}

/// `NaturalSpawner.SpawnState` of one tick.
#[derive(Debug, Default)]
struct SpawnState {
    spawnable_chunks: i32,
    counts: [i32; 7],
    local: LocalCaps,
}

impl SpawnState {
    fn can_spawn_global(&self, c: Category) -> bool {
        let max = c.max_per_chunk() * self.spawnable_chunks / 289;
        self.counts[c.index()] < max
    }
}

fn chunk_center_distance_sqr(chunk: ChunkPos, pos: Vec3) -> f64 {
    let x = f64::from(chunk.x) * 16.0 + 8.0;
    let z = f64::from(chunk.z) * 16.0 + 8.0;
    let dx = x - pos.x;
    let dz = z - pos.z;
    dx * dx + dz * dz
}

/// The players close enough to `chunk` for spawning (`getPlayersCloseForSpawning`).
fn players_near(input: &SpawnInput<'_>, chunk: ChunkPos, out: &mut Vec<usize>) {
    out.clear();
    for (i, (_, p)) in input.players.iter().enumerate() {
        if chunk_center_distance_sqr(chunk, *p) < 16384.0 {
            out.push(i);
        }
    }
}

/// The reusable state of a world's spawner.
#[derive(Debug, Default)]
pub struct Spawner {
    chunks: Vec<ChunkPos>,
    seen: FxHashSet<ChunkPos>,
    near: Vec<usize>,
    /// Chunks that passed the spawn step and wait for their animals.
    pub generated: Vec<ChunkPos>,
    /// Structure spawn overrides (ocean monuments) by chunk.
    pub structures: StructureSpawns,
}

/// The emptied spawn lists of an ocean monument (axolotls, underground water creatures).
const NO_SPAWNS: &[SpawnEntry] = &[];

/// The structure starts with spawn overrides that chunks reference, read from the chunks'
/// stored structure data and cached per chunk (`StructureManager.getStructureAt` for the
/// `full` bounding boxes of ocean monuments).
#[derive(Debug, Default)]
pub struct StructureSpawns {
    cache: FxHashMap<ChunkPos, CachedChunk>,
}

/// The monuments a chunk references.
#[derive(Debug)]
struct CachedChunk {
    /// The chunk's stored data the boxes were read from.
    data: Option<std::sync::Arc<[u8]>>,
    /// Every referenced start chunk was available.
    complete: bool,
    /// The monuments' bounding boxes (`min_x, min_y, min_z, max_x, max_y, max_z`).
    boxes: Vec<[i32; 6]>,
}

/// Most chunks kept in [`StructureSpawns`] before it starts over.
const STRUCTURE_CACHE_LIMIT: usize = 4096;

fn chunk_tags(
    level: &Level,
    chunk: ChunkPos,
) -> Option<(std::sync::Arc<[u8]>, vanillite_nbt::Compound)> {
    let c = level.chunks.chunk(chunk)?;
    let bytes = c.extra.clone()?;
    let tag = vanillite_nbt::read::read_network(
        &mut &bytes[..],
        &mut vanillite_nbt::read::Budget::file(),
    )
    .ok()??;
    match tag {
        vanillite_nbt::Tag::Compound(c) => Some((bytes, c)),
        _ => None,
    }
}

impl StructureSpawns {
    /// Whether `pos` is inside the bounding box of an ocean monument its chunk references.
    pub fn in_monument(&mut self, level: &Level, pos: BlockPos) -> bool {
        let chunk = pos.chunk();
        let extra = level.chunks.chunk(chunk).and_then(|c| c.extra.clone());
        let fresh = match self.cache.get(&chunk) {
            Some(cached) => {
                cached.complete
                    && match (&cached.data, &extra) {
                        (Some(a), Some(b)) => std::sync::Arc::ptr_eq(a, b),
                        (None, None) => true,
                        _ => false,
                    }
            }
            None => false,
        };
        if !fresh {
            if self.cache.len() >= STRUCTURE_CACHE_LIMIT {
                self.cache.clear();
            }
            let (complete, boxes) = monument_boxes(level, chunk);
            self.cache.insert(
                chunk,
                CachedChunk {
                    data: extra,
                    complete,
                    boxes,
                },
            );
        }
        self.cache.get(&chunk).is_some_and(|c| {
            c.boxes.iter().any(|b| {
                (b[0]..=b[3]).contains(&pos.x)
                    && (b[1]..=b[4]).contains(&pos.y)
                    && (b[2]..=b[5]).contains(&pos.z)
            })
        })
    }
}

/// The bounding boxes of the monuments `chunk` references; `false` when a start chunk was
/// not available.
fn monument_boxes(level: &Level, chunk: ChunkPos) -> (bool, Vec<[i32; 6]>) {
    use vanillite_nbt::Tag;
    const NAME: &str = "minecraft:monument";
    let Some((_, tags)) = chunk_tags(level, chunk) else {
        return (true, Vec::new());
    };
    let Some(Tag::LongArray(refs)) = tags
        .get("structures")
        .and_then(Tag::as_compound)
        .and_then(|s| s.get("References"))
        .and_then(Tag::as_compound)
        .and_then(|r| r.get(NAME))
    else {
        return (true, Vec::new());
    };
    let mut complete = true;
    let mut boxes = Vec::new();
    for &key in refs {
        let start_chunk = ChunkPos::from_long(key);
        let Some((_, start_tags)) = chunk_tags(level, start_chunk) else {
            complete = false;
            continue;
        };
        let Some(start) = start_tags
            .get("structures")
            .and_then(Tag::as_compound)
            .and_then(|s| s.get("starts"))
            .and_then(Tag::as_compound)
            .and_then(|s| s.get(NAME))
            .and_then(Tag::as_compound)
        else {
            continue;
        };
        boxes.extend(start_box(start));
    }
    (complete, boxes)
}

/// The bounding box of a stored structure start (`StructureStart.getBoundingBox`): the box
/// around all its pieces' `BB`s.
fn start_box(start: &vanillite_nbt::Compound) -> Option<[i32; 6]> {
    use vanillite_nbt::Tag;
    let Some(Tag::List(children)) = start.get("Children") else {
        return None;
    };
    let mut bb: Option<[i32; 6]> = None;
    for child in &children.0 {
        let Some(Tag::IntArray(b)) = child.as_compound().and_then(|c| c.get("BB")) else {
            continue;
        };
        let [x0, y0, z0, x1, y1, z1] = b.as_slice() else {
            continue;
        };
        bb = Some(match bb {
            None => [*x0, *y0, *z0, *x1, *y1, *z1],
            Some(o) => [
                o[0].min(*x0),
                o[1].min(*y0),
                o[2].min(*z0),
                o[3].max(*x1),
                o[4].max(*y1),
                o[5].max(*z1),
            ],
        });
    }
    bb
}

/// `ChunkGenerator.getMobsAt` (via `NaturalSpawner.mobsAt`): the structure override inside an
/// ocean monument, else the biome's list.
fn mobs_at<'t>(
    level: &Level,
    table: &'t SpawnTable,
    structures: &mut StructureSpawns,
    category: Category,
    pos: BlockPos,
) -> Option<&'t [SpawnEntry]> {
    if matches!(
        category,
        Category::Monster | Category::Axolotls | Category::UndergroundWaterCreature
    ) && structures.in_monument(level, pos)
    {
        return Some(if category == Category::Monster {
            monument_monsters()
        } else {
            NO_SPAWNS
        });
    }
    table
        .biome(level.biome(pos))
        .map(|b| b.by_category[category.index()].as_slice())
}

/// The guardians of an ocean monument: weight 1, 2 to 4.
fn monument_monsters() -> &'static [SpawnEntry] {
    static ENTRIES: std::sync::OnceLock<[SpawnEntry; 1]> = std::sync::OnceLock::new();
    ENTRIES.get_or_init(|| {
        [SpawnEntry {
            type_id: vanillite_data::entity::ids::GUARDIAN,
            weight: 1,
            min: 2,
            max: 4,
        }]
    })
}

/// `ServerChunkCache.tickChunks`' spawning: counts, caps and one attempt per category and
/// chunk near players.
pub fn tick(
    level: &mut Level,
    table: &SpawnTable,
    spawner: &mut Spawner,
    input: &SpawnInput<'_>,
    spawn_persistent: bool,
) {
    if input.players.is_empty() {
        return;
    }
    // `getNaturalSpawnChunkCount`: chunks within 8 of a player chunk.
    spawner.seen.clear();
    for (_, p) in input.players {
        let c = BlockPos::containing(p.x, p.y, p.z).chunk();
        for dx in -8..=8 {
            for dz in -8..=8 {
                spawner.seen.insert(ChunkPos::new(c.x + dx, c.z + dz));
            }
        }
    }
    let mut state = SpawnState {
        spawnable_chunks: i32::try_from(spawner.seen.len()).unwrap_or(i32::MAX),
        counts: [0; 7],
        local: LocalCaps {
            counts: vec![[0; 7]; input.players.len()],
        },
    };
    // `createState`: the non-persistent mobs per category, globally and per player.
    for (id, m) in level.entities.mobs.iter() {
        if m.persistent {
            continue;
        }
        let Some(e) = level.entities.store.get(id) else {
            continue;
        };
        let Some(c) = Category::of(e.kind()) else {
            continue;
        };
        let pos = e.pos();
        let chunk = BlockPos::containing(pos.x, pos.y, pos.z).chunk();
        if level.chunks.chunk(chunk).is_none() {
            continue;
        }
        state.counts[c.index()] += 1;
        players_near(input, chunk, &mut spawner.near);
        for &i in &spawner.near {
            state.local.counts[i][c.index()] += 1;
        }
    }
    let categories: Vec<Category> = Category::ALL
        .into_iter()
        .filter(|c| {
            (input.spawn_enemies || c.is_friendly())
                && (spawn_persistent || !c.is_persistent())
                && state.can_spawn_global(*c)
        })
        .collect();
    // `collectSpawningChunks`: ticking chunks with a player within 128 blocks, shuffled.
    spawner.chunks.clear();
    spawner
        .chunks
        .extend(spawner.seen.iter().copied().filter(|&c| {
            level.is_block_ticking(c)
                && input
                    .players
                    .iter()
                    .any(|(_, p)| chunk_center_distance_sqr(c, *p) < 16384.0)
        }));
    spawner.chunks.sort_by_key(|c| (c.x, c.z));
    let n = spawner.chunks.len();
    for i in (1..n).rev() {
        let j = usize::try_from(
            level
                .random()
                .next_int_bounded(i32::try_from(i + 1).unwrap_or(i32::MAX)),
        )
        .unwrap_or(0);
        spawner.chunks.swap(i, j);
    }
    if categories.is_empty() {
        return;
    }
    let chunks = std::mem::take(&mut spawner.chunks);
    for &chunk in &chunks {
        for &c in &categories {
            // `canSpawnForCategoryLocal`.
            players_near(input, chunk, &mut spawner.near);
            let local_ok = spawner
                .near
                .iter()
                .any(|&i| state.local.counts[i][c.index()] < c.max_per_chunk());
            if !local_ok {
                continue;
            }
            spawn_category_for_chunk(level, table, spawner, input, &mut state, c, chunk);
        }
    }
    spawner.chunks = chunks;
}

/// `spawnCategoryForChunk` and `spawnCategoryForPosition`.
fn spawn_category_for_chunk(
    level: &mut Level,
    table: &SpawnTable,
    spawner: &mut Spawner,
    input: &SpawnInput<'_>,
    state: &mut SpawnState,
    category: Category,
    chunk: ChunkPos,
) {
    let x0 = chunk.x * 16 + level.random().next_int_bounded(16);
    let z0 = chunk.z * 16 + level.random().next_int_bounded(16);
    let top = surface_y(level, HeightmapKind::WorldSurface, x0, z0) + 1;
    let min_y = level.min_y();
    let y = level.random().next_int_between_inclusive(min_y, top);
    if y < min_y + 1 {
        return;
    }
    let start = BlockPos::new(x0, y, z0);
    if level.block(start).has(flags::REDSTONE_CONDUCTOR) {
        return;
    }
    let mut cluster = 0;
    for _ in 0..3 {
        let (mut x, mut z) = (x0, z0);
        let mut current: Option<SpawnEntry> = None;
        #[allow(clippy::cast_possible_truncation)]
        let mut max = (level.random().next_float() * 4.0).ceil() as i32;
        let mut group_index = 0u32;
        let mut ll = 0;
        while ll < max {
            ll += 1;
            x += level.random().next_int_bounded(6) - level.random().next_int_bounded(6);
            z += level.random().next_int_bounded(6) - level.random().next_int_bounded(6);
            let pos = BlockPos::new(x, y, z);
            let (xx, zz) = (f64::from(x) + 0.5, f64::from(z) + 0.5);
            let at = Vec3::new(xx, f64::from(y), zz);
            let Some(nearest) = input
                .players
                .iter()
                .map(|(_, p)| p.distance_to_sqr(at))
                .min_by(f64::total_cmp)
            else {
                continue;
            };
            if !right_distance(level, input, chunk, pos, nearest) {
                continue;
            }
            if current.is_none() {
                let Some(list) = mobs_at(level, table, &mut spawner.structures, category, pos)
                else {
                    break;
                };
                let Some(entry) = pick(list, level.random()) else {
                    break;
                };
                max = level
                    .random()
                    .next_int_between_inclusive(entry.min, entry.max);
                current = Some(entry);
            }
            let Some(entry) = current else {
                break;
            };
            if !valid_position_for_type(
                level,
                table,
                &mut spawner.structures,
                input,
                category,
                entry,
                pos,
                nearest,
            ) {
                continue;
            }
            let Some(kind) = Kind::of(entry.type_id) else {
                // Mobs without behaviour yet do not spawn; the attempt ends like a failed
                // creation in Vanilla.
                return;
            };
            let seed = level.random().next_long();
            let mut mob = MobData::new(kind, seed);
            let y_rot = level.random().next_float() * 360.0;
            let pos_v = Vec3::new(xx, f64::from(y), zz);
            if !valid_position_for_mob(level, input, &mob, pos_v, nearest) {
                continue;
            }
            let climate = table
                .biome(level.biome(pos))
                .map_or(FarmClimate::Temperate, |b| b.climate);
            kinds::finalize_spawn(&mut mob, input.difficulty, climate, group_index);
            group_index += 1;
            cluster += 1;
            add_mob(level, mob, pos_v, y_rot);
            state.counts[category.index()] += 1;
            players_near(input, chunk, &mut spawner.near);
            for &i in &spawner.near {
                state.local.counts[i][category.index()] += 1;
            }
            if cluster >= 4 {
                return;
            }
        }
    }
}

/// Adds a finished mob to the level (`addFreshEntityWithPassengers`).
pub fn add_mob(level: &mut Level, mut mob: MobData, pos: Vec3, y_rot: f32) -> EntityId {
    let uuid = random_uuid(&mut mob.random);
    mob.y_body_rot = y_rot;
    let rot = Rotation {
        y_rot,
        x_rot: 0.0,
        y_head_rot: y_rot,
    };
    let network_ids = std::sync::Arc::clone(level.network_ids_arc());
    super::spawn(&mut level.entities, &network_ids, uuid, pos, rot, mob)
}

/// `isRightDistanceToPlayerAndSpawnPoint`.
fn right_distance(
    level: &Level,
    input: &SpawnInput<'_>,
    chunk: ChunkPos,
    pos: BlockPos,
    nearest: f64,
) -> bool {
    if nearest <= 576.0 {
        return false;
    }
    if let Some(spawn) = input.spawn_point {
        let c = Vec3::new(
            f64::from(pos.x) + 0.5,
            f64::from(pos.y),
            f64::from(pos.z) + 0.5,
        );
        let s = Vec3::new(
            f64::from(spawn.x) + 0.5,
            f64::from(spawn.y) + 0.5,
            f64::from(spawn.z) + 0.5,
        );
        if c.distance_to_sqr(s) < 24.0 * 24.0 {
            return false;
        }
    }
    let c = pos.chunk();
    c == chunk || level.is_block_ticking(c)
}

/// `isValidSpawnPostitionForType`.
#[allow(clippy::too_many_arguments)]
fn valid_position_for_type(
    level: &Level,
    table: &SpawnTable,
    structures: &mut StructureSpawns,
    input: &SpawnInput<'_>,
    category: Category,
    entry: SpawnEntry,
    pos: BlockPos,
    nearest: f64,
) -> bool {
    let info = entry.type_id.info();
    let despawn = f64::from(category.despawn_distance());
    if !info.can_spawn_far_from_player && nearest > despawn * despawn {
        return false;
    }
    if !info.summonable {
        return false;
    }
    // `canSpawnMobAt`: the entry is still in the list there.
    if !mobs_at(level, table, structures, category, pos).is_some_and(|l| l.contains(&entry)) {
        return false;
    }
    if !spawn_position_ok(level, entry.type_id, pos) {
        return false;
    }
    let Some(kind) = Kind::of(entry.type_id) else {
        return true;
    };
    let mut random =
        LegacyRandom::new(i64::from(pos.x) ^ (i64::from(pos.z) << 20) ^ level.game_time());
    let rules = if super::boss::guardian::is_guardian(kind) {
        super::boss::guardian::check_spawn_rules(level, pos, input.difficulty, &mut random)
    } else {
        check_spawn_rules(level, &input.env, kind, pos, &mut random, false)
    };
    if !rules {
        return false;
    }
    let dims = super::Dimensions::of(entry.type_id);
    let aabb = dims.bounding_box(Vec3::new(
        f64::from(pos.x) + 0.5,
        f64::from(pos.y),
        f64::from(pos.z) + 0.5,
    ));
    let mut scratch = Vec::new();
    physics::no_collision(level, &aabb, &mut scratch)
}

/// `isValidPositionForMob`.
fn valid_position_for_mob(
    level: &Level,
    input: &SpawnInput<'_>,
    mob: &MobData,
    pos: Vec3,
    nearest: f64,
) -> bool {
    let category = Category::of(mob.kind.type_id()).unwrap_or(Category::Monster);
    let despawn = f64::from(category.despawn_distance());
    if nearest > despawn * despawn && mob.kind.remove_when_far_away() {
        return false;
    }
    // `checkSpawnRules`: the walk target value at the position is not negative.
    let block = BlockPos::containing(pos.x, pos.y, pos.z);
    if kinds::walk_target_value(mob.kind, level, &input.env, block) < 0.0 {
        return false;
    }
    // `checkSpawnObstruction`: no liquid in the box (guardians: no blocking entity).
    if super::boss::guardian::is_guardian(mob.kind) {
        return true;
    }
    let aabb = super::Dimensions::of(mob.kind.type_id()).bounding_box(pos);
    !physics::contains_any_liquid(level, &aabb)
}

/// `SpawnPlacementTypes.ON_GROUND.isSpawnPositionOk`.
pub fn spawn_position_ok(level: &Level, type_id: EntityTypeId, pos: BlockPos) -> bool {
    if type_id == vanillite_data::entity::ids::GUARDIAN
        || type_id == vanillite_data::entity::ids::ELDER_GUARDIAN
    {
        // `SpawnPlacementTypes.IN_WATER`.
        return super::boss::guardian::in_water_spawn_position_ok(level, pos);
    }
    let below = BlockPos::new(pos.x, pos.y - 1, pos.z);
    let above = BlockPos::new(pos.x, pos.y + 1, pos.z);
    if !valid_spawn_floor(level.block(below), type_id) {
        return false;
    }
    valid_empty_spawn_block(level.block(pos), type_id)
        && valid_empty_spawn_block(level.block(above), type_id)
}

/// `BlockState.isValidSpawn` for a mob: a sturdy top face that is not too bright, except
/// the blocks that never let mobs spawn on them.
fn valid_spawn_floor(state: BlockState, type_id: EntityTypeId) -> bool {
    let name = state.block().name();
    if name.ends_with("glass")
        || name.ends_with("glass_pane")
        || name.ends_with("leaves")
        || name == "minecraft:barrier"
        || name == "minecraft:bedrock"
        || name.ends_with("ice")
        || name == "minecraft:spawner"
    {
        return false;
    }
    if state.block() == ids::MAGMA_BLOCK {
        return type_id.info().fire_immune;
    }
    // Bit 1 of `sturdy_full` is the up face.
    state.misc().sturdy_full & 2 != 0 && state.light_emission() < 14
}

/// `NaturalSpawner.isValidEmptySpawnBlock`.
fn valid_empty_spawn_block(state: BlockState, type_id: EntityTypeId) -> bool {
    if state.has(flags::COLLISION_FULL_BLOCK) || state.has(flags::SIGNAL_SOURCE) {
        return false;
    }
    if !state.fluid_state().is_empty() {
        return false;
    }
    let name = state.block().name();
    if name.contains("rail") {
        return false;
    }
    // `EntityType.isBlockDangerous`.
    let fire_immune = type_id.info().fire_immune;
    let dangerous = (!fire_immune
        && (state.block() == ids::FIRE
            || state.block() == ids::SOUL_FIRE
            || state.block() == ids::MAGMA_BLOCK
            || state.block() == ids::LAVA_CAULDRON
            || name.ends_with("campfire")))
        || state.block() == ids::WITHER_ROSE
        || state.block() == ids::SWEET_BERRY_BUSH
        || state.block() == ids::CACTUS
        || state.block() == ids::POWDER_SNOW;
    !dangerous
}

/// The spawn rules (`SpawnPlacements.checkSpawnRules`) of a kind; `chunk_generation`
/// ignores the light requirements of animals.
pub fn check_spawn_rules(
    level: &Level,
    env: &Environment,
    kind: Kind,
    pos: BlockPos,
    random: &mut LegacyRandom,
    chunk_generation: bool,
) -> bool {
    let below = level.block(BlockPos::new(pos.x, pos.y - 1, pos.z));
    if kind.is_monster() {
        // `Monster.checkMonsterSpawnRules`.
        is_dark_enough_to_spawn(level, env, pos, random) && valid_spawn_floor(below, kind.type_id())
    } else {
        // `Animal.checkAnimalSpawnRules`: grass below and bright enough.
        let bright = chunk_generation || {
            let (sky, block) = level.light(pos);
            sky.max(block) > 8
        };
        below.block() == ids::GRASS_BLOCK && bright
    }
}

/// `Monster.isDarkEnoughToSpawn`.
pub fn is_dark_enough_to_spawn(
    level: &Level,
    env: &Environment,
    pos: BlockPos,
    random: &mut LegacyRandom,
) -> bool {
    let (sky, block) = level.light(pos);
    if sky > random.next_int_bounded(32) {
        return false;
    }
    if env.monster_block_light_limit < 15 && block > env.monster_block_light_limit {
        return false;
    }
    let brightness = if env.thundering {
        let darkened = Environment {
            sky_darken: 10,
            ..*env
        };
        max_local_raw_brightness(level, &darkened, pos)
    } else {
        max_local_raw_brightness(level, env, pos)
    };
    let (lo, hi) = env.monster_light_test;
    brightness <= random.next_int_between_inclusive(lo, hi)
}

fn surface_y(level: &Level, kind: HeightmapKind, x: i32, z: i32) -> i32 {
    let Some(c) = level.chunks.chunk(ChunkPos::new(x >> 4, z >> 4)) else {
        return level.min_y();
    };
    let Some(h) = c.chunk.heightmaps().get(kind) else {
        return level.min_y();
    };
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_wrap)]
    let top = h.get((x & 15) as u32, (z & 15) as u32) as i32 + c.chunk.min_y();
    top
}

/// `getTopNonCollidingPos` for ground mobs.
fn top_non_colliding(level: &Level, x: i32, z: i32) -> BlockPos {
    let y = surface_y(level, HeightmapKind::MotionBlockingNoLeaves, x, z);
    let mut pos = BlockPos::new(x, y, z);
    if level.settings.has_sky_light {
        // `dimensionType().hasCeiling()` is false for the overworld.
    }
    while pos.y > level.min_y() + 1 && level.block(BlockPos::new(x, pos.y - 1, z)).is_air() {
        pos = BlockPos::new(x, pos.y - 1, z);
    }
    pos
}

/// `NaturalSpawner.spawnMobsForChunkGeneration`: the animals of a freshly generated chunk.
pub fn spawn_for_chunk_generation(
    level: &mut Level,
    table: &SpawnTable,
    env: &Environment,
    chunk: ChunkPos,
    seed: i64,
    difficulty: Difficulty,
) {
    let source = BlockPos::new(chunk.x * 16 + 8, 0, chunk.z * 16 + 8);
    let biome = level.biome(BlockPos::new(
        source.x,
        surface_y(level, HeightmapKind::WorldSurface, source.x, source.z),
        source.z,
    ));
    let Some(b) = table.biome(biome) else {
        return;
    };
    let mobs = b.by_category[Category::Creature.index()].clone();
    if mobs.is_empty() {
        return;
    }
    let probability = b.creature_probability;
    let climate = b.climate;
    // `WorldgenRandom.setDecorationSeed(seed, minX, minZ)` on a fresh random.
    let mut random = LegacyRandom::new(seed);
    let a = random.next_long() | 1;
    let c = random.next_long() | 1;
    let decoration = i64::from(chunk.x * 16)
        .wrapping_mul(a)
        .wrapping_add(i64::from(chunk.z * 16).wrapping_mul(c))
        ^ seed;
    random.set_seed(decoration);
    let (xo, zo) = (chunk.x * 16, chunk.z * 16);
    while random.next_float() < probability {
        let Some(entry) = pick(&mobs, &mut random) else {
            continue;
        };
        let count = random.next_int_between_inclusive(entry.min, entry.max);
        let mut x = xo + random.next_int_bounded(16);
        let mut z = zo + random.next_int_bounded(16);
        let (start_x, start_z) = (x, z);
        let mut group_index = 0;
        for _ in 0..count {
            let mut success = false;
            let mut attempts = 0;
            while !success && attempts < 4 {
                attempts += 1;
                let pos = top_non_colliding(level, x, z);
                if entry.type_id.info().summonable && spawn_position_ok(level, entry.type_id, pos) {
                    let width = f64::from(entry.type_id.info().width);
                    let fx =
                        f64::from(x).clamp(f64::from(xo) + width, f64::from(xo) + 16.0 - width);
                    let fz =
                        f64::from(z).clamp(f64::from(zo) + width, f64::from(zo) + 16.0 - width);
                    let at = Vec3::new(fx, f64::from(pos.y), fz);
                    let aabb = super::Dimensions::of(entry.type_id).bounding_box(at);
                    let mut scratch = Vec::new();
                    let block = BlockPos::containing(fx, f64::from(pos.y), fz);
                    let rules = Kind::of(entry.type_id).is_some_and(|k| {
                        check_spawn_rules(level, env, k, block, &mut random, true)
                    });
                    if physics::no_collision(level, &aabb, &mut scratch) && rules {
                        let kind = Kind::of(entry.type_id);
                        let y_rot = random.next_float() * 360.0;
                        if let Some(kind) = kind {
                            let mut mob = MobData::new(kind, random.next_long());
                            if kinds::walk_target_value(kind, level, env, block) >= 0.0
                                && !physics::contains_any_liquid(level, &aabb)
                            {
                                kinds::finalize_spawn(&mut mob, difficulty, climate, group_index);
                                group_index += 1;
                                add_mob(level, mob, at, y_rot);
                                success = true;
                            }
                        }
                    }
                }
                x += random.next_int_bounded(5) - random.next_int_bounded(5);
                z += random.next_int_bounded(5) - random.next_int_bounded(5);
                while x < xo || x >= xo + 16 || z < zo || z >= zo + 16 {
                    x = start_x + random.next_int_bounded(5) - random.next_int_bounded(5);
                    z = start_z + random.next_int_bounded(5) - random.next_int_bounded(5);
                }
            }
        }
    }
}

/// Spawns a mob by command (`/summon`): `finalizeSpawn` with `COMMAND`, persistent.
pub fn summon(
    level: &mut Level,
    table: Option<&SpawnTable>,
    kind: Kind,
    pos: Vec3,
    difficulty: Difficulty,
) -> EntityId {
    let seed = level.random().next_long();
    let mut mob = MobData::new(kind, seed);
    let block = BlockPos::containing(pos.x, pos.y, pos.z);
    let climate = table
        .and_then(|t| t.biome(level.biome(block)))
        .map_or(FarmClimate::Temperate, |b| b.climate);
    kinds::finalize_spawn(&mut mob, difficulty, climate, 0);
    let y_rot = level.random().next_float() * 360.0;
    add_mob(level, mob, pos, y_rot)
}

/// Counts the mobs per kind (for `/vanillite` style statistics and tests).
pub fn count_by_kind(level: &Level) -> FxHashMap<Kind, usize> {
    let mut out = FxHashMap::default();
    for (_, m) in level.entities.mobs.iter() {
        *out.entry(m.kind).or_insert(0) += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use vanillite_nbt::{Compound, List, Tag};

    #[test]
    fn a_start_box_covers_all_pieces() {
        let piece = |bb: Vec<i32>| Tag::Compound(Compound::new().with("BB", bb));
        let start = Compound::new().with(
            "Children",
            Tag::List(List(vec![
                piece(vec![0, 39, 0, 57, 61, 57]),
                piece(vec![-3, 30, 5, 10, 70, 6]),
            ])),
        );
        assert_eq!(start_box(&start), Some([-3, 30, 0, 57, 70, 57]));
        assert_eq!(start_box(&Compound::new()), None);
    }

    #[test]
    fn monuments_override_the_monster_list() {
        let list = monument_monsters();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].type_id, vanillite_data::entity::ids::GUARDIAN);
        assert_eq!((list[0].weight, list[0].min, list[0].max), (1, 2, 4));
    }
}

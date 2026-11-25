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

use super::fam::{ExtraMob, Relation, SpawnContext, SpawnReason};
use super::kinds::{self, FarmClimate, Kind};
use super::physics;
use super::{Environment, MobData, max_local_raw_brightness};
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
    /// The biome id (`minecraft:plains`).
    pub name: Box<str>,
    /// The biome tags that contain the biome (`minecraft:is_forest`), sorted.
    pub tags: Vec<Box<str>>,
    pub by_category: [Vec<SpawnEntry>; 7],
    /// `spawn_costs` (`MobSpawnCost`): charge and energy budget by entity type.
    pub spawn_costs: Vec<(EntityTypeId, f64, f64)>,
    /// `creature_world_gen_spawn_probability`.
    pub creature_probability: f32,
    pub climate: FarmClimate,
    /// The base temperature of the biome.
    pub temperature: f32,
    /// `minecraft:gameplay/can_pillager_patrol_spawn`.
    pub patrol_spawn: bool,
}

impl BiomeSpawns {
    /// `biome.is(tag)`, with or without the `minecraft:` namespace.
    pub fn is(&self, tag: &str) -> bool {
        let tag = tag.strip_prefix('#').unwrap_or(tag);
        if tag.contains(':') {
            self.tags.binary_search_by(|t| (**t).cmp(tag)).is_ok()
        } else {
            let full = format!("minecraft:{tag}");
            self.tags.binary_search_by(|t| (**t).cmp(&*full)).is_ok()
        }
    }

    /// The spawn cost of an entity type (`getMobSpawnCost`).
    pub fn spawn_cost(&self, t: EntityTypeId) -> Option<(f64, f64)> {
        self.spawn_costs
            .iter()
            .find(|c| c.0 == t)
            .map(|&(_, charge, budget)| (charge, budget))
    }
}

/// The spawn settings of every biome, by protocol id, and the structures' spawn overrides.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SpawnTable {
    pub biomes: Vec<BiomeSpawns>,
    /// `Structure.spawnOverrides` by structure id and category.
    pub structures: FxHashMap<Box<str>, [Option<StructureOverride>; 7]>,
}

/// `StructureSpawnOverride`: the spawns inside a structure (its pieces or its whole box).
#[derive(Clone, Debug, PartialEq)]
pub struct StructureOverride {
    /// `bounding_box: piece` (else `full`).
    pub piece: bool,
    pub spawns: Vec<SpawnEntry>,
}

impl SpawnTable {
    /// The spawn settings of a biome by protocol id.
    pub fn biome(&self, id: u16) -> Option<&BiomeSpawns> {
        self.biomes.get(usize::from(id))
    }
}

/// The spawn table of the loaded data packs for code without a world's table at hand (spawn
/// rules that check the biome); data packs are frozen for the process.
static INSTALLED: std::sync::OnceLock<std::sync::Arc<SpawnTable>> = std::sync::OnceLock::new();

/// Installs the process' spawn table (later calls are ignored).
pub fn install_table(table: std::sync::Arc<SpawnTable>) {
    let _ = INSTALLED.set(table);
}

/// The spawn settings of the biome at `pos`, `None` without data packs.
pub fn biome_at(level: &Level, pos: BlockPos) -> Option<&'static BiomeSpawns> {
    INSTALLED.get()?.biome(level.biome(pos))
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

/// The players in cells of 8 × 8 chunks (128 blocks): the spawner's player queries look at
/// the 3 × 3 cells around a chunk instead of every player.
#[derive(Debug, Default)]
struct PlayerGrid {
    cells: FxHashMap<(i32, i32), Vec<usize>>,
}

impl PlayerGrid {
    fn build(&mut self, players: &[(EntityId, Vec3)]) {
        for v in self.cells.values_mut() {
            v.clear();
        }
        for (i, (_, p)) in players.iter().enumerate() {
            let c = BlockPos::containing(p.x, p.y, p.z).chunk();
            self.cells.entry((c.x >> 3, c.z >> 3)).or_default().push(i);
        }
        self.cells.retain(|_, v| !v.is_empty());
    }

    /// Calls `f` with the players whose cell is next to `chunk`'s.
    fn around(&self, chunk: ChunkPos, mut f: impl FnMut(usize)) {
        let (cx, cz) = (chunk.x >> 3, chunk.z >> 3);
        for dx in -1..=1 {
            for dz in -1..=1 {
                if let Some(list) = self.cells.get(&(cx + dx, cz + dz)) {
                    for &i in list {
                        f(i);
                    }
                }
            }
        }
    }
}

/// The players close enough to `chunk` for spawning (`getPlayersCloseForSpawning`), in
/// player order.
fn players_near(input: &SpawnInput<'_>, grid: &PlayerGrid, chunk: ChunkPos, out: &mut Vec<usize>) {
    out.clear();
    grid.around(chunk, |i| {
        if chunk_center_distance_sqr(chunk, input.players[i].1) < 16384.0 {
            out.push(i);
        }
    });
    out.sort_unstable();
}

/// The nearest player to a position (`getNearestPlayer(x, y, z, -1, false)`), from the cells
/// around it; `None` when no player is within 128 blocks of its chunk cell neighbourhood.
fn nearest_player_distance(input: &SpawnInput<'_>, grid: &PlayerGrid, at: Vec3) -> Option<f64> {
    let chunk = BlockPos::containing(at.x, at.y, at.z).chunk();
    let mut best: Option<f64> = None;
    grid.around(chunk, |i| {
        let d = input.players[i].1.distance_to_sqr(at);
        if best.is_none_or(|b| d < b) {
            best = Some(d);
        }
    });
    if best.is_none() {
        // Farther players still count for the distance checks.
        best = input
            .players
            .iter()
            .map(|(_, p)| p.distance_to_sqr(at))
            .min_by(f64::total_cmp);
    }
    best
}

/// `PotentialCalculator`: the charges of the mobs with a spawn cost.
#[derive(Debug, Default)]
struct Potential {
    charges: Vec<(BlockPos, f64)>,
}

impl Potential {
    fn add(&mut self, pos: BlockPos, charge: f64) {
        if charge != 0.0 {
            self.charges.push((pos, charge));
        }
    }

    /// `getPotentialEnergyChange`.
    fn energy_change(&self, pos: BlockPos, charge: f64) -> f64 {
        if charge == 0.0 {
            return 0.0;
        }
        let mut sum = 0.0;
        for &(p, c) in &self.charges {
            let d = p.dist_sqr(pos);
            sum += if d == 0.0 {
                f64::INFINITY
            } else {
                c / d.sqrt()
            };
        }
        sum * charge
    }
}

/// The reusable state of a world's spawner.
#[derive(Debug, Default)]
pub struct Spawner {
    chunks: Vec<ChunkPos>,
    seen: FxHashSet<ChunkPos>,
    near: Vec<usize>,
    grid: PlayerGrid,
    potential: Potential,
    /// Parsed structure data of chunks.
    pub structures: super::structures::StructureIndex,
    /// Chunks that passed the spawn step and wait for their animals.
    pub generated: Vec<ChunkPos>,
}

/// The spawn cost of a type at a position (`getRoughBiome(pos).getMobSpawnCost(type)`).
fn spawn_cost(
    level: &Level,
    table: &SpawnTable,
    t: EntityTypeId,
    pos: BlockPos,
) -> Option<(f64, f64)> {
    table
        .biome(rough_biome(level, pos))
        .and_then(|b| b.spawn_cost(t))
}

/// `getRoughBiome`: the noise biome of the quart position without the biome zoom.
fn rough_biome(level: &Level, pos: BlockPos) -> u16 {
    let default = level.chunks.info().default_biome;
    let Some(c) = level.chunks.chunk(pos.chunk()) else {
        return default;
    };
    let sections = c.chunk.sections();
    let min_quart = c.chunk.min_section() * 4;
    let len = i32::try_from(sections.len()).unwrap_or(0);
    let qy = (pos.y >> 2).clamp(min_quart, min_quart + len * 4 - 1);
    let index = usize::try_from((qy - min_quart) >> 2).unwrap_or(0);
    #[allow(clippy::cast_sign_loss)]
    sections[index].biome(
        ((pos.x >> 2) & 3) as u32,
        (qy & 3) as u32,
        ((pos.z >> 2) & 3) as u32,
    )
}

/// Whether a mob counts for the caps (`!isPersistenceRequired && !requiresCustomPersistence`).
fn counts_for_caps(level: &Level, id: EntityId, m: &MobData) -> bool {
    !m.persistent
        && !super::fam::requires_custom_persistence(m)
        && level.entities.riding.vehicle_of(id).is_none()
        && !level.entities.leashes.contains_key(&id)
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
    spawner.grid.build(input.players);
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
    spawner.potential.charges.clear();
    // `createState`: the non-persistent mobs per category, globally and per player, and the
    // charges of the mobs with a spawn cost.
    for (id, m) in level.entities.mobs.iter() {
        if !counts_for_caps(level, id, m) {
            continue;
        }
        let Some(e) = level.entities.store.get(id) else {
            continue;
        };
        let Some(c) = Category::of(e.kind()) else {
            continue;
        };
        let pos = e.pos();
        let block = BlockPos::containing(pos.x, pos.y, pos.z);
        let chunk = block.chunk();
        if level.chunks.chunk(chunk).is_none() {
            continue;
        }
        if let Some((charge, _)) = spawn_cost(level, table, e.kind(), block) {
            spawner.potential.add(block, charge);
        }
        state.counts[c.index()] += 1;
        players_near(input, &spawner.grid, chunk, &mut spawner.near);
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
    {
        let grid = &spawner.grid;
        spawner
            .chunks
            .extend(spawner.seen.iter().copied().filter(|&c| {
                if !level.is_block_ticking(c) {
                    return false;
                }
                let mut any = false;
                grid.around(c, |i| {
                    any |= chunk_center_distance_sqr(c, input.players[i].1) < 16384.0;
                });
                any
            }));
    }
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
            players_near(input, &spawner.grid, chunk, &mut spawner.near);
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

/// The nether fortress enemies (`NetherFortressStructure.FORTRESS_ENEMIES`).
fn fortress_enemies() -> &'static [SpawnEntry] {
    use vanillite_data::entity::ids as e;
    static LIST: std::sync::OnceLock<[SpawnEntry; 5]> = std::sync::OnceLock::new();
    LIST.get_or_init(|| {
        let entry = |type_id, weight, min, max| SpawnEntry {
            type_id,
            weight,
            min,
            max,
        };
        [
            entry(e::BLAZE, 10, 2, 3),
            entry(e::ZOMBIFIED_PIGLIN, 5, 4, 4),
            entry(e::WITHER_SKELETON, 8, 5, 5),
            entry(e::SKELETON, 2, 5, 5),
            entry(e::MAGMA_CUBE, 3, 4, 4),
        ]
    })
}

/// `NaturalSpawner.mobsAt`: the fortress enemies on nether bricks in a fortress, a
/// structure's spawn override, else the biome's list.
fn mobs_at<'t>(
    level: &Level,
    table: &'t SpawnTable,
    index: &mut super::structures::StructureIndex,
    category: Category,
    pos: BlockPos,
) -> &'t [SpawnEntry] {
    let mut fortress = false;
    let mut overridden: Option<&'t [SpawnEntry]> = None;
    let below_bricks = category == Category::Monster
        && level
            .block(BlockPos::new(pos.x, pos.y - 1, pos.z))
            .block()
            .name()
            == "minecraft:nether_bricks";
    if !table.structures.is_empty() || below_bricks {
        index.structures_at(level, pos, |name, in_piece, in_box| {
            if below_bricks && name == "minecraft:fortress" && in_box {
                fortress = true;
            }
            if overridden.is_none()
                && let Some(o) = table
                    .structures
                    .get(name)
                    .and_then(|o| o[category.index()].as_ref())
                && (if o.piece { in_piece } else { in_box })
            {
                overridden = Some(&o.spawns);
            }
        });
    }
    if fortress {
        return fortress_enemies();
    }
    if let Some(list) = overridden {
        return list;
    }
    table
        .biome(level.biome(pos))
        .map_or(&[], |b| &b.by_category[category.index()])
}

/// `Mob.getMaxSpawnClusterSize`.
pub fn max_spawn_cluster_size(kind: Kind) -> i32 {
    match kind {
        Kind::Wolf => 8,
        Kind::Horse
        | Kind::Donkey
        | Kind::Mule
        | Kind::SkeletonHorse
        | Kind::ZombieHorse
        | Kind::Llama
        | Kind::TraderLlama
        | Kind::Camel
        | Kind::CamelHusk => 6,
        Kind::Ghast | Kind::HappyGhast | Kind::Pillager => 1,
        Kind::Cod | Kind::Pufferfish | Kind::TropicalFish => 8,
        Kind::Salmon => 5,
        _ => 4,
    }
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
        let mut group_size = 0;
        let mut ll = 0;
        while ll < max {
            ll += 1;
            x += level.random().next_int_bounded(6) - level.random().next_int_bounded(6);
            z += level.random().next_int_bounded(6) - level.random().next_int_bounded(6);
            let pos = BlockPos::new(x, y, z);
            let (xx, zz) = (f64::from(x) + 0.5, f64::from(z) + 0.5);
            let at = Vec3::new(xx, f64::from(y), zz);
            let Some(nearest) = nearest_player_distance(input, &spawner.grid, at) else {
                continue;
            };
            if !right_distance(level, input, chunk, pos, nearest) {
                continue;
            }
            if current.is_none() {
                // `getRandomSpawnMobAt`: most water ambient spawns fail in some biomes.
                if category == Category::WaterAmbient
                    && table
                        .biome(level.biome(pos))
                        .is_some_and(|b| b.is("minecraft:reduced_water_ambient_spawns"))
                    && level.random().next_float() < 0.98
                {
                    break;
                }
                let list = mobs_at(level, table, &mut spawner.structures, category, pos);
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
            if !valid_position_for_type(level, table, spawner, input, category, entry, pos, nearest)
            {
                continue;
            }
            // `SpawnState.canSpawn`: the spawn cost's energy budget.
            let cost = spawn_cost(level, table, entry.type_id, pos);
            if let Some((charge, budget)) = cost
                && spawner.potential.energy_change(pos, charge) > budget
            {
                continue;
            }
            let Some(kind) = Kind::of(entry.type_id) else {
                // Types without behaviour yet do not spawn; the attempt ends like a failed
                // creation in Vanilla.
                return;
            };
            let seed = crate::mob::new_entity_seed();
            let mut mob = MobData::new(kind, seed);
            let y_rot = level.random().next_float() * 360.0;
            let pos_v = Vec3::new(xx, f64::from(y), zz);
            if !valid_position_for_mob(level, input, &mob, pos_v, nearest) {
                continue;
            }
            let extra = with_level_random(level, |level, random| {
                finalize(
                    level,
                    Some(table),
                    &input.env,
                    &mut mob,
                    pos,
                    input.difficulty,
                    group_index,
                    SpawnReason::Natural,
                    random,
                )
            });
            group_index += 1;
            cluster += 1;
            group_size += 1;
            let group_reached = super::fam::is_max_group_size_reached(&mob, group_size);
            add_mob_with(level, mob, pos_v, y_rot, extra);
            // `afterSpawn`: the charge, the counts.
            spawner.potential.add(pos, cost.map_or(0.0, |c| c.0));
            state.counts[category.index()] += 1;
            players_near(input, &spawner.grid, chunk, &mut spawner.near);
            for &i in &spawner.near {
                state.local.counts[i][category.index()] += 1;
            }
            if cluster >= max_spawn_cluster_size(kind) {
                return;
            }
            if group_reached {
                break;
            }
        }
    }
}

/// Runs `f` with the level and its random taken out of it (`finalizeSpawn` reads the level
/// and draws from its random).
pub fn with_level_random<R>(
    level: &mut Level,
    f: impl FnOnce(&Level, &mut LegacyRandom) -> R,
) -> R {
    let mut random = std::mem::replace(level.random(), LegacyRandom::new(0));
    let out = f(level, &mut random);
    *level.random() = random;
    out
}

/// Builds the spawn context at `pos` and runs `finalizeSpawn`; returns the mobs that spawn
/// with it (jockeys).
#[allow(clippy::too_many_arguments)]
pub fn finalize(
    level: &Level,
    table: Option<&SpawnTable>,
    env: &Environment,
    mob: &mut MobData,
    pos: BlockPos,
    difficulty: Difficulty,
    group_index: u32,
    reason: SpawnReason,
    random: &mut LegacyRandom,
) -> Vec<ExtraMob> {
    let biome = table.and_then(|t| t.biome(level.biome(pos)));
    let mut ctx = SpawnContext {
        difficulty,
        special_multiplier: super::fam::special_multiplier(difficulty),
        effective_difficulty: super::fam::effective_difficulty(difficulty),
        climate: biome.map_or(FarmClimate::Temperate, |b| b.climate),
        group_index,
        reason,
        biome,
        pos,
        level: Some(level),
        env: *env,
        extra: Vec::new(),
        ride: None,
    };
    kinds::finalize_spawn(mob, &mut ctx, random);
    if let Some(vehicle) = ctx.ride {
        ctx.extra.push(ExtraMob {
            mob: MobData::new(Kind::Chicken, 0),
            offset: Vec3::ZERO,
            relation: Relation::Existing(vehicle),
        });
    }
    ctx.extra
}

/// Adds a finished mob and the mobs spawned with it (`addFreshEntityWithPassengers`).
pub fn add_mob_with(
    level: &mut Level,
    mob: MobData,
    pos: Vec3,
    y_rot: f32,
    extra: Vec<ExtraMob>,
) -> EntityId {
    let id = add_mob(level, mob, pos, y_rot);
    let mut spawned = Vec::with_capacity(extra.len());
    for e in extra {
        if let Relation::Existing(vehicle) = e.relation {
            level.entities.riding.add(id, vehicle, false);
            spawned.push(vehicle);
            continue;
        }
        let other = add_mob(level, e.mob, pos + e.offset, y_rot);
        spawned.push(other);
        match e.relation {
            Relation::RidesExtra(i) => {
                if let Some(&vehicle) = spawned.get(i) {
                    level.entities.riding.add(other, vehicle, false);
                }
            }
            Relation::RidesIt => level.entities.riding.add(other, id, false),
            Relation::CarriesIt => level.entities.riding.add(id, other, false),
            Relation::Beside | Relation::Existing(_) => {}
        }
    }
    id
}

/// Adds a finished mob to the level (`addFreshEntityWithPassengers`).
pub fn add_mob(level: &mut Level, mut mob: MobData, pos: Vec3, y_rot: f32) -> EntityId {
    let uuid = crate::mob::new_uuid();
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
    spawner: &mut Spawner,
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
    if !mobs_at(level, table, &mut spawner.structures, category, pos).contains(&entry) {
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
    if !check_spawn_rules(
        level,
        &input.env,
        kind,
        pos,
        &mut random,
        SpawnReason::Natural,
    ) {
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

/// `Mob.checkSpawnRules` of a fresh mob: pathfinder mobs need a walk target value of at
/// least 0 (`PathfinderMob`), the others always pass.
pub fn mob_check_spawn_rules(
    level: &Level,
    env: &Environment,
    mob: &MobData,
    pos: BlockPos,
) -> bool {
    let pathfinder = !matches!(
        mob.kind,
        Kind::Slime | Kind::MagmaCube | Kind::SulfurCube | Kind::Ghast | Kind::Phantom | Kind::Bat
    );
    !pathfinder || kinds::walk_target_value(mob.kind, level, env, pos) >= 0.0
}

/// `Mob.checkSpawnObstruction` with the kinds' overrides (water mobs only need room).
pub fn check_spawn_obstruction(level: &Level, env: &Environment, mob: &MobData, pos: Vec3) -> bool {
    let aabb = kinds::mob_dimensions(mob).bounding_box(pos);
    let liquid = || physics::contains_any_liquid(level, &aabb);
    match mob.kind {
        Kind::Squid
        | Kind::GlowSquid
        | Kind::Dolphin
        | Kind::Cod
        | Kind::Salmon
        | Kind::Pufferfish
        | Kind::TropicalFish
        | Kind::Nautilus
        | Kind::ZombieNautilus
        | Kind::Axolotl
        | Kind::Guardian
        | Kind::Strider
        | Kind::Drowned => true,
        Kind::Ravager | Kind::ZombifiedPiglin => !liquid(),
        Kind::Ocelot => {
            if liquid() {
                return false;
            }
            let block = BlockPos::containing(pos.x, pos.y, pos.z);
            if block.y < env.sea_level {
                return false;
            }
            let below = level.block(BlockPos::new(block.x, block.y - 1, block.z));
            below.block() == ids::GRASS_BLOCK || in_block_tag(below, "minecraft:leaves")
        }
        Kind::IronGolem => {
            let block = BlockPos::containing(pos.x, pos.y, pos.z);
            let below = level.block(BlockPos::new(block.x, block.y - 1, block.z));
            if !below.has(flags::COLLISION_FULL_BLOCK) && below.collision_shape().is_empty() {
                return false;
            }
            let t = mob.kind.type_id();
            (1..3).all(|i| {
                let s = level.block(BlockPos::new(block.x, block.y + i, block.z));
                valid_empty_spawn_block(s, t)
            }) && {
                let s = level.block(block);
                !s.has(flags::COLLISION_FULL_BLOCK)
                    && !s.has(flags::SIGNAL_SOURCE)
                    && valid_empty_spawn_block_ignoring_fluid(s, t)
            }
        }
        _ => !liquid(),
    }
}

/// `isValidEmptySpawnBlock` with an empty fluid state.
fn valid_empty_spawn_block_ignoring_fluid(state: BlockState, type_id: EntityTypeId) -> bool {
    let name = state.block().name();
    if name.contains("rail") {
        return false;
    }
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
    if nearest > despawn * despawn && super::fam::remove_when_far_away_mob(mob, nearest) {
        return false;
    }
    let block = BlockPos::containing(pos.x, pos.y, pos.z);
    mob_check_spawn_rules(level, &input.env, mob, block)
        && check_spawn_obstruction(level, &input.env, mob, pos)
}
/// `SpawnPlacementType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    OnGround,
    InWater,
    InLava,
    NoRestrictions,
}

/// `SpawnPlacements`: the placement type and heightmap of an entity type (unregistered types
/// have no restrictions).
pub fn placement(t: EntityTypeId) -> (Placement, HeightmapKind) {
    use vanillite_data::entity::ids as e;
    let no_leaves = HeightmapKind::MotionBlockingNoLeaves;
    let p = if [
        e::AXOLOTL,
        e::COD,
        e::DOLPHIN,
        e::DROWNED,
        e::GUARDIAN,
        e::PUFFERFISH,
        e::SALMON,
        e::SQUID,
        e::TROPICAL_FISH,
        e::GLOW_SQUID,
        e::NAUTILUS,
        e::ELDER_GUARDIAN,
    ]
    .contains(&t)
    {
        Placement::InWater
    } else if t == e::STRIDER {
        Placement::InLava
    } else if [
        e::EVOKER,
        e::FOX,
        e::ILLUSIONER,
        e::PANDA,
        e::PHANTOM,
        e::SHULKER,
        e::TRADER_LLAMA,
        e::VEX,
        e::VINDICATOR,
        e::WARDEN,
    ]
    .contains(&t)
        || !Kind::of(t).is_some_and(registered_placement)
    {
        Placement::NoRestrictions
    } else {
        Placement::OnGround
    };
    let heightmap = if t == e::OCELOT || t == e::PARROT {
        HeightmapKind::MotionBlocking
    } else {
        no_leaves
    };
    (p, heightmap)
}

/// Whether a kind has a `SpawnPlacements` entry (the others have no restrictions).
fn registered_placement(k: Kind) -> bool {
    !matches!(
        k,
        Kind::ZombieNautilus | Kind::CopperGolem | Kind::Allay | Kind::Sniffer | Kind::Tadpole
    )
}

/// `SpawnPlacements.isSpawnPositionOk` by the type's placement.
pub fn spawn_position_ok(level: &Level, type_id: EntityTypeId, pos: BlockPos) -> bool {
    let above = BlockPos::new(pos.x, pos.y + 1, pos.z);
    match placement(type_id).0 {
        Placement::NoRestrictions => true,
        Placement::InWater => {
            super::is_water(level.block(pos)) && !level.block(above).has(flags::REDSTONE_CONDUCTOR)
        }
        Placement::InLava => super::is_lava(level.block(pos)),
        Placement::OnGround => {
            let below = BlockPos::new(pos.x, pos.y - 1, pos.z);
            if !is_valid_spawn(level, level.block(below), type_id) {
                return false;
            }
            valid_empty_spawn_block(level.block(pos), type_id)
                && valid_empty_spawn_block(level.block(above), type_id)
        }
    }
}

/// `BlockState.isValidSpawn(level, pos, type)` with the light check of the block below.
pub fn is_valid_spawn(_level: &Level, state: BlockState, type_id: EntityTypeId) -> bool {
    valid_spawn_floor(state, type_id)
}

/// `BlockState.isValidSpawn` for a mob: a sturdy top face that is not too bright, except
/// the blocks that never let mobs spawn on them.
pub fn valid_spawn_floor(state: BlockState, type_id: EntityTypeId) -> bool {
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
pub fn valid_empty_spawn_block(state: BlockState, type_id: EntityTypeId) -> bool {
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

/// The spawn rules (`SpawnPlacements.checkSpawnRules`) of a kind.
pub fn check_spawn_rules(
    level: &Level,
    env: &Environment,
    kind: Kind,
    pos: BlockPos,
    random: &mut LegacyRandom,
    reason: SpawnReason,
) -> bool {
    super::fam::check_spawn_rules(kind, level, env, pos, random, reason)
}

/// `Monster.checkMonsterSpawnRules` for monsters, `Animal.checkAnimalSpawnRules` for the
/// others.
pub fn default_spawn_rules(
    kind: Kind,
    level: &Level,
    env: &Environment,
    pos: BlockPos,
    random: &mut LegacyRandom,
    reason: SpawnReason,
) -> bool {
    if kind.is_monster() {
        check_monster_spawn_rules(kind, level, env, pos, random, reason)
    } else {
        check_animal_spawn_rules(kind, level, env, pos, reason)
    }
}

/// `EntitySpawnReason.isSpawner`.
pub fn is_spawner(reason: SpawnReason) -> bool {
    matches!(reason, SpawnReason::Spawner | SpawnReason::TrialSpawner)
}

/// `Mob.checkMobSpawnRules`: a valid spawn block below (spawners ignore it).
pub fn check_mob_spawn_rules(
    kind: Kind,
    level: &Level,
    pos: BlockPos,
    reason: SpawnReason,
) -> bool {
    let below = BlockPos::new(pos.x, pos.y - 1, pos.z);
    is_spawner(reason) || is_valid_spawn(level, level.block(below), kind.type_id())
}

/// `Monster.checkMonsterSpawnRules`: dark enough (trial spawners ignore light) and the mob
/// rules.
pub fn check_monster_spawn_rules(
    kind: Kind,
    level: &Level,
    env: &Environment,
    pos: BlockPos,
    random: &mut LegacyRandom,
    reason: SpawnReason,
) -> bool {
    (reason == SpawnReason::TrialSpawner || is_dark_enough_to_spawn(level, env, pos, random))
        && check_mob_spawn_rules(kind, level, pos, reason)
}

/// `Monster.checkAnyLightMonsterSpawnRules`.
pub fn check_any_light_monster_spawn_rules(
    kind: Kind,
    level: &Level,
    pos: BlockPos,
    reason: SpawnReason,
) -> bool {
    check_mob_spawn_rules(kind, level, pos, reason)
}

/// `Monster.checkSurfaceMonstersSpawnRules`: the monster rules under the open sky.
pub fn check_surface_monsters_spawn_rules(
    kind: Kind,
    level: &Level,
    env: &Environment,
    pos: BlockPos,
    random: &mut LegacyRandom,
    reason: SpawnReason,
) -> bool {
    check_monster_spawn_rules(kind, level, env, pos, random, reason)
        && (is_spawner(reason) || super::can_see_sky(level, pos))
}

/// `Animal.checkAnimalSpawnRules`: `#animals_spawnable_on` below and bright enough
/// (`isBrightEnoughToSpawn`; chunk generation ignores the light).
pub fn check_animal_spawn_rules(
    _kind: Kind,
    level: &Level,
    _env: &Environment,
    pos: BlockPos,
    reason: SpawnReason,
) -> bool {
    let below = level.block(BlockPos::new(pos.x, pos.y - 1, pos.z));
    let bright = reason == SpawnReason::ChunkGeneration || {
        let (sky, block) = level.light(pos);
        sky.max(block) > 8
    };
    in_block_tag(below, "minecraft:animals_spawnable_on") && bright
}

/// Whether a block is in a block tag of the data packs (`false` without data packs, except
/// the grass block for `#animals_spawnable_on`).
pub fn in_block_tag(state: BlockState, tag: &str) -> bool {
    match crate::player::play::game_data() {
        Some(d) if d.block_tags.contains_key(tag) => d.block_in_tag(state.block().0, tag),
        _ => tag == "minecraft:animals_spawnable_on" && state.block() == ids::GRASS_BLOCK,
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

/// `getHeight(heightmap, x, z)`: the first free y of a column.
pub fn heightmap_y(level: &Level, kind: HeightmapKind, x: i32, z: i32) -> i32 {
    surface_y(level, kind, x, z)
}

pub fn surface_y(level: &Level, kind: HeightmapKind, x: i32, z: i32) -> i32 {
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
fn top_non_colliding(
    level: &Level,
    env: &Environment,
    type_id: EntityTypeId,
    x: i32,
    z: i32,
) -> BlockPos {
    let (placement, heightmap) = placement(type_id);
    let y = surface_y(level, heightmap, x, z);
    let mut pos = BlockPos::new(x, y, z);
    if env.has_ceiling {
        loop {
            pos = BlockPos::new(x, pos.y - 1, z);
            if level.block(pos).is_air() || pos.y <= level.min_y() {
                break;
            }
        }
        loop {
            pos = BlockPos::new(x, pos.y - 1, z);
            if !(level.block(pos).is_air() && pos.y > level.min_y()) {
                break;
            }
        }
    }
    // `adjustSpawnPosition` of `ON_GROUND`: onto a pathfindable block below.
    if placement == Placement::OnGround {
        let below = BlockPos::new(x, pos.y - 1, z);
        if super::path::is_pathfindable_land(level.block(below)) {
            return below;
        }
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
                let pos = top_non_colliding(level, env, entry.type_id, x, z);
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
                        check_spawn_rules(
                            level,
                            env,
                            k,
                            block,
                            &mut random,
                            SpawnReason::ChunkGeneration,
                        )
                    });
                    if physics::no_collision(level, &aabb, &mut scratch) && rules {
                        let kind = Kind::of(entry.type_id);
                        let y_rot = random.next_float() * 360.0;
                        if let Some(kind) = kind {
                            let mut mob = MobData::new(kind, crate::mob::new_entity_seed());
                            if mob_check_spawn_rules(level, env, &mob, block)
                                && check_spawn_obstruction(level, env, &mob, at)
                            {
                                let extra = finalize(
                                    level,
                                    Some(table),
                                    env,
                                    &mut mob,
                                    block,
                                    difficulty,
                                    group_index,
                                    SpawnReason::ChunkGeneration,
                                    // `WorldGenRegion.getRandom()` is a positional random of
                                    // the region; this one is distribution equal.
                                    &mut random,
                                );
                                group_index += 1;
                                add_mob_with(level, mob, at, y_rot, extra);
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
    let seed = crate::mob::new_entity_seed();
    let mut mob = MobData::new(kind, seed);
    let block = BlockPos::containing(pos.x, pos.y, pos.z);
    let env = Environment::default();
    let extra = with_level_random(level, |level, random| {
        finalize(
            level,
            table,
            &env,
            &mut mob,
            block,
            difficulty,
            0,
            SpawnReason::Command,
            random,
        )
    });
    let y_rot = level.random().next_float() * 360.0;
    add_mob_with(level, mob, pos, y_rot, extra)
}

/// `CatSpawner`'s state.
#[derive(Debug, Default)]
pub struct CatSpawner {
    next_tick: i32,
}

/// `CatSpawner.tick`: every minute a cat may spawn 8–24 blocks from a random player, in a
/// swamp hut (`#cats_spawn_in`, a black persistent cat when none is within 16 blocks).
/// Villages (`isCloseToVillage`, more than four occupied homes) need POIs (#60).
pub fn cat_spawner_tick(
    level: &mut Level,
    table: Option<&SpawnTable>,
    spawner: &mut CatSpawner,
    structures: &mut super::structures::StructureIndex,
    env: &Environment,
    difficulty: Difficulty,
    players: &[EntityId],
) {
    spawner.next_tick -= 1;
    if spawner.next_tick > 0 {
        return;
    }
    spawner.next_tick = 1200;
    if players.is_empty() {
        return;
    }
    // `getRandomPlayer`.
    let i = level
        .random()
        .next_int_bounded(i32::try_from(players.len()).unwrap_or(i32::MAX));
    let Some(p) = usize::try_from(i)
        .ok()
        .and_then(|i| players.get(i))
        .and_then(|&id| level.entities.store.get(id))
        .map(|e| e.pos())
    else {
        return;
    };
    let r = level.random();
    let x = (8 + r.next_int_bounded(24)) * if r.next_boolean() { -1 } else { 1 };
    let z = (8 + r.next_int_bounded(24)) * if r.next_boolean() { -1 } else { 1 };
    let base = BlockPos::containing(p.x, p.y, p.z);
    let pos = BlockPos::new(base.x + x, base.y, base.z + z);
    let loaded = (-10..=10).step_by(10).all(|dx| {
        (-10..=10)
            .step_by(10)
            .all(|dz| level.is_loaded(BlockPos::new(pos.x + dx, pos.y, pos.z + dz)))
    });
    if !loaded || !spawn_position_ok(level, vanillite_data::entity::ids::CAT, pos) {
        return;
    }
    let mut in_hut = false;
    structures.structures_at(level, pos, |name, in_piece, _| {
        in_hut |= in_piece && name == "minecraft:swamp_hut";
    });
    if !in_hut {
        return;
    }
    // `spawnInHut`: no cat within 16 × 8 × 16.
    let area = vanillite_base::Aabb::new(
        f64::from(pos.x) - 16.0,
        f64::from(pos.y) - 8.0,
        f64::from(pos.z) - 16.0,
        f64::from(pos.x) + 17.0,
        f64::from(pos.y) + 9.0,
        f64::from(pos.z) + 17.0,
    );
    let mut cats = false;
    let ents = &level.entities;
    ents.store.for_each_in(&area, false, |id| {
        cats |= ents.mob(id).is_some_and(|m| m.kind == Kind::Cat);
    });
    if cats {
        return;
    }
    let seed = crate::mob::new_entity_seed();
    let mut cat = MobData::new(Kind::Cat, seed);
    let extra = with_level_random(level, |level, random| {
        let extra = finalize(
            level,
            table,
            env,
            &mut cat,
            pos,
            difficulty,
            0,
            SpawnReason::Natural,
            random,
        );
        // The variant's spawn conditions (in a witch hut) with the level random.
        cat.variant = super::fam::companion::feline::pick_cat_variant(random, true, false);
        extra
    });
    cat.persistent = true;
    let at = Vec3::new(f64::from(pos.x), f64::from(pos.y), f64::from(pos.z));
    add_mob_with(level, cat, at, 0.0, extra);
}

/// Counts the mobs per kind (for `/vanillite` style statistics and tests).
pub fn count_by_kind(level: &Level) -> FxHashMap<Kind, usize> {
    let mut out = FxHashMap::default();
    for (_, m) in level.entities.mobs.iter() {
        *out.entry(m.kind).or_insert(0) += 1;
    }
    out
}

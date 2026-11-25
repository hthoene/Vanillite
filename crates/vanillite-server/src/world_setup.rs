//! What the worlds need at startup: the block facts, each dimension's [`WorldInfo`], the
//! compute pool as the chunk runtime's executor, and chunk packets for the players.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use vanillite_base::ChunkPos;
use vanillite_base::hash::FxHashMap;
use vanillite_chunk::packet::BlockEntityWire;
use vanillite_game::player::ChunkSource;
use vanillite_protocol::packets::play::clientbound::LevelChunkWithLight;
use vanillite_protocol::{Packet, SharedPacket};
use vanillite_world::chunks::Chunks;

use vanillite_chunk::slots::{ChunkGenerator, ChunkGeneratorProvider, FlatGenerator, WorldInfo};
use vanillite_datapack::LoadedPacks;
use vanillite_world::chunks::Executor;
use vanillite_worldgen::{DimensionSpec, TerrainGenerator, VanillaChunkGenerator};

use crate::compute::Compute;
use crate::server::StartError;

/// Installs the per-state block facts the chunk format needs (once per process). Without data
/// packs (tests) the heightmap tags are empty and there is one biome.
pub(crate) fn install_facts(packs: Option<&LoadedPacks>) -> Result<(), StartError> {
    let Some(packs) = packs else {
        vanillite_chunk::facts::install_vanilla(&[], &[], 1);
        return Ok(());
    };
    let tag = |name: &str| {
        packs
            .tag_ids("minecraft:block", name)
            .map_err(|e| StartError::Io(format!("data packs: {e:?}")))
    };
    let biomes = packs
        .registry("minecraft:worldgen/biome")
        .map_or(0, |r| r.entries().len());
    vanillite_chunk::facts::install_vanilla(
        &tag("minecraft:blocks_motion_in_heightmap")?,
        &tag("minecraft:blocks_motion_in_heightmap_no_leaves")?,
        u32::try_from(biomes).unwrap_or(u32::MAX),
    );
    Ok(())
}

/// `level-seed`: a number, or any other text hashed like Java's `String.hashCode`; `None` if
/// empty.
pub(crate) fn parse_seed(text: &str) -> Option<i64> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    Some(text.parse::<i64>().unwrap_or_else(|_| {
        let hash = text
            .encode_utf16()
            .fold(0i32, |h, c| h.wrapping_mul(31).wrapping_add(i32::from(c)));
        i64::from(hash)
    }))
}

/// A seed for a new world without `level-seed` (`WorldOptions.randomSeed`).
fn random_seed() -> i64 {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    if let Ok(now) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        hasher.write_u128(now.as_nanos());
    }
    hasher.finish() as i64
}

/// Saved data id of Vanilla's `WorldGenSettings`.
const WORLD_GEN_SETTINGS: &str = "minecraft:world_gen_settings";

/// Vanilla's `WorldGenSettings` of a world: the seed and the generator of each dimension.
#[derive(Debug, Clone)]
pub(crate) struct WorldGen {
    /// World seed.
    pub(crate) seed: i64,
    /// `generate_structures`.
    generate_structures: bool,
    /// The `generator` JSON of each dimension (`dimensions.<key>.generator`).
    generators: serde_json::Map<String, serde_json::Value>,
}

impl WorldGen {
    /// Reads the world's `data/minecraft/world_gen_settings.dat` (Paper keeps it in the
    /// overworld's folder). A new world gets its seed from `level-seed` (else a random one) and
    /// its generators from the world preset of `level-type`, and the file is written.
    pub(crate) fn load_or_create(
        level_dir: &Path,
        properties: &vanillite_config::ServerProperties,
        packs: Option<&LoadedPacks>,
    ) -> Result<Self, StartError> {
        let overworld = vanillite_storage::dimension_dir(level_dir, "minecraft:overworld");
        for dir in [level_dir, overworld.as_path()] {
            let data = vanillite_storage::read_saved_data(dir, WORLD_GEN_SETTINGS)
                .map_err(|e| StartError::io(dir, e))?;
            if let Some(data) = data {
                return Ok(Self::from_nbt(&data));
            }
        }
        let seed = parse_seed(&properties.level_seed).unwrap_or_else(random_seed);
        let preset = match properties.level_type.trim().to_lowercase().as_str() {
            "" | "default" | "minecraft:default" => "minecraft:normal".to_owned(),
            name if name.contains(':') => name.to_owned(),
            name => format!("minecraft:{name}"),
        };
        let dimensions = packs
            .and_then(|p| p.registry("minecraft:worldgen/world_preset"))
            .and_then(|r| r.get(&preset))
            .and_then(|entry| serde_json::from_str::<serde_json::Value>(entry.json.get()).ok())
            .and_then(|json| json.get("dimensions").cloned());
        let Some(serde_json::Value::Object(dimensions)) = dimensions else {
            // Without data packs (tests) there are no presets: the flat stand-in everywhere.
            return Ok(Self {
                seed,
                generate_structures: properties.generate_structures,
                generators: serde_json::Map::new(),
            });
        };
        let mut stems = vanillite_nbt::Compound::new();
        for (key, stem) in &dimensions {
            if let vanillite_nbt::Tag::Compound(stem) = json_to_nbt(stem) {
                stems.insert(key.clone(), stem);
            }
        }
        let data = vanillite_nbt::Compound::new()
            .with("generate_structures", properties.generate_structures)
            .with("bonus_chest", false)
            .with("seed", seed)
            .with("dimensions", stems);
        vanillite_storage::write_saved_data(level_dir, WORLD_GEN_SETTINGS, data.clone())
            .map_err(|e| StartError::io(level_dir, e))?;
        log::info!("New world: preset {preset}, seed {seed}");
        Ok(Self::from_nbt(&data))
    }

    fn from_nbt(data: &vanillite_nbt::Compound) -> Self {
        let seed = data
            .get("seed")
            .and_then(vanillite_nbt::Tag::as_i64)
            .unwrap_or(0);
        let mut generators = serde_json::Map::new();
        if let Some(dimensions) = data
            .get("dimensions")
            .and_then(vanillite_nbt::Tag::as_compound)
        {
            for (key, stem) in dimensions.iter() {
                if let Some(generator) = stem.as_compound().and_then(|s| s.get("generator")) {
                    generators.insert(key.to_owned(), nbt_to_json(generator));
                }
            }
        }
        // Vanilla reads a missing or non-byte value as `true`.
        let generate_structures = !matches!(
            data.get("generate_structures"),
            Some(vanillite_nbt::Tag::Byte(0))
        );
        Self {
            seed,
            generate_structures,
            generators,
        }
    }

    /// The generator JSON of a dimension.
    fn generator(&self, dimension: &str) -> Option<&serde_json::Value> {
        self.generators.get(dimension)
    }

    /// Whether the overworld uses the flat generator (`is_flat` of the login packet).
    pub(crate) fn is_flat(&self) -> bool {
        self.generator("minecraft:overworld")
            .is_none_or(|g| g.get("type").and_then(|t| t.as_str()) != Some("minecraft:noise"))
    }
}

/// JSON as NBT the way Vanilla's codecs read it back: integers as `Int` (`Long` if too large),
/// other numbers as `Double`, booleans as `Byte`; `null` becomes an empty compound.
fn json_to_nbt(json: &serde_json::Value) -> vanillite_nbt::Tag {
    use serde_json::Value;
    use vanillite_nbt::Tag;
    match json {
        Value::Null => Tag::Compound(vanillite_nbt::Compound::new()),
        Value::Bool(b) => Tag::from(*b),
        Value::Number(n) => match n.as_i64() {
            Some(v) => i32::try_from(v).map_or(Tag::Long(v), Tag::Int),
            None => Tag::Double(n.as_f64().unwrap_or(0.0)),
        },
        Value::String(s) => Tag::String(s.clone()),
        Value::Array(items) => {
            Tag::List(vanillite_nbt::List(items.iter().map(json_to_nbt).collect()))
        }
        Value::Object(map) => {
            let mut c = vanillite_nbt::Compound::with_capacity(map.len());
            for (k, v) in map {
                c.insert(k.clone(), json_to_nbt(v));
            }
            Tag::Compound(c)
        }
    }
}

/// NBT as JSON for the data-driven parsers (numbers stay numbers, `Byte` too).
fn nbt_to_json(tag: &vanillite_nbt::Tag) -> serde_json::Value {
    use serde_json::Value;
    use vanillite_nbt::Tag;
    match tag {
        Tag::Byte(v) => Value::from(*v),
        Tag::Short(v) => Value::from(*v),
        Tag::Int(v) => Value::from(*v),
        Tag::Long(v) => Value::from(*v),
        Tag::Float(v) => Value::from(*v),
        Tag::Double(v) => Value::from(*v),
        Tag::String(s) => Value::String(s.clone()),
        Tag::ByteArray(v) => v.iter().map(|&b| Value::from(b)).collect(),
        Tag::IntArray(v) => v.iter().map(|&b| Value::from(b)).collect(),
        Tag::LongArray(v) => v.iter().map(|&b| Value::from(b)).collect(),
        Tag::List(list) => list.0.iter().map(nbt_to_json).collect(),
        Tag::Compound(c) => Value::Object(
            c.iter()
                .map(|(k, v)| (k.to_owned(), nbt_to_json(v)))
                .collect(),
        ),
    }
}

/// The built-in `chunk-generator` slot: Vanilla's noise generator for `minecraft:noise`
/// dimensions, the classic flat layers for everything else.
#[derive(Debug)]
pub(crate) struct BuiltInGenerators {
    pub(crate) packs: Option<Arc<LoadedPacks>>,
    pub(crate) world_gen: WorldGen,
}

impl ChunkGeneratorProvider for BuiltInGenerators {
    fn create(&self, world: &WorldInfo) -> Result<Arc<dyn ChunkGenerator>, String> {
        let generator = self.world_gen.generator(&world.dimension);
        let kind = generator
            .and_then(|g| g.get("type"))
            .and_then(|t| t.as_str());
        let (Some(packs), Some(generator), Some("minecraft:noise")) =
            (self.packs.as_deref(), generator, kind)
        else {
            if let Some(kind) = kind.filter(|k| *k != "minecraft:flat") {
                log::warn!(
                    "{}: generator {kind} is not supported, using flat",
                    world.dimension
                );
            }
            return Ok(Arc::new(FlatGenerator::classic(world)));
        };
        let noise_settings = generator
            .get("settings")
            .and_then(|s| s.as_str())
            .ok_or("inline noise settings are not supported")?;
        let spec = DimensionSpec {
            noise_settings,
            biome_source: generator.get("biome_source").ok_or("no biome source")?,
            min_y: world.min_y,
            height: i32::try_from(world.height).map_err(|e| e.to_string())?,
            has_sky_light: world.has_sky_light,
        };
        let terrain =
            TerrainGenerator::new(packs, spec, self.world_gen.seed).map_err(|e| e.to_string())?;
        Ok(Arc::new(
            VanillaChunkGenerator::new(terrain, world.clone())
                .with_structures(self.world_gen.generate_structures),
        ))
    }
}

/// The [`WorldInfo`] of a dimension stored in `dir`; its height and sky light come from its
/// dimension type (Vanilla's overworld values without data packs).
pub(crate) fn world_info(
    packs: Option<&LoadedPacks>,
    dir: PathBuf,
    dimension: &str,
    seed: i64,
) -> Result<WorldInfo, StartError> {
    let json: serde_json::Value = match packs {
        Some(packs) => {
            let kind = packs
                .registry("minecraft:dimension_type")
                .and_then(|r| r.get(dimension))
                .ok_or_else(|| StartError::Io(format!("unknown dimension type {dimension}")))?;
            serde_json::from_str(kind.json.get())
                .map_err(|e| StartError::Io(format!("dimension type {dimension}: {e}")))?
        }
        None => serde_json::json!({ "min_y": -64, "height": 384, "has_skylight": true }),
    };
    let int = |key: &str| json.get(key).and_then(serde_json::Value::as_i64);
    let min_y = int("min_y")
        .and_then(|v| i32::try_from(v).ok())
        .unwrap_or(0);
    let height = int("height")
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(256);
    let has_sky_light = json
        .get("has_skylight")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let biome_registry = packs.and_then(|p| p.registry("minecraft:worldgen/biome"));
    let biomes: Arc<[String]> = biome_registry
        .map(|r| r.entries().iter().map(|e| e.id.clone()).collect())
        .unwrap_or_else(|| Arc::from(["minecraft:plains".to_owned()]));
    let default_biome = biomes
        .iter()
        .position(|b| b == "minecraft:plains")
        .and_then(|i| u16::try_from(i).ok())
        .unwrap_or(0);
    Ok(WorldInfo {
        dimension: dimension.to_owned(),
        dir,
        min_y,
        height,
        seed,
        has_sky_light,
        biomes,
        default_biome,
        air: vanillite_data::block::BlockState::AIR.0,
    })
}

/// The compute pool as the chunk runtime's executor: background jobs by priority.
#[derive(Debug)]
pub(crate) struct ComputeExecutor(pub(crate) Compute);

impl Executor for ComputeExecutor {
    fn spawn(&self, priority: u32, job: Box<dyn FnOnce() + Send>) {
        self.0.spawn_background(priority, job);
    }
}

/// Chunk packets for the players' chunk sending, encoded once per chunk and shared by every
/// player that receives it. Entries are dropped when a chunk's blocks or light change or it
/// unloads.
#[derive(Debug, Default)]
pub(crate) struct ChunkPackets {
    cache: RefCell<FxHashMap<ChunkPos, Arc<SharedPacket>>>,
}

impl ChunkPackets {
    /// Drops the packets of changed and unloaded chunks.
    pub(crate) fn invalidate(
        &mut self,
        chunks: &mut Chunks,
        light: &[(ChunkPos, vanillite_world::chunks::LightChange)],
    ) {
        let cache = self.cache.get_mut();
        for (pos, _) in light {
            cache.remove(pos);
        }
        let mut blocks = Vec::new();
        chunks.take_block_changes(&mut blocks);
        for pos in blocks {
            cache.remove(&pos.chunk());
        }
        if cache.len() > 4096 {
            cache.retain(|pos, _| chunks.chunk(*pos).is_some());
        }
    }

    /// Drops the packet of one chunk (its block entity data changed).
    pub(crate) fn invalidate_chunk(&mut self, pos: ChunkPos) {
        self.cache.get_mut().remove(&pos);
    }

    /// The source over a world's chunks and their live block entities.
    pub(crate) fn source<'a>(
        &'a self,
        chunks: &'a Chunks,
        block_entities: &'a vanillite_game::level::BlockEntities,
    ) -> WorldChunkSource<'a> {
        WorldChunkSource {
            chunks,
            block_entities,
            cache: &self.cache,
        }
    }
}

/// [`ChunkSource`] over the full chunks of a world.
pub(crate) struct WorldChunkSource<'a> {
    chunks: &'a Chunks,
    block_entities: &'a vanillite_game::level::BlockEntities,
    cache: &'a RefCell<FxHashMap<ChunkPos, Arc<SharedPacket>>>,
}

impl ChunkSource for WorldChunkSource<'_> {
    fn chunk_packet(&self, pos: ChunkPos) -> Option<Arc<SharedPacket>> {
        if let Some(packet) = self.cache.borrow().get(&pos) {
            return Some(Arc::clone(packet));
        }
        let chunk = self.chunks.chunk(pos)?;
        // Block entities go out with their update tag (`getUpdateTag`): live ones from the
        // level, stored ones of kinds the level does not model with their type only.
        let mut tags: Vec<(vanillite_base::BlockPos, u32, Vec<u8>)> = Vec::new();
        for (bpos, entity) in self.block_entities.in_chunk(pos) {
            let Some(id) = self.chunks.block(bpos) else {
                continue;
            };
            let state = vanillite_data::block::BlockState(id);
            let kind = state.block().info().block_entity;
            if kind == u16::MAX {
                continue;
            }
            let mut nbt = Vec::new();
            match vanillite_game::level::block_entity::update_tag(entity) {
                Some(tag) => {
                    if vanillite_nbt::write::write_network_compound(&tag, &mut nbt).is_err() {
                        nbt = vec![0];
                    }
                }
                None => nbt.push(0),
            }
            tags.push((bpos, u32::from(kind), nbt));
        }
        for be in &chunk.block_entities {
            if tags.iter().any(|(p, _, _)| *p == be.pos) {
                continue;
            }
            if let Some(kind) = be.kind {
                tags.push((be.pos, u32::from(kind), vec![0]));
            }
        }
        let block_entities: Vec<BlockEntityWire<'_>> = tags
            .iter()
            .filter_map(|(bpos, kind, nbt)| {
                Some(BlockEntityWire {
                    packed_xz: (((bpos.x & 15) << 4) | (bpos.z & 15)) as u8,
                    y: i16::try_from(bpos.y).ok()?,
                    type_id: *kind,
                    nbt,
                })
            })
            .collect();
        let mut body = Vec::with_capacity(16 * 1024);
        vanillite_protocol::codec::write_varint(&mut body, LevelChunkWithLight::ID);
        vanillite_chunk::packet::encode_chunk_with_light(
            &mut body,
            pos.x,
            pos.z,
            &chunk.chunk,
            &block_entities,
        );
        let packet = Arc::new(SharedPacket::new(body));
        self.cache.borrow_mut().insert(pos, Arc::clone(&packet));
        Some(packet)
    }
}

/// The simulation settings of a world from its rules, `server.properties` and the
/// `gameplay` attributes of its dimension type. Game rules keep Vanilla's defaults until
/// game rules are stored.
pub(crate) fn level_settings(
    packs: Option<&LoadedPacks>,
    info: &WorldInfo,
    rules: &vanillite_config::WorldRules,
    properties: &vanillite_config::ServerProperties,
) -> vanillite_game::level::LevelSettings {
    let attribute = |name: &str| {
        packs
            .and_then(|p| p.registry("minecraft:dimension_type"))
            .and_then(|r| r.get(&info.dimension))
            .and_then(|entry| serde_json::from_str::<serde_json::Value>(entry.json.get()).ok())
            .and_then(|json| json.get("attributes")?.get(name)?.as_bool())
            .unwrap_or(false)
    };
    let infiniburn = packs
        .and_then(|p| p.registry("minecraft:dimension_type"))
        .and_then(|r| r.get(&info.dimension))
        .and_then(|entry| serde_json::from_str::<serde_json::Value>(entry.json.get()).ok())
        .and_then(|json| Some(json.get("infiniburn")?.as_str()?.to_owned()))
        .map_or_else(Default::default, |tag| {
            vanillite_game::block::fire::Infiniburn::from_tag(&tag)
        });
    let mut settings = vanillite_game::level::LevelSettings {
        infiniburn,
        fast_lava: attribute("minecraft:gameplay/fast_lava"),
        water_evaporates: attribute("minecraft:gameplay/water_evaporates"),
        has_sky_light: info.has_sky_light,
        // `BaseFireBlock.inPortalDimension`.
        portal_dimension: matches!(
            info.dimension.as_str(),
            "minecraft:overworld" | "minecraft:the_nether"
        ),
        ..vanillite_game::level::LevelSettings::default()
    };
    apply_simulation_rules(
        &mut settings,
        rules,
        properties.max_chained_neighbor_updates,
    );
    settings
}

/// The exploit switches of `exploits` (decision D1).
pub(crate) fn exploit_settings(
    rules: &vanillite_config::WorldRules,
) -> vanillite_game::level::ExploitSettings {
    let e = &rules.exploits;
    vanillite_game::level::ExploitSettings {
        tnt_duplication: e.tnt_duplication,
        carpet_duplication: e.carpet_duplication,
        rail_duplication: e.rail_duplication,
        headless_pistons: e.headless_pistons,
        break_unbreakable_blocks: e.break_unbreakable_blocks,
        string_duplication: e.string_duplication,
    }
}

/// Sets the budgets of `performance.simulation`; `vanilla_max_chained` is
/// `max-chained-neighbor-updates` of `server.properties`.
pub(crate) fn apply_simulation_rules(
    settings: &mut vanillite_game::level::LevelSettings,
    rules: &vanillite_config::WorldRules,
    vanilla_max_chained: i32,
) {
    let simulation = &rules.simulation;
    settings.max_block_ticks = usize::try_from(simulation.max_block_ticks).unwrap_or(usize::MAX);
    settings.max_fluid_ticks = usize::try_from(simulation.max_fluid_ticks).unwrap_or(usize::MAX);
    settings.max_chained_neighbor_updates = match simulation.max_chained_neighbor_updates {
        vanillite_config::VanillaOr::Value(v) => i32::try_from(v).unwrap_or(i32::MAX),
        vanillite_config::VanillaOr::Vanilla => vanilla_max_chained,
    };
    settings.exploits = exploit_settings(rules);
}

/// The biomes of a world for the simulation: the biome registry, the seed's biome manager and
/// the sea level of the dimension's generator (Vanilla's -63 for flat worlds). `None` without
/// data packs.
pub(crate) fn climate(
    packs: Option<&LoadedPacks>,
    world_gen: &WorldGen,
    dimension: &str,
) -> Option<vanillite_game::block::weather::Climate> {
    let packs = packs?;
    let biomes = match vanillite_worldgen::biome::BiomeRegistry::load(packs) {
        Ok(biomes) => biomes,
        Err(e) => {
            log::warn!("{dimension}: biomes for the simulation: {e}");
            return None;
        }
    };
    let generator = world_gen.generator(dimension);
    let sea_level = generator
        .and_then(|g| g.get("settings")?.as_str())
        .and_then(|id| packs.registry("minecraft:worldgen/noise_settings")?.get(id))
        .and_then(|entry| serde_json::from_str::<serde_json::Value>(entry.json.get()).ok())
        .and_then(|json| json.get("sea_level")?.as_i64())
        .and_then(|v| i32::try_from(v).ok())
        .unwrap_or(-63);
    Some(vanillite_game::block::weather::Climate {
        biomes,
        manager: vanillite_worldgen::biome::BiomeManager::for_world_seed(world_gen.seed),
        sea_level,
    })
}

/// The spawn settings of every biome from the data packs: `gameplay/natural_mob_spawns`,
/// `gameplay/creature_world_gen_spawn_probability` and the farm animal climate tags. `None`
/// without data packs.
pub(crate) fn spawn_table(
    packs: Option<&LoadedPacks>,
) -> Option<vanillite_game::mob::spawn::SpawnTable> {
    use vanillite_game::mob::kinds::FarmClimate;
    use vanillite_game::mob::spawn::{BiomeSpawns, Category, SpawnEntry, SpawnTable};
    let packs = packs?;
    let registry = packs.registry("minecraft:worldgen/biome")?;
    let tag = |name: &str| {
        packs
            .tag_ids("minecraft:worldgen/biome", name)
            .unwrap_or_default()
    };
    let warm = tag("minecraft:spawns_warm_variant_farm_animals");
    let cold = tag("minecraft:spawns_cold_variant_farm_animals");
    // An attribute value, directly or as the `argument` of a modifier.
    fn attribute<'a>(attrs: &'a serde_json::Value, key: &str) -> Option<&'a serde_json::Value> {
        let v = attrs.get(key)?;
        Some(v.get("argument").unwrap_or(v))
    }
    let mut biomes = Vec::with_capacity(registry.entries().len());
    for (index, entry) in registry.entries().iter().enumerate() {
        let id = u32::try_from(index).unwrap_or(u32::MAX);
        let json: serde_json::Value = serde_json::from_str(entry.json.get()).unwrap_or_default();
        let attrs = json.get("attributes").cloned().unwrap_or_default();
        let mut b = BiomeSpawns {
            creature_probability: 0.1,
            climate: if warm.contains(&id) {
                FarmClimate::Warm
            } else if cold.contains(&id) {
                FarmClimate::Cold
            } else {
                FarmClimate::Temperate
            },
            ..BiomeSpawns::default()
        };
        if let Some(p) = attribute(
            &attrs,
            "minecraft:gameplay/creature_world_gen_spawn_probability",
        )
        .and_then(serde_json::Value::as_f64)
        {
            #[allow(clippy::cast_possible_truncation)]
            {
                b.creature_probability = p as f32;
            }
        }
        if let Some(by) = attribute(&attrs, "minecraft:gameplay/natural_mob_spawns")
            .and_then(|s| s.get("spawns_by_category"))
            .and_then(serde_json::Value::as_object)
        {
            for (category, list) in by {
                let Some(c) = Category::by_name(category) else {
                    continue;
                };
                let Some(list) = list.as_array() else {
                    continue;
                };
                let slot = &mut b.by_category[c as usize];
                for e in list {
                    let Some(type_id) = e
                        .get("type")
                        .and_then(serde_json::Value::as_str)
                        .and_then(vanillite_data::entity::EntityTypeId::by_name)
                    else {
                        continue;
                    };
                    let int = |v: Option<&serde_json::Value>| {
                        v.and_then(serde_json::Value::as_i64)
                            .and_then(|v| i32::try_from(v).ok())
                    };
                    let weight = int(e.get("weight")).unwrap_or(1);
                    let (min, max) = match e.get("count") {
                        Some(serde_json::Value::Object(o)) => (
                            int(o.get("min_inclusive")).unwrap_or(1),
                            int(o.get("max_inclusive")).unwrap_or(1),
                        ),
                        other => {
                            let n = int(other).unwrap_or(1);
                            (n, n)
                        }
                    };
                    slot.push(SpawnEntry {
                        type_id,
                        weight,
                        min,
                        max,
                    });
                }
            }
        }
        biomes.push(b);
    }
    Some(SpawnTable { biomes })
}

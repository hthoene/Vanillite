//! What the worlds need at startup: the block facts, each dimension's [`WorldInfo`], the
//! compute pool as the chunk runtime's executor, and chunk packets for the players.

use std::cell::RefCell;
use std::path::Path;
use std::sync::Arc;

use vanillite_base::ChunkPos;
use vanillite_base::hash::FxHashMap;
use vanillite_chunk::packet::BlockEntityWire;
use vanillite_game::player::ChunkSource;
use vanillite_protocol::packets::play::clientbound::LevelChunkWithLight;
use vanillite_protocol::{Packet, SharedPacket};
use vanillite_world::chunks::Chunks;

use vanillite_chunk::slots::WorldInfo;
use vanillite_datapack::LoadedPacks;
use vanillite_world::chunks::Executor;

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

/// `level-seed`: a number, or any other text hashed like Java's `String.hashCode`.
pub(crate) fn parse_seed(text: &str) -> i64 {
    let text = text.trim();
    if text.is_empty() {
        return 0;
    }
    text.parse::<i64>().unwrap_or_else(|_| {
        let hash = text
            .encode_utf16()
            .fold(0i32, |h, c| h.wrapping_mul(31).wrapping_add(i32::from(c)));
        i64::from(hash)
    })
}

/// The [`WorldInfo`] of a dimension; its height and sky light come from its dimension type
/// (Vanilla's overworld values without data packs).
pub(crate) fn world_info(
    packs: Option<&LoadedPacks>,
    world_dir: &Path,
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
        dir: vanillite_storage::dimension_dir(world_dir, dimension),
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
    pub(crate) fn invalidate(&mut self, chunks: &mut Chunks) {
        let cache = self.cache.get_mut();
        let mut changed = Vec::new();
        chunks.take_light_changes(&mut changed);
        for pos in changed {
            cache.remove(&pos);
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

    /// The source over a world's chunks.
    pub(crate) fn source<'a>(&'a self, chunks: &'a Chunks) -> WorldChunkSource<'a> {
        WorldChunkSource {
            chunks,
            cache: &self.cache,
        }
    }
}

/// [`ChunkSource`] over the full chunks of a world.
pub(crate) struct WorldChunkSource<'a> {
    chunks: &'a Chunks,
    cache: &'a RefCell<FxHashMap<ChunkPos, Arc<SharedPacket>>>,
}

impl ChunkSource for WorldChunkSource<'_> {
    fn chunk_packet(&self, pos: ChunkPos) -> Option<Arc<SharedPacket>> {
        if let Some(packet) = self.cache.borrow().get(&pos) {
            return Some(Arc::clone(packet));
        }
        let chunk = self.chunks.chunk(pos)?;
        // Block entities go out with their type only; the client creates them with default
        // data (their update tags need the block entity port).
        let block_entities: Vec<BlockEntityWire<'_>> = chunk
            .block_entities
            .iter()
            .filter_map(|be| {
                Some(BlockEntityWire {
                    packed_xz: (((be.pos.x & 15) << 4) | (be.pos.z & 15)) as u8,
                    y: i16::try_from(be.pos.y).ok()?,
                    type_id: u32::from(be.kind?),
                    nbt: &[0],
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

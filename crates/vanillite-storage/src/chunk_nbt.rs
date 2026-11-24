//! Chunk NBT (Vanilla's `SerializableChunkData`) to and from [`ProtoChunk`].
//!
//! Blocks, biomes, light, the status, block entities, scheduled ticks, post-processing marks
//! and the inhabited time are decoded; every other tag of the chunk (structures, …) is kept as
//! [`ProtoChunk::extra`] and written back unchanged. Heightmaps are recomputed on load and
//! written on save.
//!
//! Block states in palettes are written like 26.x does: the default state of a block as its
//! name, other states as `{id, properties}` (older `{Name, Properties}` entries are read too).

use std::io;
use std::sync::Arc;

use vanillite_base::hash::FxHashMap;
use vanillite_base::{BlockPos, ChunkPos};
use vanillite_chunk::bits::{self, Word};
use vanillite_chunk::container::{ContainerData, Strategy};
use vanillite_chunk::light::{ChunkLight, LightNibbles, NIBBLE_BYTES};
use vanillite_chunk::slots::{BlockEntityData, ChunkStatus, SavedTick, WorldInfo};
use vanillite_chunk::{
    Biomes, BlockStates, Chunk, HeightmapKind, PalettedContainer, ProtoChunk, Section, facts,
};
use vanillite_data::block::{BlockId, BlockState};
use vanillite_data::registry::{BLOCK, BLOCK_ENTITY_TYPE, FLUID, Registry};
use vanillite_data::version::DATA_VERSION;
use vanillite_nbt::{Budget, Compound, List, Tag};

/// Oldest data version read without conversion: 1.21 (D7). The chunk format is unchanged
/// since; older worlds are opened once in current Vanilla first.
pub const OLDEST_DATA_VERSION: i32 = 3953;

/// Tags this module decodes; everything else goes to `extra`.
const DECODED: [&str; 13] = [
    "DataVersion",
    "xPos",
    "yPos",
    "zPos",
    "Status",
    "InhabitedTime",
    "sections",
    "isLightOn",
    "block_entities",
    "Heightmaps",
    "block_ticks",
    "fluid_ticks",
    "PostProcessing",
];

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn nbt_err(e: vanillite_nbt::Error) -> io::Error {
    invalid(format!("NBT: {e:?}"))
}

/// Name ↔ id lookups of one world, built once.
#[derive(Debug)]
pub struct Codec {
    info: WorldInfo,
    biome_ids: FxHashMap<String, u16>,
}

impl Codec {
    /// The codec of a world.
    pub fn new(info: &WorldInfo) -> Self {
        let biome_ids = info
            .biomes
            .iter()
            .zip(0u16..)
            .map(|(name, id)| (name.clone(), id))
            .collect();
        Codec {
            info: info.clone(),
            biome_ids,
        }
    }

    /// Decodes a chunk compound read for `pos`.
    pub fn read(&self, pos: ChunkPos, mut root: Compound) -> io::Result<ProtoChunk> {
        let int = |root: &Compound, name: &str| root.get(name).and_then(Tag::as_i64);
        let version = int(&root, "DataVersion").unwrap_or(0);
        if version > i64::from(DATA_VERSION) {
            return Err(invalid(format!(
                "chunk {pos:?} has data version {version}, newer than this server ({DATA_VERSION})"
            )));
        }
        if version < i64::from(OLDEST_DATA_VERSION) {
            return Err(invalid(format!(
                "chunk {pos:?} has data version {version}; open the world once in Minecraft \
                 {} first",
                vanillite_data::version::VERSION_ID
            )));
        }
        let stored = (int(&root, "xPos"), int(&root, "zPos"));
        if stored != (Some(i64::from(pos.x)), Some(i64::from(pos.z))) {
            return Err(invalid(format!("chunk {pos:?} is stored at {stored:?}")));
        }
        let status = match root.get("Status").and_then(Tag::as_str) {
            // Before 26.1, terrain was three statuses; their result is regenerated.
            Some("minecraft:noise" | "minecraft:surface" | "minecraft:carvers") => {
                ChunkStatus::Biomes
            }
            Some(name) => ChunkStatus::by_name(name)
                .ok_or_else(|| invalid(format!("chunk {pos:?}: unknown status {name}")))?,
            None => ChunkStatus::Empty,
        };
        let light_on = int(&root, "isLightOn") == Some(1);
        let inhabited_time = int(&root, "InhabitedTime").unwrap_or(0);

        let info = &self.info;
        let min_section = info.min_y >> 4;
        let count = (info.height / 16) as usize;
        let empty = Arc::new(Section::new(info.air, info.default_biome));
        let mut sections = vec![Arc::clone(&empty); count];
        let mut light = ChunkLight::new(count);
        if let Some(Tag::List(list)) = root.get("sections") {
            let mut palette_cache = FxHashMap::default();
            for section in list.0.iter().filter_map(Tag::as_compound) {
                let Some(y) = section.get("Y").and_then(Tag::as_i64) else {
                    continue;
                };
                let light_index = y - i64::from(min_section) + 1;
                let Ok(light_index) = usize::try_from(light_index) else {
                    continue;
                };
                if light_index > count + 1 {
                    continue;
                }
                if light_on {
                    if let Some(layer) = light_layer(section.get("BlockLight")) {
                        light.block_mut()[light_index] = layer;
                    }
                    if info.has_sky_light
                        && let Some(layer) = light_layer(section.get("SkyLight"))
                    {
                        light.sky_mut()[light_index] = layer;
                    }
                }
                let Some(index) = light_index.checked_sub(1).filter(|&i| i < count) else {
                    continue;
                };
                let blocks = match section.get("block_states").and_then(Tag::as_compound) {
                    Some(states) => self
                        .blocks(states, &mut palette_cache)
                        .map_err(|e| invalid(format!("chunk {pos:?} section {y}: {e}")))?,
                    None => PalettedContainer::new(info.air),
                };
                let biomes = match section.get("biomes").and_then(Tag::as_compound) {
                    Some(biomes) => self
                        .biomes(biomes)
                        .map_err(|e| invalid(format!("chunk {pos:?} section {y}: {e}")))?,
                    None => PalettedContainer::new(info.default_biome),
                };
                sections[index] = Arc::new(Section::from_containers(blocks, biomes));
            }
        }
        let chunk = Chunk::from_sections(min_section, sections, light);

        let mut block_entities = Vec::new();
        if let Some(Tag::List(list)) = root.get("block_entities") {
            for entity in list.0.iter().filter_map(Tag::as_compound) {
                let coord = |name| {
                    entity
                        .get(name)
                        .and_then(Tag::as_i64)
                        .and_then(|v| i32::try_from(v).ok())
                };
                let (Some(x), Some(y), Some(z)) = (coord("x"), coord("y"), coord("z")) else {
                    continue;
                };
                let kind = entity
                    .get("id")
                    .and_then(Tag::as_str)
                    .and_then(|id| BLOCK_ENTITY_TYPE.id(id));
                let mut nbt = Vec::new();
                vanillite_nbt::write::write_network_compound(entity, &mut nbt).map_err(nbt_err)?;
                block_entities.push(BlockEntityData {
                    pos: BlockPos::new(x, y, z),
                    kind,
                    nbt: nbt.into(),
                });
            }
        }

        let block_ticks = read_ticks(root.get("block_ticks"), &BLOCK, pos);
        let fluid_ticks = read_ticks(root.get("fluid_ticks"), &FLUID, pos);
        let mut post_processing = Vec::new();
        if let Some(Tag::List(list)) = root.get("PostProcessing") {
            for (index, section) in list.0.iter().enumerate() {
                let Tag::List(offsets) = section else {
                    continue;
                };
                let section_y = min_section + index as i32;
                for packed in offsets.0.iter().filter_map(Tag::as_i64) {
                    // `ProtoChunk.unpackOffsetCoordinates`.
                    let packed = packed as i32;
                    post_processing.push(BlockPos::new(
                        pos.x * 16 + (packed & 15),
                        section_y * 16 + (packed >> 4 & 15),
                        pos.z * 16 + (packed >> 8 & 15),
                    ));
                }
            }
        }

        for name in DECODED {
            root.remove(name);
        }
        let extra = if root.is_empty() {
            None
        } else {
            let mut bytes = Vec::new();
            vanillite_nbt::write::write_network_compound(&root, &mut bytes).map_err(nbt_err)?;
            Some(bytes.into())
        };
        Ok(ProtoChunk {
            pos,
            status,
            chunk,
            block_entities,
            inhabited_time,
            light_on,
            block_ticks,
            fluid_ticks,
            post_processing,
            extra,
            generation: None,
        })
    }

    fn blocks(
        &self,
        states: &Compound,
        cache: &mut FxHashMap<String, u16>,
    ) -> io::Result<PalettedContainer<BlockStates>> {
        let Some(Tag::List(entries)) = states.get("palette") else {
            return Err(invalid("block_states without palette"));
        };
        let mut palette = Vec::with_capacity(entries.0.len());
        for entry in &entries.0 {
            palette.push(self.block_state(entry, cache));
        }
        from_anvil::<BlockStates>(&palette, states.get("data"), 4)
    }

    /// A palette entry: a block name (its default state) or `{id|Name, properties|Properties}`.
    fn block_state(&self, entry: &Tag, cache: &mut FxHashMap<String, u16>) -> u16 {
        let (name, properties) = match entry {
            Tag::String(name) => (name.as_str(), None),
            Tag::Compound(c) => (
                c.get("id")
                    .or_else(|| c.get("Name"))
                    .and_then(Tag::as_str)
                    .unwrap_or(""),
                c.get("properties")
                    .or_else(|| c.get("Properties"))
                    .and_then(Tag::as_compound),
            ),
            _ => ("", None),
        };
        if properties.is_none()
            && let Some(&state) = cache.get(name)
        {
            return state;
        }
        let Some(block) = BlockId::by_name(name) else {
            log::warn!("Unknown block {name:?} replaced by air");
            return self.info.air;
        };
        let mut state = block.default_state();
        for (key, value) in properties.iter().flat_map(|p| p.iter()) {
            if let Some(value) = value.as_str()
                && let Some(with) = state.with(key, value)
            {
                state = with;
            }
        }
        if properties.is_none() {
            cache.insert(name.to_owned(), state.0);
        }
        state.0
    }

    fn biomes(&self, biomes: &Compound) -> io::Result<PalettedContainer<Biomes>> {
        let Some(Tag::List(entries)) = biomes.get("palette") else {
            return Err(invalid("biomes without palette"));
        };
        let palette: Vec<u16> = entries
            .0
            .iter()
            .map(|entry| {
                entry
                    .as_str()
                    .and_then(|name| self.biome_ids.get(name).copied())
                    .unwrap_or(self.info.default_biome)
            })
            .collect();
        from_anvil::<Biomes>(&palette, biomes.get("data"), 0)
    }

    /// Encodes a chunk as its compound.
    pub fn write(&self, chunk: &ProtoChunk) -> io::Result<Compound> {
        let mut root = match &chunk.extra {
            Some(bytes) => {
                match vanillite_nbt::read::read_network(&mut &bytes[..], &mut Budget::file())
                    .map_err(nbt_err)?
                {
                    Some(Tag::Compound(c)) => c,
                    _ => Compound::new(),
                }
            }
            None => Compound::new(),
        };
        let info = &self.info;
        let min_section = info.min_y >> 4;
        root.insert("DataVersion", DATA_VERSION);
        root.insert("xPos", chunk.pos.x);
        root.insert("yPos", min_section);
        root.insert("zPos", chunk.pos.z);
        root.insert("Status", chunk.status.name());
        root.insert("InhabitedTime", chunk.inhabited_time);
        root.insert("isLightOn", chunk.light_on);

        let light = chunk.chunk.light();
        let mut sections = Vec::with_capacity(light.len());
        for light_index in 0..light.len() {
            let mut section = Compound::new();
            let y = min_section + light_index as i32 - 1;
            section.insert("Y", y as i8);
            if let Some(blocks) = light_index
                .checked_sub(1)
                .and_then(|i| chunk.chunk.sections().get(i))
            {
                let (palette, data) = to_anvil(blocks.blocks(), 4);
                let palette: Vec<Tag> = palette.into_iter().map(block_state_tag).collect();
                section.insert("block_states", palette_compound(palette, data));
                let (palette, data) = to_anvil(blocks.biomes(), 0);
                let palette = palette
                    .into_iter()
                    .map(|id| {
                        Tag::from(
                            info.biomes
                                .get(usize::from(id))
                                .map_or("minecraft:plains", String::as_str),
                        )
                    })
                    .collect();
                section.insert("biomes", palette_compound(palette, data));
            }
            if chunk.light_on {
                if let Some(bytes) = layer_bytes(&light.block()[light_index], false) {
                    section.insert("BlockLight", bytes);
                }
                if let Some(bytes) = layer_bytes(&light.sky()[light_index], true) {
                    section.insert("SkyLight", bytes);
                }
            }
            if section.len() > 1 {
                sections.push(Tag::Compound(section));
            }
        }
        root.insert("sections", List(sections));

        let mut heightmaps = Compound::new();
        for (kind, map) in chunk.chunk.heightmaps().iter() {
            let name = match kind {
                HeightmapKind::WorldSurfaceWg => "WORLD_SURFACE_WG",
                HeightmapKind::WorldSurface => "WORLD_SURFACE",
                HeightmapKind::OceanFloorWg => "OCEAN_FLOOR_WG",
                HeightmapKind::OceanFloor => "OCEAN_FLOOR",
                HeightmapKind::MotionBlocking => "MOTION_BLOCKING",
                HeightmapKind::MotionBlockingNoLeaves => "MOTION_BLOCKING_NO_LEAVES",
            };
            heightmaps.insert(name, words_to_longs(map.raw()));
        }
        root.insert("Heightmaps", heightmaps);

        let mut entities = Vec::with_capacity(chunk.block_entities.len());
        for entity in &chunk.block_entities {
            if let Some(tag) =
                vanillite_nbt::read::read_network(&mut &entity.nbt[..], &mut Budget::file())
                    .map_err(nbt_err)?
            {
                entities.push(tag);
            }
        }
        root.insert("block_entities", List(entities));
        root.insert("block_ticks", write_ticks(&chunk.block_ticks, &BLOCK));
        root.insert("fluid_ticks", write_ticks(&chunk.fluid_ticks, &FLUID));
        if !chunk.post_processing.is_empty() {
            let mut lists = vec![Vec::new(); (info.height / 16) as usize];
            for pos in &chunk.post_processing {
                if let Some(list) = usize::try_from((pos.y >> 4) - min_section)
                    .ok()
                    .and_then(|index| lists.get_mut(index))
                {
                    // `ProtoChunk.packOffsetCoordinates`.
                    let packed = (pos.x & 15) | (pos.y & 15) << 4 | (pos.z & 15) << 8;
                    list.push(Tag::Short(packed as i16));
                }
            }
            root.insert(
                "PostProcessing",
                List(lists.into_iter().map(|l| Tag::List(List(l))).collect()),
            );
        }
        Ok(root)
    }
}

/// Reads a `block_ticks` or `fluid_ticks` list (`SavedTick` codec: `i`, `x`, `y`, `z`, `t`,
/// `p`); ticks of unknown types or outside the chunk are dropped like Vanilla does.
fn read_ticks(tag: Option<&Tag>, registry: &Registry, chunk: ChunkPos) -> Vec<SavedTick> {
    let Some(Tag::List(list)) = tag else {
        return Vec::new();
    };
    list.0
        .iter()
        .filter_map(Tag::as_compound)
        .filter_map(|tick| {
            let int = |name| {
                tick.get(name)
                    .and_then(Tag::as_i64)
                    .and_then(|v| i32::try_from(v).ok())
            };
            let kind = registry.id(tick.get("i")?.as_str()?)?;
            let pos = BlockPos::new(int("x")?, int("y")?, int("z")?);
            // `TickPriority.byValue` clamps unknown values.
            let priority = int("p").unwrap_or(0).clamp(-3, 3) as i8;
            (pos.chunk() == chunk).then_some(SavedTick {
                kind,
                pos,
                delay: int("t")?,
                priority,
            })
        })
        .collect()
}

fn write_ticks(ticks: &[SavedTick], registry: &Registry) -> List {
    List(
        ticks
            .iter()
            .filter_map(|tick| {
                Some(Tag::Compound(
                    Compound::new()
                        .with("i", registry.name(tick.kind)?)
                        .with("x", tick.pos.x)
                        .with("y", tick.pos.y)
                        .with("z", tick.pos.z)
                        .with("t", tick.delay)
                        .with("p", i32::from(tick.priority)),
                ))
            })
            .collect(),
    )
}

fn block_state_tag(id: u16) -> Tag {
    let state = BlockState(id);
    let block = state.block();
    if block.default_state() == state {
        return Tag::from(block.name());
    }
    let mut properties = Compound::new();
    for (name, value) in state.properties() {
        properties.insert(name, value);
    }
    Tag::Compound(
        Compound::new()
            .with("id", block.name())
            .with("properties", properties),
    )
}

fn palette_compound(palette: Vec<Tag>, data: Option<Vec<i64>>) -> Compound {
    let mut c = Compound::new().with("palette", List(palette));
    if let Some(data) = data {
        c.insert("data", data);
    }
    c
}

fn light_layer(tag: Option<&Tag>) -> Option<LightNibbles> {
    let Some(Tag::ByteArray(bytes)) = tag else {
        return None;
    };
    if bytes.len() != NIBBLE_BYTES {
        return None;
    }
    let mut array = [0u8; NIBBLE_BYTES];
    for (out, &b) in array.iter_mut().zip(bytes) {
        *out = b as u8;
    }
    if let Some(&first) = array.first()
        && array.iter().all(|&b| b == first)
        && first >> 4 == first & 15
    {
        return Some(LightNibbles::Uniform(first & 15));
    }
    Some(LightNibbles::Data(Arc::new(array)))
}

/// The stored bytes of a layer; block light at level 0 and absent layers are not stored.
fn layer_bytes(layer: &LightNibbles, sky: bool) -> Option<Vec<i8>> {
    match layer {
        LightNibbles::Absent => None,
        LightNibbles::Uniform(0) if !sky => None,
        other => Some(other.to_bytes().iter().map(|&b| b as i8).collect()),
    }
}

fn words_to_longs(words: &[Word]) -> Vec<i64> {
    words.iter().map(|w| i64::from_be_bytes(*w)).collect()
}

/// A container from an Anvil palette and its packed indices (`bits = max(min_bits,
/// ceillog2(len))`), converted to the configuration Vanilla would use in memory.
fn from_anvil<S: Strategy>(
    palette: &[u16],
    data: Option<&Tag>,
    min_bits: u8,
) -> io::Result<PalettedContainer<S>> {
    let decode = |e| invalid(format!("{e:?}"));
    match palette {
        [] => return Err(invalid("empty palette")),
        [single] => return Ok(PalettedContainer::new(*single)),
        _ => {}
    }
    let Some(Tag::LongArray(longs)) = data else {
        return Err(invalid("palette without data"));
    };
    let len = u32::try_from(palette.len()).unwrap_or(u32::MAX);
    let stored_bits = facts::ceil_log2(len).max(min_bits);
    if longs.len() != bits::words_for(S::ENTRIES, stored_bits) {
        return Err(invalid(format!(
            "data length {} does not fit {} bits",
            longs.len(),
            stored_bits
        )));
    }
    let words: Vec<Word> = longs.iter().map(|l| l.to_be_bytes()).collect();
    let wanted = S::indirect_bits(facts::ceil_log2(len).max(1));
    if wanted == Some(stored_bits) {
        return PalettedContainer::from_parts(stored_bits, palette, &words).map_err(decode);
    }
    // Repack into the configuration of this palette size (indirect or global).
    let mut bad = false;
    let value = |index: usize| {
        let i = bits::get(&words, stored_bits, index) as usize;
        palette.get(i).copied().unwrap_or_else(|| {
            bad = true;
            0
        })
    };
    let result = match wanted {
        Some(target) => {
            let mut value = value;
            let packed = bits::pack(S::ENTRIES, target, |i| {
                let v = value(i);
                palette.iter().position(|&p| p == v).unwrap_or(0) as u32
            });
            PalettedContainer::from_parts(target, palette, &packed)
        }
        None => {
            let mut value = value;
            let packed = bits::pack(S::ENTRIES, S::global_bits(), |i| u32::from(value(i)));
            PalettedContainer::from_parts(S::global_bits(), &[], &packed)
        }
    };
    if bad {
        return Err(invalid("palette index out of range"));
    }
    result.map_err(decode)
}

/// The Anvil palette and packed data of a container (`None` data for a single value).
fn to_anvil<S: Strategy>(
    container: &PalettedContainer<S>,
    min_bits: u8,
) -> (Vec<u16>, Option<Vec<i64>>) {
    match container.data() {
        ContainerData::Single(value) => (vec![*value], None),
        ContainerData::Indirect {
            bits,
            palette,
            data,
        } if facts::ceil_log2(palette.len() as u32).max(min_bits) == *bits => {
            (palette.clone(), Some(words_to_longs(data)))
        }
        _ => {
            let mut palette = Vec::new();
            container.for_each_count(|value, _| palette.push(value));
            palette.sort_unstable();
            if palette.len() == 1 {
                return (palette, None);
            }
            let bits = facts::ceil_log2(palette.len() as u32).max(min_bits);
            let packed = bits::pack(S::ENTRIES, bits, |i| {
                palette.binary_search(&container.get(i)).unwrap_or(0) as u32
            });
            (palette, Some(words_to_longs(&packed)))
        }
    }
}

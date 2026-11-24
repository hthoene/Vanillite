//! Chunk NBT (Vanilla's `SerializableChunkData`) to and from [`ProtoChunk`].
//!
//! Reading is "Fast Anvil": one streaming pass over the NBT ([`Cursor`]) that builds no tree.
//! Block and biome palettes are mapped to ids once per distinct entry and the packed data goes
//! straight into the in-memory containers; the stored heightmaps are taken over (primed from
//! the blocks only when missing or invalid). Blocks, biomes, light, the status, block entities,
//! scheduled ticks, post-processing marks and the inhabited time are decoded; the bytes of
//! every other tag of the chunk (structures, …) are kept as [`ProtoChunk::extra`] and written
//! back unchanged, the other tags of a section under `vanillite:section_tags` in it.
//!
//! Block states in palettes are written like 26.x does: the default state of a block as its
//! name, other states as `{id, properties}` (older `{Name, Properties}` entries are read too).

use std::borrow::Cow;
use std::io;
use std::sync::Arc;

use vanillite_base::hash::FxHashMap;
use vanillite_base::{BlockPos, ChunkPos};
use vanillite_chunk::bits::{self, Word};
use vanillite_chunk::container::{ContainerData, Strategy};
use vanillite_chunk::light::{ChunkLight, LightNibbles, NIBBLE_BYTES};
use vanillite_chunk::slots::{BlockEntityData, ChunkStatus, SavedTick, WorldInfo};
use vanillite_chunk::{
    Biomes, BlockStates, Chunk, Heightmap, HeightmapKind, PalettedContainer, ProtoChunk, Section,
    facts,
};
use vanillite_data::block::{BlockId, BlockState};
use vanillite_data::registry::{BLOCK, BLOCK_ENTITY_TYPE, FLUID, Registry};
use vanillite_data::version::DATA_VERSION;
use vanillite_nbt::read::Cursor;
use vanillite_nbt::{Budget, Compound, List, Tag, TagType};

/// Oldest data version read without conversion: 1.21 (D7). The chunk format is unchanged
/// since; older worlds are opened once in current Vanilla first.
pub const OLDEST_DATA_VERSION: i32 = 3953;

/// Tags this module decodes; everything else goes to `extra`.
pub(crate) const DECODED: [&str; 13] = [
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

/// Tags of a section this module decodes; the others are kept in [`SECTION_TAGS`].
const SECTION_DECODED: [&str; 5] = ["Y", "block_states", "biomes", "BlockLight", "SkyLight"];

/// Key in `extra` of the section tags that are not decoded: a list of compounds, each with the
/// section's `Y` and its other tags, merged back into the sections on write.
pub(crate) const SECTION_TAGS: &str = "vanillite:section_tags";

const BLOCK_PALETTE: u8 = 0;
const BIOME_PALETTE: u8 = 1;

/// Decoded palette entries of one chunk by kind and payload bytes.
type PaletteCache<'b> = FxHashMap<(u8, &'b [u8]), u16>;

/// The parts of one stored section.
#[derive(Default)]
struct SectionParts<'b> {
    y: Option<i64>,
    /// The section, if blocks or biomes are stored.
    section: Option<Section>,
    block_light: Option<&'b [u8]>,
    sky_light: Option<&'b [u8]>,
    /// The entries that are not decoded, as compound entry bytes.
    unknown: Vec<u8>,
}

enum ReadError {
    Nbt(vanillite_nbt::Error),
    Io(io::Error),
}

impl From<vanillite_nbt::Error> for ReadError {
    fn from(e: vanillite_nbt::Error) -> Self {
        ReadError::Nbt(e)
    }
}

impl From<io::Error> for ReadError {
    fn from(e: io::Error) -> Self {
        ReadError::Io(e)
    }
}

/// Appends an entry name (ASCII, so modified UTF-8 is the same).
fn push_name(out: &mut Vec<u8>, name: &str) {
    out.extend_from_slice(&(name.len() as u16).to_be_bytes());
    out.extend_from_slice(name.as_bytes());
}

fn mutf8_name(bytes: &[u8]) -> vanillite_nbt::Result<Cow<'_, str>> {
    match std::str::from_utf8(bytes) {
        Ok(s) => Ok(Cow::Borrowed(s)),
        Err(_) => vanillite_nbt::mutf8::decode(bytes).map(Cow::Owned),
    }
}

/// A block entity from its compound payload; `None` without a position.
fn block_entity(raw: &[u8]) -> vanillite_nbt::Result<Option<BlockEntityData>> {
    let mut budget = Budget::file();
    let mut c = Cursor::new(raw, &mut budget);
    let (mut x, mut y, mut z, mut kind) = (None, None, None, None);
    c.enter_compound()?;
    while let Some((ty, name)) = c.entry()? {
        let coord = |v: Option<i64>| v.and_then(|v| i32::try_from(v).ok());
        match name {
            b"x" => x = coord(c.int(ty)?),
            b"y" => y = coord(c.int(ty)?),
            b"z" => z = coord(c.int(ty)?),
            b"id" => kind = c.str(ty)?.and_then(|id| BLOCK_ENTITY_TYPE.id(&id)),
            _ => {
                c.skip(ty)?;
            }
        }
    }
    let (Some(x), Some(y), Some(z)) = (x, y, z) else {
        return Ok(None);
    };
    let mut nbt = Vec::with_capacity(raw.len() + 1);
    nbt.push(TagType::Compound.id());
    nbt.extend_from_slice(raw);
    Ok(Some(BlockEntityData {
        pos: BlockPos::new(x, y, z),
        kind,
        nbt: nbt.into(),
    }))
}

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
    pub fn read(&self, pos: ChunkPos, root: Compound) -> io::Result<ProtoChunk> {
        let mut bytes = Vec::new();
        vanillite_nbt::write::write_named("", &root, &mut bytes).map_err(nbt_err)?;
        self.read_bytes(pos, &bytes)
    }

    /// Decodes a chunk from its uncompressed file NBT ("Fast Anvil"): one streaming pass that
    /// builds no NBT tree. Palettes and packed data go straight into the in-memory containers,
    /// stored heightmaps are taken over, and the bytes of tags that are not decoded are copied
    /// into `extra` as they are.
    pub fn read_bytes(&self, pos: ChunkPos, nbt: &[u8]) -> io::Result<ProtoChunk> {
        let mut budget = Budget::file();
        let mut cursor = Cursor::new(nbt, &mut budget);
        self.stream(pos, &mut cursor).map_err(|e| match e {
            ReadError::Nbt(e) => invalid(format!("chunk {pos:?}: NBT: {e:?}")),
            ReadError::Io(e) => e,
        })
    }

    fn stream<'b>(&self, pos: ChunkPos, c: &mut Cursor<'_, 'b>) -> Result<ProtoChunk, ReadError> {
        let info = &self.info;
        let min_section = info.min_y >> 4;
        let count = (info.height / 16) as usize;
        let empty = Arc::new(Section::new(info.air, info.default_biome));
        let mut sections = vec![Arc::clone(&empty); count];
        // Stored light is kept also without `isLightOn`, like Vanilla queues it; the world
        // computes untrusted light again.
        let mut light = ChunkLight::new(count);
        let (mut version, mut x, mut z, mut status) = (None, None, None, None);
        let (mut light_on, mut inhabited_time) = (false, 0);
        let mut heightmaps: [Option<&'b [u8]>; 6] = [None; 6];
        let mut block_entities = Vec::new();
        let (mut block_ticks, mut fluid_ticks) = (Vec::new(), Vec::new());
        let mut post_processing_tag = None;
        // Entries of the extra compound, and of its section list (unknown section tags).
        let mut extra: Vec<u8> = Vec::new();
        let mut section_tags: Vec<u8> = Vec::new();
        let mut section_tag_count = 0i32;
        let mut cache = FxHashMap::default();

        c.named_root()?;
        loop {
            let at = c.rest();
            let Some((ty, name)) = c.entry()? else {
                break;
            };
            match (name, ty) {
                (b"DataVersion", _) => version = c.int(ty)?,
                (b"xPos", _) => x = c.int(ty)?,
                (b"zPos", _) => z = c.int(ty)?,
                (b"Status", _) => status = c.str(ty)?,
                (b"isLightOn", _) => light_on = c.int(ty)? == Some(1),
                (b"InhabitedTime", _) => inhabited_time = c.int(ty)?.unwrap_or(0),
                (b"sections", TagType::List) => {
                    let (element, n) = c.enter_list()?;
                    for _ in 0..n {
                        if element != TagType::Compound {
                            c.skip(element)?;
                            continue;
                        }
                        let section = self.section(c, &mut cache, pos)?;
                        let Some(y) = section.y else { continue };
                        let Some(light_index) = usize::try_from(y - i64::from(min_section) + 1)
                            .ok()
                            .filter(|&i| i <= count + 1)
                        else {
                            continue;
                        };
                        if let Some(layer) = section.block_light.and_then(|b| light_layer(b, false))
                        {
                            light.block_mut()[light_index] = layer;
                        }
                        if info.has_sky_light
                            && let Some(layer) =
                                section.sky_light.and_then(|b| light_layer(b, true))
                        {
                            light.sky_mut()[light_index] = layer;
                        }
                        if !section.unknown.is_empty() {
                            section_tags.push(TagType::Byte.id());
                            push_name(&mut section_tags, "Y");
                            section_tags.push(y as u8);
                            section_tags.extend_from_slice(&section.unknown);
                            section_tags.push(0);
                            section_tag_count += 1;
                        }
                        let Some(index) = light_index.checked_sub(1).filter(|&i| i < count) else {
                            continue;
                        };
                        if let Some(section) = section.section {
                            sections[index] = Arc::new(section);
                        }
                    }
                    c.leave_list();
                }
                (b"Heightmaps", TagType::Compound) => {
                    c.enter_compound()?;
                    while let Some((ty, name)) = c.entry()? {
                        let kind = match name {
                            b"WORLD_SURFACE_WG" => HeightmapKind::WorldSurfaceWg,
                            b"WORLD_SURFACE" => HeightmapKind::WorldSurface,
                            b"OCEAN_FLOOR_WG" => HeightmapKind::OceanFloorWg,
                            b"OCEAN_FLOOR" => HeightmapKind::OceanFloor,
                            b"MOTION_BLOCKING" => HeightmapKind::MotionBlocking,
                            b"MOTION_BLOCKING_NO_LEAVES" => HeightmapKind::MotionBlockingNoLeaves,
                            _ => {
                                c.skip(ty)?;
                                continue;
                            }
                        };
                        if ty == TagType::LongArray {
                            heightmaps[usize::from(kind.id())] = c.array(ty)?;
                        } else {
                            c.skip(ty)?;
                        }
                    }
                }
                (b"block_entities", TagType::List) => {
                    let (element, n) = c.enter_list()?;
                    for _ in 0..n {
                        let raw = c.skip(element)?;
                        if element == TagType::Compound
                            && let Some(entity) = block_entity(raw)?
                        {
                            block_entities.push(entity);
                        }
                    }
                    c.leave_list();
                }
                (b"block_ticks", TagType::List) => block_ticks = read_ticks(c, &BLOCK, pos)?,
                (b"fluid_ticks", TagType::List) => fluid_ticks = read_ticks(c, &FLUID, pos)?,
                (b"PostProcessing", TagType::List) => post_processing_tag = Some(c.read(ty)?),
                (name, _) => {
                    c.skip(ty)?;
                    // Decoded tags of an unexpected type are dropped, like Vanilla ignores them.
                    if !DECODED.iter().any(|d| d.as_bytes() == name) {
                        extra.extend_from_slice(&at[..at.len() - c.rest().len()]);
                    }
                }
            }
        }

        let version = version.unwrap_or(0);
        if version > i64::from(DATA_VERSION) {
            return Err(invalid(format!(
                "chunk {pos:?} has data version {version}, newer than this server ({DATA_VERSION})"
            ))
            .into());
        }
        if version < i64::from(OLDEST_DATA_VERSION) {
            return Err(invalid(format!(
                "chunk {pos:?} has data version {version}; open the world once in Minecraft \
                 {} first",
                vanillite_data::version::VERSION_ID
            ))
            .into());
        }
        let stored = (x, z);
        if stored != (Some(i64::from(pos.x)), Some(i64::from(pos.z))) {
            return Err(invalid(format!("chunk {pos:?} is stored at {stored:?}")).into());
        }
        let status = match status.as_deref() {
            // Before 26.1, terrain was three statuses; their result is regenerated.
            Some("minecraft:noise" | "minecraft:surface" | "minecraft:carvers") => {
                ChunkStatus::Biomes
            }
            Some(name) => ChunkStatus::by_name(name)
                .ok_or_else(|| invalid(format!("chunk {pos:?}: unknown status {name}")))?,
            None => ChunkStatus::Empty,
        };
        let stored = heightmaps.map(|raw| {
            raw.and_then(|raw| Heightmap::from_raw(info.height, raw.as_chunks::<8>().0))
        });
        let chunk = Chunk::from_stored(min_section, sections, light, stored);

        let mut post_processing = Vec::new();
        if let Some(Tag::List(list)) = &post_processing_tag {
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

        if section_tag_count > 0 {
            extra.push(TagType::List.id());
            push_name(&mut extra, SECTION_TAGS);
            extra.push(TagType::Compound.id());
            extra.extend_from_slice(&section_tag_count.to_be_bytes());
            extra.extend_from_slice(&section_tags);
        }
        let extra = (!extra.is_empty()).then(|| {
            let mut bytes = Vec::with_capacity(extra.len() + 2);
            bytes.push(TagType::Compound.id());
            bytes.extend_from_slice(&extra);
            bytes.push(0);
            Arc::from(bytes)
        });
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

    /// One compound of the `sections` list.
    fn section<'b>(
        &self,
        c: &mut Cursor<'_, 'b>,
        cache: &mut PaletteCache<'b>,
        pos: ChunkPos,
    ) -> Result<SectionParts<'b>, ReadError> {
        let mut parts = SectionParts::default();
        let mut failed = None;
        let (mut blocks, mut biomes) = (None, None);
        c.enter_compound()?;
        loop {
            let at = c.rest();
            let Some((ty, name)) = c.entry()? else {
                break;
            };
            match (name, ty) {
                (b"Y", _) => parts.y = c.int(ty)?,
                (b"block_states", TagType::Compound) => {
                    match self.palette(c, cache, BLOCK_PALETTE)? {
                        Ok(parts) => blocks = Some(parts),
                        Err(e) => failed = Some(e),
                    }
                }
                (b"biomes", TagType::Compound) => match self.palette(c, cache, BIOME_PALETTE)? {
                    Ok((palette, data)) => match from_anvil::<Biomes>(&palette, data, 0) {
                        Ok(container) => biomes = Some(container),
                        Err(e) => failed = Some(e),
                    },
                    Err(e) => failed = Some(e),
                },
                (b"BlockLight", TagType::ByteArray) => parts.block_light = c.array(ty)?,
                (b"SkyLight", TagType::ByteArray) => parts.sky_light = c.array(ty)?,
                (name, _) => {
                    c.skip(ty)?;
                    if !SECTION_DECODED.iter().any(|d| d.as_bytes() == name) {
                        parts
                            .unknown
                            .extend_from_slice(&at[..at.len() - c.rest().len()]);
                    }
                }
            }
        }
        if failed.is_none() && (blocks.is_some() || biomes.is_some()) {
            let biomes = biomes.unwrap_or_else(|| PalettedContainer::new(self.info.default_biome));
            match blocks {
                Some((palette, data)) => match blocks_section(&palette, data, biomes) {
                    Ok(section) => parts.section = Some(section),
                    Err(e) => failed = Some(e),
                },
                None => {
                    let blocks = PalettedContainer::new(self.info.air);
                    parts.section = Some(Section::from_containers(blocks, biomes));
                }
            }
        }
        if let Some(e) = failed {
            return Err(invalid(format!("chunk {pos:?} section {:?}: {e}", parts.y)).into());
        }
        Ok(parts)
    }

    /// A `block_states` or `biomes` compound: its palette as ids and its packed data.
    #[allow(clippy::type_complexity)]
    fn palette<'b>(
        &self,
        c: &mut Cursor<'_, 'b>,
        cache: &mut PaletteCache<'b>,
        kind: u8,
    ) -> Result<io::Result<(Vec<u16>, Option<&'b [u8]>)>, ReadError> {
        let mut palette = None;
        let mut data = None;
        c.enter_compound()?;
        while let Some((ty, name)) = c.entry()? {
            match (name, ty) {
                (b"palette", TagType::List) => {
                    let (element, n) = c.enter_list()?;
                    let mut ids = Vec::with_capacity(n.min(4096));
                    for _ in 0..n {
                        let raw = c.skip(element)?;
                        let id = match cache.get(&(kind, raw)) {
                            Some(&id) => id,
                            None => {
                                let id = if kind == BLOCK_PALETTE {
                                    self.block_state(element, raw)?
                                } else {
                                    self.biome(element, raw)?
                                };
                                cache.insert((kind, raw), id);
                                id
                            }
                        };
                        ids.push(id);
                    }
                    c.leave_list();
                    palette = Some(ids);
                }
                (b"data", TagType::LongArray) => data = c.array(ty)?,
                _ => {
                    c.skip(ty)?;
                }
            }
        }
        Ok(match palette {
            Some(palette) => Ok((palette, data)),
            None if kind == BLOCK_PALETTE => Err(invalid("block_states without palette")),
            None => Err(invalid("biomes without palette")),
        })
    }

    /// A block palette entry from its payload: a block name (its default state, also wrapped
    /// as `{"": name}`) or `{id|Name, properties|Properties}`.
    fn block_state(&self, element: TagType, raw: &[u8]) -> vanillite_nbt::Result<u16> {
        let mut budget = Budget::file();
        let mut c = Cursor::new(raw, &mut budget);
        let mut name = None;
        let mut old_name = None;
        let mut properties: Vec<(Cow<'_, str>, Cow<'_, str>)> = Vec::new();
        let mut old_properties: Vec<(Cow<'_, str>, Cow<'_, str>)> = Vec::new();
        match element {
            TagType::String => name = c.str(element)?,
            TagType::Compound => {
                c.enter_compound()?;
                while let Some((ty, key)) = c.entry()? {
                    match (key, ty) {
                        // `{"": name}` wraps a plain name in a mixed list (26.x writes palettes
                        // with default and other states that way).
                        (b"" | b"id", _) => name = c.str(ty)?,
                        (b"Name", _) => old_name = c.str(ty)?,
                        (b"properties" | b"Properties", TagType::Compound) => {
                            let into = if key == b"properties" {
                                &mut properties
                            } else {
                                &mut old_properties
                            };
                            c.enter_compound()?;
                            while let Some((ty, property)) = c.entry()? {
                                if let Some(value) = c.str(ty)? {
                                    into.push((mutf8_name(property)?, value));
                                }
                            }
                        }
                        _ => {
                            c.skip(ty)?;
                        }
                    }
                }
                if properties.is_empty() {
                    properties = old_properties;
                }
            }
            _ => {}
        }
        let name = name.or(old_name).unwrap_or_default();
        let Some(block) = BlockId::by_name(&name) else {
            log::warn!("Unknown block {name:?} replaced by air");
            return Ok(self.info.air);
        };
        let mut state = block.default_state();
        for (key, value) in &properties {
            if let Some(with) = state.with(key, value) {
                state = with;
            }
        }
        Ok(state.0)
    }

    /// A biome palette entry from its payload; unknown biomes become the default biome.
    fn biome(&self, element: TagType, raw: &[u8]) -> vanillite_nbt::Result<u16> {
        let mut budget = Budget::file();
        let mut c = Cursor::new(raw, &mut budget);
        let mut name = None;
        if element == TagType::Compound {
            // `{"": name}` in a mixed list.
            c.enter_compound()?;
            while let Some((ty, key)) = c.entry()? {
                match key {
                    b"" => name = c.str(ty)?,
                    _ => {
                        c.skip(ty)?;
                    }
                }
            }
        } else {
            name = c.str(element)?;
        }
        Ok(name
            .and_then(|name| self.biome_ids.get(name.as_ref()).copied())
            .unwrap_or(self.info.default_biome))
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
        // Vanilla writes the flag only when set.
        if chunk.light_on {
            root.insert("isLightOn", true);
        }
        let mut section_tags: Vec<Compound> = match root.remove(SECTION_TAGS) {
            Some(Tag::List(list)) => list
                .0
                .into_iter()
                .filter_map(|tag| match tag {
                    Tag::Compound(c) => Some(c),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };

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
            // Like Vanilla, the layers are written also while the light is not trusted yet.
            if let Some(bytes) = layer_bytes(&light.block()[light_index], false) {
                section.insert("BlockLight", bytes);
            }
            if let Some(bytes) = layer_bytes(&light.sky()[light_index], true) {
                section.insert("SkyLight", bytes);
            }
            for tags in &mut section_tags {
                if tags.get("Y").and_then(Tag::as_i64) == Some(i64::from(y)) {
                    for (key, tag) in std::mem::take(tags).iter() {
                        if key != "Y" {
                            section.insert(key, tag.clone());
                        }
                    }
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
        // Vanilla writes the list of every section, also when all are empty.
        {
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
fn read_ticks(
    c: &mut Cursor<'_, '_>,
    registry: &Registry,
    chunk: ChunkPos,
) -> vanillite_nbt::Result<Vec<SavedTick>> {
    let (element, n) = c.enter_list()?;
    let mut ticks = Vec::with_capacity(n.min(4096));
    for _ in 0..n {
        if element != TagType::Compound {
            c.skip(element)?;
            continue;
        }
        let (mut kind, mut x, mut y, mut z, mut delay, mut priority) =
            (None, None, None, None, None, None);
        c.enter_compound()?;
        while let Some((ty, name)) = c.entry()? {
            let slot = match name {
                b"i" => {
                    kind = c.str(ty)?.and_then(|name| registry.id(&name));
                    continue;
                }
                b"x" => &mut x,
                b"y" => &mut y,
                b"z" => &mut z,
                b"t" => &mut delay,
                b"p" => &mut priority,
                _ => {
                    c.skip(ty)?;
                    continue;
                }
            };
            *slot = c.int(ty)?.and_then(|v| i32::try_from(v).ok());
        }
        let (Some(kind), Some(x), Some(y), Some(z), Some(delay)) = (kind, x, y, z, delay) else {
            continue;
        };
        let pos = BlockPos::new(x, y, z);
        if pos.chunk() == chunk {
            ticks.push(SavedTick {
                kind,
                pos,
                delay,
                // `TickPriority.byValue` clamps unknown values.
                priority: priority.unwrap_or(0).clamp(-3, 3) as i8,
            });
        }
    }
    c.leave_list();
    Ok(ticks)
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

/// A stored light layer. Uniform arrays become uniform layers, except all-zero block light:
/// Vanilla keeps such a layer as data (sent with its array, saved again), unlike a missing one.
fn light_layer(bytes: &[u8], sky: bool) -> Option<LightNibbles> {
    let array: &[u8; NIBBLE_BYTES] = bytes.try_into().ok()?;
    let first = array[0];
    if (sky || first != 0) && first >> 4 == first & 15 && array.iter().all(|&b| b == first) {
        return Some(LightNibbles::Uniform(first & 15));
    }
    Some(LightNibbles::Data(Arc::new(*array)))
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

/// A section from an Anvil block palette and its packed indices. Data stored with the bits
/// Vanilla also uses in memory (the common case) is validated and counted in one pass.
fn blocks_section(
    palette: &[u16],
    data: Option<&[u8]>,
    biomes: PalettedContainer<Biomes>,
) -> io::Result<Section> {
    if let (Some(longs), [_, _, ..]) = (data, palette) {
        let len = u32::try_from(palette.len()).unwrap_or(u32::MAX);
        let bits = facts::ceil_log2(len).max(4);
        let words: &[Word] = longs.as_chunks::<8>().0;
        if BlockStates::indirect_bits(facts::ceil_log2(len).max(1)) == Some(bits)
            && words.len() == bits::words_for(BlockStates::ENTRIES, bits)
        {
            return Section::from_parts(bits, palette, words, biomes)
                .map_err(|e| invalid(format!("{e:?}")));
        }
    }
    let blocks = from_anvil::<BlockStates>(palette, data, 4)?;
    Ok(Section::from_containers(blocks, biomes))
}

/// A container from an Anvil palette and its packed indices (`bits = max(min_bits,
/// ceillog2(len))`), converted to the configuration Vanilla would use in memory.
fn from_anvil<S: Strategy>(
    palette: &[u16],
    data: Option<&[u8]>,
    min_bits: u8,
) -> io::Result<PalettedContainer<S>> {
    let decode = |e| invalid(format!("{e:?}"));
    match palette {
        [] => return Err(invalid("empty palette")),
        [single] => return Ok(PalettedContainer::new(*single)),
        _ => {}
    }
    let Some(longs) = data else {
        return Err(invalid("palette without data"));
    };
    let words: &[Word] = longs.as_chunks::<8>().0;
    let len = u32::try_from(palette.len()).unwrap_or(u32::MAX);
    let stored_bits = facts::ceil_log2(len).max(min_bits);
    if words.len() != bits::words_for(S::ENTRIES, stored_bits) {
        return Err(invalid(format!(
            "data length {} does not fit {} bits",
            words.len(),
            stored_bits
        )));
    }
    let wanted = S::indirect_bits(facts::ceil_log2(len).max(1));
    if wanted == Some(stored_bits) {
        return PalettedContainer::from_parts(stored_bits, palette, words).map_err(decode);
    }
    // Repack into the configuration of this palette size (indirect or global).
    let mut bad = false;
    let value = |index: usize| {
        let i = bits::get(words, stored_bits, index) as usize;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Chunks written by Vanilla 26.3 (generated world): a full chunk with block entities and
    /// ticks, a full chunk with an all-zero block light array, and a proto chunk (`terrain`).
    const GOLDEN: [((i32, i32), &[u8]); 3] = [
        (
            (-22, -32),
            include_bytes!("../tests/data/chunk.-22.-32.zlib"),
        ),
        (
            (-30, -26),
            include_bytes!("../tests/data/chunk.-30.-26.zlib"),
        ),
        (
            (-31, -84),
            include_bytes!("../tests/data/chunk.-31.-84.zlib"),
        ),
    ];

    fn golden() -> Vec<(ChunkPos, Vec<u8>, Compound)> {
        GOLDEN
            .iter()
            .map(|&((x, z), data)| {
                let bytes = crate::Compression::Deflate.decompress(data).unwrap();
                let root =
                    vanillite_nbt::read::read_named(&mut bytes.as_slice(), &mut Budget::file())
                        .unwrap();
                (ChunkPos::new(x, z), bytes, root)
            })
            .collect()
    }

    /// An overworld codec knowing every biome of the golden chunks.
    fn codec(chunks: &[(ChunkPos, Vec<u8>, Compound)]) -> Codec {
        vanillite_chunk::facts::install_vanilla(&[], &[], 65);
        let mut biomes = vec!["minecraft:plains".to_owned()];
        for (_, _, root) in chunks {
            let Some(Tag::List(sections)) = root.get("sections") else {
                continue;
            };
            for section in sections.0.iter().filter_map(Tag::as_compound) {
                let Some(Tag::List(palette)) = section
                    .get("biomes")
                    .and_then(Tag::as_compound)
                    .and_then(|b| b.get("palette"))
                else {
                    continue;
                };
                for name in palette.0.iter().filter_map(Tag::as_str) {
                    if !biomes.iter().any(|b| b == name) {
                        biomes.push(name.to_owned());
                    }
                }
            }
        }
        Codec::new(&WorldInfo {
            dimension: "minecraft:overworld".to_owned(),
            dir: std::path::PathBuf::new(),
            min_y: -64,
            height: 384,
            seed: 0,
            has_sky_light: true,
            biomes: biomes.into(),
            default_biome: 0,
            air: vanillite_data::block::BlockState::AIR.0,
        })
    }

    /// The tag with every compound's entries sorted by name (Vanilla writes them in hash order).
    fn sorted(tag: &Tag) -> Tag {
        match tag {
            Tag::Compound(c) => Tag::Compound(sorted_compound(c)),
            Tag::List(list) => Tag::List(List(list.0.iter().map(sorted).collect())),
            other => other.clone(),
        }
    }

    fn sorted_compound(c: &Compound) -> Compound {
        let mut entries: Vec<(&str, &Tag)> = c.iter().collect();
        entries.sort_by_key(|(name, _)| *name);
        let mut out = Compound::new();
        for (name, tag) in entries {
            out.insert(name, sorted(tag));
        }
        out
    }

    /// Paths where two tags differ, with both values shortened.
    fn diff(a: &Tag, b: &Tag, path: String, out: &mut Vec<String>) {
        match (a, b) {
            (Tag::Compound(a), Tag::Compound(b)) => {
                for (name, tag) in a.iter() {
                    match b.get(name) {
                        Some(other) => diff(tag, other, format!("{path}.{name}"), out),
                        None => out.push(format!("{path}.{name}: only written")),
                    }
                }
                for (name, _) in b.iter().filter(|(name, _)| a.get(name).is_none()) {
                    out.push(format!("{path}.{name}: only in Vanilla's file"));
                }
            }
            (Tag::List(a), Tag::List(b)) if a.0.len() == b.0.len() => {
                for (i, (a, b)) in a.0.iter().zip(&b.0).enumerate() {
                    diff(a, b, format!("{path}[{i}]"), out);
                }
            }
            (a, b) if a != b => {
                let short = |t: &Tag| format!("{t:?}").chars().take(120).collect::<String>();
                out.push(format!(
                    "{path}: written {} / Vanilla {}",
                    short(a),
                    short(b)
                ));
            }
            _ => {}
        }
    }

    fn without(c: &Compound, names: &[&str]) -> Compound {
        let mut c = sorted_compound(c);
        for name in names {
            c.remove(name);
        }
        c
    }

    /// Vanilla's chunks decode and encode back to the same NBT: blocks, biomes, light (also an
    /// all-zero block light array), stored heightmaps, block entities, ticks and every tag that
    /// is not decoded. Proto chunks keep only the heightmaps of a live chunk.
    #[test]
    fn vanilla_chunks_encode_back_the_same() {
        let chunks = golden();
        let codec = codec(&chunks);
        for (pos, bytes, root) in &chunks {
            let chunk = codec.read_bytes(*pos, bytes).unwrap();
            let full = chunk.status == ChunkStatus::Full;
            if full {
                // Heightmaps are taken from the file: priming with the test's empty block facts
                // would give other heights.
                let mut primed = chunk.chunk.clone();
                primed.prime_heightmaps();
                assert_ne!(primed.heightmaps(), chunk.chunk.heightmaps());
            }
            let ignored: &[&str] = if full {
                &["DataVersion"]
            } else {
                &["DataVersion", "Heightmaps"]
            };
            let written = codec.write(&chunk).unwrap();
            let mut diffs = Vec::new();
            diff(
                &Tag::Compound(without(&written, ignored)),
                &Tag::Compound(without(root, ignored)),
                String::new(),
                &mut diffs,
            );
            assert!(diffs.is_empty(), "{pos:?}: {diffs:#?}");
        }
        let zero_light = codec.read_bytes(chunks[1].0, &chunks[1].1).unwrap();
        assert!(zero_light.chunk.light().block().iter().any(
            |layer| matches!(layer, LightNibbles::Data(data) if data.iter().all(|&b| b == 0))
        ));
    }

    /// Tags inside a section that are not decoded are kept, and a decoded chunk encodes the
    /// same after another round trip.
    #[test]
    fn unknown_section_tags_survive_round_trips() {
        let chunks = golden();
        let codec = codec(&chunks);
        let (pos, _, root) = &chunks[0];
        let mut root = root.clone();
        let Some(Tag::List(sections)) = root.remove("sections") else {
            panic!("no sections");
        };
        let mut sections = sections.0;
        let Some(Tag::Compound(section)) = sections.get_mut(3) else {
            panic!("no section 3");
        };
        let y = section.get("Y").cloned();
        section.insert("plugin:data", Compound::new().with("level", 7));
        root.insert("sections", List(sections));
        let once = codec.write(&codec.read(*pos, root).unwrap()).unwrap();
        let twice = codec
            .write(&codec.read(*pos, once.clone()).unwrap())
            .unwrap();
        assert_eq!(once, twice);
        assert!(once.get(SECTION_TAGS).is_none());
        let Some(Tag::List(sections)) = once.get("sections") else {
            panic!("no sections written");
        };
        let section = sections
            .0
            .iter()
            .filter_map(Tag::as_compound)
            .find(|s| s.get("Y") == y.as_ref())
            .unwrap();
        assert_eq!(
            section.get("plugin:data"),
            Some(&Tag::Compound(Compound::new().with("level", 7)))
        );
        assert!(section.get("block_states").is_some());
    }

    /// A stored heightmap of the wrong size or with heights above the chunk is primed instead.
    #[test]
    fn invalid_heightmaps_are_primed() {
        let chunks = golden();
        let codec = codec(&chunks);
        let (pos, _, root) = &chunks[0];
        let mut root = root.clone();
        let mut heightmaps = root
            .remove("Heightmaps")
            .and_then(|t| t.as_compound().cloned())
            .unwrap();
        heightmaps.insert("MOTION_BLOCKING", vec![0i64; 3]);
        heightmaps.insert("WORLD_SURFACE", vec![-1i64; 37]);
        root.insert("Heightmaps", heightmaps);
        let chunk = codec.read(*pos, root).unwrap().chunk;
        let mut primed = chunk.clone();
        primed.prime_heightmaps();
        for kind in [HeightmapKind::MotionBlocking, HeightmapKind::WorldSurface] {
            assert_eq!(chunk.heightmaps().get(kind), primed.heightmaps().get(kind));
        }
        assert_ne!(
            chunk.heightmaps().get(HeightmapKind::OceanFloor),
            primed.heightmaps().get(HeightmapKind::OceanFloor)
        );
    }
}

//! The chunk codec of the own world format (#25): a chunk record is one zstd frame (level 3 by
//! default, no dictionary) of the chunk in the server's own layout.
//!
//! Fields, in this order, so readers that need only the front can stop early (§4.1 of the
//! storage design):
//! 1. header: record version, status, flags, data version, position, section range,
//!    inhabited time, last update (since record version 2);
//! 2. heightmaps in their packed form (taken over on load instead of primed);
//! 3. `extra`: every chunk tag the server does not decode (structure starts and references,
//!    plugin data, …) as network NBT, written back unchanged;
//! 4. biome names of the ids used in the record, then the sections exactly as on the wire
//!    (block states, biomes), light per layer, block entities, typed scheduled ticks and
//!    post-processing marks.
//!
//! Block state, block, fluid and block entity type ids are those of the record's data version;
//! records of older versions are mapped on load ([`crate::upgrade`]). Biomes are kept by name,
//! so data packs that change the biome registry do not shift them.

use std::io;
use std::sync::Arc;

use vanillite_base::hash::FxHashMap;
use vanillite_base::{BlockPos, ChunkPos};
use vanillite_chunk::bits::{self, Word};
use vanillite_chunk::light::{ChunkLight, LightNibbles, NIBBLE_BYTES};
use vanillite_chunk::slots::{BlockEntityData, ChunkStatus, SavedTick, WorldInfo};
use vanillite_chunk::store::{ChunkCodec, RecordMeta};
use vanillite_chunk::{
    Biomes, BlockStates, Chunk, Heightmap, HeightmapKind, PalettedContainer, ProtoChunk, Section,
    Strategy,
};
use vanillite_data::version::DATA_VERSION;

use crate::compression::{zstd_compress, zstd_decompress};
use crate::upgrade::{IdRemap, Upgrades};

/// Version of the record layout.
const RECORD_VERSION: u8 = 2;
/// Default zstd level (`storage.own-format.compression-level`).
pub const DEFAULT_LEVEL: i32 = 3;

/// Record flags, also kept in the index entry.
pub const FLAG_LIGHT_ON: u8 = 1;
/// The chunk has block entities.
pub const FLAG_BLOCK_ENTITIES: u8 = 2;
/// The chunk has scheduled ticks.
pub const FLAG_TICKS: u8 = 4;
/// `extra` is present.
const FLAG_EXTRA: u8 = 8;

const LIGHT_ABSENT: u8 = 0;
const LIGHT_UNIFORM: u8 = 1;
const LIGHT_DATA: u8 = 2;
const NO_KIND: u16 = u16::MAX;

fn invalid(pos: ChunkPos, message: impl std::fmt::Display) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("chunk {}, {}: {message}", pos.x, pos.z),
    )
}

/// The own-format chunk codec of one world.
#[derive(Debug)]
pub struct OwnCodec {
    info: WorldInfo,
    biome_ids: FxHashMap<String, u16>,
    level: i32,
    upgrades: Arc<Upgrades>,
}

impl OwnCodec {
    /// The codec of a world; records are compressed with zstd `level` (1–19).
    pub fn new(info: &WorldInfo, level: i32) -> Self {
        OwnCodec {
            info: info.clone(),
            biome_ids: info
                .biomes
                .iter()
                .zip(0u16..)
                .map(|(name, id)| (name.clone(), id))
                .collect(),
            level,
            upgrades: Upgrades::none(),
        }
    }

    /// The codec that also reads records of the older data versions of `upgrades`.
    pub fn with_upgrades(mut self, upgrades: Arc<Upgrades>) -> Self {
        self.upgrades = upgrades;
        self
    }

    /// The uncompressed record of a chunk and its metadata.
    pub fn encode_raw(&self, chunk: &ProtoChunk) -> (RecordMeta, Vec<u8>) {
        let mut out = Vec::with_capacity(48 * 1024);
        let c = &chunk.chunk;
        let mut flags = 0;
        if chunk.light_on {
            flags |= FLAG_LIGHT_ON;
        }
        if !chunk.block_entities.is_empty() {
            flags |= FLAG_BLOCK_ENTITIES;
        }
        if !chunk.block_ticks.is_empty() || !chunk.fluid_ticks.is_empty() {
            flags |= FLAG_TICKS;
        }
        if chunk.extra.is_some() {
            flags |= FLAG_EXTRA;
        }
        // 1. Header.
        out.push(RECORD_VERSION);
        out.push(chunk.status as u8);
        out.push(flags);
        out.extend_from_slice(&(DATA_VERSION as u32).to_le_bytes());
        out.extend_from_slice(&chunk.pos.x.to_le_bytes());
        out.extend_from_slice(&chunk.pos.z.to_le_bytes());
        out.extend_from_slice(&c.min_section().to_le_bytes());
        out.extend_from_slice(&(c.sections().len() as u16).to_le_bytes());
        out.extend_from_slice(&chunk.inhabited_time.to_le_bytes());
        out.extend_from_slice(&chunk.last_update.to_le_bytes());
        // 2. Heightmaps.
        let mask = c
            .heightmaps()
            .iter()
            .fold(0u8, |mask, (kind, _)| mask | 1 << kind.id());
        out.push(mask);
        for (_, map) in c.heightmaps().iter() {
            out.extend_from_slice(map.raw().as_flattened());
        }
        // 3. Extra.
        if let Some(extra) = &chunk.extra {
            out.extend_from_slice(&(extra.len() as u32).to_le_bytes());
            out.extend_from_slice(extra);
        }
        // 4. Biome names, sections, light, block entities, ticks, post-processing.
        let mut biomes: Vec<u16> = Vec::new();
        for section in c.sections() {
            section.biomes().for_each_count(|id, _| {
                if !biomes.contains(&id) {
                    biomes.push(id);
                }
            });
        }
        out.extend_from_slice(&(biomes.len() as u16).to_le_bytes());
        for id in biomes {
            let name = self
                .info
                .biomes
                .get(usize::from(id))
                .map_or("minecraft:plains", String::as_str);
            out.extend_from_slice(&id.to_le_bytes());
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(name.as_bytes());
        }
        for section in c.sections() {
            section.blocks().write(&mut out);
            section.biomes().write(&mut out);
        }
        let light = c.light();
        for layer in light.sky().iter().chain(light.block()) {
            match layer {
                LightNibbles::Absent => out.push(LIGHT_ABSENT),
                LightNibbles::Uniform(level) => {
                    out.push(LIGHT_UNIFORM);
                    out.push(*level);
                }
                LightNibbles::Data(data) => {
                    out.push(LIGHT_DATA);
                    out.extend_from_slice(&data[..]);
                }
            }
        }
        out.extend_from_slice(&(chunk.block_entities.len() as u32).to_le_bytes());
        for entity in &chunk.block_entities {
            put_pos(&mut out, entity.pos);
            out.extend_from_slice(&entity.kind.unwrap_or(NO_KIND).to_le_bytes());
            out.extend_from_slice(&(entity.nbt.len() as u32).to_le_bytes());
            out.extend_from_slice(&entity.nbt);
        }
        for ticks in [&chunk.block_ticks, &chunk.fluid_ticks] {
            out.extend_from_slice(&(ticks.len() as u32).to_le_bytes());
            for tick in ticks.iter() {
                out.extend_from_slice(&tick.kind.to_le_bytes());
                put_pos(&mut out, tick.pos);
                out.extend_from_slice(&tick.delay.to_le_bytes());
                out.push(tick.priority as u8);
            }
        }
        out.extend_from_slice(&(chunk.post_processing.len() as u32).to_le_bytes());
        for &pos in &chunk.post_processing {
            put_pos(&mut out, pos);
        }
        let meta = RecordMeta {
            data_version: DATA_VERSION as u32,
            raw_len: u32::try_from(out.len()).unwrap_or(u32::MAX),
            checksum: 0,
            status: chunk.status as u8,
            flags: flags & !FLAG_EXTRA,
        };
        (meta, out)
    }

    /// Decodes an uncompressed record.
    pub fn decode_raw(&self, pos: ChunkPos, raw: &[u8]) -> io::Result<ProtoChunk> {
        let mut r = Reader { bytes: raw, pos };
        let info = &self.info;
        let version = r.u8()?;
        if !(1..=RECORD_VERSION).contains(&version) {
            return Err(invalid(pos, "unknown record version"));
        }
        let status = *ChunkStatus::ALL
            .get(usize::from(r.u8()?))
            .ok_or_else(|| invalid(pos, "unknown status"))?;
        let flags = r.u8()?;
        let data_version = r.u32()?;
        let ids = if data_version == DATA_VERSION as u32 {
            None
        } else {
            Some(self.upgrades.get(data_version).ok_or_else(|| {
                invalid(
                    pos,
                    format!(
                        "written by data version {data_version}, this server is {DATA_VERSION} \
                         and has no id table of it"
                    ),
                )
            })?)
        };
        let (x, z) = (r.i32()?, r.i32()?);
        if (x, z) != (pos.x, pos.z) {
            return Err(invalid(pos, format!("record of chunk {x}, {z}")));
        }
        let min_section = r.i32()?;
        let count = usize::from(r.u16()?);
        if min_section != info.min_y >> 4 || count as u32 != info.height / 16 {
            return Err(invalid(pos, "stored for another world height"));
        }
        let inhabited_time = r.i64()?;
        let last_update = if version >= 2 { r.i64()? } else { 0 };

        let mask = r.u8()?;
        let words = Heightmap::new(info.height).raw().len();
        let mut stored: [Option<Heightmap>; 6] = Default::default();
        for kind in HeightmapKind::ALL {
            if mask & 1 << kind.id() != 0 {
                let raw = r.take(words * 8)?;
                stored[usize::from(kind.id())] =
                    Heightmap::from_raw(info.height, raw.as_chunks::<8>().0);
            }
        }

        let extra = if flags & FLAG_EXTRA != 0 {
            let len = r.u32()? as usize;
            Some(Arc::from(r.take(len)?))
        } else {
            None
        };

        let mut remap: Vec<(u16, u16)> = Vec::new();
        for _ in 0..r.u16()? {
            let id = r.u16()?;
            let len = usize::from(r.u16()?);
            let name = std::str::from_utf8(r.take(len)?)
                .map_err(|_| invalid(pos, "biome name is not UTF-8"))?;
            let current = self.biome_ids.get(name).copied().unwrap_or_else(|| {
                log::warn!(
                    "Chunk {}, {}: unknown biome {name}, using the default biome",
                    pos.x,
                    pos.z
                );
                info.default_biome
            });
            remap.push((id, current));
        }
        let identity = remap.iter().all(|(old, new)| old == new);
        let mut sections = Vec::with_capacity(count);
        let mut palette = Vec::with_capacity(256);
        for _ in 0..count {
            // Block states in wire form; the summary is counted in the pass that validates them.
            let bits = r.u8()?;
            palette.clear();
            let words = if bits == 0 {
                palette.push(r.varint_u16()?);
                0
            } else {
                if BlockStates::indirect_bits(bits) == Some(bits) {
                    let len = r.varint()?;
                    if len == 0 || len > 1 << bits {
                        return Err(invalid(pos, "invalid block palette"));
                    }
                    for _ in 0..len {
                        palette.push(r.varint_u16()?);
                    }
                }
                bits::words_for(BlockStates::ENTRIES, bits)
            };
            let data = r.take(words * 8)?.as_chunks::<8>().0;
            let biomes = read_biomes(&mut r, &remap, identity)?;
            let section = match ids {
                None => Section::from_parts(bits, &palette, data, biomes),
                Some(ids) => remap_section(pos, ids, bits, &mut palette, data, biomes)?,
            }
            .map_err(|e| invalid(pos, e))?;
            sections.push(Arc::new(section));
        }
        let mut light = ChunkLight::new(count);
        for layer in 0..2 * (count + 2) {
            let value = match r.u8()? {
                LIGHT_ABSENT => LightNibbles::Absent,
                LIGHT_UNIFORM => LightNibbles::Uniform(r.u8()? & 15),
                LIGHT_DATA => {
                    let data: [u8; NIBBLE_BYTES] = r.take(NIBBLE_BYTES)?.try_into().unwrap();
                    LightNibbles::Data(Arc::new(data))
                }
                _ => return Err(invalid(pos, "unknown light layer")),
            };
            if layer < count + 2 {
                light.sky_mut()[layer] = value;
            } else {
                light.block_mut()[layer - count - 2] = value;
            }
        }
        let chunk = Chunk::from_stored(min_section, sections, light, stored);

        let entities = r.u32()? as usize;
        let mut block_entities = Vec::with_capacity(entities.min(4096));
        for _ in 0..entities {
            let at = r.pos()?;
            let kind = r.u16()?;
            let len = r.u32()? as usize;
            let kind = (kind != NO_KIND).then_some(kind);
            block_entities.push(BlockEntityData {
                pos: at,
                kind: match ids {
                    None => kind,
                    Some(ids) => kind.and_then(|kind| ids.block_entity_type(kind)),
                },
                nbt: Arc::from(r.take(len)?),
            });
        }
        let mut ticks = [Vec::new(), Vec::new()];
        for (fluids, list) in ticks.iter_mut().enumerate() {
            let n = r.u32()? as usize;
            list.reserve(n.min(65_536));
            for _ in 0..n {
                let tick = SavedTick {
                    kind: r.u16()?,
                    pos: r.pos()?,
                    delay: r.i32()?,
                    priority: r.u8()? as i8,
                };
                // Ticks of blocks and fluids that are gone are dropped, as Vanilla does.
                let kind = match ids {
                    None => Some(tick.kind),
                    Some(ids) if fluids == 1 => ids.fluid(tick.kind),
                    Some(ids) => ids.block(tick.kind),
                };
                if let Some(kind) = kind {
                    list.push(SavedTick { kind, ..tick });
                }
            }
        }
        let [block_ticks, fluid_ticks] = ticks;
        let n = r.u32()? as usize;
        let mut post_processing = Vec::with_capacity(n.min(65_536));
        for _ in 0..n {
            post_processing.push(r.pos()?);
        }
        if !r.bytes.is_empty() {
            return Err(invalid(pos, "trailing bytes"));
        }
        Ok(ProtoChunk {
            pos,
            status,
            chunk,
            block_entities,
            inhabited_time,
            last_update,
            light_on: flags & FLAG_LIGHT_ON != 0,
            block_ticks,
            fluid_ticks,
            post_processing,
            entities: Vec::new(),
            poi: None,
            extra,
            generation: None,
        })
    }
}

/// A section whose block states in wire form (`bits`, `palette`, `data`) are ids of an older
/// data version: the palette is mapped; where that merges entries or the data is direct, the
/// states are packed again.
fn remap_section(
    pos: ChunkPos,
    remap: &IdRemap,
    bits: u8,
    palette: &mut [u16],
    data: &[Word],
    biomes: PalettedContainer<Biomes>,
) -> io::Result<Result<Section, vanillite_chunk::DecodeError>> {
    let map = |old: u16| {
        remap.state(old).ok_or_else(|| {
            invalid(
                pos,
                format!(
                    "block state {old} is not in the id table of data version {}",
                    remap.data_version()
                ),
            )
        })
    };
    let indirect = bits == 0 || BlockStates::indirect_bits(bits) == Some(bits);
    if indirect {
        for entry in palette.iter_mut() {
            *entry = map(*entry)?;
        }
        let unique = palette
            .iter()
            .enumerate()
            .all(|(i, entry)| !palette[..i].contains(entry));
        if unique {
            return Ok(Section::from_parts(bits, palette, data, biomes));
        }
    } else if bits > 16 || data.len() != bits::words_for(BlockStates::ENTRIES, bits) {
        return Ok(Err(vanillite_chunk::DecodeError::InvalidBits(bits)));
    }
    let mut states = vec![0u16; BlockStates::ENTRIES];
    for (index, state) in states.iter_mut().enumerate() {
        let value = bits::get(data, bits, index);
        *state = if indirect {
            *palette
                .get(value as usize)
                .ok_or_else(|| invalid(pos, "invalid block palette index"))?
        } else {
            map(u16::try_from(value).map_err(|_| invalid(pos, "block state too large"))?)?
        };
    }
    // Packed again like the container would hold them.
    let mut packed: Vec<u16> = Vec::new();
    let mut index_of: FxHashMap<u16, u32> = FxHashMap::default();
    for &state in &states {
        index_of.entry(state).or_insert_with(|| {
            packed.push(state);
            packed.len() as u32 - 1
        });
    }
    if packed.len() == 1 {
        return Ok(Section::from_parts(0, &packed, &[], biomes));
    }
    let needed = (u32::BITS - (packed.len() as u32 - 1).leading_zeros()) as u8;
    let (bits, packed) = match BlockStates::indirect_bits(needed) {
        Some(bits) => (bits, packed),
        None => (BlockStates::global_bits(), Vec::new()),
    };
    let mut data = vec![Word::default(); bits::words_for(BlockStates::ENTRIES, bits)];
    for (index, &state) in states.iter().enumerate() {
        let value = if packed.is_empty() {
            u32::from(state)
        } else {
            index_of[&state]
        };
        bits::set(&mut data, bits, index, value);
    }
    Ok(Section::from_parts(bits, &packed, &data, biomes))
}

/// A biome container in wire form whose ids are mapped through `remap` (old → current).
fn read_biomes(
    r: &mut Reader<'_>,
    remap: &[(u16, u16)],
    identity: bool,
) -> io::Result<PalettedContainer<Biomes>> {
    let pos = r.pos;
    let map = |id: u16| {
        remap
            .iter()
            .find(|(old, _)| *old == id)
            .map(|(_, new)| *new)
            .ok_or_else(|| invalid(pos, format!("biome id {id} without a name")))
    };
    let bits = r.u8()?;
    if bits == 0 {
        let value =
            vanillite_chunk::wire::read_varint(&mut r.bytes).map_err(|e| invalid(pos, e))?;
        let value = u16::try_from(value).map_err(|_| invalid(pos, "biome id too large"))?;
        return Ok(PalettedContainer::new(map(value)?));
    }
    if let Some(config) = Biomes::indirect_bits(bits)
        && config == bits
    {
        let len = r.varint()?;
        if len == 0 || len > 1 << bits {
            return Err(invalid(pos, "invalid biome palette"));
        }
        let mut palette = Vec::with_capacity(len as usize);
        for _ in 0..len {
            let value =
                vanillite_chunk::wire::read_varint(&mut r.bytes).map_err(|e| invalid(pos, e))?;
            palette.push(map(
                u16::try_from(value).map_err(|_| invalid(pos, "biome id"))?
            )?);
        }
        let data = r.take(bits::words_for(Biomes::ENTRIES, bits) * 8)?;
        return PalettedContainer::from_parts(bits, &palette, data.as_chunks::<8>().0)
            .map_err(|e| invalid(pos, e));
    }
    // Packed ids with the bit count of the registry the record was written with.
    if bits > 16 {
        return Err(invalid(pos, "invalid biome bits"));
    }
    let data: &[Word] = r
        .take(bits::words_for(Biomes::ENTRIES, bits) * 8)?
        .as_chunks::<8>()
        .0;
    if identity && bits == Biomes::global_bits() {
        return PalettedContainer::from_parts(bits, &[], data).map_err(|e| invalid(pos, e));
    }
    let mut container = PalettedContainer::new(map(bits::get(data, bits, 0) as u16)?);
    for index in 1..Biomes::ENTRIES {
        container.set(index, map(bits::get(data, bits, index) as u16)?);
    }
    Ok(container)
}

fn put_pos(out: &mut Vec<u8>, pos: BlockPos) {
    out.extend_from_slice(&pos.x.to_le_bytes());
    out.extend_from_slice(&pos.y.to_le_bytes());
    out.extend_from_slice(&pos.z.to_le_bytes());
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: ChunkPos,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        if self.bytes.len() < n {
            return Err(invalid(self.pos, "record ends early"));
        }
        let (head, rest) = self.bytes.split_at(n);
        self.bytes = rest;
        Ok(head)
    }

    fn u8(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> io::Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn i32(&mut self) -> io::Result<i32> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn i64(&mut self) -> io::Result<i64> {
        Ok(i64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn varint(&mut self) -> io::Result<u32> {
        vanillite_chunk::wire::read_varint(&mut self.bytes).map_err(|e| invalid(self.pos, e))
    }

    fn varint_u16(&mut self) -> io::Result<u16> {
        let value = self.varint()?;
        u16::try_from(value).map_err(|_| invalid(self.pos, format!("id {value} too large")))
    }

    fn pos(&mut self) -> io::Result<BlockPos> {
        Ok(BlockPos::new(self.i32()?, self.i32()?, self.i32()?))
    }
}

impl ChunkCodec for OwnCodec {
    fn decode(&self, pos: ChunkPos, _: &RecordMeta, record: &[u8]) -> io::Result<ProtoChunk> {
        let raw = zstd_decompress(record).map_err(|e| invalid(pos, e))?;
        self.decode_raw(pos, &raw)
    }

    fn encode(&self, chunk: &ProtoChunk) -> io::Result<(RecordMeta, Arc<[u8]>)> {
        let (meta, raw) = self.encode_raw(chunk);
        Ok((meta, Arc::from(zstd_compress(&raw, self.level)?)))
    }

    /// An entities record is Vanilla's entities NBT (`DataVersion`, `Position`, `Entities`,
    /// entities in stored order) in one zstd frame, as the conversion writes it.
    fn decode_entities(
        &self,
        pos: ChunkPos,
        _: &RecordMeta,
        record: &[u8],
    ) -> io::Result<Vec<Arc<[u8]>>> {
        let nbt = zstd_decompress(record).map_err(|e| invalid(pos, e))?;
        crate::chunk_nbt::read_entities(pos, &crate::read_root(pos, &nbt)?)
    }

    fn encode_entities(&self, chunk: &ProtoChunk) -> io::Result<(RecordMeta, Arc<[u8]>)> {
        let root = crate::chunk_nbt::write_entities(chunk.pos, &chunk.entities)?;
        let mut nbt = Vec::with_capacity(1024);
        vanillite_nbt::write::write_named("", &root, &mut nbt)
            .map_err(|e| invalid(chunk.pos, format!("{e:?}")))?;
        let meta = RecordMeta {
            data_version: DATA_VERSION as u32,
            raw_len: u32::try_from(nbt.len()).unwrap_or(u32::MAX),
            checksum: 0,
            status: RecordMeta::STATUS_UNKNOWN,
            flags: 0,
        };
        Ok((meta, Arc::from(zstd_compress(&nbt, self.level)?)))
    }

    /// A POI record is Vanilla's POI NBT (`DataVersion`, `Sections`) in one zstd frame.
    fn decode_poi(
        &self,
        pos: ChunkPos,
        _: &RecordMeta,
        record: &[u8],
    ) -> io::Result<Option<Arc<[u8]>>> {
        let nbt = zstd_decompress(record).map_err(|e| invalid(pos, e))?;
        crate::chunk_nbt::read_poi(pos, crate::read_root(pos, &nbt)?).map(Some)
    }

    fn encode_poi(&self, chunk: &ProtoChunk) -> io::Result<(RecordMeta, Arc<[u8]>)> {
        let root =
            crate::chunk_nbt::write_poi(chunk.pos, chunk.poi.as_deref().unwrap_or_default())?;
        let mut nbt = Vec::with_capacity(1024);
        vanillite_nbt::write::write_named("", &root, &mut nbt)
            .map_err(|e| invalid(chunk.pos, format!("{e:?}")))?;
        let meta = RecordMeta {
            data_version: DATA_VERSION as u32,
            raw_len: u32::try_from(nbt.len()).unwrap_or(u32::MAX),
            checksum: 0,
            status: RecordMeta::STATUS_UNKNOWN,
            flags: 0,
        };
        Ok((meta, Arc::from(zstd_compress(&nbt, self.level)?)))
    }

    fn chunks_per_job(&self) -> usize {
        // ~80 µs per chunk with zstd 3 (#25 measurements): jobs of about 1 ms.
        12
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk_nbt::tests::{codec, diff, golden, without};
    use vanillite_nbt::Tag;

    /// Vanilla's chunks go Anvil → own format → Anvil and come back as the same NBT; the own
    /// record decodes to a chunk that encodes to the same record.
    #[test]
    fn anvil_round_trips_through_the_own_format() {
        let chunks = golden();
        let anvil = codec(&chunks);
        let own = OwnCodec::new(anvil.info(), DEFAULT_LEVEL);
        for (pos, bytes, root) in &chunks {
            let chunk = anvil.read_bytes(*pos, bytes).unwrap();
            let (meta, record) = own.encode(&chunk).unwrap();
            assert_eq!(meta.status, chunk.status as u8);
            let back = own.decode(*pos, &meta, &record).unwrap();
            assert_eq!(own.encode_raw(&back).1, own.encode_raw(&chunk).1, "{pos:?}");
            let ignored: &[&str] = if chunk.status == ChunkStatus::Full {
                &["DataVersion"]
            } else {
                &["DataVersion", "Heightmaps"]
            };
            let written = anvil.write(&back).unwrap();
            let mut diffs = Vec::new();
            diff(
                &Tag::Compound(without(&written, ignored)),
                &Tag::Compound(without(root, ignored)),
                String::new(),
                &mut diffs,
            );
            assert!(diffs.is_empty(), "{pos:?}: {diffs:#?}");
        }
    }

    /// Biomes are kept by name: a record read with another biome registry maps them.
    #[test]
    fn biomes_follow_their_names() {
        let chunks = golden();
        let anvil = codec(&chunks);
        let own = OwnCodec::new(anvil.info(), DEFAULT_LEVEL);
        let (pos, bytes, _) = &chunks[0];
        let chunk = anvil.read_bytes(*pos, bytes).unwrap();
        let (meta, record) = own.encode(&chunk).unwrap();
        let mut info = anvil.info().clone();
        let mut biomes: Vec<String> = info.biomes.iter().cloned().collect();
        biomes.reverse();
        info.biomes = biomes.into();
        let shifted = OwnCodec::new(&info, DEFAULT_LEVEL);
        let back = shifted.decode(*pos, &meta, &record).unwrap();
        let name = |info: &WorldInfo, c: &ProtoChunk, y: i32| {
            info.biomes[usize::from(c.chunk.sections()[y as usize].biome(1, 2, 3))].clone()
        };
        for y in 0..chunk.chunk.sections().len() as i32 {
            assert_eq!(name(anvil.info(), &chunk, y), name(&info, &back, y));
        }
    }

    /// Entities keep their stored order through an own-format entities record.
    #[test]
    fn entities_keep_their_order() {
        let chunks = golden();
        let anvil = codec(&chunks);
        let own = OwnCodec::new(anvil.info(), DEFAULT_LEVEL);
        let (pos, bytes, _) = &chunks[0];
        let mut chunk = anvil.read_bytes(*pos, bytes).unwrap();
        chunk.entities = (0..5)
            .map(|i| {
                let mut entity = vanillite_nbt::Compound::new();
                entity.insert("id", "minecraft:pig");
                entity.insert("Age", i);
                let mut nbt = Vec::new();
                vanillite_nbt::write::write_network_compound(&entity, &mut nbt).unwrap();
                Arc::from(nbt)
            })
            .collect();
        let (meta, record) = own.encode_entities(&chunk).unwrap();
        assert_eq!(
            own.decode_entities(*pos, &meta, &record).unwrap(),
            chunk.entities
        );
    }

    /// A record of an older data version (made-up id table: states, blocks and block entity
    /// types shifted, removed and with a lost property) loads with the current ids; a direct
    /// palette is packed again, ticks of removed blocks are dropped.
    #[test]
    fn older_records_are_remapped() {
        use crate::upgrade::tests::{older_table, upgraded_state};
        let removed = 10;
        let chunks = golden();
        let anvil = codec(&chunks);
        let (pos, bytes, _) = &chunks[0];
        let mut chunk = anvil.read_bytes(*pos, bytes).unwrap();
        let bottom = chunk.chunk.min_section() * 16;
        for i in 0..300u32 {
            let state = 1 + i as u16 * 7;
            chunk
                .chunk
                .set_block(i % 16, bottom + (i / 256) as i32, (i / 16) % 16, state);
        }
        let tick = |kind| SavedTick {
            kind,
            pos: BlockPos::new(pos.x * 16, bottom, pos.z * 16),
            delay: 3,
            priority: 0,
        };
        chunk.block_ticks = vec![tick(0), tick(5)];
        chunk.fluid_ticks = vec![tick(1)];
        for kind in [0, 3] {
            chunk.block_entities.push(BlockEntityData {
                pos: BlockPos::new(pos.x * 16, bottom + kind as i32, pos.z * 16),
                kind: Some(kind),
                nbt: Arc::from(&[10u8, 0][..]),
            });
        }
        let own = OwnCodec::new(anvil.info(), DEFAULT_LEVEL);
        let (_, mut raw) = own.encode_raw(&chunk);
        let old = DATA_VERSION as u32 - 1;
        raw[3..7].copy_from_slice(&old.to_le_bytes());
        assert!(own.decode_raw(*pos, &raw).is_err());

        let remap = crate::upgrade::IdRemap::parse(old, &older_table(removed, true)).unwrap();
        let own = own.with_upgrades(Upgrades::new(vec![remap]));
        let back = own.decode_raw(*pos, &raw).unwrap();
        assert!(matches!(
            chunk.chunk.sections()[0].blocks().data(),
            vanillite_chunk::ContainerData::Direct { .. }
        ));
        for (before, after) in chunk.chunk.sections().iter().zip(back.chunk.sections()) {
            for index in 0..BlockStates::ENTRIES {
                assert_eq!(
                    after.blocks().get(index),
                    upgraded_state(removed, before.blocks().get(index))
                );
            }
            assert_eq!(before.biomes(), after.biomes());
        }
        assert_eq!(back.block_ticks, [tick(4)]);
        assert_eq!(back.fluid_ticks, chunk.fluid_ticks);
        let kinds =
            |c: &ProtoChunk| -> Vec<_> { c.block_entities.iter().map(|b| b.kind).collect() };
        let expected: Vec<_> = kinds(&chunk)
            .into_iter()
            .map(|kind| kind.and_then(|k| k.checked_sub(1)))
            .collect();
        assert_eq!(kinds(&back), expected);
        // Saved again, the chunk has the current data version.
        let (meta, raw) = own.encode_raw(&back);
        assert_eq!(meta.data_version, DATA_VERSION as u32);
        assert_eq!(own.encode_raw(&own.decode_raw(*pos, &raw).unwrap()).1, raw);
    }

    /// A damaged or truncated record is an error, never a panic.
    #[test]
    fn damaged_records_are_errors() {
        let chunks = golden();
        let anvil = codec(&chunks);
        let own = OwnCodec::new(anvil.info(), DEFAULT_LEVEL);
        let (pos, bytes, _) = &chunks[0];
        let (_, raw) = own.encode_raw(&anvil.read_bytes(*pos, bytes).unwrap());
        for cut in (0..raw.len()).step_by(97) {
            assert!(own.decode_raw(*pos, &raw[..cut]).is_err());
        }
        assert!(
            own.decode_raw(ChunkPos::new(pos.x + 1, pos.z), &raw)
                .is_err()
        );
    }
}

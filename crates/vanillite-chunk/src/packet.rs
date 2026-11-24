//! Chunk and light packet bodies.
//!
//! Vanilla: `ClientboundLevelChunkWithLightPacket`, `ClientboundLevelChunkPacketData`,
//! `ClientboundLightUpdatePacket`, `ClientboundLightUpdatePacketData`. The functions write the
//! packet body after the packet id; framing, the id and compression belong to the protocol layer.

use crate::chunk::Chunk;
use crate::light::{ChunkLight, LightNibbles, NIBBLE_BYTES};
use crate::wire::{self, write_varint};

/// One bit per section or light section; 256 bits cover the tallest dimension plus borders.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SectionMask(pub [u64; 4]);

impl SectionMask {
    /// Sets bit `index`.
    #[inline]
    pub fn set(&mut self, index: usize) {
        self.0[index >> 6] |= 1 << (index & 63);
    }

    /// Whether bit `index` is set.
    #[inline]
    pub fn contains(&self, index: usize) -> bool {
        self.0[index >> 6] & (1 << (index & 63)) != 0
    }

    /// No bit set.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.0 == [0; 4]
    }
}

/// A block entity as sent in the chunk packet.
#[derive(Debug, Clone, Copy)]
pub struct BlockEntityWire<'a> {
    /// `(x & 15) << 4 | (z & 15)`.
    pub packed_xz: u8,
    /// World Y.
    pub y: i16,
    /// Block entity type id.
    pub type_id: u32,
    /// Network NBT of the update tag, or a single `0` (end tag) when it is empty.
    pub nbt: &'a [u8],
}

/// Bytes of the section data (`calculateChunkSize`).
pub fn chunk_data_size(chunk: &Chunk) -> usize {
    chunk.sections().iter().map(|s| s.serialized_size()).sum()
}

/// Appends the body of `level_chunk_with_light`: position, heightmaps, section data, block
/// entities and light. Sections that share an `Arc` are encoded from the same memory.
pub fn encode_chunk_with_light(
    out: &mut Vec<u8>,
    x: i32,
    z: i32,
    chunk: &Chunk,
    block_entities: &[BlockEntityWire<'_>],
) {
    let data_size = chunk_data_size(chunk);
    out.reserve(
        64 + 3 * 300 + data_size + block_entities.len() * 8 + light_size_hint(chunk.light()),
    );
    out.extend_from_slice(&x.to_be_bytes());
    out.extend_from_slice(&z.to_be_bytes());

    // Heightmaps: a map keyed by type id, in id order (EnumMap).
    let heightmaps = chunk.heightmaps();
    let sent = || heightmaps.iter().filter(|(kind, _)| kind.sent_to_client());
    write_varint(out, sent().count() as u32);
    for (kind, map) in sent() {
        write_varint(out, u32::from(kind.id()));
        write_varint(out, map.raw().len() as u32);
        out.extend_from_slice(map.raw().as_flattened());
    }

    write_varint(out, data_size as u32);
    let start = out.len();
    for section in chunk.sections() {
        section.write(out);
    }
    debug_assert_eq!(out.len() - start, data_size);

    write_varint(out, block_entities.len() as u32);
    for entity in block_entities {
        out.push(entity.packed_xz);
        out.extend_from_slice(&entity.y.to_be_bytes());
        write_varint(out, entity.type_id);
        out.extend_from_slice(entity.nbt);
    }

    encode_light_data(out, chunk.light());
}

/// Appends the body of `light_update`: VarInt position and light data.
pub fn encode_light_update(out: &mut Vec<u8>, x: i32, z: i32, light: &ChunkLight) {
    out.reserve(10 + light_size_hint(light));
    write_varint(out, x as u32);
    write_varint(out, z as u32);
    encode_light_data(out, light);
}

fn light_size_hint(light: &ChunkLight) -> usize {
    let arrays = light
        .sky()
        .iter()
        .chain(light.block())
        .filter(|layer| matches!(layer, LightNibbles::Data(_) | LightNibbles::Uniform(1..)))
        .count();
    4 * 40 + 10 + arrays * (NIBBLE_BYTES + 2)
}

/// Appends light data: four masks, then the sky and block arrays. A layer of level 0 is sent as
/// "empty", absent layers are not sent.
pub fn encode_light_data(out: &mut Vec<u8>, light: &ChunkLight) {
    let (sky_mask, empty_sky) = masks(light.sky());
    let (block_mask, empty_block) = masks(light.block());
    wire::write_bitset(out, &sky_mask.0);
    wire::write_bitset(out, &block_mask.0);
    wire::write_bitset(out, &empty_sky.0);
    wire::write_bitset(out, &empty_block.0);
    write_arrays(out, light.sky());
    write_arrays(out, light.block());
}

fn sends_array(layer: &LightNibbles) -> bool {
    matches!(layer, LightNibbles::Data(_) | LightNibbles::Uniform(1..))
}

fn masks(layers: &[LightNibbles]) -> (SectionMask, SectionMask) {
    let mut data = SectionMask::default();
    let mut empty = SectionMask::default();
    for (index, layer) in layers.iter().enumerate() {
        match layer {
            LightNibbles::Absent => {}
            LightNibbles::Uniform(0) => empty.set(index),
            _ => data.set(index),
        }
    }
    (data, empty)
}

fn write_arrays(out: &mut Vec<u8>, layers: &[LightNibbles]) {
    write_varint(
        out,
        layers.iter().filter(|layer| sends_array(layer)).count() as u32,
    );
    for layer in layers {
        match layer {
            LightNibbles::Data(data) => {
                write_varint(out, NIBBLE_BYTES as u32);
                out.extend_from_slice(&data[..]);
            }
            LightNibbles::Uniform(level @ 1..) => {
                write_varint(out, NIBBLE_BYTES as u32);
                out.resize(out.len() + NIBBLE_BYTES, (level & 15) * 0x11);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::heightmap::HeightmapKind;
    use crate::section::Section;
    use crate::test_support::{self, AIR, STONE};
    use crate::wire::{read_array, read_u8, read_varint};

    #[test]
    fn chunk_packet_layout() {
        test_support::install();
        let mut chunk = Chunk::new(-64, 384, 7, AIR);
        for x in 0..16 {
            for z in 0..16 {
                chunk.set_block(x, 0, z, STONE);
            }
        }
        chunk.set_block(3, 1, 3, STONE);
        let light = chunk.light_mut();
        light.sky_mut()[0] = LightNibbles::Uniform(0);
        light.sky_mut()[25] = LightNibbles::Uniform(15);
        light.block_mut()[5].set(1, 1, 1, 14);

        let mut out = vec![];
        let nbt = [0u8];
        let entities = [BlockEntityWire {
            packed_xz: 0x12,
            y: -5,
            type_id: 9,
            nbt: &nbt,
        }];
        encode_chunk_with_light(&mut out, -3, 4, &chunk, &entities);

        let mut input = out.as_slice();
        assert_eq!(i32::from_be_bytes(read_array(&mut input).unwrap()), -3);
        assert_eq!(i32::from_be_bytes(read_array(&mut input).unwrap()), 4);
        assert_eq!(read_varint(&mut input).unwrap(), 3);
        let mut ids = vec![];
        for _ in 0..3 {
            ids.push(read_varint(&mut input).unwrap());
            // 9 bits for 384 + 1 values: 7 per long, 37 longs.
            assert_eq!(read_varint(&mut input).unwrap(), 37);
            input = &input[37 * 8..];
        }
        assert_eq!(ids, [1, 4, 5]);
        let size = read_varint(&mut input).unwrap() as usize;
        assert_eq!(size, chunk_data_size(&chunk));
        let mut data = &input[..size];
        for expected in chunk.sections() {
            assert_eq!(&Section::read(&mut data).unwrap(), &**expected);
        }
        assert!(data.is_empty());
        input = &input[size..];
        assert_eq!(read_varint(&mut input).unwrap(), 1);
        assert_eq!(read_u8(&mut input).unwrap(), 0x12);
        assert_eq!(i16::from_be_bytes(read_array(&mut input).unwrap()), -5);
        assert_eq!(read_varint(&mut input).unwrap(), 9);
        assert_eq!(read_u8(&mut input).unwrap(), 0);

        // Masks: sky data {25}, block data {5}, empty sky {0}, empty block {}.
        let mut read_mask = || {
            let len = read_varint(&mut input).unwrap();
            (0..len)
                .map(|_| u64::from_be_bytes(read_array(&mut input).unwrap()))
                .collect::<Vec<_>>()
        };
        assert_eq!(read_mask(), [1 << 25]);
        assert_eq!(read_mask(), [1 << 5]);
        assert_eq!(read_mask(), [1]);
        assert_eq!(read_mask(), Vec::<u64>::new());
        assert_eq!(read_varint(&mut input).unwrap(), 1);
        assert_eq!(read_varint(&mut input).unwrap(), 2048);
        assert!(input[..2048].iter().all(|&b| b == 0xff));
        input = &input[2048..];
        assert_eq!(read_varint(&mut input).unwrap(), 1);
        assert_eq!(read_varint(&mut input).unwrap(), 2048);
        assert_eq!(input.len(), 2048);

        let heights = chunk.heightmaps().get(HeightmapKind::WorldSurface).unwrap();
        assert_eq!(heights.get(3, 3), 66);
        assert_eq!(heights.get(0, 0), 65);
    }
}

//! Chunk sections between versions: block states and biomes of paletted containers are
//! rewritten; the packed data is repacked only when the global palette's bit count differs.

use crate::Result;
use crate::io;
use vanillite_protocol::Error;
use vanillite_protocol::codec::{Reader, write_varint};

/// One kind of paletted container.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Kind {
    /// Entries per container (4096 block states, 64 biomes).
    pub entries: usize,
    /// Wire bits that select an indirect palette (others are direct).
    pub max_indirect: u8,
    /// Bits of the global palette on the newer side and the older side.
    pub global_new: u8,
    pub global_old: u8,
}

/// Bits of a global palette of `count` values (`Mth.ceillog2`).
pub(crate) fn global_bits(count: usize) -> u8 {
    let mut bits = 0u8;
    while (1usize << bits) < count {
        bits = bits.saturating_add(1);
    }
    bits
}

/// Longs of `entries` values of `bits` each (values do not span longs).
fn longs(entries: usize, bits: u8) -> usize {
    let per = 64usize.checked_div(usize::from(bits.max(1))).unwrap_or(1);
    entries.div_ceil(per.max(1))
}

/// Rewrites one paletted container with `map` (newer value → older value).
pub(crate) fn container(
    r: &mut Reader<'_>,
    out: &mut Vec<u8>,
    kind: Kind,
    map: &dyn Fn(i32) -> i32,
) -> Result<()> {
    let bits = r.u8()?;
    if bits == 0 {
        out.push(0);
        write_varint(out, map(r.varint()?));
        return Ok(());
    }
    if bits <= kind.max_indirect {
        out.push(bits);
        let n = io::len(r, 1 << bits, 1)?;
        write_varint(
            out,
            i32::try_from(n).map_err(|_| Error::Invalid("palette"))?,
        );
        for _ in 0..n {
            write_varint(out, map(r.varint()?));
        }
        io::bytes(r, out, longs(kind.entries, bits).saturating_mul(8))?;
        return Ok(());
    }
    // Direct: every value from the global palette.
    if bits != kind.global_new {
        return Err(Error::Invalid("direct palette bits"));
    }
    let data = r.take(longs(kind.entries, bits).saturating_mul(8))?;
    let mut values = Vec::with_capacity(kind.entries);
    let per = 64usize.checked_div(usize::from(bits)).unwrap_or(1);
    let mask = (1u64 << bits).saturating_sub(1);
    for chunk in data.as_chunks::<8>().0 {
        let mut word = u64::from_be_bytes(*chunk);
        for _ in 0..per {
            if values.len() == kind.entries {
                break;
            }
            values.push(map(i32::try_from(word & mask).unwrap_or(0)));
            word >>= bits;
        }
    }
    let old_bits = kind.global_old.max(1);
    out.push(old_bits);
    let per = 64usize.checked_div(usize::from(old_bits)).unwrap_or(1);
    for chunk in values.chunks(per) {
        let mut word = 0u64;
        for (i, v) in chunk.iter().enumerate() {
            let shift = u32::try_from(i.saturating_mul(usize::from(old_bits))).unwrap_or(0);
            word |= u64::from(u32::try_from(*v).unwrap_or(0)) << shift;
        }
        out.extend_from_slice(&word.to_be_bytes());
    }
    Ok(())
}

/// Rewrites the sections of a chunk (`level_chunk_with_light` section data): non-air and fluid
/// counts, block states, biomes; until the data ends.
pub(crate) fn sections(
    data: &[u8],
    out: &mut Vec<u8>,
    blocks: Kind,
    biomes: Kind,
    block_map: &dyn Fn(i32) -> i32,
    biome_map: &dyn Fn(i32) -> i32,
) -> Result<()> {
    let mut r = Reader::new(data);
    while r.remaining() > 0 {
        io::bytes(&mut r, out, 4)?;
        container(&mut r, out, blocks, block_map)?;
        container(&mut r, out, biomes, biome_map)?;
    }
    Ok(())
}

/// Rewrites only the biome containers of sections (`chunks_biomes` data).
pub(crate) fn biome_sections(
    data: &[u8],
    out: &mut Vec<u8>,
    biomes: Kind,
    biome_map: &dyn Fn(i32) -> i32,
) -> Result<()> {
    let mut r = Reader::new(data);
    while r.remaining() > 0 {
        container(&mut r, out, biomes, biome_map)?;
    }
    Ok(())
}

/// Copies the heightmaps of a chunk packet: a map of type id → long array.
pub(crate) fn heightmaps(r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<()> {
    let n = io::len(r, 16, 2)?;
    io::write_len(out, n);
    for _ in 0..n {
        io::varint(r, out)?;
        let longs = io::len(r, 1024, 8)?;
        io::write_len(out, longs);
        io::bytes(r, out, longs.saturating_mul(8))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_bits_match_vanilla() {
        assert_eq!(global_bits(35_723), 16);
        assert_eq!(global_bits(32_366), 15);
        assert_eq!(global_bits(67), 7);
        assert_eq!(global_bits(1), 0);
    }
}

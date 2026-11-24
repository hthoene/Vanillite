//! Chunk compression of region files (Vanilla's `RegionFileVersion`): gzip, zlib (deflate),
//! none and LZ4 in lz4-java's `LZ4BlockOutputStream` format.

use std::io::{self, Read, Write};

/// Largest decompressed chunk accepted (Vanilla's NBT file quota, SEC-26).
pub const MAX_DECOMPRESSED: usize = 100 * 1024 * 1024;

/// A compression of chunk data, by its id in region files.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compression {
    /// 1: gzip (read only in Vanilla).
    Gzip,
    /// 2: zlib, Vanilla's default (`deflate`).
    Deflate,
    /// 3: uncompressed.
    None,
    /// 4: LZ4 block stream.
    Lz4,
}

impl Compression {
    /// The id in region files.
    pub const fn id(self) -> u8 {
        match self {
            Self::Gzip => 1,
            Self::Deflate => 2,
            Self::None => 3,
            Self::Lz4 => 4,
        }
    }

    /// The compression of an id.
    pub const fn from_id(id: u8) -> Option<Self> {
        match id {
            1 => Some(Self::Gzip),
            2 => Some(Self::Deflate),
            3 => Some(Self::None),
            4 => Some(Self::Lz4),
            _ => None,
        }
    }

    /// The compression named like `region-file-compression` in `server.properties`.
    pub fn by_name(name: &str) -> Option<Self> {
        match name {
            "deflate" => Some(Self::Deflate),
            "lz4" => Some(Self::Lz4),
            "none" => Some(Self::None),
            _ => None,
        }
    }

    /// Decompresses `data`.
    pub fn decompress(self, data: &[u8]) -> io::Result<Vec<u8>> {
        match self {
            Self::None => {
                if data.len() > MAX_DECOMPRESSED {
                    return Err(too_large());
                }
                Ok(data.to_vec())
            }
            Self::Deflate => read_limited(flate2::read::ZlibDecoder::new(data)),
            Self::Gzip => read_limited(flate2::read::GzDecoder::new(data)),
            Self::Lz4 => lz4_decompress(data),
        }
    }

    /// Compresses `data`.
    pub fn compress(self, data: &[u8]) -> io::Result<Vec<u8>> {
        match self {
            Self::None => Ok(data.to_vec()),
            Self::Deflate => {
                let mut out = flate2::write::ZlibEncoder::new(
                    Vec::with_capacity(data.len() / 4),
                    flate2::Compression::default(),
                );
                out.write_all(data)?;
                out.finish()
            }
            Self::Gzip => {
                let mut out = flate2::write::GzEncoder::new(
                    Vec::with_capacity(data.len() / 4),
                    flate2::Compression::default(),
                );
                out.write_all(data)?;
                out.finish()
            }
            Self::Lz4 => Ok(lz4_compress(data)),
        }
    }
}

fn too_large() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "decompressed chunk too large")
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_owned())
}

fn read_limited(reader: impl Read) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    reader
        .take(MAX_DECOMPRESSED as u64 + 1)
        .read_to_end(&mut out)?;
    if out.len() > MAX_DECOMPRESSED {
        return Err(too_large());
    }
    Ok(out)
}

const LZ4_MAGIC: &[u8; 8] = b"LZ4Block";
const LZ4_HEADER: usize = 8 + 1 + 4 + 4 + 4;
const LZ4_RAW: u8 = 0x10;
const LZ4_COMPRESSED: u8 = 0x20;
/// lz4-java's default block size (64 KiB): compression level 16 - 10.
const LZ4_LEVEL: u8 = 6;
const LZ4_BLOCK: usize = 1 << (10 + LZ4_LEVEL as usize);
const LZ4_SEED: u32 = 0x9747_b28c;

fn lz4_checksum(data: &[u8]) -> u32 {
    xxhash32(data, LZ4_SEED) & 0x0fff_ffff
}

fn lz4_compress(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() / 2 + LZ4_HEADER * 2);
    let write_header =
        |out: &mut Vec<u8>, method: u8, compressed: usize, raw: usize, check: u32| {
            out.extend_from_slice(LZ4_MAGIC);
            out.push(method | LZ4_LEVEL);
            out.extend_from_slice(&(compressed as u32).to_le_bytes());
            out.extend_from_slice(&(raw as u32).to_le_bytes());
            out.extend_from_slice(&check.to_le_bytes());
        };
    for block in data.chunks(LZ4_BLOCK) {
        let check = lz4_checksum(block);
        let compressed = lz4_flex::block::compress(block);
        if compressed.len() < block.len() {
            write_header(
                &mut out,
                LZ4_COMPRESSED,
                compressed.len(),
                block.len(),
                check,
            );
            out.extend_from_slice(&compressed);
        } else {
            write_header(&mut out, LZ4_RAW, block.len(), block.len(), check);
            out.extend_from_slice(block);
        }
    }
    write_header(&mut out, LZ4_RAW, 0, 0, 0);
    out
}

fn lz4_decompress(mut data: &[u8]) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let Some((header, rest)) = data.split_first_chunk::<LZ4_HEADER>() else {
            return Err(invalid("truncated LZ4 block header"));
        };
        if &header[..8] != LZ4_MAGIC {
            return Err(invalid("bad LZ4 block magic"));
        }
        let token = header[8];
        let int = |at: usize| {
            u32::from_le_bytes([header[at], header[at + 1], header[at + 2], header[at + 3]])
        };
        let (compressed, raw, check) = (int(9) as usize, int(13) as usize, int(17));
        let block_size = 1usize << (10 + (token & 15));
        if raw > block_size || compressed > rest.len() {
            return Err(invalid("bad LZ4 block lengths"));
        }
        if raw == 0 && compressed == 0 {
            return Ok(out);
        }
        if out.len() + raw > MAX_DECOMPRESSED {
            return Err(too_large());
        }
        let (block, rest) = rest.split_at(compressed);
        let start = out.len();
        match token & 0xf0 {
            LZ4_RAW if compressed == raw => out.extend_from_slice(block),
            LZ4_COMPRESSED => {
                out.resize(start + raw, 0);
                let written = lz4_flex::block::decompress_into(block, &mut out[start..])
                    .map_err(|e| invalid(&format!("LZ4: {e}")))?;
                if written != raw {
                    return Err(invalid("LZ4 block length mismatch"));
                }
            }
            _ => return Err(invalid("bad LZ4 block method")),
        }
        if lz4_checksum(&out[start..]) != check {
            return Err(invalid("LZ4 block checksum mismatch"));
        }
        data = rest;
    }
}

/// XXH32.
fn xxhash32(data: &[u8], seed: u32) -> u32 {
    const P1: u32 = 0x9e37_79b1;
    const P2: u32 = 0x85eb_ca77;
    const P3: u32 = 0xc2b2_ae3d;
    const P4: u32 = 0x27d4_eb2f;
    const P5: u32 = 0x1656_67b1;
    let round = |acc: u32, lane: u32| {
        acc.wrapping_add(lane.wrapping_mul(P2))
            .rotate_left(13)
            .wrapping_mul(P1)
    };
    let word = |bytes: &[u8]| u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let mut rest = data;
    let mut hash = if data.len() >= 16 {
        let mut v = [
            seed.wrapping_add(P1).wrapping_add(P2),
            seed.wrapping_add(P2),
            seed,
            seed.wrapping_sub(P1),
        ];
        while rest.len() >= 16 {
            for (i, lane) in v.iter_mut().enumerate() {
                *lane = round(*lane, word(&rest[i * 4..]));
            }
            rest = &rest[16..];
        }
        v[0].rotate_left(1)
            .wrapping_add(v[1].rotate_left(7))
            .wrapping_add(v[2].rotate_left(12))
            .wrapping_add(v[3].rotate_left(18))
    } else {
        seed.wrapping_add(P5)
    };
    hash = hash.wrapping_add(data.len() as u32);
    while rest.len() >= 4 {
        hash = hash
            .wrapping_add(word(rest).wrapping_mul(P3))
            .rotate_left(17)
            .wrapping_mul(P4);
        rest = &rest[4..];
    }
    for &byte in rest {
        hash = hash
            .wrapping_add(u32::from(byte).wrapping_mul(P5))
            .rotate_left(11)
            .wrapping_mul(P1);
    }
    hash ^= hash >> 15;
    hash = hash.wrapping_mul(P2);
    hash ^= hash >> 13;
    hash = hash.wrapping_mul(P3);
    hash ^ (hash >> 16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xxhash_vectors() {
        assert_eq!(xxhash32(b"", 0), 0x02cc_5d05);
        assert_eq!(xxhash32(b"abc", 0), 0x32d1_53ff);
        assert_eq!(
            xxhash32(b"Nobody inspects the spammish repetition", 0),
            0xe229_3b2f
        );
    }

    #[test]
    fn round_trips() {
        let data: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        for kind in [
            Compression::Gzip,
            Compression::Deflate,
            Compression::None,
            Compression::Lz4,
        ] {
            let packed = kind.compress(&data).unwrap();
            assert_eq!(kind.decompress(&packed).unwrap(), data, "{kind:?}");
        }
    }
}

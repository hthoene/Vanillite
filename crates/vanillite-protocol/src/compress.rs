//! zlib compression of packet bodies (libdeflate: whole-buffer, no streaming state).

use crate::codec::{Error, Result};

/// The protocol's maximum uncompressed packet size.
pub const MAX_UNCOMPRESSED: usize = 8_388_608;

/// Compression and decompression of one connection or job. A libdeflate compressor holds
/// about 650 KB of match-finder tables, so compressors are shared per thread and level
/// (created on first use) instead of kept per connection; the decompressor is created on the
/// first decompression.
pub struct Codec {
    level: i32,
    decompressor: Option<libdeflater::Decompressor>,
}

thread_local! {
    /// This thread's compressors by level.
    static COMPRESSORS: std::cell::RefCell<Vec<(i32, libdeflater::Compressor)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

impl std::fmt::Debug for Codec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("compress::Codec")
    }
}

impl Default for Codec {
    fn default() -> Self {
        Self::new(6)
    }
}

impl Codec {
    /// A codec with a libdeflate level (1 = fastest, 12 = smallest; Vanilla uses zlib's default).
    pub fn new(level: i32) -> Self {
        let level = libdeflater::CompressionLvl::new(level).unwrap_or_default();
        Self {
            level: i32::from(level),
            decompressor: None,
        }
    }

    /// Appends the zlib stream of `data` to `out`.
    pub fn compress(&mut self, data: &[u8], out: &mut Vec<u8>) -> Result<()> {
        COMPRESSORS.with(|compressors| {
            let mut compressors = compressors.borrow_mut();
            let index = match compressors.iter().position(|(l, _)| *l == self.level) {
                Some(index) => index,
                None => {
                    let level = libdeflater::CompressionLvl::new(self.level).unwrap_or_default();
                    compressors.push((self.level, libdeflater::Compressor::new(level)));
                    compressors.len().saturating_sub(1)
                }
            };
            let compressor = &mut compressors
                .get_mut(index)
                .ok_or(Error::BadFrame("compress"))?
                .1;
            let start = out.len();
            let bound = compressor.zlib_compress_bound(data.len());
            out.resize(start.saturating_add(bound), 0);
            let target = out.get_mut(start..).ok_or(Error::BadFrame("compress"))?;
            match compressor.zlib_compress(data, target) {
                Ok(n) => {
                    out.truncate(start.saturating_add(n));
                    Ok(())
                }
                Err(_) => {
                    out.truncate(start);
                    Err(Error::BadFrame("compression failed"))
                }
            }
        })
    }

    /// Inflates `data`, which must produce exactly `expected` bytes (at most
    /// [`MAX_UNCOMPRESSED`]); the output is allocated once at that size, so a zip bomb cannot
    /// grow it.
    pub fn decompress(&mut self, data: &[u8], expected: usize, out: &mut Vec<u8>) -> Result<()> {
        if expected > MAX_UNCOMPRESSED {
            return Err(Error::BadFrame("uncompressed size above protocol maximum"));
        }
        out.clear();
        out.resize(expected, 0);
        let decompressor = self
            .decompressor
            .get_or_insert_with(libdeflater::Decompressor::new);
        match decompressor.zlib_decompress(data, out) {
            Ok(n) if n == expected => Ok(()),
            _ => Err(Error::BadFrame("bad compressed data")),
        }
    }
}

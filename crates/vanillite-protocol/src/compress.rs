//! zlib compression of packet bodies (libdeflate: whole-buffer, no streaming state).

use crate::codec::{Error, Result};

/// The protocol's maximum uncompressed packet size.
pub const MAX_UNCOMPRESSED: usize = 8_388_608;

/// A compressor and decompressor pair, kept per connection or per I/O thread.
pub struct Codec {
    compressor: libdeflater::Compressor,
    decompressor: libdeflater::Decompressor,
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
            compressor: libdeflater::Compressor::new(level),
            decompressor: libdeflater::Decompressor::new(),
        }
    }

    /// Appends the zlib stream of `data` to `out`.
    pub fn compress(&mut self, data: &[u8], out: &mut Vec<u8>) -> Result<()> {
        let start = out.len();
        let bound = self.compressor.zlib_compress_bound(data.len());
        out.resize(start.saturating_add(bound), 0);
        let target = out.get_mut(start..).ok_or(Error::BadFrame("compress"))?;
        match self.compressor.zlib_compress(data, target) {
            Ok(n) => {
                out.truncate(start.saturating_add(n));
                Ok(())
            }
            Err(_) => {
                out.truncate(start);
                Err(Error::BadFrame("compression failed"))
            }
        }
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
        match self.decompressor.zlib_decompress(data, out) {
            Ok(n) if n == expected => Ok(()),
            _ => Err(Error::BadFrame("bad compressed data")),
        }
    }
}

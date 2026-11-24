//! Light nibbles per section.
//!
//! Vanilla: `net.minecraft.world.level.chunk.DataLayer`. Index `y << 8 | z << 4 | x`, two levels
//! per byte, the even index in the low nibble.

use std::sync::Arc;

/// Bytes of one full nibble array.
pub const NIBBLE_BYTES: usize = 2048;

/// Light levels of one section.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum LightNibbles {
    /// No light data for this section. On the wire neither mask bit is set; for sky light the
    /// client then uses the data above (full sky light above the highest data layer).
    #[default]
    Absent,
    /// The same level everywhere, without an array.
    Uniform(u8),
    /// Full array, shared copy-on-write.
    Data(Arc<[u8; NIBBLE_BYTES]>),
}

#[inline]
const fn nibble_index(x: u32, y: u32, z: u32) -> usize {
    (((y & 15) << 8) | ((z & 15) << 4) | (x & 15)) as usize
}

impl LightNibbles {
    /// Level at local coordinates; `Absent` reads as 0.
    #[inline]
    pub fn get(&self, x: u32, y: u32, z: u32) -> u8 {
        match self {
            Self::Absent => 0,
            Self::Uniform(level) => *level,
            Self::Data(data) => {
                let index = nibble_index(x, y, z);
                (data[index >> 1] >> ((index & 1) * 4)) & 15
            }
        }
    }

    /// Sets the level at local coordinates; a uniform layer gets an array only when it changes.
    #[inline]
    pub fn set(&mut self, x: u32, y: u32, z: u32, level: u8) {
        if let Self::Uniform(current) = self
            && *current == level
        {
            return;
        }
        let data = self.data_mut();
        let index = nibble_index(x, y, z);
        let shift = (index & 1) * 4;
        let byte = &mut data[index >> 1];
        *byte = (*byte & !(15 << shift)) | ((level & 15) << shift);
    }

    /// Sets every level (`DataLayer.fill`).
    #[inline]
    pub fn fill(&mut self, level: u8) {
        *self = Self::Uniform(level & 15);
    }

    /// The array, created from the uniform level or zero if needed, copied if shared.
    pub fn data_mut(&mut self) -> &mut [u8; NIBBLE_BYTES] {
        if !matches!(self, Self::Data(_)) {
            *self = Self::Data(Arc::new(self.to_bytes()));
        }
        match self {
            Self::Data(data) => Arc::make_mut(data),
            _ => unreachable!(),
        }
    }

    /// The full array (materialised for uniform and absent layers).
    pub fn to_bytes(&self) -> [u8; NIBBLE_BYTES] {
        match self {
            Self::Absent => [0; NIBBLE_BYTES],
            Self::Uniform(level) => [(level & 15) * 0x11; NIBBLE_BYTES],
            Self::Data(data) => **data,
        }
    }
}

/// Sky and block light of a chunk: one layer per section plus one below and one above.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkLight {
    sky: Box<[LightNibbles]>,
    block: Box<[LightNibbles]>,
}

impl ChunkLight {
    /// Light for `section_count` sections, all absent. Without sky light the sky layers stay
    /// absent.
    pub fn new(section_count: usize) -> Self {
        let layers = section_count + 2;
        Self {
            sky: vec![LightNibbles::Absent; layers].into_boxed_slice(),
            block: vec![LightNibbles::Absent; layers].into_boxed_slice(),
        }
    }

    /// Number of light sections (sections + 2); index 0 is the section below the chunk.
    #[inline]
    pub fn len(&self) -> usize {
        self.sky.len()
    }

    /// Always false; a chunk has at least the two border layers.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.sky.is_empty()
    }

    /// Sky light layers.
    #[inline]
    pub fn sky(&self) -> &[LightNibbles] {
        &self.sky
    }

    /// Sky light layers, mutable.
    #[inline]
    pub fn sky_mut(&mut self) -> &mut [LightNibbles] {
        &mut self.sky
    }

    /// Block light layers.
    #[inline]
    pub fn block(&self) -> &[LightNibbles] {
        &self.block
    }

    /// Block light layers, mutable.
    #[inline]
    pub fn block_mut(&mut self) -> &mut [LightNibbles] {
        &mut self.block
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nibbles() {
        let mut layer = LightNibbles::Uniform(15);
        layer.set(1, 2, 3, 15);
        assert_eq!(layer, LightNibbles::Uniform(15));
        layer.set(1, 2, 3, 4);
        assert_eq!(layer.get(1, 2, 3), 4);
        assert_eq!(layer.get(0, 2, 3), 15);
        let bytes = layer.to_bytes();
        // Index 2 << 8 | 3 << 4 | 1 = 561: byte 280, high nibble.
        assert_eq!(bytes[280], 0x4f);
        let shared = layer.clone();
        layer.set(0, 0, 0, 0);
        assert_eq!(shared.get(0, 0, 0), 15);
        assert_eq!(layer.get(0, 0, 0), 0);
    }
}

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

    /// Raises every level to at least the level of `other` at the same position.
    pub fn merge_max(&mut self, other: &LightNibbles) {
        match (&*self, other) {
            (_, Self::Absent) => {}
            (Self::Absent, _) => *self = other.clone(),
            (Self::Uniform(a), Self::Uniform(b)) => *self = Self::Uniform((*a).max(*b)),
            (Self::Uniform(15), _) => {}
            (_, Self::Uniform(0)) => {}
            (Self::Data(a), Self::Data(b)) if Arc::ptr_eq(a, b) => {}
            _ => {
                let theirs = other.to_bytes();
                let mine = self.data_mut();
                for (mine, theirs) in mine.iter_mut().zip(theirs) {
                    let low = (*mine & 15).max(theirs & 15);
                    let high = (*mine >> 4).max(theirs >> 4);
                    *mine = high << 4 | low;
                }
            }
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
    /// Columns (bit `x + z * 16`) whose sky light sources were taken from the blocks when the
    /// chunk was loaded and that had no block change since (`ChunkSkyLightSources.fillFrom`
    /// tests no edge below the bottom block): a change there treats the column as open below
    /// the world if it is open down to its bottom block, whatever the stored light says.
    reset_sources: Option<Box<[u64; 4]>>,
}

impl ChunkLight {
    /// Heap bytes of the light arrays (shared arrays count fully; `/vanillite memory`).
    pub fn heap_bytes(&self) -> usize {
        let layer = |n: &LightNibbles| match n {
            LightNibbles::Data(_) => NIBBLE_BYTES,
            LightNibbles::Absent | LightNibbles::Uniform(_) => 0,
        };
        (self.sky.len() + self.block.len()) * size_of::<LightNibbles>()
            + self
                .sky
                .iter()
                .chain(self.block.iter())
                .map(layer)
                .sum::<usize>()
    }

    /// Light for `section_count` sections, all absent. Without sky light the sky layers stay
    /// absent.
    pub fn new(section_count: usize) -> Self {
        let layers = section_count + 2;
        Self {
            sky: vec![LightNibbles::Absent; layers].into_boxed_slice(),
            block: vec![LightNibbles::Absent; layers].into_boxed_slice(),
            reset_sources: None,
        }
    }

    /// Marks the sky light sources of every column as taken from the blocks (a chunk loaded
    /// with its light, `ChunkSkyLightSources.fillFrom`).
    pub fn reset_sky_sources(&mut self) {
        self.reset_sources = Some(Box::new([u64::MAX; 4]));
    }

    /// Whether the sky light sources of column `x + z * 16` are still the ones taken from the
    /// blocks at load ([`Self::reset_sky_sources`]).
    #[inline]
    pub fn sky_sources_reset(&self, column: usize) -> bool {
        self.reset_sources.as_ref().is_some_and(|bits| {
            bits.get(column >> 6)
                .is_some_and(|word| word & (1 << (column & 63)) != 0)
        })
    }

    /// Ends [`Self::sky_sources_reset`] for a column (a block of it changed). Returns whether
    /// it was set.
    pub fn clear_sky_sources_reset(&mut self, column: usize) -> bool {
        let Some(bits) = self.reset_sources.as_mut() else {
            return false;
        };
        let Some(word) = bits.get_mut(column >> 6) else {
            return false;
        };
        let bit = 1 << (column & 63);
        let was = *word & bit != 0;
        *word &= !bit;
        if bits.iter().all(|&word| word == 0) {
            self.reset_sources = None;
        }
        was
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

    /// Raises every level of both light types to at least the level in `other` (same layout).
    /// An absent sky layer counts as full sky light and stays absent; an absent block layer
    /// below or above the chunk (one it keeps no light in) stays absent as well.
    pub fn merge_max(&mut self, other: &ChunkLight) {
        for (mine, theirs) in self.sky.iter_mut().zip(other.sky.iter()) {
            if *mine != LightNibbles::Absent {
                mine.merge_max(theirs);
            }
        }
        let top = self.block.len() - 1;
        for (index, (mine, theirs)) in self.block.iter_mut().zip(other.block.iter()).enumerate() {
            if (index == 0 || index == top) && *mine == LightNibbles::Absent {
                continue;
            }
            mine.merge_max(theirs);
        }
    }

    /// Block light layers, mutable.
    #[inline]
    pub fn block_mut(&mut self) -> &mut [LightNibbles] {
        &mut self.block
    }
}

/// What a chunk's light keeps when a neighbour unloads (`ThreadedLevelLightEngine
/// .updateChunkStatus`): Vanilla keeps light data for a section while a section within one
/// section of it, also in the neighbouring chunks, holds blocks; a section that only the
/// unloaded chunk kept loses its light (block light reads 0, sky light continues the layer
/// above). It does not come back when the neighbour loads again.
///
/// `area` is the 3 × 3 chunks around the chunk (row-major, z then x; the chunk at index 4),
/// `unloaded` the index of the neighbour that unloads. Returns whether the light changed.
pub fn forget_unkept_light(
    light: &mut ChunkLight,
    area: &[Option<&crate::Chunk>; 9],
    unloaded: usize,
) -> bool {
    let filled = |index: usize, section: isize| {
        usize::try_from(section).ok().is_some_and(|section| {
            area[index].is_some_and(|chunk| {
                chunk
                    .sections()
                    .get(section)
                    .is_some_and(|s| !s.summary().is_empty())
            })
        })
    };
    let kept = |section: isize, with_unloaded: bool| {
        (0..9).any(|index| {
            (with_unloaded || index != unloaded)
                && (section - 1..=section + 1).any(|s| filled(index, s))
        })
    };
    let top = light.len() - 1;
    let mut changed = false;
    for layer in 0..light.len() {
        let section = layer as isize - 1;
        if !kept(section, true) || kept(section, false) {
            continue;
        }
        let forgotten = if layer == 0 || layer == top {
            LightNibbles::Absent
        } else {
            LightNibbles::Uniform(0)
        };
        if light.block[layer] != forgotten {
            light.block[layer] = forgotten;
            changed = true;
        }
        if light.sky[layer] != LightNibbles::Absent {
            light.sky[layer] = LightNibbles::Absent;
            changed = true;
        }
    }
    changed
}

/// What a chunk's light gets back when a neighbour loads again, the reverse of
/// [`forget_unkept_light`]: Vanilla creates the data layers of the sections it keeps again
/// because of the neighbour (block light empty, sky light a copy of the bottom row of the data
/// above, `SkyLightSectionStorage.createDataLayer`) and sends them to clients. Sky light layers
/// above all data and the one above the world stay absent (full sky light either way).
///
/// `area` and `loaded` as in [`forget_unkept_light`]. Returns the layers created again.
pub fn restore_kept_light(
    light: &mut ChunkLight,
    area: &[Option<&crate::Chunk>; 9],
    loaded: usize,
) -> crate::packet::SectionMask {
    let filled = |index: usize, section: isize| {
        usize::try_from(section).ok().is_some_and(|section| {
            area[index].is_some_and(|chunk| {
                chunk
                    .sections()
                    .get(section)
                    .is_some_and(|s| !s.summary().is_empty())
            })
        })
    };
    let kept = |section: isize, with_loaded: bool| {
        (0..9).any(|index| {
            (with_loaded || index != loaded)
                && (section - 1..=section + 1).any(|s| filled(index, s))
        })
    };
    let top = light.len() - 1;
    let mut restored = crate::packet::SectionMask::default();
    for layer in 0..light.len() {
        let section = layer as isize - 1;
        if !kept(section, true) || kept(section, false) {
            continue;
        }
        restored.set(layer);
        if light.block[layer] == LightNibbles::Absent {
            light.block[layer] = LightNibbles::Uniform(0);
        }
        if layer == top || light.sky[layer] != LightNibbles::Absent {
            continue;
        }
        let above = light.sky[layer + 1..]
            .iter()
            .find(|above| **above != LightNibbles::Absent);
        light.sky[layer] = match above {
            None => continue,
            Some(LightNibbles::Data(data)) => {
                let mut bytes = [0; NIBBLE_BYTES];
                for row in bytes.as_chunks_mut::<128>().0 {
                    row.copy_from_slice(&data[..128]);
                }
                LightNibbles::Data(Arc::new(bytes))
            }
            Some(uniform) => uniform.clone(),
        };
    }
    restored
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

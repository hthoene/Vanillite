//! Layout descriptors: `repr(C)` views of the chunk data for native providers.
//!
//! A descriptor borrows the data it points to; the pointers stay valid while the borrow lives and
//! the data is not written. Packed words are big-endian 64-bit values behind byte pointers (no
//! alignment promise). Providers hand data back through slices and
//! [`PalettedContainer::from_parts`](crate::PalettedContainer::from_parts), which validates it.

use std::marker::PhantomData;
use std::ptr;

use crate::container::{ContainerData, PalettedContainer, Strategy};
use crate::heightmap::Heightmap;
use crate::light::LightNibbles;
use crate::section::Section;

/// `ContainerLayout::kind`: one value, no data.
pub const CONTAINER_SINGLE: u8 = 0;
/// `ContainerLayout::kind`: palette and packed indices.
pub const CONTAINER_INDIRECT: u8 = 1;
/// `ContainerLayout::kind`: packed global ids.
pub const CONTAINER_DIRECT: u8 = 2;

/// A paletted container.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ContainerLayout<'a> {
    /// [`CONTAINER_SINGLE`], [`CONTAINER_INDIRECT`] or [`CONTAINER_DIRECT`].
    pub kind: u8,
    /// Bits per entry (0 for single).
    pub bits_per_entry: u8,
    /// Palette entries (0 unless indirect).
    pub palette_len: u16,
    /// The value of a single container.
    pub single_value: u32,
    /// Palette of global ids (null unless indirect).
    pub palette: *const u16,
    /// Packed big-endian words (null for single).
    pub data: *const u8,
    /// Number of 64-bit words behind `data`.
    pub data_words: u32,
    /// Entries in the container (4096 or 64).
    pub entries: u32,
    /// Ties the pointers to the borrowed container.
    pub _borrow: PhantomData<&'a ()>,
}

impl<S: Strategy> PalettedContainer<S> {
    /// The layout descriptor of this container.
    pub fn layout(&self) -> ContainerLayout<'_> {
        let mut layout = ContainerLayout {
            kind: CONTAINER_SINGLE,
            bits_per_entry: self.bits(),
            palette_len: 0,
            single_value: 0,
            palette: ptr::null(),
            data: ptr::null(),
            data_words: 0,
            entries: S::ENTRIES as u32,
            _borrow: PhantomData,
        };
        match self.data() {
            ContainerData::Single(value) => layout.single_value = u32::from(*value),
            ContainerData::Indirect { palette, data, .. } => {
                layout.kind = CONTAINER_INDIRECT;
                layout.palette_len = palette.len() as u16;
                layout.palette = palette.as_ptr();
                layout.data = data.as_flattened().as_ptr();
                layout.data_words = data.len() as u32;
            }
            ContainerData::Direct { data, .. } => {
                layout.kind = CONTAINER_DIRECT;
                layout.data = data.as_flattened().as_ptr();
                layout.data_words = data.len() as u32;
            }
        }
        layout
    }
}

/// A section: containers and summary.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SectionLayout<'a> {
    /// Block states (16³, index `y << 8 | z << 4 | x`).
    pub blocks: ContainerLayout<'a>,
    /// Biomes (4³, index `y << 4 | z << 2 | x`).
    pub biomes: ContainerLayout<'a>,
    /// Non-air blocks.
    pub non_air: u16,
    /// Non-air blocks with a fluid.
    pub fluids: u16,
    /// Randomly ticking blocks.
    pub ticking_blocks: u16,
    /// Randomly ticking fluids.
    pub ticking_fluids: u16,
    /// Blocks with a special collision shape.
    pub special_collision: u16,
}

impl Section {
    /// The layout descriptor of this section.
    pub fn layout(&self) -> SectionLayout<'_> {
        let summary = self.summary();
        SectionLayout {
            blocks: self.blocks().layout(),
            biomes: self.biomes().layout(),
            non_air: summary.non_air,
            fluids: summary.fluids,
            ticking_blocks: summary.ticking_blocks,
            ticking_fluids: summary.ticking_fluids,
            special_collision: summary.special_collision,
        }
    }
}

/// `NibbleLayout::kind`: no data.
pub const NIBBLES_ABSENT: u8 = 0;
/// `NibbleLayout::kind`: one level everywhere.
pub const NIBBLES_UNIFORM: u8 = 1;
/// `NibbleLayout::kind`: 2048 bytes, index `y << 8 | z << 4 | x`, even index in the low nibble.
pub const NIBBLES_DATA: u8 = 2;

/// One light layer.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct NibbleLayout<'a> {
    /// [`NIBBLES_ABSENT`], [`NIBBLES_UNIFORM`] or [`NIBBLES_DATA`].
    pub kind: u8,
    /// Level of a uniform layer.
    pub uniform: u8,
    /// 2048 bytes for a data layer, otherwise null.
    pub data: *const u8,
    /// Ties the pointer to the borrowed layer.
    pub _borrow: PhantomData<&'a ()>,
}

impl LightNibbles {
    /// The layout descriptor of this layer.
    pub fn layout(&self) -> NibbleLayout<'_> {
        let (kind, uniform, data) = match self {
            Self::Absent => (NIBBLES_ABSENT, 0, ptr::null()),
            Self::Uniform(level) => (NIBBLES_UNIFORM, *level, ptr::null()),
            Self::Data(data) => (NIBBLES_DATA, 0, data.as_ptr()),
        };
        NibbleLayout {
            kind,
            uniform,
            data,
            _borrow: PhantomData,
        }
    }
}

/// One heightmap: 256 entries (index `x + z * 16`) relative to the chunk's minimum Y.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct HeightmapLayout<'a> {
    /// Bits per entry.
    pub bits_per_entry: u8,
    /// Packed big-endian words.
    pub data: *const u8,
    /// Number of 64-bit words behind `data`.
    pub data_words: u32,
    /// Ties the pointer to the borrowed heightmap.
    pub _borrow: PhantomData<&'a ()>,
}

impl Heightmap {
    /// The layout descriptor of this heightmap.
    pub fn layout(&self) -> HeightmapLayout<'_> {
        HeightmapLayout {
            bits_per_entry: self.bits(),
            data: self.raw().as_flattened().as_ptr(),
            data_words: self.raw().len() as u32,
            _borrow: PhantomData,
        }
    }
}

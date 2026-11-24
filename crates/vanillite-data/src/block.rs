//! Blocks, block states and their per-state facts.
//!
//! [`BlockState`] is Vanilla's global state id. A block's states are contiguous; a property value
//! is `(state - first_state) / stride % value_count`; properties are sorted by name and the last
//! one varies fastest.

pub use crate::generated::blocks::{
    BLOCK_COUNT, BLOCKS, BlockKind, INSTRUMENTS, MISC, PROPERTIES, PushReaction, SHAPE_SETS,
    SHAPES, SOUND_TYPES, STATE_BLOCK, STATE_COUNT, STATE_FLAGS, STATE_FLUID, STATE_LIGHT,
    STATE_MISC, STATE_SHAPES, flags, ids,
};

use crate::fluid::FluidState;

/// A block (registry id of `minecraft:block`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct BlockId(pub u16);

/// A block state (Vanilla's global block state id).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct BlockState(pub u16);

/// Static facts of a block.
#[derive(Debug)]
pub struct BlockInfo {
    pub name: &'static str,
    pub first_state: u16,
    pub state_count: u16,
    pub default_state: u16,
    /// Indices into [`PROPERTIES`], in Vanilla's order.
    pub properties: &'static [u16],
    pub hardness: f32,
    pub blast_resistance: f32,
    pub friction: f32,
    pub speed_factor: f32,
    pub jump_factor: f32,
    /// Maximum random offset of blocks with an offset function (flowers, bamboo, …).
    pub max_horizontal_offset: f32,
    pub max_vertical_offset: f32,
    pub dynamic_shape: bool,
    /// Item id, or `u16::MAX` if the block has no item.
    pub item: u16,
    /// Block entity type id, or `u16::MAX` if the block has none.
    pub block_entity: u16,
    /// The block's Java class.
    pub kind: BlockKind,
    /// Index into [`SOUND_TYPES`] of the default state's sound type.
    pub sound: u16,
}

/// A block sound type (`SoundType`): sound event ids (`minecraft:sound_event`).
#[derive(Debug, PartialEq)]
pub struct SoundType {
    pub volume: f32,
    pub pitch: f32,
    pub break_sound: u16,
    pub step_sound: u16,
    pub place_sound: u16,
    pub hit_sound: u16,
    pub fall_sound: u16,
}

/// A block state property with its values in Vanilla's order.
#[derive(Debug)]
pub struct PropertyInfo {
    pub name: &'static str,
    pub values: &'static [&'static str],
}

/// Rarely used per-state facts, deduplicated.
#[derive(Debug)]
pub struct StateMisc {
    /// Destroy time of this state (`-1.0` for unbreakable blocks).
    pub destroy_speed: f32,
    pub map_color: u8,
    pub push_reaction: PushReaction,
    /// Index into [`INSTRUMENTS`].
    pub instrument: u8,
    /// Faces (bit = `Direction` 3D data value: down, up, north, south, west, east) that are
    /// sturdy for `SupportType::FULL`, `CENTER` and `RIGID` in an empty level.
    pub sturdy_full: u8,
    pub sturdy_center: u8,
    pub sturdy_rigid: u8,
}

/// A shape: boxes `[min_x, min_y, min_z, max_x, max_y, max_z]` in block coordinates.
pub type Shape = &'static [[f64; 6]];

impl BlockId {
    /// Static facts of this block.
    #[inline]
    pub fn info(self) -> &'static BlockInfo {
        &BLOCKS[usize::from(self.0)]
    }

    /// Registry name, e.g. `minecraft:stone`.
    #[inline]
    pub fn name(self) -> &'static str {
        self.info().name
    }

    #[inline]
    pub fn default_state(self) -> BlockState {
        BlockState(self.info().default_state)
    }

    /// The sound type of the block's default state.
    #[inline]
    pub fn sound(self) -> &'static SoundType {
        &SOUND_TYPES[usize::from(self.info().sound)]
    }

    /// Looks a block up by its registry name.
    pub fn by_name(name: &str) -> Option<Self> {
        crate::registry::BLOCK.id(name).map(Self)
    }
}

impl BlockState {
    pub const AIR: Self = Self(0);

    #[inline]
    fn index(self) -> usize {
        usize::from(self.0)
    }

    /// Whether the id is a valid state id.
    #[inline]
    pub fn is_valid(self) -> bool {
        self.index() < STATE_COUNT
    }

    #[inline]
    pub fn block(self) -> BlockId {
        BlockId(STATE_BLOCK[self.index()])
    }

    /// All `flags` bits of this state.
    #[inline]
    pub fn flags(self) -> u32 {
        STATE_FLAGS[self.index()]
    }

    /// Whether all bits of `flag` (from [`flags`]) are set.
    #[inline]
    pub fn has(self, flag: u32) -> bool {
        self.flags() & flag == flag
    }

    #[inline]
    pub fn is_air(self) -> bool {
        self.has(flags::AIR)
    }

    #[inline]
    pub fn random_ticks(self) -> bool {
        self.has(flags::RANDOM_TICKS)
    }

    #[inline]
    pub fn light_emission(self) -> u8 {
        STATE_LIGHT[self.index()] >> 4
    }

    #[inline]
    pub fn light_dampening(self) -> u8 {
        STATE_LIGHT[self.index()] & 15
    }

    #[inline]
    pub fn fluid_state(self) -> FluidState {
        FluidState(STATE_FLUID[self.index()])
    }

    #[inline]
    fn shape(self, kind: usize) -> Shape {
        let set = &SHAPE_SETS[usize::from(STATE_SHAPES[self.index()])];
        SHAPES[usize::from(set[kind])]
    }

    /// Collision shape in an empty level, without the random offset (see `flags::COLLISION_OFFSET`).
    #[inline]
    pub fn collision_shape(self) -> Shape {
        self.shape(0)
    }

    /// Outline (selection) shape in an empty level, without the random offset.
    #[inline]
    pub fn outline_shape(self) -> Shape {
        self.shape(1)
    }

    #[inline]
    pub fn interaction_shape(self) -> Shape {
        self.shape(2)
    }

    #[inline]
    pub fn support_shape(self) -> Shape {
        self.shape(3)
    }

    #[inline]
    pub fn visual_shape(self) -> Shape {
        self.shape(4)
    }

    #[inline]
    pub fn occlusion_shape(self) -> Shape {
        self.shape(5)
    }

    /// Rarely used facts (destroy speed, map colour, push reaction, sturdy faces, …).
    #[inline]
    pub fn misc(self) -> &'static StateMisc {
        &MISC[usize::from(STATE_MISC[self.index()])]
    }

    /// Position of `property` in this state's block and the stride of its values.
    fn locate(self, property: &str) -> Option<(&'static BlockInfo, usize, u16, u16)> {
        let info = self.block().info();
        let mut stride = info.state_count;
        for &p in info.properties {
            let prop = &PROPERTIES[usize::from(p)];
            let count = u16::try_from(prop.values.len()).ok()?;
            stride /= count;
            if prop.name == property {
                return Some((info, usize::from(p), stride, count));
            }
        }
        None
    }

    /// Value of `property`, e.g. `facing` → `north`.
    pub fn value(self, property: &str) -> Option<&'static str> {
        let (info, p, stride, count) = self.locate(property)?;
        let index = (self.0 - info.first_state) / stride % count;
        PROPERTIES[p].values.get(usize::from(index)).copied()
    }

    /// This state with `property` set to `value`.
    pub fn with(self, property: &str, value: &str) -> Option<Self> {
        let (info, p, stride, count) = self.locate(property)?;
        let new = u16::try_from(PROPERTIES[p].values.iter().position(|v| *v == value)?).ok()?;
        let old = (self.0 - info.first_state) / stride % count;
        Some(Self(self.0 - old * stride + new * stride))
    }

    /// Index of the value of `property` in Vanilla's value order (an integer property
    /// `0..=15` reads as its value), without parsing.
    #[inline]
    pub fn value_index(self, property: &str) -> Option<u16> {
        let (info, _, stride, count) = self.locate(property)?;
        Some((self.0 - info.first_state) / stride % count)
    }

    /// This state with `property` set to the value at `index` in Vanilla's value order.
    #[inline]
    pub fn with_index(self, property: &str, index: u16) -> Option<Self> {
        let (info, _, stride, count) = self.locate(property)?;
        if index >= count {
            return None;
        }
        let old = (self.0 - info.first_state) / stride % count;
        Some(Self(self.0 - old * stride + index * stride))
    }

    /// All properties of this state as `(name, value)`.
    pub fn properties(self) -> impl Iterator<Item = (&'static str, &'static str)> {
        let info = self.block().info();
        let offset = self.0 - info.first_state;
        let mut stride = info.state_count;
        info.properties.iter().map(move |&p| {
            let prop = &PROPERTIES[usize::from(p)];
            let count = u16::try_from(prop.values.len()).unwrap_or(u16::MAX);
            stride /= count;
            let v = prop.values[usize::from(offset / stride % count)];
            (prop.name, v)
        })
    }
}

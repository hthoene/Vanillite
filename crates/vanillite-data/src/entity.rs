//! Entity type facts and attributes.

pub use crate::generated::entities::{ATTRIBUTES, ENTITY_TYPES, ids};

/// An entity type (registry id of `minecraft:entity_type`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct EntityTypeId(pub u16);

/// Static facts of an entity type.
#[derive(Debug)]
pub struct EntityTypeInfo {
    pub name: &'static str,
    /// Mob category (`monster`, `creature`, `misc`, …).
    pub category: &'static str,
    pub width: f32,
    pub height: f32,
    pub eye_height: f32,
    pub fixed_size: bool,
    pub fire_immune: bool,
    pub summonable: bool,
    pub can_spawn_far_from_player: bool,
    /// Client tracking range in chunks.
    pub tracking_range: u32,
    /// Ticks between movement updates (`i32::MAX` = never).
    pub update_interval: i32,
    pub track_deltas: bool,
    /// Default attribute base values: (attribute id, base value).
    pub attributes: &'static [(u16, f64)],
}

/// An attribute (registry id of `minecraft:attribute`) with its default.
#[derive(Debug)]
pub struct AttributeInfo {
    pub name: &'static str,
    pub default: f64,
    pub syncable: bool,
}

impl EntityTypeId {
    #[inline]
    pub fn info(self) -> &'static EntityTypeInfo {
        &ENTITY_TYPES[usize::from(self.0)]
    }

    #[inline]
    pub fn name(self) -> &'static str {
        self.info().name
    }

    /// Looks an entity type up by its registry name.
    pub fn by_name(name: &str) -> Option<Self> {
        crate::registry::ENTITY_TYPE.id(name).map(Self)
    }
}

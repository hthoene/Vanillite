//! The dimensions of a server: Vanilla's three and those of the data packs and the world's
//! `world_gen_settings.dat` (`WorldDimensions.bake`), in tick order. Built once at start; a
//! world's index here is its [`WorldId`].

use std::sync::Arc;

use crate::world::WorldId;

/// One dimension: its key (`minecraft:the_nether`) and the key of its dimension type.
#[derive(Debug)]
struct Dimension {
    id: String,
    kind: String,
}

/// The dimensions of a server in tick order: the overworld, the nether and the end, then the
/// others. Cheap to clone.
#[derive(Clone, Debug)]
pub struct Dimensions(Arc<[Dimension]>);

impl Dimensions {
    /// The dimensions `(key, dimension type)` in tick order.
    pub(crate) fn new(dimensions: Vec<(String, String)>) -> Self {
        Dimensions(
            dimensions
                .into_iter()
                .map(|(id, kind)| Dimension { id, kind })
                .collect(),
        )
    }

    /// Vanilla's three dimensions with their own dimension types.
    pub fn vanilla() -> Self {
        Self::new(
            crate::server::VANILLA_DIMENSIONS
                .iter()
                .map(|d| ((*d).to_owned(), (*d).to_owned()))
                .collect(),
        )
    }

    /// The number of dimensions.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether there are none (never on a running server).
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The key of a world's dimension.
    pub fn name(&self, world: WorldId) -> Option<&str> {
        self.0.get(world.index()).map(|d| d.id.as_str())
    }

    /// The key of a world's dimension, the overworld's for an unknown world.
    pub fn name_or_overworld(&self, world: WorldId) -> &str {
        self.name(world).unwrap_or("minecraft:overworld")
    }

    /// The key of a world's dimension type.
    pub fn kind(&self, world: WorldId) -> Option<&str> {
        self.0.get(world.index()).map(|d| d.kind.as_str())
    }

    /// The world of a dimension key.
    pub fn world(&self, dimension: &str) -> Option<WorldId> {
        self.0
            .iter()
            .position(|d| d.id == dimension)
            .and_then(|i| u16::try_from(i).ok())
            .map(WorldId)
    }

    /// All worlds in tick order.
    pub fn worlds(&self) -> impl Iterator<Item = WorldId> + '_ {
        (0..self.0.len())
            .filter_map(|i| u16::try_from(i).ok())
            .map(WorldId)
    }

    /// The dimension keys in tick order.
    pub fn names(&self) -> impl Iterator<Item = &str> + '_ {
        self.0.iter().map(|d| d.id.as_str())
    }

    /// `(world, key, dimension type)` of every dimension in tick order.
    pub fn iter(&self) -> impl Iterator<Item = (WorldId, &str, &str)> + '_ {
        self.worlds()
            .zip(self.0.iter())
            .map(|(w, d)| (w, d.id.as_str(), d.kind.as_str()))
    }
}

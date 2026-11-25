//! The structure templates of a world for structure and test instance blocks
//! (`StructureTemplateManager` with its `generated/` directory, `StructureTemplate.fillFromWorld`
//! and `placeInWorld` on the live level).

use std::sync::Arc;

use vanillite_base::BlockPos;
use vanillite_data::block::BlockId;
use vanillite_worldgen::template::structure::StructureTemplate;
use vanillite_worldgen::template::transform::{Mirror, Rotation};

use crate::level::Level;

/// The `StructurePlaceSettings` structure and test instance blocks place with.
#[derive(Clone, Debug, PartialEq)]
pub struct PlaceSettings {
    /// `setMirror`.
    pub mirror: Mirror,
    /// `setRotation`.
    pub rotation: Rotation,
    /// `setIgnoreEntities`.
    pub ignore_entities: bool,
    /// `BlockRotProcessor` integrity (`1.0` places everything).
    pub integrity: f32,
    /// The seed of the integrity random (`StructureBlockEntity.createRandom`).
    pub seed: i64,
    /// `setKnownShape` (strict placement: no shape updates at the edge).
    pub strict: bool,
}

impl Default for PlaceSettings {
    fn default() -> Self {
        Self {
            mirror: Mirror::default(),
            rotation: Rotation::default(),
            ignore_entities: true,
            integrity: 1.0,
            seed: 0,
            strict: false,
        }
    }
}

impl Level {
    /// `StructureTemplateManager.get(id)`: the world's `generated/` templates first, then the
    /// data packs'. `None` if there is none.
    pub fn structure_template(&self, _id: &str) -> Option<Arc<StructureTemplate>> {
        None
    }

    /// `StructureTemplateManager.save(id)`: writes `template` to
    /// `generated/<namespace>/structures/<path>.nbt` and caches it; `false` if that fails.
    pub fn save_structure_template(&self, _id: &str, _template: StructureTemplate) -> bool {
        false
    }

    /// `StructureTemplate.fillFromWorld(level, origin, size, includeEntities, ignoreBlocks)`.
    pub fn capture_structure(
        &self,
        _origin: BlockPos,
        _size: BlockPos,
        _include_entities: bool,
        _ignore: &[BlockId],
    ) -> StructureTemplate {
        StructureTemplate::default()
    }

    /// `StructureTemplate.placeInWorld(level, pos, pos, settings, random, UPDATE_CLIENTS)` on
    /// the live level; whether anything was placed.
    pub fn place_structure(
        &mut self,
        _template: &StructureTemplate,
        _pos: BlockPos,
        _settings: &PlaceSettings,
    ) -> bool {
        false
    }
}

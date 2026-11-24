//! `PoolElementStructurePiece` (`minecraft:jigsaw` pieces) and the jigsaw types the core reads
//! (`StructureTemplatePool.Projection`, `JigsawJunction`).
//!
//! The pieces are built by `JigsawPlacement` ([`super::kinds::jigsaw`]); the `Beardifier`
//! reads them through [`super::StructurePiece::pool_element`].

use std::sync::Arc;

use vanillite_base::{BlockPos, ChunkPos};
use vanillite_nbt::{Compound, List, Tag};

use super::kinds::jigsaw::DEFAULT_LIQUID_SETTINGS;
use super::kinds::jigsaw::element::PoolElement;
use super::piece::{PieceBase, PieceContext, StructurePiece};
use crate::Error;
use crate::template::{BoundingBox, LiquidSettings, Rotation};

/// `StructureTemplatePool.Projection`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Projection {
    /// `terrain_matching`: follows the terrain height.
    TerrainMatching,
    /// `rigid`: placed as one block.
    #[default]
    Rigid,
}

impl Projection {
    /// Parses the serialized name.
    pub fn from_name(name: &str) -> Result<Self, Error> {
        match name {
            "terrain_matching" => Ok(Self::TerrainMatching),
            "rigid" => Ok(Self::Rigid),
            other => Err(Error::new(format!("unknown projection {other}"))),
        }
    }

    /// The serialized name.
    pub fn name(self) -> &'static str {
        match self {
            Self::TerrainMatching => "terrain_matching",
            Self::Rigid => "rigid",
        }
    }
}

/// `JigsawJunction`: where a jigsaw connection meets the ground, for the `Beardifier`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JigsawJunction {
    pub source_x: i32,
    pub source_ground_y: i32,
    pub source_z: i32,
    pub delta_y: i32,
    pub dest_projection: Projection,
}

impl JigsawJunction {
    /// `serialize(ops)`.
    pub fn save_data(&self) -> Compound {
        Compound::new()
            .with("source_x", self.source_x)
            .with("source_ground_y", self.source_ground_y)
            .with("source_z", self.source_z)
            .with("delta_y", self.delta_y)
            .with("dest_proj", self.dest_projection.name())
    }
}

/// What the `Beardifier` reads of a `PoolElementStructurePiece`.
#[derive(Clone, Copy, Debug)]
pub struct PoolElementInfo<'a> {
    /// `getElement().getProjection()`.
    pub projection: Projection,
    /// `getGroundLevelDelta()`.
    pub ground_level_delta: i32,
    /// `getJunctions()`.
    pub junctions: &'a [JigsawJunction],
}

/// `PoolElementStructurePiece`: one pool element placed by `JigsawPlacement`.
#[derive(Clone, Debug)]
pub struct PoolElementStructurePiece {
    /// The base piece: type `minecraft:jigsaw`, depth 0, no orientation.
    pub base: PieceBase,
    /// `element`.
    pub element: Arc<PoolElement>,
    /// `position`.
    pub position: BlockPos,
    /// `groundLevelDelta`.
    pub ground_level_delta: i32,
    /// `rotation`.
    pub rotation: Rotation,
    /// `junctions`.
    pub junctions: Vec<JigsawJunction>,
    /// `liquidSettings`.
    pub liquid_settings: LiquidSettings,
}

impl PoolElementStructurePiece {
    /// `new PoolElementStructurePiece(manager, element, position, groundLevelDelta, rotation,
    /// boundingBox, liquidSettings)`.
    pub fn new(
        element: Arc<PoolElement>,
        position: BlockPos,
        ground_level_delta: i32,
        rotation: Rotation,
        bounding_box: BoundingBox,
        liquid_settings: LiquidSettings,
    ) -> Self {
        Self {
            base: PieceBase::new("minecraft:jigsaw", 0, bounding_box),
            element,
            position,
            ground_level_delta,
            rotation,
            junctions: Vec::new(),
            liquid_settings,
        }
    }

    /// `move(dx, dy, dz)`: the box and the position.
    pub fn move_piece(&mut self, dx: i32, dy: i32, dz: i32) {
        self.base.bounding_box.translate(dx, dy, dz);
        self.position = self.position.offset(dx, dy, dz);
    }
}

impl StructurePiece for PoolElementStructurePiece {
    fn base(&self) -> &PieceBase {
        &self.base
    }

    fn base_mut(&mut self) -> &mut PieceBase {
        &mut self.base
    }

    /// `postProcess`: `place(level, ..., chunkBB, referencePos, keepJigsaws = false)`.
    fn post_process(
        &mut self,
        ctx: &mut PieceContext<'_, '_>,
        chunk_bb: &mut BoundingBox,
        _chunk_pos: ChunkPos,
        reference: BlockPos,
    ) {
        self.element.place(
            ctx,
            self.position,
            reference,
            self.rotation,
            chunk_bb,
            self.liquid_settings,
        );
    }

    /// `addAdditionalSaveData`: `liquid_settings` only when not the default.
    fn additional_save_data(&self) -> Option<Compound> {
        let mut tag = Compound::new()
            .with("PosX", self.position.x)
            .with("PosY", self.position.y)
            .with("PosZ", self.position.z)
            .with("ground_level_delta", self.ground_level_delta)
            .with("pool_element", self.element.save_data())
            .with("rotation", self.rotation.legacy_name())
            .with(
                "junctions",
                List(
                    self.junctions
                        .iter()
                        .map(|j| Tag::Compound(j.save_data()))
                        .collect(),
                ),
            );
        if self.liquid_settings != DEFAULT_LIQUID_SETTINGS {
            tag.insert("liquid_settings", self.liquid_settings.name());
        }
        Some(tag)
    }

    fn move_by(&mut self, dx: i32, dy: i32, dz: i32) {
        self.move_piece(dx, dy, dz);
    }

    fn rotation(&self) -> Rotation {
        self.rotation
    }

    fn pool_element(&self) -> Option<PoolElementInfo<'_>> {
        Some(PoolElementInfo {
            projection: self.element.projection(),
            ground_level_delta: self.ground_level_delta,
            junctions: &self.junctions,
        })
    }
}

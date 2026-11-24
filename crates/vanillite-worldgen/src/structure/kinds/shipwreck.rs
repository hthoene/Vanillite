//! Shipwrecks (`ShipwreckStructure`, `ShipwreckPieces`).

use std::sync::Arc;

use serde_json::Value;
use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, ChunkPos, Direction};
use vanillite_chunk::HeightmapKind;

use super::nether_fossil::ignore_structure_and_air;
use vanillite_nbt::Compound;

use crate::Error;
use crate::feature::WorldGenRegion;
use crate::feature::field;
use crate::structure::piece::{PieceContext, set_loot_table};
use crate::structure::structure::{
    lowest_y_at, mean_first_occupied_height, on_top_of_chunk_center,
};
use crate::structure::{
    GenerationContext, GenerationStub, PieceBase, PieceSettings, StructureParser, StructurePiece,
    StructureType, TemplatePiece,
};
use crate::template::structure::RANDOMIZABLE_CONTAINERS;
use crate::template::{BoundingBox, Rotation, StructureTemplate};

/// `ShipwreckPieces.STRUCTURE_LOCATION_BEACHED`.
const BEACHED: [&str; 11] = [
    "minecraft:shipwreck/with_mast",
    "minecraft:shipwreck/sideways_full",
    "minecraft:shipwreck/sideways_fronthalf",
    "minecraft:shipwreck/sideways_backhalf",
    "minecraft:shipwreck/rightsideup_full",
    "minecraft:shipwreck/rightsideup_fronthalf",
    "minecraft:shipwreck/rightsideup_backhalf",
    "minecraft:shipwreck/with_mast_degraded",
    "minecraft:shipwreck/rightsideup_full_degraded",
    "minecraft:shipwreck/rightsideup_fronthalf_degraded",
    "minecraft:shipwreck/rightsideup_backhalf_degraded",
];

/// `ShipwreckPieces.STRUCTURE_LOCATION_OCEAN`.
const OCEAN: [&str; 20] = [
    "minecraft:shipwreck/with_mast",
    "minecraft:shipwreck/upsidedown_full",
    "minecraft:shipwreck/upsidedown_fronthalf",
    "minecraft:shipwreck/upsidedown_backhalf",
    "minecraft:shipwreck/sideways_full",
    "minecraft:shipwreck/sideways_fronthalf",
    "minecraft:shipwreck/sideways_backhalf",
    "minecraft:shipwreck/rightsideup_full",
    "minecraft:shipwreck/rightsideup_fronthalf",
    "minecraft:shipwreck/rightsideup_backhalf",
    "minecraft:shipwreck/with_mast_degraded",
    "minecraft:shipwreck/upsidedown_full_degraded",
    "minecraft:shipwreck/upsidedown_fronthalf_degraded",
    "minecraft:shipwreck/upsidedown_backhalf_degraded",
    "minecraft:shipwreck/sideways_full_degraded",
    "minecraft:shipwreck/sideways_fronthalf_degraded",
    "minecraft:shipwreck/sideways_backhalf_degraded",
    "minecraft:shipwreck/rightsideup_full_degraded",
    "minecraft:shipwreck/rightsideup_fronthalf_degraded",
    "minecraft:shipwreck/rightsideup_backhalf_degraded",
];

/// `ShipwreckPieces.PIVOT`.
const PIVOT: BlockPos = BlockPos::new(4, 0, 15);

/// `ShipwreckStructure`.
#[derive(Debug)]
pub struct ShipwreckStructure {
    pub is_beached: bool,
    /// The templates of [`BEACHED`] or [`OCEAN`].
    templates: Vec<(&'static str, Arc<StructureTemplate>)>,
}

impl StructureType for ShipwreckStructure {
    fn parse(json: &Value, parser: &mut StructureParser<'_>) -> Result<Self, Error> {
        let is_beached = field(json, "is_beached")?
            .as_bool()
            .ok_or_else(|| Error::new("is_beached is not a boolean"))?;
        let names: &[&'static str] = if is_beached { &BEACHED } else { &OCEAN };
        Ok(Self {
            is_beached,
            templates: names
                .iter()
                .map(|&id| Ok((id, parser.template(id)?)))
                .collect::<Result<_, Error>>()?,
        })
    }

    fn find_generation_point<'s>(
        &'s self,
        ctx: &mut GenerationContext<'_>,
    ) -> Option<GenerationStub<'s>> {
        let heightmap = if self.is_beached {
            HeightmapKind::WorldSurfaceWg
        } else {
            HeightmapKind::OceanFloorWg
        };
        on_top_of_chunk_center(ctx, heightmap, move |builder, ctx| {
            let rotation = Rotation::random(&mut ctx.random.random);
            let offset =
                BlockPos::new(ctx.chunk_pos.min_block_x(), 90, ctx.chunk_pos.min_block_z());
            // `ShipwreckPieces.addRandomPiece`.
            let index = ctx
                .random
                .random
                .next_int_bounded(self.templates.len() as i32) as usize;
            let (name, template) = &self.templates[index];
            let mut piece = ShipwreckPiece::new(
                name,
                Arc::clone(template),
                offset,
                rotation,
                self.is_beached,
            );
            if piece.is_too_big_to_fit_in_world_gen_region() {
                let bb = *piece.bounding_box();
                let height = if self.is_beached {
                    // Vanilla passes `(minX, xSpan, minZ, zSpan)` to `getLowestY(context, minX,
                    // minZ, sizeX, sizeZ)`; the swapped arguments are kept.
                    let min_y = lowest_y_at(ctx, bb.min_x(), bb.x_span(), bb.min_z(), bb.z_span());
                    piece.calculate_beached_position(min_y, &mut ctx.random.random)
                } else {
                    mean_first_occupied_height(
                        ctx,
                        bb.min_x(),
                        bb.x_span(),
                        bb.min_z(),
                        bb.z_span(),
                    )
                };
                piece.adjust_position_height(height);
            }
            builder.add_piece(Box::new(piece));
        })
    }
}

/// `ShipwreckPieces.ShipwreckPiece` (`minecraft:shipwreck`).
#[derive(Clone, Debug)]
pub struct ShipwreckPiece {
    pub template: TemplatePiece,
    pub is_beached: bool,
    pub height_adjusted: bool,
}

impl ShipwreckPiece {
    /// `new ShipwreckPiece(manager, location, position, rotation, isBeached)`.
    pub fn new(
        name: &str,
        template: Arc<StructureTemplate>,
        position: BlockPos,
        rotation: Rotation,
        is_beached: bool,
    ) -> Self {
        let settings = PieceSettings {
            rotation,
            rotation_pivot: PIVOT,
            processors: vec![ignore_structure_and_air()],
            ..PieceSettings::new()
        };
        Self {
            template: TemplatePiece::new(
                "minecraft:shipwreck",
                0,
                name.to_owned(),
                template,
                settings,
                position,
            ),
            is_beached,
            height_adjusted: false,
        }
    }

    /// `isTooBigToFitInWorldGenRegion()`.
    pub fn is_too_big_to_fit_in_world_gen_region(&self) -> bool {
        let size = self.template.template.size();
        size.x > 32 || size.y > 32
    }

    /// `calculateBeachedPosition(minY, random)`.
    pub fn calculate_beached_position<R: RandomSource + ?Sized>(
        &self,
        min_y: i32,
        random: &mut R,
    ) -> i32 {
        min_y - self.template.template.size().y / 2 - random.next_int_bounded(3)
    }

    /// `adjustPositionHeight(newHeight)`.
    pub fn adjust_position_height(&mut self, height: i32) {
        self.height_adjusted = true;
        let p = self.template.template_position;
        self.template.template_position = BlockPos::new(p.x, height, p.z);
    }
}

/// `level.getHeight(type, x, z)`, or the level's lowest Y for columns of chunks the region
/// does not hold (Vanilla reads an empty heightmap of a chunk that may not have terrain yet).
fn height_or_min(level: &WorldGenRegion<'_>, kind: HeightmapKind, x: i32, z: i32) -> i32 {
    if level.has_chunk(x >> 4, z >> 4) {
        level.height_at(kind, x, z)
    } else {
        level.min_y()
    }
}

impl StructurePiece for ShipwreckPiece {
    fn base(&self) -> &PieceBase {
        &self.template.base
    }

    fn base_mut(&mut self) -> &mut PieceBase {
        &mut self.template.base
    }

    /// `addAdditionalSaveData`.
    fn additional_save_data(&self) -> Option<Compound> {
        Some(
            self.template
                .save_data()
                .with("isBeached", self.is_beached)
                .with("Rot", self.template.settings.rotation.legacy_name())
                .with("height_adjusted", self.height_adjusted),
        )
    }

    fn move_by(&mut self, dx: i32, dy: i32, dz: i32) {
        self.template.move_by(dx, dy, dz);
    }

    fn rotation(&self) -> Rotation {
        self.template.rotation()
    }

    fn post_process(
        &mut self,
        ctx: &mut PieceContext<'_, '_>,
        chunk_bb: &mut BoundingBox,
        _chunk_pos: ChunkPos,
        reference: BlockPos,
    ) {
        if !self.height_adjusted && !self.is_too_big_to_fit_in_world_gen_region() {
            let level = &*ctx.level;
            let mut min_y = level.max_y() + 1;
            let mut mean = 0;
            let size = self.template.template.size();
            let kind = if self.is_beached {
                HeightmapKind::WorldSurfaceWg
            } else {
                HeightmapKind::OceanFloorWg
            };
            let base_size = size.x * size.z;
            let p = self.template.template_position;
            if base_size == 0 {
                mean = height_or_min(level, kind, p.x, p.z);
            } else {
                for z in p.z..p.z + size.z {
                    for x in p.x..p.x + size.x {
                        let height = height_or_min(level, kind, x, z);
                        mean += height;
                        min_y = min_y.min(height);
                    }
                }
                mean /= base_size;
            }
            let height = if self.is_beached {
                self.calculate_beached_position(min_y, &mut *ctx.random)
            } else {
                mean
            };
            self.adjust_position_height(height);
        }
        self.template
            .place(ctx, chunk_bb, reference, |marker, pos, ctx, _| {
                // `handleDataMarker`: `RandomizableContainer.setBlockEntityLootTable`.
                let loot = match marker {
                    "map_chest" => "minecraft:chests/shipwreck_map",
                    "treasure_chest" => "minecraft:chests/shipwreck_treasure",
                    "supply_chest" => "minecraft:chests/shipwreck_supply",
                    _ => return,
                };
                let below = pos.relative(Direction::Down);
                if crate::feature::region::block_entity_type(ctx.level.block(below))
                    .is_some_and(|kind| RANDOMIZABLE_CONTAINERS.contains(&kind))
                {
                    let seed = ctx.random.next_long();
                    set_loot_table(ctx.level, below, loot, seed);
                }
            });
    }
}

//! Igloos (`IglooStructure`, `IglooPieces`): the igloo, optionally a ladder shaft and a
//! basement laboratory.

use std::sync::Arc;

use serde_json::Value;
use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, ChunkPos, Direction};
use vanillite_chunk::HeightmapKind;
use vanillite_data::block::{BlockState, ids};

use vanillite_nbt::Compound;

use crate::Error;
use crate::feature::region::block_entity_type;
use crate::structure::piece::{PieceContext, set_loot_table};
use crate::structure::structure::on_top_of_chunk_center;
use crate::structure::{
    GenerationContext, GenerationStub, PieceBase, PieceSettings, StructureParser, StructurePiece,
    StructureType, TemplatePiece,
};
use crate::template::{
    BoundingBox, LiquidSettings, Rotation, StructureProcessor, StructureTemplate,
    StructureTemplate as Template,
};

/// `BuiltInLootTables.IGLOO_CHEST`.
const LOOT_TABLE: &str = "minecraft:chests/igloo_chest";

/// The three igloo templates with their rotation pivot and offset (`PIVOTS`, `OFFSETS`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IglooPart {
    /// `igloo/top`.
    Top,
    /// `igloo/middle`.
    Ladder,
    /// `igloo/bottom`.
    Laboratory,
}

impl IglooPart {
    const ALL: [Self; 3] = [Self::Top, Self::Ladder, Self::Laboratory];

    fn name(self) -> &'static str {
        match self {
            Self::Top => "minecraft:igloo/top",
            Self::Ladder => "minecraft:igloo/middle",
            Self::Laboratory => "minecraft:igloo/bottom",
        }
    }

    fn pivot(self) -> BlockPos {
        match self {
            Self::Top => BlockPos::new(3, 5, 5),
            Self::Ladder => BlockPos::new(1, 3, 1),
            Self::Laboratory => BlockPos::new(3, 6, 7),
        }
    }

    fn offset(self) -> BlockPos {
        match self {
            Self::Top => BlockPos::ZERO,
            Self::Ladder => BlockPos::new(2, -3, 4),
            Self::Laboratory => BlockPos::new(0, -3, -2),
        }
    }

    /// `IglooPiece.makeSettings(rotation, location)`.
    fn settings(self, rotation: Rotation) -> PieceSettings {
        PieceSettings {
            rotation,
            rotation_pivot: self.pivot(),
            processors: vec![StructureProcessor::BlockIgnore(vec![ids::STRUCTURE_BLOCK])],
            liquid_settings: LiquidSettings::IgnoreWaterlogging,
            ..PieceSettings::new()
        }
    }
}

/// `IglooStructure`.
#[derive(Debug)]
pub struct IglooStructure {
    /// The templates by [`IglooPart`] order.
    templates: [Arc<StructureTemplate>; 3],
}

impl StructureType for IglooStructure {
    fn parse(_json: &Value, parser: &mut StructureParser<'_>) -> Result<Self, Error> {
        let [a, b, c] = IglooPart::ALL;
        Ok(Self {
            templates: [
                parser.template(a.name())?,
                parser.template(b.name())?,
                parser.template(c.name())?,
            ],
        })
    }

    fn find_generation_point<'s>(
        &'s self,
        ctx: &mut GenerationContext<'_>,
    ) -> Option<GenerationStub<'s>> {
        on_top_of_chunk_center(ctx, HeightmapKind::WorldSurfaceWg, |builder, ctx| {
            let random = &mut ctx.random.random;
            let position =
                BlockPos::new(ctx.chunk_pos.min_block_x(), 90, ctx.chunk_pos.min_block_z());
            let rotation = Rotation::random(random);
            // `IglooPieces.addPieces`.
            let piece = |part: IglooPart, depth: i32| {
                Box::new(IglooPiece::new(
                    Arc::clone(&self.templates[part as usize]),
                    part,
                    position,
                    rotation,
                    depth,
                ))
            };
            if random.next_double() < 0.5 {
                let depth = random.next_int_bounded(8) + 4;
                builder.add_piece(piece(IglooPart::Laboratory, depth * 3));
                for i in 0..depth - 1 {
                    builder.add_piece(piece(IglooPart::Ladder, i * 3));
                }
            }
            builder.add_piece(piece(IglooPart::Top, 0));
        })
    }
}

/// `IglooPieces.IglooPiece` (`minecraft:iglu`).
#[derive(Clone, Debug)]
pub struct IglooPiece {
    pub template: TemplatePiece,
    pub part: IglooPart,
}

impl IglooPiece {
    /// `new IglooPiece(manager, location, position, rotation, depth)`.
    pub fn new(
        template: Arc<StructureTemplate>,
        part: IglooPart,
        position: BlockPos,
        rotation: Rotation,
        depth: i32,
    ) -> Self {
        let offset = part.offset();
        let position = position.offset(offset.x, offset.y - depth, offset.z);
        Self {
            template: TemplatePiece::new(
                "minecraft:iglu",
                0,
                part.name().to_owned(),
                template,
                part.settings(rotation),
                position,
            ),
            part,
        }
    }
}

impl StructurePiece for IglooPiece {
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
                .with("Rot", self.template.settings.rotation.legacy_name()),
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
        let settings = self.part.settings(self.template.settings.rotation);
        let place = settings.place_settings();
        let offset = self.part.offset();
        let relative = Template::calculate_relative_position(
            &place,
            BlockPos::new(3 - offset.x, 0, -offset.z),
        );
        let entrance = offset_by(self.template.template_position, relative);
        let height = ctx
            .level
            .height_at(HeightmapKind::WorldSurfaceWg, entrance.x, entrance.z);
        let old_position = self.template.template_position;
        self.template.template_position = old_position.offset(0, height - 90 - 1, 0);
        self.template
            .place(ctx, chunk_bb, reference, |marker, pos, ctx, _| {
                // `handleDataMarker`.
                if marker == "chest" {
                    ctx.level.set_block_and_update(pos, BlockState::AIR);
                    let below = pos.relative(Direction::Down);
                    if matches!(
                        block_entity_type(ctx.level.block(below)),
                        Some("minecraft:chest" | "minecraft:trapped_chest")
                    ) {
                        let seed = ctx.random.next_long();
                        set_loot_table(ctx.level, below, LOOT_TABLE, seed);
                    }
                }
            });
        if self.part == IglooPart::Top {
            let relative = Template::calculate_relative_position(&place, BlockPos::new(3, 0, 5));
            let trap_door = offset_by(self.template.template_position, relative);
            let below = ctx.level.block(trap_door.relative(Direction::Down));
            if !below.is_air() && below.block() != ids::LADDER {
                ctx.level
                    .set_block_and_update(trap_door, ids::SNOW_BLOCK.default_state());
            }
        }
        self.template.template_position = old_position;
    }
}

/// `pos.offset(delta)`.
fn offset_by(pos: BlockPos, delta: BlockPos) -> BlockPos {
    pos.offset(delta.x, delta.y, delta.z)
}

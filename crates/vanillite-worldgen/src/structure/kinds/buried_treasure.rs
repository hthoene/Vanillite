//! Buried treasure (`BuriedTreasureStructure`, `BuriedTreasurePieces`).

use serde_json::Value;
use vanillite_base::{BlockPos, ChunkPos, Direction};
use vanillite_chunk::HeightmapKind;
use vanillite_data::block::{BlockState, ids};

use vanillite_nbt::Compound;

use crate::Error;
use crate::structure::piece::{PieceContext, create_chest_at};
use crate::structure::structure::on_top_of_chunk_center;
use crate::structure::{
    GenerationContext, GenerationStub, PieceBase, StructureParser, StructurePiece, StructureType,
};
use crate::template::BoundingBox;

/// `BuiltInLootTables.BURIED_TREASURE`.
const LOOT_TABLE: &str = "minecraft:chests/buried_treasure";

/// `BuriedTreasureStructure`.
#[derive(Debug)]
pub struct BuriedTreasureStructure;

impl StructureType for BuriedTreasureStructure {
    fn parse(_json: &Value, _parser: &mut StructureParser<'_>) -> Result<Self, Error> {
        Ok(Self)
    }

    fn find_generation_point<'s>(
        &'s self,
        ctx: &mut GenerationContext<'_>,
    ) -> Option<GenerationStub<'s>> {
        on_top_of_chunk_center(ctx, HeightmapKind::OceanFloorWg, |builder, ctx| {
            let pos = BlockPos::new(
                ctx.chunk_pos.min_block_x() + 9,
                90,
                ctx.chunk_pos.min_block_z() + 9,
            );
            builder.add_piece(Box::new(BuriedTreasurePiece::new(pos)));
        })
    }
}

/// `BuriedTreasurePieces.BuriedTreasurePiece` (`minecraft:btp`).
#[derive(Clone, Debug)]
pub struct BuriedTreasurePiece {
    pub base: PieceBase,
}

impl BuriedTreasurePiece {
    /// `new BuriedTreasurePiece(offset)`.
    pub fn new(offset: BlockPos) -> Self {
        Self {
            base: PieceBase::new("minecraft:btp", 0, BoundingBox::of_block(offset)),
        }
    }
}

/// `BuriedTreasurePiece.isLiquid(state)`.
fn is_liquid(state: BlockState) -> bool {
    matches!(state.block(), ids::WATER | ids::LAVA)
}

impl StructurePiece for BuriedTreasurePiece {
    fn base(&self) -> &PieceBase {
        &self.base
    }

    fn base_mut(&mut self) -> &mut PieceBase {
        &mut self.base
    }

    /// `addAdditionalSaveData`.
    fn additional_save_data(&self) -> Option<Compound> {
        Some(Compound::new())
    }

    fn post_process(
        &mut self,
        ctx: &mut PieceContext<'_, '_>,
        chunk_bb: &mut BoundingBox,
        _chunk_pos: ChunkPos,
        _reference: BlockPos,
    ) {
        let level = &mut *ctx.level;
        let (x, z) = (
            self.base.bounding_box.min_x(),
            self.base.bounding_box.min_z(),
        );
        let y = level.height_at(HeightmapKind::OceanFloorWg, x, z);
        let mut pos = BlockPos::new(x, y, z);
        while pos.y > level.min_y() {
            let current = level.block(pos);
            let below = level.block(pos.relative(Direction::Down));
            if matches!(
                below.block(),
                ids::SANDSTONE | ids::STONE | ids::ANDESITE | ids::GRANITE | ids::DIORITE
            ) {
                let soft = if !current.is_air() && !is_liquid(current) {
                    current
                } else {
                    ids::SAND.default_state()
                };
                for direction in Direction::ALL {
                    let relative = pos.relative(direction);
                    let relative_state = level.block(relative);
                    if relative_state.is_air() || is_liquid(relative_state) {
                        let below_relative = level.block(relative.relative(Direction::Down));
                        if (below_relative.is_air() || is_liquid(below_relative))
                            && direction != Direction::Up
                        {
                            level.set_block_and_update(relative, below);
                        } else {
                            level.set_block_and_update(relative, soft);
                        }
                    }
                }
                self.base.bounding_box = BoundingBox::of_block(pos);
                create_chest_at(level, chunk_bb, ctx.random, pos, LOOT_TABLE, None);
                return;
            }
            pos = pos.relative(Direction::Down);
        }
    }
}

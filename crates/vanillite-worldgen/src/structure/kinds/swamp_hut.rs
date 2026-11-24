//! Swamp huts (`SwampHutStructure`, `SwampHutPiece`).

use serde_json::Value;
use vanillite_base::{BlockPos, ChunkPos, Direction};
use vanillite_chunk::HeightmapKind;
use vanillite_data::block::{BlockState, ids};

use vanillite_nbt::Compound;

use crate::Error;
use crate::feature::region::entity_tag;
use crate::structure::piece::{PieceContext, random_horizontal_direction, with_facing};
use crate::structure::scattered::ScatteredPiece;
use crate::structure::structure::on_top_of_chunk_center;
use crate::structure::{
    GenerationContext, GenerationStub, PieceBase, StructureParser, StructurePiece, StructureType,
};
use crate::template::BoundingBox;

/// `SwampHutStructure`.
#[derive(Debug)]
pub struct SwampHutStructure;

impl StructureType for SwampHutStructure {
    fn parse(_json: &Value, _parser: &mut StructureParser<'_>) -> Result<Self, Error> {
        Ok(Self)
    }

    fn find_generation_point<'s>(
        &'s self,
        ctx: &mut GenerationContext<'_>,
    ) -> Option<GenerationStub<'s>> {
        on_top_of_chunk_center(ctx, HeightmapKind::WorldSurfaceWg, |builder, ctx| {
            let (x, z) = (ctx.chunk_pos.min_block_x(), ctx.chunk_pos.min_block_z());
            let direction = random_horizontal_direction(&mut ctx.random.random);
            builder.add_piece(Box::new(SwampHutPiece::new(x, z, direction)));
        })
    }
}

/// `SwampHutPiece` (`minecraft:tesh`).
#[derive(Clone, Debug)]
pub struct SwampHutPiece {
    pub scattered: ScatteredPiece,
    /// `spawnedWitch`.
    pub spawned_witch: bool,
    /// `spawnedCat`.
    pub spawned_cat: bool,
}

impl SwampHutPiece {
    /// `new SwampHutPiece(random, west, north)` with the direction already drawn.
    pub fn new(west: i32, north: i32, direction: Direction) -> Self {
        Self {
            scattered: ScatteredPiece::new("minecraft:tesh", west, 64, north, 7, 7, 9, direction),
            spawned_witch: false,
            spawned_cat: false,
        }
    }
}

impl StructurePiece for SwampHutPiece {
    fn base(&self) -> &PieceBase {
        &self.scattered.base
    }

    fn base_mut(&mut self) -> &mut PieceBase {
        &mut self.scattered.base
    }

    /// `addAdditionalSaveData`.
    fn additional_save_data(&self) -> Option<Compound> {
        Some(
            self.scattered
                .save_data()
                .with("Witch", self.spawned_witch)
                .with("Cat", self.spawned_cat),
        )
    }

    fn post_process(
        &mut self,
        ctx: &mut PieceContext<'_, '_>,
        chunk_bb: &mut BoundingBox,
        _chunk_pos: ChunkPos,
        _reference: BlockPos,
    ) {
        let level = &mut *ctx.level;
        if !self
            .scattered
            .update_average_ground_height(level, chunk_bb, 0)
        {
            return;
        }
        let planks = ids::SPRUCE_PLANKS.default_state();
        let log = ids::OAK_LOG.default_state();
        let fence = ids::OAK_FENCE.default_state();
        let bb: &BoundingBox = chunk_bb;
        self.generate_box(level, bb, 1, 1, 1, 5, 1, 7, planks, planks, false);
        self.generate_box(level, bb, 1, 4, 2, 5, 4, 7, planks, planks, false);
        self.generate_box(level, bb, 2, 1, 0, 4, 1, 0, planks, planks, false);
        self.generate_box(level, bb, 2, 2, 2, 3, 3, 2, planks, planks, false);
        self.generate_box(level, bb, 1, 2, 3, 1, 3, 6, planks, planks, false);
        self.generate_box(level, bb, 5, 2, 3, 5, 3, 6, planks, planks, false);
        self.generate_box(level, bb, 2, 2, 7, 4, 3, 7, planks, planks, false);
        self.generate_box(level, bb, 1, 0, 2, 1, 3, 2, log, log, false);
        self.generate_box(level, bb, 5, 0, 2, 5, 3, 2, log, log, false);
        self.generate_box(level, bb, 1, 0, 7, 1, 3, 7, log, log, false);
        self.generate_box(level, bb, 5, 0, 7, 5, 3, 7, log, log, false);
        self.place_block(level, fence, 2, 3, 2, bb);
        self.place_block(level, fence, 3, 3, 7, bb);
        self.place_block(level, BlockState::AIR, 1, 3, 4, bb);
        self.place_block(level, BlockState::AIR, 5, 3, 4, bb);
        self.place_block(level, BlockState::AIR, 5, 3, 5, bb);
        self.place_block(level, ids::POTTED_RED_MUSHROOM.default_state(), 1, 3, 5, bb);
        self.place_block(level, ids::CRAFTING_TABLE.default_state(), 3, 2, 6, bb);
        self.place_block(level, ids::CAULDRON.default_state(), 4, 2, 6, bb);
        self.place_block(level, fence, 1, 2, 1, bb);
        self.place_block(level, fence, 5, 2, 1, bb);
        let stairs = ids::SPRUCE_STAIRS.default_state();
        let north = with_facing(stairs, Direction::North);
        let east = with_facing(stairs, Direction::East);
        let west = with_facing(stairs, Direction::West);
        let south = with_facing(stairs, Direction::South);
        self.generate_box(level, bb, 0, 4, 1, 6, 4, 1, north, north, false);
        self.generate_box(level, bb, 0, 4, 2, 0, 4, 7, east, east, false);
        self.generate_box(level, bb, 6, 4, 2, 6, 4, 7, west, west, false);
        self.generate_box(level, bb, 0, 4, 8, 6, 4, 8, south, south, false);
        let shape = |state: BlockState, shape: &str| state.with("shape", shape).unwrap_or(state);
        self.place_block(level, shape(north, "outer_right"), 0, 4, 1, bb);
        self.place_block(level, shape(north, "outer_left"), 6, 4, 1, bb);
        self.place_block(level, shape(south, "outer_left"), 0, 4, 8, bb);
        self.place_block(level, shape(south, "outer_right"), 6, 4, 8, bb);
        for z in [2, 7] {
            for x in [1, 5] {
                self.fill_column_down(level, log, x, -1, z, bb);
            }
        }
        // `spawnWitch` and `spawnCat`: `finalizeSpawn` (which draws from the level's random)
        // is not run, the mobs are recorded with their type and position.
        let pos = self.world_pos(2, 2, 5);
        let (x, y, z) = (
            f64::from(pos.x) + 0.5,
            f64::from(pos.y),
            f64::from(pos.z) + 0.5,
        );
        if !self.spawned_witch && chunk_bb.is_inside(pos) {
            self.spawned_witch = true;
            level.add_fresh_entity(entity_tag("minecraft:witch", x, y, z));
        }
        if !self.spawned_cat && chunk_bb.is_inside(pos) {
            self.spawned_cat = true;
            level.add_fresh_entity(entity_tag("minecraft:cat", x, y, z));
        }
    }
}

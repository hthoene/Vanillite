//! Desert pyramids (`DesertPyramidStructure`, `DesertPyramidPiece`).

use serde_json::Value;
use vanillite_base::random::{LegacyRandom, PositionalRandomFactory, RandomSource};
use vanillite_base::{BlockPos, ChunkPos, Direction};
use vanillite_data::block::{BlockState, ids};

use vanillite_nbt::Compound;

use crate::Error;
use crate::feature::WorldGenRegion;
use crate::structure::piece::{
    HORIZONTAL, PieceContext, create_bounding_box, data_2d, random_horizontal_direction,
    set_loot_table, with_facing,
};
use crate::structure::scattered::ScatteredPiece;
use crate::structure::structure::single_piece_generation_point;
use crate::structure::{
    GenerationContext, GenerationStub, PieceBase, StructureParser, StructurePiece, StructureType,
};
use crate::template::transform::rotate_state;
use crate::template::{BoundingBox, Rotation};

/// `BuiltInLootTables.DESERT_PYRAMID`.
const CHEST_LOOT: &str = "minecraft:chests/desert_pyramid";
/// `BuiltInLootTables.DESERT_PYRAMID_ARCHAEOLOGY`.
const ARCHAEOLOGY_LOOT: &str = "minecraft:archaeology/desert_pyramid";

/// `DesertPyramidStructure` (a `SinglePieceStructure` of 21×21).
#[derive(Debug)]
pub struct DesertPyramidStructure;

impl StructureType for DesertPyramidStructure {
    fn parse(_json: &Value, _parser: &mut StructureParser<'_>) -> Result<Self, Error> {
        Ok(Self)
    }

    fn find_generation_point<'s>(
        &'s self,
        ctx: &mut GenerationContext<'_>,
    ) -> Option<GenerationStub<'s>> {
        single_piece_generation_point(ctx, 21, 21, |random, x, z| {
            let direction = random_horizontal_direction(&mut random.random);
            Box::new(DesertPyramidPiece::new(x, z, direction))
        })
    }

    /// `afterPlace`: suspicious sand at a few of the cellar's sand positions and the
    /// collapsed roof position.
    fn after_place(
        &self,
        ctx: &mut PieceContext<'_, '_>,
        chunk_bb: &BoundingBox,
        _chunk_pos: ChunkPos,
        pieces: &mut [Box<dyn StructurePiece>],
    ) {
        // `SortedArraySet.create(Vec3i::compareTo)`: y, then z, then x.
        let mut unique: Vec<BlockPos> = Vec::new();
        for piece in pieces.iter() {
            if let Some(pyramid) = (**piece).downcast_ref::<DesertPyramidPiece>() {
                unique.extend_from_slice(&pyramid.potential_suspicious_sand);
                place_suspicious_sand(ctx.level, chunk_bb, pyramid.random_collapsed_roof_pos);
            }
        }
        unique.sort_by_key(|p| (p.y, p.z, p.x));
        unique.dedup();
        let center = create_bounding_box(pieces).map_or(BlockPos::ZERO, |bb| bb.center());
        let mut positional = LegacyRandom::new(ctx.level.seed())
            .fork_positional()
            .at(center.x, center.y, center.z);
        // `Util.shuffle(list, random)`.
        let mut shuffled = unique.clone();
        for i in (2..=shuffled.len()).rev() {
            let swap = positional.next_int_bounded(i as i32) as usize;
            shuffled.swap(i - 1, swap);
        }
        let mut to_place = unique.len().min(positional.next_int_between(5, 8) as usize);
        for pos in shuffled {
            if to_place > 0 {
                to_place -= 1;
                place_suspicious_sand(ctx.level, chunk_bb, pos);
            } else if chunk_bb.is_inside(pos) {
                ctx.level.set_block(pos, ids::SAND.default_state(), 2);
            }
        }
    }
}

/// `DesertPyramidStructure.placeSuspiciousSand(chunkBB, level, pos)`.
fn place_suspicious_sand(level: &mut WorldGenRegion<'_>, chunk_bb: &BoundingBox, pos: BlockPos) {
    if !chunk_bb.is_inside(pos) {
        return;
    }
    level.set_block(pos, ids::SUSPICIOUS_SAND.default_state(), 2);
    if crate::feature::region::block_entity_type(level.block(pos))
        == Some("minecraft:brushable_block")
    {
        set_loot_table(level, pos, ARCHAEOLOGY_LOOT, pos.as_long());
    }
}

/// `DesertPyramidPiece` (`minecraft:tedp`).
#[derive(Clone, Debug)]
pub struct DesertPyramidPiece {
    pub scattered: ScatteredPiece,
    /// `hasPlacedChest`, by `Direction.get2DDataValue`.
    pub has_placed_chest: [bool; 4],
    /// `potentialSuspiciousSandWorldPositions` (grows with every chunk the cellar is placed in,
    /// as in Vanilla).
    pub potential_suspicious_sand: Vec<BlockPos>,
    /// `randomCollapsedRoofPos`.
    pub random_collapsed_roof_pos: BlockPos,
}

impl DesertPyramidPiece {
    /// `new DesertPyramidPiece(random, west, north)` with the direction already drawn.
    pub fn new(west: i32, north: i32, direction: Direction) -> Self {
        Self {
            scattered: ScatteredPiece::new(
                "minecraft:tedp",
                west,
                64,
                north,
                21,
                15,
                21,
                direction,
            ),
            has_placed_chest: [false; 4],
            potential_suspicious_sand: Vec::new(),
            random_collapsed_roof_pos: BlockPos::ZERO,
        }
    }

    /// `addCellar(level, chunkBB)`.
    fn add_cellar(&mut self, level: &mut WorldGenRegion<'_>, bb: &BoundingBox) {
        let room = BlockPos::new(16, -4, 13);
        self.add_cellar_stairs(room, level, bb);
        self.add_cellar_room(room, level, bb);
    }

    /// `addCellarStairs(roomCenter, level, chunkBB)`.
    fn add_cellar_stairs(&self, room: BlockPos, level: &mut WorldGenRegion<'_>, bb: &BoundingBox) {
        let (x, y, z) = (room.x, room.y, room.z);
        let stairs = rotate_state(
            ids::SANDSTONE_STAIRS.default_state(),
            Rotation::Counterclockwise90,
        );
        self.place_block(level, stairs, 13, -1, 17, bb);
        self.place_block(level, stairs, 14, -2, 17, bb);
        self.place_block(level, stairs, 15, -3, 17, bb);
        let sand = ids::SAND.default_state();
        let sandstone = ids::SANDSTONE.default_state();
        let variant = level.random().next_boolean();
        self.place_block(level, sand, x - 4, y + 4, z + 4, bb);
        self.place_block(level, sand, x - 3, y + 4, z + 4, bb);
        self.place_block(level, sand, x - 2, y + 4, z + 4, bb);
        self.place_block(level, sand, x - 1, y + 4, z + 4, bb);
        self.place_block(level, sand, x, y + 4, z + 4, bb);
        self.place_block(level, sand, x - 2, y + 3, z + 4, bb);
        self.place_block(
            level,
            if variant { sand } else { sandstone },
            x - 1,
            y + 3,
            z + 4,
            bb,
        );
        self.place_block(
            level,
            if variant { sandstone } else { sand },
            x,
            y + 3,
            z + 4,
            bb,
        );
        self.place_block(level, sand, x - 1, y + 2, z + 4, bb);
        self.place_block(level, sandstone, x, y + 2, z + 4, bb);
        self.place_block(level, sand, x, y + 1, z + 4, bb);
    }

    /// `addCellarRoom(roomCenter, level, chunkBB)`.
    fn add_cellar_room(
        &mut self,
        room: BlockPos,
        level: &mut WorldGenRegion<'_>,
        bb: &BoundingBox,
    ) {
        let (x, y, z) = (room.x, room.y, room.z);
        let cut = ids::CUT_SANDSTONE.default_state();
        let chiseled = ids::CHISELED_SANDSTONE.default_state();
        self.generate_box(
            level,
            bb,
            x - 3,
            y + 1,
            z - 3,
            x - 3,
            y + 1,
            z + 2,
            cut,
            cut,
            true,
        );
        self.generate_box(
            level,
            bb,
            x + 3,
            y + 1,
            z - 3,
            x + 3,
            y + 1,
            z + 2,
            cut,
            cut,
            true,
        );
        self.generate_box(
            level,
            bb,
            x - 3,
            y + 1,
            z - 3,
            x + 3,
            y + 1,
            z - 2,
            cut,
            cut,
            true,
        );
        self.generate_box(
            level,
            bb,
            x - 3,
            y + 1,
            z + 3,
            x + 3,
            y + 1,
            z + 3,
            cut,
            cut,
            true,
        );
        self.generate_box(
            level,
            bb,
            x - 3,
            y + 2,
            z - 3,
            x - 3,
            y + 2,
            z + 2,
            chiseled,
            chiseled,
            true,
        );
        self.generate_box(
            level,
            bb,
            x + 3,
            y + 2,
            z - 3,
            x + 3,
            y + 2,
            z + 2,
            chiseled,
            chiseled,
            true,
        );
        self.generate_box(
            level,
            bb,
            x - 3,
            y + 2,
            z - 3,
            x + 3,
            y + 2,
            z - 2,
            chiseled,
            chiseled,
            true,
        );
        self.generate_box(
            level,
            bb,
            x - 3,
            y + 2,
            z + 3,
            x + 3,
            y + 2,
            z + 3,
            chiseled,
            chiseled,
            true,
        );
        self.generate_box(
            level,
            bb,
            x - 3,
            -1,
            z - 3,
            x - 3,
            -1,
            z + 2,
            cut,
            cut,
            true,
        );
        self.generate_box(
            level,
            bb,
            x + 3,
            -1,
            z - 3,
            x + 3,
            -1,
            z + 2,
            cut,
            cut,
            true,
        );
        self.generate_box(
            level,
            bb,
            x - 3,
            -1,
            z - 3,
            x + 3,
            -1,
            z - 2,
            cut,
            cut,
            true,
        );
        self.generate_box(
            level,
            bb,
            x - 3,
            -1,
            z + 3,
            x + 3,
            -1,
            z + 3,
            cut,
            cut,
            true,
        );
        self.place_sand_box(x - 2, y + 1, z - 2, x + 2, y + 3, z + 2);
        self.place_collapsed_roof(level, bb, x - 2, y + 4, z - 2, x + 2, z + 2);
        let orange = ids::ORANGE_TERRACOTTA.default_state();
        let blue = ids::BLUE_TERRACOTTA.default_state();
        self.place_block(level, blue, x, y, z, bb);
        self.place_block(level, orange, x + 1, y, z - 1, bb);
        self.place_block(level, orange, x + 1, y, z + 1, bb);
        self.place_block(level, orange, x - 1, y, z - 1, bb);
        self.place_block(level, orange, x - 1, y, z + 1, bb);
        self.place_block(level, orange, x + 2, y, z, bb);
        self.place_block(level, orange, x - 2, y, z, bb);
        self.place_block(level, orange, x, y, z + 2, bb);
        self.place_block(level, orange, x, y, z - 2, bb);
        self.place_block(level, orange, x + 3, y, z, bb);
        self.place_sand(x + 3, y + 1, z);
        self.place_sand(x + 3, y + 2, z);
        self.place_block(level, cut, x + 4, y + 1, z, bb);
        self.place_block(level, chiseled, x + 4, y + 2, z, bb);
        self.place_block(level, orange, x - 3, y, z, bb);
        self.place_sand(x - 3, y + 1, z);
        self.place_sand(x - 3, y + 2, z);
        self.place_block(level, cut, x - 4, y + 1, z, bb);
        self.place_block(level, chiseled, x - 4, y + 2, z, bb);
        self.place_block(level, orange, x, y, z + 3, bb);
        self.place_sand(x, y + 1, z + 3);
        self.place_sand(x, y + 2, z + 3);
        self.place_block(level, orange, x, y, z - 3, bb);
        self.place_sand(x, y + 1, z - 3);
        self.place_sand(x, y + 2, z - 3);
        self.place_block(level, cut, x, y + 1, z - 4, bb);
        self.place_block(level, chiseled, x, -2, z - 4, bb);
    }

    /// `placeSand(x, y, z)`.
    fn place_sand(&mut self, x: i32, y: i32, z: i32) {
        let pos = self.world_pos(x, y, z);
        self.potential_suspicious_sand.push(pos);
    }

    /// `placeSandBox(x0, y0, z0, x1, y1, z1)`.
    fn place_sand_box(&mut self, x0: i32, y0: i32, z0: i32, x1: i32, y1: i32, z1: i32) {
        for y in y0..=y1 {
            for x in x0..=x1 {
                for z in z0..=z1 {
                    self.place_sand(x, y, z);
                }
            }
        }
    }

    /// `placeCollapsedRoof(level, chunkBB, x0, y0, z0, x1, z1)`.
    #[allow(clippy::too_many_arguments)]
    fn place_collapsed_roof(
        &mut self,
        level: &mut WorldGenRegion<'_>,
        bb: &BoundingBox,
        x0: i32,
        y0: i32,
        z0: i32,
        x1: i32,
        z1: i32,
    ) {
        for x in x0..=x1 {
            for z in z0..=z1 {
                // `placeCollapsedRoofPiece`.
                let state = if level.random().next_float() < 0.33 {
                    ids::SANDSTONE.default_state()
                } else {
                    ids::SAND.default_state()
                };
                self.place_block(level, state, x, y0, z, bb);
            }
        }
        let corner = self.world_pos(x0, y0, z0);
        let mut random = LegacyRandom::new(level.seed())
            .fork_positional()
            .at(corner.x, corner.y, corner.z);
        let roof_x = random.next_int_between_inclusive(x0, x1);
        let roof_z = random.next_int_between_inclusive(z0, z1);
        let base = self.base();
        self.random_collapsed_roof_pos = BlockPos::new(
            base.world_x(roof_x, roof_z),
            base.world_y(y0),
            base.world_z(roof_x, roof_z),
        );
    }
}

impl StructurePiece for DesertPyramidPiece {
    fn base(&self) -> &PieceBase {
        &self.scattered.base
    }

    fn base_mut(&mut self) -> &mut PieceBase {
        &mut self.scattered.base
    }

    /// `addAdditionalSaveData`.
    fn additional_save_data(&self) -> Option<Compound> {
        let mut tag = self.scattered.save_data();
        for (i, placed) in self.has_placed_chest.iter().enumerate() {
            tag.insert(format!("hasPlacedChest{i}"), *placed);
        }
        Some(tag)
    }

    fn post_process(
        &mut self,
        ctx: &mut PieceContext<'_, '_>,
        chunk_bb: &mut BoundingBox,
        _chunk_pos: ChunkPos,
        _reference: BlockPos,
    ) {
        let level = &mut *ctx.level;
        let random = &mut *ctx.random;
        let offset = -random.next_int_bounded(3);
        if !self
            .scattered
            .update_height_position_to_lowest_ground_height(level, offset)
        {
            return;
        }
        let bb: &BoundingBox = chunk_bb;
        let (w, d) = (self.scattered.width, self.scattered.depth);
        let sandstone = ids::SANDSTONE.default_state();
        let cut = ids::CUT_SANDSTONE.default_state();
        let chiseled = ids::CHISELED_SANDSTONE.default_state();
        let orange = ids::ORANGE_TERRACOTTA.default_state();
        let blue = ids::BLUE_TERRACOTTA.default_state();
        let air = BlockState::AIR;
        self.generate_box(
            level,
            bb,
            0,
            -4,
            0,
            w - 1,
            0,
            d - 1,
            sandstone,
            sandstone,
            false,
        );
        for p in 1..=9 {
            self.generate_box(
                level,
                bb,
                p,
                p,
                p,
                w - 1 - p,
                p,
                d - 1 - p,
                sandstone,
                sandstone,
                false,
            );
            self.generate_box(
                level,
                bb,
                p + 1,
                p,
                p + 1,
                w - 2 - p,
                p,
                d - 2 - p,
                air,
                air,
                false,
            );
        }
        for x in 0..w {
            for z in 0..d {
                self.fill_column_down(level, sandstone, x, -5, z, bb);
            }
        }
        let stairs = ids::SANDSTONE_STAIRS.default_state();
        let north = with_facing(stairs, Direction::North);
        let south = with_facing(stairs, Direction::South);
        let east = with_facing(stairs, Direction::East);
        let west = with_facing(stairs, Direction::West);
        self.generate_box(level, bb, 0, 0, 0, 4, 9, 4, sandstone, air, false);
        self.generate_box(level, bb, 1, 10, 1, 3, 10, 3, sandstone, sandstone, false);
        self.place_block(level, north, 2, 10, 0, bb);
        self.place_block(level, south, 2, 10, 4, bb);
        self.place_block(level, east, 0, 10, 2, bb);
        self.place_block(level, west, 4, 10, 2, bb);
        self.generate_box(level, bb, w - 5, 0, 0, w - 1, 9, 4, sandstone, air, false);
        self.generate_box(
            level,
            bb,
            w - 4,
            10,
            1,
            w - 2,
            10,
            3,
            sandstone,
            sandstone,
            false,
        );
        self.place_block(level, north, w - 3, 10, 0, bb);
        self.place_block(level, south, w - 3, 10, 4, bb);
        self.place_block(level, east, w - 5, 10, 2, bb);
        self.place_block(level, west, w - 1, 10, 2, bb);
        self.generate_box(level, bb, 8, 0, 0, 12, 4, 4, sandstone, air, false);
        self.generate_box(level, bb, 9, 1, 0, 11, 3, 4, air, air, false);
        self.place_block(level, cut, 9, 1, 1, bb);
        self.place_block(level, cut, 9, 2, 1, bb);
        self.place_block(level, cut, 9, 3, 1, bb);
        self.place_block(level, cut, 10, 3, 1, bb);
        self.place_block(level, cut, 11, 3, 1, bb);
        self.place_block(level, cut, 11, 2, 1, bb);
        self.place_block(level, cut, 11, 1, 1, bb);
        self.generate_box(level, bb, 4, 1, 1, 8, 3, 3, sandstone, air, false);
        self.generate_box(level, bb, 4, 1, 2, 8, 2, 2, air, air, false);
        self.generate_box(level, bb, 12, 1, 1, 16, 3, 3, sandstone, air, false);
        self.generate_box(level, bb, 12, 1, 2, 16, 2, 2, air, air, false);
        self.generate_box(
            level,
            bb,
            5,
            4,
            5,
            w - 6,
            4,
            d - 6,
            sandstone,
            sandstone,
            false,
        );
        self.generate_box(level, bb, 9, 4, 9, 11, 4, 11, air, air, false);
        self.generate_box(level, bb, 8, 1, 8, 8, 3, 8, cut, cut, false);
        self.generate_box(level, bb, 12, 1, 8, 12, 3, 8, cut, cut, false);
        self.generate_box(level, bb, 8, 1, 12, 8, 3, 12, cut, cut, false);
        self.generate_box(level, bb, 12, 1, 12, 12, 3, 12, cut, cut, false);
        self.generate_box(level, bb, 1, 1, 5, 4, 4, 11, sandstone, sandstone, false);
        self.generate_box(
            level,
            bb,
            w - 5,
            1,
            5,
            w - 2,
            4,
            11,
            sandstone,
            sandstone,
            false,
        );
        self.generate_box(level, bb, 6, 7, 9, 6, 7, 11, sandstone, sandstone, false);
        self.generate_box(
            level,
            bb,
            w - 7,
            7,
            9,
            w - 7,
            7,
            11,
            sandstone,
            sandstone,
            false,
        );
        self.generate_box(level, bb, 5, 5, 9, 5, 7, 11, cut, cut, false);
        self.generate_box(level, bb, w - 6, 5, 9, w - 6, 7, 11, cut, cut, false);
        self.place_block(level, air, 5, 5, 10, bb);
        self.place_block(level, air, 5, 6, 10, bb);
        self.place_block(level, air, 6, 6, 10, bb);
        self.place_block(level, air, w - 6, 5, 10, bb);
        self.place_block(level, air, w - 6, 6, 10, bb);
        self.place_block(level, air, w - 7, 6, 10, bb);
        self.generate_box(level, bb, 2, 4, 4, 2, 6, 4, air, air, false);
        self.generate_box(level, bb, w - 3, 4, 4, w - 3, 6, 4, air, air, false);
        self.place_block(level, north, 2, 4, 5, bb);
        self.place_block(level, north, 2, 3, 4, bb);
        self.place_block(level, north, w - 3, 4, 5, bb);
        self.place_block(level, north, w - 3, 3, 4, bb);
        self.generate_box(level, bb, 1, 1, 3, 2, 2, 3, sandstone, sandstone, false);
        self.generate_box(
            level,
            bb,
            w - 3,
            1,
            3,
            w - 2,
            2,
            3,
            sandstone,
            sandstone,
            false,
        );
        self.place_block(level, sandstone, 1, 1, 2, bb);
        self.place_block(level, sandstone, w - 2, 1, 2, bb);
        let slab = ids::SANDSTONE_SLAB.default_state();
        self.place_block(level, slab, 1, 2, 2, bb);
        self.place_block(level, slab, w - 2, 2, 2, bb);
        self.place_block(level, west, 2, 1, 2, bb);
        self.place_block(level, east, w - 3, 1, 2, bb);
        self.generate_box(level, bb, 4, 3, 5, 4, 3, 17, sandstone, sandstone, false);
        self.generate_box(
            level,
            bb,
            w - 5,
            3,
            5,
            w - 5,
            3,
            17,
            sandstone,
            sandstone,
            false,
        );
        self.generate_box(level, bb, 3, 1, 5, 4, 2, 16, air, air, false);
        self.generate_box(level, bb, w - 6, 1, 5, w - 5, 2, 16, air, air, false);
        for z in (5..=17).step_by(2) {
            self.place_block(level, cut, 4, 1, z, bb);
            self.place_block(level, chiseled, 4, 2, z, bb);
            self.place_block(level, cut, w - 5, 1, z, bb);
            self.place_block(level, chiseled, w - 5, 2, z, bb);
        }
        for (x, z) in [
            (10, 7),
            (10, 8),
            (9, 9),
            (11, 9),
            (8, 10),
            (12, 10),
            (7, 10),
            (13, 10),
            (9, 11),
            (11, 11),
            (10, 12),
            (10, 13),
        ] {
            self.place_block(level, orange, x, 0, z, bb);
        }
        self.place_block(level, blue, 10, 0, 10, bb);
        for x in [0, w - 1] {
            self.place_block(level, cut, x, 2, 1, bb);
            self.place_block(level, orange, x, 2, 2, bb);
            self.place_block(level, cut, x, 2, 3, bb);
            self.place_block(level, cut, x, 3, 1, bb);
            self.place_block(level, orange, x, 3, 2, bb);
            self.place_block(level, cut, x, 3, 3, bb);
            self.place_block(level, orange, x, 4, 1, bb);
            self.place_block(level, chiseled, x, 4, 2, bb);
            self.place_block(level, orange, x, 4, 3, bb);
            self.place_block(level, cut, x, 5, 1, bb);
            self.place_block(level, orange, x, 5, 2, bb);
            self.place_block(level, cut, x, 5, 3, bb);
            self.place_block(level, orange, x, 6, 1, bb);
            self.place_block(level, chiseled, x, 6, 2, bb);
            self.place_block(level, orange, x, 6, 3, bb);
            self.place_block(level, orange, x, 7, 1, bb);
            self.place_block(level, orange, x, 7, 2, bb);
            self.place_block(level, orange, x, 7, 3, bb);
            self.place_block(level, cut, x, 8, 1, bb);
            self.place_block(level, cut, x, 8, 2, bb);
            self.place_block(level, cut, x, 8, 3, bb);
        }
        for x in [2, w - 3] {
            self.place_block(level, cut, x - 1, 2, 0, bb);
            self.place_block(level, orange, x, 2, 0, bb);
            self.place_block(level, cut, x + 1, 2, 0, bb);
            self.place_block(level, cut, x - 1, 3, 0, bb);
            self.place_block(level, orange, x, 3, 0, bb);
            self.place_block(level, cut, x + 1, 3, 0, bb);
            self.place_block(level, orange, x - 1, 4, 0, bb);
            self.place_block(level, chiseled, x, 4, 0, bb);
            self.place_block(level, orange, x + 1, 4, 0, bb);
            self.place_block(level, cut, x - 1, 5, 0, bb);
            self.place_block(level, orange, x, 5, 0, bb);
            self.place_block(level, cut, x + 1, 5, 0, bb);
            self.place_block(level, orange, x - 1, 6, 0, bb);
            self.place_block(level, chiseled, x, 6, 0, bb);
            self.place_block(level, orange, x + 1, 6, 0, bb);
            self.place_block(level, orange, x - 1, 7, 0, bb);
            self.place_block(level, orange, x, 7, 0, bb);
            self.place_block(level, orange, x + 1, 7, 0, bb);
            self.place_block(level, cut, x - 1, 8, 0, bb);
            self.place_block(level, cut, x, 8, 0, bb);
            self.place_block(level, cut, x + 1, 8, 0, bb);
        }
        self.generate_box(level, bb, 8, 4, 0, 12, 6, 0, cut, cut, false);
        self.place_block(level, air, 8, 6, 0, bb);
        self.place_block(level, air, 12, 6, 0, bb);
        self.place_block(level, orange, 9, 5, 0, bb);
        self.place_block(level, chiseled, 10, 5, 0, bb);
        self.place_block(level, orange, 11, 5, 0, bb);
        self.generate_box(level, bb, 8, -14, 8, 12, -11, 12, cut, cut, false);
        self.generate_box(level, bb, 8, -10, 8, 12, -10, 12, chiseled, chiseled, false);
        self.generate_box(level, bb, 8, -9, 8, 12, -9, 12, cut, cut, false);
        self.generate_box(level, bb, 8, -8, 8, 12, -1, 12, sandstone, sandstone, false);
        self.generate_box(level, bb, 9, -11, 9, 11, -1, 11, air, air, false);
        self.place_block(
            level,
            ids::STONE_PRESSURE_PLATE.default_state(),
            10,
            -11,
            10,
            bb,
        );
        self.generate_box(
            level,
            bb,
            9,
            -13,
            9,
            11,
            -13,
            11,
            ids::TNT.default_state(),
            air,
            false,
        );
        self.place_block(level, air, 8, -11, 10, bb);
        self.place_block(level, air, 8, -10, 10, bb);
        self.place_block(level, chiseled, 7, -10, 10, bb);
        self.place_block(level, cut, 7, -11, 10, bb);
        self.place_block(level, air, 12, -11, 10, bb);
        self.place_block(level, air, 12, -10, 10, bb);
        self.place_block(level, chiseled, 13, -10, 10, bb);
        self.place_block(level, cut, 13, -11, 10, bb);
        self.place_block(level, air, 10, -11, 8, bb);
        self.place_block(level, air, 10, -10, 8, bb);
        self.place_block(level, chiseled, 10, -10, 7, bb);
        self.place_block(level, cut, 10, -11, 7, bb);
        self.place_block(level, air, 10, -11, 12, bb);
        self.place_block(level, air, 10, -10, 12, bb);
        self.place_block(level, chiseled, 10, -10, 13, bb);
        self.place_block(level, cut, 10, -11, 13, bb);
        for direction in HORIZONTAL {
            let index = data_2d(direction);
            if !self.has_placed_chest[index] {
                let [sx, _, sz] = direction.normal();
                self.has_placed_chest[index] =
                    self.create_chest(level, bb, random, 10 + sx * 2, -11, 10 + sz * 2, CHEST_LOOT);
            }
        }
        self.add_cellar(level, bb);
    }
}

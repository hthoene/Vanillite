//! Jungle temples (`JungleTempleStructure`, `JungleTemplePiece`).

use serde_json::Value;
use vanillite_base::random::{DecorationRandom, RandomSource};
use vanillite_base::{BlockPos, ChunkPos, Direction};
use vanillite_data::block::{BlockState, ids};

use vanillite_nbt::Compound;

use crate::Error;
use crate::structure::piece::{
    BlockSelector, PieceContext, random_horizontal_direction, with_facing,
};
use crate::structure::scattered::ScatteredPiece;
use crate::structure::structure::single_piece_generation_point;
use crate::structure::{
    GenerationContext, GenerationStub, PieceBase, StructureParser, StructurePiece, StructureType,
};
use crate::template::BoundingBox;

/// `BuiltInLootTables.JUNGLE_TEMPLE`.
const CHEST_LOOT: &str = "minecraft:chests/jungle_temple";
/// `BuiltInLootTables.JUNGLE_TEMPLE_DISPENSER`.
const DISPENSER_LOOT: &str = "minecraft:chests/jungle_temple_dispenser";

/// `JungleTempleStructure` (a `SinglePieceStructure` of 12×15).
#[derive(Debug)]
pub struct JungleTempleStructure;

impl StructureType for JungleTempleStructure {
    fn parse(_json: &Value, _parser: &mut StructureParser<'_>) -> Result<Self, Error> {
        Ok(Self)
    }

    fn find_generation_point<'s>(
        &'s self,
        ctx: &mut GenerationContext<'_>,
    ) -> Option<GenerationStub<'s>> {
        single_piece_generation_point(ctx, 12, 15, |random, x, z| {
            let direction = random_horizontal_direction(&mut random.random);
            Box::new(JungleTemplePiece::new(x, z, direction))
        })
    }
}

/// `JungleTemplePiece` (`minecraft:tejp`).
#[derive(Clone, Debug)]
pub struct JungleTemplePiece {
    pub scattered: ScatteredPiece,
    pub placed_main_chest: bool,
    pub placed_hidden_chest: bool,
    pub placed_trap1: bool,
    pub placed_trap2: bool,
}

impl JungleTemplePiece {
    /// `new JungleTemplePiece(random, west, north)` with the direction already drawn.
    pub fn new(west: i32, north: i32, direction: Direction) -> Self {
        Self {
            scattered: ScatteredPiece::new(
                "minecraft:tejp",
                west,
                64,
                north,
                12,
                10,
                15,
                direction,
            ),
            placed_main_chest: false,
            placed_hidden_chest: false,
            placed_trap1: false,
            placed_trap2: false,
        }
    }
}

/// `JungleTemplePiece.MossStoneSelector`.
struct MossStoneSelector(BlockState);

impl BlockSelector for MossStoneSelector {
    fn next(&mut self, random: &mut DecorationRandom, _x: i32, _y: i32, _z: i32, _edge: bool) {
        self.0 = if random.next_float() < 0.4 {
            ids::COBBLESTONE.default_state()
        } else {
            ids::MOSSY_COBBLESTONE.default_state()
        };
    }

    fn get_next(&self) -> BlockState {
        self.0
    }
}

/// `state.setValue(property, value)` for properties every caller's block has.
fn set(state: BlockState, pairs: &[(&str, &str)]) -> BlockState {
    pairs
        .iter()
        .fold(state, |s, (k, v)| s.with(k, v).unwrap_or(s))
}

impl StructurePiece for JungleTemplePiece {
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
                .with("placedMainChest", self.placed_main_chest)
                .with("placedHiddenChest", self.placed_hidden_chest)
                .with("placedTrap1", self.placed_trap1)
                .with("placedTrap2", self.placed_trap2),
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
        let random = &mut *ctx.random;
        if !self
            .scattered
            .update_average_ground_height(level, chunk_bb, 0)
        {
            return;
        }
        let bb: &BoundingBox = chunk_bb;
        let (width, depth) = (self.scattered.width, self.scattered.depth);
        let mut stone = MossStoneSelector(BlockState::AIR);
        let mut sel = |this: &Self,
                       level: &mut crate::feature::WorldGenRegion<'_>,
                       random: &mut DecorationRandom,
                       x0: i32,
                       y0: i32,
                       z0: i32,
                       x1: i32,
                       y1: i32,
                       z1: i32| {
            this.generate_box_with(level, bb, x0, y0, z0, x1, y1, z1, false, random, &mut stone);
        };
        sel(self, level, random, 0, -4, 0, width - 1, 0, depth - 1);
        sel(self, level, random, 2, 1, 2, 9, 2, 2);
        sel(self, level, random, 2, 1, 12, 9, 2, 12);
        sel(self, level, random, 2, 1, 3, 2, 2, 11);
        sel(self, level, random, 9, 1, 3, 9, 2, 11);
        sel(self, level, random, 1, 3, 1, 10, 6, 1);
        sel(self, level, random, 1, 3, 13, 10, 6, 13);
        sel(self, level, random, 1, 3, 2, 1, 6, 12);
        sel(self, level, random, 10, 3, 2, 10, 6, 12);
        sel(self, level, random, 2, 3, 2, 9, 3, 12);
        sel(self, level, random, 2, 6, 2, 9, 6, 12);
        sel(self, level, random, 3, 7, 3, 8, 7, 11);
        sel(self, level, random, 4, 8, 4, 7, 8, 10);
        self.generate_air_box(level, bb, 3, 1, 3, 8, 2, 11);
        self.generate_air_box(level, bb, 4, 3, 6, 7, 3, 9);
        self.generate_air_box(level, bb, 2, 4, 2, 9, 5, 12);
        self.generate_air_box(level, bb, 4, 6, 5, 7, 6, 9);
        self.generate_air_box(level, bb, 5, 7, 6, 6, 7, 8);
        self.generate_air_box(level, bb, 5, 1, 2, 6, 2, 2);
        self.generate_air_box(level, bb, 5, 2, 12, 6, 2, 12);
        self.generate_air_box(level, bb, 5, 5, 1, 6, 5, 1);
        self.generate_air_box(level, bb, 5, 5, 13, 6, 5, 13);
        let air = BlockState::AIR;
        self.place_block(level, air, 1, 5, 5, bb);
        self.place_block(level, air, 10, 5, 5, bb);
        self.place_block(level, air, 1, 5, 9, bb);
        self.place_block(level, air, 10, 5, 9, bb);
        for z in [0, 14] {
            sel(self, level, random, 2, 4, z, 2, 5, z);
            sel(self, level, random, 4, 4, z, 4, 5, z);
            sel(self, level, random, 7, 4, z, 7, 5, z);
            sel(self, level, random, 9, 4, z, 9, 5, z);
        }
        sel(self, level, random, 5, 6, 0, 6, 6, 0);
        for x in [0, 11] {
            for z in (2..=12).step_by(2) {
                sel(self, level, random, x, 4, z, x, 5, z);
            }
            sel(self, level, random, x, 6, 5, x, 6, 5);
            sel(self, level, random, x, 6, 9, x, 6, 9);
        }
        sel(self, level, random, 2, 7, 2, 2, 9, 2);
        sel(self, level, random, 9, 7, 2, 9, 9, 2);
        sel(self, level, random, 2, 7, 12, 2, 9, 12);
        sel(self, level, random, 9, 7, 12, 9, 9, 12);
        sel(self, level, random, 4, 9, 4, 4, 9, 4);
        sel(self, level, random, 7, 9, 4, 7, 9, 4);
        sel(self, level, random, 4, 9, 10, 4, 9, 10);
        sel(self, level, random, 7, 9, 10, 7, 9, 10);
        sel(self, level, random, 5, 9, 7, 6, 9, 7);
        let stairs = ids::COBBLESTONE_STAIRS.default_state();
        let east = with_facing(stairs, Direction::East);
        let west = with_facing(stairs, Direction::West);
        let south = with_facing(stairs, Direction::South);
        let north = with_facing(stairs, Direction::North);
        self.place_block(level, north, 5, 9, 6, bb);
        self.place_block(level, north, 6, 9, 6, bb);
        self.place_block(level, south, 5, 9, 8, bb);
        self.place_block(level, south, 6, 9, 8, bb);
        self.place_block(level, north, 4, 0, 0, bb);
        self.place_block(level, north, 5, 0, 0, bb);
        self.place_block(level, north, 6, 0, 0, bb);
        self.place_block(level, north, 7, 0, 0, bb);
        self.place_block(level, north, 4, 1, 8, bb);
        self.place_block(level, north, 4, 2, 9, bb);
        self.place_block(level, north, 4, 3, 10, bb);
        self.place_block(level, north, 7, 1, 8, bb);
        self.place_block(level, north, 7, 2, 9, bb);
        self.place_block(level, north, 7, 3, 10, bb);
        sel(self, level, random, 4, 1, 9, 4, 1, 9);
        sel(self, level, random, 7, 1, 9, 7, 1, 9);
        sel(self, level, random, 4, 1, 10, 7, 2, 10);
        sel(self, level, random, 5, 4, 5, 6, 4, 5);
        self.place_block(level, east, 4, 4, 5, bb);
        self.place_block(level, west, 7, 4, 5, bb);
        for i in 0..4 {
            self.place_block(level, south, 5, -i, 6 + i, bb);
            self.place_block(level, south, 6, -i, 6 + i, bb);
            self.generate_air_box(level, bb, 5, -i, 7 + i, 6, -i, 9 + i);
        }
        self.generate_air_box(level, bb, 1, -3, 12, 10, -1, 13);
        self.generate_air_box(level, bb, 1, -3, 1, 3, -1, 13);
        self.generate_air_box(level, bb, 1, -3, 1, 9, -1, 5);
        for z in (1..=13).step_by(2) {
            sel(self, level, random, 1, -3, z, 1, -2, z);
        }
        for z in (2..=12).step_by(2) {
            sel(self, level, random, 1, -1, z, 3, -1, z);
        }
        sel(self, level, random, 2, -2, 1, 5, -2, 1);
        sel(self, level, random, 7, -2, 1, 9, -2, 1);
        sel(self, level, random, 6, -3, 1, 6, -3, 1);
        sel(self, level, random, 6, -1, 1, 6, -1, 1);
        let hook = ids::TRIPWIRE_HOOK.default_state();
        let tripwire = ids::TRIPWIRE.default_state();
        let wire = ids::REDSTONE_WIRE.default_state();
        let mossy = ids::MOSSY_COBBLESTONE.default_state();
        self.place_block(
            level,
            set(hook, &[("facing", "east"), ("attached", "true")]),
            1,
            -3,
            8,
            bb,
        );
        self.place_block(
            level,
            set(hook, &[("facing", "west"), ("attached", "true")]),
            4,
            -3,
            8,
            bb,
        );
        let tripwire_ew = set(
            tripwire,
            &[("east", "true"), ("west", "true"), ("attached", "true")],
        );
        self.place_block(level, tripwire_ew, 2, -3, 8, bb);
        self.place_block(level, tripwire_ew, 3, -3, 8, bb);
        let wire_ns = set(wire, &[("north", "side"), ("south", "side")]);
        for z in (2..=7).rev() {
            self.place_block(level, wire_ns, 5, -3, z, bb);
        }
        self.place_block(
            level,
            set(wire, &[("north", "side"), ("west", "side")]),
            5,
            -3,
            1,
            bb,
        );
        self.place_block(
            level,
            set(wire, &[("east", "side"), ("west", "side")]),
            4,
            -3,
            1,
            bb,
        );
        self.place_block(level, mossy, 3, -3, 1, bb);
        if !self.placed_trap1 {
            self.placed_trap1 = self.create_dispenser(
                level,
                bb,
                random,
                3,
                -2,
                1,
                Direction::North,
                DISPENSER_LOOT,
            );
        }
        let vine = ids::VINE.default_state();
        self.place_block(level, set(vine, &[("south", "true")]), 3, -2, 2, bb);
        self.place_block(
            level,
            set(hook, &[("facing", "north"), ("attached", "true")]),
            7,
            -3,
            1,
            bb,
        );
        self.place_block(
            level,
            set(hook, &[("facing", "south"), ("attached", "true")]),
            7,
            -3,
            5,
            bb,
        );
        let tripwire_ns = set(
            tripwire,
            &[("north", "true"), ("south", "true"), ("attached", "true")],
        );
        self.place_block(level, tripwire_ns, 7, -3, 2, bb);
        self.place_block(level, tripwire_ns, 7, -3, 3, bb);
        self.place_block(level, tripwire_ns, 7, -3, 4, bb);
        self.place_block(
            level,
            set(wire, &[("east", "side"), ("west", "side")]),
            8,
            -3,
            6,
            bb,
        );
        self.place_block(
            level,
            set(wire, &[("west", "side"), ("south", "side")]),
            9,
            -3,
            6,
            bb,
        );
        self.place_block(
            level,
            set(wire, &[("north", "side"), ("south", "up")]),
            9,
            -3,
            5,
            bb,
        );
        self.place_block(level, mossy, 9, -3, 4, bb);
        self.place_block(level, wire_ns, 9, -2, 4, bb);
        if !self.placed_trap2 {
            self.placed_trap2 =
                self.create_dispenser(level, bb, random, 9, -2, 3, Direction::West, DISPENSER_LOOT);
        }
        self.place_block(level, set(vine, &[("east", "true")]), 8, -1, 3, bb);
        self.place_block(level, set(vine, &[("east", "true")]), 8, -2, 3, bb);
        if !self.placed_main_chest {
            self.placed_main_chest = self.create_chest(level, bb, random, 8, -3, 3, CHEST_LOOT);
        }
        self.place_block(level, mossy, 9, -3, 2, bb);
        self.place_block(level, mossy, 8, -3, 1, bb);
        self.place_block(level, mossy, 4, -3, 5, bb);
        self.place_block(level, mossy, 5, -2, 5, bb);
        self.place_block(level, mossy, 5, -1, 5, bb);
        self.place_block(level, mossy, 6, -3, 5, bb);
        self.place_block(level, mossy, 7, -2, 5, bb);
        self.place_block(level, mossy, 7, -1, 5, bb);
        self.place_block(level, mossy, 8, -3, 5, bb);
        sel(self, level, random, 9, -1, 1, 9, -1, 5);
        self.generate_air_box(level, bb, 8, -3, 8, 10, -1, 10);
        let chiseled = ids::CHISELED_STONE_BRICKS.default_state();
        self.place_block(level, chiseled, 8, -2, 11, bb);
        self.place_block(level, chiseled, 9, -2, 11, bb);
        self.place_block(level, chiseled, 10, -2, 11, bb);
        let lever = set(
            ids::LEVER.default_state(),
            &[("facing", "north"), ("face", "wall")],
        );
        self.place_block(level, lever, 8, -2, 12, bb);
        self.place_block(level, lever, 9, -2, 12, bb);
        self.place_block(level, lever, 10, -2, 12, bb);
        sel(self, level, random, 8, -3, 8, 8, -3, 10);
        sel(self, level, random, 10, -3, 8, 10, -3, 10);
        self.place_block(level, mossy, 10, -2, 9, bb);
        self.place_block(level, wire_ns, 8, -2, 9, bb);
        self.place_block(level, wire_ns, 8, -2, 10, bb);
        let wire_all = set(
            wire,
            &[
                ("north", "side"),
                ("south", "side"),
                ("east", "side"),
                ("west", "side"),
            ],
        );
        self.place_block(level, wire_all, 10, -1, 9, bb);
        let piston = ids::STICKY_PISTON.default_state();
        self.place_block(level, with_facing(piston, Direction::Up), 9, -2, 8, bb);
        self.place_block(level, with_facing(piston, Direction::West), 10, -2, 8, bb);
        self.place_block(level, with_facing(piston, Direction::West), 10, -1, 8, bb);
        self.place_block(
            level,
            with_facing(ids::REPEATER.default_state(), Direction::North),
            10,
            -2,
            10,
            bb,
        );
        if !self.placed_hidden_chest {
            self.placed_hidden_chest = self.create_chest(level, bb, random, 9, -3, 10, CHEST_LOOT);
        }
    }
}

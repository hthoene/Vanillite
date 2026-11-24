//! Nether fortresses (`NetherFortressStructure`, `NetherFortressPieces`).
//!
//! The start (a bridge crossing) grows bridges and castle corridors from two weighted tables
//! (`BRIDGE_PIECE_WEIGHTS`, `CASTLE_PIECE_WEIGHTS`) whose entries run out; pending pieces get
//! their children in random order, then the whole fortress is moved to a random height between
//! 48 and 70.

use serde_json::Value;
use vanillite_base::random::{DecorationRandom, LegacyRandom, RandomSource};
use vanillite_base::{BlockPos, ChunkPos, Direction};
use vanillite_data::block::{BlockState, ids};
use vanillite_nbt::{Compound, Tag};

use super::legacy::{PieceWeight, place_spawner, with_props};
use crate::Error;
use crate::feature::region::WorldGenRegion;
use crate::structure::piece::{
    PieceContext, make_bounding_box, random_horizontal_direction, with_facing,
};
use crate::structure::{
    GenerationContext, GenerationStub, PieceBase, StructureParser, StructurePiece,
    StructurePiecesBuilder, StructureType,
};
use crate::template::BoundingBox;

/// `MAX_DEPTH`.
const MAX_DEPTH: i32 = 30;
/// `BuiltInLootTables.NETHER_BRIDGE`.
const LOOT: &str = "minecraft:chests/nether_bridge";

/// `NetherFortressStructure`.
#[derive(Debug)]
pub struct NetherFortressStructure;

impl StructureType for NetherFortressStructure {
    fn parse(_json: &Value, _parser: &mut StructureParser<'_>) -> Result<Self, Error> {
        Ok(Self)
    }

    fn find_generation_point<'s>(
        &'s self,
        ctx: &mut GenerationContext<'_>,
    ) -> Option<GenerationStub<'s>> {
        let chunk = ctx.chunk_pos;
        let position = BlockPos::new(chunk.min_block_x(), 64, chunk.min_block_z());
        Some(GenerationStub::lazy(position, generate_pieces))
    }
}

/// `NetherFortressStructure.generatePieces(builder, context)`.
fn generate_pieces(builder: &mut StructurePiecesBuilder, ctx: &mut GenerationContext<'_>) {
    let chunk = ctx.chunk_pos;
    let random = &mut ctx.random.random;
    let start = FortressPiece::start(random, chunk.min_block_x() + 2, chunk.min_block_z() + 2);
    let start_bb = start.base.bounding_box;
    builder.add_piece(Box::new(start.clone()));
    let mut generator = Generator {
        builder: &mut *builder,
        random: &mut *random,
        bridge_pieces: BRIDGE_PIECE_WEIGHTS.to_vec(),
        castle_pieces: CASTLE_PIECE_WEIGHTS.to_vec(),
        previous_piece: None,
        pending: Vec::new(),
        start_min_x: start_bb.min_x(),
        start_min_z: start_bb.min_z(),
    };
    generator.add_children(&start);
    while !generator.pending.is_empty() {
        let index = generator
            .random
            .next_int_bounded(generator.pending.len() as i32) as usize;
        let piece = generator.pending.remove(index);
        generator.add_children(&piece);
    }
    builder.move_inside_heights(random, 48, 70);
}

/// The piece classes of the weight tables (`NetherFortressPieces.PieceWeight.pieceClass`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PieceClass {
    BridgeStraight,
    BridgeCrossing,
    RoomCrossing,
    StairsRoom,
    MonsterThrone,
    CastleEntrance,
    CastleSmallCorridor,
    CastleSmallCorridorCrossing,
    CastleSmallCorridorRightTurn,
    CastleSmallCorridorLeftTurn,
    CastleCorridorStairs,
    CastleCorridorTBalcony,
    CastleStalkRoom,
}

/// `BRIDGE_PIECE_WEIGHTS`.
const BRIDGE_PIECE_WEIGHTS: [PieceWeight<PieceClass>; 6] = [
    PieceWeight::new(PieceClass::BridgeStraight, 30, 0).in_row(),
    PieceWeight::new(PieceClass::BridgeCrossing, 10, 4),
    PieceWeight::new(PieceClass::RoomCrossing, 10, 4),
    PieceWeight::new(PieceClass::StairsRoom, 10, 3),
    PieceWeight::new(PieceClass::MonsterThrone, 5, 2),
    PieceWeight::new(PieceClass::CastleEntrance, 5, 1),
];

/// `CASTLE_PIECE_WEIGHTS`.
const CASTLE_PIECE_WEIGHTS: [PieceWeight<PieceClass>; 7] = [
    PieceWeight::new(PieceClass::CastleSmallCorridor, 25, 0).in_row(),
    PieceWeight::new(PieceClass::CastleSmallCorridorCrossing, 15, 5),
    PieceWeight::new(PieceClass::CastleSmallCorridorRightTurn, 5, 10),
    PieceWeight::new(PieceClass::CastleSmallCorridorLeftTurn, 5, 10),
    PieceWeight::new(PieceClass::CastleCorridorStairs, 10, 3).in_row(),
    PieceWeight::new(PieceClass::CastleCorridorTBalcony, 7, 2),
    PieceWeight::new(PieceClass::CastleStalkRoom, 5, 2),
];

/// The generation state: the start piece's `previousPiece`, `availableBridgePieces`,
/// `availableCastlePieces` and `pendingChildren`.
struct Generator<'g> {
    builder: &'g mut StructurePiecesBuilder,
    random: &'g mut LegacyRandom,
    bridge_pieces: Vec<PieceWeight<PieceClass>>,
    castle_pieces: Vec<PieceWeight<PieceClass>>,
    previous_piece: Option<PieceClass>,
    pending: Vec<FortressPiece>,
    start_min_x: i32,
    start_min_z: i32,
}

impl Generator<'_> {
    /// `NetherBridgePiece.generatePiece(startPiece, currentPieces, builder, random, footX,
    /// footY, footZ, direction, depth)`.
    fn generate_piece(
        &mut self,
        castle: bool,
        foot: BlockPos,
        direction: Direction,
        depth: i32,
    ) -> Option<FortressPiece> {
        let pieces = if castle {
            &mut self.castle_pieces
        } else {
            &mut self.bridge_pieces
        };
        // `updatePieceWeight(currentPieces)`.
        let has_any_pieces = pieces
            .iter()
            .any(|p| p.max_place_count > 0 && p.place_count < p.max_place_count);
        let total_weight = if has_any_pieces {
            pieces.iter().map(|p| p.weight).sum()
        } else {
            -1
        };
        if total_weight > 0 && depth <= MAX_DEPTH {
            for _ in 0..5 {
                let mut selection = self.random.next_int_bounded(total_weight);
                for index in 0..pieces.len() {
                    let weight = pieces[index];
                    selection -= weight.weight;
                    if selection < 0 {
                        if !weight.do_place(depth)
                            || (Some(weight.kind) == self.previous_piece && !weight.allow_in_row)
                        {
                            break;
                        }
                        let piece = create_piece(
                            weight.kind,
                            self.builder,
                            self.random,
                            foot,
                            direction,
                            depth,
                        );
                        if let Some(piece) = piece {
                            pieces[index].place_count += 1;
                            self.previous_piece = Some(weight.kind);
                            if !pieces[index].is_valid() {
                                pieces.remove(index);
                            }
                            return Some(piece);
                        }
                    }
                }
            }
        }
        create_end_filler(self.builder, self.random, foot, direction, depth)
    }

    /// `NetherBridgePiece.generateAndAddPiece(startPiece, builder, random, footX, footY, footZ,
    /// direction, depth, isCastle)`.
    #[allow(clippy::too_many_arguments)]
    fn generate_and_add_piece(
        &mut self,
        x: i32,
        y: i32,
        z: i32,
        direction: Direction,
        depth: i32,
        castle: bool,
    ) {
        let foot = BlockPos::new(x, y, z);
        if (x - self.start_min_x).abs() > 112 || (z - self.start_min_z).abs() > 112 {
            // Vanilla creates an end filler here without adding it; only its random draw
            // remains.
            create_end_filler(self.builder, self.random, foot, direction, depth);
            return;
        }
        if let Some(piece) = self.generate_piece(castle, foot, direction, depth + 1) {
            self.builder.add_piece(Box::new(piece.clone()));
            self.pending.push(piece);
        }
    }

    /// `generateChildForward(startPiece, builder, random, xOff, yOff, isCastle)`.
    fn child_forward(&mut self, piece: &FortressPiece, x_off: i32, y_off: i32, castle: bool) {
        let bb = piece.base.bounding_box;
        let depth = piece.base.gen_depth;
        let Some(orientation) = piece.base.orientation() else {
            return;
        };
        let (x, z) = match orientation {
            Direction::North => (bb.min_x() + x_off, bb.min_z() - 1),
            Direction::South => (bb.min_x() + x_off, bb.max_z() + 1),
            Direction::West => (bb.min_x() - 1, bb.min_z() + x_off),
            Direction::East => (bb.max_x() + 1, bb.min_z() + x_off),
            _ => return,
        };
        self.generate_and_add_piece(x, bb.min_y() + y_off, z, orientation, depth, castle);
    }

    /// `generateChildLeft(startPiece, builder, random, yOff, zOff, isCastle)`.
    fn child_left(&mut self, piece: &FortressPiece, y_off: i32, z_off: i32, castle: bool) {
        let bb = piece.base.bounding_box;
        let depth = piece.base.gen_depth;
        let (x, z, direction) = match piece.base.orientation() {
            Some(Direction::North | Direction::South) => {
                (bb.min_x() - 1, bb.min_z() + z_off, Direction::West)
            }
            Some(Direction::West | Direction::East) => {
                (bb.min_x() + z_off, bb.min_z() - 1, Direction::North)
            }
            _ => return,
        };
        self.generate_and_add_piece(x, bb.min_y() + y_off, z, direction, depth, castle);
    }

    /// `generateChildRight(startPiece, builder, random, yOff, zOff, isCastle)`.
    fn child_right(&mut self, piece: &FortressPiece, y_off: i32, z_off: i32, castle: bool) {
        let bb = piece.base.bounding_box;
        let depth = piece.base.gen_depth;
        let (x, z, direction) = match piece.base.orientation() {
            Some(Direction::North | Direction::South) => {
                (bb.max_x() + 1, bb.min_z() + z_off, Direction::East)
            }
            Some(Direction::West | Direction::East) => {
                (bb.min_x() + z_off, bb.max_z() + 1, Direction::South)
            }
            _ => return,
        };
        self.generate_and_add_piece(x, bb.min_y() + y_off, z, direction, depth, castle);
    }

    /// `addChildren(startPiece, builder, random)` of each piece class.
    fn add_children(&mut self, piece: &FortressPiece) {
        match piece.kind {
            FortressKind::BridgeCrossing => {
                self.child_forward(piece, 8, 3, false);
                self.child_left(piece, 3, 8, false);
                self.child_right(piece, 3, 8, false);
            }
            FortressKind::BridgeEndFiller { .. } | FortressKind::MonsterThrone { .. } => {}
            FortressKind::BridgeStraight => self.child_forward(piece, 1, 3, false),
            FortressKind::CastleCorridorStairs | FortressKind::CastleSmallCorridor => {
                self.child_forward(piece, 1, 0, true);
            }
            FortressKind::CastleCorridorTBalcony => {
                let z_off = match piece.base.orientation() {
                    Some(Direction::West | Direction::North) => 5,
                    _ => 1,
                };
                let castle = self.random.next_int_bounded(8) > 0;
                self.child_left(piece, 0, z_off, castle);
                let castle = self.random.next_int_bounded(8) > 0;
                self.child_right(piece, 0, z_off, castle);
            }
            FortressKind::CastleEntrance => self.child_forward(piece, 5, 3, true),
            FortressKind::CastleSmallCorridorCrossing => {
                self.child_forward(piece, 1, 0, true);
                self.child_left(piece, 0, 1, true);
                self.child_right(piece, 0, 1, true);
            }
            FortressKind::CastleSmallCorridorLeftTurn { .. } => {
                self.child_left(piece, 0, 1, true);
            }
            FortressKind::CastleSmallCorridorRightTurn { .. } => {
                self.child_right(piece, 0, 1, true);
            }
            FortressKind::CastleStalkRoom => {
                self.child_forward(piece, 5, 3, true);
                self.child_forward(piece, 5, 11, true);
            }
            FortressKind::RoomCrossing => {
                self.child_forward(piece, 2, 0, false);
                self.child_left(piece, 0, 2, false);
                self.child_right(piece, 0, 2, false);
            }
            FortressKind::StairsRoom => self.child_right(piece, 6, 2, false),
        }
    }
}

/// `isOkBox(box)`: above y 10.
fn is_ok_box(bb: &BoundingBox) -> bool {
    bb.min_y() > 10
}

/// `findAndCreateBridgePieceFactory(piece, builder, random, footX, footY, footZ, direction,
/// depth)`: each class's `createPiece`.
fn create_piece(
    class: PieceClass,
    builder: &StructurePiecesBuilder,
    random: &mut LegacyRandom,
    foot: BlockPos,
    direction: Direction,
    depth: i32,
) -> Option<FortressPiece> {
    let (off_x, off_y, width, height, length, kind) = match class {
        PieceClass::BridgeStraight => (-1, -3, 5, 10, 19, FortressKind::BridgeStraight),
        PieceClass::BridgeCrossing => (-8, -3, 19, 10, 19, FortressKind::BridgeCrossing),
        PieceClass::RoomCrossing => (-2, 0, 7, 9, 7, FortressKind::RoomCrossing),
        PieceClass::StairsRoom => (-2, 0, 7, 11, 7, FortressKind::StairsRoom),
        PieceClass::MonsterThrone => (
            -2,
            0,
            7,
            8,
            9,
            FortressKind::MonsterThrone {
                has_placed_spawner: false,
            },
        ),
        PieceClass::CastleEntrance => (-5, -3, 13, 14, 13, FortressKind::CastleEntrance),
        PieceClass::CastleSmallCorridor => (-1, 0, 5, 7, 5, FortressKind::CastleSmallCorridor),
        PieceClass::CastleSmallCorridorCrossing => {
            (-1, 0, 5, 7, 5, FortressKind::CastleSmallCorridorCrossing)
        }
        PieceClass::CastleSmallCorridorRightTurn => (
            -1,
            0,
            5,
            7,
            5,
            FortressKind::CastleSmallCorridorRightTurn {
                is_needing_chest: false,
            },
        ),
        PieceClass::CastleSmallCorridorLeftTurn => (
            -1,
            0,
            5,
            7,
            5,
            FortressKind::CastleSmallCorridorLeftTurn {
                is_needing_chest: false,
            },
        ),
        PieceClass::CastleCorridorStairs => (-1, -7, 5, 14, 10, FortressKind::CastleCorridorStairs),
        PieceClass::CastleCorridorTBalcony => {
            (-3, 0, 9, 7, 9, FortressKind::CastleCorridorTBalcony)
        }
        PieceClass::CastleStalkRoom => (-5, -3, 13, 14, 13, FortressKind::CastleStalkRoom),
    };
    let bb = BoundingBox::orient_box(foot, off_x, off_y, 0, width, height, length, direction);
    if !is_ok_box(&bb) || builder.find_collision_piece(&bb).is_some() {
        return None;
    }
    let kind = match kind {
        FortressKind::CastleSmallCorridorRightTurn { .. } => {
            FortressKind::CastleSmallCorridorRightTurn {
                is_needing_chest: random.next_int_bounded(3) == 0,
            }
        }
        FortressKind::CastleSmallCorridorLeftTurn { .. } => {
            FortressKind::CastleSmallCorridorLeftTurn {
                is_needing_chest: random.next_int_bounded(3) == 0,
            }
        }
        other => other,
    };
    Some(FortressPiece::new(kind, depth, bb, direction))
}

/// `BridgeEndFiller.createPiece(builder, random, footX, footY, footZ, direction, genDepth)`.
fn create_end_filler(
    builder: &StructurePiecesBuilder,
    random: &mut LegacyRandom,
    foot: BlockPos,
    direction: Direction,
    depth: i32,
) -> Option<FortressPiece> {
    let bb = BoundingBox::orient_box(foot, -1, -3, 0, 5, 10, 8, direction);
    if !is_ok_box(&bb) || builder.find_collision_piece(&bb).is_some() {
        return None;
    }
    let self_seed = random.next_int();
    Some(FortressPiece::new(
        FortressKind::BridgeEndFiller { self_seed },
        depth,
        bb,
        direction,
    ))
}

/// The concrete piece class and its own state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FortressKind {
    /// `BridgeCrossing` (`minecraft:nebcr`), also the `StartPiece`.
    BridgeCrossing,
    /// `BridgeEndFiller` (`minecraft:nebef`): a broken bridge end.
    BridgeEndFiller { self_seed: i32 },
    /// `BridgeStraight` (`minecraft:nebs`).
    BridgeStraight,
    /// `CastleCorridorStairsPiece` (`minecraft:neccs`).
    CastleCorridorStairs,
    /// `CastleCorridorTBalconyPiece` (`minecraft:nectb`).
    CastleCorridorTBalcony,
    /// `CastleEntrance` (`minecraft:nece`).
    CastleEntrance,
    /// `CastleSmallCorridorCrossingPiece` (`minecraft:nescsc`).
    CastleSmallCorridorCrossing,
    /// `CastleSmallCorridorLeftTurnPiece` (`minecraft:nesclt`).
    CastleSmallCorridorLeftTurn { is_needing_chest: bool },
    /// `CastleSmallCorridorPiece` (`minecraft:nesc`).
    CastleSmallCorridor,
    /// `CastleSmallCorridorRightTurnPiece` (`minecraft:nescrt`).
    CastleSmallCorridorRightTurn { is_needing_chest: bool },
    /// `CastleStalkRoom` (`minecraft:necsr`): nether wart.
    CastleStalkRoom,
    /// `MonsterThrone` (`minecraft:nemt`): the blaze spawner.
    MonsterThrone { has_placed_spawner: bool },
    /// `RoomCrossing` (`minecraft:nerc`).
    RoomCrossing,
    /// `StairsRoom` (`minecraft:nesr`).
    StairsRoom,
}

impl FortressKind {
    /// The `StructurePieceType` id.
    fn piece_type(self) -> &'static str {
        match self {
            Self::BridgeCrossing => "minecraft:nebcr",
            Self::BridgeEndFiller { .. } => "minecraft:nebef",
            Self::BridgeStraight => "minecraft:nebs",
            Self::CastleCorridorStairs => "minecraft:neccs",
            Self::CastleCorridorTBalcony => "minecraft:nectb",
            Self::CastleEntrance => "minecraft:nece",
            Self::CastleSmallCorridorCrossing => "minecraft:nescsc",
            Self::CastleSmallCorridorLeftTurn { .. } => "minecraft:nesclt",
            Self::CastleSmallCorridor => "minecraft:nesc",
            Self::CastleSmallCorridorRightTurn { .. } => "minecraft:nescrt",
            Self::CastleStalkRoom => "minecraft:necsr",
            Self::MonsterThrone { .. } => "minecraft:nemt",
            Self::RoomCrossing => "minecraft:nerc",
            Self::StairsRoom => "minecraft:nesr",
        }
    }
}

/// A fortress piece (`NetherFortressPieces.NetherBridgePiece` and its subclasses).
#[derive(Clone, Debug)]
pub struct FortressPiece {
    pub base: PieceBase,
    pub kind: FortressKind,
}

impl FortressPiece {
    fn new(kind: FortressKind, gen_depth: i32, bb: BoundingBox, direction: Direction) -> Self {
        let mut base = PieceBase::new(kind.piece_type(), gen_depth, bb);
        base.set_orientation(Some(direction));
        Self { base, kind }
    }

    /// `new StartPiece(random, west, north)`: a bridge crossing (saved as one).
    fn start(random: &mut LegacyRandom, west: i32, north: i32) -> Self {
        let direction = random_horizontal_direction(random);
        let bb = make_bounding_box(west, 64, north, direction, 19, 10, 19);
        Self::new(FortressKind::BridgeCrossing, 0, bb, direction)
    }

    /// `generateBox` with one block for edge and fill.
    #[allow(clippy::too_many_arguments)]
    fn fill(
        &self,
        level: &mut WorldGenRegion<'_>,
        bb: &BoundingBox,
        x0: i32,
        y0: i32,
        z0: i32,
        x1: i32,
        y1: i32,
        z1: i32,
        state: BlockState,
    ) {
        self.generate_box(level, bb, x0, y0, z0, x1, y1, z1, state, state, false);
    }

    /// `fillColumnDown(level, NETHER_BRICKS, x, -1, z, chunkBB)`.
    fn brick_column(&self, level: &mut WorldGenRegion<'_>, bb: &BoundingBox, x: i32, z: i32) {
        self.fill_column_down(level, bricks(), x, -1, z, bb);
    }

    fn post_process_bridge_crossing(&self, level: &mut WorldGenRegion<'_>, bb: &BoundingBox) {
        let b = bricks();
        let air = BlockState::AIR;
        self.fill(level, bb, 7, 3, 0, 11, 4, 18, b);
        self.fill(level, bb, 0, 3, 7, 18, 4, 11, b);
        self.fill(level, bb, 8, 5, 0, 10, 7, 18, air);
        self.fill(level, bb, 0, 5, 8, 18, 7, 10, air);
        self.fill(level, bb, 7, 5, 0, 7, 5, 7, b);
        self.fill(level, bb, 7, 5, 11, 7, 5, 18, b);
        self.fill(level, bb, 11, 5, 0, 11, 5, 7, b);
        self.fill(level, bb, 11, 5, 11, 11, 5, 18, b);
        self.fill(level, bb, 0, 5, 7, 7, 5, 7, b);
        self.fill(level, bb, 11, 5, 7, 18, 5, 7, b);
        self.fill(level, bb, 0, 5, 11, 7, 5, 11, b);
        self.fill(level, bb, 11, 5, 11, 18, 5, 11, b);
        self.fill(level, bb, 7, 2, 0, 11, 2, 5, b);
        self.fill(level, bb, 7, 2, 13, 11, 2, 18, b);
        self.fill(level, bb, 7, 0, 0, 11, 1, 3, b);
        self.fill(level, bb, 7, 0, 15, 11, 1, 18, b);
        for x in 7..=11 {
            for z in 0..=2 {
                self.brick_column(level, bb, x, z);
                self.brick_column(level, bb, x, 18 - z);
            }
        }
        self.fill(level, bb, 0, 2, 7, 5, 2, 11, b);
        self.fill(level, bb, 13, 2, 7, 18, 2, 11, b);
        self.fill(level, bb, 0, 0, 7, 3, 1, 11, b);
        self.fill(level, bb, 15, 0, 7, 18, 1, 11, b);
        for x in 0..=2 {
            for z in 7..=11 {
                self.brick_column(level, bb, x, z);
                self.brick_column(level, bb, 18 - x, z);
            }
        }
    }

    fn post_process_end_filler(&self, level: &mut WorldGenRegion<'_>, bb: &BoundingBox, seed: i32) {
        let b = bricks();
        let mut random = LegacyRandom::new(i64::from(seed));
        for x in 0..=4 {
            for y in 3..=4 {
                let z = random.next_int_bounded(8);
                self.fill(level, bb, x, y, 0, x, y, z, b);
            }
        }
        let z = random.next_int_bounded(8);
        self.fill(level, bb, 0, 5, 0, 0, 5, z, b);
        let z = random.next_int_bounded(8);
        self.fill(level, bb, 4, 5, 0, 4, 5, z, b);
        for x in 0..=4 {
            let z = random.next_int_bounded(5);
            self.fill(level, bb, x, 2, 0, x, 2, z, b);
        }
        for x in 0..=4 {
            for y in 0..=1 {
                let z = random.next_int_bounded(3);
                self.fill(level, bb, x, y, 0, x, y, z, b);
            }
        }
    }

    fn post_process_bridge_straight(&self, level: &mut WorldGenRegion<'_>, bb: &BoundingBox) {
        let b = bricks();
        self.fill(level, bb, 0, 3, 0, 4, 4, 18, b);
        self.fill(level, bb, 1, 5, 0, 3, 7, 18, BlockState::AIR);
        self.fill(level, bb, 0, 5, 0, 0, 5, 18, b);
        self.fill(level, bb, 4, 5, 0, 4, 5, 18, b);
        self.fill(level, bb, 0, 2, 0, 4, 2, 5, b);
        self.fill(level, bb, 0, 2, 13, 4, 2, 18, b);
        self.fill(level, bb, 0, 0, 0, 4, 1, 3, b);
        self.fill(level, bb, 0, 0, 15, 4, 1, 18, b);
        for x in 0..=4 {
            for z in 0..=2 {
                self.brick_column(level, bb, x, z);
                self.brick_column(level, bb, x, 18 - z);
            }
        }
        let ns = fence(&[("north", "true"), ("south", "true")]);
        let nse = with_props(ns, &[("east", "true")]);
        let nsw = with_props(ns, &[("west", "true")]);
        self.fill(level, bb, 0, 1, 1, 0, 4, 1, nse);
        self.fill(level, bb, 0, 3, 4, 0, 4, 4, nse);
        self.fill(level, bb, 0, 3, 14, 0, 4, 14, nse);
        self.fill(level, bb, 0, 1, 17, 0, 4, 17, nse);
        self.fill(level, bb, 4, 1, 1, 4, 4, 1, nsw);
        self.fill(level, bb, 4, 3, 4, 4, 4, 4, nsw);
        self.fill(level, bb, 4, 3, 14, 4, 4, 14, nsw);
        self.fill(level, bb, 4, 1, 17, 4, 4, 17, nsw);
    }

    fn post_process_corridor_stairs(&self, level: &mut WorldGenRegion<'_>, bb: &BoundingBox) {
        let b = bricks();
        let stairs = with_facing(ids::NETHER_BRICK_STAIRS.default_state(), Direction::South);
        let ns = fence(&[("north", "true"), ("south", "true")]);
        for step in 0..=9 {
            let floor = (7 - step).max(1);
            let roof = (floor + 5).max(14 - step).min(13);
            let z = step;
            self.fill(level, bb, 0, 0, step, 4, floor, step, b);
            self.fill(
                level,
                bb,
                1,
                floor + 1,
                step,
                3,
                roof - 1,
                step,
                BlockState::AIR,
            );
            if step <= 6 {
                self.place_block(level, stairs, 1, floor + 1, step, bb);
                self.place_block(level, stairs, 2, floor + 1, step, bb);
                self.place_block(level, stairs, 3, floor + 1, step, bb);
            }
            self.fill(level, bb, 0, roof, step, 4, roof, step, b);
            self.fill(level, bb, 0, floor + 1, step, 0, roof - 1, step, b);
            self.fill(level, bb, 4, floor + 1, step, 4, roof - 1, step, b);
            if step & 1 == 0 {
                self.fill(level, bb, 0, floor + 2, step, 0, floor + 3, step, ns);
                self.fill(level, bb, 4, floor + 2, step, 4, floor + 3, step, ns);
            }
            for x in 0..=4 {
                self.brick_column(level, bb, x, z);
            }
        }
    }

    fn post_process_t_balcony(&self, level: &mut WorldGenRegion<'_>, bb: &BoundingBox) {
        let b = bricks();
        let air = BlockState::AIR;
        let ns = fence(&[("north", "true"), ("south", "true")]);
        let we = fence(&[("west", "true"), ("east", "true")]);
        self.fill(level, bb, 0, 0, 0, 8, 1, 8, b);
        self.fill(level, bb, 0, 2, 0, 8, 5, 8, air);
        self.fill(level, bb, 0, 6, 0, 8, 6, 5, b);
        self.fill(level, bb, 0, 2, 0, 2, 5, 0, b);
        self.fill(level, bb, 6, 2, 0, 8, 5, 0, b);
        self.fill(level, bb, 1, 3, 0, 1, 4, 0, we);
        self.fill(level, bb, 7, 3, 0, 7, 4, 0, we);
        self.fill(level, bb, 0, 2, 4, 8, 2, 8, b);
        self.fill(level, bb, 1, 1, 4, 2, 2, 4, air);
        self.fill(level, bb, 6, 1, 4, 7, 2, 4, air);
        self.fill(level, bb, 1, 3, 8, 7, 3, 8, we);
        self.place_block(
            level,
            fence(&[("east", "true"), ("south", "true")]),
            0,
            3,
            8,
            bb,
        );
        self.place_block(
            level,
            fence(&[("west", "true"), ("south", "true")]),
            8,
            3,
            8,
            bb,
        );
        self.fill(level, bb, 0, 3, 6, 0, 3, 7, ns);
        self.fill(level, bb, 8, 3, 6, 8, 3, 7, ns);
        self.fill(level, bb, 0, 3, 4, 0, 5, 5, b);
        self.fill(level, bb, 8, 3, 4, 8, 5, 5, b);
        self.fill(level, bb, 1, 3, 5, 2, 5, 5, b);
        self.fill(level, bb, 6, 3, 5, 7, 5, 5, b);
        self.fill(level, bb, 1, 4, 5, 1, 5, 5, we);
        self.fill(level, bb, 7, 4, 5, 7, 5, 5, we);
        for z in 0..=5 {
            for x in 0..=8 {
                self.brick_column(level, bb, x, z);
            }
        }
    }

    /// The walls, fences and roof shared by the castle entrance and the stalk room.
    fn castle_hall_walls(
        &self,
        level: &mut WorldGenRegion<'_>,
        bb: &BoundingBox,
        side_fences: (BlockState, BlockState),
    ) {
        let b = bricks();
        let air = BlockState::AIR;
        self.fill(level, bb, 0, 3, 0, 12, 4, 12, b);
        self.fill(level, bb, 0, 5, 0, 12, 13, 12, air);
        self.fill(level, bb, 0, 5, 0, 1, 12, 12, b);
        self.fill(level, bb, 11, 5, 0, 12, 12, 12, b);
        self.fill(level, bb, 2, 5, 11, 4, 12, 12, b);
        self.fill(level, bb, 8, 5, 11, 10, 12, 12, b);
        self.fill(level, bb, 5, 9, 11, 7, 12, 12, b);
        self.fill(level, bb, 2, 5, 0, 4, 12, 1, b);
        self.fill(level, bb, 8, 5, 0, 10, 12, 1, b);
        self.fill(level, bb, 5, 9, 0, 7, 12, 1, b);
        self.fill(level, bb, 2, 11, 2, 10, 12, 10, b);
        let we = fence(&[("west", "true"), ("east", "true")]);
        let ns = fence(&[("north", "true"), ("south", "true")]);
        if self.kind == FortressKind::CastleEntrance {
            let plain = ids::NETHER_BRICK_FENCE.default_state();
            self.fill(level, bb, 5, 8, 0, 7, 8, 0, plain);
        }
        for i in (1..=11).step_by(2) {
            self.fill(level, bb, i, 10, 0, i, 11, 0, we);
            self.fill(level, bb, i, 10, 12, i, 11, 12, we);
            self.fill(level, bb, 0, 10, i, 0, 11, i, ns);
            self.fill(level, bb, 12, 10, i, 12, 11, i, ns);
            self.place_block(level, b, i, 13, 0, bb);
            self.place_block(level, b, i, 13, 12, bb);
            self.place_block(level, b, 0, 13, i, bb);
            self.place_block(level, b, 12, 13, i, bb);
            if i != 11 {
                self.place_block(level, we, i + 1, 13, 0, bb);
                self.place_block(level, we, i + 1, 13, 12, bb);
                self.place_block(level, ns, 0, 13, i + 1, bb);
                self.place_block(level, ns, 12, 13, i + 1, bb);
            }
        }
        self.place_block(
            level,
            fence(&[("north", "true"), ("east", "true")]),
            0,
            13,
            0,
            bb,
        );
        self.place_block(
            level,
            fence(&[("south", "true"), ("east", "true")]),
            0,
            13,
            12,
            bb,
        );
        self.place_block(
            level,
            fence(&[("south", "true"), ("west", "true")]),
            12,
            13,
            12,
            bb,
        );
        self.place_block(
            level,
            fence(&[("north", "true"), ("west", "true")]),
            12,
            13,
            0,
            bb,
        );
        let (west_side, east_side) = side_fences;
        for z in (3..=9).step_by(2) {
            self.fill(level, bb, 1, 7, z, 1, 8, z, west_side);
            self.fill(level, bb, 11, 7, z, 11, 8, z, east_side);
        }
    }

    /// The floor cross and its supporting columns shared by the castle entrance and the stalk
    /// room.
    fn castle_hall_floor(&self, level: &mut WorldGenRegion<'_>, bb: &BoundingBox) {
        let b = bricks();
        self.fill(level, bb, 4, 2, 0, 8, 2, 12, b);
        self.fill(level, bb, 0, 2, 4, 12, 2, 8, b);
        self.fill(level, bb, 4, 0, 0, 8, 1, 3, b);
        self.fill(level, bb, 4, 0, 9, 8, 1, 12, b);
        self.fill(level, bb, 0, 0, 4, 3, 1, 8, b);
        self.fill(level, bb, 9, 0, 4, 12, 1, 8, b);
        for x in 4..=8 {
            for z in 0..=2 {
                self.brick_column(level, bb, x, z);
                self.brick_column(level, bb, x, 12 - z);
            }
        }
        for x in 0..=2 {
            for z in 4..=8 {
                self.brick_column(level, bb, x, z);
                self.brick_column(level, bb, 12 - x, z);
            }
        }
    }

    fn post_process_castle_entrance(&self, level: &mut WorldGenRegion<'_>, bb: &BoundingBox) {
        let ns = fence(&[("north", "true"), ("south", "true")]);
        let side_fences = (
            with_props(ns, &[("west", "true")]),
            with_props(ns, &[("east", "true")]),
        );
        self.castle_hall_walls(level, bb, side_fences);
        self.castle_hall_floor(level, bb);
        let b = bricks();
        self.fill(level, bb, 5, 5, 5, 7, 5, 7, b);
        self.fill(level, bb, 6, 1, 6, 6, 4, 6, BlockState::AIR);
        self.place_block(level, b, 6, 0, 6, bb);
        self.place_block(level, ids::LAVA.default_state(), 6, 5, 6, bb);
        let pos = self.world_pos(6, 5, 6);
        if bb.is_inside(pos) {
            level.schedule_fluid_tick(pos, crate::feature::kinds::terrain::FLUID_LAVA);
        }
    }

    fn post_process_stalk_room(&self, level: &mut WorldGenRegion<'_>, bb: &BoundingBox) {
        let b = bricks();
        let air = BlockState::AIR;
        let ns = fence(&[("north", "true"), ("south", "true")]);
        let nsw = with_props(ns, &[("west", "true")]);
        let nse = with_props(ns, &[("east", "true")]);
        self.castle_hall_walls(level, bb, (nsw, nse));
        let stairs = with_facing(ids::NETHER_BRICK_STAIRS.default_state(), Direction::North);
        for i in 0..=6 {
            let z = i + 4;
            for x in 5..=7 {
                self.place_block(level, stairs, x, 5 + i, z, bb);
            }
            if (5..=8).contains(&z) {
                self.fill(level, bb, 5, 5, z, 7, i + 4, z, b);
            } else if (9..=10).contains(&z) {
                self.fill(level, bb, 5, 8, z, 7, i + 4, z, b);
            }
            if i >= 1 {
                self.fill(level, bb, 5, 6 + i, z, 7, 9 + i, z, air);
            }
        }
        for x in 5..=7 {
            self.place_block(level, stairs, x, 12, 11, bb);
        }
        self.fill(level, bb, 5, 6, 7, 5, 7, 7, nse);
        self.fill(level, bb, 7, 6, 7, 7, 7, 7, nsw);
        self.fill(level, bb, 5, 13, 12, 7, 13, 12, air);
        self.fill(level, bb, 2, 5, 2, 3, 5, 3, b);
        self.fill(level, bb, 2, 5, 9, 3, 5, 10, b);
        self.fill(level, bb, 2, 5, 4, 2, 5, 8, b);
        self.fill(level, bb, 9, 5, 2, 10, 5, 3, b);
        self.fill(level, bb, 9, 5, 9, 10, 5, 10, b);
        self.fill(level, bb, 10, 5, 4, 10, 5, 8, b);
        let east = with_facing(stairs, Direction::East);
        let west = with_facing(stairs, Direction::West);
        for (state, x, z) in [
            (west, 4, 2),
            (west, 4, 3),
            (west, 4, 9),
            (west, 4, 10),
            (east, 8, 2),
            (east, 8, 3),
            (east, 8, 9),
            (east, 8, 10),
        ] {
            self.place_block(level, state, x, 5, z, bb);
        }
        let soul_sand = ids::SOUL_SAND.default_state();
        let wart = ids::NETHER_WART.default_state();
        self.fill(level, bb, 3, 4, 4, 4, 4, 8, soul_sand);
        self.fill(level, bb, 8, 4, 4, 9, 4, 8, soul_sand);
        self.fill(level, bb, 3, 5, 4, 4, 5, 8, wart);
        self.fill(level, bb, 8, 5, 4, 9, 5, 8, wart);
        self.castle_hall_floor(level, bb);
    }

    /// The floor, air, roof and supporting columns of the small corridors; `walls` runs
    /// between the floor and the roof.
    fn small_corridor(
        &self,
        level: &mut WorldGenRegion<'_>,
        bb: &BoundingBox,
        walls: impl FnOnce(&Self, &mut WorldGenRegion<'_>),
    ) {
        let b = bricks();
        self.fill(level, bb, 0, 0, 0, 4, 1, 4, b);
        self.fill(level, bb, 0, 2, 0, 4, 5, 4, BlockState::AIR);
        walls(self, level);
        self.fill(level, bb, 0, 6, 0, 4, 6, 4, b);
        for x in 0..=4 {
            for z in 0..=4 {
                self.brick_column(level, bb, x, z);
            }
        }
    }

    fn post_process_small_corridor_crossing(
        &self,
        level: &mut WorldGenRegion<'_>,
        bb: &BoundingBox,
    ) {
        let b = bricks();
        self.small_corridor(level, bb, |piece, level| {
            piece.fill(level, bb, 0, 2, 0, 0, 5, 0, b);
            piece.fill(level, bb, 4, 2, 0, 4, 5, 0, b);
            piece.fill(level, bb, 0, 2, 4, 0, 5, 4, b);
            piece.fill(level, bb, 4, 2, 4, 4, 5, 4, b);
        });
    }

    fn post_process_small_corridor(&self, level: &mut WorldGenRegion<'_>, bb: &BoundingBox) {
        let b = bricks();
        let ns = fence(&[("north", "true"), ("south", "true")]);
        self.small_corridor(level, bb, |piece, level| {
            piece.fill(level, bb, 0, 2, 0, 0, 5, 4, b);
            piece.fill(level, bb, 4, 2, 0, 4, 5, 4, b);
            piece.fill(level, bb, 0, 3, 1, 0, 4, 1, ns);
            piece.fill(level, bb, 0, 3, 3, 0, 4, 3, ns);
            piece.fill(level, bb, 4, 3, 1, 4, 4, 1, ns);
            piece.fill(level, bb, 4, 3, 3, 4, 4, 3, ns);
        });
    }

    /// `CastleSmallCorridorLeftTurnPiece.postProcess` (`left`) and the right turn; returns
    /// the new `isNeedingChest`.
    fn post_process_turn(
        &self,
        level: &mut WorldGenRegion<'_>,
        bb: &BoundingBox,
        random: &mut DecorationRandom,
        left: bool,
        mut needs_chest: bool,
    ) -> bool {
        let b = bricks();
        let we = fence(&[("west", "true"), ("east", "true")]);
        let ns = fence(&[("north", "true"), ("south", "true")]);
        self.small_corridor(level, bb, |piece, level| {
            if left {
                piece.fill(level, bb, 4, 2, 0, 4, 5, 4, b);
                piece.fill(level, bb, 4, 3, 1, 4, 4, 1, ns);
                piece.fill(level, bb, 4, 3, 3, 4, 4, 3, ns);
                piece.fill(level, bb, 0, 2, 0, 0, 5, 0, b);
                piece.fill(level, bb, 0, 2, 4, 3, 5, 4, b);
                piece.fill(level, bb, 1, 3, 4, 1, 4, 4, we);
                piece.fill(level, bb, 3, 3, 4, 3, 4, 4, we);
            } else {
                piece.fill(level, bb, 0, 2, 0, 0, 5, 4, b);
                piece.fill(level, bb, 0, 3, 1, 0, 4, 1, ns);
                piece.fill(level, bb, 0, 3, 3, 0, 4, 3, ns);
                piece.fill(level, bb, 4, 2, 0, 4, 5, 0, b);
                piece.fill(level, bb, 1, 2, 4, 4, 5, 4, b);
                piece.fill(level, bb, 1, 3, 4, 1, 4, 4, we);
                piece.fill(level, bb, 3, 3, 4, 3, 4, 4, we);
            }
            let chest_x = if left { 3 } else { 1 };
            if needs_chest && bb.is_inside(piece.world_pos(chest_x, 2, 3)) {
                needs_chest = false;
                piece.create_chest(level, bb, random, chest_x, 2, 3, LOOT);
            }
        });
        needs_chest
    }

    /// `MonsterThrone.postProcess`; returns the new `hasPlacedSpawner`.
    fn post_process_monster_throne(
        &self,
        level: &mut WorldGenRegion<'_>,
        bb: &BoundingBox,
        mut has_placed_spawner: bool,
    ) -> bool {
        let b = bricks();
        self.fill(level, bb, 0, 2, 0, 6, 7, 7, BlockState::AIR);
        self.fill(level, bb, 1, 0, 0, 5, 1, 7, b);
        self.fill(level, bb, 1, 2, 1, 5, 2, 7, b);
        self.fill(level, bb, 1, 3, 2, 5, 3, 7, b);
        self.fill(level, bb, 1, 4, 3, 5, 4, 7, b);
        self.fill(level, bb, 1, 2, 0, 1, 4, 2, b);
        self.fill(level, bb, 5, 2, 0, 5, 4, 2, b);
        self.fill(level, bb, 1, 5, 2, 1, 5, 3, b);
        self.fill(level, bb, 5, 5, 2, 5, 5, 3, b);
        self.fill(level, bb, 0, 5, 3, 0, 5, 8, b);
        self.fill(level, bb, 6, 5, 3, 6, 5, 8, b);
        self.fill(level, bb, 1, 5, 8, 5, 5, 8, b);
        let we = fence(&[("west", "true"), ("east", "true")]);
        let ns = fence(&[("north", "true"), ("south", "true")]);
        let w = fence(&[("west", "true")]);
        let e = fence(&[("east", "true")]);
        self.place_block(level, w, 1, 6, 3, bb);
        self.place_block(level, e, 5, 6, 3, bb);
        self.place_block(
            level,
            fence(&[("east", "true"), ("north", "true")]),
            0,
            6,
            3,
            bb,
        );
        self.place_block(
            level,
            fence(&[("west", "true"), ("north", "true")]),
            6,
            6,
            3,
            bb,
        );
        self.fill(level, bb, 0, 6, 4, 0, 6, 7, ns);
        self.fill(level, bb, 6, 6, 4, 6, 6, 7, ns);
        self.place_block(
            level,
            fence(&[("east", "true"), ("south", "true")]),
            0,
            6,
            8,
            bb,
        );
        self.place_block(
            level,
            fence(&[("west", "true"), ("south", "true")]),
            6,
            6,
            8,
            bb,
        );
        self.fill(level, bb, 1, 6, 8, 5, 6, 8, we);
        self.place_block(level, e, 1, 7, 8, bb);
        self.fill(level, bb, 2, 7, 8, 4, 7, 8, we);
        self.place_block(level, w, 5, 7, 8, bb);
        self.place_block(level, e, 2, 8, 8, bb);
        self.place_block(level, we, 3, 8, 8, bb);
        self.place_block(level, w, 4, 8, 8, bb);
        if !has_placed_spawner {
            let pos = self.world_pos(3, 5, 5);
            if bb.is_inside(pos) {
                has_placed_spawner = true;
                place_spawner(level, pos, "minecraft:blaze");
            }
        }
        for x in 0..=6 {
            for z in 0..=6 {
                self.brick_column(level, bb, x, z);
            }
        }
        has_placed_spawner
    }

    fn post_process_room_crossing(&self, level: &mut WorldGenRegion<'_>, bb: &BoundingBox) {
        let b = bricks();
        self.fill(level, bb, 0, 0, 0, 6, 1, 6, b);
        self.fill(level, bb, 0, 2, 0, 6, 7, 6, BlockState::AIR);
        self.fill(level, bb, 0, 2, 0, 1, 6, 0, b);
        self.fill(level, bb, 0, 2, 6, 1, 6, 6, b);
        self.fill(level, bb, 5, 2, 0, 6, 6, 0, b);
        self.fill(level, bb, 5, 2, 6, 6, 6, 6, b);
        self.fill(level, bb, 0, 2, 0, 0, 6, 1, b);
        self.fill(level, bb, 0, 2, 5, 0, 6, 6, b);
        self.fill(level, bb, 6, 2, 0, 6, 6, 1, b);
        self.fill(level, bb, 6, 2, 5, 6, 6, 6, b);
        let we = fence(&[("west", "true"), ("east", "true")]);
        let ns = fence(&[("north", "true"), ("south", "true")]);
        self.fill(level, bb, 2, 6, 0, 4, 6, 0, b);
        self.fill(level, bb, 2, 5, 0, 4, 5, 0, we);
        self.fill(level, bb, 2, 6, 6, 4, 6, 6, b);
        self.fill(level, bb, 2, 5, 6, 4, 5, 6, we);
        self.fill(level, bb, 0, 6, 2, 0, 6, 4, b);
        self.fill(level, bb, 0, 5, 2, 0, 5, 4, ns);
        self.fill(level, bb, 6, 6, 2, 6, 6, 4, b);
        self.fill(level, bb, 6, 5, 2, 6, 5, 4, ns);
        for x in 0..=6 {
            for z in 0..=6 {
                self.brick_column(level, bb, x, z);
            }
        }
    }

    fn post_process_stairs_room(&self, level: &mut WorldGenRegion<'_>, bb: &BoundingBox) {
        let b = bricks();
        let air = BlockState::AIR;
        self.fill(level, bb, 0, 0, 0, 6, 1, 6, b);
        self.fill(level, bb, 0, 2, 0, 6, 10, 6, air);
        self.fill(level, bb, 0, 2, 0, 1, 8, 0, b);
        self.fill(level, bb, 5, 2, 0, 6, 8, 0, b);
        self.fill(level, bb, 0, 2, 1, 0, 8, 6, b);
        self.fill(level, bb, 6, 2, 1, 6, 8, 6, b);
        self.fill(level, bb, 1, 2, 6, 5, 8, 6, b);
        let we = fence(&[("west", "true"), ("east", "true")]);
        let ns = fence(&[("north", "true"), ("south", "true")]);
        self.fill(level, bb, 0, 3, 2, 0, 5, 4, ns);
        self.fill(level, bb, 6, 3, 2, 6, 5, 2, ns);
        self.fill(level, bb, 6, 3, 4, 6, 5, 4, ns);
        self.place_block(level, b, 5, 2, 5, bb);
        self.fill(level, bb, 4, 2, 5, 4, 3, 5, b);
        self.fill(level, bb, 3, 2, 5, 3, 4, 5, b);
        self.fill(level, bb, 2, 2, 5, 2, 5, 5, b);
        self.fill(level, bb, 1, 2, 5, 1, 6, 5, b);
        self.fill(level, bb, 1, 7, 1, 5, 7, 4, b);
        self.fill(level, bb, 6, 8, 2, 6, 8, 4, air);
        self.fill(level, bb, 2, 6, 0, 4, 8, 0, b);
        self.fill(level, bb, 2, 5, 0, 4, 5, 0, we);
        for x in 0..=6 {
            for z in 0..=6 {
                self.brick_column(level, bb, x, z);
            }
        }
    }
}

/// `Blocks.NETHER_BRICKS.defaultBlockState()`.
fn bricks() -> BlockState {
    ids::NETHER_BRICKS.default_state()
}

/// A nether brick fence with the given sides connected.
fn fence(sides: &[(&str, &str)]) -> BlockState {
    with_props(ids::NETHER_BRICK_FENCE.default_state(), sides)
}

impl StructurePiece for FortressPiece {
    fn base(&self) -> &PieceBase {
        &self.base
    }

    fn base_mut(&mut self) -> &mut PieceBase {
        &mut self.base
    }

    /// `addAdditionalSaveData`.
    fn additional_save_data(&self) -> Option<Compound> {
        let mut tag = Compound::new();
        match self.kind {
            FortressKind::BridgeEndFiller { self_seed } => tag.insert("Seed", Tag::Int(self_seed)),
            FortressKind::CastleSmallCorridorLeftTurn { is_needing_chest }
            | FortressKind::CastleSmallCorridorRightTurn { is_needing_chest } => {
                tag.insert("Chest", Tag::Byte(i8::from(is_needing_chest)));
            }
            FortressKind::MonsterThrone { has_placed_spawner } => {
                tag.insert("Mob", Tag::Byte(i8::from(has_placed_spawner)));
            }
            _ => {}
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
        let bb: &BoundingBox = chunk_bb;
        match self.kind {
            FortressKind::BridgeCrossing => self.post_process_bridge_crossing(level, bb),
            FortressKind::BridgeEndFiller { self_seed } => {
                self.post_process_end_filler(level, bb, self_seed);
            }
            FortressKind::BridgeStraight => self.post_process_bridge_straight(level, bb),
            FortressKind::CastleCorridorStairs => self.post_process_corridor_stairs(level, bb),
            FortressKind::CastleCorridorTBalcony => self.post_process_t_balcony(level, bb),
            FortressKind::CastleEntrance => self.post_process_castle_entrance(level, bb),
            FortressKind::CastleSmallCorridorCrossing => {
                self.post_process_small_corridor_crossing(level, bb);
            }
            FortressKind::CastleSmallCorridor => self.post_process_small_corridor(level, bb),
            FortressKind::CastleSmallCorridorLeftTurn { is_needing_chest } => {
                let needs = self.post_process_turn(level, bb, ctx.random, true, is_needing_chest);
                self.kind = FortressKind::CastleSmallCorridorLeftTurn {
                    is_needing_chest: needs,
                };
            }
            FortressKind::CastleSmallCorridorRightTurn { is_needing_chest } => {
                let needs = self.post_process_turn(level, bb, ctx.random, false, is_needing_chest);
                self.kind = FortressKind::CastleSmallCorridorRightTurn {
                    is_needing_chest: needs,
                };
            }
            FortressKind::CastleStalkRoom => self.post_process_stalk_room(level, bb),
            FortressKind::MonsterThrone { has_placed_spawner } => {
                let placed = self.post_process_monster_throne(level, bb, has_placed_spawner);
                self.kind = FortressKind::MonsterThrone {
                    has_placed_spawner: placed,
                };
            }
            FortressKind::RoomCrossing => self.post_process_room_crossing(level, bb),
            FortressKind::StairsRoom => self.post_process_stairs_room(level, bb),
        }
    }
}

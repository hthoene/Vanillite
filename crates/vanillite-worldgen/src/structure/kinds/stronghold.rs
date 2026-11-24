//! Strongholds (`StrongholdStructure`, `StrongholdPieces`).
//!
//! The start (a spiral staircase) grows corridors and rooms through its small doors, picking
//! each piece from a weighted table (`STRONGHOLD_PIECE_WEIGHTS`) whose entries run out; pending
//! pieces get their children in random order. Generation is retried with the next seed until
//! a portal room was placed. The generation point is the start chunk; which chunks get a start
//! is decided by the concentric rings placement.
//!
//! Not ported: `StartPiece.getLocatorPosition` (the portal room's position, for `/locate`
//! and eyes of ender); it does not affect generation.

use serde_json::Value;
use vanillite_base::random::{DecorationRandom, LegacyRandom, RandomSource};
use vanillite_base::{BlockPos, ChunkPos, Direction};
use vanillite_data::block::{BlockState, ids};
use vanillite_nbt::{Compound, Tag};

use super::legacy::{PieceWeight, place_spawner, with_props};
use crate::Error;
use crate::feature::region::WorldGenRegion;
use crate::structure::piece::{
    BlockSelector, PieceContext, make_bounding_box, random_horizontal_direction, with_facing,
};
use crate::structure::{
    GenerationContext, GenerationStub, PieceBase, StructureParser, StructurePiece,
    StructurePiecesBuilder, StructureType,
};
use crate::template::BoundingBox;

/// `MAX_DEPTH`.
const MAX_DEPTH: i32 = 50;
/// `BuiltInLootTables.STRONGHOLD_CORRIDOR`.
const LOOT_CORRIDOR: &str = "minecraft:chests/stronghold_corridor";
/// `BuiltInLootTables.STRONGHOLD_CROSSING`.
const LOOT_CROSSING: &str = "minecraft:chests/stronghold_crossing";
/// `BuiltInLootTables.STRONGHOLD_LIBRARY`.
const LOOT_LIBRARY: &str = "minecraft:chests/stronghold_library";

/// `StrongholdStructure`.
#[derive(Debug)]
pub struct StrongholdStructure;

impl StructureType for StrongholdStructure {
    fn parse(_json: &Value, _parser: &mut StructureParser<'_>) -> Result<Self, Error> {
        Ok(Self)
    }

    fn find_generation_point<'s>(
        &'s self,
        ctx: &mut GenerationContext<'_>,
    ) -> Option<GenerationStub<'s>> {
        let chunk = ctx.chunk_pos;
        let position = BlockPos::new(chunk.min_block_x(), 0, chunk.min_block_z());
        Some(GenerationStub::lazy(position, generate_pieces))
    }
}

/// `StrongholdStructure.generatePieces(builder, context)`.
fn generate_pieces(builder: &mut StructurePiecesBuilder, ctx: &mut GenerationContext<'_>) {
    let chunk = ctx.chunk_pos;
    let sea_level = ctx.generator.sea_level();
    let min_y = ctx.generator.min_y();
    let mut tries: i64 = 0;
    loop {
        builder.clear();
        ctx.random
            .set_large_feature_seed(ctx.seed.wrapping_add(tries), chunk.x, chunk.z);
        tries += 1;
        let random = &mut ctx.random.random;
        let start =
            StrongholdPiece::start(random, chunk.min_block_x() + 2, chunk.min_block_z() + 2);
        let start_bb = start.base.bounding_box;
        builder.add_piece(Box::new(start.clone()));
        let mut generator = Generator::new(&mut *builder, &mut *random, &start_bb);
        generator.add_children(&start);
        while !generator.pending.is_empty() {
            let index = generator
                .random
                .next_int_bounded(generator.pending.len() as i32) as usize;
            let piece = generator.pending.remove(index);
            generator.add_children(&piece);
        }
        let portal_room = generator.portal_room;
        builder.move_below_sea_level(sea_level, min_y, random, 10);
        if !builder.is_empty() && portal_room {
            break;
        }
    }
}

/// The piece classes of the weight table (`StrongholdPieces.PieceWeight.pieceClass`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PieceClass {
    Straight,
    PrisonHall,
    LeftTurn,
    RightTurn,
    RoomCrossing,
    StraightStairsDown,
    StairsDown,
    FiveCrossing,
    ChestCorridor,
    Library,
    PortalRoom,
}

/// `STRONGHOLD_PIECE_WEIGHTS`.
const PIECE_WEIGHTS: [PieceWeight<PieceClass>; 11] = [
    PieceWeight::new(PieceClass::Straight, 40, 0),
    PieceWeight::new(PieceClass::PrisonHall, 5, 5),
    PieceWeight::new(PieceClass::LeftTurn, 20, 0),
    PieceWeight::new(PieceClass::RightTurn, 20, 0),
    PieceWeight::new(PieceClass::RoomCrossing, 10, 6),
    PieceWeight::new(PieceClass::StraightStairsDown, 5, 5),
    PieceWeight::new(PieceClass::StairsDown, 5, 5),
    PieceWeight::new(PieceClass::FiveCrossing, 5, 4),
    PieceWeight::new(PieceClass::ChestCorridor, 5, 4),
    PieceWeight::new(PieceClass::Library, 10, 2).deeper_than(4),
    PieceWeight::new(PieceClass::PortalRoom, 20, 1).deeper_than(5),
];

/// The generation state of one try: Vanilla's static `currentPieces`, `imposedPiece` and
/// `totalWeight`, and the start piece's `previousPiece`, `portalRoomPiece` and
/// `pendingChildren`.
struct Generator<'g> {
    builder: &'g mut StructurePiecesBuilder,
    random: &'g mut LegacyRandom,
    current_pieces: Vec<PieceWeight<PieceClass>>,
    imposed_piece: Option<PieceClass>,
    total_weight: i32,
    previous_piece: Option<PieceClass>,
    portal_room: bool,
    pending: Vec<StrongholdPiece>,
    start_min_x: i32,
    start_min_z: i32,
}

impl<'g> Generator<'g> {
    /// `resetPieces()` for a new start.
    fn new(
        builder: &'g mut StructurePiecesBuilder,
        random: &'g mut LegacyRandom,
        start_bb: &BoundingBox,
    ) -> Self {
        Self {
            builder,
            random,
            current_pieces: PIECE_WEIGHTS.to_vec(),
            imposed_piece: None,
            total_weight: 0,
            previous_piece: None,
            portal_room: false,
            pending: Vec::new(),
            start_min_x: start_bb.min_x(),
            start_min_z: start_bb.min_z(),
        }
    }

    /// `updatePieceWeight()`.
    fn update_piece_weight(&mut self) -> bool {
        let mut has_any_pieces = false;
        self.total_weight = 0;
        for piece in &self.current_pieces {
            if piece.max_place_count > 0 && piece.place_count < piece.max_place_count {
                has_any_pieces = true;
            }
            self.total_weight += piece.weight;
        }
        has_any_pieces
    }

    /// `generatePieceFromSmallDoor(startPiece, builder, random, footX, footY, footZ,
    /// direction, depth)`.
    fn generate_piece_from_small_door(
        &mut self,
        foot: BlockPos,
        direction: Direction,
        depth: i32,
    ) -> Option<StrongholdPiece> {
        if !self.update_piece_weight() {
            return None;
        }
        if let Some(imposed) = self.imposed_piece.take() {
            let piece = create_piece(imposed, self.builder, self.random, foot, direction, depth);
            if piece.is_some() {
                return piece;
            }
        }
        for _ in 0..5 {
            let mut selection = self.random.next_int_bounded(self.total_weight);
            for index in 0..self.current_pieces.len() {
                let weight = self.current_pieces[index];
                selection -= weight.weight;
                if selection < 0 {
                    if !weight.do_place(depth) || Some(weight.kind) == self.previous_piece {
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
                        let entry = &mut self.current_pieces[index];
                        entry.place_count += 1;
                        self.previous_piece = Some(weight.kind);
                        if !entry.is_valid() {
                            self.current_pieces.remove(index);
                        }
                        return Some(piece);
                    }
                }
            }
        }
        let bb = find_filler_box(self.builder, foot, direction)?;
        (bb.min_y() > 1).then(|| {
            let steps = match direction {
                Direction::North | Direction::South => bb.z_span(),
                _ => bb.x_span(),
            };
            StrongholdPiece::new(
                StrongholdKind::FillerCorridor { steps },
                depth,
                bb,
                direction,
                SmallDoorType::Opening,
            )
        })
    }

    /// `StrongholdPieces.generateAndAddPiece(startPiece, builder, random, footX, footY, footZ,
    /// direction, depth)`.
    fn generate_and_add_piece(&mut self, x: i32, y: i32, z: i32, direction: Direction, depth: i32) {
        if depth > MAX_DEPTH
            || (x - self.start_min_x).abs() > 112
            || (z - self.start_min_z).abs() > 112
        {
            return;
        }
        if let Some(piece) =
            self.generate_piece_from_small_door(BlockPos::new(x, y, z), direction, depth + 1)
        {
            self.builder.add_piece(Box::new(piece.clone()));
            self.pending.push(piece);
        }
    }

    /// `generateSmallDoorChildForward(startPiece, builder, random, xOff, yOff)`.
    fn child_forward(&mut self, piece: &StrongholdPiece, x_off: i32, y_off: i32) {
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
        self.generate_and_add_piece(x, bb.min_y() + y_off, z, orientation, depth);
    }

    /// `generateSmallDoorChildLeft(startPiece, builder, random, yOff, zOff)`.
    fn child_left(&mut self, piece: &StrongholdPiece, y_off: i32, z_off: i32) {
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
        self.generate_and_add_piece(x, bb.min_y() + y_off, z, direction, depth);
    }

    /// `generateSmallDoorChildRight(startPiece, builder, random, yOff, zOff)`.
    fn child_right(&mut self, piece: &StrongholdPiece, y_off: i32, z_off: i32) {
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
        self.generate_and_add_piece(x, bb.min_y() + y_off, z, direction, depth);
    }

    /// `addChildren(startPiece, builder, random)` of each piece class.
    fn add_children(&mut self, piece: &StrongholdPiece) {
        let orientation = piece.base.orientation();
        let north_or_east = matches!(orientation, Some(Direction::North | Direction::East));
        match piece.kind {
            StrongholdKind::ChestCorridor { .. }
            | StrongholdKind::PrisonHall
            | StrongholdKind::StraightStairsDown => self.child_forward(piece, 1, 1),
            StrongholdKind::FillerCorridor { .. } | StrongholdKind::Library { .. } => {}
            StrongholdKind::FiveCrossing {
                left_low,
                left_high,
                right_low,
                right_high,
            } => {
                let (mut z_off_a, mut z_off_b) = (3, 5);
                if matches!(orientation, Some(Direction::West | Direction::North)) {
                    z_off_a = 8 - z_off_a;
                    z_off_b = 8 - z_off_b;
                }
                self.child_forward(piece, 5, 1);
                if left_low {
                    self.child_left(piece, z_off_a, 1);
                }
                if left_high {
                    self.child_left(piece, z_off_b, 7);
                }
                if right_low {
                    self.child_right(piece, z_off_a, 1);
                }
                if right_high {
                    self.child_right(piece, z_off_b, 7);
                }
            }
            StrongholdKind::LeftTurn => {
                if north_or_east {
                    self.child_left(piece, 1, 1);
                } else {
                    self.child_right(piece, 1, 1);
                }
            }
            StrongholdKind::RightTurn => {
                if north_or_east {
                    self.child_right(piece, 1, 1);
                } else {
                    self.child_left(piece, 1, 1);
                }
            }
            StrongholdKind::PortalRoom { .. } => self.portal_room = true,
            StrongholdKind::RoomCrossing { .. } => {
                self.child_forward(piece, 4, 1);
                self.child_left(piece, 1, 4);
                self.child_right(piece, 1, 4);
            }
            StrongholdKind::StairsDown { is_source } => {
                if is_source {
                    self.imposed_piece = Some(PieceClass::FiveCrossing);
                }
                self.child_forward(piece, 1, 1);
            }
            StrongholdKind::Straight {
                left_child,
                right_child,
            } => {
                self.child_forward(piece, 1, 1);
                if left_child {
                    self.child_left(piece, 1, 2);
                }
                if right_child {
                    self.child_right(piece, 1, 2);
                }
            }
        }
    }
}

/// `isOkBox(box)`: above y 10.
fn is_ok_box(bb: &BoundingBox) -> bool {
    bb.min_y() > 10
}

/// `findAndCreatePieceFactory(pieceClass, builder, random, footX, footY, footZ, direction,
/// depth)`: each class's `createPiece`.
fn create_piece(
    class: PieceClass,
    builder: &StructurePiecesBuilder,
    random: &mut LegacyRandom,
    foot: BlockPos,
    direction: Direction,
    depth: i32,
) -> Option<StrongholdPiece> {
    let fits = |bb: &BoundingBox| is_ok_box(bb) && builder.find_collision_piece(bb).is_none();
    let orient = |off_x, off_y, width, height, depth| {
        BoundingBox::orient_box(foot, off_x, off_y, 0, width, height, depth, direction)
    };
    let bb = match class {
        PieceClass::Straight | PieceClass::ChestCorridor => orient(-1, -1, 5, 5, 7),
        PieceClass::PrisonHall => orient(-1, -1, 9, 5, 11),
        PieceClass::LeftTurn | PieceClass::RightTurn => orient(-1, -1, 5, 5, 5),
        PieceClass::RoomCrossing => orient(-4, -1, 11, 7, 11),
        PieceClass::StraightStairsDown => orient(-1, -7, 5, 11, 8),
        PieceClass::StairsDown => orient(-1, -7, 5, 11, 5),
        PieceClass::FiveCrossing => orient(-4, -3, 10, 9, 11),
        PieceClass::PortalRoom => orient(-4, -1, 11, 8, 16),
        PieceClass::Library => {
            let tall = orient(-4, -1, 14, 11, 15);
            if fits(&tall) {
                tall
            } else {
                orient(-4, -1, 14, 6, 15)
            }
        }
    };
    if !fits(&bb) {
        return None;
    }
    if class == PieceClass::PortalRoom {
        let kind = StrongholdKind::PortalRoom {
            has_placed_spawner: false,
        };
        return Some(StrongholdPiece::new(
            kind,
            depth,
            bb,
            direction,
            SmallDoorType::Opening,
        ));
    }
    let entry_door = SmallDoorType::random(random);
    let kind = match class {
        PieceClass::Straight => {
            let left_child = random.next_int_bounded(2) == 0;
            let right_child = random.next_int_bounded(2) == 0;
            StrongholdKind::Straight {
                left_child,
                right_child,
            }
        }
        PieceClass::PrisonHall => StrongholdKind::PrisonHall,
        PieceClass::LeftTurn => StrongholdKind::LeftTurn,
        PieceClass::RightTurn => StrongholdKind::RightTurn,
        PieceClass::RoomCrossing => StrongholdKind::RoomCrossing {
            room_type: random.next_int_bounded(5),
        },
        PieceClass::StraightStairsDown => StrongholdKind::StraightStairsDown,
        PieceClass::StairsDown => StrongholdKind::StairsDown { is_source: false },
        PieceClass::FiveCrossing => {
            let left_low = random.next_boolean();
            let left_high = random.next_boolean();
            let right_low = random.next_boolean();
            let right_high = random.next_int_bounded(3) > 0;
            StrongholdKind::FiveCrossing {
                left_low,
                left_high,
                right_low,
                right_high,
            }
        }
        PieceClass::ChestCorridor => StrongholdKind::ChestCorridor {
            has_placed_chest: false,
        },
        PieceClass::Library => StrongholdKind::Library {
            is_tall: bb.y_span() > 6,
        },
        PieceClass::PortalRoom => unreachable!("handled above"),
    };
    Some(StrongholdPiece::new(kind, depth, bb, direction, entry_door))
}

/// `FillerCorridor.findPieceBox(builder, random, footX, footY, footZ, direction)`: a short
/// corridor up to the piece in front, if that piece is level with it.
fn find_filler_box(
    builder: &StructurePiecesBuilder,
    foot: BlockPos,
    direction: Direction,
) -> Option<BoundingBox> {
    let orient = |depth| BoundingBox::orient_box(foot, -1, -1, 0, 5, 5, depth, direction);
    let bb = orient(4);
    let collision = *builder.pieces[builder.find_collision_piece(&bb)?].bounding_box();
    if collision.min_y() == bb.min_y() {
        for depth in (1..=2).rev() {
            if !collision.intersects(&orient(depth)) {
                return Some(orient(depth + 1));
            }
        }
    }
    None
}

/// `StrongholdPiece.SmallDoorType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SmallDoorType {
    Opening,
    WoodDoor,
    Grates,
    IronDoor,
}

impl SmallDoorType {
    /// `randomSmallDoor(random)`.
    fn random(random: &mut LegacyRandom) -> Self {
        match random.next_int_bounded(5) {
            2 => Self::WoodDoor,
            3 => Self::Grates,
            4 => Self::IronDoor,
            _ => Self::Opening,
        }
    }

    /// The enum constant name (the saved `EntryDoor`).
    pub fn name(self) -> &'static str {
        match self {
            Self::Opening => "OPENING",
            Self::WoodDoor => "WOOD_DOOR",
            Self::Grates => "GRATES",
            Self::IronDoor => "IRON_DOOR",
        }
    }
}

/// The concrete piece class and its own state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StrongholdKind {
    /// `ChestCorridor` (`minecraft:shcc`).
    ChestCorridor { has_placed_chest: bool },
    /// `FillerCorridor` (`minecraft:shfc`).
    FillerCorridor { steps: i32 },
    /// `FiveCrossing` (`minecraft:sh5c`).
    FiveCrossing {
        left_low: bool,
        left_high: bool,
        right_low: bool,
        right_high: bool,
    },
    /// `LeftTurn` (`minecraft:shlt`).
    LeftTurn,
    /// `Library` (`minecraft:shli`).
    Library { is_tall: bool },
    /// `PortalRoom` (`minecraft:shpr`).
    PortalRoom { has_placed_spawner: bool },
    /// `PrisonHall` (`minecraft:shph`).
    PrisonHall,
    /// `RightTurn` (`minecraft:shrt`).
    RightTurn,
    /// `RoomCrossing` (`minecraft:shrc`).
    RoomCrossing { room_type: i32 },
    /// `StairsDown` (`minecraft:shsd`), or the `StartPiece` (`minecraft:shstart`) with
    /// `is_source`.
    StairsDown { is_source: bool },
    /// `Straight` (`minecraft:shs`).
    Straight { left_child: bool, right_child: bool },
    /// `StraightStairsDown` (`minecraft:shssd`).
    StraightStairsDown,
}

impl StrongholdKind {
    /// The `StructurePieceType` id.
    fn piece_type(self) -> &'static str {
        match self {
            Self::ChestCorridor { .. } => "minecraft:shcc",
            Self::FillerCorridor { .. } => "minecraft:shfc",
            Self::FiveCrossing { .. } => "minecraft:sh5c",
            Self::LeftTurn => "minecraft:shlt",
            Self::Library { .. } => "minecraft:shli",
            Self::PortalRoom { .. } => "minecraft:shpr",
            Self::PrisonHall => "minecraft:shph",
            Self::RightTurn => "minecraft:shrt",
            Self::RoomCrossing { .. } => "minecraft:shrc",
            Self::StairsDown { is_source: true } => "minecraft:shstart",
            Self::StairsDown { is_source: false } => "minecraft:shsd",
            Self::Straight { .. } => "minecraft:shs",
            Self::StraightStairsDown => "minecraft:shssd",
        }
    }
}

/// A stronghold piece (`StrongholdPieces.StrongholdPiece` and its subclasses).
#[derive(Clone, Debug)]
pub struct StrongholdPiece {
    pub base: PieceBase,
    /// `entryDoor`.
    pub entry_door: SmallDoorType,
    pub kind: StrongholdKind,
}

impl StrongholdPiece {
    fn new(
        kind: StrongholdKind,
        gen_depth: i32,
        bb: BoundingBox,
        direction: Direction,
        entry_door: SmallDoorType,
    ) -> Self {
        let mut base = PieceBase::new(kind.piece_type(), gen_depth, bb);
        base.set_orientation(Some(direction));
        Self {
            base,
            entry_door,
            kind,
        }
    }

    /// `new StartPiece(random, west, north)`.
    fn start(random: &mut LegacyRandom, west: i32, north: i32) -> Self {
        let direction = random_horizontal_direction(random);
        let bb = make_bounding_box(west, 64, north, direction, 5, 11, 5);
        Self::new(
            StrongholdKind::StairsDown { is_source: true },
            0,
            bb,
            direction,
            SmallDoorType::Opening,
        )
    }

    /// `generateBox(level, chunkBB, x0, y0, z0, x1, y1, z1, skipAir, random,
    /// SMOOTH_STONE_SELECTOR)`.
    #[allow(clippy::too_many_arguments)]
    fn stone_box(
        &self,
        level: &mut WorldGenRegion<'_>,
        bb: &BoundingBox,
        random: &mut DecorationRandom,
        x0: i32,
        y0: i32,
        z0: i32,
        x1: i32,
        y1: i32,
        z1: i32,
        skip_air: bool,
    ) {
        let mut selector = SmoothStoneSelector(BlockState::AIR);
        self.generate_box_with(
            level,
            bb,
            x0,
            y0,
            z0,
            x1,
            y1,
            z1,
            skip_air,
            random,
            &mut selector,
        );
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

    /// `generateSmallDoor(level, random, chunkBB, doorType, footX, footY, footZ)`.
    fn small_door(
        &self,
        level: &mut WorldGenRegion<'_>,
        bb: &BoundingBox,
        door: SmallDoorType,
        x: i32,
        y: i32,
        z: i32,
    ) {
        let bricks = ids::STONE_BRICKS.default_state();
        match door {
            SmallDoorType::Opening => {
                self.fill(level, bb, x, y, z, x + 3 - 1, y + 3 - 1, z, cave_air());
            }
            SmallDoorType::WoodDoor | SmallDoorType::IronDoor => {
                self.place_block(level, bricks, x, y, z, bb);
                self.place_block(level, bricks, x, y + 1, z, bb);
                self.place_block(level, bricks, x, y + 2, z, bb);
                self.place_block(level, bricks, x + 1, y + 2, z, bb);
                self.place_block(level, bricks, x + 2, y + 2, z, bb);
                self.place_block(level, bricks, x + 2, y + 1, z, bb);
                self.place_block(level, bricks, x + 2, y, z, bb);
                let door_block = if door == SmallDoorType::WoodDoor {
                    ids::OAK_DOOR
                } else {
                    ids::IRON_DOOR
                };
                let lower = door_block.default_state();
                self.place_block(level, lower, x + 1, y, z, bb);
                let upper = with_props(lower, &[("half", "upper")]);
                self.place_block(level, upper, x + 1, y + 1, z, bb);
                if door == SmallDoorType::IronDoor {
                    let button = ids::STONE_BUTTON.default_state();
                    let north = with_facing(button, Direction::North);
                    self.place_block(level, north, x + 2, y + 1, z + 1, bb);
                    let south = with_facing(button, Direction::South);
                    self.place_block(level, south, x + 2, y + 1, z - 1, bb);
                }
            }
            SmallDoorType::Grates => {
                let bars = ids::IRON_BARS.default_state();
                let west = with_props(bars, &[("west", "true")]);
                let east = with_props(bars, &[("east", "true")]);
                let both = with_props(bars, &[("east", "true"), ("west", "true")]);
                self.place_block(level, cave_air(), x + 1, y, z, bb);
                self.place_block(level, cave_air(), x + 1, y + 1, z, bb);
                self.place_block(level, west, x, y, z, bb);
                self.place_block(level, west, x, y + 1, z, bb);
                self.place_block(level, both, x, y + 2, z, bb);
                self.place_block(level, both, x + 1, y + 2, z, bb);
                self.place_block(level, both, x + 2, y + 2, z, bb);
                self.place_block(level, east, x + 2, y + 1, z, bb);
                self.place_block(level, east, x + 2, y, z, bb);
            }
        }
    }

    fn post_process_chest_corridor(
        &mut self,
        level: &mut WorldGenRegion<'_>,
        bb: &BoundingBox,
        random: &mut DecorationRandom,
    ) {
        self.stone_box(level, bb, random, 0, 0, 0, 4, 4, 6, true);
        self.small_door(level, bb, self.entry_door, 1, 1, 0);
        self.small_door(level, bb, SmallDoorType::Opening, 1, 1, 6);
        let bricks = ids::STONE_BRICKS.default_state();
        self.fill(level, bb, 3, 1, 2, 3, 1, 4, bricks);
        let slab = ids::STONE_BRICK_SLAB.default_state();
        self.place_block(level, slab, 3, 1, 1, bb);
        self.place_block(level, slab, 3, 1, 5, bb);
        self.place_block(level, slab, 3, 2, 2, bb);
        self.place_block(level, slab, 3, 2, 4, bb);
        for z in 2..=4 {
            self.place_block(level, slab, 2, 1, z, bb);
        }
        if let StrongholdKind::ChestCorridor { has_placed_chest } = self.kind
            && !has_placed_chest
            && bb.is_inside(self.world_pos(3, 2, 3))
        {
            self.kind = StrongholdKind::ChestCorridor {
                has_placed_chest: true,
            };
            self.create_chest(level, bb, random, 3, 2, 3, LOOT_CORRIDOR);
        }
    }

    fn post_process_filler_corridor(
        &self,
        level: &mut WorldGenRegion<'_>,
        bb: &BoundingBox,
        steps: i32,
    ) {
        let bricks = ids::STONE_BRICKS.default_state();
        for i in 0..steps {
            for x in 0..=4 {
                self.place_block(level, bricks, x, 0, i, bb);
            }
            for y in 1..=3 {
                self.place_block(level, bricks, 0, y, i, bb);
                self.place_block(level, cave_air(), 1, y, i, bb);
                self.place_block(level, cave_air(), 2, y, i, bb);
                self.place_block(level, cave_air(), 3, y, i, bb);
                self.place_block(level, bricks, 4, y, i, bb);
            }
            for x in 0..=4 {
                self.place_block(level, bricks, x, 4, i, bb);
            }
        }
    }

    fn post_process_five_crossing(
        &self,
        level: &mut WorldGenRegion<'_>,
        bb: &BoundingBox,
        random: &mut DecorationRandom,
        [left_low, left_high, right_low, right_high]: [bool; 4],
    ) {
        let air = cave_air();
        self.stone_box(level, bb, random, 0, 0, 0, 9, 8, 10, true);
        self.small_door(level, bb, self.entry_door, 4, 3, 0);
        if left_low {
            self.fill(level, bb, 0, 3, 1, 0, 5, 3, air);
        }
        if right_low {
            self.fill(level, bb, 9, 3, 1, 9, 5, 3, air);
        }
        if left_high {
            self.fill(level, bb, 0, 5, 7, 0, 7, 9, air);
        }
        if right_high {
            self.fill(level, bb, 9, 5, 7, 9, 7, 9, air);
        }
        self.fill(level, bb, 5, 1, 10, 7, 3, 10, air);
        self.stone_box(level, bb, random, 1, 2, 1, 8, 2, 6, false);
        self.stone_box(level, bb, random, 4, 1, 5, 4, 4, 9, false);
        self.stone_box(level, bb, random, 8, 1, 5, 8, 4, 9, false);
        self.stone_box(level, bb, random, 1, 4, 7, 3, 4, 9, false);
        self.stone_box(level, bb, random, 1, 3, 5, 3, 3, 6, false);
        let slab = ids::SMOOTH_STONE_SLAB.default_state();
        self.fill(level, bb, 1, 3, 4, 3, 3, 4, slab);
        self.fill(level, bb, 1, 4, 6, 3, 4, 6, slab);
        self.stone_box(level, bb, random, 5, 1, 7, 7, 1, 8, false);
        self.fill(level, bb, 5, 1, 9, 7, 1, 9, slab);
        self.fill(level, bb, 5, 2, 7, 7, 2, 7, slab);
        self.fill(level, bb, 4, 5, 7, 4, 5, 9, slab);
        self.fill(level, bb, 8, 5, 7, 8, 5, 9, slab);
        let double = with_props(slab, &[("type", "double")]);
        self.fill(level, bb, 5, 5, 7, 7, 5, 9, double);
        let torch = with_facing(ids::WALL_TORCH.default_state(), Direction::South);
        self.place_block(level, torch, 6, 5, 6, bb);
    }

    fn post_process_turn(
        &self,
        level: &mut WorldGenRegion<'_>,
        bb: &BoundingBox,
        random: &mut DecorationRandom,
        left: bool,
    ) {
        self.stone_box(level, bb, random, 0, 0, 0, 4, 4, 4, true);
        self.small_door(level, bb, self.entry_door, 1, 1, 0);
        let north_or_east = matches!(
            self.base.orientation(),
            Some(Direction::North | Direction::East)
        );
        // A left turn opens on the left side (x 0) for north and east, a right turn on x 4.
        let x = if north_or_east == left { 0 } else { 4 };
        self.fill(level, bb, x, 1, 1, x, 3, 3, cave_air());
    }

    fn post_process_library(
        &self,
        level: &mut WorldGenRegion<'_>,
        bb: &BoundingBox,
        random: &mut DecorationRandom,
        is_tall: bool,
    ) {
        let height = if is_tall { 11 } else { 6 };
        self.stone_box(level, bb, random, 0, 0, 0, 13, height - 1, 14, true);
        self.small_door(level, bb, self.entry_door, 4, 1, 0);
        let cobweb = ids::COBWEB.default_state();
        self.generate_maybe_box(
            level, bb, random, 0.07, 2, 1, 1, 11, 4, 13, cobweb, cobweb, false, false,
        );
        let planks = ids::OAK_PLANKS.default_state();
        let shelf = ids::BOOKSHELF.default_state();
        let wall_torch = ids::WALL_TORCH.default_state();
        for d in 1..=13 {
            if (d - 1) % 4 == 0 {
                self.fill(level, bb, 1, 1, d, 1, 4, d, planks);
                self.fill(level, bb, 12, 1, d, 12, 4, d, planks);
                self.place_block(level, with_facing(wall_torch, Direction::East), 2, 3, d, bb);
                self.place_block(
                    level,
                    with_facing(wall_torch, Direction::West),
                    11,
                    3,
                    d,
                    bb,
                );
                if is_tall {
                    self.fill(level, bb, 1, 6, d, 1, 9, d, planks);
                    self.fill(level, bb, 12, 6, d, 12, 9, d, planks);
                }
            } else {
                self.fill(level, bb, 1, 1, d, 1, 4, d, shelf);
                self.fill(level, bb, 12, 1, d, 12, 4, d, shelf);
                if is_tall {
                    self.fill(level, bb, 1, 6, d, 1, 9, d, shelf);
                    self.fill(level, bb, 12, 6, d, 12, 9, d, shelf);
                }
            }
        }
        for d in (3..12).step_by(2) {
            self.fill(level, bb, 3, 1, d, 4, 3, d, shelf);
            self.fill(level, bb, 6, 1, d, 7, 3, d, shelf);
            self.fill(level, bb, 9, 1, d, 10, 3, d, shelf);
        }
        if is_tall {
            self.fill(level, bb, 1, 5, 1, 3, 5, 13, planks);
            self.fill(level, bb, 10, 5, 1, 12, 5, 13, planks);
            self.fill(level, bb, 4, 5, 1, 9, 5, 2, planks);
            self.fill(level, bb, 4, 5, 12, 9, 5, 13, planks);
            self.place_block(level, planks, 9, 5, 11, bb);
            self.place_block(level, planks, 8, 5, 11, bb);
            self.place_block(level, planks, 9, 5, 10, bb);
            let fence = ids::OAK_FENCE.default_state();
            let we = with_props(fence, &[("west", "true"), ("east", "true")]);
            let ns = with_props(fence, &[("north", "true"), ("south", "true")]);
            self.fill(level, bb, 3, 6, 3, 3, 6, 11, ns);
            self.fill(level, bb, 10, 6, 3, 10, 6, 9, ns);
            self.fill(level, bb, 4, 6, 2, 9, 6, 2, we);
            self.fill(level, bb, 4, 6, 12, 7, 6, 12, we);
            let ne = with_props(fence, &[("north", "true"), ("east", "true")]);
            let se = with_props(fence, &[("south", "true"), ("east", "true")]);
            let nw = with_props(fence, &[("north", "true"), ("west", "true")]);
            let sw = with_props(fence, &[("south", "true"), ("west", "true")]);
            self.place_block(level, ne, 3, 6, 2, bb);
            self.place_block(level, se, 3, 6, 12, bb);
            self.place_block(level, nw, 10, 6, 2, bb);
            for i in 0..=2 {
                self.place_block(level, sw, 8 + i, 6, 12 - i, bb);
                if i != 2 {
                    self.place_block(level, ne, 8 + i, 6, 11 - i, bb);
                }
            }
            let ladder = with_facing(ids::LADDER.default_state(), Direction::South);
            for y in 1..=7 {
                self.place_block(level, ladder, 10, y, 13, bb);
            }
            let e = with_props(fence, &[("east", "true")]);
            let w = with_props(fence, &[("west", "true")]);
            self.place_block(level, e, 6, 9, 7, bb);
            self.place_block(level, w, 7, 9, 7, bb);
            self.place_block(level, e, 6, 8, 7, bb);
            self.place_block(level, w, 7, 8, 7, bb);
            let nswe = with_props(ns, &[("west", "true"), ("east", "true")]);
            self.place_block(level, nswe, 6, 7, 7, bb);
            self.place_block(level, nswe, 7, 7, 7, bb);
            self.place_block(level, e, 5, 7, 7, bb);
            self.place_block(level, w, 8, 7, 7, bb);
            self.place_block(level, with_props(e, &[("north", "true")]), 6, 7, 6, bb);
            self.place_block(level, with_props(e, &[("south", "true")]), 6, 7, 8, bb);
            self.place_block(level, with_props(w, &[("north", "true")]), 7, 7, 6, bb);
            self.place_block(level, with_props(w, &[("south", "true")]), 7, 7, 8, bb);
            let torch = ids::TORCH.default_state();
            self.place_block(level, torch, 5, 8, 7, bb);
            self.place_block(level, torch, 8, 8, 7, bb);
            self.place_block(level, torch, 6, 8, 6, bb);
            self.place_block(level, torch, 6, 8, 8, bb);
            self.place_block(level, torch, 7, 8, 6, bb);
            self.place_block(level, torch, 7, 8, 8, bb);
        }
        self.create_chest(level, bb, random, 3, 3, 5, LOOT_LIBRARY);
        if is_tall {
            self.place_block(level, cave_air(), 12, 9, 1, bb);
            self.create_chest(level, bb, random, 12, 8, 1, LOOT_LIBRARY);
        }
    }

    fn post_process_portal_room(
        &mut self,
        level: &mut WorldGenRegion<'_>,
        bb: &BoundingBox,
        random: &mut DecorationRandom,
    ) {
        self.stone_box(level, bb, random, 0, 0, 0, 10, 7, 15, false);
        self.small_door(level, bb, SmallDoorType::Grates, 4, 1, 0);
        self.stone_box(level, bb, random, 1, 6, 1, 1, 6, 14, false);
        self.stone_box(level, bb, random, 9, 6, 1, 9, 6, 14, false);
        self.stone_box(level, bb, random, 2, 6, 1, 8, 6, 2, false);
        self.stone_box(level, bb, random, 2, 6, 14, 8, 6, 14, false);
        self.stone_box(level, bb, random, 1, 1, 1, 2, 1, 4, false);
        self.stone_box(level, bb, random, 8, 1, 1, 9, 1, 4, false);
        let lava = ids::LAVA.default_state();
        self.fill(level, bb, 1, 1, 1, 1, 1, 3, lava);
        self.fill(level, bb, 9, 1, 1, 9, 1, 3, lava);
        self.stone_box(level, bb, random, 3, 1, 8, 7, 1, 12, false);
        self.fill(level, bb, 4, 1, 9, 6, 1, 11, lava);
        let bars = ids::IRON_BARS.default_state();
        let ns_bars = with_props(bars, &[("north", "true"), ("south", "true")]);
        let we_bars = with_props(bars, &[("west", "true"), ("east", "true")]);
        for z in (3..14).step_by(2) {
            self.fill(level, bb, 0, 3, z, 0, 4, z, ns_bars);
            self.fill(level, bb, 10, 3, z, 10, 4, z, ns_bars);
        }
        for x in (2..9).step_by(2) {
            self.fill(level, bb, x, 3, 15, x, 4, 15, we_bars);
        }
        let stairs = with_facing(ids::STONE_BRICK_STAIRS.default_state(), Direction::North);
        self.stone_box(level, bb, random, 4, 1, 5, 6, 1, 7, false);
        self.stone_box(level, bb, random, 4, 2, 6, 6, 2, 7, false);
        self.stone_box(level, bb, random, 4, 3, 7, 6, 3, 7, false);
        for x in 4..=6 {
            self.place_block(level, stairs, x, 1, 4, bb);
            self.place_block(level, stairs, x, 2, 5, bb);
            self.place_block(level, stairs, x, 3, 6, bb);
        }
        let frame = ids::END_PORTAL_FRAME.default_state();
        let mut all_eyes = true;
        let mut eyes = [false; 12];
        for eye in &mut eyes {
            *eye = random.next_float() > 0.9;
            all_eyes &= *eye;
        }
        let frames = [
            (Direction::North, 4, 8),
            (Direction::North, 5, 8),
            (Direction::North, 6, 8),
            (Direction::South, 4, 12),
            (Direction::South, 5, 12),
            (Direction::South, 6, 12),
            (Direction::East, 3, 9),
            (Direction::East, 3, 10),
            (Direction::East, 3, 11),
            (Direction::West, 7, 9),
            (Direction::West, 7, 10),
            (Direction::West, 7, 11),
        ];
        for ((facing, x, z), eye) in frames.into_iter().zip(eyes) {
            let eye = if eye { "true" } else { "false" };
            let state = with_props(with_facing(frame, facing), &[("eye", eye)]);
            self.place_block(level, state, x, 3, z, bb);
        }
        if all_eyes {
            let portal = ids::END_PORTAL.default_state();
            for z in 9..=11 {
                for x in 4..=6 {
                    self.place_block(level, portal, x, 3, z, bb);
                }
            }
        }
        if let StrongholdKind::PortalRoom {
            has_placed_spawner: false,
        } = self.kind
        {
            let pos = self.world_pos(5, 3, 6);
            if bb.is_inside(pos) {
                self.kind = StrongholdKind::PortalRoom {
                    has_placed_spawner: true,
                };
                place_spawner(level, pos, "minecraft:silverfish");
            }
        }
    }

    fn post_process_prison_hall(
        &self,
        level: &mut WorldGenRegion<'_>,
        bb: &BoundingBox,
        random: &mut DecorationRandom,
    ) {
        self.stone_box(level, bb, random, 0, 0, 0, 8, 4, 10, true);
        self.small_door(level, bb, self.entry_door, 1, 1, 0);
        self.fill(level, bb, 1, 1, 10, 3, 3, 10, cave_air());
        self.stone_box(level, bb, random, 4, 1, 1, 4, 3, 1, false);
        self.stone_box(level, bb, random, 4, 1, 3, 4, 3, 3, false);
        self.stone_box(level, bb, random, 4, 1, 7, 4, 3, 7, false);
        self.stone_box(level, bb, random, 4, 1, 9, 4, 3, 9, false);
        let bars = ids::IRON_BARS.default_state();
        let ns = with_props(bars, &[("north", "true"), ("south", "true")]);
        let nse = with_props(ns, &[("east", "true")]);
        let we = with_props(bars, &[("west", "true"), ("east", "true")]);
        for y in 1..=3 {
            self.place_block(level, ns, 4, y, 4, bb);
            self.place_block(level, nse, 4, y, 5, bb);
            self.place_block(level, ns, 4, y, 6, bb);
            self.place_block(level, we, 5, y, 5, bb);
            self.place_block(level, we, 6, y, 5, bb);
            self.place_block(level, we, 7, y, 5, bb);
        }
        self.place_block(level, ns, 4, 3, 2, bb);
        self.place_block(level, ns, 4, 3, 8, bb);
        let bottom = with_facing(ids::IRON_DOOR.default_state(), Direction::West);
        let top = with_props(bottom, &[("half", "upper")]);
        self.place_block(level, bottom, 4, 1, 2, bb);
        self.place_block(level, top, 4, 2, 2, bb);
        self.place_block(level, bottom, 4, 1, 8, bb);
        self.place_block(level, top, 4, 2, 8, bb);
    }

    fn post_process_room_crossing(
        &self,
        level: &mut WorldGenRegion<'_>,
        bb: &BoundingBox,
        random: &mut DecorationRandom,
        room_type: i32,
    ) {
        let air = cave_air();
        self.stone_box(level, bb, random, 0, 0, 0, 10, 6, 10, true);
        self.small_door(level, bb, self.entry_door, 4, 1, 0);
        self.fill(level, bb, 4, 1, 10, 6, 3, 10, air);
        self.fill(level, bb, 0, 1, 4, 0, 3, 6, air);
        self.fill(level, bb, 10, 1, 4, 10, 3, 6, air);
        let bricks = ids::STONE_BRICKS.default_state();
        let wall_torch = ids::WALL_TORCH.default_state();
        match room_type {
            0 => {
                self.place_block(level, bricks, 5, 1, 5, bb);
                self.place_block(level, bricks, 5, 2, 5, bb);
                self.place_block(level, bricks, 5, 3, 5, bb);
                self.place_block(level, with_facing(wall_torch, Direction::West), 4, 3, 5, bb);
                self.place_block(level, with_facing(wall_torch, Direction::East), 6, 3, 5, bb);
                self.place_block(
                    level,
                    with_facing(wall_torch, Direction::South),
                    5,
                    3,
                    4,
                    bb,
                );
                self.place_block(
                    level,
                    with_facing(wall_torch, Direction::North),
                    5,
                    3,
                    6,
                    bb,
                );
                let slab = ids::SMOOTH_STONE_SLAB.default_state();
                for (x, z) in [
                    (4, 4),
                    (4, 5),
                    (4, 6),
                    (6, 4),
                    (6, 5),
                    (6, 6),
                    (5, 4),
                    (5, 6),
                ] {
                    self.place_block(level, slab, x, 1, z, bb);
                }
            }
            1 => {
                for i in 0..5 {
                    self.place_block(level, bricks, 3, 1, 3 + i, bb);
                    self.place_block(level, bricks, 7, 1, 3 + i, bb);
                    self.place_block(level, bricks, 3 + i, 1, 3, bb);
                    self.place_block(level, bricks, 3 + i, 1, 7, bb);
                }
                self.place_block(level, bricks, 5, 1, 5, bb);
                self.place_block(level, bricks, 5, 2, 5, bb);
                self.place_block(level, bricks, 5, 3, 5, bb);
                self.place_block(level, ids::WATER.default_state(), 5, 4, 5, bb);
            }
            2 => {
                let cobble = ids::COBBLESTONE.default_state();
                for z in 1..=9 {
                    self.place_block(level, cobble, 1, 3, z, bb);
                    self.place_block(level, cobble, 9, 3, z, bb);
                }
                for x in 1..=9 {
                    self.place_block(level, cobble, x, 3, 1, bb);
                    self.place_block(level, cobble, x, 3, 9, bb);
                }
                for (x, y, z) in [
                    (5, 1, 4),
                    (5, 1, 6),
                    (5, 3, 4),
                    (5, 3, 6),
                    (4, 1, 5),
                    (6, 1, 5),
                    (4, 3, 5),
                    (6, 3, 5),
                ] {
                    self.place_block(level, cobble, x, y, z, bb);
                }
                for y in 1..=3 {
                    self.place_block(level, cobble, 4, y, 4, bb);
                    self.place_block(level, cobble, 6, y, 4, bb);
                    self.place_block(level, cobble, 4, y, 6, bb);
                    self.place_block(level, cobble, 6, y, 6, bb);
                }
                self.place_block(level, wall_torch, 5, 3, 5, bb);
                let planks = ids::OAK_PLANKS.default_state();
                for z in 2..=8 {
                    self.place_block(level, planks, 2, 3, z, bb);
                    self.place_block(level, planks, 3, 3, z, bb);
                    if z <= 3 || z >= 7 {
                        self.place_block(level, planks, 4, 3, z, bb);
                        self.place_block(level, planks, 5, 3, z, bb);
                        self.place_block(level, planks, 6, 3, z, bb);
                    }
                    self.place_block(level, planks, 7, 3, z, bb);
                    self.place_block(level, planks, 8, 3, z, bb);
                }
                let ladder = with_facing(ids::LADDER.default_state(), Direction::West);
                self.place_block(level, ladder, 9, 1, 3, bb);
                self.place_block(level, ladder, 9, 2, 3, bb);
                self.place_block(level, ladder, 9, 3, 3, bb);
                self.create_chest(level, bb, random, 3, 4, 8, LOOT_CROSSING);
            }
            _ => {}
        }
    }

    fn post_process_stairs_down(
        &self,
        level: &mut WorldGenRegion<'_>,
        bb: &BoundingBox,
        random: &mut DecorationRandom,
    ) {
        self.stone_box(level, bb, random, 0, 0, 0, 4, 10, 4, true);
        self.small_door(level, bb, self.entry_door, 1, 7, 0);
        self.small_door(level, bb, SmallDoorType::Opening, 1, 1, 4);
        let bricks = ids::STONE_BRICKS.default_state();
        let slab = ids::SMOOTH_STONE_SLAB.default_state();
        let steps = [
            (bricks, 2, 6, 1),
            (bricks, 1, 5, 1),
            (slab, 1, 6, 1),
            (bricks, 1, 5, 2),
            (bricks, 1, 4, 3),
            (slab, 1, 5, 3),
            (bricks, 2, 4, 3),
            (bricks, 3, 3, 3),
            (slab, 3, 4, 3),
            (bricks, 3, 3, 2),
            (bricks, 3, 2, 1),
            (slab, 3, 3, 1),
            (bricks, 2, 2, 1),
            (bricks, 1, 1, 1),
            (slab, 1, 2, 1),
            (bricks, 1, 1, 2),
            (slab, 1, 1, 3),
        ];
        for (state, x, y, z) in steps {
            self.place_block(level, state, x, y, z, bb);
        }
    }

    fn post_process_straight(
        &self,
        level: &mut WorldGenRegion<'_>,
        bb: &BoundingBox,
        random: &mut DecorationRandom,
        left_child: bool,
        right_child: bool,
    ) {
        self.stone_box(level, bb, random, 0, 0, 0, 4, 4, 6, true);
        self.small_door(level, bb, self.entry_door, 1, 1, 0);
        self.small_door(level, bb, SmallDoorType::Opening, 1, 1, 6);
        let wall_torch = ids::WALL_TORCH.default_state();
        let east = with_facing(wall_torch, Direction::East);
        let west = with_facing(wall_torch, Direction::West);
        self.maybe_generate_block(level, bb, random, 0.1, 1, 2, 1, east);
        self.maybe_generate_block(level, bb, random, 0.1, 3, 2, 1, west);
        self.maybe_generate_block(level, bb, random, 0.1, 1, 2, 5, east);
        self.maybe_generate_block(level, bb, random, 0.1, 3, 2, 5, west);
        if left_child {
            self.fill(level, bb, 0, 1, 2, 0, 3, 4, cave_air());
        }
        if right_child {
            self.fill(level, bb, 4, 1, 2, 4, 3, 4, cave_air());
        }
    }

    fn post_process_straight_stairs_down(
        &self,
        level: &mut WorldGenRegion<'_>,
        bb: &BoundingBox,
        random: &mut DecorationRandom,
    ) {
        self.stone_box(level, bb, random, 0, 0, 0, 4, 10, 7, true);
        self.small_door(level, bb, self.entry_door, 1, 7, 0);
        self.small_door(level, bb, SmallDoorType::Opening, 1, 1, 7);
        let stairs = with_facing(ids::COBBLESTONE_STAIRS.default_state(), Direction::South);
        let bricks = ids::STONE_BRICKS.default_state();
        for i in 0..6 {
            for x in 1..=3 {
                self.place_block(level, stairs, x, 6 - i, 1 + i, bb);
            }
            if i < 5 {
                for x in 1..=3 {
                    self.place_block(level, bricks, x, 5 - i, 1 + i, bb);
                }
            }
        }
    }
}

/// `Blocks.CAVE_AIR.defaultBlockState()` (`StructurePiece.CAVE_AIR`).
fn cave_air() -> BlockState {
    ids::CAVE_AIR.default_state()
}

/// `SmoothStoneSelector`: cracked, mossy, infested or plain stone bricks on the edges, cave
/// air inside.
struct SmoothStoneSelector(BlockState);

impl BlockSelector for SmoothStoneSelector {
    fn next(&mut self, random: &mut DecorationRandom, _x: i32, _y: i32, _z: i32, is_edge: bool) {
        self.0 = if is_edge {
            let selection = random.next_float();
            if selection < 0.2 {
                ids::CRACKED_STONE_BRICKS.default_state()
            } else if selection < 0.5 {
                ids::MOSSY_STONE_BRICKS.default_state()
            } else if selection < 0.55 {
                ids::INFESTED_STONE_BRICKS.default_state()
            } else {
                ids::STONE_BRICKS.default_state()
            }
        } else {
            cave_air()
        };
    }

    fn get_next(&self) -> BlockState {
        self.0
    }
}

impl StructurePiece for StrongholdPiece {
    fn base(&self) -> &PieceBase {
        &self.base
    }

    fn base_mut(&mut self) -> &mut PieceBase {
        &mut self.base
    }

    /// `addAdditionalSaveData`.
    fn additional_save_data(&self) -> Option<Compound> {
        let mut tag = Compound::new();
        tag.insert("EntryDoor", Tag::String(self.entry_door.name().to_owned()));
        let flag = |b: bool| Tag::Byte(i8::from(b));
        match self.kind {
            StrongholdKind::ChestCorridor { has_placed_chest } => {
                tag.insert("Chest", flag(has_placed_chest));
            }
            StrongholdKind::FillerCorridor { steps } => tag.insert("Steps", Tag::Int(steps)),
            StrongholdKind::FiveCrossing {
                left_low,
                left_high,
                right_low,
                right_high,
            } => {
                tag.insert("leftLow", flag(left_low));
                tag.insert("leftHigh", flag(left_high));
                tag.insert("rightLow", flag(right_low));
                tag.insert("rightHigh", flag(right_high));
            }
            StrongholdKind::Library { is_tall } => tag.insert("Tall", flag(is_tall)),
            StrongholdKind::PortalRoom { has_placed_spawner } => {
                tag.insert("Mob", flag(has_placed_spawner));
            }
            StrongholdKind::RoomCrossing { room_type } => tag.insert("Type", Tag::Int(room_type)),
            StrongholdKind::StairsDown { is_source } => tag.insert("Source", flag(is_source)),
            StrongholdKind::Straight {
                left_child,
                right_child,
            } => {
                tag.insert("Left", flag(left_child));
                tag.insert("Right", flag(right_child));
            }
            StrongholdKind::LeftTurn
            | StrongholdKind::RightTurn
            | StrongholdKind::PrisonHall
            | StrongholdKind::StraightStairsDown => {}
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
        let bb: &BoundingBox = chunk_bb;
        match self.kind {
            StrongholdKind::ChestCorridor { .. } => {
                self.post_process_chest_corridor(level, bb, random);
            }
            StrongholdKind::FillerCorridor { steps } => {
                self.post_process_filler_corridor(level, bb, steps);
            }
            StrongholdKind::FiveCrossing {
                left_low,
                left_high,
                right_low,
                right_high,
            } => self.post_process_five_crossing(
                level,
                bb,
                random,
                [left_low, left_high, right_low, right_high],
            ),
            StrongholdKind::LeftTurn => self.post_process_turn(level, bb, random, true),
            StrongholdKind::RightTurn => self.post_process_turn(level, bb, random, false),
            StrongholdKind::Library { is_tall } => {
                self.post_process_library(level, bb, random, is_tall);
            }
            StrongholdKind::PortalRoom { .. } => self.post_process_portal_room(level, bb, random),
            StrongholdKind::PrisonHall => self.post_process_prison_hall(level, bb, random),
            StrongholdKind::RoomCrossing { room_type } => {
                self.post_process_room_crossing(level, bb, random, room_type);
            }
            StrongholdKind::StairsDown { .. } => self.post_process_stairs_down(level, bb, random),
            StrongholdKind::Straight {
                left_child,
                right_child,
            } => self.post_process_straight(level, bb, random, left_child, right_child),
            StrongholdKind::StraightStairsDown => {
                self.post_process_straight_stairs_down(level, bb, random);
            }
        }
    }
}

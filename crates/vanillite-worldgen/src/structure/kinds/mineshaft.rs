//! Mineshafts (`MineshaftStructure`, `MineshaftPieces`), normal and mesa.
//!
//! The pieces are generated recursively from a central room (`addChildren`) with the legacy
//! structure random, then moved below the sea level (normal) or towards the surface (mesa).
//! Chest minecarts are entities and are not spawned; their random draws (rail shape, loot
//! seed) are kept.

use std::sync::Arc;

use serde_json::Value;
use vanillite_base::random::{DecorationRandom, LegacyRandom, RandomSource};
use vanillite_base::{BlockPos, ChunkPos, Direction};
use vanillite_chunk::HeightmapKind;
use vanillite_data::block::{BlockState, ids};
use vanillite_nbt::{Compound, List, Tag};

use super::legacy::{is_falling_block, place_spawner, with_props};
use crate::Error;
use crate::behaviour::{self, SupportType};
use crate::feature::field;
use crate::feature::region::{WorldGenRegion, entity_tag};
use crate::structure::piece::{PieceContext, data_2d, with_facing};
use crate::structure::{
    BiomeFilter, GenerationContext, GenerationStub, PieceBase, StructureParser, StructurePiece,
    StructurePiecesBuilder, StructureType,
};
use crate::template::BoundingBox;

/// `MAX_DEPTH`: pieces deeper than this get no children.
const MAX_DEPTH: i32 = 8;
/// `BiomeTags.MINESHAFT_BLOCKING`.
const BLOCKING_TAG: &str = "#minecraft:mineshaft_blocking";

/// `MineshaftStructure.Type`: the wood of the supports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MineshaftType {
    /// `normal`: oak.
    Normal,
    /// `mesa`: dark oak, generated towards the surface.
    Mesa,
}

impl MineshaftType {
    /// `getWoodState()`.
    pub fn wood(self) -> BlockState {
        match self {
            Self::Normal => ids::OAK_LOG.default_state(),
            Self::Mesa => ids::DARK_OAK_LOG.default_state(),
        }
    }

    /// `getPlanksState()`.
    pub fn planks(self) -> BlockState {
        match self {
            Self::Normal => ids::OAK_PLANKS.default_state(),
            Self::Mesa => ids::DARK_OAK_PLANKS.default_state(),
        }
    }

    /// `getFenceState()`.
    pub fn fence(self) -> BlockState {
        match self {
            Self::Normal => ids::OAK_FENCE.default_state(),
            Self::Mesa => ids::DARK_OAK_FENCE.default_state(),
        }
    }

    /// `ordinal()` (the saved `MST`).
    pub fn ordinal(self) -> i32 {
        match self {
            Self::Normal => 0,
            Self::Mesa => 1,
        }
    }
}

/// `MineshaftStructure`.
#[derive(Debug)]
pub struct MineshaftStructure {
    mineshaft_type: MineshaftType,
    /// `#minecraft:mineshaft_blocking`: pieces in these biomes are not placed.
    blocking: Arc<BiomeFilter>,
}

impl StructureType for MineshaftStructure {
    fn parse(json: &Value, parser: &mut StructureParser<'_>) -> Result<Self, Error> {
        let mineshaft_type = match field(json, "mineshaft_type")?.as_str() {
            Some("normal") => MineshaftType::Normal,
            Some("mesa") => MineshaftType::Mesa,
            other => return Err(Error::new(format!("unknown mineshaft type {other:?}"))),
        };
        let blocking = parser.biome_filter(&Value::String(BLOCKING_TAG.to_owned()))?;
        Ok(Self {
            mineshaft_type,
            blocking: Arc::new(blocking),
        })
    }

    fn find_generation_point<'s>(
        &'s self,
        ctx: &mut GenerationContext<'_>,
    ) -> Option<GenerationStub<'s>> {
        ctx.random.random.next_double();
        let chunk = ctx.chunk_pos;
        let start = BlockPos::new(chunk.min_block_x() + 8, 50, chunk.min_block_z());
        let mut builder = StructurePiecesBuilder::new();
        let dy = self.generate_pieces_and_adjust(&mut builder, ctx);
        Some(GenerationStub::built(start.offset(0, dy, 0), builder))
    }
}

impl MineshaftStructure {
    /// `generatePiecesAndAdjust(builder, context)`: the pieces and the vertical offset applied
    /// to them.
    fn generate_pieces_and_adjust(
        &self,
        builder: &mut StructurePiecesBuilder,
        ctx: &mut GenerationContext<'_>,
    ) -> i32 {
        let chunk = ctx.chunk_pos;
        let random = &mut ctx.random.random;
        let room = MineshaftPiece::room(
            random,
            chunk.min_block_x() + 2,
            chunk.min_block_z() + 2,
            self.mineshaft_type,
            &self.blocking,
        );
        let room_bb = room.base.bounding_box;
        builder.add_piece(Box::new(room));
        let entrances = Generator {
            builder: &mut *builder,
            random: &mut *random,
            start_min_x: room_bb.min_x(),
            start_min_z: room_bb.min_z(),
            mineshaft_type: self.mineshaft_type,
            blocking: &self.blocking,
        }
        .room_children(&room_bb, 0);
        if let Some(MineshaftKind::Room { entrances: e }) = builder.pieces[0]
            .downcast_mut::<MineshaftPiece>()
            .map(|p| &mut p.kind)
        {
            *e = entrances;
        }
        let generator = ctx.generator;
        let sea_level = generator.sea_level();
        if self.mineshaft_type == MineshaftType::Mesa {
            let center = builder.bounding_box().center();
            let surface = generator.base_height(
                center.x,
                center.z,
                HeightmapKind::WorldSurfaceWg,
                ctx.height,
            );
            let target = if surface <= sea_level {
                sea_level
            } else {
                ctx.random
                    .random
                    .next_int_between_inclusive(sea_level, surface)
            };
            let dy = target - center.y;
            builder.offset_pieces_vertically(dy);
            dy
        } else {
            builder.move_below_sea_level(sea_level, generator.min_y(), &mut ctx.random.random, 10)
        }
    }
}

/// The child generation of one start (`MineshaftPieces.generateAndAddPiece` and the
/// `addChildren` of each piece).
struct Generator<'g> {
    builder: &'g mut StructurePiecesBuilder,
    random: &'g mut LegacyRandom,
    /// The start room's minimum x and z.
    start_min_x: i32,
    start_min_z: i32,
    mineshaft_type: MineshaftType,
    blocking: &'g Arc<BiomeFilter>,
}

impl Generator<'_> {
    /// `createRandomShaftPiece(builder, random, footX, footY, footZ, direction, genDepth, type)`.
    fn create_random_shaft_piece(
        &mut self,
        foot: BlockPos,
        direction: Direction,
        gen_depth: i32,
    ) -> Option<MineshaftPiece> {
        let selection = self.random.next_int_bounded(100);
        let (bb, kind) = if selection >= 80 {
            let bb = find_crossing(self.builder, self.random, foot, direction)?;
            let kind = MineshaftKind::Crossing {
                direction,
                two_floored: bb.y_span() > 3,
            };
            (bb, kind)
        } else if selection >= 70 {
            (
                find_stairs(self.builder, foot, direction)?,
                MineshaftKind::Stairs,
            )
        } else {
            let bb = find_corridor_size(self.builder, self.random, foot, direction)?;
            let has_rails = self.random.next_int_bounded(3) == 0;
            let spider_corridor = !has_rails && self.random.next_int_bounded(23) == 0;
            let num_sections = match direction {
                Direction::North | Direction::South => bb.z_span() / 5,
                _ => bb.x_span() / 5,
            };
            let kind = MineshaftKind::Corridor {
                has_rails,
                spider_corridor,
                has_placed_spider: false,
                num_sections,
            };
            (bb, kind)
        };
        let mut base = PieceBase::new(kind.piece_type(), gen_depth, bb);
        if !matches!(kind, MineshaftKind::Crossing { .. }) {
            base.set_orientation(Some(direction));
        }
        Some(MineshaftPiece {
            base,
            mineshaft_type: self.mineshaft_type,
            blocking: Arc::clone(self.blocking),
            kind,
        })
    }

    /// `generateAndAddPiece(startPiece, builder, random, footX, footY, footZ, direction,
    /// depth)`: the new piece's box.
    fn generate_and_add_piece(
        &mut self,
        x: i32,
        y: i32,
        z: i32,
        direction: Direction,
        depth: i32,
    ) -> Option<BoundingBox> {
        if depth > MAX_DEPTH
            || (x - self.start_min_x).abs() > 80
            || (z - self.start_min_z).abs() > 80
        {
            return None;
        }
        let piece = self.create_random_shaft_piece(BlockPos::new(x, y, z), direction, depth + 1)?;
        let bb = piece.base.bounding_box;
        let gen_depth = piece.base.gen_depth;
        let orientation = piece.base.orientation();
        let kind = piece.kind.clone();
        self.builder.add_piece(Box::new(piece));
        match kind {
            MineshaftKind::Corridor { .. } => self.corridor_children(&bb, orientation, gen_depth),
            MineshaftKind::Crossing {
                direction,
                two_floored,
            } => self.crossing_children(&bb, direction, two_floored, gen_depth),
            MineshaftKind::Stairs => self.stairs_children(&bb, orientation, gen_depth),
            MineshaftKind::Room { .. } => {
                self.room_children(&bb, gen_depth);
            }
        }
        Some(bb)
    }

    /// `MineShaftRoom.addChildren`: returns the `childEntranceBoxes`.
    fn room_children(&mut self, bb: &BoundingBox, depth: i32) -> Vec<BoundingBox> {
        let mut entrances = Vec::new();
        let height_space = (bb.y_span() - 3 - 1).max(1);
        // North side.
        let mut pos = 0;
        while pos < bb.x_span() {
            pos += self.random.next_int_bounded(bb.x_span());
            if pos + 3 > bb.x_span() {
                break;
            }
            let y = bb.min_y() + self.random.next_int_bounded(height_space) + 1;
            if let Some(child) = self.generate_and_add_piece(
                bb.min_x() + pos,
                y,
                bb.min_z() - 1,
                Direction::North,
                depth,
            ) {
                entrances.push(BoundingBox::new(
                    child.min_x(),
                    child.min_y(),
                    bb.min_z(),
                    child.max_x(),
                    child.max_y(),
                    bb.min_z() + 1,
                ));
            }
            pos += 4;
        }
        // South side.
        pos = 0;
        while pos < bb.x_span() {
            pos += self.random.next_int_bounded(bb.x_span());
            if pos + 3 > bb.x_span() {
                break;
            }
            let y = bb.min_y() + self.random.next_int_bounded(height_space) + 1;
            if let Some(child) = self.generate_and_add_piece(
                bb.min_x() + pos,
                y,
                bb.max_z() + 1,
                Direction::South,
                depth,
            ) {
                entrances.push(BoundingBox::new(
                    child.min_x(),
                    child.min_y(),
                    bb.max_z() - 1,
                    child.max_x(),
                    child.max_y(),
                    bb.max_z(),
                ));
            }
            pos += 4;
        }
        // West side.
        pos = 0;
        while pos < bb.z_span() {
            pos += self.random.next_int_bounded(bb.z_span());
            if pos + 3 > bb.z_span() {
                break;
            }
            let y = bb.min_y() + self.random.next_int_bounded(height_space) + 1;
            if let Some(child) = self.generate_and_add_piece(
                bb.min_x() - 1,
                y,
                bb.min_z() + pos,
                Direction::West,
                depth,
            ) {
                entrances.push(BoundingBox::new(
                    bb.min_x(),
                    child.min_y(),
                    child.min_z(),
                    bb.min_x() + 1,
                    child.max_y(),
                    child.max_z(),
                ));
            }
            pos += 4;
        }
        // East side.
        pos = 0;
        while pos < bb.z_span() {
            pos += self.random.next_int_bounded(bb.z_span());
            if pos + 3 > bb.z_span() {
                break;
            }
            let y = bb.min_y() + self.random.next_int_bounded(height_space) + 1;
            if let Some(child) = self.generate_and_add_piece(
                bb.max_x() + 1,
                y,
                bb.min_z() + pos,
                Direction::East,
                depth,
            ) {
                entrances.push(BoundingBox::new(
                    bb.max_x() - 1,
                    child.min_y(),
                    child.min_z(),
                    bb.max_x(),
                    child.max_y(),
                    child.max_z(),
                ));
            }
            pos += 4;
        }
        entrances
    }

    /// `MineShaftCorridor.addChildren`.
    fn corridor_children(&mut self, bb: &BoundingBox, orientation: Option<Direction>, depth: i32) {
        let end_selection = self.random.next_int_bounded(4);
        if let Some(orientation) = orientation {
            // The end of the corridor: straight on (0, 1) or to either side (2, 3).
            let (x, z, direction) = match orientation {
                Direction::South => match end_selection {
                    0 | 1 => (bb.min_x(), bb.max_z() + 1, orientation),
                    2 => (bb.min_x() - 1, bb.max_z() - 3, Direction::West),
                    _ => (bb.max_x() + 1, bb.max_z() - 3, Direction::East),
                },
                Direction::West => match end_selection {
                    0 | 1 => (bb.min_x() - 1, bb.min_z(), orientation),
                    2 => (bb.min_x(), bb.min_z() - 1, Direction::North),
                    _ => (bb.min_x(), bb.max_z() + 1, Direction::South),
                },
                Direction::East => match end_selection {
                    0 | 1 => (bb.max_x() + 1, bb.min_z(), orientation),
                    2 => (bb.max_x() - 3, bb.min_z() - 1, Direction::North),
                    _ => (bb.max_x() - 3, bb.max_z() + 1, Direction::South),
                },
                _ => match end_selection {
                    0 | 1 => (bb.min_x(), bb.min_z() - 1, orientation),
                    2 => (bb.min_x() - 1, bb.min_z(), Direction::West),
                    _ => (bb.max_x() + 1, bb.min_z(), Direction::East),
                },
            };
            let y = bb.min_y() - 1 + self.random.next_int_bounded(3);
            self.generate_and_add_piece(x, y, z, direction, depth);
        }
        if depth < MAX_DEPTH {
            if orientation != Some(Direction::North) && orientation != Some(Direction::South) {
                let mut x = bb.min_x() + 3;
                while x + 3 <= bb.max_x() {
                    match self.random.next_int_bounded(5) {
                        0 => {
                            self.generate_and_add_piece(
                                x,
                                bb.min_y(),
                                bb.min_z() - 1,
                                Direction::North,
                                depth + 1,
                            );
                        }
                        1 => {
                            self.generate_and_add_piece(
                                x,
                                bb.min_y(),
                                bb.max_z() + 1,
                                Direction::South,
                                depth + 1,
                            );
                        }
                        _ => {}
                    }
                    x += 5;
                }
            } else {
                let mut z = bb.min_z() + 3;
                while z + 3 <= bb.max_z() {
                    match self.random.next_int_bounded(5) {
                        0 => {
                            self.generate_and_add_piece(
                                bb.min_x() - 1,
                                bb.min_y(),
                                z,
                                Direction::West,
                                depth + 1,
                            );
                        }
                        1 => {
                            self.generate_and_add_piece(
                                bb.max_x() + 1,
                                bb.min_y(),
                                z,
                                Direction::East,
                                depth + 1,
                            );
                        }
                        _ => {}
                    }
                    z += 5;
                }
            }
        }
    }

    /// `MineShaftCrossing.addChildren`.
    fn crossing_children(
        &mut self,
        bb: &BoundingBox,
        direction: Direction,
        two_floored: bool,
        depth: i32,
    ) {
        let (min_x, min_y, min_z, max_x, max_z) =
            (bb.min_x(), bb.min_y(), bb.min_z(), bb.max_x(), bb.max_z());
        let north = (min_x + 1, min_y, min_z - 1, Direction::North);
        let south = (min_x + 1, min_y, max_z + 1, Direction::South);
        let west = (min_x - 1, min_y, min_z + 1, Direction::West);
        let east = (max_x + 1, min_y, min_z + 1, Direction::East);
        let exits = match direction {
            Direction::South => [south, west, east],
            Direction::West => [north, south, west],
            Direction::East => [north, south, east],
            _ => [north, west, east],
        };
        for (x, y, z, d) in exits {
            self.generate_and_add_piece(x, y, z, d, depth);
        }
        if two_floored {
            let y = min_y + 3 + 1;
            if self.random.next_boolean() {
                self.generate_and_add_piece(min_x + 1, y, min_z - 1, Direction::North, depth);
            }
            if self.random.next_boolean() {
                self.generate_and_add_piece(min_x - 1, y, min_z + 1, Direction::West, depth);
            }
            if self.random.next_boolean() {
                self.generate_and_add_piece(max_x + 1, y, min_z + 1, Direction::East, depth);
            }
            if self.random.next_boolean() {
                self.generate_and_add_piece(min_x + 1, y, max_z + 1, Direction::South, depth);
            }
        }
    }

    /// `MineShaftStairs.addChildren`.
    fn stairs_children(&mut self, bb: &BoundingBox, orientation: Option<Direction>, depth: i32) {
        let Some(orientation) = orientation else {
            return;
        };
        let (x, z) = match orientation {
            Direction::South => (bb.min_x(), bb.max_z() + 1),
            Direction::West => (bb.min_x() - 1, bb.min_z()),
            Direction::East => (bb.max_x() + 1, bb.min_z()),
            _ => (bb.min_x(), bb.min_z() - 1),
        };
        self.generate_and_add_piece(x, bb.min_y(), z, orientation, depth);
    }
}

/// `MineShaftCorridor.findCorridorSize(builder, random, footX, footY, footZ, direction)`.
fn find_corridor_size(
    builder: &StructurePiecesBuilder,
    random: &mut LegacyRandom,
    foot: BlockPos,
    direction: Direction,
) -> Option<BoundingBox> {
    let mut length = random.next_int_bounded(3) + 2;
    while length > 0 {
        let blocks = length * 5;
        let bb = match direction {
            Direction::South => BoundingBox::new(0, 0, 0, 2, 2, blocks - 1),
            Direction::West => BoundingBox::new(-(blocks - 1), 0, 0, 0, 2, 2),
            Direction::East => BoundingBox::new(0, 0, 0, blocks - 1, 2, 2),
            _ => BoundingBox::new(0, 0, -(blocks - 1), 2, 2, 0),
        }
        .moved(foot.x, foot.y, foot.z);
        if builder.find_collision_piece(&bb).is_none() {
            return Some(bb);
        }
        length -= 1;
    }
    None
}

/// `MineShaftCrossing.findCrossing(builder, random, footX, footY, footZ, direction)`.
fn find_crossing(
    builder: &StructurePiecesBuilder,
    random: &mut LegacyRandom,
    foot: BlockPos,
    direction: Direction,
) -> Option<BoundingBox> {
    let y1 = if random.next_int_bounded(4) == 0 {
        6
    } else {
        2
    };
    let bb = match direction {
        Direction::South => BoundingBox::new(-1, 0, 0, 3, y1, 4),
        Direction::West => BoundingBox::new(-4, 0, -1, 0, y1, 3),
        Direction::East => BoundingBox::new(0, 0, -1, 4, y1, 3),
        _ => BoundingBox::new(-1, 0, -4, 3, y1, 0),
    }
    .moved(foot.x, foot.y, foot.z);
    builder.find_collision_piece(&bb).is_none().then_some(bb)
}

/// `MineShaftStairs.findStairs(builder, random, footX, footY, footZ, direction)`.
fn find_stairs(
    builder: &StructurePiecesBuilder,
    foot: BlockPos,
    direction: Direction,
) -> Option<BoundingBox> {
    let bb = match direction {
        Direction::South => BoundingBox::new(0, -5, 0, 2, 2, 8),
        Direction::West => BoundingBox::new(-8, -5, 0, 0, 2, 2),
        Direction::East => BoundingBox::new(0, -5, 0, 8, 2, 2),
        _ => BoundingBox::new(0, -5, -8, 2, 2, 0),
    }
    .moved(foot.x, foot.y, foot.z);
    builder.find_collision_piece(&bb).is_none().then_some(bb)
}

/// The concrete piece class and its own state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MineshaftKind {
    /// `MineShaftRoom` (`minecraft:msroom`): the start.
    Room {
        /// `childEntranceBoxes`: openings to the children.
        entrances: Vec<BoundingBox>,
    },
    /// `MineShaftCorridor` (`minecraft:mscorridor`).
    Corridor {
        /// `hasRails`.
        has_rails: bool,
        /// `spiderCorridor`: cobwebs and a cave spider spawner.
        spider_corridor: bool,
        /// `hasPlacedSpider`.
        has_placed_spider: bool,
        /// `numSections`: 5 blocks each.
        num_sections: i32,
    },
    /// `MineShaftCrossing` (`minecraft:mscrossing`): no orientation.
    Crossing {
        /// `direction`.
        direction: Direction,
        /// `isTwoFloored`.
        two_floored: bool,
    },
    /// `MineShaftStairs` (`minecraft:msstairs`).
    Stairs,
}

impl MineshaftKind {
    /// The `StructurePieceType` id.
    fn piece_type(&self) -> &'static str {
        match self {
            Self::Room { .. } => "minecraft:msroom",
            Self::Corridor { .. } => "minecraft:mscorridor",
            Self::Crossing { .. } => "minecraft:mscrossing",
            Self::Stairs => "minecraft:msstairs",
        }
    }
}

/// A mineshaft piece (`MineshaftPieces.MineShaftPiece` and its subclasses).
#[derive(Clone, Debug)]
pub struct MineshaftPiece {
    pub base: PieceBase,
    /// `type`.
    pub mineshaft_type: MineshaftType,
    /// `#minecraft:mineshaft_blocking`.
    blocking: Arc<BiomeFilter>,
    pub kind: MineshaftKind,
}

impl MineshaftPiece {
    /// `new MineShaftRoom(0, random, west, north, type)`.
    fn room(
        random: &mut LegacyRandom,
        west: i32,
        north: i32,
        mineshaft_type: MineshaftType,
        blocking: &Arc<BiomeFilter>,
    ) -> Self {
        let max_x = west + 7 + random.next_int_bounded(6);
        let max_y = 54 + random.next_int_bounded(6);
        let max_z = north + 7 + random.next_int_bounded(6);
        let bb = BoundingBox::new(west, 50, north, max_x, max_y, max_z);
        Self {
            base: PieceBase::new("minecraft:msroom", 0, bb),
            mineshaft_type,
            blocking: Arc::clone(blocking),
            kind: MineshaftKind::Room {
                entrances: Vec::new(),
            },
        }
    }

    /// `isSupportingBox(level, chunkBB, x0, x1, y1, z0)`.
    fn is_supporting_box(
        &self,
        level: &WorldGenRegion<'_>,
        chunk_bb: &BoundingBox,
        x0: i32,
        x1: i32,
        y1: i32,
        z0: i32,
    ) -> bool {
        (x0..=x1).all(|x| !self.get_block(level, x, y1 + 1, z0, chunk_bb).is_air())
    }

    /// `isInInvalidLocation(level, chunkBB)`: in a blocking biome or next to a liquid.
    fn is_in_invalid_location(&self, level: &WorldGenRegion<'_>, chunk_bb: &BoundingBox) -> bool {
        let bb = &self.base.bounding_box;
        let x0 = (bb.min_x() - 1).max(chunk_bb.min_x());
        let y0 = (bb.min_y() - 1).max(chunk_bb.min_y());
        let z0 = (bb.min_z() - 1).max(chunk_bb.min_z());
        let x1 = (bb.max_x() + 1).min(chunk_bb.max_x());
        let y1 = (bb.max_y() + 1).min(chunk_bb.max_y());
        let z1 = (bb.max_z() + 1).min(chunk_bb.max_z());
        let center = BlockPos::new((x0 + x1) / 2, (y0 + y1) / 2, (z0 + z1) / 2);
        if self.blocking.contains(level.biome(center)) {
            return true;
        }
        let liquid = |x, y, z| {
            level
                .block(BlockPos::new(x, y, z))
                .has(vanillite_data::block::flags::LIQUID)
        };
        for x in x0..=x1 {
            for z in z0..=z1 {
                if liquid(x, y0, z) || liquid(x, y1, z) {
                    return true;
                }
            }
        }
        for x in x0..=x1 {
            for y in y0..=y1 {
                if liquid(x, y, z0) || liquid(x, y, z1) {
                    return true;
                }
            }
        }
        for z in z0..=z1 {
            for y in y0..=y1 {
                if liquid(x0, y, z) || liquid(x1, y, z) {
                    return true;
                }
            }
        }
        false
    }

    /// `setPlanksBlock(level, chunkBB, planksBlock, x, y, z)`: planks under the floor where
    /// the ground is not sturdy.
    #[allow(clippy::too_many_arguments)]
    fn set_planks_block(
        &self,
        level: &mut WorldGenRegion<'_>,
        chunk_bb: &BoundingBox,
        planks: BlockState,
        x: i32,
        y: i32,
        z: i32,
    ) {
        if self.is_interior(level, x, y, z, chunk_bb) {
            let pos = self.world_pos(x, y, z);
            if !behaviour::is_face_sturdy(level.block(pos), Direction::Up, SupportType::Full) {
                level.set_block(pos, planks, 2);
            }
        }
    }

    fn post_process_room(&self, level: &mut WorldGenRegion<'_>, bb: &BoundingBox) {
        let MineshaftKind::Room { entrances } = &self.kind else {
            return;
        };
        let air = ids::CAVE_AIR.default_state();
        let me = self.base.bounding_box;
        self.generate_box(
            level,
            bb,
            me.min_x(),
            me.min_y() + 1,
            me.min_z(),
            me.max_x(),
            (me.min_y() + 3).min(me.max_y()),
            me.max_z(),
            air,
            air,
            false,
        );
        for entrance in entrances {
            self.generate_box(
                level,
                bb,
                entrance.min_x(),
                entrance.max_y() - 2,
                entrance.min_z(),
                entrance.max_x(),
                entrance.max_y(),
                entrance.max_z(),
                air,
                air,
                false,
            );
        }
        self.generate_upper_half_sphere(
            level,
            bb,
            me.min_x(),
            me.min_y() + 4,
            me.min_z(),
            me.max_x(),
            me.max_y(),
            me.max_z(),
            air,
            false,
        );
    }

    fn post_process_crossing(&self, level: &mut WorldGenRegion<'_>, bb: &BoundingBox) {
        let MineshaftKind::Crossing { two_floored, .. } = self.kind else {
            return;
        };
        let air = ids::CAVE_AIR.default_state();
        let planks = self.mineshaft_type.planks();
        let me = self.base.bounding_box;
        let (x0, y0, z0, x1, y1, z1) = (
            me.min_x(),
            me.min_y(),
            me.min_z(),
            me.max_x(),
            me.max_y(),
            me.max_z(),
        );
        if two_floored {
            self.generate_box(
                level,
                bb,
                x0 + 1,
                y0,
                z0,
                x1 - 1,
                y0 + 3 - 1,
                z1,
                air,
                air,
                false,
            );
            self.generate_box(
                level,
                bb,
                x0,
                y0,
                z0 + 1,
                x1,
                y0 + 3 - 1,
                z1 - 1,
                air,
                air,
                false,
            );
            self.generate_box(
                level,
                bb,
                x0 + 1,
                y1 - 2,
                z0,
                x1 - 1,
                y1,
                z1,
                air,
                air,
                false,
            );
            self.generate_box(
                level,
                bb,
                x0,
                y1 - 2,
                z0 + 1,
                x1,
                y1,
                z1 - 1,
                air,
                air,
                false,
            );
            self.generate_box(
                level,
                bb,
                x0 + 1,
                y0 + 3,
                z0 + 1,
                x1 - 1,
                y0 + 3,
                z1 - 1,
                air,
                air,
                false,
            );
        } else {
            self.generate_box(level, bb, x0 + 1, y0, z0, x1 - 1, y1, z1, air, air, false);
            self.generate_box(level, bb, x0, y0, z0 + 1, x1, y1, z1 - 1, air, air, false);
        }
        self.place_support_pillar(level, bb, x0 + 1, y0, z0 + 1, y1);
        self.place_support_pillar(level, bb, x0 + 1, y0, z1 - 1, y1);
        self.place_support_pillar(level, bb, x1 - 1, y0, z0 + 1, y1);
        self.place_support_pillar(level, bb, x1 - 1, y0, z1 - 1, y1);
        let y = y0 - 1;
        for x in x0..=x1 {
            for z in z0..=z1 {
                self.set_planks_block(level, bb, planks, x, y, z);
            }
        }
    }

    /// `MineShaftCrossing.placeSupportPillar(level, chunkBB, x, y0, z, y1)`.
    fn place_support_pillar(
        &self,
        level: &mut WorldGenRegion<'_>,
        bb: &BoundingBox,
        x: i32,
        y0: i32,
        z: i32,
        y1: i32,
    ) {
        if !self.get_block(level, x, y1 + 1, z, bb).is_air() {
            let planks = self.mineshaft_type.planks();
            let air = ids::CAVE_AIR.default_state();
            self.generate_box(level, bb, x, y0, z, x, y1, z, planks, air, false);
        }
    }

    fn post_process_stairs(&self, level: &mut WorldGenRegion<'_>, bb: &BoundingBox) {
        let air = ids::CAVE_AIR.default_state();
        self.generate_box(level, bb, 0, 5, 0, 2, 7, 1, air, air, false);
        self.generate_box(level, bb, 0, 0, 7, 2, 2, 8, air, air, false);
        for i in 0..5 {
            let y0 = 5 - i - i32::from(i < 4);
            self.generate_box(level, bb, 0, y0, 2 + i, 2, 7 - i, 2 + i, air, air, false);
        }
    }

    /// `MineShaftCorridor.postProcess`; returns the new `hasPlacedSpider`.
    fn post_process_corridor(
        &self,
        level: &mut WorldGenRegion<'_>,
        random: &mut DecorationRandom,
        bb: &BoundingBox,
    ) -> bool {
        let MineshaftKind::Corridor {
            has_rails,
            spider_corridor,
            mut has_placed_spider,
            num_sections,
        } = self.kind
        else {
            return false;
        };
        let air = ids::CAVE_AIR.default_state();
        let length = num_sections * 5 - 1;
        let planks = self.mineshaft_type.planks();
        self.generate_box(level, bb, 0, 0, 0, 2, 1, length, air, air, false);
        self.generate_maybe_box(
            level, bb, random, 0.8, 0, 2, 0, 2, 2, length, air, air, false, false,
        );
        if spider_corridor {
            let cobweb = ids::COBWEB.default_state();
            self.generate_maybe_box(
                level, bb, random, 0.6, 0, 0, 0, 2, 1, length, cobweb, air, false, true,
            );
        }
        for section in 0..num_sections {
            let z = 2 + section * 5;
            self.place_support(level, bb, 0, 0, z, 2, 2, random);
            self.maybe_place_cobweb(level, bb, random, 0.1, 0, 2, z - 1);
            self.maybe_place_cobweb(level, bb, random, 0.1, 2, 2, z - 1);
            self.maybe_place_cobweb(level, bb, random, 0.1, 0, 2, z + 1);
            self.maybe_place_cobweb(level, bb, random, 0.1, 2, 2, z + 1);
            self.maybe_place_cobweb(level, bb, random, 0.05, 0, 2, z - 2);
            self.maybe_place_cobweb(level, bb, random, 0.05, 2, 2, z - 2);
            self.maybe_place_cobweb(level, bb, random, 0.05, 0, 2, z + 2);
            self.maybe_place_cobweb(level, bb, random, 0.05, 2, 2, z + 2);
            if random.next_int_bounded(100) == 0 {
                self.create_minecart_chest(level, bb, random, 2, 0, z - 1);
            }
            if random.next_int_bounded(100) == 0 {
                self.create_minecart_chest(level, bb, random, 0, 0, z + 1);
            }
            if spider_corridor && !has_placed_spider {
                let new_z = z - 1 + random.next_int_bounded(3);
                let pos = self.world_pos(1, 0, new_z);
                if bb.is_inside(pos) && self.is_interior(level, 1, 0, new_z, bb) {
                    has_placed_spider = true;
                    place_spawner(level, pos, "minecraft:cave_spider");
                }
            }
        }
        for x in 0..=2 {
            for z in 0..=length {
                self.set_planks_block(level, bb, planks, x, -1, z);
            }
        }
        self.place_double_lower_or_upper_support(level, bb, 0, -1, 2);
        if num_sections > 1 {
            self.place_double_lower_or_upper_support(level, bb, 0, -1, length - 2);
        }
        if has_rails {
            let rail = with_props(ids::RAIL.default_state(), &[("shape", "north_south")]);
            for z in 0..=length {
                let floor = self.get_block(level, 1, -1, z, bb);
                if !floor.is_air() && behaviour::is_solid_render(floor) {
                    let probability = if self.is_interior(level, 1, 0, z, bb) {
                        0.7
                    } else {
                        0.9
                    };
                    self.maybe_generate_block(level, bb, random, probability, 1, 0, z, rail);
                }
            }
        }
        has_placed_spider
    }

    /// `MineShaftCorridor.createChest`: a rail with a chest minecart on it. The minecart is
    /// an entity and is not spawned; its loot seed is still drawn.
    fn create_minecart_chest(
        &self,
        level: &mut WorldGenRegion<'_>,
        bb: &BoundingBox,
        random: &mut DecorationRandom,
        x: i32,
        y: i32,
        z: i32,
    ) -> bool {
        let pos = self.world_pos(x, y, z);
        if !bb.is_inside(pos)
            || !level.block(pos).is_air()
            || level.block(pos.relative(Direction::Down)).is_air()
        {
            return false;
        }
        let shape = if random.next_boolean() {
            "north_south"
        } else {
            "east_west"
        };
        let rail = with_props(ids::RAIL.default_state(), &[("shape", shape)]);
        self.place_block(level, rail, x, y, z, bb);
        // The chest minecart with `setLootTable(ABANDONED_MINESHAFT, random.nextLong())`.
        let seed = random.next_long();
        let (cx, cy, cz) = (
            f64::from(pos.x) + 0.5,
            f64::from(pos.y) + 0.5,
            f64::from(pos.z) + 0.5,
        );
        let mut chest = entity_tag("minecraft:chest_minecart", cx, cy, cz);
        chest.insert("LootTable", "minecraft:chests/abandoned_mineshaft");
        if seed != 0 {
            chest.insert("LootTableSeed", seed);
        }
        level.add_fresh_entity(chest);
        true
    }

    /// `placeDoubleLowerOrUpperSupport(level, chunkBB, x, y, z)`.
    fn place_double_lower_or_upper_support(
        &self,
        level: &mut WorldGenRegion<'_>,
        bb: &BoundingBox,
        x: i32,
        y: i32,
        z: i32,
    ) {
        let wood = self.mineshaft_type.wood();
        let planks = self.mineshaft_type.planks().block();
        if self.get_block(level, x, y, z, bb).block() == planks {
            self.fill_pillar_down_or_chain_up(level, wood, x, y, z, bb);
        }
        if self.get_block(level, x + 2, y, z, bb).block() == planks {
            self.fill_pillar_down_or_chain_up(level, wood, x + 2, y, z, bb);
        }
    }

    /// `fillPillarDownOrChainUp(level, pillarState, x, y, z, chunkBB)`: a pillar down to
    /// sturdy ground or a fence and chain up to a ceiling, whichever is found first.
    fn fill_pillar_down_or_chain_up(
        &self,
        level: &mut WorldGenRegion<'_>,
        pillar: BlockState,
        x: i32,
        y: i32,
        z: i32,
        bb: &BoundingBox,
    ) {
        let origin = self.world_pos(x, y, z);
        if !bb.is_inside(origin) {
            return;
        }
        let world_y = origin.y;
        let at = |y| BlockPos::new(origin.x, y, origin.z);
        let mut distance = 1;
        let mut check_below = true;
        let mut check_above = true;
        while check_below || check_above {
            if check_below {
                let pos = at(world_y - distance);
                let below = level.block(pos);
                let empty_below =
                    self.is_replaceable_by_structures(below) && below.block() != ids::LAVA;
                if !empty_below
                    && behaviour::is_face_sturdy(below, Direction::Up, SupportType::Full)
                {
                    fill_column_between(level, pillar, origin, world_y - distance + 1, world_y);
                    return;
                }
                check_below = distance <= 20 && empty_below && pos.y > level.min_y() + 1;
            }
            if check_above {
                let pos = at(world_y + distance);
                let above = level.block(pos);
                let empty_above = self.is_replaceable_by_structures(above);
                if !empty_above
                    && behaviour::can_support_center(level, pos, Direction::Down)
                    && !is_falling_block(above)
                {
                    level.set_block(at(world_y + 1), self.mineshaft_type.fence(), 2);
                    fill_column_between(
                        level,
                        ids::IRON_CHAIN.default_state(),
                        origin,
                        world_y + 2,
                        world_y + distance,
                    );
                    return;
                }
                check_above = distance <= 50 && empty_above && pos.y < level.max_y();
            }
            distance += 1;
        }
    }

    /// `placeSupport(level, chunkBB, x0, y0, z, y1, x1, random)`.
    #[allow(clippy::too_many_arguments)]
    fn place_support(
        &self,
        level: &mut WorldGenRegion<'_>,
        bb: &BoundingBox,
        x0: i32,
        y0: i32,
        z: i32,
        y1: i32,
        x1: i32,
        random: &mut DecorationRandom,
    ) {
        if !self.is_supporting_box(level, bb, x0, x1, y1, z) {
            return;
        }
        let air = ids::CAVE_AIR.default_state();
        let planks = self.mineshaft_type.planks();
        let fence = self.mineshaft_type.fence();
        let west = with_props(fence, &[("west", "true")]);
        let east = with_props(fence, &[("east", "true")]);
        self.generate_box(level, bb, x0, y0, z, x0, y1 - 1, z, west, air, false);
        self.generate_box(level, bb, x1, y0, z, x1, y1 - 1, z, east, air, false);
        if random.next_int_bounded(4) == 0 {
            self.generate_box(level, bb, x0, y1, z, x0, y1, z, planks, air, false);
            self.generate_box(level, bb, x1, y1, z, x1, y1, z, planks, air, false);
        } else {
            self.generate_box(level, bb, x0, y1, z, x1, y1, z, planks, air, false);
            let torch = ids::WALL_TORCH.default_state();
            self.maybe_generate_block(
                level,
                bb,
                random,
                0.05,
                x0 + 1,
                y1,
                z - 1,
                with_facing(torch, Direction::South),
            );
            self.maybe_generate_block(
                level,
                bb,
                random,
                0.05,
                x0 + 1,
                y1,
                z + 1,
                with_facing(torch, Direction::North),
            );
        }
    }

    /// `maybePlaceCobWeb(level, chunkBB, random, probability, x, y, z)`.
    #[allow(clippy::too_many_arguments)]
    fn maybe_place_cobweb(
        &self,
        level: &mut WorldGenRegion<'_>,
        bb: &BoundingBox,
        random: &mut DecorationRandom,
        probability: f32,
        x: i32,
        y: i32,
        z: i32,
    ) {
        if self.is_interior(level, x, y, z, bb)
            && random.next_float() < probability
            && self.has_sturdy_neighbours(level, bb, x, y, z, 2)
        {
            self.place_block(level, ids::COBWEB.default_state(), x, y, z, bb);
        }
    }

    /// `hasSturdyNeighbours(level, chunkBB, x, y, z, count)`.
    fn has_sturdy_neighbours(
        &self,
        level: &WorldGenRegion<'_>,
        bb: &BoundingBox,
        x: i32,
        y: i32,
        z: i32,
        count: i32,
    ) -> bool {
        let pos = self.world_pos(x, y, z);
        let mut sturdy = 0;
        for direction in Direction::ALL {
            let neighbour = pos.relative(direction);
            if bb.is_inside(neighbour)
                && behaviour::is_face_sturdy(
                    level.block(neighbour),
                    direction.opposite(),
                    SupportType::Full,
                )
            {
                sturdy += 1;
                if sturdy >= count {
                    return true;
                }
            }
        }
        false
    }
}

/// `MineShaftCorridor.fillColumnBetween(level, pillarState, pos, bottomInclusive,
/// topExclusive)`.
fn fill_column_between(
    level: &mut WorldGenRegion<'_>,
    state: BlockState,
    column: BlockPos,
    bottom: i32,
    top: i32,
) {
    for y in bottom..top {
        level.set_block(BlockPos::new(column.x, y, column.z), state, 2);
    }
}

impl StructurePiece for MineshaftPiece {
    fn base(&self) -> &PieceBase {
        &self.base
    }

    fn base_mut(&mut self) -> &mut PieceBase {
        &mut self.base
    }

    /// `addAdditionalSaveData`.
    fn additional_save_data(&self) -> Option<Compound> {
        let mut tag = Compound::new();
        tag.insert("MST", Tag::Int(self.mineshaft_type.ordinal()));
        match &self.kind {
            MineshaftKind::Room { entrances } => {
                let list = entrances
                    .iter()
                    .map(|b| {
                        Tag::IntArray(vec![
                            b.min_x(),
                            b.min_y(),
                            b.min_z(),
                            b.max_x(),
                            b.max_y(),
                            b.max_z(),
                        ])
                    })
                    .collect();
                tag.insert("Entrances", Tag::List(List(list)));
            }
            MineshaftKind::Corridor {
                has_rails,
                spider_corridor,
                has_placed_spider,
                num_sections,
            } => {
                tag.insert("hr", Tag::Byte(i8::from(*has_rails)));
                tag.insert("sc", Tag::Byte(i8::from(*spider_corridor)));
                tag.insert("hps", Tag::Byte(i8::from(*has_placed_spider)));
                tag.insert("Num", Tag::Int(*num_sections));
            }
            MineshaftKind::Crossing {
                direction,
                two_floored,
            } => {
                tag.insert("tf", Tag::Byte(i8::from(*two_floored)));
                tag.insert("D", Tag::Int(data_2d(*direction) as i32));
            }
            MineshaftKind::Stairs => {}
        }
        Some(tag)
    }

    /// `MineShaftRoom.move` also moves the entrances.
    fn move_by(&mut self, dx: i32, dy: i32, dz: i32) {
        self.base.bounding_box.translate(dx, dy, dz);
        if let MineshaftKind::Room { entrances } = &mut self.kind {
            for entrance in entrances {
                entrance.translate(dx, dy, dz);
            }
        }
    }

    /// `MineShaftPiece.canBeReplaced`: not the mineshaft's own wood, planks, fences and chains.
    fn can_be_replaced(
        &self,
        level: &WorldGenRegion<'_>,
        x: i32,
        y: i32,
        z: i32,
        chunk_bb: &BoundingBox,
    ) -> bool {
        let block = self.get_block(level, x, y, z, chunk_bb).block();
        let t = self.mineshaft_type;
        block != t.planks().block()
            && block != t.wood().block()
            && block != t.fence().block()
            && block != ids::IRON_CHAIN
    }

    /// `MineShaftCorridor.fillColumnDown`: the column is only filled on sturdy ground.
    fn fill_column_down(
        &self,
        level: &mut WorldGenRegion<'_>,
        state: BlockState,
        x: i32,
        start_y: i32,
        z: i32,
        chunk_bb: &BoundingBox,
    ) {
        let start = self.world_pos(x, start_y, z);
        if !chunk_bb.is_inside(start) {
            return;
        }
        let mut pos = start;
        while self.is_replaceable_by_structures(level.block(pos)) && pos.y > level.min_y() + 1 {
            pos = pos.relative(Direction::Down);
        }
        if behaviour::is_face_sturdy(level.block(pos), Direction::Up, SupportType::Full) {
            while pos.y < start.y {
                pos = pos.relative(Direction::Up);
                level.set_block(pos, state, 2);
            }
        }
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
        if self.is_in_invalid_location(level, bb) {
            return;
        }
        match self.kind {
            MineshaftKind::Room { .. } => self.post_process_room(level, bb),
            MineshaftKind::Crossing { .. } => self.post_process_crossing(level, bb),
            MineshaftKind::Stairs => self.post_process_stairs(level, bb),
            MineshaftKind::Corridor { .. } => {
                let placed = self.post_process_corridor(level, ctx.random, bb);
                if let MineshaftKind::Corridor {
                    has_placed_spider, ..
                } = &mut self.kind
                {
                    *has_placed_spider = placed;
                }
            }
        }
    }
}

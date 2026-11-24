//! `StructurePiece`: one part of a structure start, placed chunk by chunk in the features stage.
//!
//! A piece is a `Box<dyn StructurePiece>`: [`PieceBase`] holds the state of Vanilla's base class
//! (bounding box, orientation and the mirror/rotation derived from it, generation depth), the
//! trait's provided methods are the base class helpers (`placeBlock`, `generateBox`, …) with
//! Vanilla's semantics, and implementations override what Vanilla's subclasses override.

use std::any::Any;
use std::fmt;

use vanillite_base::random::{DecorationRandom, RandomSource};
use vanillite_base::{BlockPos, ChunkPos, Direction};
use vanillite_chunk::HeightmapKind;
use vanillite_data::block::{BlockState, flags, ids};
use vanillite_nbt::{Compound, Tag};

use crate::behaviour;
use crate::feature::kinds::terrain::schedule_fluid_tick;
use crate::feature::region::{WorldGenRegion, block_entity_type};
use crate::generator::TerrainGenerator;
use crate::template::transform::{mirror_state, rotate_state};
use crate::template::{BoundingBox, Mirror, Rotation};

/// `StructurePiece.SHAPE_CHECK_BLOCKS`: blocks whose position `placeBlock` marks for
/// post-processing.
const SHAPE_CHECK_BLOCKS: [vanillite_data::block::BlockId; 12] = [
    ids::NETHER_BRICK_FENCE,
    ids::TORCH,
    ids::WALL_TORCH,
    ids::OAK_FENCE,
    ids::SPRUCE_FENCE,
    ids::DARK_OAK_FENCE,
    ids::PALE_OAK_FENCE,
    ids::ACACIA_FENCE,
    ids::BIRCH_FENCE,
    ids::JUNGLE_FENCE,
    ids::LADDER,
    ids::IRON_BARS,
];

/// What `postProcess` (and `afterPlace`) receive besides the chunk box: the level, the
/// decoration random and the chunk generator.
///
/// Vanilla's `StructureManager` parameter is reachable through
/// [`WorldGenRegion::structure_starts`] (the starts referenced by the chunk being decorated).
#[derive(Debug)]
pub struct PieceContext<'r, 'a> {
    /// The `WorldGenLevel`: the region around the chunk being decorated.
    pub level: &'r mut WorldGenRegion<'a>,
    /// The `RandomSource` (`WorldgenRandom(Xoroshiro)` seeded with the structure's feature
    /// seed).
    pub random: &'r mut DecorationRandom,
    /// The `ChunkGenerator` (feature placement, sea level, ...).
    pub generator: &'r TerrainGenerator,
}

/// The state of Vanilla's `StructurePiece` base class.
#[derive(Clone, Debug)]
pub struct PieceBase {
    /// `StructurePieceType` registry id (e.g. `minecraft:tesh` for swamp huts).
    pub kind: &'static str,
    /// `boundingBox` (pieces may change it while they are placed).
    pub bounding_box: BoundingBox,
    /// `genDepth`.
    pub gen_depth: i32,
    orientation: Option<Direction>,
    mirror: Mirror,
    rotation: Rotation,
}

impl PieceBase {
    /// `new StructurePiece(type, genDepth, boundingBox)`: no orientation.
    pub fn new(kind: &'static str, gen_depth: i32, bounding_box: BoundingBox) -> Self {
        Self {
            kind,
            bounding_box,
            gen_depth,
            orientation: None,
            mirror: Mirror::None,
            rotation: Rotation::None,
        }
    }

    /// `getOrientation()`.
    pub fn orientation(&self) -> Option<Direction> {
        self.orientation
    }

    /// `setOrientation(orientation)`: also sets the mirror and rotation that `placeBlock`
    /// applies.
    pub fn set_orientation(&mut self, orientation: Option<Direction>) {
        self.orientation = orientation;
        (self.mirror, self.rotation) = match orientation {
            Some(Direction::South) => (Mirror::LeftRight, Rotation::None),
            Some(Direction::West) => (Mirror::LeftRight, Rotation::Clockwise90),
            Some(Direction::East) => (Mirror::None, Rotation::Clockwise90),
            _ => (Mirror::None, Rotation::None),
        };
    }

    /// `getMirror()`.
    pub fn mirror(&self) -> Mirror {
        self.mirror
    }

    /// The rotation `placeBlock` applies (the base class `rotation` field).
    pub fn rotation(&self) -> Rotation {
        self.rotation
    }

    /// `getWorldX(x, z)`.
    pub fn world_x(&self, x: i32, z: i32) -> i32 {
        let bb = &self.bounding_box;
        match self.orientation {
            Some(Direction::North | Direction::South) => bb.min_x() + x,
            Some(Direction::West) => bb.max_x() - z,
            Some(Direction::East) => bb.min_x() + z,
            _ => x,
        }
    }

    /// `getWorldY(y)`.
    pub fn world_y(&self, y: i32) -> i32 {
        match self.orientation {
            None => y,
            Some(_) => y + self.bounding_box.min_y(),
        }
    }

    /// `getWorldZ(x, z)`.
    pub fn world_z(&self, x: i32, z: i32) -> i32 {
        let bb = &self.bounding_box;
        match self.orientation {
            Some(Direction::North) => bb.max_z() - z,
            Some(Direction::South) => bb.min_z() + z,
            Some(Direction::West | Direction::East) => bb.min_z() + x,
            _ => z,
        }
    }

    /// `getWorldPos(x, y, z)`.
    pub fn world_pos(&self, x: i32, y: i32, z: i32) -> BlockPos {
        BlockPos::new(self.world_x(x, z), self.world_y(y), self.world_z(x, z))
    }
}

/// `StructurePiece.BlockSelector`: picks the block of each position of a box.
pub trait BlockSelector {
    /// `next(random, worldX, worldY, worldZ, isEdge)`: chooses the next block.
    fn next(&mut self, random: &mut DecorationRandom, x: i32, y: i32, z: i32, is_edge: bool);
    /// `getNext()`.
    fn get_next(&self) -> BlockState;
}

/// A structure piece (`StructurePiece`).
///
/// Required: [`Self::base`], [`Self::base_mut`] and [`Self::post_process`]. Everything else
/// ports the base class and is overridden where Vanilla's subclass overrides it
/// (`move` → [`Self::move_by`], `getRotation` → [`Self::rotation`], `canBeReplaced` →
/// [`Self::can_be_replaced`], `isReplaceableByStructures`, `fillColumnDown`, ...).
///
/// Helpers that take piece-local coordinates go through `getWorldPos` and only touch blocks
/// inside `chunk_bb`, exactly as in Vanilla.
pub trait StructurePiece: Any + Send + Sync + fmt::Debug {
    /// The base class state.
    fn base(&self) -> &PieceBase;

    /// The base class state, mutable.
    fn base_mut(&mut self) -> &mut PieceBase;

    /// `postProcess(level, structureManager, generator, random, chunkBB, chunkPos,
    /// referencePos)`: places the part of the piece inside `chunk_bb`. The box is the one
    /// `placeInChunk` received for this start; a piece may grow it (as nether fossils and
    /// ruined portals do), which the later pieces of the start and `afterPlace` see.
    fn post_process(
        &mut self,
        ctx: &mut PieceContext<'_, '_>,
        chunk_bb: &mut BoundingBox,
        chunk_pos: ChunkPos,
        reference: BlockPos,
    );

    /// `move(dx, dy, dz)`.
    fn move_by(&mut self, dx: i32, dy: i32, dz: i32) {
        self.base_mut().bounding_box.translate(dx, dy, dz);
    }

    /// `getRotation()`.
    fn rotation(&self) -> Rotation {
        self.base().rotation()
    }

    /// `addAdditionalSaveData(context, tag)`: the piece's own NBT fields (`createTag` without
    /// the base fields `id`, `BB`, `O` and `GD`). `None` while a piece type does not write them
    /// yet (the structure parity tests report that as a difference).
    fn additional_save_data(&self) -> Option<Compound> {
        None
    }

    /// For `PoolElementStructurePiece`s: what the `Beardifier` reads of them (`getElement()
    /// .getProjection()`, `getGroundLevelDelta()`, `getJunctions()`). `None` for other pieces.
    fn pool_element(&self) -> Option<super::pool::PoolElementInfo<'_>> {
        None
    }

    /// `getBoundingBox()`.
    fn bounding_box(&self) -> &BoundingBox {
        &self.base().bounding_box
    }

    /// `getGenDepth()`.
    fn gen_depth(&self) -> i32 {
        self.base().gen_depth
    }

    /// `getOrientation()`.
    fn orientation(&self) -> Option<Direction> {
        self.base().orientation()
    }

    /// `getMirror()`.
    fn mirror(&self) -> Mirror {
        self.base().mirror()
    }

    /// `isCloseToChunk(pos, distance)`.
    fn is_close_to_chunk(&self, pos: ChunkPos, distance: i32) -> bool {
        let (x, z) = (pos.min_block_x(), pos.min_block_z());
        self.bounding_box().intersects_xz(
            x - distance,
            z - distance,
            x + 15 + distance,
            z + 15 + distance,
        )
    }

    /// `getLocatorPosition()`.
    fn locator_position(&self) -> BlockPos {
        self.bounding_box().center()
    }

    /// `getWorldPos(x, y, z)`.
    fn world_pos(&self, x: i32, y: i32, z: i32) -> BlockPos {
        self.base().world_pos(x, y, z)
    }

    /// `canBeReplaced(level, x, y, z, chunkBB)`.
    fn can_be_replaced(
        &self,
        _level: &WorldGenRegion<'_>,
        _x: i32,
        _y: i32,
        _z: i32,
        _chunk_bb: &BoundingBox,
    ) -> bool {
        true
    }

    /// `placeBlock(level, blockState, x, y, z, chunkBB)`: mirrored and rotated by the
    /// orientation, with a fluid tick for fluids and a post-processing mark for
    /// `SHAPE_CHECK_BLOCKS`.
    fn place_block(
        &self,
        level: &mut WorldGenRegion<'_>,
        state: BlockState,
        x: i32,
        y: i32,
        z: i32,
        chunk_bb: &BoundingBox,
    ) {
        let pos = self.world_pos(x, y, z);
        if !chunk_bb.is_inside(pos) || !self.can_be_replaced(level, x, y, z, chunk_bb) {
            return;
        }
        let base = self.base();
        let mut state = state;
        if base.mirror() != Mirror::None {
            state = mirror_state(state, base.mirror());
        }
        if base.rotation() != Rotation::None {
            state = rotate_state(state, base.rotation());
        }
        level.set_block(pos, state, 2);
        let fluid = level.fluid(pos);
        if !fluid.is_empty() {
            schedule_fluid_tick(level, pos, fluid);
        }
        if SHAPE_CHECK_BLOCKS.contains(&state.block()) {
            level.mark_post_processing(pos);
        }
    }

    /// `getBlock(level, x, y, z, chunkBB)`: air outside `chunk_bb`.
    fn get_block(
        &self,
        level: &WorldGenRegion<'_>,
        x: i32,
        y: i32,
        z: i32,
        chunk_bb: &BoundingBox,
    ) -> BlockState {
        let pos = self.world_pos(x, y, z);
        if chunk_bb.is_inside(pos) {
            level.block(pos)
        } else {
            BlockState::AIR
        }
    }

    /// `isInterior(level, x, y, z, chunkBB)`: below the ocean floor heightmap.
    fn is_interior(
        &self,
        level: &WorldGenRegion<'_>,
        x: i32,
        y: i32,
        z: i32,
        chunk_bb: &BoundingBox,
    ) -> bool {
        let pos = self.world_pos(x, y + 1, z);
        chunk_bb.is_inside(pos)
            && pos.y < level.height_at(HeightmapKind::OceanFloorWg, pos.x, pos.z)
    }

    /// `generateAirBox(level, chunkBB, x0, y0, z0, x1, y1, z1)`.
    #[allow(clippy::too_many_arguments)]
    fn generate_air_box(
        &self,
        level: &mut WorldGenRegion<'_>,
        chunk_bb: &BoundingBox,
        x0: i32,
        y0: i32,
        z0: i32,
        x1: i32,
        y1: i32,
        z1: i32,
    ) {
        for y in y0..=y1 {
            for x in x0..=x1 {
                for z in z0..=z1 {
                    self.place_block(level, BlockState::AIR, x, y, z, chunk_bb);
                }
            }
        }
    }

    /// `generateBox(level, chunkBB, x0, y0, z0, x1, y1, z1, edgeBlock, fillBlock, skipAir)`.
    #[allow(clippy::too_many_arguments)]
    fn generate_box(
        &self,
        level: &mut WorldGenRegion<'_>,
        chunk_bb: &BoundingBox,
        x0: i32,
        y0: i32,
        z0: i32,
        x1: i32,
        y1: i32,
        z1: i32,
        edge: BlockState,
        fill: BlockState,
        skip_air: bool,
    ) {
        for y in y0..=y1 {
            for x in x0..=x1 {
                for z in z0..=z1 {
                    if skip_air && self.get_block(level, x, y, z, chunk_bb).is_air() {
                        continue;
                    }
                    let inner = y != y0 && y != y1 && x != x0 && x != x1 && z != z0 && z != z1;
                    let state = if inner { fill } else { edge };
                    self.place_block(level, state, x, y, z, chunk_bb);
                }
            }
        }
    }

    /// `generateBox(level, chunkBB, boxBB, edgeBlock, fillBlock, skipAir)`.
    fn generate_box_in(
        &self,
        level: &mut WorldGenRegion<'_>,
        chunk_bb: &BoundingBox,
        bx: &BoundingBox,
        edge: BlockState,
        fill: BlockState,
        skip_air: bool,
    ) {
        self.generate_box(
            level,
            chunk_bb,
            bx.min_x(),
            bx.min_y(),
            bx.min_z(),
            bx.max_x(),
            bx.max_y(),
            bx.max_z(),
            edge,
            fill,
            skip_air,
        );
    }

    /// `generateBox(level, chunkBB, x0, y0, z0, x1, y1, z1, skipAir, random, selector)`.
    #[allow(clippy::too_many_arguments)]
    fn generate_box_with(
        &self,
        level: &mut WorldGenRegion<'_>,
        chunk_bb: &BoundingBox,
        x0: i32,
        y0: i32,
        z0: i32,
        x1: i32,
        y1: i32,
        z1: i32,
        skip_air: bool,
        random: &mut DecorationRandom,
        selector: &mut dyn BlockSelector,
    ) {
        for y in y0..=y1 {
            for x in x0..=x1 {
                for z in z0..=z1 {
                    if skip_air && self.get_block(level, x, y, z, chunk_bb).is_air() {
                        continue;
                    }
                    let edge = y == y0 || y == y1 || x == x0 || x == x1 || z == z0 || z == z1;
                    selector.next(random, x, y, z, edge);
                    self.place_block(level, selector.get_next(), x, y, z, chunk_bb);
                }
            }
        }
    }

    /// `generateBox(level, chunkBB, boxBB, skipAir, random, selector)`.
    fn generate_box_in_with(
        &self,
        level: &mut WorldGenRegion<'_>,
        chunk_bb: &BoundingBox,
        bx: &BoundingBox,
        skip_air: bool,
        random: &mut DecorationRandom,
        selector: &mut dyn BlockSelector,
    ) {
        self.generate_box_with(
            level,
            chunk_bb,
            bx.min_x(),
            bx.min_y(),
            bx.min_z(),
            bx.max_x(),
            bx.max_y(),
            bx.max_z(),
            skip_air,
            random,
            selector,
        );
    }

    /// `generateMaybeBox(level, chunkBB, random, probability, x0, y0, z0, x1, y1, z1,
    /// edgeBlock, fillBlock, skipAir, hasToBeInside)`.
    #[allow(clippy::too_many_arguments)]
    fn generate_maybe_box(
        &self,
        level: &mut WorldGenRegion<'_>,
        chunk_bb: &BoundingBox,
        random: &mut DecorationRandom,
        probability: f32,
        x0: i32,
        y0: i32,
        z0: i32,
        x1: i32,
        y1: i32,
        z1: i32,
        edge: BlockState,
        fill: BlockState,
        skip_air: bool,
        has_to_be_inside: bool,
    ) {
        for y in y0..=y1 {
            for x in x0..=x1 {
                for z in z0..=z1 {
                    if random.next_float() > probability
                        || (skip_air && self.get_block(level, x, y, z, chunk_bb).is_air())
                        || (has_to_be_inside && !self.is_interior(level, x, y, z, chunk_bb))
                    {
                        continue;
                    }
                    let inner = y != y0 && y != y1 && x != x0 && x != x1 && z != z0 && z != z1;
                    let state = if inner { fill } else { edge };
                    self.place_block(level, state, x, y, z, chunk_bb);
                }
            }
        }
    }

    /// `maybeGenerateBlock(level, chunkBB, random, probability, x, y, z, blockState)`.
    #[allow(clippy::too_many_arguments)]
    fn maybe_generate_block(
        &self,
        level: &mut WorldGenRegion<'_>,
        chunk_bb: &BoundingBox,
        random: &mut DecorationRandom,
        probability: f32,
        x: i32,
        y: i32,
        z: i32,
        state: BlockState,
    ) {
        if random.next_float() < probability {
            self.place_block(level, state, x, y, z, chunk_bb);
        }
    }

    /// `generateUpperHalfSphere(level, chunkBB, x0, y0, z0, x1, y1, z1, fillBlock, skipAir)`.
    #[allow(clippy::too_many_arguments)]
    fn generate_upper_half_sphere(
        &self,
        level: &mut WorldGenRegion<'_>,
        chunk_bb: &BoundingBox,
        x0: i32,
        y0: i32,
        z0: i32,
        x1: i32,
        y1: i32,
        z1: i32,
        fill: BlockState,
        skip_air: bool,
    ) {
        let diag_x = (x1 - x0 + 1) as f32;
        let diag_y = (y1 - y0 + 1) as f32;
        let diag_z = (z1 - z0 + 1) as f32;
        let cx = x0 as f32 + diag_x / 2.0;
        let cz = z0 as f32 + diag_z / 2.0;
        for y in y0..=y1 {
            let dy = (y - y0) as f32 / diag_y;
            for x in x0..=x1 {
                let dx = (x as f32 - cx) / (diag_x * 0.5);
                for z in z0..=z1 {
                    let dz = (z as f32 - cz) / (diag_z * 0.5);
                    if skip_air && self.get_block(level, x, y, z, chunk_bb).is_air() {
                        continue;
                    }
                    let dist = dx * dx + dy * dy + dz * dz;
                    if dist <= 1.05 {
                        self.place_block(level, fill, x, y, z, chunk_bb);
                    }
                }
            }
        }
    }

    /// `isReplaceableByStructures(state)`.
    fn is_replaceable_by_structures(&self, state: BlockState) -> bool {
        state.is_air()
            || state.has(flags::LIQUID)
            || matches!(
                state.block(),
                ids::GLOW_LICHEN | ids::SEAGRASS | ids::TALL_SEAGRASS
            )
    }

    /// `fillColumnDown(level, blockState, x, startY, z, chunkBB)`: no mirror or rotation.
    #[allow(clippy::too_many_arguments)]
    fn fill_column_down(
        &self,
        level: &mut WorldGenRegion<'_>,
        state: BlockState,
        x: i32,
        start_y: i32,
        z: i32,
        chunk_bb: &BoundingBox,
    ) {
        let mut pos = self.world_pos(x, start_y, z);
        if !chunk_bb.is_inside(pos) {
            return;
        }
        while self.is_replaceable_by_structures(level.block(pos)) && pos.y > level.min_y() + 1 {
            level.set_block(pos, state, 2);
            pos = pos.relative(Direction::Down);
        }
    }

    /// `createChest(level, chunkBB, random, x, y, z, lootTable)`.
    #[allow(clippy::too_many_arguments)]
    fn create_chest(
        &self,
        level: &mut WorldGenRegion<'_>,
        chunk_bb: &BoundingBox,
        random: &mut DecorationRandom,
        x: i32,
        y: i32,
        z: i32,
        loot_table: &str,
    ) -> bool {
        let pos = self.world_pos(x, y, z);
        create_chest_at(level, chunk_bb, random, pos, loot_table, None)
    }

    /// `createDispenser(level, chunkBB, random, x, y, z, facing, lootTable)`.
    #[allow(clippy::too_many_arguments)]
    fn create_dispenser(
        &self,
        level: &mut WorldGenRegion<'_>,
        chunk_bb: &BoundingBox,
        random: &mut DecorationRandom,
        x: i32,
        y: i32,
        z: i32,
        facing: Direction,
        loot_table: &str,
    ) -> bool {
        let pos = self.world_pos(x, y, z);
        if !chunk_bb.is_inside(pos) || level.block(pos).block() == ids::DISPENSER {
            return false;
        }
        let state = with_facing(ids::DISPENSER.default_state(), facing);
        self.place_block(level, state, x, y, z, chunk_bb);
        if matches!(
            block_entity_type(level.block(pos)),
            Some("minecraft:dispenser" | "minecraft:dropper")
        ) {
            set_loot_table(level, pos, loot_table, random.next_long());
        }
        true
    }
}

impl dyn StructurePiece {
    /// The piece as `T`, if it is one (`piece instanceof T`).
    pub fn downcast_ref<T: StructurePiece>(&self) -> Option<&T> {
        (self as &dyn Any).downcast_ref()
    }

    /// The piece as a mutable `T`, if it is one.
    pub fn downcast_mut<T: StructurePiece>(&mut self) -> Option<&mut T> {
        (self as &mut dyn Any).downcast_mut()
    }
}

/// `state.setValue(FACING, direction)` (the state is returned unchanged without the property).
pub fn with_facing(state: BlockState, direction: Direction) -> BlockState {
    state
        .with(
            "facing",
            crate::template::transform::direction_name(direction),
        )
        .unwrap_or(state)
}

/// `StructurePiece.makeBoundingBox(x, y, z, direction, width, height, depth)`.
pub fn make_bounding_box(
    x: i32,
    y: i32,
    z: i32,
    direction: Direction,
    width: i32,
    height: i32,
    depth: i32,
) -> BoundingBox {
    if matches!(direction, Direction::North | Direction::South) {
        BoundingBox::new(x, y, z, x + width - 1, y + height - 1, z + depth - 1)
    } else {
        BoundingBox::new(x, y, z, x + depth - 1, y + height - 1, z + width - 1)
    }
}

/// `Direction.Plane.HORIZONTAL` in Vanilla's order.
pub const HORIZONTAL: [Direction; 4] = [
    Direction::North,
    Direction::East,
    Direction::South,
    Direction::West,
];

/// `getRandomHorizontalDirection(random)`: `Direction.Plane.HORIZONTAL.getRandomDirection`.
pub fn random_horizontal_direction<R: RandomSource + ?Sized>(random: &mut R) -> Direction {
    HORIZONTAL[random.next_int_bounded(4) as usize]
}

/// `Direction.get2DDataValue()` of a horizontal direction (south 0, west 1, north 2, east 3).
pub fn data_2d(direction: Direction) -> usize {
    match direction {
        Direction::South => 0,
        Direction::West => 1,
        Direction::North => 2,
        _ => 3,
    }
}

/// `StructurePiece.reorient(level, pos, state)`: turns a chest away from its single solid
/// neighbour, or away from walls.
pub fn reorient(level: &WorldGenRegion<'_>, pos: BlockPos, state: BlockState) -> BlockState {
    let mut solid_neighbor = None;
    for direction in HORIZONTAL {
        let neighbour = level.block(pos.relative(direction));
        if neighbour.block() == ids::CHEST {
            return state;
        }
        if behaviour::is_solid_render(neighbour) {
            if solid_neighbor.is_some() {
                solid_neighbor = None;
                break;
            }
            solid_neighbor = Some(direction);
        }
    }
    if let Some(direction) = solid_neighbor {
        return with_facing(state, direction.opposite());
    }
    let mut lock = state
        .value("facing")
        .and_then(crate::template::transform::direction_by_name)
        .unwrap_or(Direction::North);
    let solid = |d: Direction| behaviour::is_solid_render(level.block(pos.relative(d)));
    if solid(lock) {
        lock = lock.opposite();
    }
    if solid(lock) {
        lock = crate::template::transform::clockwise(lock);
    }
    if solid(lock) {
        lock = lock.opposite();
    }
    with_facing(state, lock)
}

/// `createChest(level, chunkBB, random, pos, lootTable, blockState)`: a chest (reoriented when
/// `state` is `None`) with the loot table and a seed from `random.nextLong()`.
pub fn create_chest_at(
    level: &mut WorldGenRegion<'_>,
    chunk_bb: &BoundingBox,
    random: &mut DecorationRandom,
    pos: BlockPos,
    loot_table: &str,
    state: Option<BlockState>,
) -> bool {
    if !chunk_bb.is_inside(pos) || level.block(pos).block() == ids::CHEST {
        return false;
    }
    let state = state.unwrap_or_else(|| reorient(level, pos, ids::CHEST.default_state()));
    level.set_block(pos, state, 2);
    if matches!(
        block_entity_type(level.block(pos)),
        Some("minecraft:chest" | "minecraft:trapped_chest")
    ) {
        set_loot_table(level, pos, loot_table, random.next_long());
    }
    true
}

/// `RandomizableContainer.setLootTable(lootTable, seed)` on the block entity at `pos`, as
/// `saveAdditional` writes it (`LootTable`, `LootTableSeed` unless 0).
pub fn set_loot_table(level: &mut WorldGenRegion<'_>, pos: BlockPos, loot_table: &str, seed: i64) {
    let Some(tag) = level.block_entity_mut(pos) else {
        return;
    };
    tag.insert("LootTable", Tag::String(loot_table.to_owned()));
    if seed != 0 {
        tag.insert("LootTableSeed", Tag::Long(seed));
    } else {
        tag.remove("LootTableSeed");
    }
}

/// `StructurePiece.createBoundingBox(pieces)`: the box around every piece; `None` without
/// pieces (Vanilla throws).
pub fn create_bounding_box(pieces: &[Box<dyn StructurePiece>]) -> Option<BoundingBox> {
    let mut result: Option<BoundingBox> = None;
    for piece in pieces {
        let bb = *piece.bounding_box();
        match &mut result {
            Some(r) => r.encapsulate(&bb),
            None => result = Some(bb),
        }
    }
    result
}

/// `StructurePiece.findCollisionPiece(pieces, box)`: the index of the first piece whose box
/// intersects `bx`.
pub fn find_collision_piece(pieces: &[Box<dyn StructurePiece>], bx: &BoundingBox) -> Option<usize> {
    pieces.iter().position(|p| p.bounding_box().intersects(bx))
}

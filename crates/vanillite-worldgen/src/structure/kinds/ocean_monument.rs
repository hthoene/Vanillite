//! Ocean monuments (`OceanMonumentStructure`, `OceanMonumentPieces`): the building with its wings,
//! walls and penthouse, and the rooms of the 5×5×3 room graph inside it.
//!
//! The monument start holds one piece, the [`MonumentBuilding`]; the rooms are its children and
//! are placed by it, as in Vanilla. Elder guardians are recorded as entities (type and position);
//! their `finalizeSpawn` makes its draws from the level's random.

use std::sync::Arc;

use serde_json::Value;
use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, ChunkPos, Direction};
use vanillite_chunk::HeightmapKind;
use vanillite_data::block::{BlockState, ids};
use vanillite_nbt::Compound;

use crate::Error;
use crate::feature::WorldGenRegion;
use crate::feature::kinds::tree::shuffle;
use crate::feature::region::entity_tag;
use crate::structure::piece::PieceContext;
use crate::structure::structure::on_top_of_chunk_center;
use crate::structure::{
    BiomeFilter, GenerationContext, GenerationStub, PieceBase, StructureParser, StructurePiece,
    StructureType, make_bounding_box, random_horizontal_direction,
};
use crate::template::BoundingBox;

/// `OceanMonumentPiece.BASE_GRAY`.
fn base_gray() -> BlockState {
    ids::PRISMARINE.default_state()
}

/// `OceanMonumentPiece.BASE_LIGHT` (also `DOT_DECO_DATA`).
fn base_light() -> BlockState {
    ids::PRISMARINE_BRICKS.default_state()
}

/// `OceanMonumentPiece.BASE_BLACK`.
fn base_black() -> BlockState {
    ids::DARK_PRISMARINE.default_state()
}

/// `OceanMonumentPiece.LAMP_BLOCK`.
fn lamp_block() -> BlockState {
    ids::SEA_LANTERN.default_state()
}

/// `OceanMonumentPiece.FILL_BLOCK`.
fn fill_block() -> BlockState {
    ids::WATER.default_state()
}

/// `MonumentBuilding.BIOME_RANGE_CHECK`.
const BIOME_RANGE_CHECK: i32 = 29;

/// `OceanMonumentStructure`.
#[derive(Debug)]
pub struct OceanMonumentStructure {
    /// `BiomeTags.REQUIRED_OCEAN_MONUMENT_SURROUNDING`.
    surrounding: BiomeFilter,
}

impl StructureType for OceanMonumentStructure {
    fn parse(_json: &Value, parser: &mut StructureParser<'_>) -> Result<Self, Error> {
        let tag = Value::String("#minecraft:required_ocean_monument_surrounding".to_owned());
        Ok(Self {
            surrounding: parser.biome_filter(&tag)?,
        })
    }

    fn find_generation_point<'s>(
        &'s self,
        ctx: &mut GenerationContext<'_>,
    ) -> Option<GenerationStub<'s>> {
        let x = ctx.chunk_pos.min_block_x() + 9;
        let z = ctx.chunk_pos.min_block_z() + 9;
        let y = ctx.generator.sea_level();
        // `biomeResolver().getBiomesWithin(x, seaLevel, z, 29)`: every biome must be in the tag.
        let r = BIOME_RANGE_CHECK;
        for quart_z in (z - r) >> 2..=(z + r) >> 2 {
            for quart_x in (x - r) >> 2..=(x + r) >> 2 {
                for quart_y in (y - r) >> 2..=(y + r) >> 2 {
                    if !self
                        .surrounding
                        .contains(ctx.noise_biome(quart_x, quart_y, quart_z))
                    {
                        return None;
                    }
                }
            }
        }
        on_top_of_chunk_center(ctx, HeightmapKind::OceanFloorWg, |builder, ctx| {
            // `createTopPiece(chunkPos, random)`.
            let west = ctx.chunk_pos.min_block_x() - 29;
            let north = ctx.chunk_pos.min_block_z() - 29;
            let random = &mut ctx.random.random;
            let orientation = random_horizontal_direction(random);
            builder.add_piece(Box::new(MonumentBuilding::new(
                random,
                west,
                north,
                orientation,
            )));
        })
    }
}

/// `OceanMonumentPiece.getRoomIndex(roomX, roomY, roomZ)`.
const fn room_index(x: i32, y: i32, z: i32) -> i32 {
    y * 25 + z * 5 + x
}

/// `GRIDROOM_SOURCE_INDEX`.
const GRIDROOM_SOURCE_INDEX: i32 = room_index(2, 0, 0);
/// `GRIDROOM_TOP_CONNECT_INDEX`.
const GRIDROOM_TOP_CONNECT_INDEX: i32 = room_index(2, 2, 0);
/// `GRIDROOM_LEFTWING_CONNECT_INDEX`.
const GRIDROOM_LEFTWING_CONNECT_INDEX: i32 = room_index(0, 1, 0);
/// `GRIDROOM_RIGHTWING_CONNECT_INDEX`.
const GRIDROOM_RIGHTWING_CONNECT_INDEX: i32 = room_index(4, 1, 0);

/// `OceanMonumentPieces.RoomDefinition`: a cell of the room graph. Rooms refer to each other by
/// their position in the graph's list.
#[derive(Clone, Debug)]
pub struct RoomDefinition {
    /// `index`: the grid index, or 1001–1003 for the wings and the penthouse.
    index: i32,
    connections: [Option<usize>; 6],
    has_opening: [bool; 6],
    claimed: bool,
    is_source: bool,
    scan_index: i32,
}

impl RoomDefinition {
    fn new(index: i32) -> Self {
        Self {
            index,
            connections: [None; 6],
            has_opening: [false; 6],
            claimed: false,
            is_source: false,
            scan_index: 0,
        }
    }

    /// `hasOpening[direction]`.
    fn opens(&self, direction: Direction) -> bool {
        self.has_opening[direction as usize]
    }

    /// `connections[direction] != null`.
    fn connects(&self, direction: Direction) -> bool {
        self.connections[direction as usize].is_some()
    }

    /// `connections[direction]`, which the room's shape guarantees.
    fn connection(&self, direction: Direction) -> usize {
        self.connections[direction as usize].unwrap_or_else(|| {
            panic!(
                "monument room {} has no {direction:?} neighbour",
                self.index
            )
        })
    }

    /// `updateOpenings()`.
    fn update_openings(&mut self) {
        for i in 0..6 {
            self.has_opening[i] = self.connections[i].is_some();
        }
    }

    /// `isSpecial()`.
    fn is_special(&self) -> bool {
        self.index >= 75
    }

    /// `countOpenings()`.
    fn count_openings(&self) -> usize {
        self.has_opening.iter().filter(|&&open| open).count()
    }
}

/// `setConnection(direction, definition)` of room `a`.
fn set_connection(rooms: &mut [RoomDefinition], a: usize, direction: Direction, b: usize) {
    rooms[a].connections[direction as usize] = Some(b);
    rooms[b].connections[direction.opposite() as usize] = Some(a);
}

/// `findSource(scanIndex)` of room `id`.
fn find_source(rooms: &mut [RoomDefinition], id: usize, scan_index: i32) -> bool {
    if rooms[id].is_source {
        return true;
    }
    rooms[id].scan_index = scan_index;
    for i in 0..6 {
        if let Some(next) = rooms[id].connections[i]
            && rooms[id].has_opening[i]
            && rooms[next].scan_index != scan_index
            && find_source(rooms, next, scan_index)
        {
            return true;
        }
    }
    false
}

/// The result of `generateRoomGraph`: the rooms, their order (`roomDefs`: the grid rooms
/// shuffled, then the penthouse and the wings), the source and the core room.
struct RoomGraph {
    rooms: Vec<RoomDefinition>,
    order: Vec<usize>,
    source: usize,
    core: usize,
}

/// `MonumentBuilding.generateRoomGraph(random)`.
fn generate_room_graph<R: RandomSource>(random: &mut R) -> RoomGraph {
    let mut rooms = Vec::new();
    let mut grid: [Option<usize>; 75] = [None; 75];
    let mut add = |rooms: &mut Vec<RoomDefinition>, x: i32, y: i32, z: i32| {
        let index = room_index(x, y, z);
        rooms.push(RoomDefinition::new(index));
        grid[index as usize] = Some(rooms.len() - 1);
    };
    for x in 0..5 {
        for z in 0..4 {
            add(&mut rooms, x, 0, z);
        }
    }
    for x in 0..5 {
        for z in 0..4 {
            add(&mut rooms, x, 1, z);
        }
    }
    for x in 1..4 {
        for z in 0..2 {
            add(&mut rooms, x, 2, z);
        }
    }
    let at = |index: i32| grid[index as usize].expect("monument grid room");
    let source = at(GRIDROOM_SOURCE_INDEX);
    for x in 0..5 {
        for z in 0..5 {
            for y in 0..3 {
                let Some(room) = grid[room_index(x, y, z) as usize] else {
                    continue;
                };
                for direction in Direction::ALL {
                    let [dx, dy, dz] = direction.normal();
                    let (nx, ny, nz) = (x + dx, y + dy, z + dz);
                    if !(0..5).contains(&nx) || !(0..5).contains(&nz) || !(0..3).contains(&ny) {
                        continue;
                    }
                    if let Some(neighbour) = grid[room_index(nx, ny, nz) as usize] {
                        let direction = if nz == z {
                            direction
                        } else {
                            direction.opposite()
                        };
                        set_connection(&mut rooms, room, direction, neighbour);
                    }
                }
            }
        }
    }
    let roof = rooms.len();
    rooms.push(RoomDefinition::new(1003));
    let left_wing = rooms.len();
    rooms.push(RoomDefinition::new(1001));
    let right_wing = rooms.len();
    rooms.push(RoomDefinition::new(1002));
    set_connection(
        &mut rooms,
        at(GRIDROOM_TOP_CONNECT_INDEX),
        Direction::Up,
        roof,
    );
    set_connection(
        &mut rooms,
        at(GRIDROOM_LEFTWING_CONNECT_INDEX),
        Direction::South,
        left_wing,
    );
    set_connection(
        &mut rooms,
        at(GRIDROOM_RIGHTWING_CONNECT_INDEX),
        Direction::South,
        right_wing,
    );
    rooms[roof].claimed = true;
    rooms[left_wing].claimed = true;
    rooms[right_wing].claimed = true;
    rooms[source].is_source = true;
    let core = at(room_index(random.next_int_bounded(4), 0, 2));
    {
        let east = rooms[core].connection(Direction::East);
        let north = rooms[core].connection(Direction::North);
        let east_north = rooms[east].connection(Direction::North);
        let up = rooms[core].connection(Direction::Up);
        let east_up = rooms[east].connection(Direction::Up);
        let north_up = rooms[north].connection(Direction::Up);
        let east_north_up = rooms[east_north].connection(Direction::Up);
        for id in [
            core,
            east,
            north,
            east_north,
            up,
            east_up,
            north_up,
            east_north_up,
        ] {
            rooms[id].claimed = true;
        }
    }
    let mut order: Vec<usize> = grid.iter().flatten().copied().collect();
    for &id in &order {
        rooms[id].update_openings();
    }
    rooms[roof].update_openings();
    shuffle(&mut order, random);
    let mut scan_index = 1;
    for &id in &order {
        let mut close_count = 0;
        let mut attempt_count = 0;
        while close_count < 2 && attempt_count < 5 {
            attempt_count += 1;
            let f = random.next_int_bounded(6) as usize;
            if !rooms[id].has_opening[f] {
                continue;
            }
            let of = Direction::ALL[f].opposite() as usize;
            let next = rooms[id].connections[f].expect("an opening has a neighbour");
            rooms[id].has_opening[f] = false;
            rooms[next].has_opening[of] = false;
            let first = find_source(&mut rooms, id, scan_index);
            scan_index += 1;
            // `&&`: the second search only runs when the first succeeded.
            let connected = first && {
                let second = find_source(&mut rooms, next, scan_index);
                scan_index += 1;
                second
            };
            if connected {
                close_count += 1;
            } else {
                rooms[id].has_opening[f] = true;
                rooms[next].has_opening[of] = true;
            }
        }
    }
    order.extend([roof, left_wing, right_wing]);
    RoomGraph {
        rooms,
        order,
        source,
        core,
    }
}

/// `MonumentRoomFitter`: the room shapes, tried in Vanilla's order.
#[derive(Clone, Copy, Debug)]
enum Fitter {
    DoubleXY,
    DoubleYZ,
    DoubleZ,
    DoubleX,
    DoubleY,
    SimpleTop,
    Simple,
}

impl Fitter {
    const ALL: [Self; 7] = [
        Self::DoubleXY,
        Self::DoubleYZ,
        Self::DoubleZ,
        Self::DoubleX,
        Self::DoubleY,
        Self::SimpleTop,
        Self::Simple,
    ];

    /// `fits(definition)`; `claimed` holds the claimed flags of the rooms.
    fn fits(self, rooms: &[RoomDefinition], claimed: &[bool], id: usize) -> bool {
        use Direction::{East, North, South, Up, West};
        let room = &rooms[id];
        let free = |id: usize, direction: Direction| {
            rooms[id].opens(direction) && !claimed[rooms[id].connection(direction)]
        };
        match self {
            Self::DoubleXY => free(id, East) && free(id, Up) && free(room.connection(East), Up),
            Self::DoubleYZ => free(id, North) && free(id, Up) && free(room.connection(North), Up),
            Self::DoubleZ => free(id, North),
            Self::DoubleX => free(id, East),
            Self::DoubleY => free(id, Up),
            Self::SimpleTop => {
                !room.opens(West)
                    && !room.opens(East)
                    && !room.opens(North)
                    && !room.opens(South)
                    && !room.opens(Up)
            }
            Self::Simple => true,
        }
    }

    /// `create(orientation, definition, random)`: claims the rooms and makes the piece.
    fn create<R: RandomSource>(
        self,
        graph: &Arc<[RoomDefinition]>,
        claimed: &mut [bool],
        id: usize,
        orientation: Direction,
        random: &mut R,
    ) -> MonumentRoom {
        use Direction::{East, North, South, Up};
        let rooms = &**graph;
        let room = &rooms[id];
        let kind = match self {
            Self::DoubleXY => {
                let east = room.connection(East);
                claimed[id] = true;
                claimed[east] = true;
                claimed[room.connection(Up)] = true;
                claimed[rooms[east].connection(Up)] = true;
                RoomKind::DoubleXY
            }
            Self::DoubleYZ => {
                let north = room.connection(North);
                claimed[id] = true;
                claimed[north] = true;
                claimed[room.connection(Up)] = true;
                claimed[rooms[north].connection(Up)] = true;
                RoomKind::DoubleYZ
            }
            Self::DoubleZ => {
                let mut source = id;
                if !room.opens(North) || claimed[room.connection(North)] {
                    source = room.connection(South);
                }
                claimed[source] = true;
                claimed[rooms[source].connection(North)] = true;
                return MonumentRoom::new(RoomKind::DoubleZ, orientation, graph, source);
            }
            Self::DoubleX => {
                claimed[id] = true;
                claimed[room.connection(East)] = true;
                RoomKind::DoubleX
            }
            Self::DoubleY => {
                claimed[id] = true;
                claimed[room.connection(Up)] = true;
                RoomKind::DoubleY
            }
            Self::SimpleTop => {
                claimed[id] = true;
                RoomKind::SimpleTop
            }
            Self::Simple => {
                claimed[id] = true;
                RoomKind::Simple {
                    main_design: random.next_int_bounded(3),
                }
            }
        };
        MonumentRoom::new(kind, orientation, graph, id)
    }
}

/// `OceanMonumentPieces.MonumentBuilding` (`minecraft:omb`).
#[derive(Clone, Debug)]
pub struct MonumentBuilding {
    base: PieceBase,
    /// `childPieces`: the rooms, wings and penthouse, placed by the building.
    pub children: Vec<MonumentRoom>,
}

impl MonumentBuilding {
    /// `new MonumentBuilding(random, west, north, direction)`.
    pub fn new<R: RandomSource>(
        random: &mut R,
        west: i32,
        north: i32,
        direction: Direction,
    ) -> Self {
        let mut base = PieceBase::new(
            "minecraft:omb",
            0,
            make_bounding_box(west, 39, north, direction, 58, 23, 58),
        );
        base.set_orientation(Some(direction));
        let graph = generate_room_graph(random);
        let mut claimed: Vec<bool> = graph.rooms.iter().map(|r| r.claimed).collect();
        let rooms: Arc<[RoomDefinition]> = graph.rooms.into();
        claimed[graph.source] = true;
        let mut children = vec![
            MonumentRoom::new(RoomKind::Entry, direction, &rooms, graph.source),
            MonumentRoom::new(RoomKind::Core, direction, &rooms, graph.core),
        ];
        for &id in &graph.order {
            if claimed[id] || rooms[id].is_special() {
                continue;
            }
            if let Some(fitter) = Fitter::ALL
                .into_iter()
                .find(|fitter| fitter.fits(&rooms, &claimed, id))
            {
                children.push(fitter.create(&rooms, &mut claimed, id, direction, random));
            }
        }
        let offset = base.world_pos(9, 0, 22);
        for child in &mut children {
            child
                .base
                .bounding_box
                .translate(offset.x, offset.y, offset.z);
        }
        let left_wing =
            BoundingBox::from_corners(base.world_pos(1, 1, 1), base.world_pos(23, 8, 21));
        let right_wing =
            BoundingBox::from_corners(base.world_pos(34, 1, 1), base.world_pos(56, 8, 21));
        let penthouse =
            BoundingBox::from_corners(base.world_pos(22, 13, 22), base.world_pos(35, 17, 35));
        let wing_random = random.next_int();
        children.push(MonumentRoom::boxed(
            RoomKind::Wing {
                main_design: wing_random & 1,
            },
            direction,
            left_wing,
            &rooms,
        ));
        children.push(MonumentRoom::boxed(
            RoomKind::Wing {
                main_design: wing_random.wrapping_add(1) & 1,
            },
            direction,
            right_wing,
            &rooms,
        ));
        children.push(MonumentRoom::boxed(
            RoomKind::Penthouse,
            direction,
            penthouse,
            &rooms,
        ));
        Self { base, children }
    }

    /// `chunkIntersects(chunkBB, x0, z0, x1, z1)`.
    fn chunk_intersects(&self, chunk_bb: &BoundingBox, x0: i32, z0: i32, x1: i32, z1: i32) -> bool {
        chunk_intersects(&self.base, chunk_bb, x0, z0, x1, z1)
    }

    /// `generateWing(isFlipped, xoff, level, random, chunkBB)`.
    fn generate_wing(
        &self,
        is_flipped: bool,
        xoff: i32,
        level: &mut WorldGenRegion<'_>,
        chunk_bb: &BoundingBox,
    ) {
        let (gray, light) = (base_gray(), base_light());
        if self.chunk_intersects(chunk_bb, xoff, 0, xoff + 23, 20) {
            self.generate_box(
                level,
                chunk_bb,
                xoff,
                0,
                0,
                xoff + 24,
                0,
                20,
                gray,
                gray,
                false,
            );
            generate_water_box(self, level, chunk_bb, xoff, 1, 0, xoff + 24, 10, 20);

            for i in 0..4 {
                self.generate_box(
                    level,
                    chunk_bb,
                    xoff + i,
                    i + 1,
                    i,
                    xoff + i,
                    i + 1,
                    20,
                    light,
                    light,
                    false,
                );
                self.generate_box(
                    level,
                    chunk_bb,
                    xoff + i + 7,
                    i + 5,
                    i + 7,
                    xoff + i + 7,
                    i + 5,
                    20,
                    light,
                    light,
                    false,
                );
                self.generate_box(
                    level,
                    chunk_bb,
                    xoff + 17 - i,
                    i + 5,
                    i + 7,
                    xoff + 17 - i,
                    i + 5,
                    20,
                    light,
                    light,
                    false,
                );
                self.generate_box(
                    level,
                    chunk_bb,
                    xoff + 24 - i,
                    i + 1,
                    i,
                    xoff + 24 - i,
                    i + 1,
                    20,
                    light,
                    light,
                    false,
                );
                self.generate_box(
                    level,
                    chunk_bb,
                    xoff + i + 1,
                    i + 1,
                    i,
                    xoff + 23 - i,
                    i + 1,
                    i,
                    light,
                    light,
                    false,
                );
                self.generate_box(
                    level,
                    chunk_bb,
                    xoff + i + 8,
                    i + 5,
                    i + 7,
                    xoff + 16 - i,
                    i + 5,
                    i + 7,
                    light,
                    light,
                    false,
                );
            }

            self.generate_box(
                level,
                chunk_bb,
                xoff + 4,
                4,
                4,
                xoff + 6,
                4,
                20,
                gray,
                gray,
                false,
            );
            self.generate_box(
                level,
                chunk_bb,
                xoff + 7,
                4,
                4,
                xoff + 17,
                4,
                6,
                gray,
                gray,
                false,
            );
            self.generate_box(
                level,
                chunk_bb,
                xoff + 18,
                4,
                4,
                xoff + 20,
                4,
                20,
                gray,
                gray,
                false,
            );
            self.generate_box(
                level,
                chunk_bb,
                xoff + 11,
                8,
                11,
                xoff + 13,
                8,
                20,
                gray,
                gray,
                false,
            );
            self.place_block(level, light, xoff + 12, 9, 12, chunk_bb);
            self.place_block(level, light, xoff + 12, 9, 15, chunk_bb);
            self.place_block(level, light, xoff + 12, 9, 18, chunk_bb);
            let left_pos = xoff + if is_flipped { 19 } else { 5 };
            let right_pos = xoff + if is_flipped { 5 } else { 19 };

            for z in (5..=20).rev().step_by(3) {
                self.place_block(level, light, left_pos, 5, z, chunk_bb);
            }

            for z in (7..=19).rev().step_by(3) {
                self.place_block(level, light, right_pos, 5, z, chunk_bb);
            }

            for i in 0..4 {
                let pos = if is_flipped {
                    xoff + 24 - (17 - i * 3)
                } else {
                    xoff + 17 - i * 3
                };
                self.place_block(level, light, pos, 5, 5, chunk_bb);
            }

            self.place_block(level, light, right_pos, 5, 5, chunk_bb);
            self.generate_box(
                level,
                chunk_bb,
                xoff + 11,
                1,
                12,
                xoff + 13,
                7,
                12,
                gray,
                gray,
                false,
            );
            self.generate_box(
                level,
                chunk_bb,
                xoff + 12,
                1,
                11,
                xoff + 12,
                7,
                13,
                gray,
                gray,
                false,
            );
        }
    }

    /// `generateEntranceArchs(level, random, chunkBB)`.
    fn generate_entrance_archs(&self, level: &mut WorldGenRegion<'_>, chunk_bb: &BoundingBox) {
        let (gray, light, lamp) = (base_gray(), base_light(), lamp_block());
        if self.chunk_intersects(chunk_bb, 22, 5, 35, 17) {
            generate_water_box(self, level, chunk_bb, 25, 0, 0, 32, 8, 20);

            for i in 0..4 {
                self.generate_box(
                    level,
                    chunk_bb,
                    24,
                    2,
                    5 + i * 4,
                    24,
                    4,
                    5 + i * 4,
                    light,
                    light,
                    false,
                );
                self.generate_box(
                    level,
                    chunk_bb,
                    22,
                    4,
                    5 + i * 4,
                    23,
                    4,
                    5 + i * 4,
                    light,
                    light,
                    false,
                );
                self.place_block(level, light, 25, 5, 5 + i * 4, chunk_bb);
                self.place_block(level, light, 26, 6, 5 + i * 4, chunk_bb);
                self.place_block(level, lamp, 26, 5, 5 + i * 4, chunk_bb);
                self.generate_box(
                    level,
                    chunk_bb,
                    33,
                    2,
                    5 + i * 4,
                    33,
                    4,
                    5 + i * 4,
                    light,
                    light,
                    false,
                );
                self.generate_box(
                    level,
                    chunk_bb,
                    34,
                    4,
                    5 + i * 4,
                    35,
                    4,
                    5 + i * 4,
                    light,
                    light,
                    false,
                );
                self.place_block(level, light, 32, 5, 5 + i * 4, chunk_bb);
                self.place_block(level, light, 31, 6, 5 + i * 4, chunk_bb);
                self.place_block(level, lamp, 31, 5, 5 + i * 4, chunk_bb);
                self.generate_box(
                    level,
                    chunk_bb,
                    27,
                    6,
                    5 + i * 4,
                    30,
                    6,
                    5 + i * 4,
                    gray,
                    gray,
                    false,
                );
            }
        }
    }

    /// `generateEntranceWall(level, random, chunkBB)`.
    fn generate_entrance_wall(&self, level: &mut WorldGenRegion<'_>, chunk_bb: &BoundingBox) {
        let (gray, light, black) = (base_gray(), base_light(), base_black());
        if self.chunk_intersects(chunk_bb, 15, 20, 42, 21) {
            self.generate_box(level, chunk_bb, 15, 0, 21, 42, 0, 21, gray, gray, false);
            generate_water_box(self, level, chunk_bb, 26, 1, 21, 31, 3, 21);
            self.generate_box(level, chunk_bb, 21, 12, 21, 36, 12, 21, gray, gray, false);
            self.generate_box(level, chunk_bb, 17, 11, 21, 40, 11, 21, gray, gray, false);
            self.generate_box(level, chunk_bb, 16, 10, 21, 41, 10, 21, gray, gray, false);
            self.generate_box(level, chunk_bb, 15, 7, 21, 42, 9, 21, gray, gray, false);
            self.generate_box(level, chunk_bb, 16, 6, 21, 41, 6, 21, gray, gray, false);
            self.generate_box(level, chunk_bb, 17, 5, 21, 40, 5, 21, gray, gray, false);
            self.generate_box(level, chunk_bb, 21, 4, 21, 36, 4, 21, gray, gray, false);
            self.generate_box(level, chunk_bb, 22, 3, 21, 26, 3, 21, gray, gray, false);
            self.generate_box(level, chunk_bb, 31, 3, 21, 35, 3, 21, gray, gray, false);
            self.generate_box(level, chunk_bb, 23, 2, 21, 25, 2, 21, gray, gray, false);
            self.generate_box(level, chunk_bb, 32, 2, 21, 34, 2, 21, gray, gray, false);
            self.generate_box(level, chunk_bb, 28, 4, 20, 29, 4, 21, light, light, false);
            self.place_block(level, light, 27, 3, 21, chunk_bb);
            self.place_block(level, light, 30, 3, 21, chunk_bb);
            self.place_block(level, light, 26, 2, 21, chunk_bb);
            self.place_block(level, light, 31, 2, 21, chunk_bb);
            self.place_block(level, light, 25, 1, 21, chunk_bb);
            self.place_block(level, light, 32, 1, 21, chunk_bb);

            for i in 0..7 {
                self.place_block(level, black, 28 - i, 6 + i, 21, chunk_bb);
                self.place_block(level, black, 29 + i, 6 + i, 21, chunk_bb);
            }

            for i in 0..4 {
                self.place_block(level, black, 28 - i, 9 + i, 21, chunk_bb);
                self.place_block(level, black, 29 + i, 9 + i, 21, chunk_bb);
            }

            self.place_block(level, black, 28, 12, 21, chunk_bb);
            self.place_block(level, black, 29, 12, 21, chunk_bb);

            for i in 0..3 {
                self.place_block(level, black, 22 - i * 2, 8, 21, chunk_bb);
                self.place_block(level, black, 22 - i * 2, 9, 21, chunk_bb);
                self.place_block(level, black, 35 + i * 2, 8, 21, chunk_bb);
                self.place_block(level, black, 35 + i * 2, 9, 21, chunk_bb);
            }

            generate_water_box(self, level, chunk_bb, 15, 13, 21, 42, 15, 21);
            generate_water_box(self, level, chunk_bb, 15, 1, 21, 15, 6, 21);
            generate_water_box(self, level, chunk_bb, 16, 1, 21, 16, 5, 21);
            generate_water_box(self, level, chunk_bb, 17, 1, 21, 20, 4, 21);
            generate_water_box(self, level, chunk_bb, 21, 1, 21, 21, 3, 21);
            generate_water_box(self, level, chunk_bb, 22, 1, 21, 22, 2, 21);
            generate_water_box(self, level, chunk_bb, 23, 1, 21, 24, 1, 21);
            generate_water_box(self, level, chunk_bb, 42, 1, 21, 42, 6, 21);
            generate_water_box(self, level, chunk_bb, 41, 1, 21, 41, 5, 21);
            generate_water_box(self, level, chunk_bb, 37, 1, 21, 40, 4, 21);
            generate_water_box(self, level, chunk_bb, 36, 1, 21, 36, 3, 21);
            generate_water_box(self, level, chunk_bb, 33, 1, 21, 34, 1, 21);
            generate_water_box(self, level, chunk_bb, 35, 1, 21, 35, 2, 21);
        }
    }

    /// `generateRoofPiece(level, random, chunkBB)`.
    fn generate_roof_piece(&self, level: &mut WorldGenRegion<'_>, chunk_bb: &BoundingBox) {
        let (gray, light, lamp) = (base_gray(), base_light(), lamp_block());
        if self.chunk_intersects(chunk_bb, 21, 21, 36, 36) {
            self.generate_box(level, chunk_bb, 21, 0, 22, 36, 0, 36, gray, gray, false);
            generate_water_box(self, level, chunk_bb, 21, 1, 22, 36, 23, 36);

            for i in 0..4 {
                self.generate_box(
                    level,
                    chunk_bb,
                    21 + i,
                    13 + i,
                    21 + i,
                    36 - i,
                    13 + i,
                    21 + i,
                    light,
                    light,
                    false,
                );
                self.generate_box(
                    level,
                    chunk_bb,
                    21 + i,
                    13 + i,
                    36 - i,
                    36 - i,
                    13 + i,
                    36 - i,
                    light,
                    light,
                    false,
                );
                self.generate_box(
                    level,
                    chunk_bb,
                    21 + i,
                    13 + i,
                    22 + i,
                    21 + i,
                    13 + i,
                    35 - i,
                    light,
                    light,
                    false,
                );
                self.generate_box(
                    level,
                    chunk_bb,
                    36 - i,
                    13 + i,
                    22 + i,
                    36 - i,
                    13 + i,
                    35 - i,
                    light,
                    light,
                    false,
                );
            }

            self.generate_box(level, chunk_bb, 25, 16, 25, 32, 16, 32, gray, gray, false);
            self.generate_box(level, chunk_bb, 25, 17, 25, 25, 19, 25, light, light, false);
            self.generate_box(level, chunk_bb, 32, 17, 25, 32, 19, 25, light, light, false);
            self.generate_box(level, chunk_bb, 25, 17, 32, 25, 19, 32, light, light, false);
            self.generate_box(level, chunk_bb, 32, 17, 32, 32, 19, 32, light, light, false);
            self.place_block(level, light, 26, 20, 26, chunk_bb);
            self.place_block(level, light, 27, 21, 27, chunk_bb);
            self.place_block(level, lamp, 27, 20, 27, chunk_bb);
            self.place_block(level, light, 26, 20, 31, chunk_bb);
            self.place_block(level, light, 27, 21, 30, chunk_bb);
            self.place_block(level, lamp, 27, 20, 30, chunk_bb);
            self.place_block(level, light, 31, 20, 31, chunk_bb);
            self.place_block(level, light, 30, 21, 30, chunk_bb);
            self.place_block(level, lamp, 30, 20, 30, chunk_bb);
            self.place_block(level, light, 31, 20, 26, chunk_bb);
            self.place_block(level, light, 30, 21, 27, chunk_bb);
            self.place_block(level, lamp, 30, 20, 27, chunk_bb);
            self.generate_box(level, chunk_bb, 28, 21, 27, 29, 21, 27, gray, gray, false);
            self.generate_box(level, chunk_bb, 27, 21, 28, 27, 21, 29, gray, gray, false);
            self.generate_box(level, chunk_bb, 28, 21, 30, 29, 21, 30, gray, gray, false);
            self.generate_box(level, chunk_bb, 30, 21, 28, 30, 21, 29, gray, gray, false);
        }
    }

    /// `generateLowerWall(level, random, chunkBB)`.
    fn generate_lower_wall(&self, level: &mut WorldGenRegion<'_>, chunk_bb: &BoundingBox) {
        let (gray, light) = (base_gray(), base_light());
        if self.chunk_intersects(chunk_bb, 0, 21, 6, 58) {
            self.generate_box(level, chunk_bb, 0, 0, 21, 6, 0, 57, gray, gray, false);
            generate_water_box(self, level, chunk_bb, 0, 1, 21, 6, 7, 57);
            self.generate_box(level, chunk_bb, 4, 4, 21, 6, 4, 53, gray, gray, false);

            for i in 0..4 {
                self.generate_box(
                    level,
                    chunk_bb,
                    i,
                    i + 1,
                    21,
                    i,
                    i + 1,
                    57 - i,
                    light,
                    light,
                    false,
                );
            }

            for z in (23..53).step_by(3) {
                self.place_block(level, light, 5, 5, z, chunk_bb);
            }

            self.place_block(level, light, 5, 5, 52, chunk_bb);

            for i in 0..4 {
                self.generate_box(
                    level,
                    chunk_bb,
                    i,
                    i + 1,
                    21,
                    i,
                    i + 1,
                    57 - i,
                    light,
                    light,
                    false,
                );
            }

            self.generate_box(level, chunk_bb, 4, 1, 52, 6, 3, 52, gray, gray, false);
            self.generate_box(level, chunk_bb, 5, 1, 51, 5, 3, 53, gray, gray, false);
        }

        if self.chunk_intersects(chunk_bb, 51, 21, 58, 58) {
            self.generate_box(level, chunk_bb, 51, 0, 21, 57, 0, 57, gray, gray, false);
            generate_water_box(self, level, chunk_bb, 51, 1, 21, 57, 7, 57);
            self.generate_box(level, chunk_bb, 51, 4, 21, 53, 4, 53, gray, gray, false);

            for i in 0..4 {
                self.generate_box(
                    level,
                    chunk_bb,
                    57 - i,
                    i + 1,
                    21,
                    57 - i,
                    i + 1,
                    57 - i,
                    light,
                    light,
                    false,
                );
            }

            for z in (23..53).step_by(3) {
                self.place_block(level, light, 52, 5, z, chunk_bb);
            }

            self.place_block(level, light, 52, 5, 52, chunk_bb);
            self.generate_box(level, chunk_bb, 51, 1, 52, 53, 3, 52, gray, gray, false);
            self.generate_box(level, chunk_bb, 52, 1, 51, 52, 3, 53, gray, gray, false);
        }

        if self.chunk_intersects(chunk_bb, 0, 51, 57, 57) {
            self.generate_box(level, chunk_bb, 7, 0, 51, 50, 0, 57, gray, gray, false);
            generate_water_box(self, level, chunk_bb, 7, 1, 51, 50, 10, 57);

            for i in 0..4 {
                self.generate_box(
                    level,
                    chunk_bb,
                    i + 1,
                    i + 1,
                    57 - i,
                    56 - i,
                    i + 1,
                    57 - i,
                    light,
                    light,
                    false,
                );
            }
        }
    }

    /// `generateMiddleWall(level, random, chunkBB)`.
    fn generate_middle_wall(&self, level: &mut WorldGenRegion<'_>, chunk_bb: &BoundingBox) {
        let (gray, light) = (base_gray(), base_light());
        if self.chunk_intersects(chunk_bb, 7, 21, 13, 50) {
            self.generate_box(level, chunk_bb, 7, 0, 21, 13, 0, 50, gray, gray, false);
            generate_water_box(self, level, chunk_bb, 7, 1, 21, 13, 10, 50);
            self.generate_box(level, chunk_bb, 11, 8, 21, 13, 8, 53, gray, gray, false);

            for i in 0..4 {
                self.generate_box(
                    level,
                    chunk_bb,
                    i + 7,
                    i + 5,
                    21,
                    i + 7,
                    i + 5,
                    54,
                    light,
                    light,
                    false,
                );
            }

            for z in (21..=45).step_by(3) {
                self.place_block(level, light, 12, 9, z, chunk_bb);
            }
        }

        if self.chunk_intersects(chunk_bb, 44, 21, 50, 54) {
            self.generate_box(level, chunk_bb, 44, 0, 21, 50, 0, 50, gray, gray, false);
            generate_water_box(self, level, chunk_bb, 44, 1, 21, 50, 10, 50);
            self.generate_box(level, chunk_bb, 44, 8, 21, 46, 8, 53, gray, gray, false);

            for i in 0..4 {
                self.generate_box(
                    level,
                    chunk_bb,
                    50 - i,
                    i + 5,
                    21,
                    50 - i,
                    i + 5,
                    54,
                    light,
                    light,
                    false,
                );
            }

            for z in (21..=45).step_by(3) {
                self.place_block(level, light, 45, 9, z, chunk_bb);
            }
        }

        if self.chunk_intersects(chunk_bb, 8, 44, 49, 54) {
            self.generate_box(level, chunk_bb, 14, 0, 44, 43, 0, 50, gray, gray, false);
            generate_water_box(self, level, chunk_bb, 14, 1, 44, 43, 10, 50);

            for x in (12..=45).step_by(3) {
                self.place_block(level, light, x, 9, 45, chunk_bb);
                self.place_block(level, light, x, 9, 52, chunk_bb);
                if x == 12 || x == 18 || x == 24 || x == 33 || x == 39 || x == 45 {
                    self.place_block(level, light, x, 9, 47, chunk_bb);
                    self.place_block(level, light, x, 9, 50, chunk_bb);
                    self.place_block(level, light, x, 10, 45, chunk_bb);
                    self.place_block(level, light, x, 10, 46, chunk_bb);
                    self.place_block(level, light, x, 10, 51, chunk_bb);
                    self.place_block(level, light, x, 10, 52, chunk_bb);
                    self.place_block(level, light, x, 11, 47, chunk_bb);
                    self.place_block(level, light, x, 11, 50, chunk_bb);
                    self.place_block(level, light, x, 12, 48, chunk_bb);
                    self.place_block(level, light, x, 12, 49, chunk_bb);
                }
            }

            for i in 0..3 {
                self.generate_box(
                    level,
                    chunk_bb,
                    8 + i,
                    5 + i,
                    54,
                    49 - i,
                    5 + i,
                    54,
                    gray,
                    gray,
                    false,
                );
            }

            self.generate_box(level, chunk_bb, 11, 8, 54, 46, 8, 54, light, light, false);
            self.generate_box(level, chunk_bb, 14, 8, 44, 43, 8, 53, gray, gray, false);
        }
    }

    /// `generateUpperWall(level, random, chunkBB)`.
    fn generate_upper_wall(&self, level: &mut WorldGenRegion<'_>, chunk_bb: &BoundingBox) {
        let (gray, light) = (base_gray(), base_light());
        if self.chunk_intersects(chunk_bb, 14, 21, 20, 43) {
            self.generate_box(level, chunk_bb, 14, 0, 21, 20, 0, 43, gray, gray, false);
            generate_water_box(self, level, chunk_bb, 14, 1, 22, 20, 14, 43);
            self.generate_box(level, chunk_bb, 18, 12, 22, 20, 12, 39, gray, gray, false);
            self.generate_box(level, chunk_bb, 18, 12, 21, 20, 12, 21, light, light, false);

            for i in 0..4 {
                self.generate_box(
                    level,
                    chunk_bb,
                    i + 14,
                    i + 9,
                    21,
                    i + 14,
                    i + 9,
                    43 - i,
                    light,
                    light,
                    false,
                );
            }

            for z in (23..=39).step_by(3) {
                self.place_block(level, light, 19, 13, z, chunk_bb);
            }
        }

        if self.chunk_intersects(chunk_bb, 37, 21, 43, 43) {
            self.generate_box(level, chunk_bb, 37, 0, 21, 43, 0, 43, gray, gray, false);
            generate_water_box(self, level, chunk_bb, 37, 1, 22, 43, 14, 43);
            self.generate_box(level, chunk_bb, 37, 12, 22, 39, 12, 39, gray, gray, false);
            self.generate_box(level, chunk_bb, 37, 12, 21, 39, 12, 21, light, light, false);

            for i in 0..4 {
                self.generate_box(
                    level,
                    chunk_bb,
                    43 - i,
                    i + 9,
                    21,
                    43 - i,
                    i + 9,
                    43 - i,
                    light,
                    light,
                    false,
                );
            }

            for z in (23..=39).step_by(3) {
                self.place_block(level, light, 38, 13, z, chunk_bb);
            }
        }

        if self.chunk_intersects(chunk_bb, 15, 37, 42, 43) {
            self.generate_box(level, chunk_bb, 21, 0, 37, 36, 0, 43, gray, gray, false);
            generate_water_box(self, level, chunk_bb, 21, 1, 37, 36, 14, 43);
            self.generate_box(level, chunk_bb, 21, 12, 37, 36, 12, 39, gray, gray, false);

            for i in 0..4 {
                self.generate_box(
                    level,
                    chunk_bb,
                    15 + i,
                    i + 9,
                    43 - i,
                    42 - i,
                    i + 9,
                    43 - i,
                    light,
                    light,
                    false,
                );
            }

            for x in (21..=36).step_by(3) {
                self.place_block(level, light, x, 13, 38, chunk_bb);
            }
        }
    }
}

impl StructurePiece for MonumentBuilding {
    fn base(&self) -> &PieceBase {
        &self.base
    }

    fn base_mut(&mut self) -> &mut PieceBase {
        &mut self.base
    }

    /// `OceanMonumentPiece.addAdditionalSaveData`: writes nothing.
    fn additional_save_data(&self) -> Option<Compound> {
        Some(Compound::new())
    }

    fn post_process(
        &mut self,
        ctx: &mut PieceContext<'_, '_>,
        chunk_bb: &mut BoundingBox,
        chunk_pos: ChunkPos,
        reference: BlockPos,
    ) {
        let light = base_light();
        {
            let chunk_bb: &BoundingBox = chunk_bb;
            let level = &mut *ctx.level;
            let water_height = level.sea_level().max(64) - self.base.bounding_box.min_y();
            generate_water_box(self, level, chunk_bb, 0, 0, 0, 58, water_height, 58);
            self.generate_wing(false, 0, level, chunk_bb);
            self.generate_wing(true, 33, level, chunk_bb);
            self.generate_entrance_archs(level, chunk_bb);
            self.generate_entrance_wall(level, chunk_bb);
            self.generate_roof_piece(level, chunk_bb);
            self.generate_lower_wall(level, chunk_bb);
            self.generate_middle_wall(level, chunk_bb);
            self.generate_upper_wall(level, chunk_bb);
            for pillar_x in 0..7 {
                let mut pillar_z = 0;
                while pillar_z < 7 {
                    if pillar_z == 0 && pillar_x == 3 {
                        pillar_z = 6;
                    }
                    let bx = pillar_x * 9;
                    let bz = pillar_z * 9;
                    for w in 0..4 {
                        for d in 0..4 {
                            self.place_block(level, light, bx + w, 0, bz + d, chunk_bb);
                            self.fill_column_down(level, light, bx + w, -1, bz + d, chunk_bb);
                        }
                    }
                    if pillar_x != 0 && pillar_x != 6 {
                        pillar_z += 6;
                    } else {
                        pillar_z += 1;
                    }
                }
            }
            for i in 0..5 {
                generate_water_box(
                    self,
                    level,
                    chunk_bb,
                    -1 - i,
                    i * 2,
                    -1 - i,
                    -1 - i,
                    23,
                    58 + i,
                );
                generate_water_box(
                    self,
                    level,
                    chunk_bb,
                    58 + i,
                    i * 2,
                    -1 - i,
                    58 + i,
                    23,
                    58 + i,
                );
                generate_water_box(self, level, chunk_bb, -i, i * 2, -1 - i, 57 + i, 23, -1 - i);
                generate_water_box(self, level, chunk_bb, -i, i * 2, 58 + i, 57 + i, 23, 58 + i);
            }
        }
        for child in &mut self.children {
            if child.bounding_box().intersects(chunk_bb) {
                child.post_process(ctx, chunk_bb, chunk_pos, reference);
            }
        }
    }
}

/// The kind of a [`MonumentRoom`], with its own state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoomKind {
    /// `OceanMonumentEntryRoom` (`minecraft:omentry`).
    Entry,
    /// `OceanMonumentCoreRoom` (`minecraft:omcr`).
    Core,
    /// `OceanMonumentDoubleXRoom` (`minecraft:omdxr`).
    DoubleX,
    /// `OceanMonumentDoubleXYRoom` (`minecraft:omdxyr`).
    DoubleXY,
    /// `OceanMonumentDoubleYRoom` (`minecraft:omdyr`).
    DoubleY,
    /// `OceanMonumentDoubleYZRoom` (`minecraft:omdyzr`).
    DoubleYZ,
    /// `OceanMonumentDoubleZRoom` (`minecraft:omdzr`).
    DoubleZ,
    /// `OceanMonumentSimpleRoom` (`minecraft:omsimple`).
    Simple { main_design: i32 },
    /// `OceanMonumentSimpleTopRoom` (`minecraft:omsimplet`).
    SimpleTop,
    /// `OceanMonumentWingRoom` (`minecraft:omwr`).
    Wing { main_design: i32 },
    /// `OceanMonumentPenthouse` (`minecraft:ompenthouse`).
    Penthouse,
}

impl RoomKind {
    /// The `StructurePieceType` id.
    fn id(self) -> &'static str {
        match self {
            Self::Entry => "minecraft:omentry",
            Self::Core => "minecraft:omcr",
            Self::DoubleX => "minecraft:omdxr",
            Self::DoubleXY => "minecraft:omdxyr",
            Self::DoubleY => "minecraft:omdyr",
            Self::DoubleYZ => "minecraft:omdyzr",
            Self::DoubleZ => "minecraft:omdzr",
            Self::Simple { .. } => "minecraft:omsimple",
            Self::SimpleTop => "minecraft:omsimplet",
            Self::Wing { .. } => "minecraft:omwr",
            Self::Penthouse => "minecraft:ompenthouse",
        }
    }

    /// The room's size in grid cells (`roomWidth`, `roomHeight`, `roomDepth`).
    fn size(self) -> (i32, i32, i32) {
        match self {
            Self::Core => (2, 2, 2),
            Self::DoubleX => (2, 1, 1),
            Self::DoubleXY => (2, 2, 1),
            Self::DoubleY => (1, 2, 1),
            Self::DoubleYZ => (1, 2, 2),
            Self::DoubleZ => (1, 1, 2),
            _ => (1, 1, 1),
        }
    }
}

/// An `OceanMonumentPiece` below the building: a room of the graph, a wing or the penthouse.
#[derive(Clone, Debug)]
pub struct MonumentRoom {
    base: PieceBase,
    pub kind: RoomKind,
    /// The room graph (shared by the building's rooms).
    rooms: Arc<[RoomDefinition]>,
    /// `roomDefinition`: the room's index in `rooms` (unused by wings and the penthouse).
    room: usize,
}

impl MonumentRoom {
    /// `new OceanMonumentPiece(type, 1, orientation, roomDefinition, width, height, depth)`.
    fn new(
        kind: RoomKind,
        orientation: Direction,
        rooms: &Arc<[RoomDefinition]>,
        room: usize,
    ) -> Self {
        // `makeBoundingBox(orientation, roomDefinition, roomWidth, roomHeight, roomDepth)`.
        let index = rooms[room].index;
        let (room_x, room_z, room_y) = (index % 5, index / 5 % 5, index / 25);
        let (width, height, depth) = kind.size();
        let mut bounding_box =
            make_bounding_box(0, 0, 0, orientation, width * 8, height * 4, depth * 8);
        match orientation {
            Direction::North => {
                bounding_box.translate(room_x * 8, room_y * 4, -(room_z + depth) * 8 + 1)
            }
            Direction::South => bounding_box.translate(room_x * 8, room_y * 4, room_z * 8),
            Direction::West => {
                bounding_box.translate(-(room_z + depth) * 8 + 1, room_y * 4, room_x * 8)
            }
            _ => bounding_box.translate(room_z * 8, room_y * 4, room_x * 8),
        }
        let mut base = PieceBase::new(kind.id(), 1, bounding_box);
        base.set_orientation(Some(orientation));
        Self {
            base,
            kind,
            rooms: Arc::clone(rooms),
            room,
        }
    }

    /// `new OceanMonumentPiece(type, orientation, 1, boundingBox)`: a wing or the penthouse.
    fn boxed(
        kind: RoomKind,
        orientation: Direction,
        bounding_box: BoundingBox,
        rooms: &Arc<[RoomDefinition]>,
    ) -> Self {
        let mut base = PieceBase::new(kind.id(), 1, bounding_box);
        base.set_orientation(Some(orientation));
        Self {
            base,
            kind,
            rooms: Arc::clone(rooms),
            room: 0,
        }
    }

    /// `generateDefaultFloor(level, chunkBB, xOff, zOff, downOpening)`.
    fn generate_default_floor(
        &self,
        level: &mut WorldGenRegion<'_>,
        chunk_bb: &BoundingBox,
        x_off: i32,
        z_off: i32,
        down_opening: bool,
    ) {
        let (gray, light) = (base_gray(), base_light());
        if down_opening {
            self.generate_box(
                level,
                chunk_bb,
                x_off,
                0,
                z_off,
                x_off + 2,
                0,
                z_off + 8 - 1,
                gray,
                gray,
                false,
            );
            self.generate_box(
                level,
                chunk_bb,
                x_off + 5,
                0,
                z_off,
                x_off + 8 - 1,
                0,
                z_off + 8 - 1,
                gray,
                gray,
                false,
            );
            self.generate_box(
                level,
                chunk_bb,
                x_off + 3,
                0,
                z_off,
                x_off + 4,
                0,
                z_off + 2,
                gray,
                gray,
                false,
            );
            self.generate_box(
                level,
                chunk_bb,
                x_off + 3,
                0,
                z_off + 5,
                x_off + 4,
                0,
                z_off + 8 - 1,
                gray,
                gray,
                false,
            );
            self.generate_box(
                level,
                chunk_bb,
                x_off + 3,
                0,
                z_off + 2,
                x_off + 4,
                0,
                z_off + 2,
                light,
                light,
                false,
            );
            self.generate_box(
                level,
                chunk_bb,
                x_off + 3,
                0,
                z_off + 5,
                x_off + 4,
                0,
                z_off + 5,
                light,
                light,
                false,
            );
            self.generate_box(
                level,
                chunk_bb,
                x_off + 2,
                0,
                z_off + 3,
                x_off + 2,
                0,
                z_off + 4,
                light,
                light,
                false,
            );
            self.generate_box(
                level,
                chunk_bb,
                x_off + 5,
                0,
                z_off + 3,
                x_off + 5,
                0,
                z_off + 4,
                light,
                light,
                false,
            );
        } else {
            self.generate_box(
                level,
                chunk_bb,
                x_off,
                0,
                z_off,
                x_off + 8 - 1,
                0,
                z_off + 8 - 1,
                gray,
                gray,
                false,
            );
        }
    }

    /// `spawnElder(level, chunkBB, x, y, z)`: an elder guardian at the bottom centre of the
    /// block (`finalizeSpawn` draws from the level's random).
    fn spawn_elder(
        &self,
        level: &mut WorldGenRegion<'_>,
        chunk_bb: &BoundingBox,
        x: i32,
        y: i32,
        z: i32,
    ) {
        let pos = self.world_pos(x, y, z);
        if chunk_bb.is_inside(pos) {
            mob_finalize_spawn(level.random());
            level.add_fresh_entity(entity_tag(
                "minecraft:elder_guardian",
                f64::from(pos.x) + 0.5,
                f64::from(pos.y),
                f64::from(pos.z) + 0.5,
            ));
        }
    }

    /// `OceanMonumentCoreRoom.postProcess`.
    fn core_room(&self, ctx: &mut PieceContext<'_, '_>, chunk_bb: &BoundingBox) {
        let (gray, light, black, lamp) = (base_gray(), base_light(), base_black(), lamp_block());
        generate_box_on_fill_only(self, ctx.level, chunk_bb, 1, 8, 0, 14, 8, 14, gray);
        let mut block = light;
        self.generate_box(ctx.level, chunk_bb, 0, 7, 0, 0, 7, 15, block, block, false);
        self.generate_box(
            ctx.level, chunk_bb, 15, 7, 0, 15, 7, 15, block, block, false,
        );
        self.generate_box(ctx.level, chunk_bb, 1, 7, 0, 15, 7, 0, block, block, false);
        self.generate_box(
            ctx.level, chunk_bb, 1, 7, 15, 14, 7, 15, block, block, false,
        );

        for yx in 1..=6 {
            block = light;
            if yx == 2 || yx == 6 {
                block = gray;
            }

            for x in (0..=15).step_by(15) {
                self.generate_box(ctx.level, chunk_bb, x, yx, 0, x, yx, 1, block, block, false);
                self.generate_box(ctx.level, chunk_bb, x, yx, 6, x, yx, 9, block, block, false);
                self.generate_box(
                    ctx.level, chunk_bb, x, yx, 14, x, yx, 15, block, block, false,
                );
            }

            self.generate_box(ctx.level, chunk_bb, 1, yx, 0, 1, yx, 0, block, block, false);
            self.generate_box(ctx.level, chunk_bb, 6, yx, 0, 9, yx, 0, block, block, false);
            self.generate_box(
                ctx.level, chunk_bb, 14, yx, 0, 14, yx, 0, block, block, false,
            );
            self.generate_box(
                ctx.level, chunk_bb, 1, yx, 15, 14, yx, 15, block, block, false,
            );
        }

        self.generate_box(ctx.level, chunk_bb, 6, 3, 6, 9, 6, 9, black, black, false);
        self.generate_box(
            ctx.level,
            chunk_bb,
            7,
            4,
            7,
            8,
            5,
            8,
            ids::GOLD_BLOCK.default_state(),
            ids::GOLD_BLOCK.default_state(),
            false,
        );

        for yx in (3..=6).step_by(3) {
            for x in (6..=9).step_by(3) {
                self.place_block(ctx.level, lamp, x, yx, 6, chunk_bb);
                self.place_block(ctx.level, lamp, x, yx, 9, chunk_bb);
            }
        }

        self.generate_box(ctx.level, chunk_bb, 5, 1, 6, 5, 2, 6, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 5, 1, 9, 5, 2, 9, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 10, 1, 6, 10, 2, 6, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 10, 1, 9, 10, 2, 9, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 6, 1, 5, 6, 2, 5, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 9, 1, 5, 9, 2, 5, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 6, 1, 10, 6, 2, 10, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 9, 1, 10, 9, 2, 10, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 5, 2, 5, 5, 6, 5, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 5, 2, 10, 5, 6, 10, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 10, 2, 5, 10, 6, 5, light, light, false);
        self.generate_box(
            ctx.level, chunk_bb, 10, 2, 10, 10, 6, 10, light, light, false,
        );
        self.generate_box(ctx.level, chunk_bb, 5, 7, 1, 5, 7, 6, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 10, 7, 1, 10, 7, 6, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 5, 7, 9, 5, 7, 14, light, light, false);
        self.generate_box(
            ctx.level, chunk_bb, 10, 7, 9, 10, 7, 14, light, light, false,
        );
        self.generate_box(ctx.level, chunk_bb, 1, 7, 5, 6, 7, 5, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 1, 7, 10, 6, 7, 10, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 9, 7, 5, 14, 7, 5, light, light, false);
        self.generate_box(
            ctx.level, chunk_bb, 9, 7, 10, 14, 7, 10, light, light, false,
        );
        self.generate_box(ctx.level, chunk_bb, 2, 1, 2, 2, 1, 3, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 3, 1, 2, 3, 1, 2, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 13, 1, 2, 13, 1, 3, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 12, 1, 2, 12, 1, 2, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 2, 1, 12, 2, 1, 13, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 3, 1, 13, 3, 1, 13, light, light, false);
        self.generate_box(
            ctx.level, chunk_bb, 13, 1, 12, 13, 1, 13, light, light, false,
        );
        self.generate_box(
            ctx.level, chunk_bb, 12, 1, 13, 12, 1, 13, light, light, false,
        );
    }

    /// `OceanMonumentDoubleXRoom.postProcess`.
    fn double_x_room(&self, ctx: &mut PieceContext<'_, '_>, chunk_bb: &BoundingBox) {
        let (gray, light, lamp) = (base_gray(), base_light(), lamp_block());
        let rooms = &*self.rooms;
        let room = &rooms[self.room];
        let east = &rooms[room.connection(Direction::East)];
        let west = room;
        if room.index / 25 > 0 {
            self.generate_default_floor(ctx.level, chunk_bb, 8, 0, east.opens(Direction::Down));
            self.generate_default_floor(ctx.level, chunk_bb, 0, 0, west.opens(Direction::Down));
        }

        if !west.connects(Direction::Up) {
            generate_box_on_fill_only(self, ctx.level, chunk_bb, 1, 4, 1, 7, 4, 6, gray);
        }

        if !east.connects(Direction::Up) {
            generate_box_on_fill_only(self, ctx.level, chunk_bb, 8, 4, 1, 14, 4, 6, gray);
        }

        self.generate_box(ctx.level, chunk_bb, 0, 3, 0, 0, 3, 7, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 15, 3, 0, 15, 3, 7, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 1, 3, 0, 15, 3, 0, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 1, 3, 7, 14, 3, 7, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 0, 2, 0, 0, 2, 7, gray, gray, false);
        self.generate_box(ctx.level, chunk_bb, 15, 2, 0, 15, 2, 7, gray, gray, false);
        self.generate_box(ctx.level, chunk_bb, 1, 2, 0, 15, 2, 0, gray, gray, false);
        self.generate_box(ctx.level, chunk_bb, 1, 2, 7, 14, 2, 7, gray, gray, false);
        self.generate_box(ctx.level, chunk_bb, 0, 1, 0, 0, 1, 7, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 15, 1, 0, 15, 1, 7, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 1, 1, 0, 15, 1, 0, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 1, 1, 7, 14, 1, 7, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 5, 1, 0, 10, 1, 4, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 6, 2, 0, 9, 2, 3, gray, gray, false);
        self.generate_box(ctx.level, chunk_bb, 5, 3, 0, 10, 3, 4, light, light, false);
        self.place_block(ctx.level, lamp, 6, 2, 3, chunk_bb);
        self.place_block(ctx.level, lamp, 9, 2, 3, chunk_bb);
        if west.opens(Direction::South) {
            generate_water_box(self, ctx.level, chunk_bb, 3, 1, 0, 4, 2, 0);
        }

        if west.opens(Direction::North) {
            generate_water_box(self, ctx.level, chunk_bb, 3, 1, 7, 4, 2, 7);
        }

        if west.opens(Direction::West) {
            generate_water_box(self, ctx.level, chunk_bb, 0, 1, 3, 0, 2, 4);
        }

        if east.opens(Direction::South) {
            generate_water_box(self, ctx.level, chunk_bb, 11, 1, 0, 12, 2, 0);
        }

        if east.opens(Direction::North) {
            generate_water_box(self, ctx.level, chunk_bb, 11, 1, 7, 12, 2, 7);
        }

        if east.opens(Direction::East) {
            generate_water_box(self, ctx.level, chunk_bb, 15, 1, 3, 15, 2, 4);
        }
    }

    /// `OceanMonumentDoubleXYRoom.postProcess`.
    fn double_xy_room(&self, ctx: &mut PieceContext<'_, '_>, chunk_bb: &BoundingBox) {
        let (gray, light, lamp) = (base_gray(), base_light(), lamp_block());
        let rooms = &*self.rooms;
        let room = &rooms[self.room];
        let east = &rooms[room.connection(Direction::East)];
        let west = room;
        let west_up = &rooms[west.connection(Direction::Up)];
        let east_up = &rooms[east.connection(Direction::Up)];
        if room.index / 25 > 0 {
            self.generate_default_floor(ctx.level, chunk_bb, 8, 0, east.opens(Direction::Down));
            self.generate_default_floor(ctx.level, chunk_bb, 0, 0, west.opens(Direction::Down));
        }

        if !west_up.connects(Direction::Up) {
            generate_box_on_fill_only(self, ctx.level, chunk_bb, 1, 8, 1, 7, 8, 6, gray);
        }

        if !east_up.connects(Direction::Up) {
            generate_box_on_fill_only(self, ctx.level, chunk_bb, 8, 8, 1, 14, 8, 6, gray);
        }

        for y in 1..=7 {
            let mut block = light;
            if y == 2 || y == 6 {
                block = gray;
            }

            self.generate_box(ctx.level, chunk_bb, 0, y, 0, 0, y, 7, block, block, false);
            self.generate_box(ctx.level, chunk_bb, 15, y, 0, 15, y, 7, block, block, false);
            self.generate_box(ctx.level, chunk_bb, 1, y, 0, 15, y, 0, block, block, false);
            self.generate_box(ctx.level, chunk_bb, 1, y, 7, 14, y, 7, block, block, false);
        }

        self.generate_box(ctx.level, chunk_bb, 2, 1, 3, 2, 7, 4, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 3, 1, 2, 4, 7, 2, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 3, 1, 5, 4, 7, 5, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 13, 1, 3, 13, 7, 4, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 11, 1, 2, 12, 7, 2, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 11, 1, 5, 12, 7, 5, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 5, 1, 3, 5, 3, 4, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 10, 1, 3, 10, 3, 4, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 5, 7, 2, 10, 7, 5, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 5, 5, 2, 5, 7, 2, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 10, 5, 2, 10, 7, 2, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 5, 5, 5, 5, 7, 5, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 10, 5, 5, 10, 7, 5, light, light, false);
        self.place_block(ctx.level, light, 6, 6, 2, chunk_bb);
        self.place_block(ctx.level, light, 9, 6, 2, chunk_bb);
        self.place_block(ctx.level, light, 6, 6, 5, chunk_bb);
        self.place_block(ctx.level, light, 9, 6, 5, chunk_bb);
        self.generate_box(ctx.level, chunk_bb, 5, 4, 3, 6, 4, 4, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 9, 4, 3, 10, 4, 4, light, light, false);
        self.place_block(ctx.level, lamp, 5, 4, 2, chunk_bb);
        self.place_block(ctx.level, lamp, 5, 4, 5, chunk_bb);
        self.place_block(ctx.level, lamp, 10, 4, 2, chunk_bb);
        self.place_block(ctx.level, lamp, 10, 4, 5, chunk_bb);
        if west.opens(Direction::South) {
            generate_water_box(self, ctx.level, chunk_bb, 3, 1, 0, 4, 2, 0);
        }

        if west.opens(Direction::North) {
            generate_water_box(self, ctx.level, chunk_bb, 3, 1, 7, 4, 2, 7);
        }

        if west.opens(Direction::West) {
            generate_water_box(self, ctx.level, chunk_bb, 0, 1, 3, 0, 2, 4);
        }

        if east.opens(Direction::South) {
            generate_water_box(self, ctx.level, chunk_bb, 11, 1, 0, 12, 2, 0);
        }

        if east.opens(Direction::North) {
            generate_water_box(self, ctx.level, chunk_bb, 11, 1, 7, 12, 2, 7);
        }

        if east.opens(Direction::East) {
            generate_water_box(self, ctx.level, chunk_bb, 15, 1, 3, 15, 2, 4);
        }

        if west_up.opens(Direction::South) {
            generate_water_box(self, ctx.level, chunk_bb, 3, 5, 0, 4, 6, 0);
        }

        if west_up.opens(Direction::North) {
            generate_water_box(self, ctx.level, chunk_bb, 3, 5, 7, 4, 6, 7);
        }

        if west_up.opens(Direction::West) {
            generate_water_box(self, ctx.level, chunk_bb, 0, 5, 3, 0, 6, 4);
        }

        if east_up.opens(Direction::South) {
            generate_water_box(self, ctx.level, chunk_bb, 11, 5, 0, 12, 6, 0);
        }

        if east_up.opens(Direction::North) {
            generate_water_box(self, ctx.level, chunk_bb, 11, 5, 7, 12, 6, 7);
        }

        if east_up.opens(Direction::East) {
            generate_water_box(self, ctx.level, chunk_bb, 15, 5, 3, 15, 6, 4);
        }
    }

    /// `OceanMonumentDoubleYRoom.postProcess`.
    fn double_y_room(&self, ctx: &mut PieceContext<'_, '_>, chunk_bb: &BoundingBox) {
        let (gray, light) = (base_gray(), base_light());
        let rooms = &*self.rooms;
        let room = &rooms[self.room];
        if room.index / 25 > 0 {
            self.generate_default_floor(ctx.level, chunk_bb, 0, 0, room.opens(Direction::Down));
        }

        let above = &rooms[room.connection(Direction::Up)];
        if !above.connects(Direction::Up) {
            generate_box_on_fill_only(self, ctx.level, chunk_bb, 1, 8, 1, 6, 8, 6, gray);
        }

        self.generate_box(ctx.level, chunk_bb, 0, 4, 0, 0, 4, 7, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 7, 4, 0, 7, 4, 7, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 1, 4, 0, 6, 4, 0, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 1, 4, 7, 6, 4, 7, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 2, 4, 1, 2, 4, 2, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 1, 4, 2, 1, 4, 2, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 5, 4, 1, 5, 4, 2, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 6, 4, 2, 6, 4, 2, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 2, 4, 5, 2, 4, 6, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 1, 4, 5, 1, 4, 5, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 5, 4, 5, 5, 4, 6, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 6, 4, 5, 6, 4, 5, light, light, false);
        let mut definition = room;

        for y in (1..=5).step_by(4) {
            let z = 0;
            if definition.opens(Direction::South) {
                self.generate_box(
                    ctx.level,
                    chunk_bb,
                    2,
                    y,
                    z,
                    2,
                    y + 2,
                    z,
                    light,
                    light,
                    false,
                );
                self.generate_box(
                    ctx.level,
                    chunk_bb,
                    5,
                    y,
                    z,
                    5,
                    y + 2,
                    z,
                    light,
                    light,
                    false,
                );
                self.generate_box(
                    ctx.level,
                    chunk_bb,
                    3,
                    y + 2,
                    z,
                    4,
                    y + 2,
                    z,
                    light,
                    light,
                    false,
                );
            } else {
                self.generate_box(
                    ctx.level,
                    chunk_bb,
                    0,
                    y,
                    z,
                    7,
                    y + 2,
                    z,
                    light,
                    light,
                    false,
                );
                self.generate_box(
                    ctx.level,
                    chunk_bb,
                    0,
                    y + 1,
                    z,
                    7,
                    y + 1,
                    z,
                    gray,
                    gray,
                    false,
                );
            }

            let z = 7;
            if definition.opens(Direction::North) {
                self.generate_box(
                    ctx.level,
                    chunk_bb,
                    2,
                    y,
                    z,
                    2,
                    y + 2,
                    z,
                    light,
                    light,
                    false,
                );
                self.generate_box(
                    ctx.level,
                    chunk_bb,
                    5,
                    y,
                    z,
                    5,
                    y + 2,
                    z,
                    light,
                    light,
                    false,
                );
                self.generate_box(
                    ctx.level,
                    chunk_bb,
                    3,
                    y + 2,
                    z,
                    4,
                    y + 2,
                    z,
                    light,
                    light,
                    false,
                );
            } else {
                self.generate_box(
                    ctx.level,
                    chunk_bb,
                    0,
                    y,
                    z,
                    7,
                    y + 2,
                    z,
                    light,
                    light,
                    false,
                );
                self.generate_box(
                    ctx.level,
                    chunk_bb,
                    0,
                    y + 1,
                    z,
                    7,
                    y + 1,
                    z,
                    gray,
                    gray,
                    false,
                );
            }

            let x = 0;
            if definition.opens(Direction::West) {
                self.generate_box(
                    ctx.level,
                    chunk_bb,
                    x,
                    y,
                    2,
                    x,
                    y + 2,
                    2,
                    light,
                    light,
                    false,
                );
                self.generate_box(
                    ctx.level,
                    chunk_bb,
                    x,
                    y,
                    5,
                    x,
                    y + 2,
                    5,
                    light,
                    light,
                    false,
                );
                self.generate_box(
                    ctx.level,
                    chunk_bb,
                    x,
                    y + 2,
                    3,
                    x,
                    y + 2,
                    4,
                    light,
                    light,
                    false,
                );
            } else {
                self.generate_box(
                    ctx.level,
                    chunk_bb,
                    x,
                    y,
                    0,
                    x,
                    y + 2,
                    7,
                    light,
                    light,
                    false,
                );
                self.generate_box(
                    ctx.level,
                    chunk_bb,
                    x,
                    y + 1,
                    0,
                    x,
                    y + 1,
                    7,
                    gray,
                    gray,
                    false,
                );
            }

            let x = 7;
            if definition.opens(Direction::East) {
                self.generate_box(
                    ctx.level,
                    chunk_bb,
                    x,
                    y,
                    2,
                    x,
                    y + 2,
                    2,
                    light,
                    light,
                    false,
                );
                self.generate_box(
                    ctx.level,
                    chunk_bb,
                    x,
                    y,
                    5,
                    x,
                    y + 2,
                    5,
                    light,
                    light,
                    false,
                );
                self.generate_box(
                    ctx.level,
                    chunk_bb,
                    x,
                    y + 2,
                    3,
                    x,
                    y + 2,
                    4,
                    light,
                    light,
                    false,
                );
            } else {
                self.generate_box(
                    ctx.level,
                    chunk_bb,
                    x,
                    y,
                    0,
                    x,
                    y + 2,
                    7,
                    light,
                    light,
                    false,
                );
                self.generate_box(
                    ctx.level,
                    chunk_bb,
                    x,
                    y + 1,
                    0,
                    x,
                    y + 1,
                    7,
                    gray,
                    gray,
                    false,
                );
            }

            definition = above;
        }
    }

    /// `OceanMonumentDoubleYZRoom.postProcess`.
    fn double_yz_room(&self, ctx: &mut PieceContext<'_, '_>, chunk_bb: &BoundingBox) {
        let (gray, light, black, lamp) = (base_gray(), base_light(), base_black(), lamp_block());
        let rooms = &*self.rooms;
        let room = &rooms[self.room];
        let north = &rooms[room.connection(Direction::North)];
        let south = room;
        let north_up = &rooms[north.connection(Direction::Up)];
        let south_up = &rooms[south.connection(Direction::Up)];
        if room.index / 25 > 0 {
            self.generate_default_floor(ctx.level, chunk_bb, 0, 8, north.opens(Direction::Down));
            self.generate_default_floor(ctx.level, chunk_bb, 0, 0, south.opens(Direction::Down));
        }

        if !south_up.connects(Direction::Up) {
            generate_box_on_fill_only(self, ctx.level, chunk_bb, 1, 8, 1, 6, 8, 7, gray);
        }

        if !north_up.connects(Direction::Up) {
            generate_box_on_fill_only(self, ctx.level, chunk_bb, 1, 8, 8, 6, 8, 14, gray);
        }

        for y in 1..=7 {
            let mut block = light;
            if y == 2 || y == 6 {
                block = gray;
            }

            self.generate_box(ctx.level, chunk_bb, 0, y, 0, 0, y, 15, block, block, false);
            self.generate_box(ctx.level, chunk_bb, 7, y, 0, 7, y, 15, block, block, false);
            self.generate_box(ctx.level, chunk_bb, 1, y, 0, 6, y, 0, block, block, false);
            self.generate_box(ctx.level, chunk_bb, 1, y, 15, 6, y, 15, block, block, false);
        }

        for y in 1..=7 {
            let mut block = black;
            if y == 2 || y == 6 {
                block = lamp;
            }

            self.generate_box(ctx.level, chunk_bb, 3, y, 7, 4, y, 8, block, block, false);
        }

        if south.opens(Direction::South) {
            generate_water_box(self, ctx.level, chunk_bb, 3, 1, 0, 4, 2, 0);
        }

        if south.opens(Direction::East) {
            generate_water_box(self, ctx.level, chunk_bb, 7, 1, 3, 7, 2, 4);
        }

        if south.opens(Direction::West) {
            generate_water_box(self, ctx.level, chunk_bb, 0, 1, 3, 0, 2, 4);
        }

        if north.opens(Direction::North) {
            generate_water_box(self, ctx.level, chunk_bb, 3, 1, 15, 4, 2, 15);
        }

        if north.opens(Direction::West) {
            generate_water_box(self, ctx.level, chunk_bb, 0, 1, 11, 0, 2, 12);
        }

        if north.opens(Direction::East) {
            generate_water_box(self, ctx.level, chunk_bb, 7, 1, 11, 7, 2, 12);
        }

        if south_up.opens(Direction::South) {
            generate_water_box(self, ctx.level, chunk_bb, 3, 5, 0, 4, 6, 0);
        }

        if south_up.opens(Direction::East) {
            generate_water_box(self, ctx.level, chunk_bb, 7, 5, 3, 7, 6, 4);
            self.generate_box(ctx.level, chunk_bb, 5, 4, 2, 6, 4, 5, light, light, false);
            self.generate_box(ctx.level, chunk_bb, 6, 1, 2, 6, 3, 2, light, light, false);
            self.generate_box(ctx.level, chunk_bb, 6, 1, 5, 6, 3, 5, light, light, false);
        }

        if south_up.opens(Direction::West) {
            generate_water_box(self, ctx.level, chunk_bb, 0, 5, 3, 0, 6, 4);
            self.generate_box(ctx.level, chunk_bb, 1, 4, 2, 2, 4, 5, light, light, false);
            self.generate_box(ctx.level, chunk_bb, 1, 1, 2, 1, 3, 2, light, light, false);
            self.generate_box(ctx.level, chunk_bb, 1, 1, 5, 1, 3, 5, light, light, false);
        }

        if north_up.opens(Direction::North) {
            generate_water_box(self, ctx.level, chunk_bb, 3, 5, 15, 4, 6, 15);
        }

        if north_up.opens(Direction::West) {
            generate_water_box(self, ctx.level, chunk_bb, 0, 5, 11, 0, 6, 12);
            self.generate_box(ctx.level, chunk_bb, 1, 4, 10, 2, 4, 13, light, light, false);
            self.generate_box(ctx.level, chunk_bb, 1, 1, 10, 1, 3, 10, light, light, false);
            self.generate_box(ctx.level, chunk_bb, 1, 1, 13, 1, 3, 13, light, light, false);
        }

        if north_up.opens(Direction::East) {
            generate_water_box(self, ctx.level, chunk_bb, 7, 5, 11, 7, 6, 12);
            self.generate_box(ctx.level, chunk_bb, 5, 4, 10, 6, 4, 13, light, light, false);
            self.generate_box(ctx.level, chunk_bb, 6, 1, 10, 6, 3, 10, light, light, false);
            self.generate_box(ctx.level, chunk_bb, 6, 1, 13, 6, 3, 13, light, light, false);
        }
    }

    /// `OceanMonumentDoubleZRoom.postProcess`.
    fn double_z_room(&self, ctx: &mut PieceContext<'_, '_>, chunk_bb: &BoundingBox) {
        let (gray, light, lamp) = (base_gray(), base_light(), lamp_block());
        let rooms = &*self.rooms;
        let room = &rooms[self.room];
        let north = &rooms[room.connection(Direction::North)];
        let south = room;
        if room.index / 25 > 0 {
            self.generate_default_floor(ctx.level, chunk_bb, 0, 8, north.opens(Direction::Down));
            self.generate_default_floor(ctx.level, chunk_bb, 0, 0, south.opens(Direction::Down));
        }

        if !south.connects(Direction::Up) {
            generate_box_on_fill_only(self, ctx.level, chunk_bb, 1, 4, 1, 6, 4, 7, gray);
        }

        if !north.connects(Direction::Up) {
            generate_box_on_fill_only(self, ctx.level, chunk_bb, 1, 4, 8, 6, 4, 14, gray);
        }

        self.generate_box(ctx.level, chunk_bb, 0, 3, 0, 0, 3, 15, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 7, 3, 0, 7, 3, 15, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 1, 3, 0, 7, 3, 0, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 1, 3, 15, 6, 3, 15, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 0, 2, 0, 0, 2, 15, gray, gray, false);
        self.generate_box(ctx.level, chunk_bb, 7, 2, 0, 7, 2, 15, gray, gray, false);
        self.generate_box(ctx.level, chunk_bb, 1, 2, 0, 7, 2, 0, gray, gray, false);
        self.generate_box(ctx.level, chunk_bb, 1, 2, 15, 6, 2, 15, gray, gray, false);
        self.generate_box(ctx.level, chunk_bb, 0, 1, 0, 0, 1, 15, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 7, 1, 0, 7, 1, 15, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 1, 1, 0, 7, 1, 0, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 1, 1, 15, 6, 1, 15, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 1, 1, 1, 1, 1, 2, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 6, 1, 1, 6, 1, 2, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 1, 3, 1, 1, 3, 2, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 6, 3, 1, 6, 3, 2, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 1, 1, 13, 1, 1, 14, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 6, 1, 13, 6, 1, 14, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 1, 3, 13, 1, 3, 14, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 6, 3, 13, 6, 3, 14, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 2, 1, 6, 2, 3, 6, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 5, 1, 6, 5, 3, 6, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 2, 1, 9, 2, 3, 9, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 5, 1, 9, 5, 3, 9, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 3, 2, 6, 4, 2, 6, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 3, 2, 9, 4, 2, 9, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 2, 2, 7, 2, 2, 8, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 5, 2, 7, 5, 2, 8, light, light, false);
        self.place_block(ctx.level, lamp, 2, 2, 5, chunk_bb);
        self.place_block(ctx.level, lamp, 5, 2, 5, chunk_bb);
        self.place_block(ctx.level, lamp, 2, 2, 10, chunk_bb);
        self.place_block(ctx.level, lamp, 5, 2, 10, chunk_bb);
        self.place_block(ctx.level, light, 2, 3, 5, chunk_bb);
        self.place_block(ctx.level, light, 5, 3, 5, chunk_bb);
        self.place_block(ctx.level, light, 2, 3, 10, chunk_bb);
        self.place_block(ctx.level, light, 5, 3, 10, chunk_bb);
        if south.opens(Direction::South) {
            generate_water_box(self, ctx.level, chunk_bb, 3, 1, 0, 4, 2, 0);
        }

        if south.opens(Direction::East) {
            generate_water_box(self, ctx.level, chunk_bb, 7, 1, 3, 7, 2, 4);
        }

        if south.opens(Direction::West) {
            generate_water_box(self, ctx.level, chunk_bb, 0, 1, 3, 0, 2, 4);
        }

        if north.opens(Direction::North) {
            generate_water_box(self, ctx.level, chunk_bb, 3, 1, 15, 4, 2, 15);
        }

        if north.opens(Direction::West) {
            generate_water_box(self, ctx.level, chunk_bb, 0, 1, 11, 0, 2, 12);
        }

        if north.opens(Direction::East) {
            generate_water_box(self, ctx.level, chunk_bb, 7, 1, 11, 7, 2, 12);
        }
    }

    /// `OceanMonumentEntryRoom.postProcess`.
    fn entry_room(&self, ctx: &mut PieceContext<'_, '_>, chunk_bb: &BoundingBox) {
        let light = base_light();
        let room = &self.rooms[self.room];
        self.generate_box(ctx.level, chunk_bb, 0, 3, 0, 2, 3, 7, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 5, 3, 0, 7, 3, 7, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 0, 2, 0, 1, 2, 7, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 6, 2, 0, 7, 2, 7, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 0, 1, 0, 0, 1, 7, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 7, 1, 0, 7, 1, 7, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 0, 1, 7, 7, 3, 7, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 1, 1, 0, 2, 3, 0, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 5, 1, 0, 6, 3, 0, light, light, false);
        if room.opens(Direction::North) {
            generate_water_box(self, ctx.level, chunk_bb, 3, 1, 7, 4, 2, 7);
        }

        if room.opens(Direction::West) {
            generate_water_box(self, ctx.level, chunk_bb, 0, 1, 3, 1, 2, 4);
        }

        if room.opens(Direction::East) {
            generate_water_box(self, ctx.level, chunk_bb, 6, 1, 3, 7, 2, 4);
        }
    }

    /// `OceanMonumentPenthouse.postProcess`.
    fn penthouse(&self, ctx: &mut PieceContext<'_, '_>, chunk_bb: &BoundingBox) {
        let (gray, light, black, lamp) = (base_gray(), base_light(), base_black(), lamp_block());
        self.generate_box(
            ctx.level, chunk_bb, 2, -1, 2, 11, -1, 11, light, light, false,
        );
        self.generate_box(ctx.level, chunk_bb, 0, -1, 0, 1, -1, 11, gray, gray, false);
        self.generate_box(
            ctx.level, chunk_bb, 12, -1, 0, 13, -1, 11, gray, gray, false,
        );
        self.generate_box(ctx.level, chunk_bb, 2, -1, 0, 11, -1, 1, gray, gray, false);
        self.generate_box(
            ctx.level, chunk_bb, 2, -1, 12, 11, -1, 13, gray, gray, false,
        );
        self.generate_box(ctx.level, chunk_bb, 0, 0, 0, 0, 0, 13, light, light, false);
        self.generate_box(
            ctx.level, chunk_bb, 13, 0, 0, 13, 0, 13, light, light, false,
        );
        self.generate_box(ctx.level, chunk_bb, 1, 0, 0, 12, 0, 0, light, light, false);
        self.generate_box(
            ctx.level, chunk_bb, 1, 0, 13, 12, 0, 13, light, light, false,
        );

        for i in (2..=11).step_by(3) {
            self.place_block(ctx.level, lamp, 0, 0, i, chunk_bb);
            self.place_block(ctx.level, lamp, 13, 0, i, chunk_bb);
            self.place_block(ctx.level, lamp, i, 0, 0, chunk_bb);
        }

        self.generate_box(ctx.level, chunk_bb, 2, 0, 3, 4, 0, 9, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 9, 0, 3, 11, 0, 9, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 4, 0, 9, 9, 0, 11, light, light, false);
        self.place_block(ctx.level, light, 5, 0, 8, chunk_bb);
        self.place_block(ctx.level, light, 8, 0, 8, chunk_bb);
        self.place_block(ctx.level, light, 10, 0, 10, chunk_bb);
        self.place_block(ctx.level, light, 3, 0, 10, chunk_bb);
        self.generate_box(ctx.level, chunk_bb, 3, 0, 3, 3, 0, 7, black, black, false);
        self.generate_box(ctx.level, chunk_bb, 10, 0, 3, 10, 0, 7, black, black, false);
        self.generate_box(ctx.level, chunk_bb, 6, 0, 10, 7, 0, 10, black, black, false);
        let mut x = 3;

        for _ in 0..2 {
            for z in (2..=8).step_by(3) {
                self.generate_box(ctx.level, chunk_bb, x, 0, z, x, 2, z, light, light, false);
            }

            x = 10;
        }

        self.generate_box(ctx.level, chunk_bb, 5, 0, 10, 5, 2, 10, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 8, 0, 10, 8, 2, 10, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 6, -1, 7, 7, -1, 8, black, black, false);
        generate_water_box(self, ctx.level, chunk_bb, 6, -1, 3, 7, -1, 4);
        self.spawn_elder(ctx.level, chunk_bb, 6, 1, 6);
    }

    /// `OceanMonumentSimpleRoom.postProcess`.
    fn simple_room(
        &self,
        ctx: &mut PieceContext<'_, '_>,
        chunk_bb: &BoundingBox,
        main_design: i32,
    ) {
        let (gray, light, black, lamp) = (base_gray(), base_light(), base_black(), lamp_block());
        let room = &self.rooms[self.room];
        if room.index / 25 > 0 {
            self.generate_default_floor(ctx.level, chunk_bb, 0, 0, room.opens(Direction::Down));
        }

        if !room.connects(Direction::Up) {
            generate_box_on_fill_only(self, ctx.level, chunk_bb, 1, 4, 1, 6, 4, 6, gray);
        }

        let center_pillar = main_design != 0
            && ctx.random.next_boolean()
            && !room.opens(Direction::Down)
            && !room.opens(Direction::Up)
            && room.count_openings() > 1;
        if main_design == 0 {
            self.generate_box(ctx.level, chunk_bb, 0, 1, 0, 2, 1, 2, light, light, false);
            self.generate_box(ctx.level, chunk_bb, 0, 3, 0, 2, 3, 2, light, light, false);
            self.generate_box(ctx.level, chunk_bb, 0, 2, 0, 0, 2, 2, gray, gray, false);
            self.generate_box(ctx.level, chunk_bb, 1, 2, 0, 2, 2, 0, gray, gray, false);
            self.place_block(ctx.level, lamp, 1, 2, 1, chunk_bb);
            self.generate_box(ctx.level, chunk_bb, 5, 1, 0, 7, 1, 2, light, light, false);
            self.generate_box(ctx.level, chunk_bb, 5, 3, 0, 7, 3, 2, light, light, false);
            self.generate_box(ctx.level, chunk_bb, 7, 2, 0, 7, 2, 2, gray, gray, false);
            self.generate_box(ctx.level, chunk_bb, 5, 2, 0, 6, 2, 0, gray, gray, false);
            self.place_block(ctx.level, lamp, 6, 2, 1, chunk_bb);
            self.generate_box(ctx.level, chunk_bb, 0, 1, 5, 2, 1, 7, light, light, false);
            self.generate_box(ctx.level, chunk_bb, 0, 3, 5, 2, 3, 7, light, light, false);
            self.generate_box(ctx.level, chunk_bb, 0, 2, 5, 0, 2, 7, gray, gray, false);
            self.generate_box(ctx.level, chunk_bb, 1, 2, 7, 2, 2, 7, gray, gray, false);
            self.place_block(ctx.level, lamp, 1, 2, 6, chunk_bb);
            self.generate_box(ctx.level, chunk_bb, 5, 1, 5, 7, 1, 7, light, light, false);
            self.generate_box(ctx.level, chunk_bb, 5, 3, 5, 7, 3, 7, light, light, false);
            self.generate_box(ctx.level, chunk_bb, 7, 2, 5, 7, 2, 7, gray, gray, false);
            self.generate_box(ctx.level, chunk_bb, 5, 2, 7, 6, 2, 7, gray, gray, false);
            self.place_block(ctx.level, lamp, 6, 2, 6, chunk_bb);
            if room.opens(Direction::South) {
                self.generate_box(ctx.level, chunk_bb, 3, 3, 0, 4, 3, 0, light, light, false);
            } else {
                self.generate_box(ctx.level, chunk_bb, 3, 3, 0, 4, 3, 1, light, light, false);
                self.generate_box(ctx.level, chunk_bb, 3, 2, 0, 4, 2, 0, gray, gray, false);
                self.generate_box(ctx.level, chunk_bb, 3, 1, 0, 4, 1, 1, light, light, false);
            }

            if room.opens(Direction::North) {
                self.generate_box(ctx.level, chunk_bb, 3, 3, 7, 4, 3, 7, light, light, false);
            } else {
                self.generate_box(ctx.level, chunk_bb, 3, 3, 6, 4, 3, 7, light, light, false);
                self.generate_box(ctx.level, chunk_bb, 3, 2, 7, 4, 2, 7, gray, gray, false);
                self.generate_box(ctx.level, chunk_bb, 3, 1, 6, 4, 1, 7, light, light, false);
            }

            if room.opens(Direction::West) {
                self.generate_box(ctx.level, chunk_bb, 0, 3, 3, 0, 3, 4, light, light, false);
            } else {
                self.generate_box(ctx.level, chunk_bb, 0, 3, 3, 1, 3, 4, light, light, false);
                self.generate_box(ctx.level, chunk_bb, 0, 2, 3, 0, 2, 4, gray, gray, false);
                self.generate_box(ctx.level, chunk_bb, 0, 1, 3, 1, 1, 4, light, light, false);
            }

            if room.opens(Direction::East) {
                self.generate_box(ctx.level, chunk_bb, 7, 3, 3, 7, 3, 4, light, light, false);
            } else {
                self.generate_box(ctx.level, chunk_bb, 6, 3, 3, 7, 3, 4, light, light, false);
                self.generate_box(ctx.level, chunk_bb, 7, 2, 3, 7, 2, 4, gray, gray, false);
                self.generate_box(ctx.level, chunk_bb, 6, 1, 3, 7, 1, 4, light, light, false);
            }
        } else if main_design == 1 {
            self.generate_box(ctx.level, chunk_bb, 2, 1, 2, 2, 3, 2, light, light, false);
            self.generate_box(ctx.level, chunk_bb, 2, 1, 5, 2, 3, 5, light, light, false);
            self.generate_box(ctx.level, chunk_bb, 5, 1, 5, 5, 3, 5, light, light, false);
            self.generate_box(ctx.level, chunk_bb, 5, 1, 2, 5, 3, 2, light, light, false);
            self.place_block(ctx.level, lamp, 2, 2, 2, chunk_bb);
            self.place_block(ctx.level, lamp, 2, 2, 5, chunk_bb);
            self.place_block(ctx.level, lamp, 5, 2, 5, chunk_bb);
            self.place_block(ctx.level, lamp, 5, 2, 2, chunk_bb);
            self.generate_box(ctx.level, chunk_bb, 0, 1, 0, 1, 3, 0, light, light, false);
            self.generate_box(ctx.level, chunk_bb, 0, 1, 1, 0, 3, 1, light, light, false);
            self.generate_box(ctx.level, chunk_bb, 0, 1, 7, 1, 3, 7, light, light, false);
            self.generate_box(ctx.level, chunk_bb, 0, 1, 6, 0, 3, 6, light, light, false);
            self.generate_box(ctx.level, chunk_bb, 6, 1, 7, 7, 3, 7, light, light, false);
            self.generate_box(ctx.level, chunk_bb, 7, 1, 6, 7, 3, 6, light, light, false);
            self.generate_box(ctx.level, chunk_bb, 6, 1, 0, 7, 3, 0, light, light, false);
            self.generate_box(ctx.level, chunk_bb, 7, 1, 1, 7, 3, 1, light, light, false);
            self.place_block(ctx.level, gray, 1, 2, 0, chunk_bb);
            self.place_block(ctx.level, gray, 0, 2, 1, chunk_bb);
            self.place_block(ctx.level, gray, 1, 2, 7, chunk_bb);
            self.place_block(ctx.level, gray, 0, 2, 6, chunk_bb);
            self.place_block(ctx.level, gray, 6, 2, 7, chunk_bb);
            self.place_block(ctx.level, gray, 7, 2, 6, chunk_bb);
            self.place_block(ctx.level, gray, 6, 2, 0, chunk_bb);
            self.place_block(ctx.level, gray, 7, 2, 1, chunk_bb);
            if !room.opens(Direction::South) {
                self.generate_box(ctx.level, chunk_bb, 1, 3, 0, 6, 3, 0, light, light, false);
                self.generate_box(ctx.level, chunk_bb, 1, 2, 0, 6, 2, 0, gray, gray, false);
                self.generate_box(ctx.level, chunk_bb, 1, 1, 0, 6, 1, 0, light, light, false);
            }

            if !room.opens(Direction::North) {
                self.generate_box(ctx.level, chunk_bb, 1, 3, 7, 6, 3, 7, light, light, false);
                self.generate_box(ctx.level, chunk_bb, 1, 2, 7, 6, 2, 7, gray, gray, false);
                self.generate_box(ctx.level, chunk_bb, 1, 1, 7, 6, 1, 7, light, light, false);
            }

            if !room.opens(Direction::West) {
                self.generate_box(ctx.level, chunk_bb, 0, 3, 1, 0, 3, 6, light, light, false);
                self.generate_box(ctx.level, chunk_bb, 0, 2, 1, 0, 2, 6, gray, gray, false);
                self.generate_box(ctx.level, chunk_bb, 0, 1, 1, 0, 1, 6, light, light, false);
            }

            if !room.opens(Direction::East) {
                self.generate_box(ctx.level, chunk_bb, 7, 3, 1, 7, 3, 6, light, light, false);
                self.generate_box(ctx.level, chunk_bb, 7, 2, 1, 7, 2, 6, gray, gray, false);
                self.generate_box(ctx.level, chunk_bb, 7, 1, 1, 7, 1, 6, light, light, false);
            }
        } else if main_design == 2 {
            self.generate_box(ctx.level, chunk_bb, 0, 1, 0, 0, 1, 7, light, light, false);
            self.generate_box(ctx.level, chunk_bb, 7, 1, 0, 7, 1, 7, light, light, false);
            self.generate_box(ctx.level, chunk_bb, 1, 1, 0, 6, 1, 0, light, light, false);
            self.generate_box(ctx.level, chunk_bb, 1, 1, 7, 6, 1, 7, light, light, false);
            self.generate_box(ctx.level, chunk_bb, 0, 2, 0, 0, 2, 7, black, black, false);
            self.generate_box(ctx.level, chunk_bb, 7, 2, 0, 7, 2, 7, black, black, false);
            self.generate_box(ctx.level, chunk_bb, 1, 2, 0, 6, 2, 0, black, black, false);
            self.generate_box(ctx.level, chunk_bb, 1, 2, 7, 6, 2, 7, black, black, false);
            self.generate_box(ctx.level, chunk_bb, 0, 3, 0, 0, 3, 7, light, light, false);
            self.generate_box(ctx.level, chunk_bb, 7, 3, 0, 7, 3, 7, light, light, false);
            self.generate_box(ctx.level, chunk_bb, 1, 3, 0, 6, 3, 0, light, light, false);
            self.generate_box(ctx.level, chunk_bb, 1, 3, 7, 6, 3, 7, light, light, false);
            self.generate_box(ctx.level, chunk_bb, 0, 1, 3, 0, 2, 4, black, black, false);
            self.generate_box(ctx.level, chunk_bb, 7, 1, 3, 7, 2, 4, black, black, false);
            self.generate_box(ctx.level, chunk_bb, 3, 1, 0, 4, 2, 0, black, black, false);
            self.generate_box(ctx.level, chunk_bb, 3, 1, 7, 4, 2, 7, black, black, false);
            if room.opens(Direction::South) {
                generate_water_box(self, ctx.level, chunk_bb, 3, 1, 0, 4, 2, 0);
            }

            if room.opens(Direction::North) {
                generate_water_box(self, ctx.level, chunk_bb, 3, 1, 7, 4, 2, 7);
            }

            if room.opens(Direction::West) {
                generate_water_box(self, ctx.level, chunk_bb, 0, 1, 3, 0, 2, 4);
            }

            if room.opens(Direction::East) {
                generate_water_box(self, ctx.level, chunk_bb, 7, 1, 3, 7, 2, 4);
            }
        }

        if center_pillar {
            self.generate_box(ctx.level, chunk_bb, 3, 1, 3, 4, 1, 4, light, light, false);
            self.generate_box(ctx.level, chunk_bb, 3, 2, 3, 4, 2, 4, gray, gray, false);
            self.generate_box(ctx.level, chunk_bb, 3, 3, 3, 4, 3, 4, light, light, false);
        }
    }

    /// `OceanMonumentSimpleTopRoom.postProcess`.
    fn simple_top_room(&self, ctx: &mut PieceContext<'_, '_>, chunk_bb: &BoundingBox) {
        let (gray, light, black) = (base_gray(), base_light(), base_black());
        let room = &self.rooms[self.room];
        if room.index / 25 > 0 {
            self.generate_default_floor(ctx.level, chunk_bb, 0, 0, room.opens(Direction::Down));
        }

        if !room.connects(Direction::Up) {
            generate_box_on_fill_only(self, ctx.level, chunk_bb, 1, 4, 1, 6, 4, 6, gray);
        }

        for x in 1..=6 {
            for z in 1..=6 {
                if ctx.random.next_int_bounded(3) != 0 {
                    let y0 = 2 + if ctx.random.next_int_bounded(4) == 0 {
                        0
                    } else {
                        1
                    };
                    let wet_sponge = ids::WET_SPONGE.default_state();
                    self.generate_box(
                        ctx.level, chunk_bb, x, y0, z, x, 3, z, wet_sponge, wet_sponge, false,
                    );
                }
            }
        }

        self.generate_box(ctx.level, chunk_bb, 0, 1, 0, 0, 1, 7, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 7, 1, 0, 7, 1, 7, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 1, 1, 0, 6, 1, 0, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 1, 1, 7, 6, 1, 7, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 0, 2, 0, 0, 2, 7, black, black, false);
        self.generate_box(ctx.level, chunk_bb, 7, 2, 0, 7, 2, 7, black, black, false);
        self.generate_box(ctx.level, chunk_bb, 1, 2, 0, 6, 2, 0, black, black, false);
        self.generate_box(ctx.level, chunk_bb, 1, 2, 7, 6, 2, 7, black, black, false);
        self.generate_box(ctx.level, chunk_bb, 0, 3, 0, 0, 3, 7, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 7, 3, 0, 7, 3, 7, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 1, 3, 0, 6, 3, 0, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 1, 3, 7, 6, 3, 7, light, light, false);
        self.generate_box(ctx.level, chunk_bb, 0, 1, 3, 0, 2, 4, black, black, false);
        self.generate_box(ctx.level, chunk_bb, 7, 1, 3, 7, 2, 4, black, black, false);
        self.generate_box(ctx.level, chunk_bb, 3, 1, 0, 4, 2, 0, black, black, false);
        self.generate_box(ctx.level, chunk_bb, 3, 1, 7, 4, 2, 7, black, black, false);
        if room.opens(Direction::South) {
            generate_water_box(self, ctx.level, chunk_bb, 3, 1, 0, 4, 2, 0);
        }
    }

    /// `OceanMonumentWingRoom.postProcess`.
    fn wing_room(&self, ctx: &mut PieceContext<'_, '_>, chunk_bb: &BoundingBox, main_design: i32) {
        let (light, black, lamp) = (base_light(), base_black(), lamp_block());
        if main_design == 0 {
            for i in 0..4 {
                self.generate_box(
                    ctx.level,
                    chunk_bb,
                    10 - i,
                    3 - i,
                    20 - i,
                    12 + i,
                    3 - i,
                    20,
                    light,
                    light,
                    false,
                );
            }

            self.generate_box(ctx.level, chunk_bb, 7, 0, 6, 15, 0, 16, light, light, false);
            self.generate_box(ctx.level, chunk_bb, 6, 0, 6, 6, 3, 20, light, light, false);
            self.generate_box(
                ctx.level, chunk_bb, 16, 0, 6, 16, 3, 20, light, light, false,
            );
            self.generate_box(ctx.level, chunk_bb, 7, 1, 7, 7, 1, 20, light, light, false);
            self.generate_box(
                ctx.level, chunk_bb, 15, 1, 7, 15, 1, 20, light, light, false,
            );
            self.generate_box(ctx.level, chunk_bb, 7, 1, 6, 9, 3, 6, light, light, false);
            self.generate_box(ctx.level, chunk_bb, 13, 1, 6, 15, 3, 6, light, light, false);
            self.generate_box(ctx.level, chunk_bb, 8, 1, 7, 9, 1, 7, light, light, false);
            self.generate_box(ctx.level, chunk_bb, 13, 1, 7, 14, 1, 7, light, light, false);
            self.generate_box(ctx.level, chunk_bb, 9, 0, 5, 13, 0, 5, light, light, false);
            self.generate_box(ctx.level, chunk_bb, 10, 0, 7, 12, 0, 7, black, black, false);
            self.generate_box(ctx.level, chunk_bb, 8, 0, 10, 8, 0, 12, black, black, false);
            self.generate_box(
                ctx.level, chunk_bb, 14, 0, 10, 14, 0, 12, black, black, false,
            );

            for z in (7..=18).rev().step_by(3) {
                self.place_block(ctx.level, lamp, 6, 3, z, chunk_bb);
                self.place_block(ctx.level, lamp, 16, 3, z, chunk_bb);
            }

            self.place_block(ctx.level, lamp, 10, 0, 10, chunk_bb);
            self.place_block(ctx.level, lamp, 12, 0, 10, chunk_bb);
            self.place_block(ctx.level, lamp, 10, 0, 12, chunk_bb);
            self.place_block(ctx.level, lamp, 12, 0, 12, chunk_bb);
            self.place_block(ctx.level, lamp, 8, 3, 6, chunk_bb);
            self.place_block(ctx.level, lamp, 14, 3, 6, chunk_bb);
            self.place_block(ctx.level, light, 4, 2, 4, chunk_bb);
            self.place_block(ctx.level, lamp, 4, 1, 4, chunk_bb);
            self.place_block(ctx.level, light, 4, 0, 4, chunk_bb);
            self.place_block(ctx.level, light, 18, 2, 4, chunk_bb);
            self.place_block(ctx.level, lamp, 18, 1, 4, chunk_bb);
            self.place_block(ctx.level, light, 18, 0, 4, chunk_bb);
            self.place_block(ctx.level, light, 4, 2, 18, chunk_bb);
            self.place_block(ctx.level, lamp, 4, 1, 18, chunk_bb);
            self.place_block(ctx.level, light, 4, 0, 18, chunk_bb);
            self.place_block(ctx.level, light, 18, 2, 18, chunk_bb);
            self.place_block(ctx.level, lamp, 18, 1, 18, chunk_bb);
            self.place_block(ctx.level, light, 18, 0, 18, chunk_bb);
            self.place_block(ctx.level, light, 9, 7, 20, chunk_bb);
            self.place_block(ctx.level, light, 13, 7, 20, chunk_bb);
            self.generate_box(ctx.level, chunk_bb, 6, 0, 21, 7, 4, 21, light, light, false);
            self.generate_box(
                ctx.level, chunk_bb, 15, 0, 21, 16, 4, 21, light, light, false,
            );
            self.spawn_elder(ctx.level, chunk_bb, 11, 2, 16);
        } else if main_design == 1 {
            self.generate_box(
                ctx.level, chunk_bb, 9, 3, 18, 13, 3, 20, light, light, false,
            );
            self.generate_box(ctx.level, chunk_bb, 9, 0, 18, 9, 2, 18, light, light, false);
            self.generate_box(
                ctx.level, chunk_bb, 13, 0, 18, 13, 2, 18, light, light, false,
            );
            let mut x = 9;

            for _ in 0..2 {
                self.place_block(ctx.level, light, x, 6, 20, chunk_bb);
                self.place_block(ctx.level, lamp, x, 5, 20, chunk_bb);
                self.place_block(ctx.level, light, x, 4, 20, chunk_bb);
                x = 13;
            }

            self.generate_box(ctx.level, chunk_bb, 7, 3, 7, 15, 3, 14, light, light, false);
            let mut pillar_x = 10;

            for _ in 0..2 {
                self.generate_box(
                    ctx.level, chunk_bb, pillar_x, 0, 10, pillar_x, 6, 10, light, light, false,
                );
                self.generate_box(
                    ctx.level, chunk_bb, pillar_x, 0, 12, pillar_x, 6, 12, light, light, false,
                );
                self.place_block(ctx.level, lamp, pillar_x, 0, 10, chunk_bb);
                self.place_block(ctx.level, lamp, pillar_x, 0, 12, chunk_bb);
                self.place_block(ctx.level, lamp, pillar_x, 4, 10, chunk_bb);
                self.place_block(ctx.level, lamp, pillar_x, 4, 12, chunk_bb);
                pillar_x = 12;
            }

            pillar_x = 8;

            for _ in 0..2 {
                self.generate_box(
                    ctx.level, chunk_bb, pillar_x, 0, 7, pillar_x, 2, 7, light, light, false,
                );
                self.generate_box(
                    ctx.level, chunk_bb, pillar_x, 0, 14, pillar_x, 2, 14, light, light, false,
                );
                pillar_x = 14;
            }

            self.generate_box(ctx.level, chunk_bb, 8, 3, 8, 8, 3, 13, black, black, false);
            self.generate_box(
                ctx.level, chunk_bb, 14, 3, 8, 14, 3, 13, black, black, false,
            );
            self.spawn_elder(ctx.level, chunk_bb, 11, 5, 13);
        }
    }
}

impl StructurePiece for MonumentRoom {
    fn base(&self) -> &PieceBase {
        &self.base
    }

    fn base_mut(&mut self) -> &mut PieceBase {
        &mut self.base
    }

    /// `OceanMonumentPiece.addAdditionalSaveData`: writes nothing.
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
        let chunk_bb: &BoundingBox = chunk_bb;
        match self.kind {
            RoomKind::Entry => self.entry_room(ctx, chunk_bb),
            RoomKind::Core => self.core_room(ctx, chunk_bb),
            RoomKind::DoubleX => self.double_x_room(ctx, chunk_bb),
            RoomKind::DoubleXY => self.double_xy_room(ctx, chunk_bb),
            RoomKind::DoubleY => self.double_y_room(ctx, chunk_bb),
            RoomKind::DoubleYZ => self.double_yz_room(ctx, chunk_bb),
            RoomKind::DoubleZ => self.double_z_room(ctx, chunk_bb),
            RoomKind::Simple { main_design } => self.simple_room(ctx, chunk_bb, main_design),
            RoomKind::SimpleTop => self.simple_top_room(ctx, chunk_bb),
            RoomKind::Wing { main_design } => self.wing_room(ctx, chunk_bb, main_design),
            RoomKind::Penthouse => self.penthouse(ctx, chunk_bb),
        }
    }
}

/// `OceanMonumentPiece.generateWaterBox(level, chunkBB, x0, y0, z0, x1, y1, z1)`: water below
/// the sea level and air above it, keeping water and ice.
#[allow(clippy::too_many_arguments)]
fn generate_water_box(
    piece: &dyn StructurePiece,
    level: &mut WorldGenRegion<'_>,
    chunk_bb: &BoundingBox,
    x0: i32,
    y0: i32,
    z0: i32,
    x1: i32,
    y1: i32,
    z1: i32,
) {
    let fill = fill_block();
    for y in y0..=y1 {
        for x in x0..=x1 {
            for z in z0..=z1 {
                let block = piece.get_block(level, x, y, z, chunk_bb);
                // `FILL_KEEP`.
                if matches!(
                    block.block(),
                    ids::ICE | ids::PACKED_ICE | ids::BLUE_ICE | ids::WATER
                ) {
                    continue;
                }
                if piece.base().world_y(y) >= level.sea_level() && block != fill {
                    piece.place_block(level, BlockState::AIR, x, y, z, chunk_bb);
                } else {
                    piece.place_block(level, fill, x, y, z, chunk_bb);
                }
            }
        }
    }
}

/// `OceanMonumentPiece.generateBoxOnFillOnly(level, chunkBB, x0, y0, z0, x1, y1, z1,
/// targetBlock)`: replaces only water.
#[allow(clippy::too_many_arguments)]
fn generate_box_on_fill_only(
    piece: &dyn StructurePiece,
    level: &mut WorldGenRegion<'_>,
    chunk_bb: &BoundingBox,
    x0: i32,
    y0: i32,
    z0: i32,
    x1: i32,
    y1: i32,
    z1: i32,
    target: BlockState,
) {
    let fill = fill_block();
    for y in y0..=y1 {
        for x in x0..=x1 {
            for z in z0..=z1 {
                if piece.get_block(level, x, y, z, chunk_bb) == fill {
                    piece.place_block(level, target, x, y, z, chunk_bb);
                }
            }
        }
    }
}

/// `OceanMonumentPiece.chunkIntersects(chunkBB, x0, z0, x1, z1)`.
fn chunk_intersects(
    base: &PieceBase,
    chunk_bb: &BoundingBox,
    x0: i32,
    z0: i32,
    x1: i32,
    z1: i32,
) -> bool {
    let wx0 = base.world_x(x0, z0);
    let wz0 = base.world_z(x0, z0);
    let wx1 = base.world_x(x1, z1);
    let wz1 = base.world_z(x1, z1);
    chunk_bb.intersects_xz(wx0.min(wx1), wz0.min(wz1), wx0.max(wx1), wz0.max(wz1))
}

/// The random draws of `Mob.finalizeSpawn` (from the level's random) for a mob spawned by a
/// structure: the follow range bonus (`triangle(0, 0.11485)`) and the left-handedness roll.
pub(crate) fn mob_finalize_spawn<R: RandomSource + ?Sized>(random: &mut R) {
    random.triangle(0.0, 0.114_850_000_000_000_01);
    random.next_float();
}

//! Woodland mansions (`WoodlandMansionStructure`, `WoodlandMansionPieces`): the floor plan grid
//! (`MansionGrid`, `SimpleGrid`), the placement of walls, roofs, corridors and rooms
//! (`MansionPiecePlacer`) and the template pieces with their chests and mobs.
//!
//! Mobs (evokers, vindicators, allays) are entities: only the random draws of their creation
//! and `finalizeSpawn` are made, and the data marker they stand on is cleared as Vanilla does
//! once the mob exists (assuming a difficulty other than peaceful and below hard, where
//! `getSpecialMultiplier()` is 0).

use std::sync::Arc;

use rustc_hash::FxHashMap;
use serde_json::Value;
use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, ChunkPos, Direction};
use vanillite_data::block::{BlockState, flags, ids};

use vanillite_nbt::Compound;

use super::ocean_monument::mob_finalize_spawn;
use crate::Error;
use crate::feature::kinds::tree::shuffle;
use crate::feature::region::entity_tag;
use crate::structure::piece::{
    HORIZONTAL, PieceContext, create_bounding_box, create_chest_at, with_facing,
};
use crate::structure::structure::lowest_y_in_5by5_box;
use crate::structure::{
    GenerationContext, GenerationStub, PieceBase, PieceSettings, StructureParser, StructurePiece,
    StructureType, TemplatePiece,
};
use crate::template::structure::zero_position_with_transform;
use crate::template::transform::{clockwise, counter_clockwise};
use crate::template::{BoundingBox, Mirror, Rotation, StructureProcessor, StructureTemplate};

/// `BuiltInLootTables.WOODLAND_MANSION`.
const LOOT_TABLE: &str = "minecraft:chests/woodland_mansion";

/// Every template of `woodland_mansion/`.
const TEMPLATES: [&str; 73] = [
    "1x1_a1",
    "1x1_a2",
    "1x1_a3",
    "1x1_a4",
    "1x1_a5",
    "1x1_as1",
    "1x1_as2",
    "1x1_as3",
    "1x1_as4",
    "1x1_b1",
    "1x1_b2",
    "1x1_b3",
    "1x1_b4",
    "1x1_b5",
    "1x2_a1",
    "1x2_a2",
    "1x2_a3",
    "1x2_a4",
    "1x2_a5",
    "1x2_a6",
    "1x2_a7",
    "1x2_a8",
    "1x2_a9",
    "1x2_b1",
    "1x2_b2",
    "1x2_b3",
    "1x2_b4",
    "1x2_b5",
    "1x2_c1",
    "1x2_c2",
    "1x2_c3",
    "1x2_c4",
    "1x2_c_stairs",
    "1x2_d1",
    "1x2_d2",
    "1x2_d3",
    "1x2_d4",
    "1x2_d5",
    "1x2_d_stairs",
    "1x2_s1",
    "1x2_s2",
    "1x2_se1",
    "2x2_a1",
    "2x2_a2",
    "2x2_a3",
    "2x2_a4",
    "2x2_b1",
    "2x2_b2",
    "2x2_b3",
    "2x2_b4",
    "2x2_b5",
    "2x2_s1",
    "carpet_east",
    "carpet_north",
    "carpet_south_1",
    "carpet_south_2",
    "carpet_west_1",
    "carpet_west_2",
    "corridor_floor",
    "entrance",
    "indoors_door_1",
    "indoors_door_2",
    "indoors_wall_1",
    "indoors_wall_2",
    "roof",
    "roof_corner",
    "roof_front",
    "roof_inner_corner",
    "small_wall",
    "small_wall_corner",
    "wall_corner",
    "wall_flat",
    "wall_window",
];

/// The templates by name (without the `woodland_mansion/` prefix).
type Templates = FxHashMap<&'static str, Arc<StructureTemplate>>;

/// `WoodlandMansionStructure`.
#[derive(Debug)]
pub struct WoodlandMansionStructure {
    templates: Templates,
}

impl StructureType for WoodlandMansionStructure {
    fn parse(_json: &Value, parser: &mut StructureParser<'_>) -> Result<Self, Error> {
        let templates = TEMPLATES
            .iter()
            .map(|&name| {
                let template = parser.template(&format!("minecraft:woodland_mansion/{name}"))?;
                Ok((name, template))
            })
            .collect::<Result<_, Error>>()?;
        Ok(Self { templates })
    }

    fn find_generation_point<'s>(
        &'s self,
        ctx: &mut GenerationContext<'_>,
    ) -> Option<GenerationStub<'s>> {
        let block_x = ctx.chunk_pos.min_block_x() + 7;
        let block_z = ctx.chunk_pos.min_block_z() + 7;
        if !ctx.could_valid_biome_exist_in_terrain_column(block_x, block_z) {
            return None;
        }
        let rotation = Rotation::random(&mut ctx.random.random);
        let start = lowest_y_in_5by5_box(ctx, block_x, block_z, rotation);
        if start.y < 60 {
            return None;
        }
        Some(GenerationStub::lazy(start, move |builder, ctx| {
            // `WoodlandMansionPieces.generateMansion`.
            let random = &mut ctx.random.random;
            let grid = MansionGrid::new(random);
            let mut placer = MansionPiecePlacer {
                templates: &self.templates,
                random,
                start_x: 0,
                start_y: 0,
                pieces: Vec::new(),
            };
            placer.create_mansion(start, rotation, &grid);
            for piece in placer.pieces {
                builder.add_piece(Box::new(piece));
            }
        }))
    }

    /// `afterPlace`: fills the space below the mansion's lowest floor with cobblestone, down to
    /// the ground.
    fn after_place(
        &self,
        ctx: &mut PieceContext<'_, '_>,
        chunk_bb: &BoundingBox,
        _chunk_pos: ChunkPos,
        pieces: &mut [Box<dyn StructurePiece>],
    ) {
        let Some(bounding_box) = create_bounding_box(pieces) else {
            return;
        };
        let level = &mut *ctx.level;
        let min_y = level.min_y();
        let y_start = bounding_box.min_y();
        for x in chunk_bb.min_x()..=chunk_bb.max_x() {
            for z in chunk_bb.min_z()..=chunk_bb.max_z() {
                let pos = BlockPos::new(x, y_start, z);
                // `pieces.isInsidePiece(pos)`.
                if level.is_empty_block(pos)
                    || !bounding_box.is_inside(pos)
                    || !pieces.iter().any(|p| p.bounding_box().is_inside(pos))
                {
                    continue;
                }
                let mut y = y_start - 1;
                while y > min_y {
                    let pos = BlockPos::new(x, y, z);
                    if !level.is_empty_block(pos) && !level.block(pos).has(flags::LIQUID) {
                        break;
                    }
                    level.set_block(pos, ids::COBBLESTONE.default_state(), 2);
                    y -= 1;
                }
            }
        }
    }
}

/// `pos.relative(direction, distance)`.
fn relative(pos: BlockPos, direction: Direction, distance: i32) -> BlockPos {
    let [dx, dy, dz] = direction.normal();
    pos.offset(dx * distance, dy * distance, dz * distance)
}

/// `pos.above(distance)`.
fn above(pos: BlockPos, distance: i32) -> BlockPos {
    pos.offset(0, distance, 0)
}

/// `BlockPos.rotate(rotation)`: around the origin.
fn rotate_pos(pos: BlockPos, rotation: Rotation) -> BlockPos {
    match rotation {
        Rotation::None => pos,
        Rotation::Clockwise90 => BlockPos::new(-pos.z, pos.y, pos.x),
        Rotation::Clockwise180 => BlockPos::new(-pos.x, pos.y, -pos.z),
        Rotation::Counterclockwise90 => BlockPos::new(pos.z, pos.y, -pos.x),
    }
}

/// `Direction.from2DDataValue(value)`.
fn from_2d_data_value(value: i32) -> Direction {
    [
        Direction::South,
        Direction::West,
        Direction::North,
        Direction::East,
    ][(value % 4).unsigned_abs() as usize]
}

/// `getStepX()` and `getStepZ()`.
fn step(direction: Direction) -> (i32, i32) {
    let [x, _, z] = direction.normal();
    (x, z)
}

/// `MansionGrid` cell values and room flags.
mod cell {
    pub const CLEAR: i32 = 0;
    pub const CORRIDOR: i32 = 1;
    pub const ROOM: i32 = 2;
    pub const START_ROOM: i32 = 3;
    pub const TEST_ROOM: i32 = 4;
    pub const BLOCKED: i32 = 5;
    pub const ROOM_1X1: i32 = 65536;
    pub const ROOM_1X2: i32 = 131_072;
    pub const ROOM_2X2: i32 = 262_144;
    pub const ROOM_ORIGIN_FLAG: i32 = 1_048_576;
    pub const ROOM_DOOR_FLAG: i32 = 2_097_152;
    pub const ROOM_STAIRS_FLAG: i32 = 4_194_304;
    pub const ROOM_CORRIDOR_FLAG: i32 = 8_388_608;
    pub const ROOM_TYPE_MASK: i32 = 983_040;
    pub const ROOM_ID_MASK: i32 = 65535;
}

/// `WoodlandMansionPieces.SimpleGrid`.
#[derive(Clone, Debug)]
struct SimpleGrid {
    /// `grid[x][y]`, x-major.
    grid: Vec<i32>,
    width: i32,
    height: i32,
    value_if_outside: i32,
}

impl SimpleGrid {
    fn new(width: i32, height: i32, value_if_outside: i32) -> Self {
        Self {
            grid: vec![0; (width * height) as usize],
            width,
            height,
            value_if_outside,
        }
    }

    fn inside(&self, x: i32, y: i32) -> bool {
        x >= 0 && x < self.width && y >= 0 && y < self.height
    }

    /// `set(x, y, value)`.
    fn set(&mut self, x: i32, y: i32, value: i32) {
        if self.inside(x, y) {
            self.grid[(x * self.height + y) as usize] = value;
        }
    }

    /// `set(x0, y0, x1, y1, value)`.
    fn set_area(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, value: i32) {
        for y in y0..=y1 {
            for x in x0..=x1 {
                self.set(x, y, value);
            }
        }
    }

    /// `get(x, y)`.
    fn get(&self, x: i32, y: i32) -> i32 {
        if self.inside(x, y) {
            self.grid[(x * self.height + y) as usize]
        } else {
            self.value_if_outside
        }
    }

    /// `setif(x, y, ifValue, value)`.
    fn set_if(&mut self, x: i32, y: i32, if_value: i32, value: i32) {
        if self.get(x, y) == if_value {
            self.set(x, y, value);
        }
    }

    /// `edgesTo(x, y, ifValue)`.
    fn edges_to(&self, x: i32, y: i32, if_value: i32) -> bool {
        self.get(x - 1, y) == if_value
            || self.get(x + 1, y) == if_value
            || self.get(x, y + 1) == if_value
            || self.get(x, y - 1) == if_value
    }
}

/// `MansionGrid.isHouse(grid, x, y)`.
fn is_house(grid: &SimpleGrid, x: i32, y: i32) -> bool {
    matches!(
        grid.get(x, y),
        cell::CORRIDOR | cell::ROOM | cell::START_ROOM | cell::TEST_ROOM
    )
}

/// `WoodlandMansionPieces.MansionGrid`: the floor plan of the three floors.
#[derive(Clone, Debug)]
struct MansionGrid {
    base_grid: SimpleGrid,
    third_floor_grid: SimpleGrid,
    floor_rooms: [SimpleGrid; 3],
    entrance_x: i32,
    entrance_y: i32,
}

impl MansionGrid {
    /// `new MansionGrid(random)`.
    fn new<R: RandomSource>(random: &mut R) -> Self {
        let (entrance_x, entrance_y) = (7, 4);
        let mut base_grid = SimpleGrid::new(11, 11, cell::BLOCKED);
        let (ex, ey) = (entrance_x, entrance_y);
        base_grid.set_area(ex, ey, ex + 1, ey + 1, cell::START_ROOM);
        base_grid.set_area(ex - 1, ey, ex - 1, ey + 1, cell::ROOM);
        base_grid.set_area(ex + 2, ey - 2, ex + 3, ey + 3, cell::BLOCKED);
        base_grid.set_area(ex + 1, ey - 2, ex + 1, ey - 1, cell::CORRIDOR);
        base_grid.set_area(ex + 1, ey + 2, ex + 1, ey + 3, cell::CORRIDOR);
        base_grid.set(ex - 1, ey - 1, cell::CORRIDOR);
        base_grid.set(ex - 1, ey + 2, cell::CORRIDOR);
        base_grid.set_area(0, 0, 11, 1, cell::BLOCKED);
        base_grid.set_area(0, 9, 11, 11, cell::BLOCKED);
        recursive_corridor(random, &mut base_grid, ex, ey - 2, Direction::West, 6);
        recursive_corridor(random, &mut base_grid, ex, ey + 3, Direction::West, 6);
        recursive_corridor(random, &mut base_grid, ex - 2, ey - 1, Direction::West, 3);
        recursive_corridor(random, &mut base_grid, ex - 2, ey + 2, Direction::West, 3);
        while clean_edges(&mut base_grid) {}
        let mut floor_rooms = [
            SimpleGrid::new(11, 11, cell::BLOCKED),
            SimpleGrid::new(11, 11, cell::BLOCKED),
            SimpleGrid::new(11, 11, cell::BLOCKED),
        ];
        identify_rooms(random, &base_grid, &mut floor_rooms[0]);
        identify_rooms(random, &base_grid, &mut floor_rooms[1]);
        floor_rooms[0].set_area(ex + 1, ey, ex + 1, ey + 1, cell::ROOM_CORRIDOR_FLAG);
        floor_rooms[1].set_area(ex + 1, ey, ex + 1, ey + 1, cell::ROOM_CORRIDOR_FLAG);
        let third_floor_grid = SimpleGrid::new(base_grid.width, base_grid.height, cell::BLOCKED);
        let mut grid = Self {
            base_grid,
            third_floor_grid,
            floor_rooms,
            entrance_x,
            entrance_y,
        };
        grid.setup_third_floor(random);
        let [_, _, third_rooms] = &mut grid.floor_rooms;
        identify_rooms(random, &grid.third_floor_grid, third_rooms);
        grid
    }

    /// `isRoomId(grid, x, y, floor, roomId)`.
    fn is_room_id(&self, x: i32, y: i32, floor: usize, room_id: i32) -> bool {
        (self.floor_rooms[floor].get(x, y) & cell::ROOM_ID_MASK) == room_id
    }

    /// `get1x2RoomDirection(grid, x, y, floorNum, roomId)`.
    fn get_1x2_room_direction(
        &self,
        x: i32,
        y: i32,
        floor: usize,
        room_id: i32,
    ) -> Option<Direction> {
        HORIZONTAL.into_iter().find(|&direction| {
            let (dx, dz) = step(direction);
            self.is_room_id(x + dx, y + dz, floor, room_id)
        })
    }

    /// `setupThirdFloor()`.
    fn setup_third_floor<R: RandomSource>(&mut self, random: &mut R) {
        let mut potential_rooms = Vec::new();
        let (width, height) = (self.third_floor_grid.width, self.third_floor_grid.height);
        for y in 0..height {
            for x in 0..width {
                let room_data = self.floor_rooms[1].get(x, y);
                let room_type = room_data & cell::ROOM_TYPE_MASK;
                if room_type == cell::ROOM_1X2
                    && (room_data & cell::ROOM_DOOR_FLAG) == cell::ROOM_DOOR_FLAG
                {
                    potential_rooms.push((x, y));
                }
            }
        }
        if potential_rooms.is_empty() {
            self.third_floor_grid
                .set_area(0, 0, width, height, cell::BLOCKED);
            return;
        }
        let (room_x, room_y) =
            potential_rooms[random.next_int_bounded(potential_rooms.len() as i32) as usize];
        let room_data = self.floor_rooms[1].get(room_x, room_y);
        self.floor_rooms[1].set(room_x, room_y, room_data | cell::ROOM_STAIRS_FLAG);
        let room_dir = self
            .get_1x2_room_direction(room_x, room_y, 1, room_data & cell::ROOM_ID_MASK)
            .expect("a 1x2 room has a second cell");
        let (dx, dz) = step(room_dir);
        let (room_end_x, room_end_y) = (room_x + dx, room_y + dz);
        for y in 0..height {
            for x in 0..width {
                if !is_house(&self.base_grid, x, y) {
                    self.third_floor_grid.set(x, y, cell::BLOCKED);
                } else if x == room_x && y == room_y {
                    self.third_floor_grid.set(x, y, cell::START_ROOM);
                } else if x == room_end_x && y == room_end_y {
                    self.third_floor_grid.set(x, y, cell::START_ROOM);
                    self.floor_rooms[2].set(x, y, cell::ROOM_CORRIDOR_FLAG);
                }
            }
        }
        let potential_corridors: Vec<Direction> = HORIZONTAL
            .into_iter()
            .filter(|&direction| {
                let (dx, dz) = step(direction);
                self.third_floor_grid.get(room_end_x + dx, room_end_y + dz) == cell::CLEAR
            })
            .collect();
        if potential_corridors.is_empty() {
            self.third_floor_grid
                .set_area(0, 0, width, height, cell::BLOCKED);
            self.floor_rooms[1].set(room_x, room_y, room_data);
        } else {
            let corridor_dir = potential_corridors
                [random.next_int_bounded(potential_corridors.len() as i32) as usize];
            let (dx, dz) = step(corridor_dir);
            recursive_corridor(
                random,
                &mut self.third_floor_grid,
                room_end_x + dx,
                room_end_y + dz,
                corridor_dir,
                4,
            );
            while clean_edges(&mut self.third_floor_grid) {}
        }
    }
}

/// `MansionGrid.recursiveCorridor(grid, x, y, heading, depth)`.
fn recursive_corridor<R: RandomSource>(
    random: &mut R,
    grid: &mut SimpleGrid,
    x: i32,
    y: i32,
    heading: Direction,
    depth: i32,
) {
    if depth <= 0 {
        return;
    }
    let (hx, hz) = step(heading);
    grid.set(x, y, cell::CORRIDOR);
    grid.set_if(x + hx, y + hz, cell::CLEAR, cell::CORRIDOR);
    for _ in 0..8 {
        let next_dir = from_2d_data_value(random.next_int_bounded(4));
        if next_dir != heading.opposite() && (next_dir != Direction::East || !random.next_boolean())
        {
            let (nx, ny) = (x + hx, y + hz);
            let (dx, dz) = step(next_dir);
            if grid.get(nx + dx, ny + dz) == cell::CLEAR
                && grid.get(nx + dx * 2, ny + dz * 2) == cell::CLEAR
            {
                recursive_corridor(random, grid, x + hx + dx, y + hz + dz, next_dir, depth - 1);
                break;
            }
        }
    }
    let (cwx, cwz) = step(clockwise(heading));
    let (ccwx, ccwz) = step(counter_clockwise(heading));
    grid.set_if(x + cwx, y + cwz, cell::CLEAR, cell::ROOM);
    grid.set_if(x + ccwx, y + ccwz, cell::CLEAR, cell::ROOM);
    grid.set_if(x + hx + cwx, y + hz + cwz, cell::CLEAR, cell::ROOM);
    grid.set_if(x + hx + ccwx, y + hz + ccwz, cell::CLEAR, cell::ROOM);
    grid.set_if(x + hx * 2, y + hz * 2, cell::CLEAR, cell::ROOM);
    grid.set_if(x + cwx * 2, y + cwz * 2, cell::CLEAR, cell::ROOM);
    grid.set_if(x + ccwx * 2, y + ccwz * 2, cell::CLEAR, cell::ROOM);
}

/// `MansionGrid.cleanEdges(grid)`.
fn clean_edges(grid: &mut SimpleGrid) -> bool {
    let mut touched = false;
    for y in 0..grid.height {
        for x in 0..grid.width {
            if grid.get(x, y) != cell::CLEAR {
                continue;
            }
            let house = |x, y| i32::from(is_house(grid, x, y));
            let direct = house(x + 1, y) + house(x - 1, y) + house(x, y + 1) + house(x, y - 1);
            if direct >= 3 {
                grid.set(x, y, cell::ROOM);
                touched = true;
            } else if direct == 2 {
                let diagonal = house(x + 1, y + 1)
                    + house(x - 1, y + 1)
                    + house(x + 1, y - 1)
                    + house(x - 1, y - 1);
                if diagonal <= 1 {
                    grid.set(x, y, cell::ROOM);
                    touched = true;
                }
            }
        }
    }
    touched
}

/// `MansionGrid.identifyRooms(fromGrid, roomGrid)`.
fn identify_rooms<R: RandomSource>(random: &mut R, from: &SimpleGrid, rooms: &mut SimpleGrid) {
    let mut room_pos = Vec::new();
    for y in 0..from.height {
        for x in 0..from.width {
            if from.get(x, y) == cell::ROOM {
                room_pos.push((x, y));
            }
        }
    }
    shuffle(&mut room_pos, random);
    let mut room_id = 10;
    for (xx, y) in room_pos {
        if rooms.get(xx, y) != cell::CLEAR {
            continue;
        }
        let (mut x0, mut x1, mut y0, mut y1) = (xx, xx, y, y);
        let mut kind = cell::ROOM_1X1;
        let free = |x, y| rooms.get(x, y) == cell::CLEAR && from.get(x, y) == cell::ROOM;
        if rooms.get(xx + 1, y) == cell::CLEAR
            && rooms.get(xx, y + 1) == cell::CLEAR
            && rooms.get(xx + 1, y + 1) == cell::CLEAR
            && from.get(xx + 1, y) == cell::ROOM
            && from.get(xx, y + 1) == cell::ROOM
            && from.get(xx + 1, y + 1) == cell::ROOM
        {
            x1 = xx + 1;
            y1 = y + 1;
            kind = cell::ROOM_2X2;
        } else if rooms.get(xx - 1, y) == cell::CLEAR
            && rooms.get(xx, y + 1) == cell::CLEAR
            && rooms.get(xx - 1, y + 1) == cell::CLEAR
            && from.get(xx - 1, y) == cell::ROOM
            && from.get(xx, y + 1) == cell::ROOM
            && from.get(xx - 1, y + 1) == cell::ROOM
        {
            x0 = xx - 1;
            y1 = y + 1;
            kind = cell::ROOM_2X2;
        } else if rooms.get(xx - 1, y) == cell::CLEAR
            && rooms.get(xx, y - 1) == cell::CLEAR
            && rooms.get(xx - 1, y - 1) == cell::CLEAR
            && from.get(xx - 1, y) == cell::ROOM
            && from.get(xx, y - 1) == cell::ROOM
            && from.get(xx - 1, y - 1) == cell::ROOM
        {
            x0 = xx - 1;
            y0 = y - 1;
            kind = cell::ROOM_2X2;
        } else if free(xx + 1, y) {
            x1 = xx + 1;
            kind = cell::ROOM_1X2;
        } else if free(xx, y + 1) {
            y1 = y + 1;
            kind = cell::ROOM_1X2;
        } else if free(xx - 1, y) {
            x0 = xx - 1;
            kind = cell::ROOM_1X2;
        } else if free(xx, y - 1) {
            y0 = y - 1;
            kind = cell::ROOM_1X2;
        }
        let mut door_x = if random.next_boolean() { x0 } else { x1 };
        let mut door_y = if random.next_boolean() { y0 } else { y1 };
        let mut door_flag = cell::ROOM_DOOR_FLAG;
        let other = |v: i32, a: i32, b: i32| if v == a { b } else { a };
        if !from.edges_to(door_x, door_y, cell::CORRIDOR) {
            door_x = other(door_x, x0, x1);
            door_y = other(door_y, y0, y1);
            if !from.edges_to(door_x, door_y, cell::CORRIDOR) {
                door_y = other(door_y, y0, y1);
                if !from.edges_to(door_x, door_y, cell::CORRIDOR) {
                    door_x = other(door_x, x0, x1);
                    door_y = other(door_y, y0, y1);
                    if !from.edges_to(door_x, door_y, cell::CORRIDOR) {
                        door_flag = 0;
                        door_x = x0;
                        door_y = y0;
                    }
                }
            }
        }
        for ry in y0..=y1 {
            for rx in x0..=x1 {
                if rx == door_x && ry == door_y {
                    rooms.set(rx, ry, cell::ROOM_ORIGIN_FLAG | door_flag | kind | room_id);
                } else {
                    rooms.set(rx, ry, kind | room_id);
                }
            }
        }
        room_id += 1;
    }
}

/// `FloorRoomCollection`: the room templates of a floor (`FirstFloorRoomCollection`,
/// `SecondFloorRoomCollection`, `ThirdFloorRoomCollection`, the last two being equal).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Floor {
    First,
    Upper,
}

impl Floor {
    /// `get1x1(random)`.
    fn get_1x1<R: RandomSource>(self, random: &mut R) -> String {
        match self {
            Self::First => format!("1x1_a{}", random.next_int_bounded(5) + 1),
            Self::Upper => format!("1x1_b{}", random.next_int_bounded(5) + 1),
        }
    }

    /// `get1x1Secret(random)`.
    fn get_1x1_secret<R: RandomSource>(self, random: &mut R) -> String {
        format!("1x1_as{}", random.next_int_bounded(4) + 1)
    }

    /// `get1x2SideEntrance(random, isStairsRoom)`.
    fn get_1x2_side_entrance<R: RandomSource>(
        self,
        random: &mut R,
        is_stairs_room: bool,
    ) -> String {
        match self {
            Self::First => format!("1x2_a{}", random.next_int_bounded(9) + 1),
            Self::Upper if is_stairs_room => "1x2_c_stairs".to_owned(),
            Self::Upper => format!("1x2_c{}", random.next_int_bounded(4) + 1),
        }
    }

    /// `get1x2FrontEntrance(random, isStairsRoom)`.
    fn get_1x2_front_entrance<R: RandomSource>(
        self,
        random: &mut R,
        is_stairs_room: bool,
    ) -> String {
        match self {
            Self::First => format!("1x2_b{}", random.next_int_bounded(5) + 1),
            Self::Upper if is_stairs_room => "1x2_d_stairs".to_owned(),
            Self::Upper => format!("1x2_d{}", random.next_int_bounded(5) + 1),
        }
    }

    /// `get1x2Secret(random)`.
    fn get_1x2_secret<R: RandomSource>(self, random: &mut R) -> String {
        match self {
            Self::First => format!("1x2_s{}", random.next_int_bounded(2) + 1),
            Self::Upper => format!("1x2_se{}", random.next_int_bounded(1) + 1),
        }
    }

    /// `get2x2(random)`.
    fn get_2x2<R: RandomSource>(self, random: &mut R) -> String {
        match self {
            Self::First => format!("2x2_a{}", random.next_int_bounded(4) + 1),
            Self::Upper => format!("2x2_b{}", random.next_int_bounded(5) + 1),
        }
    }

    /// `get2x2Secret(random)`.
    fn get_2x2_secret(self) -> String {
        "2x2_s1".to_owned()
    }
}

/// `WoodlandMansionPieces.PlacementData`.
#[derive(Clone, Copy, Debug)]
struct PlacementData {
    rotation: Rotation,
    position: BlockPos,
    wall_type: &'static str,
}

/// `WoodlandMansionPieces.MansionPiecePlacer`.
struct MansionPiecePlacer<'a, R> {
    templates: &'a Templates,
    random: &'a mut R,
    start_x: i32,
    start_y: i32,
    pieces: Vec<WoodlandMansionPiece>,
}

impl<R: RandomSource> MansionPiecePlacer<'_, R> {
    /// Adds `new WoodlandMansionPiece(manager, templateName, position, rotation, mirror)`.
    fn add(&mut self, name: &str, position: BlockPos, rotation: Rotation, mirror: Mirror) {
        let (&name, template) = self
            .templates
            .get_key_value(name)
            .unwrap_or_else(|| panic!("unknown woodland mansion template {name}"));
        self.pieces.push(WoodlandMansionPiece::new(
            name,
            Arc::clone(template),
            position,
            rotation,
            mirror,
        ));
    }

    /// The position of grid cell `(x, y)` relative to `origin` (`relative(SOUTH, 8 + (y -
    /// startY) * 8).relative(EAST, (x - startX) * 8)` rotated by `rotation`).
    fn cell_pos(
        &self,
        origin: BlockPos,
        rotation: Rotation,
        x: i32,
        y: i32,
        east: i32,
    ) -> BlockPos {
        let pos = relative(
            origin,
            rotation.rotate(Direction::South),
            8 + (y - self.start_y) * 8,
        );
        relative(
            pos,
            rotation.rotate(Direction::East),
            east + (x - self.start_x) * 8,
        )
    }

    /// `createMansion(origin, rotation, pieces, mansion)`.
    fn create_mansion(&mut self, origin: BlockPos, rotation: Rotation, mansion: &MansionGrid) {
        let mut data = PlacementData {
            rotation,
            position: origin,
            wall_type: "wall_flat",
        };
        self.entrance(&mut data);
        let mut second_data = PlacementData {
            rotation: data.rotation,
            position: above(data.position, 8),
            wall_type: "wall_window",
        };
        let base_grid = &mansion.base_grid;
        let third_grid = &mansion.third_floor_grid;
        self.start_x = mansion.entrance_x + 1;
        self.start_y = mansion.entrance_y + 1;
        let end_x = mansion.entrance_x + 1;
        let end_y = mansion.entrance_y;
        let (start_x, start_y) = (self.start_x, self.start_y);
        self.traverse_outer_walls(
            &mut data,
            base_grid,
            Direction::South,
            start_x,
            start_y,
            end_x,
            end_y,
        );
        self.traverse_outer_walls(
            &mut second_data,
            base_grid,
            Direction::South,
            start_x,
            start_y,
            end_x,
            end_y,
        );
        let mut third_data = PlacementData {
            rotation: data.rotation,
            position: above(data.position, 19),
            wall_type: "wall_window",
        };
        'search: for y in 0..third_grid.height {
            for x in (0..third_grid.width).rev() {
                if is_house(third_grid, x, y) {
                    third_data.position = relative(
                        third_data.position,
                        rotation.rotate(Direction::South),
                        8 + (y - self.start_y) * 8,
                    );
                    third_data.position = relative(
                        third_data.position,
                        rotation.rotate(Direction::East),
                        (x - self.start_x) * 8,
                    );
                    self.traverse_wall_piece(&mut third_data);
                    self.traverse_outer_walls(
                        &mut third_data,
                        third_grid,
                        Direction::South,
                        x,
                        y,
                        x,
                        y,
                    );
                    break 'search;
                }
            }
        }
        self.create_roof(above(origin, 16), rotation, base_grid, Some(third_grid));
        self.create_roof(above(origin, 27), rotation, third_grid, None);
        let floors = [Floor::First, Floor::Upper, Floor::Upper];
        for (floor_num, floor) in floors.into_iter().enumerate() {
            self.place_floor(origin, rotation, mansion, floor_num, floor);
        }
    }

    /// One iteration of `createMansion`'s floor loop: corridors with carpets, inner walls and
    /// doors, and the rooms of floor `floor_num`.
    fn place_floor(
        &mut self,
        origin: BlockPos,
        rotation: Rotation,
        mansion: &MansionGrid,
        floor_num: usize,
        floor: Floor,
    ) {
        let floor_origin = above(
            origin,
            8 * floor_num as i32 + if floor_num == 2 { 3 } else { 0 },
        );
        let rooms = &mansion.floor_rooms[floor_num];
        let grid = if floor_num == 2 {
            &mansion.third_floor_grid
        } else {
            &mansion.base_grid
        };
        let south_piece = if floor_num == 0 {
            "carpet_south_1"
        } else {
            "carpet_south_2"
        };
        let west_piece = if floor_num == 0 {
            "carpet_west_1"
        } else {
            "carpet_west_2"
        };
        let corridor = |x: i32, y: i32| {
            grid.get(x, y) == cell::CORRIDOR
                || (rooms.get(x, y) & cell::ROOM_CORRIDOR_FLAG) == cell::ROOM_CORRIDOR_FLAG
        };
        let r = |direction: Direction| rotation.rotate(direction);
        for y in 0..grid.height {
            for x in 0..grid.width {
                if grid.get(x, y) != cell::CORRIDOR {
                    continue;
                }
                let pos = self.cell_pos(floor_origin, rotation, x, y, 0);
                self.add("corridor_floor", pos, rotation, Mirror::None);
                if corridor(x, y - 1) {
                    let p = above(relative(pos, r(Direction::East), 1), 1);
                    self.add("carpet_north", p, rotation, Mirror::None);
                }
                if corridor(x + 1, y) {
                    let p = relative(pos, r(Direction::South), 1);
                    let p = above(relative(p, r(Direction::East), 5), 1);
                    self.add("carpet_east", p, rotation, Mirror::None);
                }
                if corridor(x, y + 1) {
                    let p = relative(pos, r(Direction::South), 5);
                    let p = relative(p, r(Direction::West), 1);
                    self.add(south_piece, p, rotation, Mirror::None);
                }
                if corridor(x - 1, y) {
                    let p = relative(pos, r(Direction::West), 1);
                    let p = relative(p, r(Direction::North), 1);
                    self.add(west_piece, p, rotation, Mirror::None);
                }
            }
        }
        let wall_piece = if floor_num == 0 {
            "indoors_wall_1"
        } else {
            "indoors_wall_2"
        };
        let door_piece = if floor_num == 0 {
            "indoors_door_1"
        } else {
            "indoors_door_2"
        };
        let mut door_dirs = Vec::new();
        for y in 0..grid.height {
            for x in 0..grid.width {
                let mut third_floor_start_room =
                    floor_num == 2 && grid.get(x, y) == cell::START_ROOM;
                if grid.get(x, y) != cell::ROOM && !third_floor_start_room {
                    continue;
                }
                let room_data = rooms.get(x, y);
                let room_type = room_data & cell::ROOM_TYPE_MASK;
                let room_id = room_data & cell::ROOM_ID_MASK;
                third_floor_start_room = third_floor_start_room
                    && (room_data & cell::ROOM_CORRIDOR_FLAG) == cell::ROOM_CORRIDOR_FLAG;
                door_dirs.clear();
                if (room_data & cell::ROOM_DOOR_FLAG) == cell::ROOM_DOOR_FLAG {
                    for direction in HORIZONTAL {
                        let (dx, dz) = step(direction);
                        if grid.get(x + dx, y + dz) == cell::CORRIDOR {
                            door_dirs.push(direction);
                        }
                    }
                }
                let door_dir = if !door_dirs.is_empty() {
                    Some(door_dirs[self.random.next_int_bounded(door_dirs.len() as i32) as usize])
                } else if (room_data & cell::ROOM_ORIGIN_FLAG) == cell::ROOM_ORIGIN_FLAG {
                    Some(Direction::Up)
                } else {
                    None
                };
                let room_pos = self.cell_pos(floor_origin, rotation, x, y, -1);
                let wall_or_door = |direction: Direction| {
                    if door_dir == Some(direction) {
                        door_piece
                    } else {
                        wall_piece
                    }
                };
                if is_house(grid, x - 1, y) && !mansion.is_room_id(x - 1, y, floor_num, room_id) {
                    self.add(
                        wall_or_door(Direction::West),
                        room_pos,
                        rotation,
                        Mirror::None,
                    );
                }
                if grid.get(x + 1, y) == cell::CORRIDOR && !third_floor_start_room {
                    let p = relative(room_pos, r(Direction::East), 8);
                    self.add(wall_or_door(Direction::East), p, rotation, Mirror::None);
                }
                if is_house(grid, x, y + 1) && !mansion.is_room_id(x, y + 1, floor_num, room_id) {
                    let p = relative(room_pos, r(Direction::South), 7);
                    let p = relative(p, r(Direction::East), 7);
                    self.add(
                        wall_or_door(Direction::South),
                        p,
                        rotation.rotated(Rotation::Clockwise90),
                        Mirror::None,
                    );
                }
                if grid.get(x, y - 1) == cell::CORRIDOR && !third_floor_start_room {
                    let p = relative(room_pos, r(Direction::North), 1);
                    let p = relative(p, r(Direction::East), 7);
                    self.add(
                        wall_or_door(Direction::North),
                        p,
                        rotation.rotated(Rotation::Clockwise90),
                        Mirror::None,
                    );
                }
                match (room_type, door_dir) {
                    (cell::ROOM_1X1, _) => self.add_room_1x1(room_pos, rotation, door_dir, floor),
                    (cell::ROOM_1X2, Some(door_dir)) => {
                        let room_dir = mansion.get_1x2_room_direction(x, y, floor_num, room_id);
                        let is_stairs_room =
                            (room_data & cell::ROOM_STAIRS_FLAG) == cell::ROOM_STAIRS_FLAG;
                        self.add_room_1x2(
                            room_pos,
                            rotation,
                            room_dir,
                            door_dir,
                            floor,
                            is_stairs_room,
                        );
                    }
                    (cell::ROOM_2X2, Some(Direction::Up)) => {
                        self.add_room_2x2_secret(room_pos, rotation, floor);
                    }
                    (cell::ROOM_2X2, Some(door_dir)) => {
                        let mut room_dir = clockwise(door_dir);
                        let (dx, dz) = step(room_dir);
                        if !mansion.is_room_id(x + dx, y + dz, floor_num, room_id) {
                            room_dir = room_dir.opposite();
                        }
                        self.add_room_2x2(room_pos, rotation, room_dir, door_dir, floor);
                    }
                    _ => {}
                }
            }
        }
    }

    /// `traverseOuterWalls(pieces, data, grid, gridDirection, startX, startY, endX, endY)`.
    #[allow(clippy::too_many_arguments)]
    fn traverse_outer_walls(
        &mut self,
        data: &mut PlacementData,
        grid: &SimpleGrid,
        mut grid_direction: Direction,
        start_x: i32,
        start_y: i32,
        end_x: i32,
        end_y: i32,
    ) {
        let (mut grid_x, mut grid_y) = (start_x, start_y);
        let start_direction = grid_direction;
        loop {
            let (dx, dz) = step(grid_direction);
            let (ccx, ccz) = step(counter_clockwise(grid_direction));
            if !is_house(grid, grid_x + dx, grid_y + dz) {
                self.traverse_turn(data);
                grid_direction = clockwise(grid_direction);
                if grid_x != end_x || grid_y != end_y || start_direction != grid_direction {
                    self.traverse_wall_piece(data);
                }
            } else if is_house(grid, grid_x + dx + ccx, grid_y + dz + ccz) {
                self.traverse_inner_turn(data);
                grid_x += dx;
                grid_y += dz;
                grid_direction = counter_clockwise(grid_direction);
            } else {
                grid_x += dx;
                grid_y += dz;
                if grid_x != end_x || grid_y != end_y || start_direction != grid_direction {
                    self.traverse_wall_piece(data);
                }
            }
            if grid_x == end_x && grid_y == end_y && start_direction == grid_direction {
                break;
            }
        }
    }

    /// `createRoof(pieces, roofOrigin, rotation, grid, aboveGrid)`.
    fn create_roof(
        &mut self,
        roof_origin: BlockPos,
        rotation: Rotation,
        grid: &SimpleGrid,
        above_grid: Option<&SimpleGrid>,
    ) {
        let r = |direction: Direction| rotation.rotate(direction);
        let none = Mirror::None;
        for y in 0..grid.height {
            for x in 0..grid.width {
                let position = self.cell_pos(roof_origin, rotation, x, y, 0);
                let is_above = above_grid.is_some_and(|g| is_house(g, x, y));
                if !is_house(grid, x, y) || is_above {
                    continue;
                }
                self.add("roof", above(position, 3), rotation, none);
                if !is_house(grid, x + 1, y) {
                    let p = relative(position, r(Direction::East), 6);
                    self.add("roof_front", p, rotation, none);
                }
                if !is_house(grid, x - 1, y) {
                    let p = relative(position, r(Direction::East), 0);
                    let p = relative(p, r(Direction::South), 7);
                    self.add(
                        "roof_front",
                        p,
                        rotation.rotated(Rotation::Clockwise180),
                        none,
                    );
                }
                if !is_house(grid, x, y - 1) {
                    let p = relative(position, r(Direction::West), 1);
                    self.add(
                        "roof_front",
                        p,
                        rotation.rotated(Rotation::Counterclockwise90),
                        none,
                    );
                }
                if !is_house(grid, x, y + 1) {
                    let p = relative(position, r(Direction::East), 6);
                    let p = relative(p, r(Direction::South), 6);
                    self.add(
                        "roof_front",
                        p,
                        rotation.rotated(Rotation::Clockwise90),
                        none,
                    );
                }
            }
        }
        if let Some(above_grid) = above_grid {
            for y in 0..grid.height {
                for x in 0..grid.width {
                    let position = self.cell_pos(roof_origin, rotation, x, y, 0);
                    if !is_house(grid, x, y) || !is_house(above_grid, x, y) {
                        continue;
                    }
                    if !is_house(grid, x + 1, y) {
                        let p = relative(position, r(Direction::East), 7);
                        self.add("small_wall", p, rotation, none);
                    }
                    if !is_house(grid, x - 1, y) {
                        let p = relative(position, r(Direction::West), 1);
                        let p = relative(p, r(Direction::South), 6);
                        self.add(
                            "small_wall",
                            p,
                            rotation.rotated(Rotation::Clockwise180),
                            none,
                        );
                    }
                    if !is_house(grid, x, y - 1) {
                        let p = relative(position, r(Direction::West), 0);
                        let p = relative(p, r(Direction::North), 1);
                        self.add(
                            "small_wall",
                            p,
                            rotation.rotated(Rotation::Counterclockwise90),
                            none,
                        );
                    }
                    if !is_house(grid, x, y + 1) {
                        let p = relative(position, r(Direction::East), 6);
                        let p = relative(p, r(Direction::South), 7);
                        self.add(
                            "small_wall",
                            p,
                            rotation.rotated(Rotation::Clockwise90),
                            none,
                        );
                    }
                    if !is_house(grid, x + 1, y) {
                        if !is_house(grid, x, y - 1) {
                            let p = relative(position, r(Direction::East), 7);
                            let p = relative(p, r(Direction::North), 2);
                            self.add("small_wall_corner", p, rotation, none);
                        }
                        if !is_house(grid, x, y + 1) {
                            let p = relative(position, r(Direction::East), 8);
                            let p = relative(p, r(Direction::South), 7);
                            self.add(
                                "small_wall_corner",
                                p,
                                rotation.rotated(Rotation::Clockwise90),
                                none,
                            );
                        }
                    }
                    if !is_house(grid, x - 1, y) {
                        if !is_house(grid, x, y - 1) {
                            let p = relative(position, r(Direction::West), 2);
                            let p = relative(p, r(Direction::North), 1);
                            self.add(
                                "small_wall_corner",
                                p,
                                rotation.rotated(Rotation::Counterclockwise90),
                                none,
                            );
                        }
                        if !is_house(grid, x, y + 1) {
                            let p = relative(position, r(Direction::West), 1);
                            let p = relative(p, r(Direction::South), 8);
                            self.add(
                                "small_wall_corner",
                                p,
                                rotation.rotated(Rotation::Clockwise180),
                                none,
                            );
                        }
                    }
                }
            }
        }
        for y in 0..grid.height {
            for x in 0..grid.width {
                let position = self.cell_pos(roof_origin, rotation, x, y, 0);
                let is_above = above_grid.is_some_and(|g| is_house(g, x, y));
                if !is_house(grid, x, y) || is_above {
                    continue;
                }
                if !is_house(grid, x + 1, y) {
                    let p2 = relative(position, r(Direction::East), 6);
                    if !is_house(grid, x, y + 1) {
                        let p3 = relative(p2, r(Direction::South), 6);
                        self.add("roof_corner", p3, rotation, none);
                    } else if is_house(grid, x + 1, y + 1) {
                        let p3 = relative(p2, r(Direction::South), 5);
                        self.add("roof_inner_corner", p3, rotation, none);
                    }
                    if !is_house(grid, x, y - 1) {
                        self.add(
                            "roof_corner",
                            p2,
                            rotation.rotated(Rotation::Counterclockwise90),
                            none,
                        );
                    } else if is_house(grid, x + 1, y - 1) {
                        let p3 = relative(position, r(Direction::East), 9);
                        let p3 = relative(p3, r(Direction::North), 2);
                        self.add(
                            "roof_inner_corner",
                            p3,
                            rotation.rotated(Rotation::Clockwise90),
                            none,
                        );
                    }
                }
                if !is_house(grid, x - 1, y) {
                    let p2 = relative(position, r(Direction::East), 0);
                    let p2 = relative(p2, r(Direction::South), 0);
                    if !is_house(grid, x, y + 1) {
                        let p3 = relative(p2, r(Direction::South), 6);
                        self.add(
                            "roof_corner",
                            p3,
                            rotation.rotated(Rotation::Clockwise90),
                            none,
                        );
                    } else if is_house(grid, x - 1, y + 1) {
                        let p3 = relative(p2, r(Direction::South), 8);
                        let p3 = relative(p3, r(Direction::West), 3);
                        self.add(
                            "roof_inner_corner",
                            p3,
                            rotation.rotated(Rotation::Counterclockwise90),
                            none,
                        );
                    }
                    if !is_house(grid, x, y - 1) {
                        self.add(
                            "roof_corner",
                            p2,
                            rotation.rotated(Rotation::Clockwise180),
                            none,
                        );
                    } else if is_house(grid, x - 1, y - 1) {
                        let p3 = relative(p2, r(Direction::South), 1);
                        self.add(
                            "roof_inner_corner",
                            p3,
                            rotation.rotated(Rotation::Clockwise180),
                            none,
                        );
                    }
                }
            }
        }
    }

    /// `entrance(pieces, data)`.
    fn entrance(&mut self, data: &mut PlacementData) {
        let west = data.rotation.rotate(Direction::West);
        self.add(
            "entrance",
            relative(data.position, west, 9),
            data.rotation,
            Mirror::None,
        );
        data.position = relative(data.position, data.rotation.rotate(Direction::South), 16);
    }

    /// `traverseWallPiece(pieces, data)`.
    fn traverse_wall_piece(&mut self, data: &mut PlacementData) {
        let position = relative(data.position, data.rotation.rotate(Direction::East), 7);
        self.add(data.wall_type, position, data.rotation, Mirror::None);
        data.position = relative(data.position, data.rotation.rotate(Direction::South), 8);
    }

    /// `traverseTurn(pieces, data)`.
    fn traverse_turn(&mut self, data: &mut PlacementData) {
        data.position = relative(data.position, data.rotation.rotate(Direction::South), -1);
        self.add("wall_corner", data.position, data.rotation, Mirror::None);
        data.position = relative(data.position, data.rotation.rotate(Direction::South), -7);
        data.position = relative(data.position, data.rotation.rotate(Direction::West), -6);
        data.rotation = data.rotation.rotated(Rotation::Clockwise90);
    }

    /// `traverseInnerTurn(pieces, data)`.
    fn traverse_inner_turn(&mut self, data: &mut PlacementData) {
        data.position = relative(data.position, data.rotation.rotate(Direction::South), 6);
        data.position = relative(data.position, data.rotation.rotate(Direction::East), 8);
        data.rotation = data.rotation.rotated(Rotation::Counterclockwise90);
    }

    /// `addRoom1x1(pieces, roomPos, rotation, doorDir, rooms)`.
    fn add_room_1x1(
        &mut self,
        room_pos: BlockPos,
        rotation: Rotation,
        door_dir: Option<Direction>,
        floor: Floor,
    ) {
        let mut piece_rot = Rotation::None;
        let mut room_type = floor.get_1x1(self.random);
        if door_dir != Some(Direction::East) {
            match door_dir {
                Some(Direction::North) => {
                    piece_rot = piece_rot.rotated(Rotation::Counterclockwise90)
                }
                Some(Direction::West) => piece_rot = piece_rot.rotated(Rotation::Clockwise180),
                Some(Direction::South) => piece_rot = piece_rot.rotated(Rotation::Clockwise90),
                _ => room_type = floor.get_1x1_secret(self.random),
            }
        }
        let orientation =
            zero_position_with_transform(BlockPos::new(1, 0, 0), Mirror::None, piece_rot, 7, 7);
        let piece_rot = piece_rot.rotated(rotation);
        let orientation = rotate_pos(orientation, rotation);
        let pos = room_pos.offset(orientation.x, 0, orientation.z);
        self.add(&room_type, pos, piece_rot, Mirror::None);
    }

    /// `addRoom1x2(pieces, roomPos, rotation, roomDir, doorDir, rooms, isStairsRoom)`.
    fn add_room_1x2(
        &mut self,
        room_pos: BlockPos,
        rotation: Rotation,
        room_dir: Option<Direction>,
        door_dir: Direction,
        floor: Floor,
        is_stairs_room: bool,
    ) {
        use Direction::{East, North, South, Up, West};
        let r = |direction: Direction| rotation.rotate(direction);
        let (east, south) = (r(East), r(South));
        // (template kind, east offset, south offset, rotation, mirror); the template name is
        // drawn after the position, as in Vanilla.
        enum Kind {
            Side,
            Front,
            Secret,
        }
        let (kind, pos, rot, mirror) = match (door_dir, room_dir) {
            (East, Some(South)) => (
                Kind::Side,
                relative(room_pos, east, 1),
                rotation,
                Mirror::None,
            ),
            (East, Some(North)) => (
                Kind::Side,
                relative(relative(room_pos, east, 1), south, 6),
                rotation,
                Mirror::LeftRight,
            ),
            (West, Some(North)) => (
                Kind::Side,
                relative(relative(room_pos, east, 7), south, 6),
                rotation.rotated(Rotation::Clockwise180),
                Mirror::None,
            ),
            (West, Some(South)) => (
                Kind::Side,
                relative(room_pos, east, 7),
                rotation,
                Mirror::FrontBack,
            ),
            (South, Some(East)) => (
                Kind::Side,
                relative(room_pos, east, 1),
                rotation.rotated(Rotation::Clockwise90),
                Mirror::LeftRight,
            ),
            (South, Some(West)) => (
                Kind::Side,
                relative(room_pos, east, 7),
                rotation.rotated(Rotation::Clockwise90),
                Mirror::None,
            ),
            (North, Some(West)) => (
                Kind::Side,
                relative(relative(room_pos, east, 7), south, 6),
                rotation.rotated(Rotation::Clockwise90),
                Mirror::FrontBack,
            ),
            (North, Some(East)) => (
                Kind::Side,
                relative(relative(room_pos, east, 1), south, 6),
                rotation.rotated(Rotation::Counterclockwise90),
                Mirror::None,
            ),
            (South, Some(North)) => (
                Kind::Front,
                relative(relative(room_pos, east, 1), r(North), 8),
                rotation,
                Mirror::None,
            ),
            (North, Some(South)) => (
                Kind::Front,
                relative(relative(room_pos, east, 7), south, 14),
                rotation.rotated(Rotation::Clockwise180),
                Mirror::None,
            ),
            (West, Some(East)) => (
                Kind::Front,
                relative(room_pos, east, 15),
                rotation.rotated(Rotation::Clockwise90),
                Mirror::None,
            ),
            (East, Some(West)) => (
                Kind::Front,
                relative(relative(room_pos, r(West), 7), south, 6),
                rotation.rotated(Rotation::Counterclockwise90),
                Mirror::None,
            ),
            (Up, Some(East)) => (
                Kind::Secret,
                relative(room_pos, east, 15),
                rotation.rotated(Rotation::Clockwise90),
                Mirror::None,
            ),
            (Up, Some(South)) => (
                Kind::Secret,
                relative(relative(room_pos, east, 1), r(North), 0),
                rotation,
                Mirror::None,
            ),
            _ => return,
        };
        let name = match kind {
            Kind::Side => floor.get_1x2_side_entrance(self.random, is_stairs_room),
            Kind::Front => floor.get_1x2_front_entrance(self.random, is_stairs_room),
            Kind::Secret => floor.get_1x2_secret(self.random),
        };
        self.add(&name, pos, rot, mirror);
    }

    /// `addRoom2x2(pieces, roomPos, rotation, roomDir, doorDir, rooms)`.
    fn add_room_2x2(
        &mut self,
        room_pos: BlockPos,
        rotation: Rotation,
        room_dir: Direction,
        door_dir: Direction,
        floor: Floor,
    ) {
        use Direction::{East, North, South, West};
        let (east, south, rot, mirror) = match (door_dir, room_dir) {
            (East, South) => (-7, 0, rotation, Mirror::None),
            (East, North) => (-7, 6, rotation, Mirror::LeftRight),
            (North, East) => (
                1,
                14,
                rotation.rotated(Rotation::Counterclockwise90),
                Mirror::None,
            ),
            (North, West) => (
                7,
                14,
                rotation.rotated(Rotation::Counterclockwise90),
                Mirror::LeftRight,
            ),
            (South, West) => (7, -8, rotation.rotated(Rotation::Clockwise90), Mirror::None),
            (South, East) => (
                1,
                -8,
                rotation.rotated(Rotation::Clockwise90),
                Mirror::LeftRight,
            ),
            (West, North) => (
                15,
                6,
                rotation.rotated(Rotation::Clockwise180),
                Mirror::None,
            ),
            (West, South) => (15, 0, rotation, Mirror::FrontBack),
            _ => (0, 0, rotation, Mirror::None),
        };
        let pos = relative(room_pos, rotation.rotate(East), east);
        let pos = relative(pos, rotation.rotate(South), south);
        let name = floor.get_2x2(self.random);
        self.add(&name, pos, rot, mirror);
    }

    /// `addRoom2x2Secret(pieces, roomPos, rotation, rooms)`.
    fn add_room_2x2_secret(&mut self, room_pos: BlockPos, rotation: Rotation, floor: Floor) {
        let pos = relative(room_pos, rotation.rotate(Direction::East), 1);
        self.add(&floor.get_2x2_secret(), pos, rotation, Mirror::None);
    }
}

/// `WoodlandMansionPieces.WoodlandMansionPiece` (`minecraft:wmp`).
#[derive(Clone, Debug)]
pub struct WoodlandMansionPiece {
    pub template: TemplatePiece,
}

impl WoodlandMansionPiece {
    /// `new WoodlandMansionPiece(manager, templateName, position, rotation, mirror)`.
    pub fn new(
        name: &str,
        template: Arc<StructureTemplate>,
        position: BlockPos,
        rotation: Rotation,
        mirror: Mirror,
    ) -> Self {
        // `makeSettings(mirror, rotation)`.
        let settings = PieceSettings {
            rotation,
            mirror,
            ignore_entities: true,
            processors: vec![StructureProcessor::BlockIgnore(vec![ids::STRUCTURE_BLOCK])],
            ..PieceSettings::new()
        };
        Self {
            template: TemplatePiece::new(
                "minecraft:wmp",
                0,
                name.to_owned(),
                template,
                settings,
                position,
            ),
        }
    }
}

impl StructurePiece for WoodlandMansionPiece {
    fn base(&self) -> &PieceBase {
        &self.template.base
    }

    fn base_mut(&mut self) -> &mut PieceBase {
        &mut self.template.base
    }

    fn move_by(&mut self, dx: i32, dy: i32, dz: i32) {
        self.template.move_by(dx, dy, dz);
    }

    fn rotation(&self) -> Rotation {
        self.template.rotation()
    }

    /// `addAdditionalSaveData`: the template fields, `Rot` and `Mi`.
    fn additional_save_data(&self) -> Option<Compound> {
        let settings = &self.template.settings;
        Some(
            self.template
                .save_data()
                .with("Rot", settings.rotation.legacy_name())
                .with("Mi", settings.mirror.legacy_name()),
        )
    }

    fn post_process(
        &mut self,
        ctx: &mut PieceContext<'_, '_>,
        chunk_bb: &mut BoundingBox,
        _chunk_pos: ChunkPos,
        reference: BlockPos,
    ) {
        let rotation = self.template.settings.rotation;
        self.template
            .place(ctx, chunk_bb, reference, |marker, pos, ctx, chunk_bb| {
                handle_data_marker(rotation, marker, pos, ctx, chunk_bb);
            });
    }
}

/// `WoodlandMansionPiece.handleDataMarker(markerId, position, level, random, chunkBB)`.
fn handle_data_marker(
    rotation: Rotation,
    marker: &str,
    pos: BlockPos,
    ctx: &mut PieceContext<'_, '_>,
    chunk_bb: &BoundingBox,
) {
    if marker.starts_with("Chest") {
        let mut state = ids::CHEST.default_state();
        let facing = match marker {
            "ChestWest" => Some(Direction::West),
            "ChestEast" => Some(Direction::East),
            "ChestSouth" => Some(Direction::South),
            "ChestNorth" => Some(Direction::North),
            _ => None,
        };
        if let Some(facing) = facing {
            state = with_facing(state, rotation.rotate(facing));
        }
        create_chest_at(
            ctx.level,
            chunk_bb,
            ctx.random,
            pos,
            LOOT_TABLE,
            Some(state),
        );
        return;
    }
    let (kind, count) = match marker {
        "Mage" => ("minecraft:evoker", 1),
        "Warrior" => ("minecraft:vindicator", 1),
        "Group of Allays" => (
            "minecraft:allay",
            ctx.level.random().next_int_bounded(3) + 1,
        ),
        _ => return,
    };
    // Each mob: `setPersistenceRequired`, `snapTo(position)` (the block's bottom centre),
    // `finalizeSpawn` (draws from the level's random), `addFreshEntityWithPassengers`, then the
    // marker block becomes air.
    let (x, y, z) = (
        f64::from(pos.x) + 0.5,
        f64::from(pos.y),
        f64::from(pos.z) + 0.5,
    );
    for _ in 0..count {
        let random = ctx.level.random();
        mob_finalize_spawn(random);
        if marker == "Warrior" {
            // `Vindicator.finalizeSpawn`: `populateDefaultEquipmentEnchantments` rolls for the
            // main hand axe (`nextFloat() < 0.25 * specialMultiplier`); the armour slots are
            // empty and roll nothing. The special multiplier is 0 below an effective difficulty
            // of 2 (always on easy, where the goldens are recorded); the enchanting that may
            // follow on harder difficulties is not ported.
            random.next_float();
        }
        let mob = entity_tag(kind, x, y, z).with("PersistenceRequired", true);
        ctx.level.add_fresh_entity(mob);
        ctx.level.set_block(pos, BlockState::AIR, 2);
    }
}

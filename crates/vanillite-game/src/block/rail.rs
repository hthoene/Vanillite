//! Rails (`BaseRailBlock`, `RailBlock`, `PoweredRailBlock` for powered and activator rails,
//! `DetectorRailBlock`) and the connection logic of `RailState`.
//!
//! Shape updates (the waterlogged water tick) and survival are shared with world generation.
//! Detector rails sense minecart entities of every kind and give the fill level of a container
//! minecart to comparators.

use vanillite_base::{Aabb, BlockPos, Direction};
use vanillite_data::block::{BlockId, BlockState, ids};
use vanillite_data::entity::ids as entity_ids;
use vanillite_worldgen::behaviour::{self as shared, PosExt, StateExt};

use super::{BlockBehaviour, drops, is_rail};
use crate::level::Level;
use crate::level::signal::powered;

/// `RailShape`, in the value order of the `shape` property.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RailShape {
    NorthSouth,
    EastWest,
    AscendingEast,
    AscendingWest,
    AscendingNorth,
    AscendingSouth,
    SouthEast,
    SouthWest,
    NorthWest,
    NorthEast,
}

impl RailShape {
    const ALL: [RailShape; 10] = [
        RailShape::NorthSouth,
        RailShape::EastWest,
        RailShape::AscendingEast,
        RailShape::AscendingWest,
        RailShape::AscendingNorth,
        RailShape::AscendingSouth,
        RailShape::SouthEast,
        RailShape::SouthWest,
        RailShape::NorthWest,
        RailShape::NorthEast,
    ];

    /// The serialized name.
    pub fn name(self) -> &'static str {
        match self {
            RailShape::NorthSouth => "north_south",
            RailShape::EastWest => "east_west",
            RailShape::AscendingEast => "ascending_east",
            RailShape::AscendingWest => "ascending_west",
            RailShape::AscendingNorth => "ascending_north",
            RailShape::AscendingSouth => "ascending_south",
            RailShape::SouthEast => "south_east",
            RailShape::SouthWest => "south_west",
            RailShape::NorthWest => "north_west",
            RailShape::NorthEast => "north_east",
        }
    }

    /// The shape of a rail state.
    pub fn of(state: BlockState) -> RailShape {
        let name = state.value("shape").unwrap_or("north_south");
        Self::ALL
            .into_iter()
            .find(|s| s.name() == name)
            .unwrap_or(RailShape::NorthSouth)
    }

    /// `isSlope()`.
    pub fn is_slope(self) -> bool {
        matches!(
            self,
            RailShape::AscendingEast
                | RailShape::AscendingWest
                | RailShape::AscendingNorth
                | RailShape::AscendingSouth
        )
    }
}

/// `state.setValue(shapeProperty, shape)`.
fn with_shape(state: BlockState, shape: RailShape) -> BlockState {
    state.set("shape", shape.name())
}

/// `BaseRailBlock.isStraight()`: every rail but the plain one.
fn is_straight(block: BlockId) -> bool {
    block != ids::RAIL
}

/// `BaseRailBlock.isRail(level, pos)`.
fn rail_at(level: &Level, pos: BlockPos) -> bool {
    is_rail(level.block(pos).block())
}

/// Fills the rail behaviours of `block`.
pub(super) fn fill(block: BlockId, b: &mut BlockBehaviour) {
    if !is_rail(block) {
        return;
    }
    b.neighbor_changed = Some(neighbor_changed);
    b.affect_neighbors_after_removal = Some(affect_neighbors_after_removal);
    if block == ids::DETECTOR_RAIL {
        b.on_place = Some(detector_on_place);
        b.tick = Some(detector_tick);
        b.entity_inside = Some(detector_entity_inside);
        b.signal = Some(detector_signal);
        b.direct_signal = Some(detector_direct_signal);
        b.analog_output = Some(detector_analog_output);
    } else {
        b.on_place = Some(on_place);
    }
}

// --- BaseRailBlock -----------------------------------------------------------------------

/// `onPlace`.
fn on_place(level: &mut Level, state: BlockState, pos: BlockPos, old: BlockState, moved: bool) {
    if !old.is(state.block()) {
        update_state_placed(level, state, pos, moved);
    }
}

/// `updateState(state, level, pos, movedByPiston)`: the direction, then for straight rails a
/// neighbour update of the rail itself.
fn update_state_placed(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    moved: bool,
) -> BlockState {
    let state = update_dir(level, pos, state, true);
    if is_straight(state.block()) {
        level.neighbor_changed_moved(state, pos, state.block(), moved);
    }
    state
}

/// `neighborChanged`.
fn neighbor_changed(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    source: BlockId,
    moved: bool,
) {
    if !level.block(pos).is(state.block()) {
        return;
    }
    let shape = RailShape::of(state);
    if should_be_removed(level, pos, shape) {
        drops::drop_resources(level, pos, state, None);
        level.remove_block(pos, moved);
    } else {
        update_state(level, state, pos, source);
    }
}

/// `shouldBeRemoved(pos, level, shape)`.
fn should_be_removed(level: &Level, pos: BlockPos, shape: RailShape) -> bool {
    let support = |p: BlockPos| shared::can_support_rigid_block(level, p);
    if !support(pos.below()) {
        return true;
    }
    match shape {
        RailShape::AscendingEast => !support(pos.relative(Direction::East)),
        RailShape::AscendingWest => !support(pos.relative(Direction::West)),
        RailShape::AscendingNorth => !support(pos.relative(Direction::North)),
        RailShape::AscendingSouth => !support(pos.relative(Direction::South)),
        _ => false,
    }
}

/// `updateState(state, level, pos, block)` of the rail kinds.
fn update_state(level: &mut Level, state: BlockState, pos: BlockPos, source: BlockId) {
    let block = state.block();
    if block == ids::RAIL {
        // `RailBlock.updateState`.
        if level.is_signal_source(source.default_state())
            && RailState::new(level, pos, state).count_potential_connections(level) == 3
        {
            update_dir(level, pos, state, false);
        }
    } else if block == ids::POWERED_RAIL || block == ids::ACTIVATOR_RAIL {
        powered_update_state(level, state, pos);
    }
}

/// `updateDir(level, pos, state, first)`.
fn update_dir(level: &mut Level, pos: BlockPos, state: BlockState, first: bool) -> BlockState {
    let current = RailShape::of(state);
    let has_signal = level.has_neighbor_signal(pos);
    RailState::new(level, pos, state)
        .place(level, has_signal, first, current)
        .state
}

/// `affectNeighborsAfterRemoval`.
fn affect_neighbors_after_removal(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    moved: bool,
) {
    if moved {
        return;
    }
    let block = state.block();
    if RailShape::of(state).is_slope() {
        level.update_neighbors_at(pos.above(), block);
    }
    if is_straight(block) {
        level.update_neighbors_at(pos, block);
        level.update_neighbors_at(pos.below(), block);
    }
}

// --- RailState ---------------------------------------------------------------------------

/// The connections of a rail; at most two plus one being added.
#[derive(Clone, Copy, Debug)]
struct Connections {
    items: [BlockPos; 4],
    len: usize,
}

impl Connections {
    const EMPTY: Connections = Connections {
        items: [BlockPos::new(0, 0, 0); 4],
        len: 0,
    };

    fn push(&mut self, pos: BlockPos) {
        if self.len < self.items.len() {
            self.items[self.len] = pos;
            self.len += 1;
        }
    }

    fn remove(&mut self, index: usize) {
        self.items.copy_within(index + 1..self.len, index);
        self.len -= 1;
    }

    fn as_slice(&self) -> &[BlockPos] {
        &self.items[..self.len]
    }
}

/// `RailState`: a rail and the positions it connects to.
#[derive(Clone, Copy, Debug)]
struct RailState {
    pos: BlockPos,
    state: BlockState,
    straight: bool,
    connections: Connections,
}

impl RailState {
    fn new(_level: &Level, pos: BlockPos, state: BlockState) -> RailState {
        let mut rail = RailState {
            pos,
            state,
            straight: is_straight(state.block()),
            connections: Connections::EMPTY,
        };
        rail.update_connections(RailShape::of(state));
        rail
    }

    /// `updateConnections(direction)`.
    fn update_connections(&mut self, shape: RailShape) {
        let p = self.pos;
        let (n, s, w, e) = (
            p.relative(Direction::North),
            p.relative(Direction::South),
            p.relative(Direction::West),
            p.relative(Direction::East),
        );
        let (a, b) = match shape {
            RailShape::NorthSouth => (n, s),
            RailShape::EastWest => (w, e),
            RailShape::AscendingEast => (w, e.above()),
            RailShape::AscendingWest => (w.above(), e),
            RailShape::AscendingNorth => (n.above(), s),
            RailShape::AscendingSouth => (n, s.above()),
            RailShape::SouthEast => (e, s),
            RailShape::SouthWest => (w, s),
            RailShape::NorthWest => (w, n),
            RailShape::NorthEast => (e, n),
        };
        self.connections = Connections::EMPTY;
        self.connections.push(a);
        self.connections.push(b);
    }

    /// `removeSoftConnections()`: keeps the connections whose rail connects back.
    fn remove_soft_connections(&mut self, level: &Level) {
        let mut i = 0;
        while i < self.connections.len {
            match get_rail(level, self.connections.items[i]) {
                Some(rail) if rail.has_connection(self.pos) => {
                    self.connections.items[i] = rail.pos;
                    i += 1;
                }
                _ => self.connections.remove(i),
            }
        }
    }

    /// `hasConnection(railPos)`: same column.
    fn has_connection(&self, pos: BlockPos) -> bool {
        self.connections
            .as_slice()
            .iter()
            .any(|p| p.x == pos.x && p.z == pos.z)
    }

    /// `countPotentialConnections()`.
    fn count_potential_connections(&self, level: &Level) -> usize {
        shared::HORIZONTAL
            .into_iter()
            .filter(|&d| {
                let p = self.pos.relative(d);
                rail_at(level, p) || rail_at(level, p.above()) || rail_at(level, p.below())
            })
            .count()
    }

    /// `canConnectTo(rail)`.
    fn can_connect_to(&self, other: &RailState) -> bool {
        self.has_connection(other.pos) || self.connections.len != 2
    }

    /// `connectTo(rail)`: the new shape towards `other`, set at once.
    fn connect_to(&mut self, level: &mut Level, other: &RailState) {
        self.connections.push(other.pos);
        let p = self.pos;
        let north = p.relative(Direction::North);
        let south = p.relative(Direction::South);
        let west = p.relative(Direction::West);
        let east = p.relative(Direction::East);
        let n = self.has_connection(north);
        let s = self.has_connection(south);
        let w = self.has_connection(west);
        let e = self.has_connection(east);
        let mut shape = None;
        if n || s {
            shape = Some(RailShape::NorthSouth);
        }
        if w || e {
            shape = Some(RailShape::EastWest);
        }
        if !self.straight {
            if s && e && !n && !w {
                shape = Some(RailShape::SouthEast);
            }
            if s && w && !n && !e {
                shape = Some(RailShape::SouthWest);
            }
            if n && w && !s && !e {
                shape = Some(RailShape::NorthWest);
            }
            if n && e && !s && !w {
                shape = Some(RailShape::NorthEast);
            }
        }
        let shape = ascend(level, shape, north, south, east, west).unwrap_or(RailShape::NorthSouth);
        self.state = with_shape(self.state, shape);
        level.set_block_and_update(self.pos, self.state);
    }

    /// `hasNeighborRail(railPos)`.
    fn has_neighbor_rail(&self, level: &Level, pos: BlockPos) -> bool {
        match get_rail(level, pos) {
            Some(mut neighbour) => {
                neighbour.remove_soft_connections(level);
                neighbour.can_connect_to(self)
            }
            None => false,
        }
    }

    /// `place(hasSignal, first, defaultShape)`.
    fn place(
        mut self,
        level: &mut Level,
        has_signal: bool,
        first: bool,
        default: RailShape,
    ) -> Self {
        let p = self.pos;
        let north = p.relative(Direction::North);
        let south = p.relative(Direction::South);
        let west = p.relative(Direction::West);
        let east = p.relative(Direction::East);
        let n = self.has_neighbor_rail(level, north);
        let s = self.has_neighbor_rail(level, south);
        let w = self.has_neighbor_rail(level, west);
        let e = self.has_neighbor_rail(level, east);
        let mut shape = None;
        let north_or_south = n || s;
        let west_or_east = w || e;
        if north_or_south && !west_or_east {
            shape = Some(RailShape::NorthSouth);
        }
        if west_or_east && !north_or_south {
            shape = Some(RailShape::EastWest);
        }
        let south_east = s && e;
        let south_west = s && w;
        let north_east = n && e;
        let north_west = n && w;
        if !self.straight {
            if south_east && !n && !w {
                shape = Some(RailShape::SouthEast);
            }
            if south_west && !n && !e {
                shape = Some(RailShape::SouthWest);
            }
            if north_west && !s && !e {
                shape = Some(RailShape::NorthWest);
            }
            if north_east && !s && !w {
                shape = Some(RailShape::NorthEast);
            }
        }
        if shape.is_none() {
            if north_or_south && west_or_east {
                shape = Some(default);
            } else if north_or_south {
                shape = Some(RailShape::NorthSouth);
            } else if west_or_east {
                shape = Some(RailShape::EastWest);
            }
            if !self.straight {
                let order: [(bool, RailShape); 4] = if has_signal {
                    [
                        (south_east, RailShape::SouthEast),
                        (south_west, RailShape::SouthWest),
                        (north_east, RailShape::NorthEast),
                        (north_west, RailShape::NorthWest),
                    ]
                } else {
                    [
                        (north_west, RailShape::NorthWest),
                        (north_east, RailShape::NorthEast),
                        (south_west, RailShape::SouthWest),
                        (south_east, RailShape::SouthEast),
                    ]
                };
                for (condition, s) in order {
                    if condition {
                        shape = Some(s);
                    }
                }
            }
        }
        let shape = ascend(level, shape, north, south, east, west).unwrap_or(default);
        self.update_connections(shape);
        self.state = with_shape(self.state, shape);
        if first || level.block(self.pos) != self.state {
            level.set_block_and_update(self.pos, self.state);
            for i in 0..self.connections.len {
                let Some(mut neighbour) = get_rail(level, self.connections.items[i]) else {
                    continue;
                };
                neighbour.remove_soft_connections(level);
                if neighbour.can_connect_to(&self) {
                    neighbour.connect_to(level, &self);
                }
            }
        }
        self
    }
}

/// The slope part of `connectTo` and `place`: a straight shape ascends towards a rail one
/// block up.
fn ascend(
    level: &Level,
    shape: Option<RailShape>,
    north: BlockPos,
    south: BlockPos,
    east: BlockPos,
    west: BlockPos,
) -> Option<RailShape> {
    let mut shape = shape;
    if shape == Some(RailShape::NorthSouth) {
        if rail_at(level, north.above()) {
            shape = Some(RailShape::AscendingNorth);
        }
        if rail_at(level, south.above()) {
            shape = Some(RailShape::AscendingSouth);
        }
    }
    if shape == Some(RailShape::EastWest) {
        if rail_at(level, east.above()) {
            shape = Some(RailShape::AscendingEast);
        }
        if rail_at(level, west.above()) {
            shape = Some(RailShape::AscendingWest);
        }
    }
    shape
}

/// `getRail(pos)`: the rail at, above or below `pos`.
fn get_rail(level: &Level, pos: BlockPos) -> Option<RailState> {
    for p in [pos, pos.above(), pos.below()] {
        let state = level.block(p);
        if is_rail(state.block()) {
            return Some(RailState::new(level, p, state));
        }
    }
    None
}

// --- PoweredRailBlock --------------------------------------------------------------------

/// `PoweredRailBlock.updateState`.
fn powered_update_state(level: &mut Level, state: BlockState, pos: BlockPos) {
    let is_powered = powered(state);
    let should_power = level.has_neighbor_signal(pos)
        || find_powered_rail_signal(level, pos, state, true, 0)
        || find_powered_rail_signal(level, pos, state, false, 0);
    if should_power != is_powered {
        let block = state.block();
        level.set_block_and_update(pos, state.set_flag("powered", should_power));
        level.update_neighbors_at(pos.below(), block);
        if RailShape::of(state).is_slope() {
            level.update_neighbors_at(pos.above(), block);
        }
    }
}

/// `findPoweredRailSignal(level, pos, state, forward, searchDepth)`.
fn find_powered_rail_signal(
    level: &Level,
    pos: BlockPos,
    state: BlockState,
    forward: bool,
    depth: i32,
) -> bool {
    if depth >= 8 {
        return false;
    }
    let (mut x, mut y, mut z) = (pos.x, pos.y, pos.z);
    let mut check_below = true;
    let mut shape = RailShape::of(state);
    match shape {
        RailShape::NorthSouth => z += if forward { 1 } else { -1 },
        RailShape::EastWest => x += if forward { -1 } else { 1 },
        RailShape::AscendingEast => {
            if forward {
                x -= 1;
            } else {
                x += 1;
                y += 1;
                check_below = false;
            }
            shape = RailShape::EastWest;
        }
        RailShape::AscendingWest => {
            if forward {
                x -= 1;
                y += 1;
                check_below = false;
            } else {
                x += 1;
            }
            shape = RailShape::EastWest;
        }
        RailShape::AscendingNorth => {
            if forward {
                z += 1;
            } else {
                z -= 1;
                y += 1;
                check_below = false;
            }
            shape = RailShape::NorthSouth;
        }
        RailShape::AscendingSouth => {
            if forward {
                z += 1;
                y += 1;
                check_below = false;
            } else {
                z -= 1;
            }
            shape = RailShape::NorthSouth;
        }
        _ => {}
    }
    let block = state.block();
    is_same_rail_with_power(level, block, BlockPos::new(x, y, z), forward, depth, shape)
        || (check_below
            && is_same_rail_with_power(
                level,
                block,
                BlockPos::new(x, y - 1, z),
                forward,
                depth,
                shape,
            ))
}

/// `isSameRailWithPower(level, pos, forward, searchDepth, dir)`.
fn is_same_rail_with_power(
    level: &Level,
    block: BlockId,
    pos: BlockPos,
    forward: bool,
    depth: i32,
    dir: RailShape,
) -> bool {
    let state = level.block(pos);
    if !state.is(block) {
        return false;
    }
    let mine = RailShape::of(state);
    if dir == RailShape::EastWest
        && matches!(
            mine,
            RailShape::NorthSouth | RailShape::AscendingNorth | RailShape::AscendingSouth
        )
    {
        return false;
    }
    if dir == RailShape::NorthSouth
        && matches!(
            mine,
            RailShape::EastWest | RailShape::AscendingEast | RailShape::AscendingWest
        )
    {
        return false;
    }
    if !powered(state) {
        return false;
    }
    level.has_neighbor_signal(pos)
        || find_powered_rail_signal(level, pos, state, forward, depth + 1)
}

// --- DetectorRailBlock -------------------------------------------------------------------

/// `DetectorRailBlock.onPlace`.
fn detector_on_place(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    old: BlockState,
    moved: bool,
) {
    if !old.is(state.block()) {
        let updated = update_state_placed(level, state, pos, moved);
        detector_check_pressed(level, pos, updated);
    }
}

/// `DetectorRailBlock.tick`.
fn detector_tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    if powered(state) {
        detector_check_pressed(level, pos, state);
    }
}

/// `DetectorRailBlock.entityInside`.
fn detector_entity_inside(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    _entity: vanillite_world::entity::EntityId,
) {
    if !powered(state) {
        detector_check_pressed(level, pos, state);
    }
}

/// `ownSignal` (the weak signal to all sides).
fn detector_signal(
    _level: &Level,
    state: BlockState,
    _pos: BlockPos,
    _direction: Direction,
) -> i32 {
    if powered(state) { 15 } else { 0 }
}

/// `getDirectSignal`: strongly into the block below.
fn detector_direct_signal(
    _level: &Level,
    state: BlockState,
    _pos: BlockPos,
    direction: Direction,
) -> i32 {
    if powered(state) && direction == Direction::Up {
        15
    } else {
        0
    }
}

/// `getAnalogOutputSignal`: while pressed, the success count of a command block minecart
/// (always 0: they run no commands yet) or the fill level of a container minecart.
fn detector_analog_output(
    level: &Level,
    state: BlockState,
    pos: BlockPos,
    _direction: Direction,
) -> i32 {
    if !powered(state) {
        return 0;
    }
    let ents = &level.entities;
    let mut command_block = false;
    let mut container = None;
    ents.store
        .for_each_in(&detector_search_box(pos), false, |id| {
            if let Some(m) = ents.minecart(id) {
                if m.kind == crate::minecart::MinecartKind::CommandBlock {
                    command_block = true;
                } else if container.is_none() && m.kind.container_size() > 0 {
                    container = Some(crate::minecart::container_signal(m));
                }
            }
        });
    if command_block {
        return 0;
    }
    container.unwrap_or(0)
}

/// `getSearchBB(pos)`.
fn detector_search_box(pos: BlockPos) -> Aabb {
    let (x, y, z) = (f64::from(pos.x), f64::from(pos.y), f64::from(pos.z));
    Aabb::new(
        x + 0.2,
        y,
        z + 0.2,
        x + 1.0 - 0.2,
        y + 1.0 - 0.2,
        z + 1.0 - 0.2,
    )
}

/// `entity instanceof AbstractMinecart`.
pub fn is_minecart(kind: vanillite_data::entity::EntityTypeId) -> bool {
    kind == entity_ids::MINECART
        || kind == entity_ids::CHEST_MINECART
        || kind == entity_ids::COMMAND_BLOCK_MINECART
        || kind == entity_ids::FURNACE_MINECART
        || kind == entity_ids::HOPPER_MINECART
        || kind == entity_ids::SPAWNER_MINECART
        || kind == entity_ids::TNT_MINECART
}

/// `checkPressed(level, pos, state)`.
fn detector_check_pressed(level: &mut Level, pos: BlockPos, state: BlockState) {
    if !level.can_survive(state, pos) || !level.block(pos).is(state.block()) {
        return;
    }
    let block = state.block();
    let was_pressed = powered(state);
    let store = &level.entities.store;
    let mut should_be_pressed = false;
    store.for_each_in(&detector_search_box(pos), false, |id| {
        if store.get(id).is_some_and(|e| is_minecart(e.kind())) {
            should_be_pressed = true;
        }
    });
    if should_be_pressed != was_pressed {
        let new_state = state.set_flag("powered", should_be_pressed);
        level.set_block_and_update(pos, new_state);
        update_power_to_connected(level, pos, new_state);
        level.update_neighbors_at(pos, block);
        level.update_neighbors_at(pos.below(), block);
    }
    if should_be_pressed {
        level.schedule_block_tick(pos, block, 20);
    }
    level.update_neighbour_for_output_signal(pos, block);
}

/// `updatePowerToConnected`: the connected rails get a neighbour update of themselves.
fn update_power_to_connected(level: &mut Level, pos: BlockPos, state: BlockState) {
    let rail = RailState::new(level, pos, state);
    for &connection in rail.connections.as_slice() {
        let connection_state = level.block(connection);
        level.neighbor_changed_state(connection_state, connection, connection_state.block());
    }
}

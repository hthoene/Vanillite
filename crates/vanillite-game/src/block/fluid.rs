//! Water and lava (`FlowingFluid`, `WaterFluid`, `LavaFluid`, `LiquidBlock`), waterlogging
//! (`SimpleWaterloggedBlock`, `LiquidBlockContainer`) and the sponge.
//!
//! A fluid tick recomputes a flowing fluid's level from its neighbours (`getNewLiquid`), then
//! spreads: down first, sideways towards the nearest hole within the slope distance. Lava next
//! to water turns into obsidian, cobblestone or (over soul soil next to blue ice) basalt; lava
//! flowing down into water makes stone.

use std::sync::OnceLock;

use vanillite_worldgen::behaviour::{PosExt, StateExt};
use vanillite_base::hash::FxHashMap;
use vanillite_base::{BlockPos, Direction};
use vanillite_data::block::{BLOCK_COUNT, BLOCKS, BlockId, BlockState, PROPERTIES, ids};
use vanillite_data::fluid::{FLUID_STATES, FluidState};
use vanillite_worldgen::behaviour::{is_full_block, merged_face_occludes};

use super::{block_tags, drops, is_liquid, is_solid};
use crate::level::{Level, update};

/// `Fluids.EMPTY`.
pub const EMPTY: u16 = 0;
/// `Fluids.FLOWING_WATER`.
pub const FLOWING_WATER: u16 = 1;
/// `Fluids.WATER`.
pub const WATER: u16 = 2;
/// `Fluids.FLOWING_LAVA`.
pub const FLOWING_LAVA: u16 = 3;
/// `Fluids.LAVA`.
pub const LAVA: u16 = 4;

/// `Direction.Plane.HORIZONTAL`.
const HORIZONTAL: [Direction; 4] = [
    Direction::North,
    Direction::East,
    Direction::South,
    Direction::West,
];

/// `LiquidBlock.POSSIBLE_FLOW_DIRECTIONS`.
const POSSIBLE_FLOW_DIRECTIONS: [Direction; 5] = [
    Direction::Down,
    Direction::South,
    Direction::North,
    Direction::East,
    Direction::West,
];

/// `LevelEvent.LAVA_FIZZ`.
const LAVA_FIZZ: i32 = 1501;

/// The two flowing fluids.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Water,
    Lava,
}

impl Kind {
    fn of(fluid: u16) -> Option<Kind> {
        match fluid {
            FLOWING_WATER | WATER => Some(Kind::Water),
            FLOWING_LAVA | LAVA => Some(Kind::Lava),
            _ => None,
        }
    }

    /// `fluid.getType().isSame(this)`.
    fn is_same(self, fluid: FluidState) -> bool {
        Kind::of(fluid.info().fluid) == Some(self)
    }

    fn source(self) -> u16 {
        match self {
            Kind::Water => WATER,
            Kind::Lava => LAVA,
        }
    }

    fn flowing(self) -> u16 {
        match self {
            Kind::Water => FLOWING_WATER,
            Kind::Lava => FLOWING_LAVA,
        }
    }

    /// `getDropOff`.
    fn drop_off(self, level: &Level) -> i32 {
        match self {
            Kind::Water => 1,
            Kind::Lava if level.settings.fast_lava => 1,
            Kind::Lava => 2,
        }
    }

    /// `getSlopeFindDistance`.
    fn slope_find_distance(self, level: &Level) -> i32 {
        match self {
            Kind::Water => 4,
            Kind::Lava if level.settings.fast_lava => 4,
            Kind::Lava => 2,
        }
    }

    /// `canConvertToSource`.
    fn can_convert_to_source(self, level: &Level) -> bool {
        match self {
            Kind::Water => level.settings.water_source_conversion,
            Kind::Lava => level.settings.lava_source_conversion,
        }
    }

    /// `isSourceBlockOfThisType`.
    fn is_source_of(self, fluid: FluidState) -> bool {
        self.is_same(fluid) && fluid.info().source
    }
}

/// `fluid.getTickDelay(level)` of a fluid id.
pub fn tick_delay(level: &Level, fluid: u16) -> i32 {
    match Kind::of(fluid) {
        Some(Kind::Water) => 5,
        Some(Kind::Lava) if level.settings.fast_lava => 10,
        Some(Kind::Lava) => 30,
        None => 0,
    }
}

/// The fluid state with these values (`getSource(falling)` / `getFlowing(amount, falling)`).
fn fluid_state(fluid: u16, amount: i32, falling: bool) -> FluidState {
    static TABLE: OnceLock<FxHashMap<(u16, u8, bool), FluidState>> = OnceLock::new();
    let table = TABLE.get_or_init(|| {
        FLUID_STATES
            .iter()
            .enumerate()
            .map(|(id, info)| ((info.fluid, info.amount, info.falling), FluidState(id as u8)))
            .collect()
    });
    let amount = if matches!(fluid, WATER | LAVA) {
        8
    } else {
        amount.clamp(0, 8) as u8
    };
    table
        .get(&(fluid, amount, falling))
        .copied()
        .unwrap_or(FluidState::EMPTY)
}

/// `fluidState.getHeight(level, pos)`.
fn height(level: &Level, fluid: FluidState, pos: BlockPos) -> f32 {
    match Kind::of(fluid.info().fluid) {
        Some(kind) if kind.is_same(level.fluid(pos.above())) => 1.0,
        _ => fluid.info().own_height,
    }
}

/// `fluidState.canBeReplacedWith(level, pos, other, direction)`.
fn can_be_replaced_with(level: &Level, fluid: FluidState, pos: BlockPos, other: u16, direction: Direction) -> bool {
    let other_is_water = Kind::of(other) == Some(Kind::Water);
    match Kind::of(fluid.info().fluid) {
        None => true,
        Some(Kind::Water) => direction == Direction::Down && !other_is_water,
        Some(Kind::Lava) => height(level, fluid, pos) >= 0.444_444_45 && other_is_water,
    }
}

/// How a block holds fluids (`LiquidBlockContainer`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Container {
    /// Not a container.
    None,
    /// `SimpleWaterloggedBlock`: takes a water source.
    Waterlogged,
    /// Kelp and seagrass: hold water but take none.
    Closed,
}

fn container(block: BlockId) -> Container {
    static TABLE: OnceLock<Box<[Container]>> = OnceLock::new();
    TABLE.get_or_init(|| {
        (0..BLOCK_COUNT)
            .map(|id| {
                let block = BlockId(id as u16);
                if [ids::KELP, ids::KELP_PLANT, ids::SEAGRASS, ids::TALL_SEAGRASS].contains(&block) {
                    Container::Closed
                } else if BLOCKS[id]
                    .properties
                    .iter()
                    .any(|&p| PROPERTIES[usize::from(p)].name == "waterlogged")
                {
                    Container::Waterlogged
                } else {
                    Container::None
                }
            })
            .collect()
    })[usize::from(block.0)]
}

/// `canHoldAnyFluid(state)`.
fn can_hold_any_fluid(state: BlockState) -> bool {
    container(state.block()) != Container::None || block_tags().washed_away_by_fluids.contains(state)
}

/// `canHoldSpecificFluid(level, pos, state, fluid)`.
fn can_hold_specific_fluid(state: BlockState, fluid: u16) -> bool {
    match container(state.block()) {
        Container::None => true,
        // `SimpleWaterloggedBlock.canPlaceLiquid`: `type == Fluids.WATER`.
        Container::Waterlogged => fluid == WATER,
        Container::Closed => false,
    }
}

/// `canPassThroughWall(direction, level, sourcePos, sourceState, targetPos, targetState)`.
fn can_pass_through_wall(direction: Direction, source: BlockState, target: BlockState) -> bool {
    let target_shape = target.collision_shape();
    if is_full_block(target_shape) {
        return false;
    }
    let source_shape = source.collision_shape();
    if is_full_block(source_shape) {
        return false;
    }
    if source_shape.is_empty() && target_shape.is_empty() {
        return true;
    }
    !merged_face_occludes(source_shape, target_shape, direction)
}

/// Block reads and hole checks of one spread computation (`FlowingFluid.SpreadContext`).
struct SpreadContext {
    states: FxHashMap<BlockPos, BlockState>,
    holes: FxHashMap<BlockPos, bool>,
}

impl SpreadContext {
    fn new() -> SpreadContext {
        SpreadContext {
            states: FxHashMap::default(),
            holes: FxHashMap::default(),
        }
    }

    fn block(&mut self, level: &Level, pos: BlockPos) -> BlockState {
        *self.states.entry(pos).or_insert_with(|| level.block(pos))
    }

    fn is_hole(&mut self, level: &Level, kind: Kind, pos: BlockPos) -> bool {
        if let Some(&hole) = self.holes.get(&pos) {
            return hole;
        }
        let state = self.block(level, pos);
        let below = pos.below();
        let hole = is_water_hole(kind, state, level.block(below));
        self.holes.insert(pos, hole);
        hole
    }
}

/// `isWaterHole(level, topPos, topState, bottomPos, bottomState)`.
fn is_water_hole(kind: Kind, top: BlockState, bottom: BlockState) -> bool {
    if !can_pass_through_wall(Direction::Down, top, bottom) {
        return false;
    }
    kind.is_same(bottom.fluid_state())
        || can_hold_any_fluid(bottom) && can_hold_specific_fluid(bottom, kind.flowing())
}

/// `canMaybePassThrough(level, sourcePos, sourceState, direction, testPos, testState, testFluid)`.
fn can_maybe_pass_through(kind: Kind, source: BlockState, direction: Direction, test: BlockState) -> bool {
    !kind.is_source_of(test.fluid_state())
        && can_hold_any_fluid(test)
        && can_pass_through_wall(direction, source, test)
}

/// `getNewLiquid(level, pos, state)`: the fluid this position gets from its neighbours.
fn new_liquid(level: &Level, kind: Kind, pos: BlockPos, state: BlockState) -> FluidState {
    let mut highest = 0;
    let mut sources = 0;
    for direction in HORIZONTAL {
        let neighbour = level.block(pos.relative(direction));
        let fluid = neighbour.fluid_state();
        if kind.is_same(fluid) && can_pass_through_wall(direction, state, neighbour) {
            if fluid.info().source {
                sources += 1;
            }
            highest = highest.max(i32::from(fluid.info().amount));
        }
    }
    if sources >= 2 && kind.can_convert_to_source(level) {
        let below = level.block(pos.below());
        if is_solid(below) || kind.is_source_of(below.fluid_state()) {
            return fluid_state(kind.source(), 8, false);
        }
    }
    let above = level.block(pos.above());
    let above_fluid = above.fluid_state();
    if !above_fluid.is_empty()
        && kind.is_same(above_fluid)
        && can_pass_through_wall(Direction::Up, state, above)
    {
        return fluid_state(kind.flowing(), 8, true);
    }
    let amount = highest - kind.drop_off(level);
    if amount <= 0 {
        FluidState::EMPTY
    } else {
        fluid_state(kind.flowing(), amount, false)
    }
}

/// `FlowingFluid.tick(level, pos, blockState, fluidState)`.
pub fn tick(level: &mut Level, pos: BlockPos, state: BlockState, fluid: FluidState) {
    let Some(kind) = Kind::of(fluid.info().fluid) else {
        return;
    };
    let (mut state, mut fluid) = (state, fluid);
    if !fluid.info().source {
        let current = level.block(pos);
        let new = new_liquid(level, kind, pos, current);
        let delay = spread_delay(level, kind, pos, fluid, new);
        if new.is_empty() {
            fluid = new;
            state = BlockState::AIR;
            level.set_block_and_update(pos, state);
        } else if new != fluid {
            fluid = new;
            state = new.block_state();
            level.set_block_and_update(pos, state);
            level.schedule_fluid_tick(pos, new.info().fluid, delay);
        }
    }
    spread(level, kind, pos, state, fluid);
}

/// `getSpreadDelay`: lava rising in height waits four times as long three times in four.
fn spread_delay(level: &mut Level, kind: Kind, pos: BlockPos, old: FluidState, new: FluidState) -> i32 {
    let delay = tick_delay(level, kind.source());
    if kind == Kind::Lava
        && !old.is_empty()
        && !new.is_empty()
        && !old.info().falling
        && !new.info().falling
        && height(level, new, pos) > height(level, old, pos)
        && vanillite_base::random::RandomSource::next_int_bounded(level.random(), 4) != 0
    {
        delay * 4
    } else {
        delay
    }
}

/// `spread(level, pos, state, fluidState)`.
fn spread(level: &mut Level, kind: Kind, pos: BlockPos, state: BlockState, fluid: FluidState) {
    if fluid.is_empty() {
        return;
    }
    let below_pos = pos.below();
    let below = level.block(below_pos);
    if can_maybe_pass_through(kind, state, Direction::Down, below) {
        let new = new_liquid(level, kind, below_pos, below);
        let new_type = new.info().fluid;
        if can_be_replaced_with(level, below.fluid_state(), below_pos, new_type, Direction::Down)
            && can_hold_specific_fluid(below, new_type)
        {
            spread_to(level, kind, below_pos, below, Direction::Down, new);
            if source_neighbour_count(level, kind, pos) >= 3 {
                spread_to_sides(level, kind, pos, fluid, state);
            }
            return;
        }
    }
    if fluid.info().source || !is_water_hole(kind, state, below) {
        spread_to_sides(level, kind, pos, fluid, state);
    }
}

/// `sourceNeighborCount(level, pos)`.
fn source_neighbour_count(level: &Level, kind: Kind, pos: BlockPos) -> usize {
    HORIZONTAL
        .into_iter()
        .filter(|&d| kind.is_source_of(level.fluid(pos.relative(d))))
        .count()
}

/// `spreadToSides(level, pos, fluidState, state)`.
fn spread_to_sides(level: &mut Level, kind: Kind, pos: BlockPos, fluid: FluidState, state: BlockState) {
    let mut amount = i32::from(fluid.info().amount) - kind.drop_off(level);
    if fluid.info().falling {
        amount = 7;
    }
    if amount <= 0 {
        return;
    }
    for (direction, new) in get_spread(level, kind, pos, state) {
        let neighbour = pos.relative(direction);
        let neighbour_state = level.block(neighbour);
        spread_to(level, kind, neighbour, neighbour_state, direction, new);
    }
}

/// `getSpread(level, pos, state)`: the directions towards the nearest holes with the fluid each
/// gets, in `Direction` order (the iteration order of Vanilla's `EnumMap`).
fn get_spread(level: &Level, kind: Kind, pos: BlockPos, state: BlockState) -> Vec<(Direction, FluidState)> {
    let mut lowest = 1000;
    let mut result: Vec<(Direction, FluidState)> = Vec::with_capacity(4);
    let mut context: Option<SpreadContext> = None;
    for direction in HORIZONTAL {
        let test_pos = pos.relative(direction);
        let test = level.block(test_pos);
        if !can_maybe_pass_through(kind, state, direction, test) {
            continue;
        }
        let new = new_liquid(level, kind, test_pos, test);
        if !can_hold_specific_fluid(test, new.info().fluid) {
            continue;
        }
        let context = context.get_or_insert_with(SpreadContext::new);
        let distance = if context.is_hole(level, kind, test_pos) {
            0
        } else {
            slope_distance(level, kind, test_pos, 1, direction.opposite(), test, context)
        };
        if distance < lowest {
            result.clear();
        }
        if distance <= lowest {
            if can_be_replaced_with(level, test.fluid_state(), test_pos, new.info().fluid, direction) {
                result.push((direction, new));
            }
            lowest = distance;
        }
    }
    result.sort_by_key(|&(direction, _)| direction as u8);
    result
}

/// `getSlopeDistance(level, pos, pass, from, state, context)`.
fn slope_distance(
    level: &Level,
    kind: Kind,
    pos: BlockPos,
    pass: i32,
    from: Direction,
    state: BlockState,
    context: &mut SpreadContext,
) -> i32 {
    let mut lowest = 1000;
    for direction in HORIZONTAL {
        if direction == from {
            continue;
        }
        let test_pos = pos.relative(direction);
        let test = context.block(level, test_pos);
        // `canPassThrough(level, getFlowing(), ...)`.
        if !(can_maybe_pass_through(kind, state, direction, test)
            && can_hold_specific_fluid(test, kind.flowing()))
        {
            continue;
        }
        if context.is_hole(level, kind, test_pos) {
            return pass;
        }
        if pass < kind.slope_find_distance(level) {
            let distance = slope_distance(level, kind, test_pos, pass + 1, direction.opposite(), test, context);
            lowest = lowest.min(distance);
        }
    }
    lowest
}

/// `spreadTo(level, pos, state, direction, target)` with `LavaFluid`'s stone rule.
fn spread_to(level: &mut Level, kind: Kind, pos: BlockPos, state: BlockState, direction: Direction, target: FluidState) {
    if kind == Kind::Lava && direction == Direction::Down && Kind::Water.is_same(level.fluid(pos)) {
        if is_liquid(state) {
            level.set_block_and_update(pos, ids::STONE.default_state());
        }
        level.level_event(None, LAVA_FIZZ, pos, 0);
        return;
    }
    match container(state.block()) {
        Container::Waterlogged => {
            place_liquid(level, pos, state, target);
        }
        Container::Closed => {}
        Container::None => {
            if !state.is_air() {
                // `beforeDestroyingBlock`.
                match kind {
                    Kind::Water => drops::drop_resources(level, pos, state, None),
                    Kind::Lava => level.level_event(None, LAVA_FIZZ, pos, 0),
                }
            }
            level.set_block_and_update(pos, target.block_state());
        }
    }
}

/// `SimpleWaterloggedBlock.placeLiquid`: a water source waterlogs the block.
pub fn place_liquid(level: &mut Level, pos: BlockPos, state: BlockState, fluid: FluidState) -> bool {
    if container(state.block()) != Container::Waterlogged
        || waterlogged(state)
        || fluid.info().fluid != WATER
    {
        return false;
    }
    let Some(waterlogged) = state.with("waterlogged", "true") else {
        return false;
    };
    level.set_block_and_update(pos, waterlogged);
    let delay = tick_delay(level, WATER);
    level.schedule_fluid_tick(pos, WATER, delay);
    true
}

/// `fluidState.is(FluidTags.WATER)`.
pub fn is_water(fluid: FluidState) -> bool {
    Kind::Water.is_same(fluid)
}

/// The `waterlogged` property.
pub fn waterlogged(state: BlockState) -> bool {
    state.value("waterlogged") == Some("true")
}

/// The random tick of fluids: only lava ticks randomly, to set fire; fire is not simulated yet,
/// so lava leaves its surroundings alone.
pub fn random_tick(_level: &mut Level, _pos: BlockPos, _fluid: FluidState) {}

/// `LiquidBlock.onPlace`.
pub fn liquid_on_place(level: &mut Level, state: BlockState, pos: BlockPos, _old: BlockState, _moved: bool) {
    schedule_if_spreading(level, state, pos);
}

/// `LiquidBlock.neighborChanged`.
pub fn liquid_neighbor_changed(level: &mut Level, state: BlockState, pos: BlockPos, _source: BlockId, _moved: bool) {
    schedule_if_spreading(level, state, pos);
}

fn schedule_if_spreading(level: &mut Level, state: BlockState, pos: BlockPos) {
    if should_spread_liquid(level, state, pos) {
        let fluid = state.fluid_state().info().fluid;
        let delay = tick_delay(level, fluid);
        level.schedule_fluid_tick(pos, fluid, delay);
    }
}

/// `LiquidBlock.shouldSpreadLiquid`: lava touching water hardens.
fn should_spread_liquid(level: &mut Level, state: BlockState, pos: BlockPos) -> bool {
    if !Kind::Lava.is_same(state.fluid_state()) {
        return true;
    }
    let over_soul_soil = level.block(pos.below()).is(ids::SOUL_SOIL);
    for direction in POSSIBLE_FLOW_DIRECTIONS {
        let neighbour = pos.relative(direction.opposite());
        if Kind::Water.is_same(level.fluid(neighbour)) {
            let block = if level.fluid(pos).info().source {
                ids::OBSIDIAN
            } else {
                ids::COBBLESTONE
            };
            level.set_block_and_update(pos, block.default_state());
            level.level_event(None, LAVA_FIZZ, pos, 0);
            return false;
        }
        if over_soul_soil && level.block(neighbour).is(ids::BLUE_ICE) {
            level.set_block_and_update(pos, ids::BASALT.default_state());
            level.level_event(None, LAVA_FIZZ, pos, 0);
            return false;
        }
    }
    true
}

/// `SpongeBlock.onPlace`.
pub fn sponge_on_place(level: &mut Level, state: BlockState, pos: BlockPos, old: BlockState, _moved: bool) {
    if !old.is(state.block()) {
        try_absorb_water(level, pos);
    }
}

/// `SpongeBlock.neighborChanged`.
pub fn sponge_neighbor_changed(level: &mut Level, _state: BlockState, pos: BlockPos, _source: BlockId, _moved: bool) {
    try_absorb_water(level, pos);
}

/// `SpongeBlock.tryAbsorbWater`: removes water within 6 blocks (at most 64 blocks of it) and
/// turns wet.
fn try_absorb_water(level: &mut Level, start: BlockPos) {
    // `BlockPos.breadthFirstTraversal(start, 6, 65, ...)`.
    let mut nodes = std::collections::VecDeque::from([(start, 0)]);
    let mut visited = vanillite_base::hash::FxHashSet::default();
    let mut count = 0;
    while let Some((pos, depth)) = nodes.pop_front() {
        if !visited.insert(pos) {
            continue;
        }
        if !absorb(level, start, pos) {
            continue;
        }
        count += 1;
        if count >= 65 {
            break;
        }
        if depth < 6 {
            for direction in Direction::ALL {
                nodes.push_back((pos.relative(direction), depth + 1));
            }
        }
    }
    if count > 1 {
        level.set_block(start, ids::WET_SPONGE.default_state(), update::CLIENTS);
    }
}

/// The node processor of the sponge's search: whether to accept `pos` (and remove its water).
fn absorb(level: &mut Level, start: BlockPos, pos: BlockPos) -> bool {
    if pos == start {
        return true;
    }
    let state = level.block(pos);
    if !Kind::Water.is_same(state.fluid_state()) {
        return false;
    }
    // `BucketPickup.pickupBlock`.
    if is_liquid(state) {
        if state.value("level") == Some("0") {
            level.set_block(pos, BlockState::AIR, update::ALL_IMMEDIATE);
            return true;
        }
        level.set_block_and_update(pos, BlockState::AIR);
        return true;
    }
    if container(state.block()) == Container::Waterlogged && waterlogged(state) {
        if let Some(dry) = state.with("waterlogged", "false") {
            level.set_block_and_update(pos, dry);
            if !level.can_survive(dry, pos) {
                level.destroy_block(pos, true, None);
            }
        }
        return true;
    }
    if state.is(ids::BUBBLE_COLUMN) {
        level.set_block(pos, BlockState::AIR, update::ALL_IMMEDIATE);
        return true;
    }
    if [ids::KELP, ids::KELP_PLANT, ids::SEAGRASS, ids::TALL_SEAGRASS].contains(&state.block()) {
        drops::drop_resources(level, pos, state, None);
        level.set_block_and_update(pos, BlockState::AIR);
        return true;
    }
    false
}

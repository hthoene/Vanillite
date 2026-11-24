//! Building blocks and containers: stairs, slabs, fences, walls, panes, doors, trapdoors,
//! pillars, carpets, beds, chests, leaves, lanterns, …
//!
//! Redstone power (`Level.hasNeighborSignal`) comes from [`super::signal`], which reads block
//! states only.

use vanillite_base::{BlockPos, Direction};
use vanillite_data::block::{BlockId, BlockKind, BlockState, ids};

use super::signal;
use super::tags;
use super::util::{self, DirExt, HORIZONTAL, StateExt, Support, above, below};
use super::{BlockReader, PlaceContext};

/// `getStateForPlacement` of the kinds of this group (no survival check).
pub(super) fn state_for_placement(
    block: BlockId,
    ctx: &PlaceContext,
    level: &dyn BlockReader,
) -> Option<BlockState> {
    use BlockKind as K;
    let default = block.default_state();
    let pos = ctx.pos;
    let state = match block.info().kind {
        K::StairBlock | K::WeatheringCopperStairBlock => stairs(default, ctx, level),
        K::SlabBlock | K::WeatheringCopperSlabBlock => slab(block, ctx, level),
        K::FenceBlock => fence(block, ctx, level),
        K::FenceGateBlock => fence_gate(default, ctx, level),
        K::WallBlock => wall(default, ctx, level),
        K::IronBarsBlock | K::WeatheringCopperBarsBlock | K::StainedGlassPaneBlock => {
            bars(default, ctx, level)
        }
        K::DoorBlock | K::WeatheringCopperDoorBlock => return door(default, ctx, level),
        K::TrapDoorBlock | K::WeatheringCopperTrapDoorBlock => trapdoor(default, ctx, level),
        K::RotatedPillarBlock | K::InfestedRotatedPillarBlock | K::HayBlock => {
            default.set_axis("axis", ctx.face.axis())
        }
        K::ChainBlock | K::WeatheringCopperChainBlock => {
            util::waterlogged(default.set_axis("axis", ctx.face.axis()), ctx, level)
        }
        K::CreakingHeartBlock => {
            creaking_heart(default.set_axis("axis", ctx.face.axis()), pos, level)
        }
        K::GlazedTerracottaBlock => {
            default.set_dir("facing", ctx.horizontal_direction().opposite())
        }
        K::LadderBlock => return ladder(block, ctx, level),
        K::MossyCarpetBlock => mossy_carpet_updated(default, level, pos, true),
        K::ScaffoldingBlock => {
            let distance = scaffolding_distance(level, pos);
            let bottom = distance > 0 && !level.state(below(pos)).is(block);
            util::waterlogged(default, ctx, level)
                .set_int("distance", distance)
                .set_bool("bottom", bottom)
        }
        K::SnowLayerBlock => {
            let existing = level.state(pos);
            if existing.is(block) {
                let layers = existing.get_int("layers");
                existing.set_int("layers", (layers + 1).min(8))
            } else {
                default
            }
        }
        K::BedBlock | K::StrawBedBlock => {
            let facing = ctx.horizontal_direction();
            let head = pos.relative(facing);
            // The world border check is the caller's.
            if !super::can_be_replaced(level.state(head), ctx) {
                return None;
            }
            default.set_dir("facing", facing)
        }
        K::ChestBlock | K::TrappedChestBlock => chest(default, ctx, level, |s| s.is(block)),
        K::CopperChestBlock | K::WeatheringCopperChestBlock => {
            let state = chest(default, ctx, level, |s| {
                tags::COPPER_CHESTS.has(s) && s.has_prop("type")
            });
            least_oxidized_copper_chest(state, pos, level)
        }
        K::EnderChestBlock | K::CopperGolemStatueBlock | K::WeatheringCopperGolemStatueBlock => {
            util::waterlogged(
                default.set_dir("facing", ctx.horizontal_direction().opposite()),
                ctx,
                level,
            )
        }
        K::ShelfBlock => util::waterlogged(
            default
                .set_dir("facing", ctx.horizontal_direction().opposite())
                .set_bool("powered", signal::has_neighbor_signal(level, pos)),
            ctx,
            level,
        ),
        K::WaterloggedTransparentBlock
        | K::WeatheringCopperGrateBlock
        | K::BarrierBlock
        | K::HeavyCoreBlock
        | K::MangroveRootsBlock => util::waterlogged(default, ctx, level),
        K::ConduitBlock => {
            let here = level.state(pos);
            default.set_bool(
                "waterlogged",
                util::fluid_is_water(here) && util::fluid_amount(here) == 8,
            )
        }
        K::DecoratedPotBlock => util::waterlogged(
            default
                .set_dir("facing", ctx.horizontal_direction())
                .set_bool("cracked", false),
            ctx,
            level,
        ),
        K::LanternBlock | K::WeatheringLanternBlock => {
            for direction in ctx.nearest_looking_directions() {
                if direction.is_vertical() {
                    let state = default.set_bool("hanging", direction == Direction::Up);
                    if lantern_survives(state, pos, level) {
                        return Some(util::waterlogged(state, ctx, level));
                    }
                }
            }
            return None;
        }
        K::HugeMushroomBlock => {
            let mut state = default;
            for direction in [
                Direction::Down,
                Direction::Up,
                Direction::North,
                Direction::East,
                Direction::South,
                Direction::West,
            ] {
                let connected = !level.state(pos.relative(direction)).is(block);
                state = state.set_bool(direction.name(), connected);
            }
            state
        }
        K::LeavesBlock
        | K::MangroveLeavesBlock
        | K::TintedParticleLeavesBlock
        | K::UntintedParticleLeavesBlock => {
            let state = util::waterlogged(default.set_bool("persistent", true), ctx, level);
            leaves_distance(state, pos, level)
        }
        K::ChorusPlantBlock => chorus_plant_connections(default, pos, level),
        K::AnvilBlock => default.set_dir("facing", ctx.horizontal_direction().clockwise()),
        K::GrassBlock | K::MyceliumBlock | K::SnowyBlock => {
            default.set_bool("snowy", tags::SNOW.has(level.state(above(pos))))
        }
        K::FarmlandBlock => {
            if farmland_survives(pos, level) {
                default
            } else {
                ids::DIRT.default_state()
            }
        }
        K::PathBlock => {
            if path_survives(pos, level) {
                default
            } else {
                // `Block.pushEntitiesUp` moves entities; the state is dirt.
                ids::DIRT.default_state()
            }
        }
        K::ConcretePowderBlock => {
            if concrete_should_solidify(level, pos, level.state(pos)) {
                concrete_of(block).default_state()
            } else {
                default
            }
        }
        // LightBlock, CarpetBlock, WoolCarpetBlock, CakeBlock, FlowerPotBlock: the default state.
        _ => default,
    };
    Some(state)
}

/// `canSurvive` of the kinds of this group.
pub(super) fn can_survive(state: BlockState, pos: BlockPos, level: &dyn BlockReader) -> bool {
    use BlockKind as K;
    match state.kind() {
        K::DoorBlock | K::WeatheringCopperDoorBlock => {
            let below_pos = below(pos);
            let below_state = level.state(below_pos);
            if state.get("half") == Some("lower") {
                below_state.sturdy(Direction::Up, Support::Full)
            } else {
                below_state.is(state.block())
            }
        }
        K::LadderBlock => match state.get_dir("facing") {
            Some(facing) => util::sturdy(
                level,
                pos.relative(facing.opposite()),
                facing,
                Support::Full,
            ),
            None => true,
        },
        K::CarpetBlock | K::WoolCarpetBlock => !level.state(below(pos)).is_air(),
        K::MossyCarpetBlock => {
            let below_state = level.state(below(pos));
            if state.get_bool("bottom") {
                !below_state.is_air()
            } else {
                below_state.is(state.block()) && below_state.get_bool("bottom")
            }
        }
        K::ScaffoldingBlock => scaffolding_distance(level, pos) < 7,
        K::SnowLayerBlock => {
            let below_pos = below(pos);
            let below_state = level.state(below_pos);
            if tags::CANNOT_SUPPORT_SNOW_LAYER.has(below_state) {
                false
            } else if tags::SUPPORT_OVERRIDE_SNOW_LAYER.has(below_state) {
                true
            } else {
                util::face_full(below_state.collision_shape(), Direction::Up)
                    || (below_state.is(state.block()) && below_state.get_int("layers") == 8)
            }
        }
        K::LanternBlock | K::WeatheringLanternBlock => lantern_survives(state, pos, level),
        K::ChorusPlantBlock => chorus_plant_survives(state.block(), pos, level),
        K::CakeBlock => level.state(below(pos)).is_solid(),
        K::FarmlandBlock => farmland_survives(pos, level),
        K::PathBlock => path_survives(pos, level),
        _ => true,
    }
}

/// `canBeReplaced(state, context)` of the kinds of this group.
pub(super) fn can_be_replaced(state: BlockState, ctx: &PlaceContext) -> bool {
    use BlockKind as K;
    match state.kind() {
        K::SlabBlock | K::WeatheringCopperSlabBlock => {
            let kind = state.get("type");
            if kind == Some("double") || !util::holds_item_of(ctx, state.block()) {
                false
            } else if ctx.replace_clicked {
                let upper = ctx.hit.y - f64::from(ctx.pos.y) > 0.5;
                let face = ctx.face;
                if kind == Some("bottom") {
                    face == Direction::Up || (upper && face.is_horizontal())
                } else {
                    face == Direction::Down || (!upper && face.is_horizontal())
                }
            } else {
                true
            }
        }
        K::ScaffoldingBlock => util::holds_item_of(ctx, state.block()),
        K::SnowLayerBlock => {
            let layers = state.get_int("layers");
            if !util::holds_item_of(ctx, state.block()) || layers >= 8 {
                layers == 1
            } else if ctx.replace_clicked {
                ctx.face == Direction::Up
            } else {
                true
            }
        }
        _ => util::default_can_be_replaced(state, ctx),
    }
}

/// Blocks placed with `state` (`setPlacedBy` and item specifics).
///
/// The mossy carpet's topper (`MossyCarpetBlock.setPlacedBy`) needs the level and a random
/// source; see [`mossy_carpet_topper`].
pub(super) fn placed_extra(
    state: BlockState,
    pos: BlockPos,
    _ctx: &PlaceContext,
    _level: &dyn BlockReader,
    out: &mut Vec<(BlockPos, BlockState)>,
) {
    use BlockKind as K;
    match state.kind() {
        K::DoorBlock | K::WeatheringCopperDoorBlock => {
            out.push((above(pos), state.set("half", "upper")));
        }
        K::BedBlock | K::StrawBedBlock => {
            if let Some(facing) = state.get_dir("facing") {
                out.push((pos.relative(facing), state.set("part", "head")));
            }
        }
        _ => {}
    }
}

/// Blocks changed when a player breaks `state` (`playerWillDestroy`).
pub(super) fn destroy_extra(
    state: BlockState,
    pos: BlockPos,
    level: &dyn BlockReader,
    creative: bool,
    out: &mut Vec<(BlockPos, BlockState)>,
) {
    use BlockKind as K;
    match state.kind() {
        // `DoorBlock.playerWillDestroy` → `DoublePlantBlock.preventDropFromBottomPart`. Vanilla
        // also does this in survival without the correct tool; `creative` stands for both.
        K::DoorBlock | K::WeatheringCopperDoorBlock => {
            if creative && state.get("half") == Some("upper") {
                let bottom_pos = below(pos);
                let bottom = level.state(bottom_pos);
                if bottom.is(state.block()) && bottom.get("half") == Some("lower") {
                    let replacement = if util::fluid_is_water(bottom) {
                        super::water()
                    } else {
                        BlockState::AIR
                    };
                    out.push((bottom_pos, replacement));
                }
            }
        }
        // `AbstractBedBlock.playerWillDestroy`: breaking the foot in creative removes the head.
        K::BedBlock | K::StrawBedBlock => {
            if creative
                && state.get("part") == Some("foot")
                && let Some(facing) = state.get_dir("facing")
            {
                let head_pos = pos.relative(facing);
                let head = level.state(head_pos);
                if head.is(state.block()) && head.get("part") == Some("head") {
                    out.push((head_pos, BlockState::AIR));
                }
            }
        }
        _ => {}
    }
}

/// `ScaffoldingBlock.getDistance(level, pos)`.
pub(super) fn scaffolding_distance(level: &dyn BlockReader, pos: BlockPos) -> i32 {
    let below_pos = below(pos);
    let below_state = level.state(below_pos);
    let mut distance = 7;
    if below_state.is(ids::SCAFFOLDING) {
        distance = below_state.get_int("distance");
    } else if below_state.sturdy(Direction::Up, Support::Full) {
        return 0;
    }
    for direction in HORIZONTAL {
        let state = level.state(pos.relative(direction));
        if state.is(ids::SCAFFOLDING) {
            distance = distance.min(state.get_int("distance") + 1);
            if distance == 1 {
                break;
            }
        }
    }
    distance
}

/// Whether the click is in the upper half of the placement block (`getClickLocation().y -
/// pos.getY() > 0.5`).
#[inline]
fn upper_half_click(ctx: &PlaceContext) -> bool {
    ctx.hit.y - f64::from(ctx.pos.y) > 0.5
}

// --- Stairs and slabs ----------------------------------------------------------------------

fn is_stairs(state: BlockState) -> bool {
    matches!(
        state.kind(),
        BlockKind::StairBlock | BlockKind::WeatheringCopperStairBlock
    )
}

/// `StairBlock.getStateForPlacement`.
fn stairs(default: BlockState, ctx: &PlaceContext, level: &dyn BlockReader) -> BlockState {
    let face = ctx.face;
    let bottom = face != Direction::Down && (face == Direction::Up || !upper_half_click(ctx));
    let state = default
        .set_dir("facing", ctx.horizontal_direction())
        .set("half", if bottom { "bottom" } else { "top" });
    let state = util::waterlogged(state, ctx, level);
    state.set("shape", stairs_shape(state, level, ctx.pos))
}

/// `StairBlock.getStairsShape`.
fn stairs_shape(state: BlockState, level: &dyn BlockReader, pos: BlockPos) -> &'static str {
    let Some(facing) = state.get_dir("facing") else {
        return "straight";
    };
    let half = state.get("half");
    let behind = level.state(pos.relative(facing));
    if is_stairs(behind)
        && half == behind.get("half")
        && let Some(behind_facing) = behind.get_dir("facing")
        && behind_facing.axis() != facing.axis()
        && stairs_can_take_shape(state, level, pos, behind_facing.opposite())
    {
        return if behind_facing == facing.counter_clockwise() {
            "outer_left"
        } else {
            "outer_right"
        };
    }
    let front = level.state(pos.relative(facing.opposite()));
    if is_stairs(front)
        && half == front.get("half")
        && let Some(front_facing) = front.get_dir("facing")
        && front_facing.axis() != facing.axis()
        && stairs_can_take_shape(state, level, pos, front_facing)
    {
        return if front_facing == facing.counter_clockwise() {
            "inner_left"
        } else {
            "inner_right"
        };
    }
    "straight"
}

/// `StairBlock.canTakeShape`.
fn stairs_can_take_shape(
    state: BlockState,
    level: &dyn BlockReader,
    pos: BlockPos,
    neighbour: Direction,
) -> bool {
    let other = level.state(pos.relative(neighbour));
    !is_stairs(other)
        || other.get("facing") != state.get("facing")
        || other.get("half") != state.get("half")
}

/// `SlabBlock.getStateForPlacement`.
fn slab(block: BlockId, ctx: &PlaceContext, level: &dyn BlockReader) -> BlockState {
    let existing = level.state(ctx.pos);
    if existing.is(block) {
        return existing
            .set("type", "double")
            .set_bool("waterlogged", false);
    }
    let state = util::waterlogged(block.default_state().set("type", "bottom"), ctx, level);
    let face = ctx.face;
    if face != Direction::Down && (face == Direction::Up || !upper_half_click(ctx)) {
        state
    } else {
        state.set("type", "top")
    }
}

// --- Fences, walls, panes ------------------------------------------------------------------

fn is_bars(state: BlockState) -> bool {
    matches!(
        state.kind(),
        BlockKind::IronBarsBlock
            | BlockKind::WeatheringCopperBarsBlock
            | BlockKind::StainedGlassPaneBlock
    )
}

/// `FenceGateBlock.connectsToDirection`.
fn fence_gate_connects(state: BlockState, direction: Direction) -> bool {
    state.kind() == BlockKind::FenceGateBlock
        && state
            .get_dir("facing")
            .is_some_and(|f| f.axis() == direction.clockwise().axis())
}

/// The neighbour towards `direction` and whether its face back to `pos` is sturdy.
fn neighbour(level: &dyn BlockReader, pos: BlockPos, direction: Direction) -> (BlockState, bool) {
    let state = level.state(pos.relative(direction));
    let sturdy = state.sturdy(direction.opposite(), Support::Full);
    (state, sturdy)
}

/// `FenceBlock.getStateForPlacement`.
fn fence(block: BlockId, ctx: &PlaceContext, level: &dyn BlockReader) -> BlockState {
    let wooden = tags::WOODEN_FENCES.contains(block);
    let connects = |state: BlockState, face_solid: bool, direction: Direction| {
        let same_fence = tags::FENCES.has(state) && tags::WOODEN_FENCES.has(state) == wooden;
        let gate = fence_gate_connects(state, direction);
        (!util::is_exception_for_connection(state) && face_solid) || same_fence || gate
    };
    let mut state = block.default_state();
    for direction in [
        Direction::North,
        Direction::East,
        Direction::South,
        Direction::West,
    ] {
        let (other, sturdy) = neighbour(level, ctx.pos, direction);
        state = state.set_bool(
            direction.name(),
            connects(other, sturdy, direction.opposite()),
        );
    }
    util::waterlogged(state, ctx, level)
}

/// `IronBarsBlock.getStateForPlacement` (also panes and copper bars).
fn bars(default: BlockState, ctx: &PlaceContext, level: &dyn BlockReader) -> BlockState {
    let mut state = default;
    for direction in [
        Direction::North,
        Direction::South,
        Direction::West,
        Direction::East,
    ] {
        let (other, sturdy) = neighbour(level, ctx.pos, direction);
        let attaches = (!util::is_exception_for_connection(other) && sturdy)
            || is_bars(other)
            || tags::WALLS.has(other);
        state = state.set_bool(direction.name(), attaches);
    }
    util::waterlogged(state, ctx, level)
}

/// `FenceGateBlock.getStateForPlacement`.
fn fence_gate(default: BlockState, ctx: &PlaceContext, level: &dyn BlockReader) -> BlockState {
    let pos = ctx.pos;
    let direction = ctx.horizontal_direction();
    let is_wall = |d: Direction| tags::WALLS.has(level.state(pos.relative(d)));
    let in_wall = match direction.axis() {
        util::Axis::Z => is_wall(Direction::West) || is_wall(Direction::East),
        util::Axis::X => is_wall(Direction::North) || is_wall(Direction::South),
        util::Axis::Y => false,
    };
    let open = signal::has_neighbor_signal(level, pos);
    default
        .set_dir("facing", direction)
        .set_bool("open", open)
        .set_bool("powered", open)
        .set_bool("in_wall", in_wall)
}

/// `WallBlock.TEST_SHAPES_WALL` on the face below the block above, as `[min_x, min_z, max_x,
/// max_z]`.
fn wall_test_rect(direction: Direction) -> [f64; 4] {
    const A: f64 = 7.0 / 16.0;
    const B: f64 = 9.0 / 16.0;
    match direction {
        Direction::North => [A, 0.0, B, B],
        Direction::South => [A, A, B, 1.0],
        Direction::East => [A, A, 1.0, B],
        _ => [0.0, A, B, B],
    }
}

/// `WallBlock.TEST_SHAPE_POST`.
const WALL_POST_RECT: [f64; 4] = [7.0 / 16.0, 7.0 / 16.0, 9.0 / 16.0, 9.0 / 16.0];

/// `WallBlock.getStateForPlacement`.
fn wall(default: BlockState, ctx: &PlaceContext, level: &dyn BlockReader) -> BlockState {
    let pos = ctx.pos;
    let connects = |state: BlockState, face_solid: bool, direction: Direction| {
        tags::WALLS.has(state)
            || (!util::is_exception_for_connection(state) && face_solid)
            || is_bars(state)
            || fence_gate_connects(state, direction)
    };
    let mut connected = [false; 4];
    for (i, direction) in HORIZONTAL.into_iter().enumerate() {
        let (other, sturdy) = neighbour(level, pos, direction);
        connected[i] = connects(other, sturdy, direction.opposite());
    }
    let state = util::waterlogged(default, ctx, level);
    wall_update_shape(state, level.state(above(pos)), connected)
}

/// `WallBlock.updateShape(level, state, topPos, topNeighbour, north, east, south, west)`;
/// `connected` is in [`HORIZONTAL`] order (north, east, south, west).
fn wall_update_shape(state: BlockState, top: BlockState, connected: [bool; 4]) -> BlockState {
    let above_shape = top.collision_shape();
    let covered = |rect: [f64; 4]| util::face_covers(above_shape, Direction::Down, rect);
    let mut state = state;
    for (i, direction) in HORIZONTAL.into_iter().enumerate() {
        let side = if !connected[i] {
            "none"
        } else if covered(wall_test_rect(direction)) {
            "tall"
        } else {
            "low"
        };
        state = state.set(direction.name(), side);
    }
    // `shouldRaisePost`.
    let raise = if top.kind() == BlockKind::WallBlock && top.get_bool("up") {
        true
    } else {
        let side = |d: Direction| state.get(d.name()).unwrap_or("none");
        let (north, south, east, west) = (
            side(Direction::North),
            side(Direction::South),
            side(Direction::East),
            side(Direction::West),
        );
        let (n_none, s_none, e_none, w_none) = (
            north == "none",
            south == "none",
            east == "none",
            west == "none",
        );
        let has_corner =
            (n_none && s_none && w_none && e_none) || n_none != s_none || w_none != e_none;
        if has_corner {
            true
        } else {
            let high_wall =
                (north == "tall" && south == "tall") || (east == "tall" && west == "tall");
            !high_wall && (tags::WALL_POST_OVERRIDE.has(top) || covered(WALL_POST_RECT))
        }
    };
    state.set_bool("up", raise)
}

// --- Doors, trapdoors, ladders -------------------------------------------------------------

fn is_door(state: BlockState) -> bool {
    matches!(
        state.kind(),
        BlockKind::DoorBlock | BlockKind::WeatheringCopperDoorBlock
    )
}

/// `DoorBlock.getStateForPlacement`.
fn door(default: BlockState, ctx: &PlaceContext, level: &dyn BlockReader) -> Option<BlockState> {
    let pos = ctx.pos;
    // `pos.getY() < level.getMaxY()`, Vanilla's max y being the highest block.
    if pos.y >= level.max_y() - 1 || !super::can_be_replaced(level.state(above(pos)), ctx) {
        return None;
    }
    let powered =
        signal::has_neighbor_signal(level, pos) || signal::has_neighbor_signal(level, above(pos));
    Some(
        default
            .set_dir("facing", ctx.horizontal_direction())
            .set("hinge", door_hinge(ctx, level))
            .set_bool("powered", powered)
            .set_bool("open", powered)
            .set("half", "lower"),
    )
}

/// `DoorBlock.getHinge`.
fn door_hinge(ctx: &PlaceContext, level: &dyn BlockReader) -> &'static str {
    let pos = ctx.pos;
    let place_direction = ctx.horizontal_direction();
    let above_pos = above(pos);
    let left = place_direction.counter_clockwise();
    let left_state = level.state(pos.relative(left));
    let left_above = level.state(above_pos.relative(left));
    let right = place_direction.clockwise();
    let right_state = level.state(pos.relative(right));
    let right_above = level.state(above_pos.relative(right));
    let full = |s: BlockState| i32::from(s.collision_full_block());
    let balance = -full(left_state) - full(left_above) + full(right_state) + full(right_above);
    let door_left = is_door(left_state) && left_state.get("half") == Some("lower");
    let door_right = is_door(right_state) && right_state.get("half") == Some("lower");
    if (door_left && !door_right) || balance > 0 {
        "right"
    } else if (door_right && !door_left) || balance < 0 {
        "left"
    } else {
        let step_x = place_direction.step_x();
        let step_z = place_direction.step_z();
        let click_x = ctx.hit.x - f64::from(pos.x);
        let click_z = ctx.hit.z - f64::from(pos.z);
        // Click coordinates are finite, so `!(a < b)` is `a >= b`.
        let left_hinge = (step_x >= 0 || click_z >= 0.5)
            && (step_x <= 0 || click_z <= 0.5)
            && (step_z >= 0 || click_x <= 0.5)
            && (step_z <= 0 || click_x >= 0.5);
        if left_hinge { "left" } else { "right" }
    }
}

/// `TrapDoorBlock.getStateForPlacement`.
fn trapdoor(default: BlockState, ctx: &PlaceContext, level: &dyn BlockReader) -> BlockState {
    let face = ctx.face;
    let state = if !ctx.replace_clicked && face.is_horizontal() {
        default.set_dir("facing", face).set(
            "half",
            if upper_half_click(ctx) {
                "top"
            } else {
                "bottom"
            },
        )
    } else {
        default
            .set_dir("facing", ctx.horizontal_direction().opposite())
            .set(
                "half",
                if face == Direction::Up {
                    "bottom"
                } else {
                    "top"
                },
            )
    };
    let state = if signal::has_neighbor_signal(level, ctx.pos) {
        state.set_bool("open", true).set_bool("powered", true)
    } else {
        state
    };
    util::waterlogged(state, ctx, level)
}

/// `LadderBlock.getStateForPlacement` (survival checked per direction).
fn ladder(block: BlockId, ctx: &PlaceContext, level: &dyn BlockReader) -> Option<BlockState> {
    if !ctx.replace_clicked {
        let behind = level.state(ctx.pos.relative(ctx.face.opposite()));
        if behind.is(block) && behind.get_dir("facing") == Some(ctx.face) {
            return None;
        }
    }
    let default = block.default_state();
    for direction in ctx.nearest_looking_directions() {
        if direction.is_horizontal() {
            let state = default.set_dir("facing", direction.opposite());
            if can_survive(state, ctx.pos, level) {
                return Some(util::waterlogged(state, ctx, level));
            }
        }
    }
    None
}

// --- Chests --------------------------------------------------------------------------------

/// `ChestBlock.candidatePartnerFacing`.
fn chest_partner_facing(
    level: &dyn BlockReader,
    pos: BlockPos,
    direction: Direction,
    can_connect: &dyn Fn(BlockState) -> bool,
) -> Option<Direction> {
    let state = level.state(pos.relative(direction));
    if can_connect(state) && state.get("type") == Some("single") {
        state.get_dir("facing")
    } else {
        None
    }
}

/// `ChestBlock.getStateForPlacement` with the block's `chestCanConnectTo`.
fn chest(
    default: BlockState,
    ctx: &PlaceContext,
    level: &dyn BlockReader,
    can_connect: impl Fn(BlockState) -> bool,
) -> BlockState {
    let pos = ctx.pos;
    let mut kind = "single";
    let mut facing = ctx.horizontal_direction().opposite();
    let secondary = ctx.sneaking;
    let face = ctx.face;
    if face.is_horizontal()
        && secondary
        && let Some(neighbour_facing) =
            chest_partner_facing(level, pos, face.opposite(), &can_connect)
        && neighbour_facing.axis() != face.axis()
    {
        facing = neighbour_facing;
        kind = if neighbour_facing.counter_clockwise() == face.opposite() {
            "right"
        } else {
            "left"
        };
    }
    if kind == "single" && !secondary {
        kind = if chest_partner_facing(level, pos, facing.clockwise(), &can_connect) == Some(facing)
        {
            "left"
        } else if chest_partner_facing(level, pos, facing.counter_clockwise(), &can_connect)
            == Some(facing)
        {
            "right"
        } else {
            "single"
        };
    }
    util::waterlogged(
        default.set_dir("facing", facing).set("type", kind),
        ctx,
        level,
    )
}

/// `ChestBlock.getConnectedDirection`.
fn chest_connected_direction(state: BlockState) -> Option<Direction> {
    let facing = state.get_dir("facing")?;
    Some(if state.get("type") == Some("left") {
        facing.clockwise()
    } else {
        facing.counter_clockwise()
    })
}

/// Copper chests by weathering: `(waxed, ordinal)` of a copper chest block.
fn copper_chest_info(block: BlockId) -> Option<(bool, u16)> {
    let offset = block.0.checked_sub(ids::COPPER_CHEST.0)?;
    (offset < 8).then_some((offset >= 4, offset % 4))
}

/// `CopperChestBlock.getLeastOxidizedChestOfConnectedBlocks`.
fn least_oxidized_copper_chest(
    state: BlockState,
    pos: BlockPos,
    level: &dyn BlockReader,
) -> BlockState {
    if state.get("type") == Some("single") {
        return state;
    }
    let Some(direction) = chest_connected_direction(state) else {
        return state;
    };
    let connected = level.state(pos.relative(direction));
    let (Some((waxed, ordinal)), Some((other_waxed, other_ordinal))) = (
        copper_chest_info(state.block()),
        copper_chest_info(connected.block()),
    ) else {
        return state;
    };
    let base = if waxed && other_waxed {
        ids::WAXED_COPPER_CHEST.0
    } else {
        ids::COPPER_CHEST.0
    };
    let block = BlockId(base + ordinal.min(other_ordinal));
    with_properties_of(block, state)
}

/// `Block.withPropertiesOf(state)`.
fn with_properties_of(block: BlockId, state: BlockState) -> BlockState {
    state
        .properties()
        .fold(block.default_state(), |s, (k, v)| s.set(k, v))
}

// --- Lanterns, leaves, chorus plants, creaking hearts ----------------------------------------

/// `LanternBlock.canSurvive`.
fn lantern_survives(state: BlockState, pos: BlockPos, level: &dyn BlockReader) -> bool {
    // `getConnectedDirection(state).getOpposite()`.
    let direction = if state.get_bool("hanging") {
        Direction::Up
    } else {
        Direction::Down
    };
    util::can_support_center(level, pos.relative(direction), direction.opposite())
}

fn is_leaves(state: BlockState) -> bool {
    matches!(
        state.kind(),
        BlockKind::LeavesBlock
            | BlockKind::MangroveLeavesBlock
            | BlockKind::TintedParticleLeavesBlock
            | BlockKind::UntintedParticleLeavesBlock
    )
}

/// `LeavesBlock.updateDistance`.
fn leaves_distance(state: BlockState, pos: BlockPos, level: &dyn BlockReader) -> BlockState {
    let mut distance = 7;
    for direction in Direction::ALL {
        let neighbour = level.state(pos.relative(direction));
        let at = if tags::PREVENTS_NEARBY_LEAF_DECAY.has(neighbour) {
            0
        } else if is_leaves(neighbour) {
            neighbour.get_int("distance")
        } else {
            7
        };
        distance = distance.min(at + 1);
        if distance == 1 {
            break;
        }
    }
    state.set_int("distance", distance)
}

/// `ChorusPlantBlock.getStateWithConnections`.
fn chorus_plant_connections(
    state: BlockState,
    pos: BlockPos,
    level: &dyn BlockReader,
) -> BlockState {
    let block = state.block();
    let plant = |s: BlockState| s.is(block) || s.is(ids::CHORUS_FLOWER);
    let down = level.state(below(pos));
    let mut state = state.set_bool("down", plant(down) || tags::SUPPORTS_CHORUS_PLANT.has(down));
    for direction in [
        Direction::Up,
        Direction::North,
        Direction::East,
        Direction::South,
        Direction::West,
    ] {
        state = state.set_bool(
            direction.name(),
            plant(level.state(pos.relative(direction))),
        );
    }
    state
}

/// `ChorusPlantBlock.canSurvive`.
fn chorus_plant_survives(block: BlockId, pos: BlockPos, level: &dyn BlockReader) -> bool {
    let below_state = level.state(below(pos));
    let above_or_below = !level.state(above(pos)).is_air() && !below_state.is_air();
    for direction in HORIZONTAL {
        let neighbour_pos = pos.relative(direction);
        if level.state(neighbour_pos).is(block) {
            if above_or_below {
                return false;
            }
            let under = level.state(below(neighbour_pos));
            if under.is(block) || tags::SUPPORTS_CHORUS_PLANT.has(under) {
                return true;
            }
        }
    }
    below_state.is(block) || tags::SUPPORTS_CHORUS_PLANT.has(below_state)
}

/// `CreakingHeartBlock.updateState`. Whether the heart is awake depends on the time of day
/// (`EnvironmentAttributes.CREAKING_ACTIVE`), which is not known here: a heart between logs is
/// placed dormant.
fn creaking_heart(state: BlockState, pos: BlockPos, level: &dyn BlockReader) -> BlockState {
    let Some(axis) = state.get_axis("axis") else {
        return state;
    };
    let has_logs = [util::from_axis(axis, false), util::from_axis(axis, true)]
        .into_iter()
        .all(|d| {
            let n = level.state(pos.relative(d));
            tags::PALE_OAK_LOGS.has(n) && n.get_axis("axis") == Some(axis)
        });
    if has_logs && state.get("creaking_heart_state") == Some("uprooted") {
        state.set("creaking_heart_state", "dormant")
    } else {
        state
    }
}

// --- Ground blocks -------------------------------------------------------------------------

/// `FarmlandBlock.canSurvive`.
fn farmland_survives(pos: BlockPos, level: &dyn BlockReader) -> bool {
    let above_state = level.state(above(pos));
    !above_state.is_solid() || tags::MAINTAINS_FARMLAND.has(above_state)
}

/// `PathBlock.canSurvive`.
fn path_survives(pos: BlockPos, level: &dyn BlockReader) -> bool {
    let above_state = level.state(above(pos));
    !above_state.is_solid() || above_state.kind() == BlockKind::FenceGateBlock
}

/// The concrete of a concrete powder (registered 48 blocks before the powders).
fn concrete_of(powder: BlockId) -> BlockId {
    BlockId(powder.0 - (ids::WHITE_CONCRETE_POWDER.0 - ids::WHITE_CONCRETE.0))
}

/// `ConcretePowderBlock.shouldSolidify`.
fn concrete_should_solidify(level: &dyn BlockReader, pos: BlockPos, replaced: BlockState) -> bool {
    if util::fluid_is_water(replaced) {
        return true;
    }
    // `touchesLiquid`, including its quirk of reading the previously tested position.
    let mut test_pos = pos;
    for direction in Direction::ALL {
        let state = level.state(test_pos);
        if direction != Direction::Down || util::fluid_is_water(state) {
            test_pos = pos.relative(direction);
            let state = level.state(test_pos);
            if util::fluid_is_water(state) && !state.sturdy(direction.opposite(), Support::Full) {
                return true;
            }
        }
    }
    false
}

// --- Mossy carpet --------------------------------------------------------------------------

/// `MultifaceBlock.canAttachTo(level, pos, direction)`.
fn multiface_can_attach_to(level: &dyn BlockReader, pos: BlockPos, direction: Direction) -> bool {
    let neighbour = level.state(pos.relative(direction));
    let face = direction.opposite();
    util::face_full(neighbour.support_shape(), face)
        || util::face_full(neighbour.collision_shape(), face)
}

/// `MossyCarpetBlock.getUpdatedState`.
fn mossy_carpet_updated(
    state: BlockState,
    level: &dyn BlockReader,
    pos: BlockPos,
    create_sides: bool,
) -> BlockState {
    let base = state.get_bool("bottom");
    let create_sides = create_sides || base;
    let mut state = state;
    let mut above_state = None;
    let mut below_state = None;
    for direction in HORIZONTAL {
        let prop = direction.name();
        let mut side = if multiface_can_attach_to(level, pos, direction) {
            if create_sides {
                "low"
            } else {
                state.get(prop).unwrap_or("none")
            }
        } else {
            "none"
        };
        if side == "low" {
            let above_state = *above_state.get_or_insert_with(|| level.state(above(pos)));
            if above_state.is(ids::PALE_MOSS_CARPET)
                && above_state.get(prop) != Some("none")
                && !above_state.get_bool("bottom")
            {
                side = "tall";
            }
            if !base {
                let below_state = *below_state.get_or_insert_with(|| level.state(below(pos)));
                if below_state.is(ids::PALE_MOSS_CARPET) && below_state.get(prop) == Some("none") {
                    side = "none";
                }
            }
        }
        state = state.set(prop, side);
    }
    state
}

/// A level with one block replaced.
struct Overlay<'a> {
    level: &'a dyn BlockReader,
    pos: BlockPos,
    state: BlockState,
}

impl BlockReader for Overlay<'_> {
    fn state(&self, pos: BlockPos) -> BlockState {
        if pos == self.pos {
            self.state
        } else {
            self.level.state(pos)
        }
    }

    fn min_y(&self) -> i32 {
        self.level.min_y()
    }

    fn max_y(&self) -> i32 {
        self.level.max_y()
    }
}

/// `MossyCarpetBlock.createTopperWithSideChance` for a carpet `state` just placed at `pos` (the
/// level read with `state` at `pos`): the carpet to set above, if any. `keep_side` is
/// `setPlacedBy`'s `random.nextBoolean()` per side that could hang down.
pub(super) fn mossy_carpet_topper(
    state: BlockState,
    pos: BlockPos,
    level: &dyn BlockReader,
    keep_side: &mut dyn FnMut() -> bool,
) -> Option<BlockState> {
    let level = Overlay { level, pos, state };
    let above_pos = above(pos);
    let previous = level.state(above_pos);
    let is_carpet = previous.is(ids::PALE_MOSS_CARPET);
    if (is_carpet && previous.get_bool("bottom")) || (!is_carpet && !previous.replaceable()) {
        return None;
    }
    let base = ids::PALE_MOSS_CARPET
        .default_state()
        .set_bool("bottom", false);
    let mut topper = mossy_carpet_updated(base, &level, above_pos, true);
    for direction in HORIZONTAL {
        let prop = direction.name();
        if topper.get(prop) != Some("none") && !keep_side() {
            topper = topper.set(prop, "none");
        }
    }
    let has_faces = topper.get_bool("bottom")
        || HORIZONTAL
            .into_iter()
            .any(|d| topper.get(d.name()) != Some("none"));
    (has_faces && topper != previous).then_some(topper)
}

#[cfg(test)]
mod tests {
    use vanillite_base::{BlockPos, Direction, Vec3};

    use super::super::tests::*;
    use super::super::{can_survive, destroy_extra, placed_extra, state_for_placement};
    use super::*;

    const GROUND: BlockPos = BlockPos::new(0, 64, 0);
    /// A block on the floor, for clicks on side faces.
    const PILLAR: BlockPos = BlockPos::new(0, 65, 0);

    fn stone_floor() -> TestLevel {
        let mut level = TestLevel::new();
        for x in -3..=3 {
            for z in -3..=3 {
                level.set_block(BlockPos::new(x, 64, z), "stone");
            }
        }
        level
    }

    #[test]
    fn stairs_take_facing_and_half_from_the_click() {
        let mut level = stone_floor();
        // On top of a block, looking north (yaw 180): bottom half facing north.
        let c = ctx(&level, "oak_stairs", GROUND, Direction::Up, 180.0, 30.0);
        assert_state(
            state_for_placement(&c, &level),
            "oak_stairs",
            &[
                ("facing", "north"),
                ("half", "bottom"),
                ("shape", "straight"),
            ],
        );
        // On the east side of a block, clicking its upper part, looking west (yaw 90).
        level.set_block(PILLAR, "stone");
        let hit = Vec3::new(1.0, 65.8, 0.5);
        let c = ctx_at(
            &level,
            "oak_stairs",
            PILLAR,
            Direction::East,
            hit,
            90.0,
            0.0,
            false,
        );
        assert_eq!(c.pos, BlockPos::new(1, 65, 0));
        assert_state(
            state_for_placement(&c, &level),
            "oak_stairs",
            &[("facing", "west"), ("half", "top")],
        );
        // The lower part of the side gives a bottom stair; the underside a top one.
        let hit = Vec3::new(1.0, 65.2, 0.5);
        let c = ctx_at(
            &level,
            "oak_stairs",
            PILLAR,
            Direction::East,
            hit,
            90.0,
            0.0,
            false,
        );
        assert_state(
            state_for_placement(&c, &level),
            "oak_stairs",
            &[("half", "bottom")],
        );
        let c = ctx(&level, "oak_stairs", GROUND, Direction::Down, 0.0, -30.0);
        assert_state(
            state_for_placement(&c, &level),
            "oak_stairs",
            &[("facing", "south"), ("half", "top")],
        );
    }

    #[test]
    fn stairs_form_corners_with_neighbours() {
        let mut level = stone_floor();
        // A stair facing east north of the new one, the new one facing north: outer corner.
        let east_stair = block("oak_stairs")
            .default_state()
            .set_dir("facing", Direction::East);
        level.set(BlockPos::new(0, 65, -1), east_stair);
        let c = ctx(&level, "oak_stairs", GROUND, Direction::Up, 180.0, 30.0);
        assert_state(
            state_for_placement(&c, &level),
            "oak_stairs",
            &[("facing", "north"), ("shape", "outer_right")],
        );
    }

    #[test]
    fn slabs_go_top_bottom_and_double() {
        let mut level = stone_floor();
        let c = ctx(&level, "oak_slab", GROUND, Direction::Up, 0.0, 30.0);
        assert_state(
            state_for_placement(&c, &level),
            "oak_slab",
            &[("type", "bottom")],
        );
        let side = BlockPos::new(0, 65, 2);
        level.set_block(side, "stone");
        let hit = Vec3::new(1.0, 65.7, 2.5);
        let c = ctx_at(
            &level,
            "oak_slab",
            side,
            Direction::East,
            hit,
            90.0,
            0.0,
            false,
        );
        assert_state(
            state_for_placement(&c, &level),
            "oak_slab",
            &[("type", "top")],
        );
        let c = ctx(&level, "oak_slab", GROUND, Direction::Down, 0.0, -30.0);
        assert_state(
            state_for_placement(&c, &level),
            "oak_slab",
            &[("type", "top")],
        );

        // Clicking the top of a bottom slab replaces it with a double slab.
        let slab_pos = BlockPos::new(0, 65, 0);
        level.set(
            slab_pos,
            block("oak_slab").default_state().set("type", "bottom"),
        );
        let hit = Vec3::new(0.5, 65.5, 0.5);
        let c = ctx_at(
            &level,
            "oak_slab",
            slab_pos,
            Direction::Up,
            hit,
            0.0,
            60.0,
            false,
        );
        assert!(c.replace_clicked);
        assert_eq!(c.pos, slab_pos);
        assert_state(
            state_for_placement(&c, &level),
            "oak_slab",
            &[("type", "double"), ("waterlogged", "false")],
        );
        // Another slab type does not merge: it goes above.
        let c = ctx_at(
            &level,
            "stone_slab",
            slab_pos,
            Direction::Up,
            hit,
            0.0,
            60.0,
            false,
        );
        assert!(!c.replace_clicked);
        assert_eq!(c.pos, BlockPos::new(0, 66, 0));
        // Clicking a side of the slab on its lower part does not merge.
        let hit = Vec3::new(1.0, 65.2, 0.5);
        let c = ctx_at(
            &level,
            "oak_slab",
            slab_pos,
            Direction::East,
            hit,
            90.0,
            0.0,
            false,
        );
        assert!(!c.replace_clicked);
    }

    #[test]
    fn slabs_in_water_are_waterlogged() {
        let mut level = stone_floor();
        level.set_block(BlockPos::new(0, 65, 0), "water");
        let c = ctx(&level, "oak_slab", GROUND, Direction::Up, 0.0, 30.0);
        assert!(c.replace_clicked || c.pos == BlockPos::new(0, 65, 0));
        assert_state(
            state_for_placement(&c, &level),
            "oak_slab",
            &[("type", "bottom"), ("waterlogged", "true")],
        );
    }

    #[test]
    fn logs_follow_the_clicked_face() {
        let mut level = stone_floor();
        level.set_block(PILLAR, "stone");
        for (face, axis) in [
            (Direction::Up, "y"),
            (Direction::East, "x"),
            (Direction::North, "z"),
        ] {
            let c = ctx(&level, "oak_log", PILLAR, face, 0.0, 0.0);
            assert_state(
                state_for_placement(&c, &level),
                "oak_log",
                &[("axis", axis)],
            );
        }
    }

    #[test]
    fn doors_face_the_player_and_add_the_upper_half() {
        let mut level = stone_floor();
        // Looking south, the hinge follows the click: east part left, west part right.
        let hit = Vec3::new(0.8, 65.0, 0.5);
        let c = ctx_at(
            &level,
            "oak_door",
            GROUND,
            Direction::Up,
            hit,
            0.0,
            30.0,
            false,
        );
        let state = state_for_placement(&c, &level);
        assert_state(
            state,
            "oak_door",
            &[("facing", "south"), ("half", "lower"), ("hinge", "left")],
        );
        let hit = Vec3::new(0.2, 65.0, 0.5);
        let c = ctx_at(
            &level,
            "oak_door",
            GROUND,
            Direction::Up,
            hit,
            0.0,
            30.0,
            false,
        );
        assert_state(
            state_for_placement(&c, &level),
            "oak_door",
            &[("hinge", "right")],
        );

        // A solid block on the left side (east, looking south) puts the hinge left.
        level.set_block(BlockPos::new(1, 65, 0), "stone");
        level.set_block(BlockPos::new(1, 66, 0), "stone");
        let c = ctx_at(
            &level,
            "oak_door",
            GROUND,
            Direction::Up,
            hit,
            0.0,
            30.0,
            false,
        );
        let state = state_for_placement(&c, &level);
        assert_state(state, "oak_door", &[("hinge", "left")]);

        let mut extra = Vec::new();
        let state = state.unwrap_or(BlockState::AIR);
        placed_extra(state, c.pos, &c, &level, &mut extra);
        assert_eq!(extra.len(), 1);
        assert_eq!(extra[0].0, BlockPos::new(0, 66, 0));
        assert_state(
            Some(extra[0].1),
            "oak_door",
            &[("half", "upper"), ("facing", "south"), ("hinge", "left")],
        );

        // Breaking the upper half in creative removes the lower one.
        level.set(c.pos, state);
        level.set(extra[0].0, extra[0].1);
        let mut removed = Vec::new();
        destroy_extra(extra[0].1, extra[0].0, &level, true, &mut removed);
        assert_eq!(removed, vec![(c.pos, BlockState::AIR)]);
        removed.clear();
        destroy_extra(extra[0].1, extra[0].0, &level, false, &mut removed);
        assert!(removed.is_empty());

        // No door without a floor or room above.
        let c = ctx(
            &level,
            "oak_door",
            BlockPos::new(1, 66, 0),
            Direction::Up,
            0.0,
            30.0,
        );
        level.set_block(BlockPos::new(1, 68, 0), "stone");
        assert_eq!(state_for_placement(&c, &level), None);
        assert!(!can_survive(state, BlockPos::new(5, 80, 5), &level));
    }

    #[test]
    fn beds_add_the_head() {
        let mut level = stone_floor();
        let c = ctx(&level, "red_bed", GROUND, Direction::Up, 270.0, 30.0);
        let state = state_for_placement(&c, &level);
        assert_state(state, "red_bed", &[("facing", "east"), ("part", "foot")]);
        let state = state.unwrap_or(BlockState::AIR);
        let mut extra = Vec::new();
        placed_extra(state, c.pos, &c, &level, &mut extra);
        assert_eq!(extra.len(), 1);
        assert_eq!(extra[0].0, BlockPos::new(1, 65, 0));
        assert_state(
            Some(extra[0].1),
            "red_bed",
            &[("part", "head"), ("facing", "east")],
        );

        level.set(c.pos, state);
        level.set(extra[0].0, extra[0].1);
        let mut removed = Vec::new();
        destroy_extra(state, c.pos, &level, true, &mut removed);
        assert_eq!(removed, vec![(BlockPos::new(1, 65, 0), BlockState::AIR)]);

        // A blocked head position fails.
        level.set_block(BlockPos::new(1, 65, 0), "stone");
        level.set(c.pos, BlockState::AIR);
        assert_eq!(state_for_placement(&c, &level), None);
    }

    #[test]
    fn fences_connect_to_sturdy_neighbours() {
        let mut level = stone_floor();
        level.set_block(BlockPos::new(1, 65, 0), "stone");
        level.set_block(BlockPos::new(0, 65, -1), "oak_fence");
        level.set_block(BlockPos::new(-1, 65, 0), "nether_brick_fence");
        let c = ctx(&level, "oak_fence", GROUND, Direction::Up, 0.0, 30.0);
        assert_state(
            state_for_placement(&c, &level),
            "oak_fence",
            &[
                ("east", "true"),
                ("north", "true"),
                ("west", "false"),
                ("south", "false"),
            ],
        );
    }

    #[test]
    fn walls_and_panes_connect() {
        let mut level = stone_floor();
        level.set_block(BlockPos::new(1, 65, 0), "stone");
        level.set_block(BlockPos::new(-1, 65, 0), "stone");
        let c = ctx(&level, "cobblestone_wall", GROUND, Direction::Up, 0.0, 30.0);
        assert_state(
            state_for_placement(&c, &level),
            "cobblestone_wall",
            &[
                ("east", "low"),
                ("west", "low"),
                ("north", "none"),
                ("up", "false"),
            ],
        );
        // A full block above makes the sides tall.
        level.set_block(BlockPos::new(0, 66, 0), "stone");
        assert_state(
            state_for_placement(&c, &level),
            "cobblestone_wall",
            &[("east", "tall"), ("west", "tall"), ("up", "false")],
        );
        let c = ctx(&level, "glass_pane", GROUND, Direction::Up, 0.0, 30.0);
        assert_state(
            state_for_placement(&c, &level),
            "glass_pane",
            &[("east", "true"), ("west", "true"), ("north", "false")],
        );
    }

    #[test]
    fn chests_pair_with_a_neighbour() {
        let mut level = stone_floor();
        // Looking north: the chest faces south; a south-facing chest to the west (its clockwise
        // side) pairs as left.
        let neighbour = block("chest")
            .default_state()
            .set_dir("facing", Direction::South);
        level.set(BlockPos::new(-1, 65, 0), neighbour);
        let c = ctx(&level, "chest", GROUND, Direction::Up, 180.0, 30.0);
        assert_state(
            state_for_placement(&c, &level),
            "chest",
            &[("facing", "south"), ("type", "left")],
        );
    }

    #[test]
    fn lanterns_hang_or_stand() {
        let mut level = stone_floor();
        let c = ctx(&level, "lantern", GROUND, Direction::Up, 0.0, 60.0);
        assert_state(
            state_for_placement(&c, &level),
            "lantern",
            &[("hanging", "false")],
        );
        level.set_block(BlockPos::new(0, 70, 0), "stone");
        let c = ctx(
            &level,
            "lantern",
            BlockPos::new(0, 70, 0),
            Direction::Down,
            0.0,
            -60.0,
        );
        assert_state(
            state_for_placement(&c, &level),
            "lantern",
            &[("hanging", "true")],
        );
    }

    #[test]
    fn snow_layers_stack() {
        let mut level = stone_floor();
        let pos = BlockPos::new(0, 65, 0);
        level.set(pos, block("snow").default_state().set_int("layers", 3));
        let c = ctx(&level, "snow", pos, Direction::Up, 0.0, 60.0);
        assert!(c.replace_clicked);
        assert_state(state_for_placement(&c, &level), "snow", &[("layers", "4")]);
        assert!(!can_survive(
            block("snow").default_state(),
            pos,
            &TestLevel::new()
        ));
    }

    #[test]
    fn concrete_of_matches_names() {
        for colour in ["white", "orange", "black", "light_gray"] {
            assert_eq!(
                concrete_of(block(&format!("{colour}_concrete_powder"))),
                block(&format!("{colour}_concrete"))
            );
        }
    }

    #[test]
    fn leaves_are_persistent_with_a_distance() {
        let mut level = stone_floor();
        level.set_block(BlockPos::new(1, 65, 0), "oak_log");
        let c = ctx(&level, "oak_leaves", GROUND, Direction::Up, 0.0, 60.0);
        assert_state(
            state_for_placement(&c, &level),
            "oak_leaves",
            &[("persistent", "true"), ("distance", "1")],
        );
    }

    #[test]
    fn scaffolding_needs_support() {
        let level = stone_floor();
        assert_eq!(scaffolding_distance(&level, BlockPos::new(0, 65, 0)), 0);
        assert_eq!(scaffolding_distance(&level, BlockPos::new(0, 70, 0)), 7);
    }
}

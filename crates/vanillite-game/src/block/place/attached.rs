//! Attached and hanging blocks: torches, signs, banners, heads, buttons, levers, vines,
//! multiface blocks, candles, …

use vanillite_base::{BlockPos, Direction};
use vanillite_data::block::{BlockId, BlockKind, BlockState, Shape, ids};

use super::tags;
use super::util::{self, DirExt, HORIZONTAL, StateExt, Support};
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
    match block.info().kind {
        // `WallTorchBlock`; `RedstoneWallTorchBlock` takes the facing of `Blocks.WALL_TORCH`'s
        // placement, which has the same survival rule.
        K::WallTorchBlock | K::RedstoneWallTorchBlock => first_horizontal(ctx, |direction| {
            let state = default.set_dir("facing", direction.opposite());
            wall_torch_survives(level, pos, direction.opposite()).then_some(state)
        }),
        K::StandingSignBlock => Some(util::waterlogged(
            default.set_int("rotation", util::rotation_segment(ctx.rotation() + 180.0)),
            ctx,
            level,
        )),
        K::WallSignBlock => first_horizontal(ctx, |direction| {
            let state = default.set_dir("facing", direction.opposite());
            can_survive(state, pos, level).then(|| util::waterlogged(state, ctx, level))
        }),
        K::CeilingHangingSignBlock => Some(ceiling_hanging_sign_placement(default, ctx, level)),
        K::WallHangingSignBlock => {
            let mut state = default;
            for direction in ctx.nearest_looking_directions() {
                if direction.is_horizontal() && direction.axis() != ctx.face.axis() {
                    state = state.set_dir("facing", direction.opposite());
                    if can_survive(state, pos, level)
                        && wall_hanging_sign_can_place(state, pos, level)
                    {
                        return Some(util::waterlogged(state, ctx, level));
                    }
                }
            }
            None
        }
        K::BannerBlock => {
            Some(default.set_int("rotation", util::rotation_segment(ctx.rotation() + 180.0)))
        }
        K::WallBannerBlock => first_horizontal(ctx, |direction| {
            let state = default.set_dir("facing", direction.opposite());
            can_survive(state, pos, level).then_some(state)
        }),
        // `AbstractSkullBlock` + `SkullBlock`.
        K::SkullBlock | K::WitherSkullBlock | K::PlayerHeadBlock => Some(
            default
                .set_bool("powered", super::signal::has_neighbor_signal(level, pos))
                .set_int("rotation", util::rotation_segment(ctx.rotation())),
        ),
        // `AbstractSkullBlock` + `WallSkullBlock`.
        K::WallSkullBlock
        | K::WitherWallSkullBlock
        | K::PlayerWallHeadBlock
        | K::PiglinWallSkullBlock => {
            let mut state =
                default.set_bool("powered", super::signal::has_neighbor_signal(level, pos));
            for direction in ctx.nearest_looking_directions() {
                if direction.is_horizontal() {
                    state = state.set_dir("facing", direction.opposite());
                    if !super::can_be_replaced(level.state(pos.relative(direction)), ctx) {
                        return Some(state);
                    }
                }
            }
            None
        }
        // `FaceAttachedHorizontalDirectionalBlock`.
        K::ButtonBlock | K::LeverBlock | K::GrindstoneBlock => {
            for direction in ctx.nearest_looking_directions() {
                let state = if direction.is_vertical() {
                    default
                        .set(
                            "face",
                            if direction == Direction::Up {
                                "ceiling"
                            } else {
                                "floor"
                            },
                        )
                        .set_dir("facing", ctx.horizontal_direction())
                } else {
                    default
                        .set("face", "wall")
                        .set_dir("facing", direction.opposite())
                };
                if can_survive(state, pos, level) {
                    return Some(state);
                }
            }
            None
        }
        K::BellBlock => bell_placement(default, ctx, level),
        K::EndRodBlock => {
            let face = ctx.face;
            let behind = level.state(pos.relative(face.opposite()));
            let facing = if behind.is(block) && behind.get_dir("facing") == Some(face) {
                face.opposite()
            } else {
                face
            };
            Some(default.set_dir("facing", facing))
        }
        K::LightningRodBlock | K::WeatheringLightningRodBlock | K::AmethystClusterBlock => {
            Some(util::waterlogged(default, ctx, level).set_dir("facing", ctx.face))
        }
        K::CocoaBlock => first_horizontal(ctx, |direction| {
            let state = default.set_dir("facing", direction);
            can_survive(state, pos, level).then_some(state)
        }),
        K::TripWireHookBlock => {
            let default = default
                .set_bool("powered", false)
                .set_bool("attached", false);
            first_horizontal(ctx, |direction| {
                let state = default.set_dir("facing", direction.opposite());
                can_survive(state, pos, level).then_some(state)
            })
        }
        K::TripWireBlock => {
            let mut state = default;
            for direction in HORIZONTAL {
                let connect = tripwire_connects_to(level.state(pos.relative(direction)), direction);
                state = state.set_bool(direction.name(), connect);
            }
            Some(state)
        }
        K::ShelfMushroomBlock => first_horizontal(ctx, |direction| {
            let state = default.set_dir("facing", direction.opposite());
            can_survive(state, pos, level).then_some(state)
        }),
        K::PointedDripstoneBlock | K::SulfurSpikeBlock => {
            speleothem_placement(block, default, ctx, level)
        }
        K::MultifaceBlock | K::GlowLichenBlock | K::SculkVeinBlock => {
            let old = level.state(pos);
            ctx.nearest_looking_directions()
                .into_iter()
                .find_map(|direction| multiface_state_for(block, old, level, pos, direction))
        }
        K::VineBlock => {
            let clicked = level.state(pos);
            let clicked_vine = clicked.is(block);
            let result = if clicked_vine { clicked } else { default };
            for direction in ctx.nearest_looking_directions() {
                if direction != Direction::Down {
                    let face = direction.name();
                    let occupied = clicked_vine && clicked.get_bool(face);
                    if !occupied && vine_can_support_at_face(block, level, pos, direction) {
                        return Some(result.set_bool(face, true));
                    }
                }
            }
            clicked_vine.then_some(result)
        }
        K::HangingRootsBlock => Some(util::waterlogged(default, ctx, level)),
        K::KelpBlock => {
            let fluid = level.state(pos);
            if util::fluid_is_water(fluid) && util::fluid_amount(fluid) == 8 {
                Some(growing_plant_placement(block, ctx, level))
            } else {
                None
            }
        }
        K::CaveVinesBlock
        | K::CaveVinesPlantBlock
        | K::WeepingVinesBlock
        | K::WeepingVinesPlantBlock
        | K::TwistingVinesBlock
        | K::TwistingVinesPlantBlock
        | K::KelpPlantBlock => Some(growing_plant_placement(block, ctx, level)),
        K::CandleBlock => {
            let existing = level.state(pos);
            if existing.is(block) {
                // `state.cycle(CANDLES)`.
                let next = existing.get_int("candles") % 4 + 1;
                Some(existing.set_int("candles", next))
            } else {
                Some(util::waterlogged(default, ctx, level))
            }
        }
        K::SeaPickleBlock => {
            let existing = level.state(pos);
            if existing.is(block) {
                Some(existing.set_int("pickles", (existing.get_int("pickles") + 1).min(4)))
            } else {
                Some(util::waterlogged(default, ctx, level))
            }
        }
        // `TorchBlock`, `RedstoneTorchBlock`, `SporeBlossomBlock`, `HangingMossBlock`,
        // `CandleCakeBlock`: `Block.getStateForPlacement`.
        _ => Some(default),
    }
}

/// `canSurvive` of the kinds of this group.
pub(super) fn can_survive(state: BlockState, pos: BlockPos, level: &dyn BlockReader) -> bool {
    use BlockKind as K;
    match state.kind() {
        // `BaseTorchBlock`.
        K::TorchBlock | K::RedstoneTorchBlock => {
            util::can_support_center(level, util::below(pos), Direction::Up)
        }
        K::WallTorchBlock | K::RedstoneWallTorchBlock => match state.get_dir("facing") {
            Some(facing) => wall_torch_survives(level, pos, facing),
            None => false,
        },
        K::StandingSignBlock | K::BannerBlock | K::CandleCakeBlock => {
            level.state(util::below(pos)).is_solid()
        }
        K::WallSignBlock | K::WallBannerBlock => match state.get_dir("facing") {
            Some(facing) => level.state(pos.relative(facing.opposite())).is_solid(),
            None => false,
        },
        K::CeilingHangingSignBlock => {
            util::sturdy(level, util::above(pos), Direction::Down, Support::Center)
        }
        K::ButtonBlock | K::LeverBlock => {
            can_attach(level, pos, face_connected_direction(state).opposite())
        }
        K::BellBlock => {
            let connection = bell_connected_direction(state).opposite();
            if connection == Direction::Up {
                util::can_support_center(level, util::above(pos), Direction::Down)
            } else {
                can_attach(level, pos, connection)
            }
        }
        K::AmethystClusterBlock | K::ShelfMushroomBlock => match state.get_dir("facing") {
            Some(facing) => util::sturdy(
                level,
                pos.relative(facing.opposite()),
                facing,
                Support::Full,
            ),
            None => false,
        },
        K::CocoaBlock => match state.get_dir("facing") {
            Some(facing) => tags::SUPPORTS_COCOA.has(level.state(pos.relative(facing))),
            None => false,
        },
        K::TripWireHookBlock => match state.get_dir("facing") {
            Some(facing) => {
                facing.is_horizontal()
                    && util::sturdy(
                        level,
                        pos.relative(facing.opposite()),
                        facing,
                        Support::Full,
                    )
            }
            None => false,
        },
        K::PointedDripstoneBlock | K::SulfurSpikeBlock => match state.get_dir("vertical_direction")
        {
            Some(tip) => speleothem_valid_placement(state.block(), level, pos, tip),
            None => false,
        },
        K::MultifaceBlock | K::GlowLichenBlock | K::SculkVeinBlock => {
            let mut has_face = false;
            for direction in Direction::ALL {
                if state.get_bool(direction.name()) {
                    if !multiface_can_attach_at(level, pos, direction) {
                        return false;
                    }
                    has_face = true;
                }
            }
            has_face
        }
        K::VineBlock => vine_count_faces(vine_updated_state(state, level, pos)) > 0,
        K::HangingRootsBlock => {
            util::sturdy(level, util::above(pos), Direction::Down, Support::Full)
        }
        K::SporeBlossomBlock => {
            util::can_support_center(level, util::above(pos), Direction::Down)
                && !util::is_water(level, pos)
        }
        K::HangingMossBlock => {
            let above = util::above(pos);
            let above_state = level.state(above);
            multiface_can_attach(Direction::Up, above_state) || above_state.is(state.block())
        }
        K::KelpBlock
        | K::KelpPlantBlock
        | K::CaveVinesBlock
        | K::CaveVinesPlantBlock
        | K::WeepingVinesBlock
        | K::WeepingVinesPlantBlock
        | K::TwistingVinesBlock
        | K::TwistingVinesPlantBlock => growing_plant_survives(state, pos, level),
        K::CandleBlock => util::can_support_center(level, util::below(pos), Direction::Up),
        K::SeaPickleBlock => {
            let below = level.state(util::below(pos));
            face_not_empty(below.collision_shape(), Direction::Up)
                || below.sturdy(Direction::Up, Support::Full)
        }
        // Skulls, hanging wall signs, grindstones, rods, tripwire: `true`.
        _ => true,
    }
}

/// `canBeReplaced(state, context)` of the kinds of this group.
pub(super) fn can_be_replaced(state: BlockState, ctx: &PlaceContext) -> bool {
    use BlockKind as K;
    let block = state.block();
    match state.kind() {
        K::MultifaceBlock | K::GlowLichenBlock | K::SculkVeinBlock => {
            !util::holds_item_of(ctx, block)
                || Direction::ALL
                    .iter()
                    .any(|direction| !state.get_bool(direction.name()))
        }
        // The clicked state is the state at the placement position, i.e. `state`.
        K::VineBlock => vine_count_faces(state) < 5,
        // `GrowingPlantBodyBlock`.
        K::KelpPlantBlock
        | K::CaveVinesPlantBlock
        | K::WeepingVinesPlantBlock
        | K::TwistingVinesPlantBlock => {
            let result = util::default_can_be_replaced(state, ctx);
            let head = growing_plant_head(block);
            !(result && util::holds_item_of(ctx, head)) && result
        }
        K::CandleBlock => {
            (!ctx.sneaking && util::holds_item_of(ctx, block) && state.get_int("candles") < 4)
                || util::default_can_be_replaced(state, ctx)
        }
        K::SeaPickleBlock => {
            (!ctx.sneaking && util::holds_item_of(ctx, block) && state.get_int("pickles") < 4)
                || util::default_can_be_replaced(state, ctx)
        }
        _ => util::default_can_be_replaced(state, ctx),
    }
}

/// Blocks placed with `state` (`setPlacedBy` and item specifics). None of this group's
/// `setPlacedBy` places blocks without reading the level (see [`tripwire_hook_placed`]).
pub(super) fn placed_extra(
    state: BlockState,
    pos: BlockPos,
    _ctx: &PlaceContext,
    level: &dyn BlockReader,
    out: &mut Vec<(BlockPos, BlockState)>,
) {
    if state.kind() == BlockKind::TripWireHookBlock {
        tripwire_hook_placed(state, pos, level, out);
    }
}

/// Blocks changed when a player breaks `state` (`playerWillDestroy`). Tripwire disarming with
/// shears needs the held item and is the caller's.
pub(super) fn destroy_extra(
    _state: BlockState,
    _pos: BlockPos,
    _level: &dyn BlockReader,
    _creative: bool,
    _out: &mut Vec<(BlockPos, BlockState)>,
) {
}

/// `WallHangingSignBlock.canPlace(state, level, pos)`.
pub(super) fn wall_hanging_sign_can_place(
    state: BlockState,
    pos: BlockPos,
    level: &dyn BlockReader,
) -> bool {
    let Some(facing) = state.get_dir("facing") else {
        return false;
    };
    let clockwise = facing.clockwise();
    let counter_clockwise = facing.counter_clockwise();
    let can_attach_to = |attach_pos: BlockPos, attach_face: Direction| {
        let attach = level.state(attach_pos);
        if tags::WALL_HANGING_SIGNS.has(attach) {
            attach.get_dir("facing").map(DirExt::axis) == Some(facing.axis())
        } else {
            attach.sturdy(attach_face, Support::Full)
        }
    };
    can_attach_to(pos.relative(clockwise), counter_clockwise)
        || can_attach_to(pos.relative(counter_clockwise), clockwise)
}

/// `TripWireHookBlock.setPlacedBy` (`calculateState(level, pos, state, false, false, -1, null)`):
/// connects the hook to a facing hook across tripwire. Needs the level, which
/// `placed_extra` does not get; the caller runs it after setting the hook.
fn tripwire_hook_placed(
    state: BlockState,
    pos: BlockPos,
    level: &dyn BlockReader,
    out: &mut Vec<(BlockPos, BlockState)>,
) {
    let Some(direction) = state.get_dir("facing") else {
        return;
    };
    let was_attached = state.get_bool("attached");
    let block = state.block();
    let mut attached = true;
    let mut powered = false;
    let mut receiver = 0;
    let mut wires: [Option<BlockState>; 42] = [None; 42];
    let mut test = pos;
    for (i, wire_slot) in wires.iter_mut().enumerate().skip(1) {
        test = test.relative(direction);
        let wire = level.state(test);
        if wire.is(ids::TRIPWIRE_HOOK) {
            if wire.get_dir("facing") == Some(direction.opposite()) {
                receiver = i;
            }
            break;
        }
        if !wire.is(ids::TRIPWIRE) {
            *wire_slot = None;
            attached = false;
        } else {
            let armed = !wire.get_bool("disarmed");
            powered |= armed && wire.get_bool("powered");
            *wire_slot = Some(wire);
        }
    }
    attached &= receiver > 1;
    powered &= attached;
    let new_state = block
        .default_state()
        .set_bool("attached", attached)
        .set_bool("powered", powered);
    let offset = |i: usize| {
        let n = direction.normal();
        let i = i as i32;
        pos.offset(n[0] * i, n[1] * i, n[2] * i)
    };
    if receiver > 0 {
        out.push((
            offset(receiver),
            new_state.set_dir("facing", direction.opposite()),
        ));
    }
    out.push((pos, new_state.set_dir("facing", direction)));
    if was_attached != attached {
        for (i, wire) in wires.iter().enumerate().take(receiver).skip(1) {
            if let Some(wire) = wire {
                let at = offset(i);
                let current = level.state(at);
                if current.is(ids::TRIPWIRE) || current.is(ids::TRIPWIRE_HOOK) {
                    out.push((at, wire.set_bool("attached", attached)));
                }
            }
        }
    }
}

/// The first of the nearest looking directions that is horizontal and gives a state.
fn first_horizontal(
    ctx: &PlaceContext,
    mut f: impl FnMut(Direction) -> Option<BlockState>,
) -> Option<BlockState> {
    ctx.nearest_looking_directions()
        .into_iter()
        .filter(|d| d.is_horizontal())
        .find_map(&mut f)
}

/// `WallTorchBlock.canSurvive(level, pos, facing)`.
fn wall_torch_survives(level: &dyn BlockReader, pos: BlockPos, facing: Direction) -> bool {
    util::sturdy(
        level,
        pos.relative(facing.opposite()),
        facing,
        Support::Full,
    )
}

/// `FaceAttachedHorizontalDirectionalBlock.canAttach(level, pos, direction)`.
fn can_attach(level: &dyn BlockReader, pos: BlockPos, direction: Direction) -> bool {
    util::sturdy(
        level,
        pos.relative(direction),
        direction.opposite(),
        Support::Full,
    )
}

/// `FaceAttachedHorizontalDirectionalBlock.getConnectedDirection`.
fn face_connected_direction(state: BlockState) -> Direction {
    match state.get("face") {
        Some("ceiling") => Direction::Down,
        Some("floor") => Direction::Up,
        _ => state.get_dir("facing").unwrap_or(Direction::North),
    }
}

/// `BellBlock.getConnectedDirection`.
fn bell_connected_direction(state: BlockState) -> Direction {
    match state.get("attachment") {
        Some("floor") => Direction::Up,
        Some("ceiling") => Direction::Down,
        _ => state
            .get_dir("facing")
            .unwrap_or(Direction::North)
            .opposite(),
    }
}

/// `BellBlock.getStateForPlacement`.
fn bell_placement(
    default: BlockState,
    ctx: &PlaceContext,
    level: &dyn BlockReader,
) -> Option<BlockState> {
    let face = ctx.face;
    let pos = ctx.pos;
    let axis = face.axis();
    if axis == util::Axis::Y {
        let state = default
            .set(
                "attachment",
                if face == Direction::Down {
                    "ceiling"
                } else {
                    "floor"
                },
            )
            .set_dir("facing", ctx.horizontal_direction());
        return can_survive(state, pos, level).then_some(state);
    }
    let sturdy_pair = |a: Direction, b: Direction| {
        util::sturdy(level, pos.relative(a), b, Support::Full)
            && util::sturdy(level, pos.relative(b), a, Support::Full)
    };
    let double = match axis {
        util::Axis::X => sturdy_pair(Direction::West, Direction::East),
        _ => sturdy_pair(Direction::North, Direction::South),
    };
    let state = default.set_dir("facing", face.opposite()).set(
        "attachment",
        if double { "double_wall" } else { "single_wall" },
    );
    if can_survive(state, pos, level) {
        return Some(state);
    }
    let below = util::sturdy(level, util::below(pos), Direction::Up, Support::Full);
    let state = state.set("attachment", if below { "floor" } else { "ceiling" });
    can_survive(state, pos, level).then_some(state)
}

/// `CeilingHangingSignBlock.getStateForPlacement`.
fn ceiling_hanging_sign_placement(
    default: BlockState,
    ctx: &PlaceContext,
    level: &dyn BlockReader,
) -> BlockState {
    let above = level.state(util::above(ctx.pos));
    let below_hanging_sign = tags::ALL_HANGING_SIGNS.has(above);
    let direction = util::from_y_rot(f64::from(ctx.rotation()));
    let mut attached_to_middle =
        !util::face_full(above.collision_shape(), Direction::Down) || ctx.sneaking;
    if below_hanging_sign && !ctx.sneaking {
        if let Some(above_facing) = above.get_dir("facing") {
            if above_facing.axis() == direction.axis() {
                attached_to_middle = false;
            }
        } else if above.has_prop("rotation") {
            let above_direction = match above.get_int("rotation") {
                0 => Some(Direction::North),
                4 => Some(Direction::East),
                8 => Some(Direction::South),
                12 => Some(Direction::West),
                _ => None,
            };
            if above_direction.is_some_and(|d| d.axis() == direction.axis()) {
                attached_to_middle = false;
            }
        }
    }
    let rotation = if attached_to_middle {
        util::rotation_segment(ctx.rotation() + 180.0)
    } else {
        util::rotation_segment_dir(direction.opposite())
    };
    util::waterlogged(
        default
            .set_bool("attached", attached_to_middle)
            .set_int("rotation", rotation),
        ctx,
        level,
    )
}

/// `TripWireBlock.shouldConnectTo`.
fn tripwire_connects_to(state: BlockState, direction: Direction) -> bool {
    if state.is(ids::TRIPWIRE_HOOK) {
        state.get_dir("facing") == Some(direction.opposite())
    } else {
        state.is(ids::TRIPWIRE)
    }
}

/// `SpeleothemBlock.isSpeleothemWithDirection`.
fn speleothem_with_direction(state: BlockState, tip: Direction) -> bool {
    tags::SPELEOTHEMS.has(state) && state.get_dir("vertical_direction") == Some(tip)
}

/// `SpeleothemBlock.isValidSpeleothemPlacement`.
fn speleothem_valid_placement(
    block: BlockId,
    level: &dyn BlockReader,
    pos: BlockPos,
    tip: Direction,
) -> bool {
    let behind_pos = pos.relative(tip.opposite());
    let behind = level.state(behind_pos);
    behind.sturdy(tip, Support::Full)
        || (speleothem_with_direction(behind, tip) && behind.is(block))
}

/// `SpeleothemBlock.getStateForPlacement`.
fn speleothem_placement(
    block: BlockId,
    default: BlockState,
    ctx: &PlaceContext,
    level: &dyn BlockReader,
) -> Option<BlockState> {
    let pos = ctx.pos;
    let default_tip = ctx.nearest_looking_vertical_direction().opposite();
    let tip = if speleothem_valid_placement(block, level, pos, default_tip) {
        default_tip
    } else if speleothem_valid_placement(block, level, pos, default_tip.opposite()) {
        default_tip.opposite()
    } else {
        return None;
    };
    let merge_opposing_tips = !ctx.sneaking;
    let base = tip.opposite();
    let in_front = level.state(pos.relative(tip));
    let thickness = if speleothem_with_direction(in_front, base) && in_front.is(block) {
        if !merge_opposing_tips && in_front.get("thickness") != Some("tip_merge") {
            "tip"
        } else {
            "tip_merge"
        }
    } else if !speleothem_with_direction(in_front, tip) {
        "tip"
    } else {
        match in_front.get("thickness") {
            Some("tip") | Some("tip_merge") => "frustum",
            _ => {
                let behind = level.state(pos.relative(base));
                if speleothem_with_direction(behind, tip) {
                    "middle"
                } else {
                    "base"
                }
            }
        }
    };
    Some(util::waterlogged(
        default
            .set_dir("vertical_direction", tip)
            .set("thickness", thickness),
        ctx,
        level,
    ))
}

/// Whether `shape` has a box touching the face towards `direction`
/// (`!shape.getFaceShape(direction).isEmpty()`).
fn face_not_empty(shape: Shape, direction: Direction) -> bool {
    const EPS: f64 = 1.0e-7;
    let (min, max) = match direction.axis() {
        util::Axis::X => (0, 3),
        util::Axis::Y => (1, 4),
        util::Axis::Z => (2, 5),
    };
    shape.iter().any(|b| {
        if direction.is_positive() {
            b[max] >= 1.0 - EPS
        } else {
            b[min] <= EPS
        }
    })
}

/// `MultifaceBlock.canAttachTo(level, directionTowardsNeighbour, neighbourPos, neighbourState)`.
fn multiface_can_attach(direction: Direction, neighbour: BlockState) -> bool {
    let face = direction.opposite();
    util::face_full(neighbour.support_shape(), face)
        || util::face_full(neighbour.collision_shape(), face)
}

/// `MultifaceBlock.canAttachTo(level, pos, directionTowardsNeighbour)`.
fn multiface_can_attach_at(level: &dyn BlockReader, pos: BlockPos, direction: Direction) -> bool {
    multiface_can_attach(direction, level.state(pos.relative(direction)))
}

/// `MultifaceBlock.getStateForPlacement(oldState, level, placementPos, placementDirection)`.
fn multiface_state_for(
    block: BlockId,
    old: BlockState,
    level: &dyn BlockReader,
    pos: BlockPos,
    direction: Direction,
) -> Option<BlockState> {
    let face = direction.name();
    let valid =
        (!old.is(block) || !old.get_bool(face)) && multiface_can_attach_at(level, pos, direction);
    if !valid {
        return None;
    }
    let state = if old.is(block) {
        old
    } else if old.fluid_state().info().fluid == util::fluid::WATER {
        block.default_state().set_bool("waterlogged", true)
    } else {
        block.default_state()
    };
    Some(state.set_bool(face, true))
}

/// Number of faces of a vine (`up`, `north`, `east`, `south`, `west`).
fn vine_count_faces(state: BlockState) -> usize {
    ["up", "north", "east", "south", "west"]
        .iter()
        .filter(|face| state.get_bool(face))
        .count()
}

/// `VineBlock.canSupportAtFace`.
fn vine_can_support_at_face(
    block: BlockId,
    level: &dyn BlockReader,
    pos: BlockPos,
    direction: Direction,
) -> bool {
    if direction == Direction::Down {
        return false;
    }
    if multiface_can_attach_at(level, pos, direction) {
        return true;
    }
    if direction.is_vertical() {
        return false;
    }
    let above = level.state(util::above(pos));
    above.is(block) && above.get_bool(direction.name())
}

/// `VineBlock.getUpdatedState`.
fn vine_updated_state(mut state: BlockState, level: &dyn BlockReader, pos: BlockPos) -> BlockState {
    let block = state.block();
    if state.get_bool("up") {
        state = state.set_bool("up", multiface_can_attach_at(level, pos, Direction::Up));
    }
    let above = level.state(util::above(pos));
    for direction in HORIZONTAL {
        let face = direction.name();
        if state.get_bool(face) {
            let support = vine_can_support_at_face(block, level, pos, direction)
                || (above.is(block) && above.get_bool(face));
            state = state.set_bool(face, support);
        }
    }
    state
}

/// The head block of a growing plant (`getHeadBlock`).
fn growing_plant_head(block: BlockId) -> BlockId {
    match block {
        b if b == ids::KELP_PLANT => ids::KELP,
        b if b == ids::CAVE_VINES_PLANT => ids::CAVE_VINES,
        b if b == ids::WEEPING_VINES_PLANT => ids::WEEPING_VINES,
        b if b == ids::TWISTING_VINES_PLANT => ids::TWISTING_VINES,
        b => b,
    }
}

/// The body block of a growing plant (`getBodyBlock`).
fn growing_plant_body(block: BlockId) -> BlockId {
    match growing_plant_head(block) {
        b if b == ids::KELP => ids::KELP_PLANT,
        b if b == ids::CAVE_VINES => ids::CAVE_VINES_PLANT,
        b if b == ids::WEEPING_VINES => ids::WEEPING_VINES_PLANT,
        b if b == ids::TWISTING_VINES => ids::TWISTING_VINES_PLANT,
        b => b,
    }
}

/// The growth direction of a growing plant.
fn growing_plant_direction(block: BlockId) -> Direction {
    let head = growing_plant_head(block);
    if head == ids::CAVE_VINES || head == ids::WEEPING_VINES {
        Direction::Down
    } else {
        Direction::Up
    }
}

/// `GrowingPlantBlock.getStateForPlacement`. A new head gets `random.nextInt(25)` as its age in
/// Vanilla; the level random is not available here, so the age stays 0.
fn growing_plant_placement(
    block: BlockId,
    ctx: &PlaceContext,
    level: &dyn BlockReader,
) -> BlockState {
    let head = growing_plant_head(block);
    let body = growing_plant_body(block);
    let next = level.state(ctx.pos.relative(growing_plant_direction(block)));
    if next.is(head) || next.is(body) {
        body.default_state()
    } else {
        block.default_state()
    }
}

/// `GrowingPlantBlock.canSurvive`.
fn growing_plant_survives(state: BlockState, pos: BlockPos, level: &dyn BlockReader) -> bool {
    let block = state.block();
    let growth = growing_plant_direction(block);
    let attached_pos = pos.relative(growth.opposite());
    let attached = level.state(attached_pos);
    // `KelpBlock.canAttachTo` (kelp plants use their head's).
    if growing_plant_head(block) == ids::KELP && tags::CANNOT_SUPPORT_KELP.has(attached) {
        return false;
    }
    attached.is(growing_plant_head(block))
        || attached.is(growing_plant_body(block))
        || attached.sturdy(growth, Support::Full)
}

#[cfg(test)]
mod tests {
    use vanillite_base::{BlockPos, Direction, Vec3};

    use super::super::tests::*;
    use super::super::util::StateExt;
    use super::super::{can_survive, state_for_placement};

    #[test]
    fn torch_on_top_is_a_standing_torch() {
        let mut level = TestLevel::new();
        let ground = BlockPos::new(0, 64, 0);
        level.set_block(ground, "stone");
        let c = ctx(&level, "torch", ground, Direction::Up, 30.0, 60.0);
        assert_state(state_for_placement(&c, &level), "torch", &[]);
    }

    #[test]
    fn torch_on_a_side_is_a_wall_torch() {
        let mut level = TestLevel::new();
        let wall = BlockPos::new(0, 64, 0);
        level.set_block(wall, "stone");
        // Clicking the east face while looking west.
        let c = ctx(&level, "torch", wall, Direction::East, 90.0, 10.0);
        assert_eq!(c.pos, BlockPos::new(1, 64, 0));
        assert_state(
            state_for_placement(&c, &level),
            "wall_torch",
            &[("facing", "east")],
        );
        let c = ctx(&level, "redstone_torch", wall, Direction::North, 0.0, 10.0);
        assert_state(
            state_for_placement(&c, &level),
            "redstone_wall_torch",
            &[("facing", "north"), ("lit", "true")],
        );
    }

    #[test]
    fn torch_on_air_does_not_survive() {
        let level = TestLevel::new();
        let pos = BlockPos::new(0, 64, 0);
        assert!(!can_survive(block("torch").default_state(), pos, &level));
        assert!(!can_survive(
            block("wall_torch").default_state(),
            pos,
            &level
        ));
        let mut level = TestLevel::new();
        level.set_block(BlockPos::new(0, 63, 0), "stone");
        assert!(can_survive(block("torch").default_state(), pos, &level));
        // A torch floating in the air cannot be placed against air at all.
        let level = TestLevel::new();
        let c = ctx(&level, "torch", pos, Direction::Up, 0.0, 45.0);
        assert_eq!(state_for_placement(&c, &level), None);
    }

    #[test]
    fn sign_rotation_follows_the_yaw() {
        let mut level = TestLevel::new();
        let ground = BlockPos::new(0, 64, 0);
        level.set_block(ground, "stone");
        for (yaw, rotation) in [
            (0.0, "8"),
            (-45.0, "6"),
            (10.5, "8"),
            (100.0, "12"),
            (-170.0, "0"),
            (180.0, "0"),
            (-90.0, "4"),
            (33.7, "9"),
            (725.0, "8"),
        ] {
            let c = ctx(&level, "oak_sign", ground, Direction::Up, yaw, 60.0);
            assert_state(
                state_for_placement(&c, &level),
                "oak_sign",
                &[("rotation", rotation), ("waterlogged", "false")],
            );
        }
        // On a wall: a wall sign facing away from the wall.
        let c = ctx(&level, "oak_sign", ground, Direction::South, 180.0, 0.0);
        assert_state(
            state_for_placement(&c, &level),
            "oak_wall_sign",
            &[("facing", "south")],
        );
    }

    #[test]
    fn skull_rotation_does_not_add_half_a_turn() {
        let mut level = TestLevel::new();
        let ground = BlockPos::new(0, 64, 0);
        level.set_block(ground, "stone");
        let c = ctx(&level, "skeleton_skull", ground, Direction::Up, 90.0, 60.0);
        assert_state(
            state_for_placement(&c, &level),
            "skeleton_skull",
            &[("rotation", "4"), ("powered", "false")],
        );
        level.set_block(BlockPos::new(1, 65, 0), "redstone_block");
        assert_state(
            state_for_placement(&c, &level),
            "skeleton_skull",
            &[("powered", "true")],
        );
    }

    #[test]
    fn candles_stack() {
        let mut level = TestLevel::new();
        let ground = BlockPos::new(0, 64, 0);
        level.set_block(ground, "stone");
        let c = ctx(&level, "candle", ground, Direction::Up, 0.0, 60.0);
        let first = state_for_placement(&c, &level);
        assert_state(first, "candle", &[("candles", "1")]);
        let candle_pos = BlockPos::new(0, 65, 0);
        level.set(candle_pos, first.expect("a candle"));
        let c = ctx(&level, "candle", candle_pos, Direction::Up, 0.0, 60.0);
        assert!(c.replace_clicked);
        assert_eq!(c.pos, candle_pos);
        let second = state_for_placement(&c, &level);
        assert_state(second, "candle", &[("candles", "2")]);
        // Sneaking places next to it instead.
        let hit = Vec3::new(0.5, 65.5, 0.5);
        let c = ctx_at(
            &level,
            "candle",
            candle_pos,
            Direction::Up,
            hit,
            0.0,
            60.0,
            true,
        );
        assert!(!c.replace_clicked);
        // Four candles are full.
        level.set(
            candle_pos,
            block("candle").default_state().set_int("candles", 4),
        );
        let c = ctx(&level, "candle", candle_pos, Direction::Up, 0.0, 60.0);
        assert!(!c.replace_clicked);
    }

    #[test]
    fn buttons_attach_to_the_clicked_face() {
        let mut level = TestLevel::new();
        let wall = BlockPos::new(0, 64, 0);
        level.set_block(wall, "stone");
        let c = ctx(&level, "stone_button", wall, Direction::West, -90.0, 0.0);
        assert_state(
            state_for_placement(&c, &level),
            "stone_button",
            &[("face", "wall"), ("facing", "west")],
        );
        let c = ctx(&level, "lever", wall, Direction::Up, -90.0, 80.0);
        assert_state(
            state_for_placement(&c, &level),
            "lever",
            &[("face", "floor"), ("facing", "east")],
        );
    }

    #[test]
    fn vines_and_lichen_attach() {
        let mut level = TestLevel::new();
        let wall = BlockPos::new(0, 64, 0);
        level.set_block(wall, "stone");
        let c = ctx(&level, "vine", wall, Direction::South, 180.0, 0.0);
        assert_state(
            state_for_placement(&c, &level),
            "vine",
            &[("north", "true"), ("south", "false")],
        );
        let c = ctx(&level, "glow_lichen", wall, Direction::South, 180.0, 0.0);
        assert_state(
            state_for_placement(&c, &level),
            "glow_lichen",
            &[("north", "true"), ("waterlogged", "false")],
        );
    }

    #[test]
    fn dripstone_hangs_from_a_ceiling() {
        let mut level = TestLevel::new();
        let ceiling = BlockPos::new(0, 70, 0);
        level.set_block(ceiling, "stone");
        let c = ctx(
            &level,
            "pointed_dripstone",
            ceiling,
            Direction::Down,
            0.0,
            -60.0,
        );
        assert_state(
            state_for_placement(&c, &level),
            "pointed_dripstone",
            &[("vertical_direction", "down"), ("thickness", "tip")],
        );
    }
}

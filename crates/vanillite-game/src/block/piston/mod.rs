//! Pistons and sticky pistons (`PistonBaseBlock`, `PistonHeadBlock`, `MovingPistonBlock`,
//! `PistonMovingBlockEntity`, `PistonStructureResolver`).
//!
//! A piston that gets or loses power queues a block event (`checkIfExtend`); the event moves
//! the blocks (`triggerEvent`, `moveBlocks`): every moved block becomes a `moving_piston` with
//! a block entity ([`MovingPiston`]) that lands two block entity ticks later, or at once when
//! the piston changes its mind first (`finalTick`, 0-tick behaviour).
//!
//! Exploit switches (D1, [`crate::level::ExploitSettings`]); Vanilla's behaviour is the
//! default:
//! - `headless-pistons = false`: a `moving_piston` that is removed by anything but its own
//!   landing takes an extended piston base behind it along (as a piston head does), so
//!   explosions and block updates can no longer leave an extended base without a head.
//! - `break-unbreakable-blocks = false`: a retracting piston only removes the block in front of
//!   it if that block can be broken (destroy speed not -1), so a piston turned towards bedrock
//!   or an end portal frame in the tick it retracts no longer deletes it.

mod moving;
pub mod resolver;

use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Direction};
use vanillite_data::block::{BlockId, BlockState, PushReaction, flags, ids};
use vanillite_world::entity::EntityId;
use vanillite_worldgen::behaviour::{PosExt, StateExt};

use super::{BlockBehaviour, Interaction, UseContext, sound_id};
use crate::level::game_event::{GameEventContext, events};
use crate::level::{BlockEntity, Level, SOUND_BLOCKS, UPDATE_LIMIT, update};
pub use moving::{MovingPiston, final_tick, moving_piston_tick};
use resolver::{Resolver, Structure, offset};

/// Block event types of `PistonBaseBlock`.
const TRIGGER_EXTEND: i32 = 0;
const TRIGGER_CONTRACT: i32 = 1;
const TRIGGER_DROP: i32 = 2;

/// Buffers the piston code reuses between moves (taken while in use, so a piston moved from
/// inside another move gets fresh ones).
#[derive(Debug, Default)]
pub struct Scratch {
    structure: Structure,
    /// `deleteAfterMove`: position, old state, still to delete.
    delete: Vec<(BlockPos, BlockState, bool)>,
    /// `toPushShapes`.
    shapes: Vec<BlockState>,
    /// `toUpdate`.
    to_update: Vec<BlockState>,
    /// Entities near a moving block.
    entities: Vec<EntityId>,
    /// Depth of running `finalTick`s (their landing removes a `moving_piston` legitimately).
    in_final_tick: u32,
}

/// Fills the piston behaviours of `block`.
pub(super) fn fill(block: BlockId, b: &mut BlockBehaviour) {
    if block == ids::PISTON || block == ids::STICKY_PISTON {
        b.on_place = Some(base_on_place);
        b.neighbor_changed = Some(base_neighbor_changed);
        b.trigger_event = Some(base_trigger_event);
    } else if block == ids::PISTON_HEAD {
        b.affect_neighbors_after_removal = Some(head_affect_neighbors_after_removal);
        b.neighbor_changed = Some(head_neighbor_changed);
    } else if block == ids::MOVING_PISTON {
        b.use_without_item = Some(moving_use_without_item);
        b.affect_neighbors_after_removal = Some(moving_affect_neighbors_after_removal);
    }
}

/// `state.getBlock() instanceof PistonBaseBlock`.
#[inline]
pub fn is_piston_base(state: BlockState) -> bool {
    state.is(ids::PISTON) || state.is(ids::STICKY_PISTON)
}

#[inline]
fn facing(state: BlockState) -> Direction {
    state.direction("facing").unwrap_or(Direction::North)
}

/// `state.getPistonPushReaction()`.
#[inline]
pub fn push_reaction_of(state: BlockState) -> PushReaction {
    state.misc().push_reaction
}

/// `Direction.from3DDataValue(value)`.
fn from_3d(value: i32) -> Direction {
    Direction::ALL[(value.rem_euclid(6)) as usize]
}

/// `PistonBaseBlock.isPushable(state, level, pos, direction, allowDestroyable,
/// connectionDirection)`.
pub fn is_pushable(
    state: BlockState,
    level: &Level,
    pos: BlockPos,
    direction: Direction,
    allow_destroyable: bool,
    connection_direction: Direction,
) -> bool {
    if !level.in_height(pos.y) || !level.border.is_block_within_bounds(pos) {
        return false;
    }
    if state.is_air() {
        return true;
    }
    if (direction == Direction::Down && pos.y == level.min_y())
        || (direction == Direction::Up && pos.y == level.max_y())
    {
        return false;
    }
    if !is_piston_base(state) {
        if state.misc().destroy_speed == -1.0 {
            return false;
        }
        match push_reaction_of(state) {
            PushReaction::Immoveable => return false,
            PushReaction::Popped => return allow_destroyable,
            PushReaction::Push => return direction == connection_direction,
            PushReaction::PushPull => {}
        }
    } else if state.flag("extended") {
        return false;
    }
    !state.has(flags::HAS_BLOCK_ENTITY)
}

/// `getNeighborSignal(level, pos, pushDirection)`: power from any side but the front, or into
/// the block above (quasi-connectivity).
fn neighbor_signal(level: &Level, pos: BlockPos, push: Direction) -> bool {
    for direction in Direction::ALL {
        if direction != push && level.has_signal(pos.relative(direction), direction) {
            return true;
        }
    }
    if level.has_signal(pos, Direction::Down) {
        return true;
    }
    let above = pos.above();
    Direction::ALL
        .into_iter()
        .any(|d| d != Direction::Down && level.has_signal(above.relative(d), d))
}

/// `new PistonStructureResolver(level, pos, direction, extending).resolve()`.
fn can_move(level: &mut Level, pos: BlockPos, direction: Direction, extending: bool) -> bool {
    let mut structure = std::mem::take(&mut level.pistons.structure);
    let ok = Resolver::new(pos, direction, extending).resolve(level, &mut structure);
    level.pistons.structure = structure;
    ok
}

/// `checkIfExtend(level, pos, state)`.
fn check_if_extend(level: &mut Level, pos: BlockPos, state: BlockState) {
    let direction = facing(state);
    let extend = neighbor_signal(level, pos, direction);
    let extended = state.flag("extended");
    let data = direction as i32;
    if extend && !extended {
        if can_move(level, pos, direction, true) {
            level.block_event(pos, state.block(), TRIGGER_EXTEND, data);
        }
    } else if !extend && extended {
        let pushed = offset(pos, direction, 2);
        let pushed_state = level.block(pushed);
        let mut event = TRIGGER_CONTRACT;
        if pushed_state.is(ids::MOVING_PISTON)
            && pushed_state.direction("facing") == Some(direction)
            && let Some(BlockEntity::MovingPiston(m)) = level.block_entity(pushed)
            && m.extending
            && (m.progress(0.0) < 0.5
                || level.game_time() == m.last_ticked
                || level.is_handling_tick())
        {
            event = TRIGGER_DROP;
        }
        level.block_event(pos, state.block(), event, data);
    }
}

/// `PistonBaseBlock.setPlacedBy`: a placed piston checks its power (called by block placing).
pub fn base_placed(level: &mut Level, pos: BlockPos, state: BlockState) {
    check_if_extend(level, pos, state);
}

fn base_on_place(level: &mut Level, state: BlockState, pos: BlockPos, old: BlockState, _: bool) {
    if !old.is(state.block()) && level.block_entity(pos).is_none() {
        check_if_extend(level, pos, state);
    }
}

fn base_neighbor_changed(level: &mut Level, state: BlockState, pos: BlockPos, _: BlockId, _: bool) {
    check_if_extend(level, pos, state);
}

/// `PistonBaseBlock.triggerEvent(state, level, pos, b0, b1)`.
fn base_trigger_event(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    b0: i32,
    b1: i32,
) -> bool {
    let direction = facing(state);
    let sticky = state.is(ids::STICKY_PISTON);
    let extended_state = state.set_flag("extended", true);
    let extend = neighbor_signal(level, pos, direction);
    if extend && (b0 == TRIGGER_CONTRACT || b0 == TRIGGER_DROP) {
        level.set_block(pos, extended_state, update::CLIENTS);
        return false;
    }
    if !extend && b0 == TRIGGER_EXTEND {
        return false;
    }
    let piston_type = if sticky { "sticky" } else { "normal" };
    if b0 == TRIGGER_EXTEND {
        if !move_blocks(level, pos, direction, true, sticky) {
            return false;
        }
        level.set_block(pos, extended_state, update::ALL | update::MOVE_BY_PISTON);
        let pitch = level.random().next_float() * 0.25 + 0.6;
        let sound = sound_id("block.piston.extend");
        level.play_block_sound(None, pos, sound, SOUND_BLOCKS, 0.5, pitch);
        let context = GameEventContext::with_state(None, extended_state);
        level.game_event_block(events().block_activate, pos, context);
    } else if b0 == TRIGGER_CONTRACT || b0 == TRIGGER_DROP {
        let arm = pos.relative(direction);
        if matches!(level.block_entity(arm), Some(BlockEntity::MovingPiston(_))) {
            final_tick(level, arm);
        }
        let moving_state = ids::MOVING_PISTON
            .default_state()
            .set_direction("facing", direction)
            .set("type", piston_type);
        level.set_block(
            pos,
            moving_state,
            update::KNOWN_SHAPE | update::INVISIBLE | update::SKIP_BLOCK_ENTITY_SIDEEFFECTS,
        );
        let base = state
            .block()
            .default_state()
            .set_direction("facing", from_3d(b1 & 7));
        level.set_block_entity(
            pos,
            BlockEntity::MovingPiston(Box::new(MovingPiston::new(base, direction, false, true))),
        );
        level.update_neighbors_at(pos, ids::MOVING_PISTON);
        level.update_neighbour_shapes(moving_state, pos, update::CLIENTS, UPDATE_LIMIT);
        if sticky {
            let two = offset(pos, direction, 2);
            let moving = level.block(two);
            let mut piston_piece = false;
            if moving.is(ids::MOVING_PISTON)
                && let Some(BlockEntity::MovingPiston(m)) = level.block_entity(two)
                && m.direction == direction
                && m.extending
            {
                final_tick(level, two);
                piston_piece = true;
            }
            if !piston_piece {
                if b0 != TRIGGER_CONTRACT
                    || moving.is_air()
                    || !is_pushable(moving, level, two, direction.opposite(), false, direction)
                    || (push_reaction_of(moving) != PushReaction::PushPull
                        && !is_piston_base(moving))
                {
                    remove_arm(level, arm);
                } else {
                    move_blocks(level, pos, direction, false, sticky);
                }
            }
        } else {
            remove_arm(level, arm);
        }
        let pitch = level.random().next_float() * 0.15 + 0.6;
        let sound = sound_id("block.piston.contract");
        level.play_block_sound(None, pos, sound, SOUND_BLOCKS, 0.5, pitch);
        let context = GameEventContext::with_state(None, moving_state);
        level.game_event_block(events().block_deactivate, pos, context);
    }
    true
}

/// `level.removeBlock(pos.relative(direction), false)` of a retracting piston; with
/// `break-unbreakable-blocks` off, blocks that cannot be broken stay.
fn remove_arm(level: &mut Level, arm: BlockPos) {
    if !level.settings.exploits.break_unbreakable_blocks
        && level.block(arm).misc().destroy_speed == -1.0
    {
        return;
    }
    level.remove_block(arm, false);
}

/// `moveBlocks(level, pistonPos, direction, extending)`.
fn move_blocks(
    level: &mut Level,
    piston_pos: BlockPos,
    direction: Direction,
    extending: bool,
    sticky: bool,
) -> bool {
    let arm = piston_pos.relative(direction);
    if !extending && level.block(arm).is(ids::PISTON_HEAD) {
        level.set_block(
            arm,
            BlockState::AIR,
            update::KNOWN_SHAPE | update::INVISIBLE | update::SKIP_BLOCK_ENTITY_SIDEEFFECTS,
        );
    }
    let resolver = Resolver::new(piston_pos, direction, extending);
    let mut s = std::mem::take(&mut level.pistons.structure);
    if !resolver.resolve(level, &mut s) {
        level.pistons.structure = s;
        return false;
    }
    let mut delete = std::mem::take(&mut level.pistons.delete);
    let mut shapes = std::mem::take(&mut level.pistons.shapes);
    let mut to_update = std::mem::take(&mut level.pistons.to_update);
    delete.clear();
    shapes.clear();
    to_update.clear();
    for &pos in &s.to_push {
        let state = level.block(pos);
        shapes.push(state);
        delete.push((pos, state, true));
    }
    let push_direction = if extending {
        direction
    } else {
        direction.opposite()
    };
    for &pos in s.to_destroy.iter().rev() {
        let state = level.block(pos);
        super::drops::drop_resources(level, pos, state, None);
        level.set_block(pos, BlockState::AIR, update::KNOWN_SHAPE | update::CLIENTS);
        let context = GameEventContext::with_state(None, state);
        level.game_event_block(events().block_destroy, pos, context);
        to_update.push(state);
    }
    let unmark = |delete: &mut Vec<(BlockPos, BlockState, bool)>, pos: BlockPos| {
        if let Some(entry) = delete.iter_mut().find(|e| e.0 == pos) {
            entry.2 = false;
        }
    };
    let moving_flags =
        update::INVISIBLE | update::MOVE_BY_PISTON | update::SKIP_BLOCK_ENTITY_SIDEEFFECTS;
    for i in (0..s.to_push.len()).rev() {
        let from = s.to_push[i];
        let state = level.block(from);
        let pos = from.relative(push_direction);
        unmark(&mut delete, pos);
        let moving = ids::MOVING_PISTON
            .default_state()
            .set_direction("facing", direction);
        level.set_block(pos, moving, moving_flags);
        // The TNT duplication switch (D1) decides what a primed TNT block moves as.
        let moved = crate::block::tnt::piston_moved_state(level, shapes[i], state);
        level.set_block_entity(
            pos,
            BlockEntity::MovingPiston(Box::new(MovingPiston::new(
                moved, direction, extending, false,
            ))),
        );
        to_update.push(state);
    }
    if extending {
        let piston_type = if sticky { "sticky" } else { "normal" };
        let head = ids::PISTON_HEAD
            .default_state()
            .set_direction("facing", direction)
            .set("type", piston_type);
        let moving = ids::MOVING_PISTON
            .default_state()
            .set_direction("facing", direction)
            .set("type", piston_type);
        unmark(&mut delete, arm);
        level.set_block(arm, moving, moving_flags);
        level.set_block_entity(
            arm,
            BlockEntity::MovingPiston(Box::new(MovingPiston::new(head, direction, true, true))),
        );
    }
    // `deleteAfterMove` is a `HashMap`: its keys go in Java's hash order.
    delete.retain(|e| e.2);
    java_hash_order(&mut delete, s.to_push.len());
    for &(pos, _, _) in &delete {
        level.set_block(
            pos,
            BlockState::AIR,
            update::CLIENTS | update::KNOWN_SHAPE | update::MOVE_BY_PISTON,
        );
    }
    for &(pos, old, _) in &delete {
        level.update_indirect_neighbour_shapes(old, pos, update::CLIENTS, UPDATE_LIMIT);
        level.update_neighbour_shapes(BlockState::AIR, pos, update::CLIENTS, UPDATE_LIMIT);
        level.update_indirect_neighbour_shapes(BlockState::AIR, pos, update::CLIENTS, UPDATE_LIMIT);
    }
    let mut index = 0;
    for &pos in s.to_destroy.iter().rev() {
        let state = to_update[index];
        index += 1;
        level.affect_neighbors_after_removal(state, pos, false);
        level.update_indirect_neighbour_shapes(state, pos, update::CLIENTS, UPDATE_LIMIT);
        level.update_neighbors_at(pos, state.block());
    }
    for &pos in s.to_push.iter().rev() {
        let block = to_update[index].block();
        index += 1;
        level.update_neighbors_at(pos, block);
    }
    if extending {
        level.update_neighbors_at(arm, ids::PISTON_HEAD);
    }
    level.pistons.structure = s;
    level.pistons.delete = delete;
    level.pistons.shapes = shapes;
    level.pistons.to_update = to_update;
    true
}

/// Orders the entries of a Java `HashMap<BlockPos, _>` like its iteration: by bucket of the
/// spread `BlockPos.hashCode()`, in insertion order within a bucket. `max_size` is the most
/// entries the map held (its table never shrinks).
fn java_hash_order(entries: &mut [(BlockPos, BlockState, bool)], max_size: usize) {
    let mut capacity: u32 = 16;
    while max_size as u32 > capacity / 4 * 3 {
        capacity *= 2;
    }
    let mask = capacity - 1;
    entries.sort_by_key(|(pos, _, _)| {
        let h = pos
            .y
            .wrapping_add(pos.z.wrapping_mul(31))
            .wrapping_mul(31)
            .wrapping_add(pos.x) as u32;
        (h ^ (h >> 16)) & mask
    });
}

// --- Piston head ---------------------------------------------------------------------------

/// `PistonHeadBlock.isFittingBase(armState, potentialBase)`.
fn is_fitting_base(arm: BlockState, base: BlockState) -> bool {
    let block = if arm.value("type") == Some("sticky") {
        ids::STICKY_PISTON
    } else {
        ids::PISTON
    };
    base.is(block) && base.flag("extended") && base.direction("facing") == arm.direction("facing")
}

fn head_affect_neighbors_after_removal(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    _: bool,
) {
    let base = pos.relative(facing(state).opposite());
    if is_fitting_base(state, level.block(base)) {
        level.destroy_block(base, true, None);
    }
}

fn head_neighbor_changed(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    source: BlockId,
    _: bool,
) {
    if level.can_survive(state, pos) {
        level.neighbor_changed(pos.relative(facing(state).opposite()), source);
    }
}

/// `PistonHeadBlock.playerWillDestroy`: a player who gets no drops (creative) breaks the base
/// too, without drops (called by block breaking).
pub fn head_player_will_destroy(
    level: &mut Level,
    pos: BlockPos,
    state: BlockState,
    prevents_drops: bool,
) {
    if prevents_drops {
        let base = pos.relative(facing(state).opposite());
        if is_fitting_base(state, level.block(base)) {
            level.destroy_block(base, false, None);
        }
    }
}

// --- Moving piston -------------------------------------------------------------------------

/// `MovingPistonBlock.destroy(level, pos, state)`: a player broke a moving block; an extended
/// piston base behind it goes too (called by block breaking).
pub fn moving_destroyed(level: &mut Level, pos: BlockPos, state: BlockState) {
    let base = pos.relative(facing(state).opposite());
    let base_state = level.block(base);
    if is_piston_base(base_state) && base_state.flag("extended") {
        level.remove_block(base, false);
    }
}

fn moving_use_without_item(
    level: &mut Level,
    _: BlockState,
    pos: BlockPos,
    _: &UseContext,
) -> Interaction {
    if level.block_entity(pos).is_none() {
        level.remove_block(pos, false);
        Interaction::Consume
    } else {
        Interaction::Pass
    }
}

/// Not in Vanilla: the `headless-pistons = false` rule (see the module documentation).
fn moving_affect_neighbors_after_removal(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    moved: bool,
) {
    if moved || level.settings.exploits.headless_pistons || level.pistons.in_final_tick > 0 {
        return;
    }
    let direction = facing(state);
    let base = pos.relative(direction.opposite());
    let base_state = level.block(base);
    if is_piston_base(base_state)
        && base_state.flag("extended")
        && base_state.direction("facing") == Some(direction)
    {
        level.destroy_block(base, true, None);
    }
}

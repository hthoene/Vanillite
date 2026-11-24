//! Hoppers (`HopperBlock`, `HopperBlockEntity`).
//!
//! A hopper is enabled while it receives no signal. Its block entity ticks every tick: the
//! transfer cooldown counts down and, once it is over, the hopper tries to push one item out and
//! pull one in, waiting 8 ticks after a transfer. Containers do not exist yet: [`transfer`] is
//! the seam that moves items and moves none for now.

use vanillite_base::{Aabb, BlockPos, Direction};
use vanillite_data::block::{BlockId, BlockState, ids};
use vanillite_worldgen::behaviour::{PosExt, StateExt};

use super::container::ContainerRef;
use super::{BlockBehaviour, dispenser};
use crate::item::{self, ItemStack};
use crate::level::{BlockEntity, Level, update};

/// A hopper block entity: its stacks and the transfer timing.
#[derive(Clone, Debug, PartialEq)]
pub struct Hopper {
    /// The five stacks.
    pub items: [crate::item::ItemStack; 5],
    /// `CustomName`.
    pub custom_name: Option<vanillite_protocol::text::Component>,
    /// `cooldownTime`: ticks until the next transfer attempt (`TransferCooldown`).
    pub cooldown_time: i32,
    /// `tickedGameTime`: the game time of the last tick.
    pub ticked_game_time: i64,
}

impl Default for Hopper {
    fn default() -> Self {
        Hopper {
            items: std::array::from_fn(|_| crate::item::ItemStack::EMPTY),
            custom_name: None,
            cooldown_time: -1,
            ticked_game_time: 0,
        }
    }
}

/// `HopperBlockEntity.MOVE_ITEM_SPEED`.
const MOVE_ITEM_SPEED: i32 = 8;

/// Fills the hopper behaviours of `block`.
pub(super) fn fill(block: BlockId, b: &mut BlockBehaviour) {
    if block == ids::HOPPER {
        b.on_place = Some(on_place);
        b.neighbor_changed = Some(neighbor_changed);
        b.affect_neighbors_after_removal = Some(dispenser::container_removed);
        b.analog_output = Some(dispenser::container_signal);
    }
}

/// `HopperBlock.onPlace`.
fn on_place(level: &mut Level, state: BlockState, pos: BlockPos, old: BlockState, _moved: bool) {
    if old.block() != state.block() {
        check_powered_state(level, pos, state);
    }
}

/// `HopperBlock.neighborChanged`.
fn neighbor_changed(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    _source: BlockId,
    _moved: bool,
) {
    check_powered_state(level, pos, state);
}

/// `HopperBlock.checkPoweredState`: enabled without a signal.
fn check_powered_state(level: &mut Level, pos: BlockPos, state: BlockState) {
    let should_be_on = !level.has_neighbor_signal(pos);
    if should_be_on != state.flag("enabled") {
        level.set_block(
            pos,
            state.set_flag("enabled", should_be_on),
            update::CLIENTS,
        );
    }
}

/// `HopperBlockEntity.pushItemsTick`.
pub fn push_items_tick(level: &mut Level, pos: BlockPos, state: BlockState) {
    let game_time = level.game_time();
    let Some(BlockEntity::Hopper(hopper)) = level.block_entity_mut(pos) else {
        return;
    };
    hopper.cooldown_time -= 1;
    hopper.ticked_game_time = game_time;
    if hopper.cooldown_time > 0 {
        return;
    }
    hopper.cooldown_time = 0;
    // `tryMoveItems`.
    if state.flag("enabled") && transfer(level, pos, state) {
        if let Some(BlockEntity::Hopper(hopper)) = level.block_entity_mut(pos) {
            hopper.cooldown_time = MOVE_ITEM_SPEED;
        }
        // `setChanged`: comparators re-read the hopper.
        level.update_neighbour_for_output_signal(pos, state.block());
    }
}

/// `ejectItems` and `suckInItems` of `tryMoveItems`: whether an item moved.
pub fn transfer(level: &mut Level, pos: BlockPos, state: BlockState) -> bool {
    let mut changed = false;
    let empty = hopper_items(level, pos).is_none_or(|items| items.iter().all(ItemStack::is_empty));
    if !empty {
        changed = eject_items(level, pos, state);
    }
    let full = hopper_items(level, pos).is_none_or(|items| {
        items
            .iter()
            .all(|s| !s.is_empty() && s.count == item::max_stack_size(s))
    });
    if !full {
        changed |= suck_in_items(level, pos);
    }
    changed
}

fn hopper_items(level: &Level, pos: BlockPos) -> Option<&[ItemStack]> {
    match level.block_entity(pos)? {
        BlockEntity::Hopper(h) => Some(&h.items),
        _ => None,
    }
}

/// `ejectItems`: one item of the first slot that fits goes into the container in front.
fn eject_items(level: &mut Level, pos: BlockPos, state: BlockState) -> bool {
    let facing = state.direction("facing").unwrap_or(Direction::Down);
    let Some(target) = ContainerRef::at(level, pos.relative(facing)) else {
        return false;
    };
    let face = facing.opposite();
    // `isFullContainer`.
    if target.slots_for_face(level, face).into_iter().all(|slot| {
        let s = target.get(level, slot);
        !s.is_empty() && s.count >= item::max_stack_size(&s)
    }) {
        return false;
    }
    let source = ContainerRef::Block(pos);
    for slot in 0..5 {
        let stack = source.get(level, slot);
        if stack.is_empty() {
            continue;
        }
        let one = stack.with_count(1);
        let rest = add_item(level, Some(source), target, one, Some(face));
        if rest.is_empty() {
            if let Some(s) = source.get_mut(level, slot) {
                s.shrink(1);
            }
            target.set_changed(level);
            return true;
        }
    }
    false
}

/// `suckInItems`: one item from the container above, or item entities above.
fn suck_in_items(level: &mut Level, pos: BlockPos) -> bool {
    let above = pos.above();
    let me = ContainerRef::Block(pos);
    if let Some(source) = ContainerRef::at(level, above) {
        for slot in source.slots_for_face(level, Direction::Down) {
            let stack = source.get(level, slot);
            if stack.is_empty() || !source.can_take(level, slot, &stack, Direction::Down) {
                continue;
            }
            let rest = add_item(level, Some(source), me, stack.with_count(1), None);
            if rest.is_empty() {
                if let Some(s) = source.get_mut(level, slot) {
                    s.shrink(1);
                }
                source.set_changed(level);
                return true;
            }
        }
        return false;
    }
    let state = level.block(above);
    let shape = state.collision_shape();
    let full = shape.len() == 1 && shape.first() == Some(&[0.0, 0.0, 0.0, 1.0, 1.0, 1.0]);
    let blocked = full && !(state.is(ids::BEEHIVE) || state.is(ids::BEE_NEST));
    if blocked {
        return false;
    }
    // `getItemsAtAndAbove`: the suck box (`Block.column(16, 11, 32)`) at the hopper.
    let (x, y, z) = (f64::from(pos.x), f64::from(pos.y), f64::from(pos.z));
    let aabb = Aabb::new(x, y + 11.0 / 16.0, z, x + 1.0, y + 2.0, z + 1.0);
    let mut found = Vec::new();
    level
        .entities
        .store
        .for_each_in(&aabb, false, |id| found.push(id));
    for id in found {
        let Some(stack) = level.entities.item(id).map(|d| d.stack.clone()) else {
            continue;
        };
        if stack.is_empty() {
            continue;
        }
        // `addItem(container, itemEntity)`.
        let rest = add_item(level, None, me, stack, None);
        if rest.is_empty() {
            if let Some(data) = level.entities.item_mut(id) {
                data.stack = ItemStack::EMPTY;
            }
            level.entities.despawn(id);
            return true;
        }
        if let Some(data) = level.entities.item_mut(id) {
            data.stack = rest;
        }
    }
    false
}

/// `HopperBlockEntity.addItem(from, container, stack, face)`: what does not fit.
pub fn add_item(
    level: &mut Level,
    from: Option<ContainerRef>,
    into: ContainerRef,
    mut stack: ItemStack,
    face: Option<Direction>,
) -> ItemStack {
    let slots = match face {
        Some(face) => into.slots_for_face(level, face),
        None => (0..into.size(level)).collect(),
    };
    for slot in slots {
        if stack.is_empty() {
            break;
        }
        stack = try_move_in_item(level, from, into, stack, slot);
    }
    stack
}

/// `tryMoveInItem`.
fn try_move_in_item(
    level: &mut Level,
    from: Option<ContainerRef>,
    into: ContainerRef,
    mut stack: ItemStack,
    slot: usize,
) -> ItemStack {
    if !into.can_place(level, slot, &stack) {
        return stack;
    }
    let current = into.get(level, slot);
    let was_empty = into.is_empty(level);
    let mut success = false;
    if current.is_empty() {
        into.set(level, slot, std::mem::take(&mut stack));
        success = true;
    } else if current.count <= item::max_stack_size(&current)
        && current.same_item_same_components(&stack)
    {
        let space = item::max_stack_size(&stack) - current.count;
        let n = stack.count.min(space);
        stack.shrink(n);
        if let Some(c) = into.get_mut(level, slot) {
            c.grow(n);
        }
        success = n > 0;
    }
    if success {
        if was_empty
            && let Some(target) = into.hopper()
            && let Some(BlockEntity::Hopper(h)) = level.block_entity(target)
            && h.cooldown_time <= MOVE_ITEM_SPEED
        {
            let target_time = h.ticked_game_time;
            let skip = match from.and_then(ContainerRef::hopper) {
                Some(f) => match level.block_entity(f) {
                    Some(BlockEntity::Hopper(src)) if target_time >= src.ticked_game_time => 1,
                    _ => 0,
                },
                None => 0,
            };
            if let Some(BlockEntity::Hopper(h)) = level.block_entity_mut(target) {
                h.cooldown_time = MOVE_ITEM_SPEED - skip;
            }
        }
        into.set_changed(level);
    }
    stack
}

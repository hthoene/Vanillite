//! Shelves (`ShelfBlock`, `SideChainPartBlock`, `ShelfBlockEntity`): three slots swapped with
//! the selected hotbar slot; powered shelves chain up to three wide sideways and swap the whole
//! hotbar with the chain.

use vanillite_base::{BlockPos, Direction, Vec3};
use vanillite_data::block::{BlockId, BlockKind, BlockState};
use vanillite_worldgen::behaviour::StateExt;

use super::place::tags::WOODEN_SHELVES;
use super::place::util::DirExt;
use super::{BlockBehaviour, Interaction, sound_id};
use crate::item::ItemStack;
use crate::level::game_event::{GameEventContext, events};
use crate::level::{BlockEntity, Level, SOUND_BLOCKS};

const PART: &str = "side_chain_part";
/// `getMaxChainLength`.
const MAX_CHAIN: usize = 3;

/// `SideChainPart`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    Unconnected,
    Right,
    Center,
    Left,
}

impl Part {
    fn of(state: BlockState) -> Part {
        match state.value(PART) {
            Some("right") => Part::Right,
            Some("center") => Part::Center,
            Some("left") => Part::Left,
            _ => Part::Unconnected,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Part::Unconnected => "unconnected",
            Part::Right => "right",
            Part::Center => "center",
            Part::Left => "left",
        }
    }

    fn connected(self) -> bool {
        self != Part::Unconnected
    }

    /// `isConnectionTowards`.
    fn towards(self, end: Part) -> bool {
        self == Part::Center || self == end
    }

    fn chain_end(self) -> bool {
        self != Part::Center
    }

    fn connected_right(self) -> Part {
        match self {
            Part::Unconnected | Part::Left => Part::Left,
            Part::Right | Part::Center => Part::Center,
        }
    }

    fn connected_left(self) -> Part {
        match self {
            Part::Unconnected | Part::Right => Part::Right,
            Part::Center | Part::Left => Part::Center,
        }
    }

    fn disconnected_right(self) -> Part {
        match self {
            Part::Unconnected | Part::Left => Part::Unconnected,
            Part::Right | Part::Center => Part::Right,
        }
    }

    fn disconnected_left(self) -> Part {
        match self {
            Part::Unconnected | Part::Right => Part::Unconnected,
            Part::Center | Part::Left => Part::Left,
        }
    }
}

/// Whether `block` is a shelf.
pub fn is_shelf(block: BlockId) -> bool {
    block.info().kind == BlockKind::ShelfBlock
}

/// Fills the shelf behaviours.
pub(super) fn fill(block: BlockId, b: &mut BlockBehaviour) {
    if is_shelf(block) {
        b.on_place = Some(on_place);
        b.neighbor_changed = Some(neighbor_changed);
        b.affect_neighbors_after_removal = Some(affect_neighbors_after_removal);
        b.analog_output = Some(analog_output);
    }
}

fn facing(state: BlockState) -> Direction {
    state.direction("facing").unwrap_or(Direction::North)
}

/// `isConnectable`: a powered wooden shelf.
fn connectable(state: BlockState) -> bool {
    WOODEN_SHELVES.contains(state.block()) && state.flag("powered")
}

/// `Neighbors.createNewNeighbor`: the part of a shelf that can join a chain facing `facing`.
fn neighbor(level: &Level, pos: BlockPos, facing: Direction) -> Option<Part> {
    let state = level.block(pos);
    (connectable(state) && self::facing(state) == facing).then(|| Part::of(state))
}

/// `getAllBlocksConnectedTo`: the chain through `pos`, from its left end to its right end.
fn connected(level: &Level, pos: BlockPos) -> Vec<BlockPos> {
    let state = level.block(pos);
    if !connectable(state) {
        return Vec::new();
    }
    let facing = facing(state);
    let mut left = Vec::new();
    let mut right = Vec::new();
    for (dir, end, out) in [
        (facing.clockwise(), Part::Left, &mut left),
        (facing.counter_clockwise(), Part::Right, &mut right),
    ] {
        for steps in 1..MAX_CHAIN {
            let at = offset(pos, dir, steps);
            let part = neighbor(level, at, facing);
            if part.is_some_and(|p| p.towards(end)) {
                out.push(at);
            }
            if part.is_none_or(Part::chain_end) {
                break;
            }
        }
    }
    left.reverse();
    left.push(pos);
    left.extend(right);
    left
}

fn offset(pos: BlockPos, dir: Direction, steps: usize) -> BlockPos {
    let mut at = pos;
    for _ in 0..steps {
        at = at.relative(dir);
    }
    at
}

/// `setPart`.
fn set_part(level: &mut Level, pos: BlockPos, part: Part) {
    let state = level.block(pos);
    if Part::of(state) != part {
        level.set_block_and_update(pos, state.set(PART, part.name()));
    }
}

/// `updateNeighborsAfterPoweringDown`.
fn powering_down(level: &mut Level, pos: BlockPos, state: BlockState) {
    let facing = facing(state);
    let left = pos.relative(facing.clockwise());
    if let Some(part) = neighbor(level, left, facing) {
        set_part(level, left, part.disconnected_right());
    }
    let right = pos.relative(facing.counter_clockwise());
    if let Some(part) = neighbor(level, right, facing) {
        set_part(level, right, part.disconnected_left());
    }
}

/// `updateSelfAndNeighborsOnPoweringUp`.
fn powering_up(level: &mut Level, pos: BlockPos, state: BlockState, old: BlockState) {
    if !connectable(state) {
        return;
    }
    // `isBeingUpdatedByNeighbor`.
    if Part::of(state).connected() || (connectable(old) && Part::of(old).connected()) {
        return;
    }
    let facing = facing(state);
    let (left, right) = (
        pos.relative(facing.clockwise()),
        pos.relative(facing.counter_clockwise()),
    );
    let left_part = neighbor(level, left, facing);
    let right_part = neighbor(level, right, facing);
    let chain_left = left_part.map_or(0, |_| connected(level, left).len());
    let chain_right = right_part.map_or(0, |_| connected(level, right).len());
    let can_connect = |new: usize, current: usize| new > 0 && current + new <= MAX_CHAIN;
    let mut part = Part::Unconnected;
    let mut length = 1;
    if let Some(left_part) = left_part
        && can_connect(chain_left, length)
    {
        part = part.connected_left();
        set_part(level, left, left_part.connected_right());
        length += chain_left;
    }
    if let Some(right_part) = right_part
        && can_connect(chain_right, length)
    {
        part = part.connected_right();
        set_part(level, right, right_part.connected_left());
    }
    set_part(level, pos, part);
}

fn on_place(level: &mut Level, state: BlockState, pos: BlockPos, old: BlockState, _moved: bool) {
    if state.flag("powered") {
        powering_up(level, pos, state, old);
    } else {
        powering_down(level, pos, state);
    }
}

fn neighbor_changed(level: &mut Level, state: BlockState, pos: BlockPos, _: BlockId, _: bool) {
    let signal = level.has_neighbor_signal(pos);
    if state.flag("powered") == signal {
        return;
    }
    let mut new = state.set_flag("powered", signal);
    if !signal {
        new = new.set(PART, Part::Unconnected.name());
    }
    level.set_block_and_update(pos, new);
    let sound = if signal {
        "block.shelf.activate"
    } else {
        "block.shelf.deactivate"
    };
    play_sound(level, pos, sound);
    let event = if signal {
        events().block_activate
    } else {
        events().block_deactivate
    };
    level.game_event_block(event, pos, GameEventContext::with_state(None, new));
}

fn affect_neighbors_after_removal(level: &mut Level, state: BlockState, pos: BlockPos, _: bool) {
    // `Containers.updateNeighboursAfterDestroy`.
    level.update_neighbour_for_output_signal(pos, state.block());
    powering_down(level, pos, state);
}

/// `getAnalogOutputSignal`: one bit per filled slot, read from behind.
fn analog_output(level: &Level, state: BlockState, pos: BlockPos, direction: Direction) -> i32 {
    if direction != facing(state).opposite() {
        return 0;
    }
    match level.block_entity(pos) {
        Some(BlockEntity::Shelf(shelf)) => shelf
            .items
            .iter()
            .enumerate()
            .map(|(i, s)| i32::from(!s.is_empty()) << i)
            .sum(),
        _ => 0,
    }
}

fn play_sound(level: &mut Level, pos: BlockPos, name: &str) {
    level.play_block_sound(None, pos, sound_id(name), SOUND_BLOCKS, 1.0, 1.0);
}

/// `swapItemNoUpdate`.
fn swap(level: &mut Level, pos: BlockPos, slot: usize, stack: ItemStack) -> ItemStack {
    match level.block_entity_mut(pos) {
        Some(BlockEntity::Shelf(shelf)) => shelf
            .items
            .get_mut(slot)
            .map_or(ItemStack::EMPTY, |s| std::mem::replace(s, stack)),
        _ => ItemStack::EMPTY,
    }
}

/// `ShelfBlockEntity.setChanged(event)`.
fn set_changed(level: &mut Level, pos: BlockPos, event: Option<u16>) {
    level.block_entity_changed(pos);
    super::container::set_changed(level, pos);
    if let Some(event) = event {
        let state = level.block(pos);
        level.game_event_block(event, pos, GameEventContext::with_state(None, state));
    }
    level.send_block_entity_updated(pos);
}

/// `ShelfBlock.useItemOn` with the main hand; `None` is `PASS`. `hotbar` is the player's
/// hotbar, `selected` the selected slot in it, `infinite` creative.
#[allow(clippy::too_many_arguments)]
pub fn use_item_on(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    face: Direction,
    hit: Vec3,
    infinite: bool,
    hotbar: &mut [ItemStack],
    selected: usize,
) -> Option<Interaction> {
    if !matches!(level.block_entity(pos), Some(BlockEntity::Shelf(_))) {
        return None;
    }
    let slot = super::bookshelf::selectable_hit_slot(face, facing(state), pos, hit, 1, 3)?;
    if !state.flag("powered") {
        // `swapSingleItem`.
        let held = hotbar.get(selected).cloned().unwrap_or(ItemStack::EMPTY);
        let held_empty = held.is_empty();
        let copy = held.clone();
        let removed = swap(level, pos, slot, held);
        let removed_empty = removed.is_empty();
        let new = if infinite && removed_empty {
            copy
        } else {
            removed
        };
        let event =
            crate::player::using::use_vibrations(&new).then_some(events().item_interact_finish);
        if let Some(s) = hotbar.get_mut(selected) {
            *s = new;
        }
        set_changed(level, pos, event);
        if !removed_empty {
            play_sound(
                level,
                pos,
                if held_empty {
                    "block.shelf.take_item"
                } else {
                    "block.shelf.single_swap"
                },
            );
        } else {
            if held_empty {
                return None;
            }
            play_sound(level, pos, "block.shelf.place_item");
        }
        return Some(Interaction::Success);
    }
    // `swapHotbar`.
    let chain = connected(level, pos);
    let mut swapped = false;
    let n = chain.len();
    for (index, part) in chain.into_iter().enumerate() {
        if !matches!(level.block_entity(part), Some(BlockEntity::Shelf(_))) {
            continue;
        }
        for slot in 0..3usize {
            let Some(inventory_slot) = (9 + slot).checked_sub((n - index) * 3) else {
                continue;
            };
            let Some(target) = hotbar.get_mut(inventory_slot) else {
                continue;
            };
            let placed = std::mem::take(target);
            let placed_empty = placed.is_empty();
            let removed = swap(level, part, slot, placed);
            if !placed_empty || !removed.is_empty() {
                if let Some(target) = hotbar.get_mut(inventory_slot) {
                    *target = removed;
                }
                swapped = true;
            }
        }
        set_changed(level, part, Some(events().entity_interact));
    }
    if !swapped {
        return Some(Interaction::Consume);
    }
    play_sound(level, pos, "block.shelf.multi_swap");
    Some(Interaction::Success)
}

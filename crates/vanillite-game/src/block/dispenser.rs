//! Dispensers and droppers (`DispenserBlock`, `DropperBlock`).
//!
//! A rising signal at the block or the block above sets `triggered` and schedules a tick in 4
//! ticks; the tick dispenses one item from a random non-empty slot. Containers do not exist yet:
//! every dispenser is empty, so the tick plays the "dispense failed" click
//! (`LevelEvent.SOUND_DISPENSER_FAIL`). [`dispense_slot`] is the seam the container
//! implementation fills; the item behaviours (`DispenseItemBehavior`, TNT, projectiles, buckets,
//! …) and the dropper's transfer into containers follow it.

use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Direction};
use vanillite_data::block::{BlockId, BlockState, ids};
use vanillite_worldgen::behaviour::{PosExt, StateExt};

use super::BlockBehaviour;
use crate::level::{Level, update};

/// `LevelEvent.SOUND_DISPENSER_FAIL`.
const DISPENSE_FAIL: i32 = 1001;
/// `DispenserBlock.TRIGGER_DURATION`.
const TRIGGER_DURATION: i32 = 4;

/// Fills the dispenser and dropper behaviours of `block`.
pub(super) fn fill(block: BlockId, b: &mut BlockBehaviour) {
    if block == ids::DISPENSER || block == ids::DROPPER {
        b.neighbor_changed = Some(neighbor_changed);
        b.tick = Some(tick);
        b.affect_neighbors_after_removal = Some(container_removed);
        b.analog_output = Some(container_signal);
    }
}

/// `DispenserBlock.neighborChanged`.
fn neighbor_changed(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    _source: BlockId,
    _moved: bool,
) {
    let should_trigger = level.has_neighbor_signal(pos) || level.has_neighbor_signal(pos.above());
    let triggered = state.flag("triggered");
    if should_trigger && !triggered {
        level.schedule_block_tick(pos, state.block(), TRIGGER_DURATION);
        level.set_block(pos, state.set_flag("triggered", true), update::CLIENTS);
    } else if !should_trigger && triggered {
        level.set_block(pos, state.set_flag("triggered", false), update::CLIENTS);
    }
}

/// `DispenserBlock.tick` / `DropperBlock.dispenseFrom`.
fn tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    let Some(slot) = dispense_slot(level, pos) else {
        level.level_event(None, DISPENSE_FAIL, pos, 0);
        let state = level.block(pos);
        let context = crate::level::game_event::GameEventContext::with_state(None, state);
        level.game_event_block(
            crate::level::game_event::events().block_activate,
            pos,
            context,
        );
        return;
    };
    let source = super::container::ContainerRef::Block(pos);
    let stack = source.get(level, slot);
    if stack.is_empty() {
        return;
    }
    let facing = state.direction("facing").unwrap_or(Direction::North);
    if state.block() == ids::DROPPER
        && let Some(into) = super::container::ContainerRef::at(level, pos.relative(facing))
    {
        // `DropperBlock.dispenseFrom`: one item into the container in front.
        let rest = super::hopper::add_item(
            level,
            Some(source),
            into,
            stack.with_count(1),
            Some(facing.opposite()),
        );
        let mut remaining = stack;
        if rest.is_empty() {
            remaining.shrink(1);
        }
        source.set(level, slot, remaining);
        return;
    }
    // `DefaultDispenseItemBehavior` (the item behaviours of dispensers are not ported yet:
    // every item drops like from a dropper).
    let mut remaining = stack;
    let one = remaining.split(1);
    spawn_dispensed(level, pos, facing, one);
    level.level_event(None, DISPENSE_SUCCESS, pos, 0);
    level.level_event(None, DISPENSE_ANIMATION, pos, direction_3d(facing));
    source.set(level, slot, remaining);
    super::container::set_changed(level, pos);
}

/// `LevelEvent.SOUND_DISPENSER_DISPENSE`.
const DISPENSE_SUCCESS: i32 = 1000;
/// `LevelEvent.PARTICLES_SHOOT_SMOKE`.
const DISPENSE_ANIMATION: i32 = 2000;

/// `Direction.get3DDataValue`.
pub(super) fn direction_3d(d: Direction) -> i32 {
    match d {
        Direction::Down => 0,
        Direction::Up => 1,
        Direction::North => 2,
        Direction::South => 3,
        Direction::West => 4,
        Direction::East => 5,
    }
}

/// `DefaultDispenseItemBehavior.spawnItem(level, stack, 6, facing, dispensePosition)`.
pub(super) fn spawn_dispensed(
    level: &mut Level,
    pos: BlockPos,
    facing: Direction,
    stack: crate::item::ItemStack,
) {
    let [dx, dy, dz] = facing.normal();
    let x = f64::from(pos.x) + 0.5 + 0.7 * f64::from(dx);
    let mut y = f64::from(pos.y) + 0.5 + 0.7 * f64::from(dy);
    let z = f64::from(pos.z) + 0.5 + 0.7 * f64::from(dz);
    y -= if dy != 0 { 0.125 } else { 0.156_25 };
    // `new ItemEntity(level, x, y, z, stack)` draws a velocity first.
    level.random().next_double();
    level.random().next_double();
    let pow = level.random().next_double() * 0.1 + 0.2;
    let spread = 0.017_227_5 * 6.0;
    let mut triangle = |mode: f64| {
        let a = level.random().next_double();
        let b = level.random().next_double();
        mode + spread * (a - b)
    };
    let vx = triangle(f64::from(dx) * pow);
    let vy = triangle(0.2);
    let vz = triangle(f64::from(dz) * pow);
    super::drops::spawn_item(
        level,
        vanillite_base::Vec3::new(x, y, z),
        stack,
        vanillite_base::Vec3::new(vx, vy, vz),
        0,
        None,
    );
}

/// `DispenserBlockEntity.getRandomSlot(level.getRandom())`: a random non-empty slot (one
/// `nextInt` per non-empty slot), `None` if the dispenser is empty.
pub fn dispense_slot(level: &mut Level, pos: BlockPos) -> Option<usize> {
    let filled: Vec<usize> = super::container::items(level, pos)?
        .iter()
        .enumerate()
        .filter(|(_, s)| !s.is_empty())
        .map(|(i, _)| i)
        .collect();
    let mut chosen = None;
    for (odds, slot) in (1..).zip(filled) {
        if level.random().next_int_bounded(odds) == 0 {
            chosen = Some(slot);
        }
    }
    chosen
}

/// `Containers.updateNeighboursAfterDestroy`: comparators re-read the removed container.
pub fn container_removed(level: &mut Level, state: BlockState, pos: BlockPos, _moved: bool) {
    level.update_neighbour_for_output_signal(pos, state.block());
}

/// `getAnalogOutputSignal` of containers (`AbstractContainerMenu
/// .getRedstoneSignalFromBlockEntity`): the fill level; a double chest counts both halves, a
/// blocked chest reads 0.
pub fn container_signal(
    level: &Level,
    state: BlockState,
    pos: BlockPos,
    _direction: Direction,
) -> i32 {
    use super::container::{Kind, kind_of};
    if matches!(
        kind_of(state.block()),
        Some(Kind::Chest | Kind::TrappedChest)
    ) {
        let Some(request) = super::container::menu_provider(level, state, pos) else {
            return 0;
        };
        if let crate::menu::container::Storage::Double(a, b) = request.storage {
            let mut all: Vec<crate::item::ItemStack> = Vec::with_capacity(54);
            all.extend(
                super::container::items(level, a)
                    .unwrap_or_default()
                    .iter()
                    .cloned(),
            );
            all.extend(
                super::container::items(level, b)
                    .unwrap_or_default()
                    .iter()
                    .cloned(),
            );
            return crate::menu::redstone_signal(&all, crate::menu::CONTAINER_MAX);
        }
    }
    super::container::items(level, pos).map_or(0, |items| {
        crate::menu::redstone_signal(items, crate::menu::CONTAINER_MAX)
    })
}

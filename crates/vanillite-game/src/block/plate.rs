//! Pressure plates (`BasePressurePlateBlock`, `PressurePlateBlock`,
//! `WeightedPressurePlateBlock`) and the entity sensing they share with tripwires.
//!
//! Plates sense entities whose bounding box touches the plate's touch box. "Living" entities
//! of stone plates are players and every entity type whose mob category is not `misc`, plus
//! armor stands (Vanilla's `LivingEntity`). Spectators ignore plates
//! (`isIgnoringBlockTriggers`).

use vanillite_base::{Aabb, BlockPos, Direction};
use vanillite_data::block::{BlockId, BlockState, ids};
use vanillite_data::entity::{EntityTypeId, ids as entity_ids};
use vanillite_world::entity::EntityId;
use vanillite_worldgen::behaviour::{PosExt, StateExt};

use super::{BlockBehaviour, sound_id};
use crate::level::signal::{power, powered, with_power};
use crate::level::{Level, SOUND_BLOCKS};

/// What a plate reacts to and how it sounds (`BlockSetType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// Wooden plates: every entity.
    Wood(&'static str),
    /// Stone plates: living entities.
    Stone,
    /// Weighted plates with their maximum weight.
    Weighted(i32),
}

fn kind_of(block: BlockId) -> Option<Kind> {
    Some(match block {
        b if b == ids::STONE_PRESSURE_PLATE || b == ids::POLISHED_BLACKSTONE_PRESSURE_PLATE => {
            Kind::Stone
        }
        b if b == ids::LIGHT_WEIGHTED_PRESSURE_PLATE => Kind::Weighted(15),
        b if b == ids::HEAVY_WEIGHTED_PRESSURE_PLATE => Kind::Weighted(150),
        b if b == ids::CHERRY_PRESSURE_PLATE => Kind::Wood("cherry_wood"),
        b if b == ids::BAMBOO_PRESSURE_PLATE => Kind::Wood("bamboo_wood"),
        b if b == ids::CRIMSON_PRESSURE_PLATE || b == ids::WARPED_PRESSURE_PLATE => {
            Kind::Wood("nether_wood")
        }
        b if b.name().ends_with("_pressure_plate") => Kind::Wood("wooden"),
        _ => return None,
    })
}

/// Fills the pressure plate behaviours of `block`.
pub(super) fn fill(block: BlockId, b: &mut BlockBehaviour) {
    if kind_of(block).is_none() {
        return;
    }
    b.tick = Some(tick);
    b.entity_inside = Some(entity_inside);
    b.affect_neighbors_after_removal = Some(affect_neighbors_after_removal);
    b.signal = Some(signal);
    b.direct_signal = Some(direct_signal);
}

/// `getSignalForState`.
fn signal_for_state(state: BlockState) -> i32 {
    match kind_of(state.block()) {
        Some(Kind::Weighted(_)) => power(state),
        _ => {
            if powered(state) {
                15
            } else {
                0
            }
        }
    }
}

/// `setSignalForState`.
fn set_signal_for_state(state: BlockState, signal: i32) -> BlockState {
    match kind_of(state.block()) {
        Some(Kind::Weighted(_)) => with_power(state, signal),
        _ => state.set_flag("powered", signal > 0),
    }
}

/// `getPressedTime`.
fn pressed_time(state: BlockState) -> i32 {
    match kind_of(state.block()) {
        Some(Kind::Weighted(_)) => 10,
        _ => 20,
    }
}

/// `TOUCH_AABB.move(pos)`: 14×4×14 pixels centred on the plate.
fn touch_box(pos: BlockPos) -> Aabb {
    let (x, y, z) = (f64::from(pos.x), f64::from(pos.y), f64::from(pos.z));
    Aabb::new(x + 0.0625, y, z + 0.0625, x + 0.9375, y + 0.25, z + 0.9375)
}

/// `entity instanceof LivingEntity` by type.
pub fn is_living(kind: EntityTypeId) -> bool {
    kind == entity_ids::PLAYER || kind == entity_ids::ARMOR_STAND || kind.info().category != "misc"
}

/// `entity.isIgnoringBlockTriggers()` together with `EntitySelector.NO_SPECTATORS`.
pub fn ignores_block_triggers(level: &Level, id: EntityId) -> bool {
    level
        .entities
        .player(id)
        .is_some_and(|p| p.game_mode == vanillite_config::GameMode::Spectator)
}

/// `getEntitiesOfClass(class, box, NO_SPECTATORS && !isIgnoringBlockTriggers).size()`.
pub fn count_entities(level: &Level, aabb: &Aabb, living_only: bool) -> i32 {
    let store = &level.entities.store;
    let mut count = 0;
    store.for_each_in(aabb, false, |id| {
        let Some(entity) = store.get(id) else {
            return;
        };
        if living_only && !is_living(entity.kind()) {
            return;
        }
        if !ignores_block_triggers(level, id) {
            count += 1;
        }
    });
    count
}

/// `getSignalStrength(level, pos)`.
fn signal_strength(level: &Level, state: BlockState, pos: BlockPos) -> i32 {
    let aabb = touch_box(pos);
    match kind_of(state.block()) {
        Some(Kind::Weighted(max)) => {
            let count = count_entities(level, &aabb, false).min(max);
            if count > 0 {
                let percent = count.min(max) as f32 / max as f32;
                (percent * 15.0).ceil() as i32
            } else {
                0
            }
        }
        Some(Kind::Stone) => {
            if count_entities(level, &aabb, true) > 0 {
                15
            } else {
                0
            }
        }
        _ => {
            if count_entities(level, &aabb, false) > 0 {
                15
            } else {
                0
            }
        }
    }
}

/// `tick`.
fn tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    let signal = signal_for_state(state);
    if signal > 0 {
        check_pressed(level, pos, state, signal);
    }
}

/// `entityInside`.
fn entity_inside(level: &mut Level, state: BlockState, pos: BlockPos, _entity: EntityId) {
    let signal = signal_for_state(state);
    if signal == 0 {
        check_pressed(level, pos, state, signal);
    }
}

/// The click sound of a plate (`pressurePlateClickOn` / `Off`).
fn click_sound(state: BlockState, on: bool) -> u16 {
    let suffix = if on { "click_on" } else { "click_off" };
    let name = match kind_of(state.block()) {
        Some(Kind::Stone) => "stone",
        Some(Kind::Weighted(_)) => "metal",
        Some(Kind::Wood(wood)) => wood,
        None => "stone",
    };
    let mut buf = [0u8; 64];
    let full = concat_into(&mut buf, &["block.", name, "_pressure_plate.", suffix]);
    sound_id(full)
}

/// Joins `parts` into `buf` without allocating.
fn concat_into<'a>(buf: &'a mut [u8; 64], parts: &[&str]) -> &'a str {
    let mut len = 0;
    for part in parts {
        let bytes = part.as_bytes();
        let end = (len + bytes.len()).min(buf.len());
        buf[len..end].copy_from_slice(&bytes[..end - len]);
        len = end;
    }
    std::str::from_utf8(&buf[..len]).unwrap_or_default()
}

/// `checkPressed(sourceEntity, level, pos, state, oldSignal)`.
fn check_pressed(level: &mut Level, pos: BlockPos, state: BlockState, old_signal: i32) {
    let signal = signal_strength(level, state, pos);
    let was_pressed = old_signal > 0;
    let is_pressed = signal > 0;
    if old_signal != signal {
        let new_state = set_signal_for_state(state, signal);
        level.set_block(pos, new_state, crate::level::update::CLIENTS);
        update_neighbours(level, pos, state.block());
    }
    if !is_pressed && was_pressed {
        level.play_block_sound(None, pos, click_sound(state, false), SOUND_BLOCKS, 1.0, 1.0);
    } else if is_pressed && !was_pressed {
        level.play_block_sound(None, pos, click_sound(state, true), SOUND_BLOCKS, 1.0, 1.0);
    }
    if is_pressed {
        level.schedule_block_tick(pos, state.block(), pressed_time(state));
    }
}

/// `affectNeighborsAfterRemoval`.
fn affect_neighbors_after_removal(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    moved: bool,
) {
    if !moved && signal_for_state(state) > 0 {
        update_neighbours(level, pos, state.block());
    }
}

/// `updateNeighbours`: the plate and the block below.
fn update_neighbours(level: &mut Level, pos: BlockPos, block: BlockId) {
    level.update_neighbors_at(pos, block);
    level.update_neighbors_at(pos.below(), block);
}

/// `ownSignal`.
fn signal(_level: &Level, state: BlockState, _pos: BlockPos, _direction: Direction) -> i32 {
    signal_for_state(state)
}

/// `getDirectSignal`: strongly into the block below.
fn direct_signal(_level: &Level, state: BlockState, _pos: BlockPos, direction: Direction) -> i32 {
    if direction == Direction::Up {
        signal_for_state(state)
    } else {
        0
    }
}

/// `Entity.checkInsideBlocks` in its simplest form: `entityInside` of every block the entity's
/// bounding box (shrunk by 1e-5) touches, in x, y, z order. The entity collision code calls this
/// after moving an entity; pressure plates, tripwires and detector rails react through it.
pub fn check_inside_blocks(level: &mut Level, entity: EntityId) {
    let Some(aabb) = level.entities.store.get(entity).map(|e| e.aabb()) else {
        return;
    };
    let eps = 1.0e-5;
    let min = (
        (aabb.min_x + eps).floor() as i32,
        (aabb.min_y + eps).floor() as i32,
        (aabb.min_z + eps).floor() as i32,
    );
    let max = (
        (aabb.max_x - eps).floor() as i32,
        (aabb.max_y - eps).floor() as i32,
        (aabb.max_z - eps).floor() as i32,
    );
    for x in min.0..=max.0 {
        for y in min.1..=max.1 {
            for z in min.2..=max.2 {
                let pos = BlockPos::new(x, y, z);
                if level.is_loaded(pos) {
                    level.entity_inside(pos, entity);
                }
            }
        }
    }
}

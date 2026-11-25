//! Redstone components: wire, torches, repeaters, comparators, observers, levers, buttons,
//! lamps, copper bulbs and the blocks that open and close with a signal (doors, trapdoors, fence
//! gates), plus the analog outputs that come from a block's state alone.
//!
//! Behaviours are plain functions in the table of [`super::Behaviours`]; signals go through the
//! level's `SignalGetter` ([`crate::level::signal`]). Update order, scheduled tick priorities and
//! the level random calls follow Vanilla 26.3 exactly, including its redundant updates. Redstone
//! experiments (`redstone_experiments`) are off by default in 26.3, so only the default wire
//! evaluator exists.

pub mod alternate_current;
mod components;
mod diode;
mod torch;
mod wire;

#[cfg(test)]
mod tests;

use vanillite_base::{BlockPos, Direction};
use vanillite_data::block::{BlockId, BlockState, ids};
use vanillite_world::entity::EntityId;
use vanillite_worldgen::behaviour::StateExt;

use super::BlockBehaviour;

/// Whether a block is a lightning rod of any copper stage (`#lightning_rods`).
pub fn is_lightning_rod(state: BlockState) -> bool {
    state.block().name().ends_with("lightning_rod")
}

/// `LightningRodBlock.onLightningStrike` if `pos` holds a lightning rod.
pub fn strike_lightning_rod(level: &mut crate::level::Level, pos: BlockPos) {
    let state = level.block(pos);
    if is_lightning_rod(state) {
        components::lightning_rod_strike(level, state, pos);
    }
}
use crate::level::Level;

pub use components::set_door_open;
pub use diode::set_placed_by;

/// Fills the redstone behaviours of `block`.
pub(super) fn fill(block: BlockId, b: &mut BlockBehaviour) {
    let name = block.name();
    if block == ids::REDSTONE_WIRE {
        b.on_place = Some(wire::on_place);
        b.affect_neighbors_after_removal = Some(wire::affect_neighbors_after_removal);
        b.neighbor_changed = Some(wire::neighbor_changed);
        b.signal = Some(wire::signal);
        b.direct_signal = Some(wire::direct_signal);
        b.use_without_item = Some(wire::use_without_item);
    } else if block == ids::REPEATER || block == ids::COMPARATOR {
        b.on_place = Some(diode::on_place);
        b.affect_neighbors_after_removal = Some(diode::affect_neighbors_after_removal);
        b.neighbor_changed = Some(diode::neighbor_changed);
        b.tick = Some(diode::tick);
        b.signal = Some(diode::signal);
        b.direct_signal = Some(diode::direct_signal);
        b.use_without_item = Some(diode::use_without_item);
    } else if block == ids::REDSTONE_TORCH || block == ids::REDSTONE_WALL_TORCH {
        b.on_place = Some(torch::on_place);
        b.affect_neighbors_after_removal = Some(torch::affect_neighbors_after_removal);
        b.neighbor_changed = Some(torch::neighbor_changed);
        b.tick = Some(torch::tick);
        b.signal = Some(torch::signal);
        b.direct_signal = Some(torch::direct_signal);
    } else if block == ids::OBSERVER {
        b.on_place = Some(components::observer_on_place);
        b.affect_neighbors_after_removal = Some(components::observer_removal);
        b.tick = Some(components::observer_tick);
        b.signal = Some(components::observer_signal);
        b.direct_signal = Some(components::observer_signal);
    } else if block == ids::LEVER {
        b.affect_neighbors_after_removal = Some(components::attached_removal);
        b.signal = Some(components::powered_signal);
        b.direct_signal = Some(components::attached_direct_signal);
        b.use_without_item = Some(components::lever_use);
    } else if name.ends_with("_button") {
        b.affect_neighbors_after_removal = Some(components::attached_removal);
        b.signal = Some(components::powered_signal);
        b.direct_signal = Some(components::attached_direct_signal);
        b.use_without_item = Some(components::button_use);
        b.tick = Some(components::button_tick);
    } else if block == ids::REDSTONE_BLOCK {
        b.signal = Some(components::full_signal);
    } else if block == ids::REDSTONE_LAMP {
        b.neighbor_changed = Some(components::lamp_neighbor_changed);
        b.tick = Some(components::lamp_tick);
    } else if name.ends_with("copper_bulb") {
        b.on_place = Some(components::bulb_on_place);
        b.neighbor_changed = Some(components::bulb_neighbor_changed);
        b.analog_output = Some(components::bulb_analog_output);
    } else if name.ends_with("lightning_rod") {
        b.signal = Some(components::powered_signal);
        b.direct_signal = Some(components::lightning_rod_direct_signal);
        b.tick = Some(components::lightning_rod_tick);
        b.on_place = Some(components::lightning_rod_on_place);
        b.affect_neighbors_after_removal = Some(components::lightning_rod_removal);
    } else if name.ends_with("_door") {
        b.neighbor_changed = Some(components::door_neighbor_changed);
        b.use_without_item = Some(components::door_use);
    } else if name.ends_with("_trapdoor") {
        b.neighbor_changed = Some(components::trapdoor_neighbor_changed);
        b.use_without_item = Some(components::trapdoor_use);
    } else if block == ids::COMPOSTER {
        b.on_place = Some(components::composter_on_place);
        b.tick = Some(components::composter_tick);
    } else if name.ends_with("_fence_gate") {
        b.neighbor_changed = Some(components::fence_gate_neighbor_changed);
        b.use_without_item = Some(components::fence_gate_use);
    }
    if let Some(analog) = diode::state_analog_output(block) {
        b.analog_output = Some(analog);
    }
}

/// The sounds and rules of a `BlockSetType` (doors, trapdoors, buttons) or `WoodType` (fence
/// gates), found from the block's name.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BlockSet {
    /// `canOpenByHand`.
    pub can_open_by_hand: bool,
    /// The sound family: `wooden`, `iron`, `copper`, `cherry_wood`, `nether_wood`,
    /// `bamboo_wood`, `stone`.
    family: &'static str,
}

impl BlockSet {
    /// The set of a door, trapdoor, button or fence gate.
    pub(crate) fn of(block: BlockId) -> BlockSet {
        let name = block.name().trim_start_matches("minecraft:");
        let wood = |family| BlockSet {
            can_open_by_hand: true,
            family,
        };
        if name.contains("copper") {
            BlockSet {
                can_open_by_hand: true,
                family: "copper",
            }
        } else if name.starts_with("iron_") {
            BlockSet {
                can_open_by_hand: false,
                family: "iron",
            }
        } else if name.starts_with("stone_") || name.starts_with("polished_blackstone_") {
            BlockSet {
                can_open_by_hand: true,
                family: "stone",
            }
        } else if name.starts_with("cherry_") {
            wood("cherry_wood")
        } else if name.starts_with("crimson_") || name.starts_with("warped_") {
            wood("nether_wood")
        } else if name.starts_with("bamboo_") {
            wood("bamboo_wood")
        } else {
            wood("wooden")
        }
    }

    /// `doorOpen` / `doorClose`.
    fn door_sound(self, open: bool) -> u16 {
        let family = if self.family == "stone" {
            "iron"
        } else {
            self.family
        };
        sound(&[
            "block.",
            family,
            "_door.",
            if open { "open" } else { "close" },
        ])
    }

    /// `trapdoorOpen` / `trapdoorClose`.
    fn trapdoor_sound(self, open: bool) -> u16 {
        let family = if self.family == "stone" {
            "iron"
        } else {
            self.family
        };
        sound(&[
            "block.",
            family,
            "_trapdoor.",
            if open { "open" } else { "close" },
        ])
    }

    /// `buttonClickOn` / `buttonClickOff`.
    fn button_sound(self, on: bool) -> u16 {
        let family = match self.family {
            "iron" | "copper" => "stone",
            family => family,
        };
        sound(&[
            "block.",
            family,
            "_button.",
            if on { "click_on" } else { "click_off" },
        ])
    }

    /// `fenceGateOpen` / `fenceGateClose` of the wood type.
    fn fence_gate_sound(self, open: bool) -> u16 {
        let prefix = match self.family {
            "cherry_wood" => "block.cherry_wood_fence_gate.",
            "nether_wood" => "block.nether_wood_fence_gate.",
            "bamboo_wood" => "block.bamboo_wood_fence_gate.",
            _ => "block.fence_gate.",
        };
        sound(&[prefix, if open { "open" } else { "close" }])
    }
}

/// The sound event id of the concatenated name parts.
fn sound(parts: &[&str]) -> u16 {
    let mut name = [0u8; 80];
    let mut len = 0;
    for part in parts {
        let end = len + part.len();
        if end > name.len() {
            return 0;
        }
        name[len..end].copy_from_slice(part.as_bytes());
        len = end;
    }
    super::sound_id(std::str::from_utf8(&name[..len]).unwrap_or_default())
}

/// `FaceAttachedHorizontalDirectionalBlock.getConnectedDirection(state)`: the direction from
/// the block to what it is attached to, reversed (ceiling: down, floor: up, wall: facing).
pub(crate) fn connected_direction(state: BlockState) -> Direction {
    match state.value("face") {
        Some("ceiling") => Direction::Down,
        Some("floor") => Direction::Up,
        _ => state.direction("facing").unwrap_or(Direction::North),
    }
}

/// `Direction.fromYRot(yaw)`: the horizontal direction a player looks in.
pub(crate) fn direction_from_yaw(yaw: f32) -> Direction {
    match ((f64::from(yaw) / 90.0 + 0.5).floor() as i32) & 3 {
        0 => Direction::South,
        1 => Direction::West,
        2 => Direction::North,
        _ => Direction::East,
    }
}

/// `level.gameEvent(entity, event, pos)`: the vibration of a redstone component
/// (`block_activate`, `block_deactivate`, `block_open` or `block_close`).
pub(crate) fn game_event(level: &mut Level, event: &str, pos: BlockPos, source: Option<EntityId>) {
    let events = crate::level::game_event::events();
    let id = match event {
        "block_activate" => events.block_activate,
        "block_deactivate" => events.block_deactivate,
        "block_open" => events.block_open,
        _ => events.block_close,
    };
    level.game_event_at(source, id, pos);
}

/// `level.getRandom().nextFloat() * 0.1F + 0.9F`: the pitch of door and gate sounds.
fn door_pitch(level: &mut Level) -> f32 {
    use vanillite_base::random::RandomSource;
    level.random().next_float() * 0.1 + 0.9
}

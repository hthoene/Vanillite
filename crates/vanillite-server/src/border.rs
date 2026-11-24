//! The world border of a world on the server: loaded from and saved as the dimension's saved
//! data, and its packets (`PlayerList.addWorldborderListener`, `sendLevelInfo`).

use vanillite_game::level::border::{Settings, WorldBorder};
use vanillite_game::player::join::LevelPackets;
use vanillite_protocol::Packet;
use vanillite_protocol::packets::play::clientbound::{InitializeBorder, TickingState, TickingStep};
use vanillite_storage::saved_data::SavedDataStore;
use vanillite_world::entity::EntityId;

use crate::global::Global;

/// The id of the saved data (`WorldBorder.TYPE`).
pub(crate) const SAVED_DATA: &str = "minecraft:world_border";

/// `getWorldBorder` at start: the saved settings applied at `game_time`, with the server's
/// `max-world-size`.
pub(crate) fn load(data: &SavedDataStore, game_time: i64, max_world_size: i32) -> WorldBorder {
    let settings = match data.read(SAVED_DATA) {
        Ok(Some(compound)) => Settings::from_nbt(&compound),
        Ok(None) => Settings::default(),
        Err(e) => {
            log::error!("Cannot read {SAVED_DATA}: {e}");
            Settings::default()
        }
    };
    let mut border = WorldBorder::new(settings, game_time);
    border.set_absolute_max_size(max_world_size);
    border
}

/// `ClientboundInitializeBorderPacket`.
pub(crate) fn initialize(border: &WorldBorder) -> InitializeBorder {
    InitializeBorder {
        center_x: border.center_x(),
        center_z: border.center_z(),
        old_size: border.size(),
        new_size: border.lerp_target(),
        lerp_time: border.lerp_time(),
        absolute_max_size: border.absolute_max_size(),
        warning_blocks: border.warning_blocks(),
        warning_time: border.warning_time(),
    }
}

/// The tick state packets (`ClientboundTickingStatePacket.from`, `ClientboundTickingStepPacket.from`).
pub(crate) fn ticking(global: &Global) -> (TickingState, TickingStep) {
    let rate = global.tick_rate();
    (
        TickingState {
            tick_rate: rate.tick_rate(),
            is_frozen: rate.is_frozen(),
        },
        TickingStep {
            tick_steps: i32::try_from(rate.frozen_ticks_to_run()).unwrap_or(i32::MAX),
        },
    )
}

/// What `sendLevelInfo` sends of the border and the tick state.
pub(crate) fn level_packets(border: &WorldBorder, global: &Global) -> LevelPackets {
    let (ticking, step) = ticking(global);
    LevelPackets {
        border: initialize(border),
        ticking,
        step,
    }
}

/// `broadcastAll(packet, dimension)`: to the players of one world.
pub(crate) fn broadcast_world<'a>(
    players: impl Iterator<Item = (EntityId, &'a vanillite_game::player::PlayerData)>,
    packet: &impl Packet,
) {
    let shared = vanillite_protocol::SharedPacket::of(packet);
    for (_, player) in players {
        player.connection.sink.send_shared(&shared);
    }
}

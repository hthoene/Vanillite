//! Server-wide state that worlds can read and change through narrow methods (architecture 4.7).

use vanillite_game::player::chat::ChatSettings;
use vanillite_world::entity::NetworkIds;

use std::sync::{Arc, OnceLock, Weak};

use crate::access::Access;
use crate::clock::Clock;
use crate::player_list::PlayerList;
use crate::server::ServerHandle;
use crate::world::{ServerWorld, WorldId};

/// Server-wide state shared with all worlds.
#[derive(Debug)]
pub struct Global {
    /// The monotonic server clock.
    pub clock: Clock,
    /// Online players.
    pub players: PlayerList,
    /// Network entity ids of all worlds.
    pub network_ids: Arc<NetworkIds>,
    /// Secure chat and spam settings.
    pub chat: ChatSettings,
    /// Network ids of the chat types.
    pub chat_types: ChatTypes,
    /// Operators and whitelist.
    pub access: Access,
    /// A handle to the server for work on other worlds and `/stop`; set once the server runs.
    pub handle: OnceLock<ServerHandle>,
    /// The command tree.
    pub(crate) commands: crate::commands::CommandDispatcher,
    /// `online-mode`.
    pub(crate) online_mode: bool,
    /// Game time, clocks, game rules, weather and the survival registries.
    pub(crate) level: crate::level::LevelState,
    /// The world seed (`/seed`).
    pub(crate) seed: i64,
    /// Entries of data-driven registries commands refer to (timelines, world clocks,
    /// enchantments).
    pub(crate) registries: std::collections::BTreeMap<String, Vec<String>>,
    /// Tool rules, loot tables and item data of the block and item handlers.
    pub(crate) game: Arc<vanillite_game::player::play::GameData>,
    /// `spawn-protection` radius in blocks.
    pub(crate) spawn_protection: i32,
    /// Scoreboard, command storage, functions and tags.
    pub(crate) systems: crate::systems::GameSystems,
    /// This value itself, for work handed to other worlds.
    pub(crate) this: Weak<Global>,
}

impl Global {
    /// A shared reference to this value (for tasks on other worlds).
    pub fn arc(&self) -> Option<Arc<Global>> {
        self.this.upgrade()
    }

    /// Runs `task` on another world between its ticks (cross-world command effects). Dropped
    /// before the server runs.
    pub fn run_in_world(
        &self,
        world: WorldId,
        task: impl FnOnce(&mut ServerWorld) + Send + 'static,
    ) {
        if let Some(handle) = self.handle.get() {
            handle.run_in_world(world, task);
        }
    }

    /// Asks the server to stop after the current tick (`/stop`).
    pub fn stop(&self) {
        if let Some(handle) = self.handle.get() {
            handle.stop();
        }
    }
}

/// Network ids of the chat types in the `minecraft:chat_type` registry the clients received.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChatTypes {
    /// `minecraft:chat`.
    pub chat: i32,
    /// `minecraft:say_command`.
    pub say_command: i32,
    /// `minecraft:msg_command_incoming`.
    pub msg_incoming: i32,
    /// `minecraft:msg_command_outgoing`.
    pub msg_outgoing: i32,
    /// `minecraft:emote_command`.
    pub emote_command: i32,
}

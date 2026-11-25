//! The server: startup, slot wiring, tick loop, world threads, compute pool, autosave, shutdown,
//! watchdog and metrics.
//!
//! Threads (architecture section 3): the server thread runs the tick loop and the global
//! phases; in parallel mode every world has its own tick thread; one compute pool does all
//! background and parallel-phase work; the watchdog watches the tick deadline.

mod access;
mod blocks;
mod border;
mod bossbars;
mod chat;
mod chunk_tickets;
mod clock;
mod command_state;
mod commands;
pub mod compute;
pub mod eula;
mod global;
mod level;
mod mobs;
mod net;
mod packs;
mod player_list;
mod players;
mod plugins;
mod progress;
mod scoreboard;
mod server;
pub mod slot;
mod survival;
mod systems;
mod test_bot;
mod tick_rate;
mod watchdog;
mod world;
mod world_format;
mod world_setup;
mod world_updates;

pub use blocks::{ItemScratch, game_data, tick_item};
pub use clock::Clock;
pub use commands::run_command_on_level;
pub use compute::{Compute, ComputePool, InlineScheduler, Job, Scheduler, Scope};
pub use global::Global;
pub use level::{load_block_facts, load_damage_types};
pub use net::VanillaPack;
pub use player_list::{PlayerEntry, PlayerList, Uuid};
pub use server::{DIMENSIONS, GlobalTask, Server, ServerBuilder, ServerHandle, StartError};
pub use slot::{Origin, Slot, SlotError};
pub use tick_rate::TickRateManager;
pub use vanillite_net::{VirtualConnection, VirtualOpener};
pub use watchdog::{Heartbeat, WatchState, Watchdog};
pub use world::{ServerWorld, TickContext, WorldId, WorldTask};
pub use world_format::{convert_world, export_world, upgrade_world};

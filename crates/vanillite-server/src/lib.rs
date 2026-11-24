//! The server: startup, slot wiring, tick loop, world threads, compute pool, autosave, shutdown,
//! watchdog and metrics.
//!
//! Threads (architecture section 3): the server thread runs the tick loop and the global
//! phases; in parallel mode every world has its own tick thread; one compute pool does all
//! background and parallel-phase work; the watchdog watches the tick deadline.

mod clock;
pub mod compute;
mod global;
mod player_list;
mod server;
pub mod slot;
mod tick_rate;
mod watchdog;
mod world;

pub use clock::Clock;
pub use compute::{Compute, ComputePool, InlineScheduler, Job, Scheduler, Scope};
pub use global::Global;
pub use player_list::{PlayerEntry, PlayerList, Uuid};
pub use server::{DIMENSIONS, GlobalTask, Server, ServerBuilder, ServerHandle, StartError};
pub use slot::{Origin, Slot, SlotError};
pub use tick_rate::TickRateManager;
pub use watchdog::{Heartbeat, WatchState, Watchdog};
pub use world::{ServerWorld, TickContext, WorldId, WorldTask};

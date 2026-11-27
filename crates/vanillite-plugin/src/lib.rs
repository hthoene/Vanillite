//! Plugin SDK: a safe Rust API over the Vanillite plugin C ABI.
//!
//! A plugin is a `cdylib` with a `plugin.toml` next to it. Its main type implements [`Plugin`]
//! and is exported with [`export_plugin!`]:
//!
//! ```no_run
//! use vanillite_plugin::prelude::*;
//!
//! struct Hello;
//!
//! impl Plugin for Hello {
//!     fn load(registrar: &mut Registrar<Self>) -> Result<Self, Error> {
//!         registrar.on_join(priority::NORMAL, |_, ctx, event| {
//!             let _ = ctx.server().send_message(event.player(), format!("Welcome, {}!", event.name()));
//!         });
//!         registrar.command(
//!             Command::literal("hello").executes(|_, ctx, invocation| {
//!                 invocation.reply(ctx, "Hello!");
//!                 Ok(1)
//!             }),
//!         );
//!         Ok(Hello)
//!     }
//! }
//!
//! vanillite_plugin::export_plugin!(Hello);
//! ```
//!
//! # Model
//!
//! - **Lifecycle**: [`Plugin::load`] registers events, commands and providers (registries are
//!   writable only there); [`Plugin::enable`] runs when the worlds exist, [`Plugin::disable`]
//!   at shutdown. Plugins are never unloaded; in development mode a reload loads the new build
//!   next to the old one and moves the state with [`Plugin::save_state`] and
//!   [`Plugin::restore_state`].
//! - **Threads**: there is no main thread. Event handlers and commands run on the thread that
//!   owns the player or world (one per dimension when dimensions tick in parallel). By default
//!   (`threading = "serialized"` in `plugin.toml`) the host never calls a plugin on two threads
//!   at once. Work on another owner is scheduled there with [`Server::schedule`]; slow work
//!   (I/O) runs in [`Server::run_async`].
//! - **Contexts**: a handler gets a [`Ctx`] that is valid only during the call. World access
//!   (blocks, positions) goes through it and only works on the owner of that world; everything
//!   else is on [`Server`], which may be used from any thread and kept.
//! - **Panics** are contained: every entry point catches them. A plugin that panics in a handler
//!   is disabled and the server keeps running; a panicking provider stops the server in order.
//!   Plugins must be built with `panic = "unwind"` (the default); the host refuses others.
//! - **Identity**: players and entities are identified by [`Uuid`], worlds by [`World`],
//!   blocks by their state text (`minecraft:oak_log[axis=y]`), never by numeric protocol ids,
//!   so plugins keep working across Minecraft updates. Providers are the exception: they work
//!   with numeric block state ids resolved through [`Server::block_state_id`].

mod command;
mod events;
mod ffi;
mod game;
mod generator;
mod registrar;
mod server;
mod types;

pub use command::{ArgType, Command, Invocation, Sender, SuggestRequest};
pub use events::{
    AdvancementDone, BlockChange, CustomEvent, EntityDamage, EntityDeath, GameModeChange, Interact,
    Login, LoginReason, PlayerDeath, PlayerJoin, PlayerLeave, PlayerText, PlayerWorld,
    PlayersMoved, PluginMessage,
};
pub use game::{ChunkPopulator, GameEvent, Goal, GoalAction, GoalMob};
pub use generator::{ChunkBlocks, ChunkGenerator, ChunkGeneratorProvider, WorldInfo};
pub use registrar::{Permission, PermissionDefault, Registrar};
pub use server::{
    BossBar, Ctx, Environment, Owner, RayHit, Server, TaskId, WorldBorder, WorldCreator,
    WorldDetails,
};
pub use types::{
    AccessList, BlockBox, BlockPos, DataValue, Effect, Error, GameMode, Hand, Location, NO_BLOCK,
    Particle, PlayerInfo, ServerStats, SetMode, Sound, Text, Uuid, Vec3, Weather, World,
};
pub use vanillite_abi as abi;
pub use vanillite_abi::priority;

/// The usual imports of a plugin.
pub mod prelude {
    pub use crate::{
        ArgType, BlockBox, BlockPos, Command, Ctx, DataValue, Effect, Error, GameMode, Location,
        Owner, Particle, Permission, PermissionDefault, Plugin, Registrar, Server, SetMode, Sound,
        Text, Uuid, Vec3, World, priority,
    };
}

/// The main type of a plugin.
pub trait Plugin: Send + Sized + 'static {
    /// Creates the plugin and registers its events, commands and providers. Called once per
    /// loaded build, before the worlds exist.
    fn load(registrar: &mut Registrar<Self>) -> Result<Self, Error>;

    /// Starts the plugin after the worlds exist, on the server thread.
    fn enable(&mut self, _ctx: &mut Ctx<'_>) {}

    /// Stops the plugin at shutdown or before a development reload.
    fn disable(&mut self, _ctx: &mut Ctx<'_>) {}

    /// Development reload: the state the next build takes over (any format the plugin likes).
    fn save_state(&mut self) -> Vec<u8> {
        Vec::new()
    }

    /// Development reload: takes over the state of the previous build, after [`Plugin::load`]
    /// and before [`Plugin::enable`].
    fn restore_state(&mut self, _state: &[u8]) {}
}

/// Exports a [`Plugin`] type as the library's entry point (`vanillite_plugin_abi`).
#[macro_export]
macro_rules! export_plugin {
    ($plugin:ty) => {
        /// The plugin's entry point.
        #[unsafe(no_mangle)]
        pub extern "C" fn vanillite_plugin_abi() -> *const $crate::abi::PluginInfo {
            $crate::__private::info::<$plugin>()
        }
    };
}

/// Logs a message at `info` level under the plugin's name (`format!` arguments).
#[macro_export]
macro_rules! info {
    ($($arg:tt)*) => { $crate::log($crate::abi::log_level::INFO, &format!($($arg)*)) };
}

/// Logs a message at `warn` level under the plugin's name (`format!` arguments).
#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => { $crate::log($crate::abi::log_level::WARN, &format!($($arg)*)) };
}

/// Logs a message at `error` level under the plugin's name (`format!` arguments).
#[macro_export]
macro_rules! error {
    ($($arg:tt)*) => { $crate::log($crate::abi::log_level::ERROR, &format!($($arg)*)) };
}

/// Logs a message at `debug` level under the plugin's name (`format!` arguments).
#[macro_export]
macro_rules! debug {
    ($($arg:tt)*) => { $crate::log($crate::abi::log_level::DEBUG, &format!($($arg)*)) };
}

/// Logs a message under the plugin's name (`level` from [`abi::log_level`]). Before `load` the
/// message goes to standard error.
pub fn log(level: u32, message: &str) {
    match ffi::host() {
        // SAFETY: the host table lives as long as the process; the string is valid for the call.
        Some(host) => unsafe { (host.api.log)(host.id, level, abi::Str::new(message)) },
        None => eprintln!("{message}"),
    }
}

#[doc(hidden)]
pub mod __private {
    pub use crate::ffi::info;
}

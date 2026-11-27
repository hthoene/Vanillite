//! Native plugin host: loading and validation, event bus, handle tables, lifecycle, crash attribution and slot provider adapters.
//!
//! The server owns one [`PluginHost`]. It
//!
//! 1. discovers and validates the packages in `plugins/` and calls their `load` before the
//!    worlds exist ([`PluginHost::load`]), so plugins can register commands, events and slot
//!    providers;
//! 2. adds the plugins' commands to the command tree ([`PluginHost::register_commands`]) and
//!    hands their providers to the slots ([`PluginHost::take_chunk_generator`]);
//! 3. enables them once the server runs ([`PluginHost::enable_all`]), fires events at the
//!    server's hook points, runs scheduled tasks on their owners, and disables them at
//!    shutdown.
//!
//! The server is reached through two narrow traits it implements: [`ServerAccess`]
//! (thread-safe, server-wide) and [`WorldAccess`] (one world, borrowed by its owner for a
//! call). Everything that crosses into plugin code goes through one guard: the plugin
//! is recorded for crash reports, serialized plugins are locked, re-entrant calls are refused,
//! and a contained panic disables the plugin.

mod api;
mod commands;
pub mod crash;
mod events;
mod generator;
mod goals;
pub mod loader;
mod packets;
mod scheduler;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, RwLock, Weak};

use vanillite_abi::{self as abi, Status, status};

pub use commands::{Arg, ArgValue, SUGGEST_PREFIX, Sender};
pub use events::{GameEvent, InteractTarget, LoginOutcome, NewText, PlayerDeath, TextDecision};
pub use generator::PluginGeneratorProvider;
pub use generator::PluginPopulator;
pub use goals::PluginGoal;
pub use loader::{Manifest, Threading};

/// A message for players.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Text<'a> {
    /// Plain text.
    Plain(&'a str),
    /// A JSON text component.
    Json(&'a str),
}

impl<'a> Text<'a> {
    /// The text as given (plain or JSON).
    pub fn as_str(&self) -> &'a str {
        match *self {
            Text::Plain(s) | Text::Json(s) => s,
        }
    }
}

/// A title for a player.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Title<'a> {
    /// The title; `None` for none.
    pub title: Option<Text<'a>>,
    /// The subtitle; `None` for none.
    pub subtitle: Option<Text<'a>>,
    /// Ticks to fade in, stay and fade out.
    pub times: [i32; 3],
}

/// A sound for one player ([`vanillite_abi::SoundSpec`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sound<'a> {
    /// The sound event, e.g. `minecraft:entity.experience_orb.pickup`.
    pub sound: &'a str,
    /// The category, an index into `/playsound`'s sources ([`vanillite_abi::sound_source`]).
    pub source: u32,
    /// Where it plays.
    pub pos: [f64; 3],
    /// Volume.
    pub volume: f32,
    /// Pitch.
    pub pitch: f32,
    /// Seed of the variant choice.
    pub seed: i64,
}

/// Particles for one player ([`vanillite_abi::ParticleSpec`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Particles<'a> {
    /// The particle type, e.g. `minecraft:dust`.
    pub particle: &'a str,
    /// Its options (empty for types without).
    pub options: vanillite_nbt::Compound,
    /// Where they appear.
    pub pos: [f64; 3],
    /// Spread or direction.
    pub delta: [f64; 3],
    /// Speed.
    pub speed: f32,
    /// Count.
    pub count: i32,
    /// Visible up to 512 blocks away.
    pub force: bool,
}

/// What the server knows about an online player.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlayerInfo {
    /// Its world.
    pub world: u32,
    /// Its game mode ([`vanillite_abi::game_mode`]).
    pub game_mode: u32,
    /// Its operator level, 0 for none.
    pub op_level: u32,
    /// Its latency in milliseconds.
    pub ping: i32,
    /// Its IP address.
    pub address: String,
}

/// Tick statistics ([`vanillite_abi::ServerStats`]).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ServerStats {
    /// The tick count.
    pub tick: u64,
    /// Average milliseconds per tick.
    pub mspt: f64,
    /// Ticks per second.
    pub tps: f64,
    /// The tick rate.
    pub tick_rate: f64,
    /// Online players.
    pub online: u32,
    /// The player limit.
    pub max_players: u32,
}

/// A change or query of the Vanilla scoreboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScoreboardOp<'a> {
    /// Adds an objective.
    AddObjective {
        /// Its name.
        name: &'a str,
        /// Its criterion, e.g. `dummy`.
        criteria: &'a str,
        /// Its display name.
        display: Text<'a>,
    },
    /// Removes an objective.
    RemoveObjective {
        /// Its name.
        name: &'a str,
    },
    /// Shows an objective in a display slot, or clears the slot.
    Display {
        /// The slot ([`vanillite_abi::display_slot`]).
        slot: u32,
        /// The objective; `None` clears the slot.
        objective: Option<&'a str>,
    },
    /// Reads a score ([`ScoreReply::Score`]).
    GetScore {
        /// The objective.
        objective: &'a str,
        /// The holder.
        holder: &'a str,
    },
    /// Sets a score.
    SetScore {
        /// The objective.
        objective: &'a str,
        /// The holder.
        holder: &'a str,
        /// The value.
        value: i32,
    },
    /// Resets a score.
    ResetScore {
        /// The objective; `None` for all.
        objective: Option<&'a str>,
        /// The holder.
        holder: &'a str,
    },
    /// Adds a team.
    AddTeam {
        /// Its name.
        name: &'a str,
    },
    /// Removes a team.
    RemoveTeam {
        /// Its name.
        name: &'a str,
    },
    /// Puts a holder into a team.
    JoinTeam {
        /// The team.
        team: &'a str,
        /// The holder.
        holder: &'a str,
    },
    /// Takes a holder out of its team.
    LeaveTeam {
        /// The holder.
        holder: &'a str,
    },
    /// Changes a team option as `/team modify`.
    ModifyTeam {
        /// The team.
        team: &'a str,
        /// The option, e.g. `prefix`.
        option: &'a str,
        /// The value.
        value: &'a str,
    },
    /// The team of a holder ([`ScoreReply::Team`]).
    TeamOf {
        /// The holder.
        holder: &'a str,
    },
}

/// The answer of a [`ScoreboardOp`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScoreReply {
    /// Changed.
    Done,
    /// A score.
    Score(i32),
    /// A team name.
    Team(String),
}

/// A change of a boss bar; keys are full resource locations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BossBarOp<'a> {
    /// Creates a boss bar.
    Create {
        /// Its key.
        key: &'a str,
        /// Its name.
        name: Text<'a>,
    },
    /// Removes a boss bar.
    Remove {
        /// Its key.
        key: &'a str,
    },
    /// Changes a property ([`vanillite_abi::bossbar_prop`]).
    Set {
        /// Its key.
        key: &'a str,
        /// The property.
        prop: u32,
        /// The value.
        value: i64,
    },
    /// Renames it.
    SetName {
        /// Its key.
        key: &'a str,
        /// The name.
        name: Text<'a>,
    },
    /// Shows or hides it for a player.
    Player {
        /// Its key.
        key: &'a str,
        /// The player.
        player: u128,
        /// Show or hide.
        show: bool,
    },
}

/// A query or change of an access list ([`vanillite_abi::access_list`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccessOp<'a> {
    /// The list.
    pub list: u32,
    /// The player.
    pub player: u128,
    /// The player's name, or the IP address for `IP_BAN`.
    pub name: &'a str,
    /// `None` to query; `Some((add, reason))` to change.
    pub change: Option<(bool, &'a str)>,
}

/// A new extra world (`world_create`, Bukkit's `WorldCreator`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorldCreator<'a> {
    /// Name and folder.
    pub name: &'a str,
    /// One of [`vanillite_abi::environment`].
    pub environment: u32,
    /// The generator name or dimension key; empty for `normal`.
    pub generator: &'a str,
    /// `None`: random.
    pub seed: Option<i64>,
    /// Whether structures are generated.
    pub structures: bool,
}

/// What a world is (`world_info`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorldDescription {
    /// One of [`vanillite_abi::environment`].
    pub environment: u32,
    /// Flags from [`vanillite_abi::world_flag`].
    pub flags: u32,
    /// Its seed.
    pub seed: i64,
    /// Its folder.
    pub folder: String,
}

/// Where an entity is.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Location {
    /// The world.
    pub world: u32,
    /// The position.
    pub pos: [f64; 3],
    /// Yaw in degrees.
    pub yaw: f32,
    /// Pitch in degrees.
    pub pitch: f32,
}

/// Server-wide services for plugins, callable from any thread.
pub trait ServerAccess: Send + Sync {
    /// UUIDs of the online players.
    fn online_players(&self) -> Vec<u128>;
    /// A player's name.
    fn player_name(&self, player: u128) -> Option<String>;
    /// A player's world index.
    fn player_world(&self, player: u128) -> Option<u32>;
    /// Sends a system message; `false` when the player is offline.
    fn send_message(&self, player: u128, text: Text<'_>) -> bool;
    /// Disconnects a player; `false` when offline.
    fn kick(&self, player: u128, reason: Text<'_>) -> bool;
    /// Sets the header and footer of a player's player list; `false` when offline.
    fn set_tab_list(&self, player: u128, header: Text<'_>, footer: Text<'_>) -> bool;
    /// Sends a system message to everyone and the log.
    fn broadcast(&self, text: Text<'_>);
    /// Whether a player is an operator.
    fn is_operator(&self, player: u128) -> bool;
    /// The dimension key of a world.
    fn world_name(&self, world: u32) -> Option<String>;
    /// The numeric id of a block state text.
    fn block_state_id(&self, state: &str) -> Option<u32>;
    /// The text of a numeric block state id.
    fn block_state_name(&self, state: u32) -> Option<String>;
    /// A `vanillite.toml` rule as TOML text.
    fn rule(&self, key: &str) -> Option<String>;
    /// Queues a console command.
    fn run_command(&self, line: &str);
    /// Runs a job on a background thread.
    fn spawn_background(&self, job: Box<dyn FnOnce() + Send>);
    /// Saves and stops the server (a provider failed).
    fn shutdown(&self, reason: &str);
    /// Shows a text above a player's hotbar; `false` when offline.
    fn send_action_bar(&self, _player: u128, _text: Text<'_>) -> bool {
        false
    }
    /// Shows a title to a player; `false` when offline.
    fn send_title(&self, _player: u128, _title: &Title<'_>) -> bool {
        false
    }
    /// Plays a sound to a player; `false` when offline or the sound is no resource location.
    fn play_sound(&self, _player: u128, _sound: &Sound<'_>) -> bool {
        false
    }
    /// Stops sounds (`None`: any) of a category (`None`: all) for a player; `false` when
    /// offline.
    fn stop_sound(&self, _player: u128, _sound: Option<&str>, _source: Option<u32>) -> bool {
        false
    }
    /// Shows particles to a player: [`vanillite_abi::status::INVALID_ARGUMENT`] for an
    /// unknown particle or bad options, [`vanillite_abi::status::NOT_FOUND`] when offline.
    fn spawn_particle(&self, _player: u128, _particles: &Particles<'_>) -> Result<(), Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }
    /// Sends a player to another server; `false` when offline.
    fn transfer(&self, _player: u128, _host: &str, _port: u16) -> bool {
        false
    }
    /// Sends a plugin message to a player; `false` when offline.
    fn send_plugin_message(&self, _player: u128, _channel: &str, _data: &[u8]) -> bool {
        false
    }
    /// Queues a play packet body (`id` + fields) for a player, past the packet listeners with
    /// `skip_listeners`; `false` when offline.
    fn send_packet(&self, _player: u128, _body: Vec<u8>, _skip_listeners: bool) -> bool {
        false
    }
    /// Feeds a serverbound play packet body into a player's connection as if the client sent
    /// it, past the packet listeners with `skip_listeners`. Errors are ABI status codes.
    fn receive_packet(
        &self,
        _player: u128,
        _body: Vec<u8>,
        _skip_listeners: bool,
    ) -> Result<(), i32> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }
    /// What the server knows about an online player.
    fn player_info(&self, _player: u128) -> Option<PlayerInfo> {
        None
    }
    /// Changes or reads the Vanilla scoreboard; changes reach the players with the next
    /// scoreboard flush.
    fn scoreboard(&self, _op: ScoreboardOp<'_>) -> Result<ScoreReply, Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }
    /// Changes a custom boss bar.
    fn boss_bar(&self, _op: BossBarOp<'_>) -> Result<(), Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }
    /// Tick statistics.
    fn stats(&self) -> Option<ServerStats> {
        None
    }
    /// Replaces the message of the day of the server list.
    fn set_motd(&self, _motd: Text<'_>) -> bool {
        false
    }
    /// Reads or changes an access list; returns whether the player is (or was changed) on it.
    fn access(&self, _op: AccessOp<'_>) -> Result<bool, Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }
    /// The time of day of a world.
    fn world_time(&self, _world: u32) -> Option<i64> {
        None
    }
    /// Sets the time of day of a world.
    fn set_world_time(&self, _world: u32, _time: i64) -> Result<(), Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }
    /// Sets the weather ([`vanillite_abi::weather`]) for a duration (-1: random).
    fn set_weather(&self, _world: u32, _weather: u32, _duration: i32) -> Result<(), Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }
    /// A game rule's value by name.
    fn game_rule(&self, _name: &str) -> Option<String> {
        None
    }
    /// Sets a game rule.
    fn set_game_rule(&self, _name: &str, _value: &str) -> Result<(), Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }
    /// A component of an item (`{id,count,components}`), defaults of its type included;
    /// `None` when it has none. [`vanillite_abi::status::INVALID_ARGUMENT`] for an unknown
    /// item or component.
    fn item_component(
        &self,
        _item: &vanillite_nbt::Compound,
        _component: &str,
    ) -> Result<Option<vanillite_nbt::Tag>, Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }
    /// The item with a component set (`None`: removed), the value checked as Vanilla's codec
    /// reads it.
    fn item_set_component(
        &self,
        _item: &vanillite_nbt::Compound,
        _component: &str,
        _value: Option<&vanillite_nbt::Tag>,
    ) -> Result<vanillite_nbt::Compound, Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }
    /// A plugin was disabled: the server drops what it holds for it (chunk tickets).
    fn plugin_disabled(&self, _plugin: &str) {}
    /// Creates (or loads) an extra world in the next global phase; its world index.
    fn create_world(&self, _world: &WorldCreator<'_>) -> Result<u32, Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }
    /// Loads the extra world of a folder in the next global phase; its world index.
    fn load_world(&self, _name: &str) -> Result<u32, Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }
    /// Unloads an extra world in the next global phase.
    fn unload_world(&self, _world: u32, _save: bool) -> Result<(), Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }
    /// The number of world indices, loaded or not.
    fn world_count(&self) -> u32 {
        0
    }
    /// What a world is.
    fn world_info(&self, _world: u32) -> Option<WorldDescription> {
        None
    }
    /// Moves an online player to a place in any loaded world.
    fn move_player(&self, _player: u128, _to: Location) -> Result<(), Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }
}

/// A plugin menu to open (`menu_open`).
#[derive(Clone, Copy, Debug)]
pub struct Menu<'a> {
    /// The menu type, e.g. `minecraft:generic_9x3`.
    pub menu_type: &'a str,
    /// The title.
    pub title: Text<'a>,
    /// The items as an SNBT list of stacks with `Slot`; empty for none.
    pub items: &'a str,
}

/// One world, borrowed by its owner for the duration of a plugin call.
pub trait WorldAccess {
    /// The world's index.
    fn id(&self) -> u32;
    /// The block state at a position as text; `None` outside loaded chunks.
    fn block(&self, pos: [i32; 3]) -> Option<String>;
    /// Sets a block state from text with Vanilla's updates.
    fn set_block(&mut self, pos: [i32; 3], state: &str) -> Result<(), Status>;
    /// A player's position in this world.
    fn player_position(&self, player: u128) -> Option<[f64; 3]>;
    /// Whether an entity is in this world.
    fn has_entity(&self, entity: u128) -> bool;
    /// The numeric block state id at a position; `None` outside loaded chunks.
    fn block_id(&self, pos: [i32; 3]) -> Option<u32>;
    /// Sets a block state by numeric id; returns whether the block changed.
    /// [`vanillite_abi::status::INVALID_ARGUMENT`] for an unknown id,
    /// [`vanillite_abi::status::NOT_FOUND`] outside loaded chunks.
    fn set_block_id(&mut self, pos: [i32; 3], state: u32, mode: SetMode) -> Result<bool, Status>;

    /// The block state ids of a box into `out` (its volume, in [`BlockBox`] order);
    /// [`vanillite_abi::limits::NO_BLOCK`] outside loaded chunks.
    fn blocks(&self, area: BlockBox, out: &mut [u32]) {
        for (pos, slot) in area.positions().zip(out) {
            *slot = self
                .block_id(pos)
                .unwrap_or(vanillite_abi::limits::NO_BLOCK);
        }
    }

    /// Sets the block states of a box from `states` (its volume, in [`BlockBox`] order),
    /// skipping [`vanillite_abi::limits::NO_BLOCK`] and positions outside loaded chunks;
    /// returns how many blocks changed. Every id is checked before the first block changes.
    fn set_blocks(
        &mut self,
        area: BlockBox,
        states: &[u32],
        mode: SetMode,
    ) -> Result<usize, Status>;

    /// The persistent data (`BukkitValues`) of an entity of this world (players too), for
    /// reading; [`vanillite_abi::status::NOT_FOUND`] when the entity is not here.
    fn entity_data(&self, _entity: u128) -> Result<Option<&vanillite_nbt::Compound>, Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }

    /// The persistent data of an entity of this world, for changing (created when missing,
    /// dropped by the server when left empty); [`vanillite_abi::status::NOT_FOUND`] when the
    /// entity is not here.
    fn entity_data_mut(&mut self, _entity: u128) -> Result<&mut vanillite_nbt::Compound, Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }

    /// A property ([`vanillite_abi::player_prop`]) of a player of this world.
    fn player_get(&self, _player: u128, _prop: u32) -> Result<f64, Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }

    /// Changes a property of a player of this world.
    fn player_set(&mut self, _player: u128, _prop: u32, _value: f64) -> Result<(), Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }

    /// Where an entity (or player) of this world is.
    fn entity_location(&self, _entity: u128) -> Result<Location, Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }

    /// The type of an entity of this world, e.g. `minecraft:zombie`.
    fn entity_type(&self, _entity: u128) -> Result<String, Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }

    /// Teleports an entity of this world (players also to another world).
    fn teleport(&mut self, _entity: u128, _to: Location) -> Result<(), Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }

    /// Sets an entity's motion.
    fn set_velocity(&mut self, _entity: u128, _velocity: [f64; 3]) -> Result<(), Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }

    /// Spawns an entity as `/summon` does; returns its UUID.
    fn spawn_entity(
        &mut self,
        _kind: &str,
        _pos: [f64; 3],
        _nbt: &vanillite_nbt::Compound,
    ) -> Result<u128, Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }

    /// Removes an entity (not a player).
    fn remove_entity(&mut self, _entity: u128) -> Result<(), Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }

    /// The entities within `radius` of `center`, nearest first, optionally of one type.
    fn entities_near(&self, _center: [f64; 3], _radius: f64, _kind: Option<&str>) -> Vec<u128> {
        Vec::new()
    }

    /// An entity's data as `/data get entity` shows it.
    fn entity_nbt(&self, _entity: u128) -> Result<vanillite_nbt::Compound, Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }

    /// Merges data into an entity as `/data merge entity` (not players).
    fn merge_entity_nbt(
        &mut self,
        _entity: u128,
        _nbt: &vanillite_nbt::Compound,
    ) -> Result<(), Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }

    /// Gives a living entity an effect ([`vanillite_abi::effect_flag`] flags).
    fn add_effect(
        &mut self,
        _entity: u128,
        _effect: &str,
        _duration: i32,
        _amplifier: i32,
        _flags: u32,
    ) -> Result<(), Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }

    /// Removes an effect (`None`: all) from a living entity.
    fn remove_effect(&mut self, _entity: u128, _effect: Option<&str>) -> Result<(), Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }

    /// Gives a player an item (its NBT form, `{id, count, components}`) as `/give` does.
    fn give_item(&mut self, _player: u128, _item: &vanillite_nbt::Compound) -> Result<(), Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }

    /// The item in a slot (named as `/item` does) of an entity, in its NBT form; empty for
    /// an empty slot.
    fn item(&self, _entity: u128, _slot: &str) -> Result<vanillite_nbt::Compound, Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }

    /// Puts an item (its NBT form; empty for none) into a slot of an entity.
    fn set_item(
        &mut self,
        _entity: u128,
        _slot: &str,
        _item: &vanillite_nbt::Compound,
    ) -> Result<(), Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }

    /// The block entity at a position as `/data get block` shows it.
    fn block_entity_nbt(&self, _pos: [i32; 3]) -> Result<vanillite_nbt::Compound, Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }

    /// Merges data into a block entity as `/data merge block`.
    fn merge_block_entity_nbt(
        &mut self,
        _pos: [i32; 3],
        _nbt: &vanillite_nbt::Compound,
    ) -> Result<(), Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }

    /// Grants or revokes a player's advancement criterion (`None`: all criteria).
    fn set_advancement(
        &mut self,
        _player: u128,
        _advancement: &str,
        _criterion: Option<&str>,
        _grant: bool,
    ) -> Result<(), Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }

    /// Whether a player has completed an advancement.
    fn advancement_done(&self, _player: u128, _advancement: &str) -> Result<bool, Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }

    /// A property ([`vanillite_abi::entity_prop`]) of an entity of this world (players too).
    fn entity_get(&self, _entity: u128, _prop: u32) -> Result<f64, Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }

    /// Changes a property of an entity of this world.
    fn entity_set(&mut self, _entity: u128, _prop: u32, _value: f64) -> Result<(), Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }

    /// An entity's custom name as JSON; `None` without one.
    fn entity_name(&self, _entity: u128) -> Result<Option<String>, Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }

    /// Sets (`None`: removes) an entity's custom name.
    fn set_entity_name(&mut self, _entity: u128, _name: Option<Text<'_>>) -> Result<(), Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }

    /// The passengers of an entity, the controlling one first.
    fn passengers(&self, _entity: u128) -> Result<Vec<u128>, Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }

    /// The vehicle of an entity; `None` when it rides nothing.
    fn vehicle(&self, _entity: u128) -> Result<Option<u128>, Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }

    /// Mounts `passenger` on `vehicle` as `/ride mount`.
    fn mount(&mut self, _passenger: u128, _vehicle: u128) -> Result<(), Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }

    /// Dismounts an entity as `/ride dismount`.
    fn dismount(&mut self, _passenger: u128) -> Result<(), Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }

    /// An attribute of a living entity: its base and its value.
    fn attribute(&self, _entity: u128, _attribute: &str) -> Result<(f64, f64), Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }

    /// Sets an attribute's base value.
    fn set_attribute_base(
        &mut self,
        _entity: u128,
        _attribute: &str,
        _value: f64,
    ) -> Result<(), Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }

    /// Adds (`Some((amount, operation))`, operation from [`vanillite_abi::attribute_op`]) or
    /// removes (`None`) the attribute modifier `id` (a full resource location).
    fn attribute_modifier(
        &mut self,
        _entity: u128,
        _attribute: &str,
        _id: &str,
        _change: Option<(f64, u32)>,
    ) -> Result<(), Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }

    /// The target of a mob; `None` for none.
    fn mob_target(&self, _mob: u128) -> Result<Option<u128>, Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }

    /// Sets (`None`: forgets) the target of a mob, with the target event.
    fn set_mob_target(&mut self, _mob: u128, _target: Option<u128>) -> Result<(), Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }

    /// Lets a mob walk to a position (speed modifier; 0 or less stops it).
    /// [`vanillite_abi::status::NOT_FOUND`] when no path is found.
    fn navigate(&mut self, _mob: u128, _to: [f64; 3], _speed: f64) -> Result<(), Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }

    /// Adds a plugin goal to a mob; returns its id.
    fn add_goal(&mut self, _mob: u128, _goal: PluginGoal) -> Result<u64, Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }

    /// Removes a plugin goal of a mob.
    fn remove_goal(&mut self, _mob: u128, _goal: u64) -> Result<(), Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }

    /// Removes a mob's Vanilla goals ([`vanillite_abi::goal_clear`] bits).
    fn clear_goals(&mut self, _mob: u128, _which: u32) -> Result<(), Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }

    /// The biome at a position, e.g. `minecraft:plains`.
    fn biome(&self, _pos: [i32; 3]) -> Result<String, Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }

    /// Sets the biome of a box as `/fillbiome`.
    fn set_biome(&mut self, _area: BlockBox, _biome: &str) -> Result<(), Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }

    /// The sky and block light at a position.
    fn light(&self, _pos: [i32; 3]) -> Result<(u32, u32), Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }

    /// How far a chunk is loaded ([`vanillite_abi::chunk_state`]).
    fn chunk_state(&self, _x: i32, _z: i32) -> Result<u32, Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }

    /// Force-loads a chunk (`true`) or ends it, as `/forceload`.
    fn force_load(&mut self, _x: i32, _z: i32, _on: bool) -> Result<(), Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }

    /// Adds (`true`) or removes a plugin's ticket on a chunk.
    fn chunk_ticket(&mut self, _plugin: &str, _x: i32, _z: i32, _add: bool) -> Result<(), Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }

    /// The world border.
    fn border(&self) -> Result<vanillite_abi::BorderInfo, Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }

    /// Changes the world border as `/worldborder`.
    fn set_border(&mut self, _border: &vanillite_abi::BorderInfo) -> Result<(), Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }

    /// An explosion as TNT makes it ([`vanillite_abi::explode_flag`] bits).
    fn explode(
        &mut self,
        _center: [f64; 3],
        _power: f32,
        _flags: u32,
        _source: Option<u128>,
    ) -> Result<(), Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }

    /// The nearest hit of a ray ([`vanillite_abi::raytrace_flag`] bits).
    fn raytrace(
        &self,
        _from: [f64; 3],
        _to: [f64; 3],
        _flags: u32,
        _ignore: Option<u128>,
    ) -> Result<vanillite_abi::RayHit, Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }

    /// Opens a plugin menu for a player; returns the container id.
    fn open_menu(&mut self, _player: u128, _menu: &Menu<'_>) -> Result<u32, Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }

    /// The item in a slot of a player's open menu (`{}` for none).
    fn menu_item(&self, _player: u128, _slot: u32) -> Result<vanillite_nbt::Compound, Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }

    /// Puts an item (empty compound: none) into a slot of a player's open menu.
    fn set_menu_item(
        &mut self,
        _player: u128,
        _slot: u32,
        _item: &vanillite_nbt::Compound,
    ) -> Result<(), Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }

    /// Closes a player's open menu.
    fn close_menu(&mut self, _player: u128) -> Result<(), Status> {
        Err(vanillite_abi::status::UNSUPPORTED)
    }
}

/// How plugin block changes update the world ([`vanillite_abi::set_mode`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetMode {
    /// Vanilla's `setBlock` with neighbour and shape updates.
    Update,
    /// No neighbour and shape updates; clients still see the change.
    NoPhysics,
}

impl SetMode {
    /// From [`vanillite_abi::set_mode`]; `None` for unknown values.
    pub fn from_abi(mode: u32) -> Option<SetMode> {
        match mode {
            vanillite_abi::set_mode::UPDATE => Some(SetMode::Update),
            vanillite_abi::set_mode::NO_PHYSICS => Some(SetMode::NoPhysics),
            _ => None,
        }
    }
}

/// A box of block positions, both corners inclusive; its blocks are ordered by Y, then Z,
/// then X (X fastest), as [`vanillite_abi::BlockBox`] arrays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockBox {
    /// The lowest corner.
    pub min: [i32; 3],
    /// The highest corner.
    pub max: [i32; 3],
}

impl BlockBox {
    /// The positions in array order.
    pub fn positions(self) -> impl Iterator<Item = [i32; 3]> {
        let [x0, y0, z0] = self.min;
        let [x1, y1, z1] = self.max;
        (y0..=y1).flat_map(move |y| (z0..=z1).flat_map(move |z| (x0..=x1).map(move |x| [x, y, z])))
    }
}

/// Settings of the host (`plugins.*` in `vanillite.toml`).
#[derive(Clone, Debug, Default)]
pub struct Settings {
    /// Load plugins at all.
    pub enabled: bool,
    /// Skip a plugin that crashed the server at the last run (same build).
    pub safe_mode_after_crash: bool,
    /// Development mode: `vanillite plugin reload <id>` loads a new build at runtime.
    pub development: bool,
    /// Pinned SHA-256 hashes: `id=hex,id=hex`.
    pub pins: String,
    /// The server's data version (providers and plugins built for another one are refused).
    pub data_version: i32,
}

const LOADING: u8 = 0;
const RUNNING: u8 = 1;

/// A pointer that may cross threads (plugin user data; the plugin guarantees its use).
#[derive(Clone, Copy, Debug)]
pub(crate) struct SendPtr(pub(crate) *mut std::ffi::c_void);

// SAFETY: user data belongs to the plugin, which declares how it may be used (threading mode).
unsafe impl Send for SendPtr {}
// SAFETY: as above.
unsafe impl Sync for SendPtr {}

/// A loaded plugin. Never unloaded: its library stays mapped until the process ends.
pub(crate) struct Plugin {
    pub(crate) manifest: Manifest,
    pub(crate) local: u32,
    pub(crate) abi_id: abi::PluginId,
    pub(crate) data_dir: PathBuf,
    pub(crate) sha256: String,
    pub(crate) info: abi::PluginInfo,
    pub(crate) tag: &'static crash::Tag,
    serialized: Option<Mutex<()>>,
    pub(crate) enabled: AtomicBool,
    pub(crate) config: Mutex<Option<toml::Table>>,
}

// SAFETY: `info` points to static data and functions of the plugin's library, which is never
// unloaded.
unsafe impl Send for Plugin {}
// SAFETY: as above; mutable state is behind atomics and locks.
unsafe impl Sync for Plugin {}

impl std::fmt::Debug for Plugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Plugin")
            .field("id", &self.manifest.id)
            .field("version", &self.manifest.version)
            .finish_non_exhaustive()
    }
}

impl Plugin {
    /// The plugin's id.
    pub(crate) fn id(&self) -> &str {
        &self.manifest.id
    }

    /// Calls into the plugin: records it for crash reports, holds its lock in serialized mode,
    /// refuses re-entrant calls and disables it when it panicked. `what` names the call in logs.
    pub(crate) fn call(&self, what: &str, f: impl FnOnce() -> Status) -> Status {
        if !self.enabled.load(Ordering::Acquire) {
            return status::DISABLED;
        }
        self.call_always(what, f)
    }

    /// [`Plugin::call`] also for a disabled plugin (cleanup: `disable`, dropping user data).
    pub(crate) fn call_always(&self, what: &str, f: impl FnOnce() -> Status) -> Status {
        let Some(_entered) = crash::enter(self.tag) else {
            log::warn!(
                "Plugin {} was called again while in a call ({what}); the call is dropped",
                self.id()
            );
            return status::WRONG_PHASE;
        };
        let _lock = self
            .serialized
            .as_ref()
            .map(|m| m.lock().unwrap_or_else(PoisonError::into_inner));
        let result = f();
        if result == status::PANICKED && self.enabled.swap(false, Ordering::AcqRel) {
            log::error!(
                "Plugin {} panicked in {what} and is disabled; the server keeps running",
                self.id()
            );
        }
        result
    }

    /// Calls a provider of the plugin: attributed like any call, but without the plugin lock
    /// (providers are thread-safe by contract) and regardless of the enabled flag.
    pub(crate) fn call_provider(&self, f: impl FnOnce() -> Status) -> Status {
        let _entered = crash::enter(self.tag);
        f()
    }
}

/// An event subscription.
#[derive(Clone, Debug)]
pub(crate) struct Subscriber {
    pub(crate) plugin: u32,
    pub(crate) priority: i32,
    /// [`abi::subscribe_flag`] bits.
    pub(crate) flags: u32,
    pub(crate) callback: abi::EventFn,
    pub(crate) user: SendPtr,
}

/// A service a plugin offers to others.
#[derive(Debug)]
pub(crate) struct Service {
    pub(crate) plugin: u32,
    pub(crate) name: String,
    pub(crate) version: u32,
    pub(crate) table: SendPtr,
}

/// The permission provider.
#[derive(Debug)]
pub(crate) struct PermissionProvider {
    pub(crate) plugin: u32,
    pub(crate) check: abi::PermissionFn,
    pub(crate) user: SendPtr,
}

/// State of one host, shared with the C functions through [`HOSTS`].
pub(crate) struct Inner {
    pub(crate) index: u16,
    pub(crate) dir: PathBuf,
    pub(crate) settings: Settings,
    pub(crate) plugins: RwLock<Vec<Arc<Plugin>>>,
    phase: AtomicU8,
    /// Local index + 1 of the plugin being reloaded (its registrations replace the old ones).
    reloading: AtomicU32,
    pub(crate) events: events::Events,
    pub(crate) packets: packets::Packets,
    pub(crate) commands: commands::Commands,
    pub(crate) permission: RwLock<Option<PermissionProvider>>,
    /// Registered permission nodes with their [`abi::permission_default`].
    pub(crate) permission_defaults: RwLock<HashMap<String, u32>>,
    /// Services by registration order.
    pub(crate) services: RwLock<Vec<Service>>,
    /// Plugin message channels: (plugin, channel).
    pub(crate) channels: RwLock<Vec<(u32, String)>>,
    pub(crate) generator: Mutex<Option<(u32, abi::ChunkGeneratorProvider)>>,
    /// Chunk populators in registration order.
    pub(crate) populators: Mutex<Vec<(u32, abi::ChunkPopulatorProvider)>>,
    pub(crate) tasks: scheduler::Tasks,
    pub(crate) server: OnceLock<Arc<dyn ServerAccess>>,
    pub(crate) tick: AtomicU64,
    /// Number of the next development copy of a library.
    dev_copies: AtomicU32,
}

// SAFETY: the provider table holds the plugin's function pointers and user data, which the
// plugin declares thread-safe (providers are called from many threads by contract).
unsafe impl Send for Inner {}
// SAFETY: as above.
unsafe impl Sync for Inner {}

impl std::fmt::Debug for Inner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginHost")
            .field("dir", &self.dir)
            .finish_non_exhaustive()
    }
}

/// All hosts of the process (normally one; tests may run several servers). A plugin id holds
/// the host's index in its upper 16 bits.
static HOSTS: RwLock<Vec<Weak<Inner>>> = RwLock::new(Vec::new());

/// Libraries loaded in this process: a library's statics are shared, so it can only serve one
/// host.
static LOADED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

impl Inner {
    /// The host and plugin of a plugin id from a C call.
    pub(crate) fn resolve(id: abi::PluginId) -> Option<(Arc<Inner>, Arc<Plugin>)> {
        let host = usize::try_from(id >> 16).ok()?;
        let inner = HOSTS
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(host)?
            .upgrade()?;
        let plugin = inner.plugin(id & 0xffff)?;
        Some((inner, plugin))
    }

    pub(crate) fn plugin(&self, local: u32) -> Option<Arc<Plugin>> {
        self.plugins
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(usize::try_from(local).ok()?)
            .cloned()
    }

    /// Whether `plugin` may register now: during `load`, or its own development reload.
    pub(crate) fn may_register(&self, plugin: &Plugin) -> bool {
        self.phase.load(Ordering::Acquire) == LOADING
            || self.reloading.load(Ordering::Acquire) == plugin.local + 1
    }

    /// Whether `plugin` is being reloaded right now.
    pub(crate) fn is_reloading(&self, plugin: &Plugin) -> bool {
        self.reloading.load(Ordering::Acquire) == plugin.local + 1
    }

    pub(crate) fn server(&self) -> Option<&Arc<dyn ServerAccess>> {
        self.server.get()
    }
}

/// The plugin host of a server.
#[derive(Clone, Debug)]
pub struct PluginHost {
    inner: Arc<Inner>,
}

impl PluginHost {
    /// A host without plugins.
    pub fn empty() -> PluginHost {
        Self::new(PathBuf::new(), Settings::default())
    }

    fn new(dir: PathBuf, settings: Settings) -> PluginHost {
        let mut hosts = HOSTS.write().unwrap_or_else(PoisonError::into_inner);
        let index = hosts
            .iter()
            .position(|w| w.strong_count() == 0)
            .unwrap_or(hosts.len());
        let inner = Arc::new(Inner {
            index: u16::try_from(index).unwrap_or(u16::MAX),
            dir,
            settings,
            plugins: RwLock::new(Vec::new()),
            phase: AtomicU8::new(LOADING),
            reloading: AtomicU32::new(0),
            events: events::Events::default(),
            packets: packets::Packets::default(),
            commands: commands::Commands::default(),
            permission: RwLock::new(None),
            permission_defaults: RwLock::new(HashMap::new()),
            services: RwLock::new(Vec::new()),
            channels: RwLock::new(Vec::new()),
            generator: Mutex::new(None),
            populators: Mutex::new(Vec::new()),
            tasks: scheduler::Tasks::default(),
            server: OnceLock::new(),
            tick: AtomicU64::new(0),
            dev_copies: AtomicU32::new(0),
        });
        inner
            .packets
            .tap
            .install(Arc::new(packets::Bridge(Arc::downgrade(&inner))));
        if index == hosts.len() {
            hosts.push(Arc::downgrade(&inner));
        } else if let Some(slot) = hosts.get_mut(index) {
            *slot = Arc::downgrade(&inner);
        }
        PluginHost { inner }
    }

    /// Discovers, validates and loads the plugins in `dir` (normally `plugins/`), in
    /// dependency order, and calls their `load`. Failures are logged; the plugin is skipped.
    pub fn load(dir: &Path, settings: Settings) -> PluginHost {
        let enabled = settings.enabled;
        let host = Self::new(dir.to_owned(), settings);
        if !enabled {
            return host;
        }
        if let Err(e) = std::fs::create_dir_all(dir) {
            log::error!("Cannot create {}: {e}", dir.display());
            return host;
        }
        let packages = loader::order(loader::discover(dir));
        if packages.is_empty() {
            return host;
        }
        crash::install(dir);
        let crashed = crash::read_crashed(dir);
        let mut still_crashed = Vec::new();
        let pins = parse_pins(&host.inner.settings.pins);
        for package in packages {
            let id = package.manifest.id.clone();
            if let Some((_, sha)) = crashed.iter().find(|(i, _)| *i == id) {
                if *sha == package.sha256 && host.inner.settings.safe_mode_after_crash {
                    log::warn!(
                        "Safe mode: plugin {id} crashed the server at the last run and is skipped \
                         until its library changes or plugins/{} is removed",
                        crash::CRASH_FILE
                    );
                    still_crashed.push((id, sha.clone()));
                    continue;
                }
                if *sha == package.sha256 {
                    log::warn!("Plugin {id} crashed the server at the last run");
                }
            }
            if let Some(pin) = pins.get(id.as_str())
                && !pin.eq_ignore_ascii_case(&package.sha256)
            {
                log::error!(
                    "Skipping plugin {id}: its SHA-256 {} does not match the pin {pin} \
                     (plugins.pins)",
                    package.sha256
                );
                continue;
            }
            if let Err(e) = host.load_package(&package) {
                log::error!("Skipping plugin {id}: {e}");
            }
        }
        crash::write_crashed(dir, &still_crashed);
        host
    }

    /// Loads one package and calls its `load`; the plugin's registrations are kept.
    fn load_package(&self, package: &loader::Package) -> Result<(), String> {
        let manifest = &package.manifest;
        for dependency in &manifest.depends {
            if !self.is_loaded(dependency) {
                return Err(format!("its dependency {dependency} did not load"));
            }
        }
        let library = canonical(&package.library);
        {
            let mut loaded = LOADED.lock().unwrap_or_else(PoisonError::into_inner);
            if loaded.contains(&library) {
                return Err(format!(
                    "{} is already loaded by another server in this process",
                    library.display()
                ));
            }
            loaded.push(library.clone());
        }
        log::info!(
            "Loading plugin {} {} (SHA-256 {})",
            manifest.id,
            manifest.version,
            package.sha256
        );
        if !manifest.uses.is_empty() {
            log::info!(
                "Plugin {} declares that it uses: {}",
                manifest.id,
                manifest.uses.join(", ")
            );
        }
        let info = open(&library, manifest, self.inner.settings.data_version)?;
        let local = {
            let mut plugins = self
                .inner
                .plugins
                .write()
                .unwrap_or_else(PoisonError::into_inner);
            let local = u32::try_from(plugins.len()).map_err(|_| "too many plugins")?;
            let data_dir = self.inner.dir.join(&manifest.id);
            std::fs::create_dir_all(&data_dir)
                .map_err(|e| format!("{}: {e}", data_dir.display()))?;
            plugins.push(Arc::new(Plugin {
                manifest: manifest.clone(),
                local,
                abi_id: (u32::from(self.inner.index) << 16) | local,
                data_dir,
                sha256: package.sha256.clone(),
                info,
                tag: crash::Tag::new(&manifest.id, &package.sha256),
                serialized: (manifest.threading == Threading::Serialized).then(|| Mutex::new(())),
                enabled: AtomicBool::new(true),
                config: Mutex::new(None),
            }));
            local
        };
        let plugin = self.inner.plugin(local).ok_or("plugin vanished")?;
        let result = plugin.call("load", || {
            // SAFETY: the host table is static; `load` is the validated entry of the plugin.
            unsafe { (plugin.info.load)(&api::HOST_API, plugin.abi_id) }
        });
        if result != status::OK {
            plugin.enabled.store(false, Ordering::Release);
            self.inner.forget(local);
            return Err(format!("its load failed (status {result})"));
        }
        Ok(())
    }

    fn is_loaded(&self, id: &str) -> bool {
        self.inner
            .plugins
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .any(|p| p.id() == id && p.enabled.load(Ordering::Acquire))
    }

    /// Ids of the enabled plugins in load order.
    pub fn plugins(&self) -> Vec<String> {
        self.inner
            .plugins
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|p| p.enabled.load(Ordering::Acquire))
            .map(|p| p.id().to_owned())
            .collect()
    }

    /// Connects the host to the running server. Until then server functions fail with
    /// `WRONG_PHASE`.
    pub fn set_server(&self, server: Arc<dyn ServerAccess>) {
        let _ = self.inner.server.set(server);
    }

    /// Ends the registration phase and enables every plugin in load order.
    pub fn enable_all(&self) {
        self.inner.phase.store(RUNNING, Ordering::Release);
        let plugins = self
            .inner
            .plugins
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        for plugin in plugins {
            let mut ctx = api::CallCtx::new(None, &plugin);
            let result = plugin.call("enable", || {
                // SAFETY: validated entry; the context lives for the call.
                unsafe { (plugin.info.enable)(ctx.as_raw()) }
            });
            if result == status::OK {
                log::info!("Enabled plugin {} {}", plugin.id(), plugin.manifest.version);
            } else if result != status::DISABLED {
                log::error!(
                    "Enabling plugin {} failed (status {result}); it is disabled",
                    plugin.id()
                );
                plugin.enabled.store(false, Ordering::Release);
                self.inner.forget(plugin.local);
            }
        }
    }

    /// Disables every plugin in reverse load order (shutdown) and drops their tasks.
    pub fn disable_all(&self) {
        let plugins = self
            .inner
            .plugins
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        for plugin in plugins.iter().rev() {
            if !plugin.enabled.load(Ordering::Acquire) {
                continue;
            }
            let mut ctx = api::CallCtx::new(None, plugin);
            log::info!("Disabling plugin {}", plugin.id());
            plugin.call("disable", || {
                // SAFETY: validated entry; the context lives for the call.
                unsafe { (plugin.info.disable)(ctx.as_raw()) }
            });
            plugin.enabled.store(false, Ordering::Release);
        }
        self.inner.tasks.clear(&self.inner);
    }

    /// Development reload (`plugins.development`): loads the current build of a plugin under a
    /// fresh file name next to the old one, moves its state with `save_state`/`restore_state`
    /// and replaces its event handlers, command callbacks and tasks. The old library stays
    /// mapped (a deliberate leak). Command trees and providers are fixed at startup: a changed
    /// tree or a new provider needs a restart.
    pub fn reload(&self, id: &str) -> Result<(), String> {
        if !self.inner.settings.development {
            return Err(
                "reloading needs plugins.development = true; restart the server instead".into(),
            );
        }
        let old = self
            .inner
            .plugins
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|p| p.id() == id)
            .cloned()
            .ok_or_else(|| format!("no plugin {id}"))?;
        let package = loader::discover(&self.inner.dir)
            .into_iter()
            .find(|p| p.manifest.id == id)
            .ok_or_else(|| format!("the package of {id} is gone"))?;
        let copies = self.inner.dir.join(".dev");
        std::fs::create_dir_all(&copies).map_err(|e| e.to_string())?;
        let n = self.inner.dev_copies.fetch_add(1, Ordering::Relaxed);
        let copy = copies.join(format!(
            "{id}-{}-{n}-{}",
            std::process::id(),
            loader::library_file(&package.manifest.library)
        ));
        std::fs::copy(&package.library, &copy).map_err(|e| format!("{}: {e}", copy.display()))?;
        let info = open(&copy, &package.manifest, self.inner.settings.data_version)?;

        // Save the state and stop the old build.
        let mut state = Vec::new();
        if let Some(save) = old.info.save_state {
            let sink = abi::ByteSink {
                size: abi::size_of::<abi::ByteSink>(),
                state: std::ptr::from_mut(&mut state).cast(),
                write: api::collect_bytes,
            };
            // SAFETY: validated entry; the sink lives for the call.
            old.call("save_state", || unsafe { save(&sink) });
        }
        if old.enabled.load(Ordering::Acquire) {
            let mut ctx = api::CallCtx::new(None, &old);
            // SAFETY: validated entry; the context lives for the call.
            old.call("disable", || unsafe { (old.info.disable)(ctx.as_raw()) });
        }
        old.enabled.store(false, Ordering::Release);
        self.inner.tasks.drop_plugin(&self.inner, old.local);

        // Start the new build in the old one's place.
        let new = Arc::new(Plugin {
            manifest: package.manifest.clone(),
            local: old.local,
            abi_id: old.abi_id,
            data_dir: old.data_dir.clone(),
            sha256: package.sha256.clone(),
            info,
            tag: crash::Tag::new(id, &package.sha256),
            serialized: (package.manifest.threading == Threading::Serialized)
                .then(|| Mutex::new(())),
            enabled: AtomicBool::new(true),
            config: Mutex::new(None),
        });
        if let Some(slot) = self
            .inner
            .plugins
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .get_mut(usize::try_from(old.local).unwrap_or(usize::MAX))
        {
            *slot = Arc::clone(&new);
        }
        self.inner.events.remove_plugin(&self.inner, old.local);
        self.inner.packets.remove(old.local, None);
        self.inner
            .services
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|s| s.plugin != old.local);
        self.inner
            .channels
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(p, _)| *p != old.local);
        self.inner.commands.begin_reload(old.local);
        self.inner.reloading.store(old.local + 1, Ordering::Release);
        // SAFETY: the host table is static; `load` is the validated entry of the new build.
        let loaded = new.call("load", || unsafe {
            (new.info.load)(&api::HOST_API, new.abi_id)
        });
        self.inner.reloading.store(0, Ordering::Release);
        if loaded != status::OK {
            new.enabled.store(false, Ordering::Release);
            self.inner.forget(new.local);
            return Err(format!(
                "the new build's load failed (status {loaded}); {id} is disabled"
            ));
        }
        if let Some(restore) = new.info.restore_state {
            // SAFETY: validated entry; the bytes live for the call.
            new.call("restore_state", || unsafe {
                restore(abi::Bytes::new(&state))
            });
        }
        let mut ctx = api::CallCtx::new(None, &new);
        // SAFETY: validated entry; the context lives for the call.
        let enabled = new.call("enable", || unsafe { (new.info.enable)(ctx.as_raw()) });
        if enabled != status::OK {
            new.enabled.store(false, Ordering::Release);
            self.inner.forget(new.local);
            return Err(format!(
                "the new build's enable failed (status {enabled}); {id} is disabled"
            ));
        }
        log::info!(
            "Reloaded plugin {id} {} (SHA-256 {})",
            new.manifest.version,
            new.sha256
        );
        Ok(())
    }

    /// Whether any plugin listens to an event kind ([`abi::event`]); hook points check this
    /// before they build an event.
    #[inline]
    pub fn wants(&self, kind: u32) -> bool {
        self.inner.events.wants(kind)
    }

    /// Runs the tasks due on the global owner (call once per tick in the global pre-phase).
    pub fn run_global_tasks(&self, tick: u64) {
        self.inner.tick.store(tick, Ordering::Release);
        self.inner.tasks.run_global(&self.inner, tick);
    }

    /// Runs the tasks due on a world (call once per world tick, with the world borrowed).
    pub fn run_world_tasks(&self, world: &mut dyn WorldAccess, tick: u64) {
        self.inner.tasks.run_world(&self.inner, world, tick);
    }

    /// Whether a player has a permission node: the permission provider decides; without one
    /// or when it does not decide, operators have every node.
    pub fn has_permission(&self, player: u128, node: &str, operator: bool) -> bool {
        api::has_permission(&self.inner, player, node, operator)
    }
}

impl Inner {
    /// Removes everything a plugin registered (after a failed load or enable).
    pub(crate) fn forget(&self, local: u32) {
        if let (Some(server), Some(plugin)) = (self.server(), self.plugin(local)) {
            server.plugin_disabled(plugin.id());
        }
        self.populators
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(p, _)| *p != local);
        self.events.remove_plugin(self, local);
        self.packets.remove(local, None);
        self.tasks.drop_plugin(self, local);
        self.services
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|s| s.plugin != local);
        self.channels
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(p, _)| *p != local);
        let mut permission = self
            .permission
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        if permission.as_ref().is_some_and(|p| p.plugin == local) {
            *permission = None;
        }
    }
}

/// Opens a library and reads and validates its plugin info.
fn open(library: &Path, manifest: &Manifest, data_version: i32) -> Result<abi::PluginInfo, String> {
    // SAFETY: loading a library runs its initialisers: plugins are trusted code (architecture
    // 6.5). The library is never unloaded, so everything it hands out stays valid.
    let lib = unsafe { libloading::Library::new(library) }
        .map_err(|e| format!("cannot load {}: {e}", library.display()))?;
    // SAFETY: the symbol has the ABI's entry type by contract; checked by its results below.
    let entry: abi::EntryFn = unsafe {
        *lib.get::<abi::EntryFn>(abi::ENTRY_SYMBOL.as_bytes())
            .map_err(|_| format!("{} exports no {}", library.display(), abi::ENTRY_SYMBOL))?
    };
    // SAFETY: the entry returns a pointer to static plugin info, or null.
    let info = unsafe { entry() };
    if info.is_null() {
        return Err("the plugin returned no plugin info".into());
    }
    // SAFETY: non-null and static; every version of the record starts with its size.
    let size = unsafe { (*info).size } as usize;
    if size < size_of::<abi::PluginInfo>() {
        return Err(format!(
            "the plugin info has {size} bytes, expected {}",
            size_of::<abi::PluginInfo>()
        ));
    }
    // SAFETY: non-null, static and at least as large as this version's record.
    let info = unsafe { *info };
    loader::validate(&info, manifest, data_version)?;
    std::mem::forget(lib);
    Ok(info)
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned())
}

fn parse_pins(pins: &str) -> HashMap<&str, &str> {
    pins.split(',')
        .filter_map(|p| p.split_once('='))
        .map(|(id, sha)| (id.trim(), sha.trim()))
        .collect()
}

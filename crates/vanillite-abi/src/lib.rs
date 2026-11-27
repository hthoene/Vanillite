//! The stable C ABI for plugins and providers: `repr(C)` tables generated from one declarative API definition.
//!
//! Everything that crosses the boundary between the server and a plugin library is declared
//! here, once, with the `abi!` macro: plain C types (integers, floats, `repr(C)` records,
//! `(ptr, len)` strings, function pointers), never Rust types such as `Box`, `Vec`, `String` or
//! trait objects. Plugins therefore depend neither on the server's compiler version nor on its
//! dependencies. Memory is freed by the side that allocated it.
//!
//! # Versioning
//!
//! - [`ABI_MAJOR`] changes on a breaking change; [`ABI_MINOR`] when functions, events or record
//!   fields are appended. A plugin built against `1.2` runs on a `1.4` host; a plugin that needs
//!   `1.4` is refused by a `1.2` host.
//! - Tables and records start with a `size` field: each side reads only what the other knows.
//!   Fields are only ever appended at the end. Arrays of records carry their element size
//!   (`stride`), so a record in an array can grow too.
//! - Enumerations are `u32` constants; unknown values are ignored or refused, never trusted.
//! - [`layout`] describes every declaration; the test `abi_snapshot` compares it with the
//!   recorded `abi.snapshot` and fails on a breaking change without a major version bump.
//!
//! # Entry point
//!
//! A plugin library exports `vanillite_plugin_abi` ([`ENTRY_SYMBOL`], type [`EntryFn`]). It
//! returns a [`PluginInfo`] before any other plugin code runs; the host validates it (ABI
//! version, panic strategy, target) and only then calls [`PluginInfo::load`] with the
//! [`HostApi`] table.
//!
//! # Threads and contexts
//!
//! Host functions whose first parameter is a [`PluginId`] are thread-safe and may be called
//! from any thread after `load`. Functions whose first parameter is a `*mut` [`Context`] touch
//! state of one owner (a world) and are only valid during the callback that received the
//! context; they fail with [`status::WRONG_OWNER`] when the context does not own the data.

#![no_std]

use core::ffi::c_void;

pub mod layout;

/// Major version of the plugin ABI: changes on breaking changes.
pub const ABI_MAJOR: u16 = 1;
/// Minor version of the plugin ABI: changes when declarations are appended.
pub const ABI_MINOR: u16 = 7;
/// The symbol every plugin library exports (type [`EntryFn`]).
pub const ENTRY_SYMBOL: &str = "vanillite_plugin_abi";

/// An opaque per-call context the host passes to callbacks. It is only valid during the call.
#[repr(C)]
#[derive(Debug)]
pub struct Context {
    _opaque: [u8; 0],
}

/// Declares the ABI: type aliases, constant groups and `repr(C)` records, and records each
/// declaration in [`layout::TYPES`], [`layout::CONSTS`] and [`layout::STRUCTS`].
macro_rules! abi {
    (
        types { $( $(#[$tmeta:meta])* pub type $tname:ident = $tty:ty; )* }
        consts { $(
            $(#[$cmeta:meta])* pub mod $cmod:ident: $cty:ty {
                $( $(#[$vmeta:meta])* $cname:ident = $cvalue:expr, )*
            }
        )* }
        structs { $(
            $(#[$smeta:meta])* pub struct $sname:ident {
                $( $(#[$fmeta:meta])* pub $fname:ident: $fty:ty, )*
            }
        )* }
    ) => {
        $( $(#[$tmeta])* pub type $tname = $tty; )*
        $(
            $(#[$cmeta])* pub mod $cmod {
                #[allow(unused_imports)]
                use super::*;
                $( $(#[$vmeta])* pub const $cname: $cty = $cvalue; )*
            }
        )*
        $(
            $(#[$smeta])*
            #[repr(C)]
            #[derive(Clone, Copy, Debug)]
            pub struct $sname { $( $(#[$fmeta])* pub $fname: $fty, )* }
        )*

        /// The declarations of this ABI as data, for the compatibility check.
        pub(crate) mod declared {
            use super::*;

            pub(crate) const TYPES: &[layout::TypeLayout] = &[
                $( layout::TypeLayout { name: stringify!($tname), definition: stringify!($tty) }, )*
            ];

            pub(crate) const CONSTS: &[layout::ConstLayout] = &[
                $( $( layout::ConstLayout {
                    name: concat!(stringify!($cmod), "::", stringify!($cname)),
                    ty: stringify!($cty),
                    value: $cmod::$cname as i64,
                }, )* )*
            ];

            pub(crate) const STRUCTS: &[layout::StructLayout] = &[
                $( layout::StructLayout {
                    name: stringify!($sname),
                    size: core::mem::size_of::<$sname>(),
                    align: core::mem::align_of::<$sname>(),
                    fields: &[ $( layout::FieldLayout {
                        name: stringify!($fname),
                        ty: stringify!($fty),
                        offset: core::mem::offset_of!($sname, $fname),
                    }, )* ],
                }, )*
            ];
        }
    };
}

abi! {
    types {
        /// Result of a call: [`status::OK`] or an error code of [`status`].
        pub type Status = i32;
        /// Identifies the calling plugin in host calls; the host passes it to `load`.
        pub type PluginId = u32;
        /// The plugin's entry point, exported as `vanillite_plugin_abi`.
        pub type EntryFn = unsafe extern "C" fn() -> *const PluginInfo;
        /// `PluginInfo::load`: register commands, events and providers.
        pub type LoadFn = unsafe extern "C" fn(host: *const HostApi, plugin: PluginId) -> Status;
        /// `PluginInfo::enable` and `PluginInfo::disable`.
        pub type LifecycleFn = unsafe extern "C" fn(ctx: *mut Context) -> Status;
        /// `PluginInfo::save_state`: writes the state to keep over a development reload.
        pub type SaveStateFn = unsafe extern "C" fn(sink: *const ByteSink) -> Status;
        /// `PluginInfo::restore_state`: takes over the state written by the previous build.
        pub type RestoreStateFn = unsafe extern "C" fn(state: Bytes) -> Status;
        /// Receives bytes (copied by the receiver before it returns).
        pub type ByteSinkFn = unsafe extern "C" fn(state: *mut c_void, data: Bytes);
        /// An event handler; `event` points to the record of the event's kind.
        pub type EventFn =
            unsafe extern "C" fn(user: *mut c_void, ctx: *mut Context, event: *mut EventHeader) -> Status;
        /// A scheduled task.
        pub type TaskFn = unsafe extern "C" fn(user: *mut c_void, ctx: *mut Context) -> Status;
        /// Frees user data once the host no longer needs it.
        pub type DropFn = unsafe extern "C" fn(user: *mut c_void);
        /// Runs a command; `result` receives the command's result value.
        pub type CommandFn = unsafe extern "C" fn(
            user: *mut c_void,
            ctx: *mut Context,
            invocation: *const CommandInvocation,
            result: *mut i32,
        ) -> Status;
        /// Answers a custom suggestion provider of a command argument.
        pub type SuggestFn = unsafe extern "C" fn(
            user: *mut c_void,
            ctx: *mut Context,
            request: *const SuggestRequest,
            sink: *const SuggestSink,
        ) -> Status;
        /// Adds one suggestion (copied by the host).
        pub type SuggestAddFn = unsafe extern "C" fn(state: *mut c_void, text: Str);
        /// A permission provider: writes one of [`permission`] to `out`.
        pub type PermissionFn =
            unsafe extern "C" fn(user: *mut c_void, player: Uuid, node: Str, out: *mut u32) -> Status;
        /// Creates the generator of one world; a null instance keeps the built-in generator.
        pub type GeneratorCreateFn = unsafe extern "C" fn(
            user: *mut c_void,
            world: *const WorldInfo,
            instance: *mut *mut c_void,
        ) -> Status;
        /// Fills the blocks of a batch of chunks (`chunks[i]` at `chunks + i * stride` bytes).
        pub type GeneratorRunFn = unsafe extern "C" fn(
            instance: *mut c_void,
            chunks: *const GenChunk,
            count: usize,
            stride: usize,
        ) -> Status;
        /// Destroys a generator instance.
        pub type GeneratorDestroyFn = unsafe extern "C" fn(instance: *mut c_void);
        /// Replaces a text (copied by the receiver before it returns; `format` from
        /// [`text_format`]). Since ABI 1.4.
        pub type TextSetFn = unsafe extern "C" fn(state: *mut c_void, text: Str, format: u32);
        /// A packet listener; the event is valid for the call. Since ABI 1.6.
        pub type PacketFn =
            unsafe extern "C" fn(user: *mut c_void, ctx: *mut Context, event: *mut PacketEvent) -> Status;
        /// Replaces a packet's body (copied by the host before it returns); fails with
        /// [`status::INVALID_ARGUMENT`] for an empty or oversized body. Since ABI 1.6.
        pub type PacketSetFn = unsafe extern "C" fn(state: *mut c_void, body: Bytes) -> Status;
    }
    consts {
        /// Result codes ([`Status`]).
        pub mod status: Status {
            /// Success.
            OK = 0,
            /// A failure without a more specific code.
            ERROR = 1,
            /// The host or plugin does not support this call or value.
            UNSUPPORTED = 2,
            /// The thing does not exist (offline player, unknown key, stale handle).
            NOT_FOUND = 3,
            /// The context does not own this data; schedule a task on its owner instead.
            WRONG_OWNER = 4,
            /// An argument is invalid (bad UTF-8, unknown block state, out of range).
            INVALID_ARGUMENT = 5,
            /// The callee panicked; the panic was contained.
            PANICKED = 6,
            /// The call is not allowed in this phase (e.g. registering after `load`).
            WRONG_PHASE = 7,
            /// The plugin is disabled.
            DISABLED = 8,
        }
        /// Panic strategies ([`PluginInfo::panic_strategy`]).
        pub mod panic_strategy: u32 {
            /// `panic = "unwind"`: panics can be contained. Required.
            UNWIND = 0,
            /// `panic = "abort"`: a panic aborts the process. Refused by the host.
            ABORT = 1,
        }
        /// Log levels.
        pub mod log_level: u32 {
            /// Errors.
            ERROR = 1,
            /// Warnings.
            WARN = 2,
            /// Information.
            INFO = 3,
            /// Debug output.
            DEBUG = 4,
            /// Trace output.
            TRACE = 5,
        }
        /// Text formats of messages.
        pub mod text_format: u32 {
            /// Plain text.
            PLAIN = 0,
            /// A JSON text component, e.g. `{"text":"Hi","color":"gold"}`.
            JSON = 1,
        }
        /// Event kinds ([`EventHeader::kind`]) with the record each one points to.
        pub mod event: u32 {
            /// A player joined ([`PlayerEvent`]), after the join message.
            PLAYER_JOIN = 1,
            /// A player left ([`PlayerEvent`]).
            PLAYER_LEAVE = 2,
            /// A player sends a chat message ([`PlayerTextEvent`]); cancellable.
            PLAYER_CHAT = 3,
            /// A player runs a command ([`PlayerTextEvent`], without `/`); cancellable.
            PLAYER_COMMAND = 4,
            /// The players of a world that moved this tick ([`PlayersMovedEvent`]); batched.
            PLAYERS_MOVED = 5,
            /// An entity is about to take damage ([`EntityDamageEvent`]); cancellable,
            /// the amount can be changed.
            ENTITY_DAMAGE = 6,
            /// A player is about to break a block ([`BlockEvent`]); cancellable.
            BLOCK_BREAK = 7,
            /// A player is about to place a block ([`BlockEvent`]); cancellable.
            BLOCK_PLACE = 8,
            /// A player is about to fall asleep in a bed ([`BlockEvent`]: the bed's head);
            /// cancellable. Since ABI 1.3.
            PLAYER_BED_ENTER = 9,
            /// A player left its bed ([`BlockEvent`]: the bed's head). Since ABI 1.3.
            PLAYER_BED_LEAVE = 10,
            /// A bed or respawn anchor is about to become a player's respawn point
            /// ([`BlockEvent`]); cancellable. Since ABI 1.3.
            PLAYER_SPAWN_CHANGE = 11,
            /// A player logs in, after the server's own checks (bans, whitelist, player limit)
            /// and before it joins a world ([`LoginEvent`]); runs on the global owner.
            /// Cancelled means refused: handlers may refuse with a message or let a refused
            /// player in. Since ABI 1.4.
            PLAYER_LOGIN = 12,
            /// A player died ([`PlayerDeathEvent`]): the death message and whether it keeps its
            /// inventory can be changed. Since ABI 1.4.
            PLAYER_DEATH = 13,
            /// A mob died ([`EntityDeathEvent`]). Since ABI 1.4.
            ENTITY_DEATH = 14,
            /// A player respawned, after death or the end credits ([`PlayerWorldEvent`]).
            /// Since ABI 1.4.
            PLAYER_RESPAWN = 15,
            /// A player arrived in another world through a portal or a teleport
            /// ([`PlayerWorldEvent`]). Since ABI 1.4.
            PLAYER_CHANGE_WORLD = 16,
            /// A player's game mode is about to change by `/gamemode` or the forced default
            /// game mode ([`GameModeEvent`]); cancellable. Since ABI 1.4.
            PLAYER_GAME_MODE_CHANGE = 17,
            /// A player uses its item on a block or in the air ([`InteractEvent`]);
            /// cancellable: nothing happens and the client is corrected. Since ABI 1.4.
            PLAYER_INTERACT = 18,
            /// A player sent a message on a channel a plugin registered
            /// ([`PluginMessageEvent`]). Since ABI 1.5.
            PLAYER_PLUGIN_MESSAGE = 19,
            /// A player completed an advancement ([`AdvancementEvent`]). Since ABI 1.5.
            PLAYER_ADVANCEMENT_DONE = 20,
        }
        /// Flags of `subscribe_flags`. Since ABI 1.5.
        pub mod subscribe_flag: u32 {
            /// The handler is skipped while the event is cancelled (Bukkit's `ignoreCancelled`).
            IGNORE_CANCELLED = 1,
        }
        /// Event priorities: handlers run from the lowest to the highest value, then monitors.
        pub mod priority: i32 {
            /// First.
            LOWEST = -200,
            /// Early.
            LOW = -100,
            /// Default.
            NORMAL = 0,
            /// Late.
            HIGH = 100,
            /// Last that may change the event.
            HIGHEST = 200,
            /// Observes the outcome; must not change the event.
            MONITOR = 1000,
        }
        /// Owners a task runs on ([`Owner::kind`]).
        pub mod owner: u32 {
            /// The server thread between ticks (global pre-phase).
            GLOBAL = 0,
            /// A world, between the players' packets and the world's simulation.
            WORLD = 1,
            /// The owner of a block position (today its world; a region later).
            LOCATION = 2,
            /// The owner of an entity; the task ends when the entity is gone.
            ENTITY = 3,
            /// A background thread off the tick (I/O, databases). No world access.
            BACKGROUND = 4,
        }
        /// Command node kinds ([`CommandNode::kind`]).
        pub mod node: u32 {
            /// A literal word.
            LITERAL = 0,
            /// An argument.
            ARGUMENT = 1,
        }
        /// Command argument types ([`CommandNode::arg_type`], [`CommandArg::kind`]).
        pub mod arg: u32 {
            /// `true` or `false` ([`CommandArg::int`] 0 or 1).
            BOOL = 0,
            /// A 32-bit integer, bounds from `min`/`max` ([`CommandArg::int`]).
            INTEGER = 1,
            /// A 64-bit integer ([`CommandArg::int`]).
            LONG = 2,
            /// A 32-bit float ([`CommandArg::float`]).
            FLOAT = 3,
            /// A 64-bit float ([`CommandArg::float`]).
            DOUBLE = 4,
            /// One word ([`CommandArg::text`]).
            WORD = 5,
            /// A word or a quoted string ([`CommandArg::text`]).
            STRING = 6,
            /// The rest of the input ([`CommandArg::text`]).
            GREEDY_STRING = 7,
            /// One player: a name or a selector ([`CommandArg::players`], one entry).
            PLAYER = 8,
            /// Players: names or selectors ([`CommandArg::players`]).
            PLAYERS = 9,
            /// A block position, relative coordinates resolved ([`CommandArg::pos`]).
            BLOCK_POS = 10,
            /// One entity: a selector or a UUID ([`CommandArg::players`], one entry). Since
            /// ABI 1.5.
            ENTITY = 11,
            /// Entities: a selector or a UUID ([`CommandArg::players`]). Since ABI 1.5.
            ENTITIES = 12,
            /// A position, relative and local coordinates resolved ([`CommandArg::vec`]).
            /// Since ABI 1.5.
            VEC3 = 13,
            /// A rotation, relative values resolved ([`CommandArg::vec`]: `x` yaw, `y`
            /// pitch). Since ABI 1.5.
            ROTATION = 14,
            /// A resource location, e.g. `minecraft:stone` ([`CommandArg::text`], with its
            /// namespace). Since ABI 1.5.
            IDENTIFIER = 15,
            /// A block state with optional NBT, as typed ([`CommandArg::text`]). Since ABI 1.5.
            BLOCK_STATE = 16,
            /// An item with optional components, as typed ([`CommandArg::text`]), e.g.
            /// `diamond_sword[damage=5]`. Since ABI 1.5.
            ITEM_STACK = 17,
            /// A text component in SNBT or JSON, as typed ([`CommandArg::text`]). Since
            /// ABI 1.5.
            COMPONENT = 18,
            /// An SNBT compound, as typed ([`CommandArg::text`]). Since ABI 1.5.
            NBT_COMPOUND = 19,
            /// A game mode ([`CommandArg::int`], one of [`game_mode`]). Since ABI 1.5.
            GAME_MODE = 20,
            /// A duration like `5s` or `2d`, in ticks ([`CommandArg::int`]). Since ABI 1.5.
            TIME = 21,
            /// A UUID in its text form ([`CommandArg::text`]). Since ABI 1.5.
            UUID = 22,
        }
        /// Command senders ([`Sender::kind`]).
        pub mod sender: u32 {
            /// The server console.
            CONSOLE = 0,
            /// A player ([`Sender::player`]).
            PLAYER = 1,
            /// A command block or command block minecart. Since ABI 1.5.
            COMMAND_BLOCK = 2,
            /// An entity that is no player, e.g. through `execute as` ([`Sender::player`] holds
            /// its UUID). Since ABI 1.5.
            ENTITY = 3,
        }
        /// Answers of a permission provider.
        pub mod permission: u32 {
            /// Denied.
            DENY = 0,
            /// Granted.
            ALLOW = 1,
            /// Not decided: the server's default applies (operators and the console).
            UNDEFINED = 2,
        }
        /// Special values.
        pub mod limits: u32 {
            /// [`CommandNode::parent`] of top-level nodes.
            ROOT = u32::MAX,
            /// No world ([`Owner::world`], [`Sender::world`]).
            NO_WORLD = u32::MAX,
            /// A block state id that is no block: written by `blocks_get` for positions
            /// outside loaded chunks, skipped by `blocks_set`. Since ABI 1.2.
            NO_BLOCK = u32::MAX,
            /// The most blocks one `blocks_get` or `blocks_set` call covers. Since ABI 1.2.
            MAX_BOX_VOLUME = 1 << 24,
            /// The first event kind `event_register` hands out. Since ABI 1.5.
            FIRST_CUSTOM_EVENT = 192,
            /// The last event kind `event_register` hands out. Since ABI 1.5.
            LAST_CUSTOM_EVENT = 255,
            /// The longest plugin message in bytes (Vanilla's limit of client payloads).
            /// Since ABI 1.5.
            MAX_PLUGIN_MESSAGE = 32767,
            /// The most channels a plugin registers. Since ABI 1.5.
            MAX_CHANNELS = 128,
            /// The longest SNBT text the host parses (entity, item and block entity data).
            /// Since ABI 1.5.
            MAX_SNBT = 1 << 21,
        }
        /// Why a login was refused ([`LoginEvent::reason`]). Since ABI 1.4.
        pub mod login_reason: u32 {
            /// Not refused.
            ALLOWED = 0,
            /// The player or its address is banned.
            BANNED = 1,
            /// The whitelist is on and the player is not on it.
            NOT_WHITELISTED = 2,
            /// The server is full.
            FULL = 3,
            /// Refused by a plugin.
            PLUGIN = 4,
        }
        /// What a player does with its item ([`InteractEvent::action`]). Since ABI 1.4.
        pub mod interact: u32 {
            /// Uses the item on a block (right click on a block).
            USE_ON_BLOCK = 0,
            /// Uses the item in the air (right click).
            USE_ITEM = 1,
        }
        /// Hands ([`InteractEvent::hand`]). Since ABI 1.4.
        pub mod hand: u32 {
            /// The main hand.
            MAIN = 0,
            /// The off hand.
            OFF = 1,
        }
        /// Game modes ([`GameModeEvent`]). Since ABI 1.4.
        pub mod game_mode: u32 {
            /// Survival.
            SURVIVAL = 0,
            /// Creative.
            CREATIVE = 1,
            /// Adventure.
            ADVENTURE = 2,
            /// Spectator.
            SPECTATOR = 3,
        }
        /// Value types of persistent data, the NBT tag ids they are stored as. Numbers and
        /// arrays are little-endian, strings UTF-8. Since ABI 1.4.
        pub mod data_type: u32 {
            /// One byte (`i8`).
            BYTE = 1,
            /// `i16`.
            SHORT = 2,
            /// `i32`.
            INT = 3,
            /// `i64`.
            LONG = 4,
            /// `f32`.
            FLOAT = 5,
            /// `f64`.
            DOUBLE = 6,
            /// Bytes.
            BYTE_ARRAY = 7,
            /// A UTF-8 string.
            STRING = 8,
            /// `i32` values.
            INT_ARRAY = 11,
            /// `i64` values.
            LONG_ARRAY = 12,
        }
        /// Limits of persistent data. Since ABI 1.4.
        pub mod data_limits: u32 {
            /// The longest value in bytes.
            MAX_VALUE = 1 << 16,
            /// The most keys of one entity.
            MAX_KEYS = 256,
            /// The longest key in bytes.
            MAX_KEY = 256,
        }
        /// Sound categories ([`SoundSpec::source`], `player_stop_sound`), as `/playsound`'s
        /// sources. Since ABI 1.5.
        pub mod sound_source: u32 {
            /// `master`.
            MASTER = 0,
            /// `music`.
            MUSIC = 1,
            /// `record`.
            RECORD = 2,
            /// `weather`.
            WEATHER = 3,
            /// `block`.
            BLOCK = 4,
            /// `hostile`.
            HOSTILE = 5,
            /// `neutral`.
            NEUTRAL = 6,
            /// `player`.
            PLAYER = 7,
            /// `ambient`.
            AMBIENT = 8,
            /// `voice`.
            VOICE = 9,
            /// `ui`.
            UI = 10,
            /// Every category (`player_stop_sound` only).
            ANY = u32::MAX,
        }
        /// Properties of a player for `player_get` and `player_set`. Values are `f64`;
        /// flags are 0 or 1. Since ABI 1.5.
        pub mod player_prop: u32 {
            /// Health (0 kills the player).
            HEALTH = 1,
            /// Maximum health (the `max_health` attribute; read only).
            MAX_HEALTH = 2,
            /// Food level, 0 to 20.
            FOOD = 3,
            /// Saturation, 0 to the food level.
            SATURATION = 4,
            /// Exhaustion, 0 to 40.
            EXHAUSTION = 5,
            /// Experience level.
            XP_LEVEL = 6,
            /// Progress to the next level, 0 to 1.
            XP_PROGRESS = 7,
            /// Total experience points (read only).
            XP_TOTAL = 8,
            /// Game mode, one of [`game_mode`] (set: as `/gamemode`, with the change event).
            GAME_MODE = 9,
            /// Flying now.
            FLYING = 10,
            /// May fly.
            MAY_FLY = 11,
            /// Flying speed (Vanilla's default 0.05).
            FLY_SPEED = 12,
            /// Walking speed (Vanilla's default 0.1).
            WALK_SPEED = 13,
            /// The selected hotbar slot, 0 to 8.
            SELECTED_SLOT = 14,
            /// Air supply in ticks.
            AIR = 15,
            /// Ticks it still burns.
            FIRE_TICKS = 16,
        }
        /// Flags of `entity_effect_add`. Since ABI 1.5.
        pub mod effect_flag: u32 {
            /// Ambient (as from a beacon).
            AMBIENT = 1,
            /// Without particles.
            HIDE_PARTICLES = 2,
            /// Without the icon in the inventory and HUD.
            HIDE_ICON = 4,
        }
        /// Scoreboard display slots (`objective_display`), Vanilla's `DisplaySlot` ids; the
        /// team sidebars follow as `SIDEBAR_TEAM_BLACK + colour index`. Since ABI 1.5.
        pub mod display_slot: u32 {
            /// The player list.
            LIST = 0,
            /// The sidebar.
            SIDEBAR = 1,
            /// Below the name.
            BELOW_NAME = 2,
            /// The sidebar of members of black teams (+ colour index up to 15, white).
            SIDEBAR_TEAM_BLACK = 3,
        }
        /// Properties of a boss bar for `bossbar_set`. Since ABI 1.5.
        pub mod bossbar_prop: u32 {
            /// The value, 0 to the maximum.
            VALUE = 0,
            /// The maximum, at least 1.
            MAX = 1,
            /// The colour, one of [`bossbar_color`].
            COLOR = 2,
            /// The style, one of [`bossbar_style`].
            STYLE = 3,
            /// Visible (0 or 1).
            VISIBLE = 4,
        }
        /// Boss bar colours. Since ABI 1.5.
        pub mod bossbar_color: u32 {
            /// Pink.
            PINK = 0,
            /// Blue.
            BLUE = 1,
            /// Red.
            RED = 2,
            /// Green.
            GREEN = 3,
            /// Yellow.
            YELLOW = 4,
            /// Purple.
            PURPLE = 5,
            /// White.
            WHITE = 6,
        }
        /// Boss bar styles. Since ABI 1.5.
        pub mod bossbar_style: u32 {
            /// One bar.
            PROGRESS = 0,
            /// 6 notches.
            NOTCHED_6 = 1,
            /// 10 notches.
            NOTCHED_10 = 2,
            /// 12 notches.
            NOTCHED_12 = 3,
            /// 20 notches.
            NOTCHED_20 = 4,
        }
        /// The server's access lists (`access_set`, `access_get`). Since ABI 1.5.
        pub mod access_list: u32 {
            /// Operators (`ops.json`, level 4 on add).
            OPERATOR = 0,
            /// The whitelist.
            WHITELIST = 1,
            /// Banned players.
            BAN = 2,
            /// Banned IP addresses (the address in `name`).
            IP_BAN = 3,
            /// Whether the whitelist is on (`add` switches it on, otherwise off).
            WHITELIST_ON = 4,
        }
        /// Who has a registered permission node when no provider decides. Since ABI 1.5.
        pub mod permission_default: u32 {
            /// Nobody.
            FALSE = 0,
            /// Everyone.
            TRUE = 1,
            /// Operators (the default of unregistered nodes).
            OP = 2,
            /// Everyone but operators.
            NOT_OP = 3,
        }
        /// Weather (`world_set_weather`). Since ABI 1.5.
        pub mod weather: u32 {
            /// Clear.
            CLEAR = 0,
            /// Rain.
            RAIN = 1,
            /// Thunder.
            THUNDER = 2,
        }
        /// Protocol states of packets. Since ABI 1.6.
        pub mod packet_state: u32 {
            /// The first packet of a connection.
            HANDSHAKE = 0,
            /// The server list ping.
            STATUS = 1,
            /// Login, up to the client's acknowledgement.
            LOGIN = 2,
            /// Configuration (registries, resource packs).
            CONFIGURATION = 3,
            /// In a world.
            PLAY = 4,
        }
        /// Packet directions. Since ABI 1.6.
        pub mod packet_direction: u32 {
            /// From the server to a client.
            CLIENTBOUND = 0,
            /// From a client to the server.
            SERVERBOUND = 1,
        }
        /// Special packet ids. Since ABI 1.6.
        pub mod packet_id: i32 {
            /// [`PacketSpec::id`]: every packet of the state and direction.
            ANY = -1,
        }
        /// Flags of a packet listener ([`PacketSpec::flags`]). Since ABI 1.6.
        pub mod packet_listen: u32 {
            /// Clientbound only: the listener does not depend on the receiver. The host calls
            /// it once per encoded packet where the packet is encoded once for many receivers
            /// (then without a player, [`packet_event::BROADCAST`]); its decision holds for all
            /// of them, and a replacement is still one buffer shared by all. Broadcast
            /// listeners run before the listeners per receiver.
            BROADCAST = 1,
            /// The listener is skipped while the packet is cancelled.
            IGNORE_CANCELLED = 2,
        }
        /// Flags of a packet event ([`PacketEvent::flags`]). Since ABI 1.6.
        pub mod packet_event: u32 {
            /// The packet is encoded once for many receivers and this call decides for all of
            /// them: [`PacketEvent::player`] is nil and [`PacketEvent::connection`] 0.
            BROADCAST = 1,
            /// An earlier listener replaced the body.
            REPLACED = 2,
        }
        /// Flags of `packet_send` and `packet_receive`. Since ABI 1.6.
        pub mod packet_send: u32 {
            /// The packet passes no packet listener.
            SKIP_LISTENERS = 1,
        }
        /// Limits of the packet interface. Since ABI 1.6.
        pub mod packet_limits: u32 {
            /// The longest packet body in bytes (Vanilla's limit of an uncompressed packet).
            MAX_BODY = 1 << 23,
        }
        /// How `block_set_id` and `blocks_set` change blocks. Since ABI 1.2.
        pub mod set_mode: u32 {
            /// Like `block_set`: Vanilla's `setBlock` with neighbour and shape updates.
            UPDATE = 0,
            /// Without neighbour and shape updates (no physics); clients still see the change.
            NO_PHYSICS = 1,
        }
    }
    structs {
        /// A borrowed UTF-8 string, valid for the call.
        pub struct Str {
            /// First byte (may dangle when `len` is 0).
            pub ptr: *const u8,
            /// Length in bytes.
            pub len: usize,
        }
        /// Borrowed bytes, valid for the call.
        pub struct Bytes {
            /// First byte (may dangle when `len` is 0).
            pub ptr: *const u8,
            /// Length in bytes.
            pub len: usize,
        }
        /// A caller-owned output buffer for a string. The callee writes up to `cap` bytes and
        /// sets `len` to the full length; when `len > cap` the caller retries with more room.
        pub struct StrBuf {
            /// The buffer.
            pub ptr: *mut u8,
            /// Its capacity in bytes.
            pub cap: usize,
            /// Set by the callee: the full length of the string.
            pub len: usize,
        }
        /// A UUID as two big-endian halves (`hi` holds the first 8 bytes). Players and entities
        /// are identified by it; a UUID that is no longer present fails with
        /// [`status::NOT_FOUND`].
        pub struct Uuid {
            /// Most significant bits.
            pub hi: u64,
            /// Least significant bits.
            pub lo: u64,
        }
        /// A block position.
        pub struct BlockPos {
            /// X.
            pub x: i32,
            /// Y.
            pub y: i32,
            /// Z.
            pub z: i32,
        }
        /// A position.
        pub struct Vec3 {
            /// X.
            pub x: f64,
            /// Y.
            pub y: f64,
            /// Z.
            pub z: f64,
        }
        /// A box of block positions, both corners inclusive (`min` ≤ `max` on every axis).
        /// Block arrays over a box are indexed `((y - min.y) * size_z + (z - min.z)) * size_x +
        /// (x - min.x)` (as [`GenChunk`]). Since ABI 1.2.
        pub struct BlockBox {
            /// The lowest corner.
            pub min: BlockPos,
            /// The highest corner.
            pub max: BlockPos,
        }
        /// What a plugin library reports about itself before any other of its code runs.
        pub struct PluginInfo {
            /// Size of this record in bytes.
            pub size: u32,
            /// [`ABI_MAJOR`] the plugin was built against.
            pub abi_major: u16,
            /// [`ABI_MINOR`] the plugin was built against (the minimum it needs).
            pub abi_minor: u16,
            /// One of [`panic_strategy`]; only `UNWIND` is accepted.
            pub panic_strategy: u32,
            /// Data version of the Minecraft release a provider was built for; 0 when the
            /// plugin provides no system (providers work with numeric block state ids).
            pub data_version: i32,
            /// Version of the SDK that built the plugin.
            pub sdk_version: Str,
            /// Target triple the library was built for, e.g. `x86_64-unknown-linux-gnu`.
            pub target: Str,
            /// Registers commands, events and providers. Registries are writable only here.
            pub load: LoadFn,
            /// Starts the plugin after the worlds exist.
            pub enable: LifecycleFn,
            /// Stops the plugin at shutdown or before a development reload.
            pub disable: LifecycleFn,
            /// Development reload: writes the state the next build takes over.
            pub save_state: Option<SaveStateFn>,
            /// Development reload: takes over the state of the previous build.
            pub restore_state: Option<RestoreStateFn>,
        }
        /// A receiver of bytes.
        pub struct ByteSink {
            /// Size of this record in bytes.
            pub size: u32,
            /// Passed to `write`.
            pub state: *mut c_void,
            /// Receives a chunk of bytes; may be called several times.
            pub write: ByteSinkFn,
        }
        /// The start of every event record.
        pub struct EventHeader {
            /// Size of the whole event record in bytes.
            pub size: u32,
            /// One of [`event`].
            pub kind: u32,
            /// Non-zero when cancelled. Handlers of cancellable events may set or clear it.
            pub cancelled: u32,
        }
        /// [`event::PLAYER_JOIN`] and [`event::PLAYER_LEAVE`].
        pub struct PlayerEvent {
            /// Header.
            pub header: EventHeader,
            /// The player.
            pub player: Uuid,
            /// Its name.
            pub name: Str,
            /// Its world.
            pub world: u32,
            /// The join or leave message as a JSON text component (empty for none). Since
            /// ABI 1.5.
            pub message: Str,
            /// Replaces the join or leave message; an empty text shows none. Since ABI 1.5.
            pub message_out: *const TextOut,
        }
        /// [`event::PLAYER_CHAT`] and [`event::PLAYER_COMMAND`].
        pub struct PlayerTextEvent {
            /// Header.
            pub header: EventHeader,
            /// The player.
            pub player: Uuid,
            /// Its name.
            pub name: Str,
            /// The message, or the command line without `/`.
            pub text: Str,
        }
        /// One move of [`PlayersMovedEvent`].
        pub struct PlayerMove {
            /// The player.
            pub player: Uuid,
            /// Position at the start of the tick.
            pub from: Vec3,
            /// Position after its movement packets.
            pub to: Vec3,
        }
        /// [`event::PLAYERS_MOVED`]: all moves of one world in one tick.
        pub struct PlayersMovedEvent {
            /// Header.
            pub header: EventHeader,
            /// The world.
            pub world: u32,
            /// The moves (`moves[i]` at `moves + i * stride` bytes).
            pub moves: *const PlayerMove,
            /// Number of moves.
            pub count: usize,
            /// Size of one move record in bytes.
            pub stride: usize,
        }
        /// [`event::ENTITY_DAMAGE`].
        pub struct EntityDamageEvent {
            /// Header.
            pub header: EventHeader,
            /// The world.
            pub world: u32,
            /// The entity.
            pub entity: Uuid,
            /// Its type, e.g. `minecraft:player`.
            pub entity_type: Str,
            /// The damage type, e.g. `minecraft:fall`.
            pub damage_type: Str,
            /// The damage after armour-independent scaling; handlers may change it.
            pub amount: f32,
        }
        /// [`event::BLOCK_BREAK`], [`event::BLOCK_PLACE`], [`event::PLAYER_BED_ENTER`],
        /// [`event::PLAYER_BED_LEAVE`] and [`event::PLAYER_SPAWN_CHANGE`].
        pub struct BlockEvent {
            /// Header.
            pub header: EventHeader,
            /// The world.
            pub world: u32,
            /// The player.
            pub player: Uuid,
            /// The block.
            pub pos: BlockPos,
            /// The block state broken, or the one to be placed, e.g. `minecraft:oak_log[axis=y]`.
            pub state: Str,
        }
        /// Where a handler writes a replacement text. Since ABI 1.4.
        pub struct TextOut {
            /// Size of this record in bytes.
            pub size: u32,
            /// Passed to `set`.
            pub state: *mut c_void,
            /// Replaces the text; the last call wins.
            pub set: TextSetFn,
        }
        /// [`event::PLAYER_LOGIN`]. Since ABI 1.4.
        pub struct LoginEvent {
            /// Header; `cancelled` is non-zero while the login is refused.
            pub header: EventHeader,
            /// The player.
            pub player: Uuid,
            /// Its name.
            pub name: Str,
            /// Its IP address.
            pub address: Str,
            /// Why the server refused it, one of [`login_reason`] (`ALLOWED` when not).
            pub reason: u32,
            /// The refusal message as a JSON text component; empty when not refused.
            pub message: Str,
            /// Replaces the refusal message.
            pub message_out: *const TextOut,
        }
        /// [`event::PLAYER_DEATH`]. Since ABI 1.4.
        pub struct PlayerDeathEvent {
            /// Header (not cancellable).
            pub header: EventHeader,
            /// The world.
            pub world: u32,
            /// The player.
            pub player: Uuid,
            /// The damage type that killed it, e.g. `minecraft:fall`.
            pub damage_type: Str,
            /// The entity responsible ([`Uuid::NIL`] for none).
            pub killer: Uuid,
            /// The death message as a JSON text component; empty when none is shown.
            pub message: Str,
            /// Replaces the death message; an empty text shows none.
            pub message_out: *const TextOut,
            /// Non-zero when the player keeps its inventory and experience; handlers may
            /// change it.
            pub keep_inventory: u32,
        }
        /// [`event::ENTITY_DEATH`]. Since ABI 1.4.
        pub struct EntityDeathEvent {
            /// Header (not cancellable).
            pub header: EventHeader,
            /// The world.
            pub world: u32,
            /// The entity.
            pub entity: Uuid,
            /// Its type, e.g. `minecraft:zombie`.
            pub entity_type: Str,
            /// The damage type that killed it.
            pub damage_type: Str,
            /// The entity responsible ([`Uuid::NIL`] for none).
            pub killer: Uuid,
        }
        /// [`event::PLAYER_RESPAWN`] and [`event::PLAYER_CHANGE_WORLD`]. Since ABI 1.4.
        pub struct PlayerWorldEvent {
            /// Header (not cancellable).
            pub header: EventHeader,
            /// The player.
            pub player: Uuid,
            /// The world it came from.
            pub from: u32,
            /// The world it is in now.
            pub world: u32,
            /// Its position there.
            pub pos: Vec3,
        }
        /// [`event::PLAYER_GAME_MODE_CHANGE`]. Since ABI 1.4.
        pub struct GameModeEvent {
            /// Header.
            pub header: EventHeader,
            /// The player's world.
            pub world: u32,
            /// The player.
            pub player: Uuid,
            /// Its game mode, one of [`game_mode`].
            pub from: u32,
            /// The new game mode.
            pub to: u32,
        }
        /// [`event::PLAYER_INTERACT`]. Since ABI 1.4.
        pub struct InteractEvent {
            /// Header.
            pub header: EventHeader,
            /// The world.
            pub world: u32,
            /// The player.
            pub player: Uuid,
            /// One of [`interact`].
            pub action: u32,
            /// One of [`hand`].
            pub hand: u32,
            /// The item in that hand, e.g. `minecraft:flint_and_steel` (`minecraft:air` for none).
            pub item: Str,
            /// The block used (`USE_ON_BLOCK`).
            pub pos: BlockPos,
            /// The clicked face (`USE_ON_BLOCK`): 0 down, 1 up, 2 north, 3 south, 4 west, 5 east.
            pub face: u32,
            /// The block state used (`USE_ON_BLOCK`; empty otherwise).
            pub state: Str,
        }
        /// A title for `player_send_title`. Since ABI 1.4.
        pub struct TitleSpec {
            /// Size of this record in bytes.
            pub size: u32,
            /// The title; empty for none.
            pub title: Str,
            /// The subtitle; empty for none.
            pub subtitle: Str,
            /// Format of both, from [`text_format`].
            pub format: u32,
            /// Ticks to fade in.
            pub fade_in: i32,
            /// Ticks to stay.
            pub stay: i32,
            /// Ticks to fade out.
            pub fade_out: i32,
        }
        /// [`event::PLAYER_PLUGIN_MESSAGE`]. Since ABI 1.5.
        pub struct PluginMessageEvent {
            /// Header (not cancellable).
            pub header: EventHeader,
            /// The player's world.
            pub world: u32,
            /// The player.
            pub player: Uuid,
            /// The channel, e.g. `myplugin:sync`.
            pub channel: Str,
            /// The payload.
            pub data: Bytes,
        }
        /// [`event::PLAYER_ADVANCEMENT_DONE`]. Since ABI 1.5.
        pub struct AdvancementEvent {
            /// Header (not cancellable).
            pub header: EventHeader,
            /// The player's world.
            pub world: u32,
            /// The player.
            pub player: Uuid,
            /// The advancement, e.g. `minecraft:story/mine_stone`.
            pub advancement: Str,
        }
        /// An event of a kind from `event_register`, fired by a plugin with `event_fire`.
        /// Since ABI 1.5.
        pub struct CustomEvent {
            /// Header; cancellable.
            pub header: EventHeader,
            /// The name the kind was registered with.
            pub name: Str,
            /// The id of the plugin that fired it.
            pub source: Str,
            /// The payload, in a format the plugins agree on.
            pub data: Bytes,
        }
        /// A sound for `player_play_sound`. Since ABI 1.5.
        pub struct SoundSpec {
            /// Size of this record in bytes.
            pub size: u32,
            /// The sound event, e.g. `minecraft:entity.experience_orb.pickup` (also sounds of
            /// resource packs).
            pub sound: Str,
            /// One of [`sound_source`].
            pub source: u32,
            /// Where it plays.
            pub pos: Vec3,
            /// Volume (1 is normal; above 1 widens its range).
            pub volume: f32,
            /// Pitch, 0.5 to 2.
            pub pitch: f32,
            /// Seed of the sound's variant choice.
            pub seed: i64,
        }
        /// Particles for `player_spawn_particle`. Since ABI 1.5.
        pub struct ParticleSpec {
            /// Size of this record in bytes.
            pub size: u32,
            /// The particle as `/particle` takes it: its id with options as SNBT, e.g.
            /// `minecraft:dust{color:[1.0,0.0,0.0],scale:1.0}`.
            pub particle: Str,
            /// Where they appear.
            pub pos: Vec3,
            /// The spread on each axis (with `count` 0: the direction of motion).
            pub delta: Vec3,
            /// Speed.
            pub speed: f32,
            /// Count (0: one particle moving along `delta`).
            pub count: i32,
            /// Non-zero to show them up to 512 blocks away (`force`), else 32.
            pub force: u32,
        }
        /// What `player_info` reports about an online player. Since ABI 1.5.
        pub struct PlayerInfo {
            /// Size of this record in bytes.
            pub size: u32,
            /// Its world.
            pub world: u32,
            /// Its game mode, one of [`game_mode`].
            pub game_mode: u32,
            /// Its operator level, 0 for none.
            pub op_level: u32,
            /// Its latency in milliseconds, as the player list shows it.
            pub ping: i32,
        }
        /// What `server_stats` reports. Since ABI 1.5.
        pub struct ServerStats {
            /// Size of this record in bytes.
            pub size: u32,
            /// The server's tick count.
            pub tick: u64,
            /// Average milliseconds per tick over the last 100 ticks.
            pub mspt: f64,
            /// Ticks per second over the last 100 ticks (at most the tick rate).
            pub tps: f64,
            /// The tick rate (20 unless changed with `/tick rate`).
            pub tick_rate: f64,
            /// Online players.
            pub online: u32,
            /// The player limit.
            pub max_players: u32,
        }
        /// A place in a world. Since ABI 1.5.
        pub struct Location {
            /// The world.
            pub world: u32,
            /// The position.
            pub pos: Vec3,
            /// Yaw in degrees.
            pub yaw: f32,
            /// Pitch in degrees.
            pub pitch: f32,
        }
        /// Where a task runs.
        pub struct Owner {
            /// One of [`owner`].
            pub kind: u32,
            /// The world (`WORLD`, `LOCATION`, `ENTITY`).
            pub world: u32,
            /// The position (`LOCATION`).
            pub pos: BlockPos,
            /// The entity (`ENTITY`).
            pub entity: Uuid,
        }
        /// A task to schedule.
        pub struct TaskSpec {
            /// Size of this record in bytes.
            pub size: u32,
            /// Where it runs.
            pub owner: Owner,
            /// Ticks before the first run (0 = the owner's next turn).
            pub delay: u64,
            /// Ticks between runs; 0 runs it once. Background tasks repeat on a background
            /// thread for plugins built against ABI 1.4 or later (before, it is ignored).
            pub period: u64,
            /// The task.
            pub run: TaskFn,
            /// Frees `user` when the task is finished or cancelled.
            pub drop: Option<DropFn>,
            /// Passed to `run` and `drop`.
            pub user: *mut c_void,
        }
        /// One node of a command tree.
        pub struct CommandNode {
            /// Index of the parent in the node array, or [`limits::ROOT`]. Parents come first.
            pub parent: u32,
            /// One of [`node`].
            pub kind: u32,
            /// The literal, or the argument's name.
            pub name: Str,
            /// One of [`arg`] (arguments only).
            pub arg_type: u32,
            /// Non-zero when `min` applies.
            pub has_min: u32,
            /// Lower bound of numeric arguments.
            pub min: f64,
            /// Non-zero when `max` applies.
            pub has_max: u32,
            /// Upper bound of numeric arguments.
            pub max: f64,
            /// Permission level needed (0 = everyone .. 4 = owners).
            pub level: u32,
            /// Permission node needed besides the level; empty for none.
            pub permission: Str,
            /// The plugin's number of the command that runs when the input ends here; 0 = none.
            pub executes: u32,
            /// The plugin's number of the suggestion provider of this argument; 0 = none.
            pub suggests: u32,
        }
        /// A command tree to add to the server's command tree.
        pub struct CommandSpec {
            /// Size of this record in bytes.
            pub size: u32,
            /// The nodes (`nodes[i]` at `nodes + i * stride` bytes).
            pub nodes: *const CommandNode,
            /// Number of nodes.
            pub count: usize,
            /// Size of one node record in bytes.
            pub stride: usize,
            /// Runs the commands of this tree.
            pub execute: CommandFn,
            /// Answers the suggestion providers of this tree.
            pub suggest: Option<SuggestFn>,
            /// Passed to `execute` and `suggest`; never freed (commands live as long as the server).
            pub user: *mut c_void,
        }
        /// Who runs a command.
        pub struct Sender {
            /// One of [`sender`].
            pub kind: u32,
            /// The player (`PLAYER`).
            pub player: Uuid,
            /// The sender's world, or [`limits::NO_WORLD`].
            pub world: u32,
            /// The sender's position.
            pub pos: Vec3,
        }
        /// A parsed command argument.
        pub struct CommandArg {
            /// The argument's name.
            pub name: Str,
            /// One of [`arg`].
            pub kind: u32,
            /// Integer and boolean values.
            pub int: i64,
            /// Float values.
            pub float: f64,
            /// String values.
            pub text: Str,
            /// Block positions.
            pub pos: BlockPos,
            /// Players selected by `PLAYER` and `PLAYERS` arguments.
            pub players: *const Uuid,
            /// Number of players.
            pub player_count: usize,
            /// Positions and rotations (`VEC3`, `ROTATION`). Since ABI 1.5.
            pub vec: Vec3,
        }
        /// A command to run.
        pub struct CommandInvocation {
            /// Size of this record in bytes.
            pub size: u32,
            /// The plugin's number of the command ([`CommandNode::executes`]).
            pub command: u32,
            /// Who runs it.
            pub sender: Sender,
            /// The whole command line without `/`.
            pub line: Str,
            /// The arguments (`args[i]` at `args + i * stride` bytes).
            pub args: *const CommandArg,
            /// Number of arguments.
            pub arg_count: usize,
            /// Size of one argument record in bytes.
            pub arg_stride: usize,
        }
        /// A request for suggestions of a custom provider.
        pub struct SuggestRequest {
            /// Size of this record in bytes.
            pub size: u32,
            /// The plugin's number of the provider ([`CommandNode::suggests`]).
            pub provider: u32,
            /// Who asks.
            pub sender: Sender,
            /// The text of the argument typed so far.
            pub remaining: Str,
            /// The arguments parsed before this one.
            pub args: *const CommandArg,
            /// Number of arguments.
            pub arg_count: usize,
            /// Size of one argument record in bytes.
            pub arg_stride: usize,
        }
        /// Where suggestions go.
        pub struct SuggestSink {
            /// Size of this record in bytes.
            pub size: u32,
            /// Passed to `add`.
            pub state: *mut c_void,
            /// Adds one suggestion; the host filters by the typed prefix and the budget.
            pub add: SuggestAddFn,
        }
        /// The world a generator is created for.
        pub struct WorldInfo {
            /// Size of this record in bytes.
            pub size: u32,
            /// Dimension key, e.g. `minecraft:overworld`.
            pub dimension: Str,
            /// Lowest block Y.
            pub min_y: i32,
            /// Height in blocks.
            pub height: u32,
            /// World seed.
            pub seed: i64,
        }
        /// One chunk of a generator batch: a dense block array, pre-filled with air.
        pub struct GenChunk {
            /// Chunk X.
            pub x: i32,
            /// Chunk Z.
            pub z: i32,
            /// Numeric block state ids of the provider's data version, index
            /// `((y - min_y) * 16 + z) * 16 + x` with `x`, `z` in `0..16`.
            pub blocks: *mut u32,
            /// Number of entries (`16 * 16 * height`).
            pub len: usize,
        }
        /// A chunk generator provider (the `chunk-generator` slot). Called from many threads
        /// at once: instances must be thread-safe.
        pub struct ChunkGeneratorProvider {
            /// Size of this record in bytes.
            pub size: u32,
            /// Data version of the block state ids the provider writes.
            pub data_version: i32,
            /// Passed to `create`; never freed.
            pub user: *mut c_void,
            /// Creates the generator of a world.
            pub create: GeneratorCreateFn,
            /// Generates the terrain of a batch of chunks.
            pub generate: GeneratorRunFn,
            /// Destroys an instance.
            pub destroy: GeneratorDestroyFn,
        }
        /// A packet listener for `packet_subscribe`. Since ABI 1.6.
        pub struct PacketSpec {
            /// Size of this record in bytes.
            pub size: u32,
            /// One of [`packet_direction`].
            pub direction: u32,
            /// One of [`packet_state`].
            pub state: u32,
            /// The packet id in that state and direction (`packet_id` resolves a name), or
            /// [`packet_id::ANY`].
            pub id: i32,
            /// One of [`priority`], or any value between: listeners run from the lowest to the
            /// highest, then the monitors.
            pub priority: i32,
            /// [`packet_listen`] bits.
            pub flags: u32,
            /// The listener. It runs where packets are written and read: on network threads,
            /// for many connections at once (a broadcast listener also on the thread that
            /// encodes the packet), with a context that owns no world. The packets of one
            /// connection arrive in order.
            pub run: PacketFn,
            /// Frees `user` once the listener is removed and no call is running.
            pub drop: Option<DropFn>,
            /// Passed to `run` and `drop`.
            pub user: *mut c_void,
        }
        /// Where a packet listener writes a replacement body. Since ABI 1.6.
        pub struct PacketOut {
            /// Size of this record in bytes.
            pub size: u32,
            /// Passed to `set`.
            pub state: *mut c_void,
            /// Replaces the body (`id` + fields; the id may differ); the last call wins.
            pub set: PacketSetFn,
        }
        /// What a packet listener sees. Since ABI 1.6.
        pub struct PacketEvent {
            /// Size of this record in bytes.
            pub size: u32,
            /// One of [`packet_direction`].
            pub direction: u32,
            /// One of [`packet_state`].
            pub state: u32,
            /// The packet id the body starts with.
            pub id: i32,
            /// Non-zero when cancelled: a clientbound packet is not sent, a serverbound one
            /// not handled. Listeners may set or clear it.
            pub cancelled: u32,
            /// [`packet_event`] bits.
            pub flags: u32,
            /// The protocol version the body is encoded for (the server's own; clients of
            /// other versions are translated behind the listeners).
            pub protocol: i32,
            /// The connection, a number unique in this server run (0 in a broadcast call).
            pub connection: u64,
            /// The player the packet goes to or comes from; nil before its login finished and
            /// in a broadcast call.
            pub player: Uuid,
            /// The packet body: the id as a VarInt, then the fields.
            pub body: Bytes,
            /// Replaces the body. Monitors must not use it.
            pub out: *const PacketOut,
        }
        /// The host's functions for plugins.
        pub struct HostApi {
            /// Size of this record in bytes.
            pub size: u32,
            /// The host's [`ABI_MAJOR`].
            pub abi_major: u16,
            /// The host's [`ABI_MINOR`].
            pub abi_minor: u16,
            /// Logs a message under the plugin's name (`level` from [`log_level`]).
            pub log: unsafe extern "C" fn(PluginId, u32, Str),
            /// The plugin's data folder (`plugins/<id>/`), created by the host.
            pub data_dir: unsafe extern "C" fn(PluginId, *mut StrBuf) -> Status,
            /// Loads `config.toml` from the data folder; writes `defaults` there first when the
            /// file does not exist. Keys are read with the `config_*` functions.
            pub config_load: unsafe extern "C" fn(PluginId, Str) -> Status,
            /// A string value (dotted key, e.g. `messages.join`).
            pub config_string: unsafe extern "C" fn(PluginId, Str, *mut StrBuf) -> Status,
            /// An integer value.
            pub config_int: unsafe extern "C" fn(PluginId, Str, *mut i64) -> Status,
            /// A float value (integers are converted).
            pub config_float: unsafe extern "C" fn(PluginId, Str, *mut f64) -> Status,
            /// A boolean value (0 or 1).
            pub config_bool: unsafe extern "C" fn(PluginId, Str, *mut u32) -> Status,
            /// The value of a `vanillite.toml` rule as TOML text (e.g. `chat.max-pending-messages`).
            pub server_rule: unsafe extern "C" fn(PluginId, Str, *mut StrBuf) -> Status,
            /// The online players: writes up to `cap` UUIDs and the total count.
            pub online_players: unsafe extern "C" fn(PluginId, *mut Uuid, usize, *mut usize) -> Status,
            /// A player's name.
            pub player_name: unsafe extern "C" fn(PluginId, Uuid, *mut StrBuf) -> Status,
            /// A player's world.
            pub player_world: unsafe extern "C" fn(PluginId, Uuid, *mut u32) -> Status,
            /// Sends a system message (format from [`text_format`]).
            pub player_send_message: unsafe extern "C" fn(PluginId, Uuid, Str, u32) -> Status,
            /// Disconnects a player with a reason (format from [`text_format`]).
            pub player_kick: unsafe extern "C" fn(PluginId, Uuid, Str, u32) -> Status,
            /// Whether a player has a permission node (0 or 1).
            pub player_has_permission: unsafe extern "C" fn(PluginId, Uuid, Str, *mut u32) -> Status,
            /// Sends a system message to every player and the log.
            pub broadcast_message: unsafe extern "C" fn(PluginId, Str, u32) -> Status,
            /// The dimension key of a world; [`status::NOT_FOUND`] past the last world.
            pub world_name: unsafe extern "C" fn(PluginId, u32, *mut StrBuf) -> Status,
            /// The numeric id of a block state (`minecraft:stone`, `minecraft:oak_log[axis=x]`)
            /// in the server's data version, for providers.
            pub block_state_id: unsafe extern "C" fn(PluginId, Str, *mut u32) -> Status,
            /// Runs a console command in the next global phase.
            pub run_command: unsafe extern "C" fn(PluginId, Str) -> Status,
            /// Schedules a task; writes its id.
            pub schedule: unsafe extern "C" fn(PluginId, *const TaskSpec, *mut u64) -> Status,
            /// Cancels a task; its `drop` runs.
            pub cancel_task: unsafe extern "C" fn(PluginId, u64) -> Status,
            /// Subscribes to an event kind with a priority (`load` only).
            pub subscribe: unsafe extern "C" fn(PluginId, u32, i32, EventFn, *mut c_void) -> Status,
            /// Adds a command tree (`load` only).
            pub register_command: unsafe extern "C" fn(PluginId, *const CommandSpec) -> Status,
            /// Becomes the permission provider (`load` only; one per server).
            pub set_permission_provider: unsafe extern "C" fn(PluginId, PermissionFn, *mut c_void) -> Status,
            /// Fills the `chunk-generator` slot (`load` only).
            pub provide_chunk_generator:
                unsafe extern "C" fn(PluginId, *const ChunkGeneratorProvider) -> Status,
            /// The world this context owns; [`status::NOT_FOUND`] for global and background
            /// contexts.
            pub owner_world: unsafe extern "C" fn(*mut Context, *mut u32) -> Status,
            /// The block state at a position of the owned world, as text.
            pub block_get: unsafe extern "C" fn(*mut Context, u32, BlockPos, *mut StrBuf) -> Status,
            /// Sets a block state (text) with Vanilla's updates in the owned world.
            pub block_set: unsafe extern "C" fn(*mut Context, u32, BlockPos, Str) -> Status,
            /// The position of a player of the owned world.
            pub player_position: unsafe extern "C" fn(*mut Context, Uuid, *mut Vec3) -> Status,
            /// Sets the header and footer of a player's player list (tab list); both in the
            /// format from [`text_format`], empty text removes them. Since ABI 1.1.
            pub player_set_tab_list: unsafe extern "C" fn(PluginId, Uuid, Str, Str, u32) -> Status,
            /// The text of a numeric block state id (the inverse of `block_state_id`), e.g.
            /// `minecraft:oak_log[axis=y]`. Since ABI 1.2.
            pub block_state_name: unsafe extern "C" fn(PluginId, u32, *mut StrBuf) -> Status,
            /// The numeric block state id at a position of the owned world;
            /// [`status::NOT_FOUND`] outside loaded chunks. Since ABI 1.2.
            pub block_get_id: unsafe extern "C" fn(*mut Context, u32, BlockPos, *mut u32) -> Status,
            /// Sets a block state by numeric id in the owned world (`mode` from [`set_mode`]).
            /// Since ABI 1.2.
            pub block_set_id: unsafe extern "C" fn(*mut Context, u32, BlockPos, u32, u32) -> Status,
            /// Reads the block state ids of a box of the owned world into `out` (`cap` entries,
            /// at least the box's volume, at most [`limits::MAX_BOX_VOLUME`]); positions outside
            /// loaded chunks read [`limits::NO_BLOCK`]. Since ABI 1.2.
            pub blocks_get:
                unsafe extern "C" fn(*mut Context, u32, *const BlockBox, *mut u32, usize) -> Status,
            /// Sets the block states of a box of the owned world from `states` (`len` entries,
            /// the box's volume), in array order with `mode` from [`set_mode`]; entries of
            /// [`limits::NO_BLOCK`] and positions outside loaded chunks are skipped. Writes the
            /// number of blocks that changed to the last argument. Since ABI 1.2.
            pub blocks_set: unsafe extern "C" fn(
                *mut Context,
                u32,
                *const BlockBox,
                *const u32,
                usize,
                u32,
                *mut usize,
            ) -> Status,
            /// Shows a text above a player's hotbar (format from [`text_format`]). Since ABI 1.4.
            pub player_send_action_bar: unsafe extern "C" fn(PluginId, Uuid, Str, u32) -> Status,
            /// Shows a title and subtitle to a player. Since ABI 1.4.
            pub player_send_title: unsafe extern "C" fn(PluginId, Uuid, *const TitleSpec) -> Status,
            /// Reads a persistent data value of an entity of the owned world (players too):
            /// writes its type (from [`data_type`]) and its bytes to `sink`.
            /// [`status::NOT_FOUND`] when the entity is not there or has no such key. Keys are
            /// `namespace:path`; without a namespace the plugin's id is used. The values are
            /// saved with the entity in its NBT (`BukkitValues`). Since ABI 1.4.
            pub entity_data_get: unsafe extern "C" fn(
                *mut Context,
                u32,
                Uuid,
                Str,
                *mut u32,
                *const ByteSink,
            ) -> Status,
            /// Sets a persistent data value of an entity of the owned world (type from
            /// [`data_type`], at most [`data_limits::MAX_VALUE`] bytes, at most
            /// [`data_limits::MAX_KEYS`] keys per entity). Since ABI 1.4.
            pub entity_data_set:
                unsafe extern "C" fn(*mut Context, u32, Uuid, Str, u32, Bytes) -> Status,
            /// Removes a persistent data value of an entity of the owned world; OK when it
            /// had none. Since ABI 1.4.
            pub entity_data_remove: unsafe extern "C" fn(*mut Context, u32, Uuid, Str) -> Status,
            /// The persistent data keys of an entity of the owned world, one per line, to
            /// `sink`. Since ABI 1.4.
            pub entity_data_keys:
                unsafe extern "C" fn(*mut Context, u32, Uuid, *const ByteSink) -> Status,
            /// Subscribes to an event kind with a priority and flags from [`subscribe_flag`]
            /// (`load` only). Since ABI 1.5.
            pub subscribe_flags:
                unsafe extern "C" fn(PluginId, u32, i32, u32, EventFn, *mut c_void) -> Status,
            /// Removes the plugin's handlers of an event kind, at any time. Since ABI 1.5.
            pub unsubscribe: unsafe extern "C" fn(PluginId, u32) -> Status,
            /// The event kind of a plugin event name (e.g. `economy:balance_change`): the same
            /// kind for the same name in every plugin, from [`limits::FIRST_CUSTOM_EVENT`] on.
            /// Since ABI 1.5.
            pub event_register: unsafe extern "C" fn(PluginId, Str, *mut u32) -> Status,
            /// Fires a plugin event ([`CustomEvent`]) to the handlers of its kind on the calling
            /// thread (not to the firing plugin's own: plugins are never re-entered); `ctx`
            /// (may be null) is the caller's context, whose world the handlers may touch.
            /// Writes whether a handler cancelled it. Since ABI 1.5.
            pub event_fire:
                unsafe extern "C" fn(PluginId, *mut Context, u32, Bytes, *mut u32) -> Status,
            /// Offers a service to other plugins: a table of functions (any `repr(C)` record,
            /// static or alive until unregistered) under a name and version; replaces the
            /// plugin's earlier offer of that name. Since ABI 1.5.
            pub service_register:
                unsafe extern "C" fn(PluginId, Str, u32, *const c_void) -> Status,
            /// Withdraws a service of the plugin. Since ABI 1.5.
            pub service_unregister: unsafe extern "C" fn(PluginId, Str) -> Status,
            /// The newest table offered under a name and its version; [`status::NOT_FOUND`]
            /// when nobody offers it. Since ABI 1.5.
            pub service_get:
                unsafe extern "C" fn(PluginId, Str, *mut u32, *mut *const c_void) -> Status,
            /// The enabled plugins, one `id version` per line, in load order. Since ABI 1.5.
            pub plugin_list: unsafe extern "C" fn(PluginId, *const ByteSink) -> Status,
            /// Sets a key of the loaded `config.toml` to a value in TOML (`5`, `"text"`,
            /// `[1, 2]`); dotted keys create tables. Since ABI 1.5.
            pub config_set: unsafe extern "C" fn(PluginId, Str, Str) -> Status,
            /// Writes the loaded configuration to `config.toml`. Since ABI 1.5.
            pub config_save: unsafe extern "C" fn(PluginId) -> Status,
            /// A value of the configuration as TOML text (also lists and tables). Since
            /// ABI 1.5.
            pub config_toml: unsafe extern "C" fn(PluginId, Str, *mut StrBuf) -> Status,
            /// Registers a permission node with who has it when no provider decides (from
            /// [`permission_default`]); `node.*` patterns are not expanded. Since ABI 1.5.
            pub permission_register: unsafe extern "C" fn(PluginId, Str, u32) -> Status,
            /// What the server knows about an online player. Since ABI 1.5.
            pub player_info: unsafe extern "C" fn(PluginId, Uuid, *mut PlayerInfo) -> Status,
            /// An online player's IP address. Since ABI 1.5.
            pub player_address: unsafe extern "C" fn(PluginId, Uuid, *mut StrBuf) -> Status,
            /// Plays a sound to one player. Since ABI 1.5.
            pub player_play_sound: unsafe extern "C" fn(PluginId, Uuid, *const SoundSpec) -> Status,
            /// Stops a sound (empty: all) of a category ([`sound_source`], `ANY` for all) for
            /// a player. Since ABI 1.5.
            pub player_stop_sound: unsafe extern "C" fn(PluginId, Uuid, Str, u32) -> Status,
            /// Shows particles to one player. Since ABI 1.5.
            pub player_spawn_particle:
                unsafe extern "C" fn(PluginId, Uuid, *const ParticleSpec) -> Status,
            /// Sends a player to another server (host and port); the server must accept
            /// transfers. Since ABI 1.5.
            pub player_transfer: unsafe extern "C" fn(PluginId, Uuid, Str, u32) -> Status,
            /// Registers a channel the plugin receives plugin messages on, e.g. `myplugin:sync`
            /// (`load` only; at most [`limits::MAX_CHANNELS`]). Since ABI 1.5.
            pub register_channel: unsafe extern "C" fn(PluginId, Str) -> Status,
            /// Sends a plugin message (custom payload) to a player (at most 1 MiB). Since
            /// ABI 1.5.
            pub player_send_plugin_message:
                unsafe extern "C" fn(PluginId, Uuid, Str, Bytes) -> Status,
            /// Adds a scoreboard objective: name, criterion (e.g. `dummy`), display name.
            /// [`status::INVALID_ARGUMENT`] when it exists. Since ABI 1.5.
            pub objective_add: unsafe extern "C" fn(PluginId, Str, Str, Str, u32) -> Status,
            /// Removes an objective. Since ABI 1.5.
            pub objective_remove: unsafe extern "C" fn(PluginId, Str) -> Status,
            /// Shows an objective in a display slot ([`display_slot`]); an empty name clears
            /// the slot. Since ABI 1.5.
            pub objective_display: unsafe extern "C" fn(PluginId, u32, Str) -> Status,
            /// A score of a holder (a player name or an entity UUID) in an objective;
            /// [`status::NOT_FOUND`] when it has none. Since ABI 1.5.
            pub score_get: unsafe extern "C" fn(PluginId, Str, Str, *mut i32) -> Status,
            /// Sets a score. Since ABI 1.5.
            pub score_set: unsafe extern "C" fn(PluginId, Str, Str, i32) -> Status,
            /// Resets a holder's score in an objective (empty: in all). Since ABI 1.5.
            pub score_reset: unsafe extern "C" fn(PluginId, Str, Str) -> Status,
            /// Adds a team. Since ABI 1.5.
            pub team_add: unsafe extern "C" fn(PluginId, Str) -> Status,
            /// Removes a team. Since ABI 1.5.
            pub team_remove: unsafe extern "C" fn(PluginId, Str) -> Status,
            /// Puts a holder into a team (leaving its old one). Since ABI 1.5.
            pub team_join: unsafe extern "C" fn(PluginId, Str, Str) -> Status,
            /// Takes a holder out of its team. Since ABI 1.5.
            pub team_leave: unsafe extern "C" fn(PluginId, Str) -> Status,
            /// Changes a team option as `/team modify` names it (`displayName`, `color`,
            /// `prefix`, `suffix` take JSON text or a colour name; `friendlyFire`,
            /// `seeFriendlyInvisibles` take `true`/`false`; `nametagVisibility`,
            /// `deathMessageVisibility`, `collisionRule` their Vanilla values). Since ABI 1.5.
            pub team_modify: unsafe extern "C" fn(PluginId, Str, Str, Str) -> Status,
            /// The team of a holder; [`status::NOT_FOUND`] for none. Since ABI 1.5.
            pub team_of: unsafe extern "C" fn(PluginId, Str, *mut StrBuf) -> Status,
            /// Creates a boss bar (a Vanilla custom boss bar, saved with the world) with a
            /// key (`namespace:path`; without a namespace the plugin's id) and a name.
            /// Since ABI 1.5.
            pub bossbar_create: unsafe extern "C" fn(PluginId, Str, Str, u32) -> Status,
            /// Removes a boss bar. Since ABI 1.5.
            pub bossbar_remove: unsafe extern "C" fn(PluginId, Str) -> Status,
            /// Changes a boss bar property ([`bossbar_prop`]). Since ABI 1.5.
            pub bossbar_set: unsafe extern "C" fn(PluginId, Str, u32, i64) -> Status,
            /// Renames a boss bar. Since ABI 1.5.
            pub bossbar_set_name: unsafe extern "C" fn(PluginId, Str, Str, u32) -> Status,
            /// Shows (non-zero) or hides a boss bar for a player. Since ABI 1.5.
            pub bossbar_player: unsafe extern "C" fn(PluginId, Str, Uuid, u32) -> Status,
            /// Tick statistics and player counts. Since ABI 1.5.
            pub server_stats: unsafe extern "C" fn(PluginId, *mut ServerStats) -> Status,
            /// Replaces the server list's message of the day. Since ABI 1.5.
            pub server_set_motd: unsafe extern "C" fn(PluginId, Str, u32) -> Status,
            /// Adds to (non-zero) or removes from an access list ([`access_list`]): player
            /// UUID and name (IP address for `IP_BAN`), reason for bans. Since ABI 1.5.
            pub access_set: unsafe extern "C" fn(PluginId, u32, Uuid, Str, u32, Str) -> Status,
            /// Whether a player (or address) is on an access list (0 or 1). Since ABI 1.5.
            pub access_get: unsafe extern "C" fn(PluginId, u32, Uuid, Str, *mut u32) -> Status,
            /// The time of day of a world in ticks (`/time query daytime` without the
            /// modulo). Since ABI 1.5.
            pub world_time: unsafe extern "C" fn(PluginId, u32, *mut i64) -> Status,
            /// Sets the time of day (`/time set`). Since ABI 1.5.
            pub world_set_time: unsafe extern "C" fn(PluginId, u32, i64) -> Status,
            /// Sets the weather ([`weather`]) for a duration in ticks (-1: Vanilla's random
            /// duration), as `/weather`. Since ABI 1.5.
            pub world_set_weather: unsafe extern "C" fn(PluginId, u32, i32) -> Status,
            /// A game rule's value as `/gamerule` shows it, by its name
            /// (e.g. `keep_inventory`). Since ABI 1.5.
            pub game_rule_get: unsafe extern "C" fn(PluginId, Str, *mut StrBuf) -> Status,
            /// Sets a game rule (`true`, `false` or a number). Since ABI 1.5.
            pub game_rule_set: unsafe extern "C" fn(PluginId, Str, Str) -> Status,
            /// A property ([`player_prop`]) of a player of the owned world. Since ABI 1.5.
            pub player_get: unsafe extern "C" fn(*mut Context, u32, Uuid, u32, *mut f64) -> Status,
            /// Changes a property of a player of the owned world. Since ABI 1.5.
            pub player_set: unsafe extern "C" fn(*mut Context, u32, Uuid, u32, f64) -> Status,
            /// Where an entity (or player) of the owned world is. Since ABI 1.5.
            pub entity_location:
                unsafe extern "C" fn(*mut Context, u32, Uuid, *mut Location) -> Status,
            /// The type of an entity of the owned world, e.g. `minecraft:zombie`. Since
            /// ABI 1.5.
            pub entity_type: unsafe extern "C" fn(*mut Context, u32, Uuid, *mut StrBuf) -> Status,
            /// Teleports an entity of the owned world; players may go to another world
            /// (`Location::world`), other entities only within theirs. Since ABI 1.5.
            pub entity_teleport:
                unsafe extern "C" fn(*mut Context, u32, Uuid, *const Location) -> Status,
            /// Sets an entity's motion in blocks per tick. Since ABI 1.5.
            pub entity_set_velocity: unsafe extern "C" fn(*mut Context, u32, Uuid, Vec3) -> Status,
            /// Spawns an entity as `/summon` does (type, position, SNBT data or empty); writes
            /// its UUID. Since ABI 1.5.
            pub entity_spawn:
                unsafe extern "C" fn(*mut Context, u32, Str, Vec3, Str, *mut Uuid) -> Status,
            /// Removes an entity (not a player) of the owned world. Since ABI 1.5.
            pub entity_remove: unsafe extern "C" fn(*mut Context, u32, Uuid) -> Status,
            /// The entities of the owned world within a radius of a position, nearest first,
            /// optionally of one type (empty: any): writes up to `cap` UUIDs and the total
            /// count. Since ABI 1.5.
            pub entities_near: unsafe extern "C" fn(
                *mut Context,
                u32,
                Vec3,
                f64,
                Str,
                *mut Uuid,
                usize,
                *mut usize,
            ) -> Status,
            /// An entity's data as `/data get entity` shows it, as SNBT to the sink. Since
            /// ABI 1.5.
            pub entity_nbt:
                unsafe extern "C" fn(*mut Context, u32, Uuid, *const ByteSink) -> Status,
            /// Merges SNBT into an entity's data as `/data merge entity` (not players).
            /// Since ABI 1.5.
            pub entity_nbt_merge: unsafe extern "C" fn(*mut Context, u32, Uuid, Str) -> Status,
            /// Gives a living entity an effect (e.g. `minecraft:speed`): duration in ticks
            /// (-1: infinite), amplifier, flags from [`effect_flag`]. Since ABI 1.5.
            pub entity_effect_add:
                unsafe extern "C" fn(*mut Context, u32, Uuid, Str, i32, i32, u32) -> Status,
            /// Removes an effect (empty: all) from a living entity. Since ABI 1.5.
            pub entity_effect_remove: unsafe extern "C" fn(*mut Context, u32, Uuid, Str) -> Status,
            /// Gives a player an item as `/give` does (what does not fit drops): the item as
            /// SNBT, e.g. `{id:"minecraft:diamond",count:3}`. Since ABI 1.5.
            pub player_give: unsafe extern "C" fn(*mut Context, u32, Uuid, Str) -> Status,
            /// The item in a slot of an entity, slots named as `/item` does
            /// (`container.0`, `weapon.mainhand`, `armor.head`, `enderchest.3`, ...), as SNBT
            /// to the sink (`{}` for an empty slot). Since ABI 1.5.
            pub entity_item_get:
                unsafe extern "C" fn(*mut Context, u32, Uuid, Str, *const ByteSink) -> Status,
            /// Puts an item (SNBT, `{}` for none) into a slot of an entity. Since ABI 1.5.
            pub entity_item_set: unsafe extern "C" fn(*mut Context, u32, Uuid, Str, Str) -> Status,
            /// The block entity at a position as `/data get block` shows it, as SNBT to the
            /// sink; [`status::NOT_FOUND`] without one. Since ABI 1.5.
            pub block_entity_nbt:
                unsafe extern "C" fn(*mut Context, u32, BlockPos, *const ByteSink) -> Status,
            /// Merges SNBT into a block entity as `/data merge block`. Since ABI 1.5.
            pub block_entity_merge: unsafe extern "C" fn(*mut Context, u32, BlockPos, Str) -> Status,
            /// Grants (non-zero) or revokes a player's advancement criterion (empty: all of the
            /// advancement), as `/advancement only`. Since ABI 1.5.
            pub advancement_set:
                unsafe extern "C" fn(*mut Context, u32, Uuid, Str, Str, u32) -> Status,
            /// Whether a player has completed an advancement (0 or 1). Since ABI 1.5.
            pub advancement_done:
                unsafe extern "C" fn(*mut Context, u32, Uuid, Str, *mut u32) -> Status,
            /// Adds a packet listener; writes its handle. Allowed at any time and from any
            /// thread. Packet types without a listener cost the server nothing. Since ABI 1.6.
            pub packet_subscribe:
                unsafe extern "C" fn(PluginId, *const PacketSpec, *mut u64) -> Status,
            /// Removes a packet listener by its handle; calls that are running finish first.
            /// Since ABI 1.6.
            pub packet_unsubscribe: unsafe extern "C" fn(PluginId, u64) -> Status,
            /// The id of a packet by its name in a state ([`packet_state`]) and direction
            /// ([`packet_direction`]), e.g. `set_entity_data`. Ids change between Minecraft
            /// versions, names rarely. Since ABI 1.6.
            pub packet_id: unsafe extern "C" fn(PluginId, u32, u32, Str, *mut i32) -> Status,
            /// The name of a packet by its id. Since ABI 1.6.
            pub packet_name: unsafe extern "C" fn(PluginId, u32, u32, i32, *mut StrBuf) -> Status,
            /// Sends a play packet body (`id` + fields, the server's protocol version) to a
            /// player, in order with the server's packets from this thread; flags from
            /// [`packet_send`]. Since ABI 1.6.
            pub packet_send: unsafe extern "C" fn(PluginId, Uuid, Bytes, u32) -> Status,
            /// The protocol version packet bodies are encoded for. Since ABI 1.6.
            pub protocol_version: unsafe extern "C" fn(PluginId, *mut i32) -> Status,
            /// Feeds a serverbound play packet body (`id` + fields, the server's protocol
            /// version) into a player's connection as if the client sent it: it passes the
            /// packet listeners (unless [`packet_send::SKIP_LISTENERS`]), the validating
            /// decoder and the player's rate limits, and is handled in order with the
            /// client's packets. Fails with [`status::INVALID_ARGUMENT`] for a body that does
            /// not decode, [`status::NOT_FOUND`] for a player who is offline and
            /// [`status::ERROR`] while too many fed packets wait. Since ABI 1.7.
            pub packet_receive: unsafe extern "C" fn(PluginId, Uuid, Bytes, u32) -> Status,
        }
    }
}

impl Str {
    /// A string borrowed from `s`.
    pub const fn new(s: &str) -> Str {
        Str {
            ptr: s.as_ptr(),
            len: s.len(),
        }
    }

    /// The empty string.
    pub const EMPTY: Str = Str::new("");
}

impl Bytes {
    /// Bytes borrowed from `b`.
    pub const fn new(b: &[u8]) -> Bytes {
        Bytes {
            ptr: b.as_ptr(),
            len: b.len(),
        }
    }
}

impl Uuid {
    /// No UUID (all zero).
    pub const NIL: Uuid = Uuid { hi: 0, lo: 0 };

    /// From a 128-bit value.
    pub const fn from_u128(v: u128) -> Uuid {
        Uuid {
            hi: (v >> 64) as u64,
            lo: v as u64,
        }
    }

    /// As a 128-bit value.
    pub const fn to_u128(self) -> u128 {
        ((self.hi as u128) << 64) | self.lo as u128
    }
}

/// The size of a record as its `size` field holds it.
pub const fn size_of<T>() -> u32 {
    core::mem::size_of::<T>() as u32
}

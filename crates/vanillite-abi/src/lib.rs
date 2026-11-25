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
pub const ABI_MINOR: u16 = 0;
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
        }
        /// Command senders ([`Sender::kind`]).
        pub mod sender: u32 {
            /// The server console.
            CONSOLE = 0,
            /// A player ([`Sender::player`]).
            PLAYER = 1,
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
        /// [`event::BLOCK_BREAK`] and [`event::BLOCK_PLACE`].
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
            /// Ticks between runs; 0 runs it once. Ignored for background tasks.
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

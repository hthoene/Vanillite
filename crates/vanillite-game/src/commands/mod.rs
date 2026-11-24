//! The commands of the game: their syntax (Vanilla 26.3's Brigadier trees) and the keys the
//! game executes them by. Execution matches on [`CommandId`] and reads the typed arguments.

// SEC-07: command input handling denies the lints of the input-handling crates.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::as_conversions
)]

mod entities;
mod execute;
mod players;
mod server;
mod sound;
mod world;

pub use sound::SOUND_SOURCES;

pub use vanillite_command::Dispatcher;

/// Vanilla's permission levels (`PermissionLevel`).
pub mod level {
    /// Everyone.
    pub const ALL: u8 = 0;
    /// Moderators.
    pub const MODERATORS: u8 = 1;
    /// Game masters: selectors, game modes, teleports.
    pub const GAMEMASTERS: u8 = 2;
    /// Admins: kick, op, whitelist.
    pub const ADMINS: u8 = 3;
    /// Owners: stop, save-all, save-off, save-on.
    pub const OWNERS: u8 = 4;
}

/// Custom suggestion providers the game answers in `Source::suggest_custom` (Vanilla's
/// `.suggests(...)` lambdas; the client asks the server for them).
pub mod suggest {
    /// Online players that are not operators (`/op`).
    pub const OP_CANDIDATES: &str = "op.candidates";
    /// Names of operators (`/deop`).
    pub const OPS: &str = "ops";
    /// Online players that are not whitelisted (`/whitelist add`).
    pub const WHITELIST_CANDIDATES: &str = "whitelist.candidates";
    /// Whitelisted names (`/whitelist remove`).
    pub const WHITELISTED: &str = "whitelist.names";
    /// Time markers of the clock (`/time set <timemarker>`).
    pub const TIME_MARKERS: &str = "time.markers";
    /// Timelines of the clock (`/time query <timeline>`).
    pub const TIMELINES: &str = "time.timelines";
    /// Score holders: online players and tracked holders (`SUGGEST_SCORE_HOLDERS`).
    pub const SCORE_HOLDERS: &str = "score.holders";
    /// Objectives with the `trigger` criteria (`scoreboard players enable`).
    pub const TRIGGER_OBJECTIVES: &str = "score.triggers";
    /// Functions and function tags (`SUGGEST_FUNCTION`).
    pub const FUNCTIONS: &str = "functions";
    /// Command storage ids (`DataCommands.SUGGEST_STORAGE`).
    pub const STORAGE: &str = "storage";
    /// Boss bar ids.
    pub const BOSSBARS: &str = "bossbars";
    /// Stopwatch ids.
    pub const STOPWATCHES: &str = "stopwatches";
    /// Entity types `summon` may create; the client answers it itself.
    pub const SUMMONABLE_ENTITIES: &str = "minecraft:summonable_entities";
    /// Sound event ids; the client answers it itself.
    pub const AVAILABLE_SOUNDS: &str = "minecraft:available_sounds";
    /// Post effect ids; the client answers it itself.
    pub const POST_EFFECTS: &str = "minecraft:post_effects";
}

/// A score comparison of `execute if score`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompareOp {
    /// `=`.
    Eq,
    /// `<`.
    Lt,
    /// `<=`.
    Le,
    /// `>`.
    Gt,
    /// `>=`.
    Ge,
}

/// Where `data`-like arguments read or write (`DataCommands` providers).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DataKind {
    /// `entity <target>`.
    Entity,
    /// `block <pos>`.
    Block,
    /// `storage <id>`.
    Storage,
}

/// The numeric type of `execute store ... <path> <type> <scale>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NumType {
    Int,
    Float,
    Short,
    Long,
    Double,
    Byte,
}

/// What `execute store` writes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StoreTarget {
    /// `score <targets> <objective>`.
    Score,
    /// `bossbar <id> value`.
    BossbarValue,
    /// `bossbar <id> max`.
    BossbarMax,
    /// `entity|block|storage ... <path> <type> <scale>`.
    Data(DataKind, NumType),
}

/// A condition of `execute if|unless`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExecuteCondition {
    Block,
    Biome,
    Loaded,
    Dimension,
    /// `score <target> <objective> <op> <source> <objective>`.
    Score(CompareOp),
    /// `score <target> <objective> matches <range>`.
    ScoreMatches,
    /// `blocks <start> <end> <destination> all|masked` (`true` = masked).
    Blocks(bool),
    Entity,
    Predicate,
    Stopwatch,
    Data(DataKind),
    Items(DataKind),
    Slots(DataKind),
}

/// `execute on` relations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Relation {
    Owner,
    Leasher,
    Target,
    Attacker,
    Vehicle,
    Controller,
    Origin,
    Passengers,
}

/// The subcommands of `execute`: redirect modifiers, and the conditions also as commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExecuteOp {
    /// `if|unless <condition>`.
    Test {
        condition: ExecuteCondition,
        unless: bool,
    },
    As,
    At,
    Positioned,
    PositionedAs,
    PositionedOver,
    Rotated,
    RotatedAs,
    Facing,
    FacingEntity,
    Align,
    Anchored,
    In,
    Summon,
    On(Relation),
    /// `store result|success` (`result` = true).
    Store {
        target: StoreTarget,
        result: bool,
    },
}

/// The subcommands of `scoreboard`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScoreboardOp {
    ObjectivesList,
    ObjectivesAdd,
    ObjectivesRemove,
    ObjectiveDisplayName,
    RenderInteger,
    RenderHearts,
    DisplayAutoUpdate,
    ObjectiveFormatClear,
    ObjectiveFormatBlank,
    ObjectiveFormatFixed,
    ObjectiveFormatStyled,
    SetDisplayClear,
    SetDisplay,
    PlayersList,
    PlayersListTarget,
    Set,
    Get,
    Add,
    Remove,
    ResetAll,
    Reset,
    Enable,
    DisplayNameClear,
    DisplayName,
    PlayerFormatClear,
    PlayerFormatBlank,
    PlayerFormatFixed,
    PlayerFormatStyled,
    Operation,
}

/// The subcommands of `bossbar`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BossbarOp {
    Add,
    Remove,
    List,
    SetName,
    /// `set <id> color <color>`: the `BossBarColor` id.
    SetColor(u8),
    /// `set <id> style <style>`: the `BossBarOverlay` id.
    SetStyle(u8),
    SetValue,
    SetMax,
    SetVisible,
    /// `set <id> players [<targets>]`.
    SetPlayers,
    GetValue,
    GetMax,
    GetVisible,
    GetPlayers,
}

/// The subcommands of `posteffect`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PostEffectOp {
    Add,
    Clear,
    List,
    Remove,
}

/// What to execute. Variants that share a syntax with optional arguments say so; the
/// arguments are read by name from `Parsed::args`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommandId {
    /// `gamemode <gamemode>` for the source.
    GameModeSelf,
    /// `gamemode <gamemode> <target>`.
    GameModeTargets,
    /// `gamerule <rule>`: the index into [`RULES`](crate::survival::rules::RULES).
    GameRuleQuery(u16),
    /// `gamerule <rule> <value>`.
    GameRuleSet(u16),
    /// `help`.
    Help,
    /// `help <command>`.
    HelpCommand,
    /// `kick <targets> [<reason>]`.
    Kick,
    /// `kill`: the source.
    KillSelf,
    /// `kill <targets>`.
    KillTargets,
    /// `list`.
    List,
    /// `list uuids`.
    ListUuids,
    /// `msg|tell|w <targets> <message>`.
    Msg,
    /// `say <message>`.
    Say,
    /// `seed`.
    Seed,
    /// `teleport|tp <location>`: the source to a position.
    TeleportToLocation,
    /// `teleport|tp <destination>`: the source to an entity.
    TeleportToEntity,
    /// `teleport|tp <targets> <location> [<rotation> | facing (<facingLocation> | entity
    /// <facingEntity> [<facingAnchor>])]`.
    TeleportTargetsToLocation,
    /// `teleport|tp <targets> <destination>`.
    TeleportTargetsToEntity,
    /// `time [of <clock>] set <time>`.
    TimeSet,
    /// `time [of <clock>] set <timemarker>`.
    TimeSetMarker,
    /// `time [of <clock>] add <time>`.
    TimeAdd,
    /// `time [of <clock>] pause`.
    TimePause,
    /// `time [of <clock>] resume`.
    TimeResume,
    /// `time [of <clock>] rate <rate>`.
    TimeRate,
    /// `time [of <clock>] query time`.
    TimeQueryTime,
    /// `time [of <clock>] query <timeline>`.
    TimeQueryTimeline,
    /// `time [of <clock>] query <timeline> repetition`.
    TimeQueryRepetition,
    /// `time query gametime`.
    TimeQueryGameTime,
    /// `weather clear [<duration>]`.
    WeatherClear,
    /// `weather rain [<duration>]`.
    WeatherRain,
    /// `weather thunder [<duration>]`.
    WeatherThunder,
    /// `deop <targets>`.
    Deop,
    /// `op <targets>`.
    Op,
    /// `save-all`.
    SaveAll,
    /// `save-all flush`.
    SaveAllFlush,
    /// `save-off`.
    SaveOff,
    /// `save-on`.
    SaveOn,
    /// `stop`.
    Stop,
    /// `whitelist on`.
    WhitelistOn,
    /// `whitelist off`.
    WhitelistOff,
    /// `whitelist list`.
    WhitelistList,
    /// `whitelist add <targets>`.
    WhitelistAdd,
    /// `whitelist remove <targets>`.
    WhitelistRemove,
    /// `whitelist reload`.
    WhitelistReload,
    /// `execute ...`.
    Execute(ExecuteOp),
    /// `execute if function` (a custom modifier).
    ExecuteIfFunction,
    /// `execute unless function`.
    ExecuteUnlessFunction,
    /// `function <name>`.
    Function,
    /// `function <name> <arguments>`.
    FunctionWithArguments,
    /// `function <name> with <source>`.
    FunctionWith,
    /// `function <name> with <source> <path>`.
    FunctionWithPath,
    /// `return <value>`.
    ReturnValue,
    /// `return fail`.
    ReturnFail,
    /// `return run` (a custom modifier).
    ReturnRun,
    /// `scoreboard ...`.
    Scoreboard(ScoreboardOp),
    /// `bossbar ...`.
    Bossbar(BossbarOp),
    /// `damage <target> <amount> [<damageType> [at <location> | by <entity> [from <cause>]]]`.
    Damage,
    /// `effect clear [<targets> [<effect>]]`.
    EffectClear,
    /// `effect give <targets> <effect> [<seconds> | infinite] [<amplifier> [<hideParticles>]]`.
    EffectGive { infinite: bool },
    /// `swing [<targets> [mainhand | offhand [<animation> [<duration>]]]]`.
    Swing { offhand: bool },
    /// `particle <name> [<pos> [<delta> <speed> <count> [force | normal [<viewers>]]]]`.
    Particle { force: bool },
    /// `playsound <sound> [<source> [<targets> [<pos> ...]]]`: the index into
    /// [`SOUND_SOURCES`] (`master` without a source).
    PlaySound(u8),
    /// `posteffect ...`.
    PostEffect(PostEffectOp),
    /// `ride <target> mount <vehicle>`.
    RideMount,
    /// `ride <target> dismount`.
    RideDismount,
    /// `rotate <target> (<rotation> | facing (entity <facingEntity> [<facingAnchor>] |
    /// <facingLocation>))`.
    Rotate,
    /// `stopsound <targets> [* | <source>] [<sound>]`: the index into [`SOUND_SOURCES`].
    StopSound(Option<u8>),
    /// `summon <entity> [<pos> [<nbt>]]`.
    Summon,
}

/// Registers all commands in Vanilla's registration order (`Commands` constructor).
pub fn register(d: &mut Dispatcher<CommandId>) {
    execute::register_execute(d);
    entities::register_bossbar(d);
    entities::register_damage(d);
    entities::register_effect(d);
    execute::register_function(d);
    players::register_game_mode(d);
    world::register_game_rule(d);
    players::register_help(d);
    server::register_kick(d);
    world::register_kill(d);
    players::register_list(d);
    players::register_msg(d);
    entities::register_swing(d);
    sound::register_particle(d);
    sound::register_playsound(d);
    sound::register_posteffect(d);
    execute::register_return(d);
    entities::register_ride(d);
    entities::register_rotate(d);
    players::register_say(d);
    execute::register_scoreboard(d);
    world::register_seed(d);
    sound::register_stopsound(d);
    entities::register_summon(d);
    players::register_teleport(d);
    world::register_time(d);
    world::register_weather(d);
    server::register_deop(d);
    server::register_op(d);
    server::register_save_all(d);
    server::register_save_off(d);
    server::register_save_on(d);
    server::register_stop(d);
    server::register_whitelist(d);
}

/// A dispatcher with all commands.
pub fn dispatcher() -> Dispatcher<CommandId> {
    let mut d = Dispatcher::new();
    register(&mut d);
    d
}

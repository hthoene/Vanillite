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

mod admin;
mod chat;
mod data;
mod entities;
mod execute;
mod items;
mod players;
mod server;
mod sound;
mod system;
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
    /// Tags of the `targets` entities (`tag <targets> remove`).
    pub const ENTITY_TAGS: &str = "entity.tags";
    /// Modifier ids of an attribute of the `target` entity (`attribute ... modifier`).
    pub const ATTRIBUTE_MODIFIERS: &str = "attribute.modifiers";
    /// Trigger objectives the source has an unlocked score in (`TriggerCommand`).
    pub const TRIGGERS: &str = "trigger.objectives";
    /// Random sequences (`RandomCommand.suggestRandomSequence`).
    pub const RANDOM_SEQUENCES: &str = "random.sequences";
    /// Ids of scheduled functions (`ScheduleCommand.SUGGEST_SCHEDULE`).
    pub const SCHEDULED: &str = "schedule.ids";
    /// Names of banned players (`/pardon`).
    pub const BANNED_PLAYERS: &str = "bans.players";
    /// Banned IP addresses (`/pardon-ip`).
    pub const BANNED_IPS: &str = "bans.ips";
    /// Test instances (`TestCommand.suggestTestInstanceIds`).
    pub const TEST_INSTANCES: &str = "test.instances";
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

/// The subcommands of `attribute`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AttributeOp {
    /// `get [<scale>]`.
    Get,
    /// `base set <value>`.
    BaseSet,
    /// `base get [<scale>]`.
    BaseGet,
    /// `base reset`.
    BaseReset,
    /// `modifier add <id> <value> <operation>`.
    ModifierAdd(ModifierOperation),
    /// `modifier remove <id>`.
    ModifierRemove,
    /// `modifier value get <id> [<scale>]`.
    ModifierValueGet,
}

/// `AttributeModifier.Operation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModifierOperation {
    AddValue,
    AddMultipliedBase,
    AddMultipliedTotal,
}

/// The subcommands of `data` (the target kind is read from the arguments).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DataOp {
    /// `merge <target> <nbt>`.
    Merge,
    /// `get <target> [<path> [<scale>]]`.
    Get,
    /// `remove <target> <path>`.
    Remove,
    /// `modify <target> <targetPath> <operation> <source>`.
    Modify(ModifyOp, ModifySource),
}

/// How `data modify` changes the target path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModifyOp {
    /// `insert <index>`.
    Insert,
    Prepend,
    Append,
    Set,
    Merge,
}

/// Where `data modify` takes its values from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModifySource {
    /// `from <source> [<sourcePath>]`.
    From,
    /// `string <source> [<sourcePath> [<start> [<end>]]]`.
    String,
    /// `compute (default|block <computePos>|entity <computeTarget>) (float|integer)
    /// <provider>`.
    Compute,
    /// `value <value>`.
    Value,
}

/// The subcommands of `experience`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExperienceOp {
    Add,
    Set,
    Query,
}

/// The subcommands of `item`; `with <item>` or `from <source>` is read from the arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ItemOp {
    /// `replace`: the items in order, then nothing.
    Replace,
    /// `fill`: the items cycled.
    Fill,
    /// `override`: the items in order, then empty slots.
    Override,
    /// `modify <slots> <modifier>`.
    Modify,
}

/// The subcommands of `tag`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TagOp {
    Add,
    Remove,
    List,
}

/// The subcommands of `title <targets>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TitleOp {
    Clear,
    Reset,
    Title,
    Subtitle,
    Actionbar,
    Times,
}

/// The forms of `trigger <objective>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TriggerOp {
    /// Adds 1.
    Simple,
    Add,
    Set,
}

/// The subcommands of `team`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TeamOp {
    List,
    ListMembers,
    Add,
    Remove,
    Empty,
    /// `join <team>`: the source joins.
    JoinSelf,
    Join,
    Leave,
    DisplayName,
    ColorReset,
    Color,
    FriendlyFire,
    SeeFriendlyInvisibles,
    /// `nametagVisibility <visibility>` (the `Team.Visibility` id).
    NametagVisibility(u8),
    /// `deathMessageVisibility <visibility>` (the `Team.Visibility` id).
    DeathMessageVisibility(u8),
    /// `collisionRule <rule>` (the `Team.CollisionRule` id).
    CollisionRule(u8),
    Prefix,
    Suffix,
}

/// The subcommands of `random`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RandomOp {
    /// `value <range> [<sequence>]`.
    Value,
    /// `roll <range> [<sequence>]`.
    Roll,
    /// `reset *`.
    ResetAll,
    /// `reset * <seed> [<includeWorldSeed> [<includeSequenceId>]]`.
    ResetAllWithDefaults,
    /// `reset <sequence>`.
    Reset,
    /// `reset <sequence> <seed> [<includeWorldSeed> [<includeSequenceId>]]`.
    ResetWithSeed,
}

/// The subcommands of `stopwatch`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StopwatchOp {
    Create,
    Query,
    Restart,
    Remove,
}

/// The subcommands of `waypoint`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WaypointOp {
    List,
    Color,
    ColorHex,
    ColorReset,
    StyleReset,
    StyleSet,
}

/// The subcommands of `fetchprofile`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FetchProfileOp {
    Name,
    Id,
    Entity,
}

/// The subcommands of `test` (`TestCommand`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TestOp {
    Run,
    RunMultiple,
    RunThese,
    RunClosest,
    RunThat,
    RunFailed,
    Verify,
    Locate,
    ResetClosest,
    ResetThese,
    ResetThat,
    ClearThat,
    ClearThese,
    ClearAll,
    Stop,
    Pos,
    Create,
}

/// Which lists `banlist` shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BanListKind {
    /// Players, then IPs.
    All,
    Ips,
    Players,
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
    /// `attribute <target> <attribute> ...`.
    Attribute(AttributeOp),
    /// `clear [<targets> [<item> [<maxCount>]]]`.
    Clear,
    /// `data ...`.
    Data(DataOp),
    /// `enchant <targets> <enchantment> [<level>]`.
    Enchant,
    /// `experience|xp ...`; `levels` for the `levels` unit.
    Experience { op: ExperienceOp, levels: bool },
    /// `give <targets> <item> [<count>]`.
    Give,
    /// `item ...`.
    Item(ItemOp),
    /// `recipe give <targets> (<recipe>|*)`.
    RecipeGive,
    /// `recipe take <targets> (<recipe>|*)`.
    RecipeTake,
    /// `tag <targets> ...`.
    Tag(TagOp),
    /// `debug start`.
    DebugStart,
    /// `debug stop`.
    DebugStop,
    /// `debug function <name>` (works on the queue like `function`).
    DebugFunction,
    /// `dialog show <targets> <dialog>`.
    DialogShow,
    /// `dialog clear <targets>`.
    DialogClear,
    /// `me <action>`.
    Me,
    /// `random ...`.
    Random(RandomOp),
    /// `fetchprofile ...`.
    FetchProfile(FetchProfileOp),
    /// `schedule function <function> <time> [append|replace]` (`true` = replace).
    ScheduleFunction(bool),
    /// `schedule clear <function>`.
    ScheduleClear,
    /// `version`.
    Version,
    /// `spectate [<target> [<player>]]`.
    Spectate,
    /// `stopwatch ...`.
    Stopwatch(StopwatchOp),
    /// `team ...`.
    Team(TeamOp),
    /// `teammsg|tm <message>`.
    TeamMsg,
    /// `tellraw <targets> <message>`.
    TellRaw,
    /// `test ...`.
    Test(TestOp),
    /// `title <targets> ...`.
    Title(TitleOp),
    /// `trigger <objective> ...`.
    Trigger(TriggerOp),
    /// `waypoint ...`.
    Waypoint(WaypointOp),
    /// `jfr start`.
    JfrStart,
    /// `jfr stop`.
    JfrStop,
    /// `ban-ip <target> [<reason>]`.
    BanIp,
    /// `banlist [ips|players]`.
    BanList(BanListKind),
    /// `ban <targets> [<reason>]`.
    Ban,
    /// `pardon <targets>`.
    Pardon,
    /// `pardon-ip <target>`.
    PardonIp,
    /// `perf start`.
    PerfStart,
    /// `perf stop`.
    PerfStop,
    /// `setidletimeout <minutes>`.
    SetIdleTimeout,
    /// `transfer <hostname> [<port> [<players>]]`.
    Transfer,
}

/// Registers all commands in Vanilla's registration order (`Commands` constructor).
pub fn register(d: &mut Dispatcher<CommandId>) {
    items::register_attribute(d);
    execute::register_execute(d);
    entities::register_bossbar(d);
    items::register_clear(d);
    entities::register_damage(d);
    data::register_data(d);
    system::register_debug(d);
    chat::register_dialog(d);
    entities::register_effect(d);
    chat::register_me(d);
    items::register_enchant(d);
    items::register_experience(d);
    execute::register_function(d);
    players::register_game_mode(d);
    world::register_game_rule(d);
    items::register_give(d);
    players::register_help(d);
    items::register_item(d);
    server::register_kick(d);
    world::register_kill(d);
    players::register_list(d);
    players::register_msg(d);
    entities::register_swing(d);
    sound::register_particle(d);
    sound::register_playsound(d);
    sound::register_posteffect(d);
    system::register_random(d);
    items::register_recipe(d);
    system::register_fetch_profile(d);
    execute::register_return(d);
    entities::register_ride(d);
    entities::register_rotate(d);
    players::register_say(d);
    system::register_schedule(d);
    execute::register_scoreboard(d);
    world::register_seed(d);
    system::register_version(d);
    system::register_spectate(d);
    sound::register_stopsound(d);
    system::register_stopwatch(d);
    entities::register_summon(d);
    data::register_tag(d);
    chat::register_team(d);
    chat::register_team_msg(d);
    players::register_teleport(d);
    chat::register_tellraw(d);
    system::register_test(d);
    world::register_time(d);
    chat::register_title(d);
    chat::register_trigger(d);
    system::register_waypoint(d);
    world::register_weather(d);
    system::register_jfr(d);
    // Dedicated server only.
    admin::register_ban_ip(d);
    admin::register_ban_list(d);
    admin::register_ban(d);
    server::register_deop(d);
    server::register_op(d);
    admin::register_pardon(d);
    admin::register_pardon_ip(d);
    admin::register_perf(d);
    server::register_save_all(d);
    server::register_save_off(d);
    server::register_save_on(d);
    admin::register_set_idle_timeout(d);
    server::register_stop(d);
    admin::register_transfer(d);
    server::register_whitelist(d);
}

/// A dispatcher with all commands.
pub fn dispatcher() -> Dispatcher<CommandId> {
    let mut d = Dispatcher::new();
    register(&mut d);
    d
}

//! Syntax of the world commands: gamerule, kill, seed, time, weather.

use vanillite_command::{ArgumentType, Builder, Dispatcher, argument, literal};

use super::{CommandId, level, suggest};

/// `SeedCommand` on a dedicated server.
pub(super) fn register_seed(d: &mut Dispatcher<CommandId>) {
    d.register(
        literal("seed")
            .requires(level::GAMEMASTERS)
            .executes(CommandId::Seed),
    );
}

/// `TimeCommand.addClockNodes`: the actions on a clock (the default clock or `of <clock>`).
fn clock_nodes(node: Builder<CommandId>) -> Builder<CommandId> {
    node.then(
        literal("set")
            .then(argument("time", ArgumentType::Time { min: 0 }).executes(CommandId::TimeSet))
            .then(
                argument("timemarker", ArgumentType::Identifier)
                    .suggests(suggest::TIME_MARKERS)
                    .executes(CommandId::TimeSetMarker),
            ),
    )
    .then(
        literal("add").then(
            argument("time", ArgumentType::Time { min: i32::MIN }).executes(CommandId::TimeAdd),
        ),
    )
    .then(literal("pause").executes(CommandId::TimePause))
    .then(literal("resume").executes(CommandId::TimeResume))
    .then(
        literal("rate").then(
            argument(
                "rate",
                ArgumentType::Float {
                    min: Some(1.0e-5),
                    max: Some(1000.0),
                },
            )
            .executes(CommandId::TimeRate),
        ),
    )
    .then(
        literal("query")
            .then(literal("time").executes(CommandId::TimeQueryTime))
            .then(
                argument(
                    "timeline",
                    ArgumentType::Resource {
                        registry: "minecraft:timeline",
                    },
                )
                .suggests(suggest::TIMELINES)
                .executes(CommandId::TimeQueryTimeline)
                .then(literal("repetition").executes(CommandId::TimeQueryRepetition)),
            ),
    )
}

/// `TimeCommand`: Vanilla registers `time` twice; the second registration adds
/// `query gametime` and `of <clock>`, merged into the first.
pub(super) fn register_time(d: &mut Dispatcher<CommandId>) {
    d.register(clock_nodes(literal("time").requires(level::GAMEMASTERS)));
    d.register(
        literal("time")
            .requires(level::GAMEMASTERS)
            .then(literal("query").then(literal("gametime").executes(CommandId::TimeQueryGameTime)))
            .then(literal("of").then(clock_nodes(argument(
                "clock",
                ArgumentType::Resource {
                    registry: "minecraft:world_clock",
                },
            )))),
    );
}

/// The order `GameRules.visitGameRuleTypes` visits the rules in (the rules of disabled
/// features, `max_minecart_speed`, are left out).
const GAME_RULE_ORDER: [&str; 58] = [
    "command_block_output",
    "mob_explosion_drop_decay",
    "players_nether_portal_creative_delay",
    "immediate_respawn",
    "show_advancement_messages",
    "fire_spread_radius_around_player",
    "max_snow_accumulation_height",
    "fall_damage",
    "mob_griefing",
    "respawn_radius",
    "spawn_monsters",
    "elytra_movement_check",
    "advance_time",
    "tnt_explosion_drop_decay",
    "drowning_damage",
    "show_death_messages",
    "freeze_damage",
    "forgive_dead_players",
    "max_block_modifications",
    "log_admin_commands",
    "advance_weather",
    "water_source_conversion",
    "mob_drops",
    "spawner_blocks_work",
    "random_tick_speed",
    "allow_entering_nether_using_portals",
    "players_sleeping_percentage",
    "global_sound_events",
    "keep_inventory",
    "natural_health_regeneration",
    "max_command_sequence_length",
    "spectators_generate_chunks",
    "player_movement_check",
    "raids",
    "reduced_debug_info",
    "spawn_patrols",
    "block_explosion_drop_decay",
    "spread_vines",
    "block_drops",
    "fire_damage",
    "projectiles_can_break_blocks",
    "spawn_wardens",
    "pvp",
    "send_command_feedback",
    "spawn_phantoms",
    "locator_bar",
    "entity_drops",
    "spawn_mobs",
    "command_blocks_work",
    "tnt_explodes",
    "max_entity_cramming",
    "universal_anger",
    "spawn_wandering_traders",
    "ender_pearls_vanish_on_death",
    "limited_crafting",
    "players_nether_portal_default_delay",
    "lava_source_conversion",
    "max_command_forks",
];

/// `GameRuleCommand`: every rule by its plain and its qualified name, queried without and set
/// with a value.
pub(super) fn register_game_rule(d: &mut Dispatcher<CommandId>) {
    use crate::survival::rules::{RULES, Rule, RuleType};
    let mut base = literal("gamerule").requires(level::GAMEMASTERS);
    for name in GAME_RULE_ORDER {
        let Some(rule) = Rule::by_name(name) else {
            continue;
        };
        let Some(info) = RULES.get(rule.0) else {
            continue;
        };
        let Ok(index) = u16::try_from(rule.0) else {
            continue;
        };
        let value = || match info.kind {
            RuleType::Bool => ArgumentType::Bool,
            RuleType::Int { min, max } => ArgumentType::Integer {
                min: Some(min),
                max: (max != i32::MAX).then_some(max),
            },
        };
        for label in [name.to_owned(), format!("minecraft:{name}")] {
            base = base.then(
                literal(&label)
                    .executes(CommandId::GameRuleQuery(index))
                    .then(argument("value", value()).executes(CommandId::GameRuleSet(index))),
            );
        }
    }
    d.register(base);
}

/// `KillCommand`.
pub(super) fn register_kill(d: &mut Dispatcher<CommandId>) {
    d.register(
        literal("kill")
            .requires(level::GAMEMASTERS)
            .executes(CommandId::KillSelf)
            .then(
                argument(
                    "targets",
                    ArgumentType::Entity {
                        single: false,
                        players_only: false,
                    },
                )
                .executes(CommandId::KillTargets),
            ),
    );
}

/// `SummonCommand`.
pub(super) fn register_summon(d: &mut Dispatcher<CommandId>) {
    d.register(
        literal("summon").requires(level::GAMEMASTERS).then(
            argument(
                "entity",
                ArgumentType::Resource {
                    registry: "minecraft:entity_type",
                },
            )
            .suggests(suggest::SUMMONABLE_ENTITIES)
            .executes(CommandId::Summon)
            .then(
                argument("pos", ArgumentType::Vec3 { center: true })
                    .executes(CommandId::Summon)
                    .then(argument("nbt", ArgumentType::NbtCompound).executes(CommandId::Summon)),
            ),
        ),
    );
}

/// `WeatherCommand`.
pub(super) fn register_weather(d: &mut Dispatcher<CommandId>) {
    let kind = |name: &str, id: CommandId| {
        literal(name)
            .executes(id)
            .then(argument("duration", ArgumentType::Time { min: 1 }).executes(id))
    };
    d.register(
        literal("weather")
            .requires(level::GAMEMASTERS)
            .then(kind("clear", CommandId::WeatherClear))
            .then(kind("rain", CommandId::WeatherRain))
            .then(kind("thunder", CommandId::WeatherThunder)),
    );
}

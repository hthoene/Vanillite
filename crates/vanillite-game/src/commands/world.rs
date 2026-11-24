//! Syntax of the world commands: seed, time.

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

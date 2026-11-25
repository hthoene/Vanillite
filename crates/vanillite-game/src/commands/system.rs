//! Syntax of the system commands: debug, fetchprofile, jfr, random, schedule, spectate,
//! stopwatch, test, version, waypoint.

use vanillite_command::{ArgumentType, Builder, Dispatcher, StringKind, argument, literal};

use super::{CommandId, FetchProfileOp, RandomOp, StopwatchOp, TestOp, WaypointOp, level, suggest};

fn int() -> ArgumentType {
    ArgumentType::Integer {
        min: None,
        max: None,
    }
}

fn entity() -> ArgumentType {
    ArgumentType::Entity {
        single: true,
        players_only: false,
    }
}

/// `DebugCommand`.
pub(super) fn register_debug(d: &mut Dispatcher<CommandId>) {
    d.register(
        literal("debug")
            .requires(level::ADMINS)
            .then(literal("start").executes(CommandId::DebugStart))
            .then(literal("stop").executes(CommandId::DebugStop))
            .then(
                literal("function").requires(level::ADMINS).then(
                    argument("name", ArgumentType::Function)
                        .suggests(suggest::FUNCTIONS)
                        .executes(CommandId::DebugFunction),
                ),
            ),
    );
}

/// `RandomCommand`.
pub(super) fn register_random(d: &mut Dispatcher<CommandId>) {
    let draw = |name: &str, op: RandomOp| {
        literal(name).then(
            argument("range", ArgumentType::IntRange)
                .executes(CommandId::Random(op))
                .then(
                    argument("sequence", ArgumentType::Identifier)
                        .suggests(suggest::RANDOM_SEQUENCES)
                        .requires(level::GAMEMASTERS)
                        .executes(CommandId::Random(op)),
                ),
        )
    };
    let seed = |op: RandomOp| {
        let id = CommandId::Random(op);
        argument("seed", int()).executes(id).then(
            argument("includeWorldSeed", ArgumentType::Bool)
                .executes(id)
                .then(argument("includeSequenceId", ArgumentType::Bool).executes(id)),
        )
    };
    d.register(
        literal("random")
            .then(draw("value", RandomOp::Value))
            .then(draw("roll", RandomOp::Roll))
            .then(
                literal("reset")
                    .requires(level::GAMEMASTERS)
                    .then(
                        literal("*")
                            .executes(CommandId::Random(RandomOp::ResetAll))
                            .then(seed(RandomOp::ResetAllWithDefaults)),
                    )
                    .then(
                        argument("sequence", ArgumentType::Identifier)
                            .suggests(suggest::RANDOM_SEQUENCES)
                            .executes(CommandId::Random(RandomOp::Reset))
                            .then(seed(RandomOp::ResetWithSeed)),
                    ),
            ),
    );
}

/// `FetchProfileCommand`.
pub(super) fn register_fetch_profile(d: &mut Dispatcher<CommandId>) {
    let fetch = CommandId::FetchProfile;
    d.register(
        literal("fetchprofile")
            .requires(level::GAMEMASTERS)
            .then(
                literal("name").then(
                    argument("name", ArgumentType::String(StringKind::GreedyPhrase))
                        .executes(fetch(FetchProfileOp::Name)),
                ),
            )
            .then(
                literal("id")
                    .then(argument("id", ArgumentType::Uuid).executes(fetch(FetchProfileOp::Id))),
            )
            .then(
                literal("entity")
                    .then(argument("entity", entity()).executes(fetch(FetchProfileOp::Entity))),
            ),
    );
}

/// `ScheduleCommand`.
pub(super) fn register_schedule(d: &mut Dispatcher<CommandId>) {
    d.register(
        literal("schedule")
            .requires(level::GAMEMASTERS)
            .then(
                literal("function").then(
                    argument("function", ArgumentType::Function)
                        .suggests(suggest::FUNCTIONS)
                        .then(
                            argument("time", ArgumentType::Time { min: 0 })
                                .executes(CommandId::ScheduleFunction(true))
                                .then(
                                    literal("append").executes(CommandId::ScheduleFunction(false)),
                                )
                                .then(
                                    literal("replace").executes(CommandId::ScheduleFunction(true)),
                                ),
                        ),
                ),
            )
            .then(
                literal("clear").then(
                    argument("function", ArgumentType::Identifier)
                        .suggests(suggest::SCHEDULED)
                        .executes(CommandId::ScheduleClear),
                ),
            ),
    );
}

/// `VersionCommand` (with the dedicated server's permission check).
pub(super) fn register_version(d: &mut Dispatcher<CommandId>) {
    d.register(
        literal("version")
            .requires(level::GAMEMASTERS)
            .executes(CommandId::Version),
    );
}

/// `SpectateCommand`.
pub(super) fn register_spectate(d: &mut Dispatcher<CommandId>) {
    d.register(
        literal("spectate")
            .requires(level::GAMEMASTERS)
            .executes(CommandId::Spectate)
            .then(
                argument("target", entity())
                    .executes(CommandId::Spectate)
                    .then(
                        argument(
                            "player",
                            ArgumentType::Entity {
                                single: true,
                                players_only: true,
                            },
                        )
                        .executes(CommandId::Spectate),
                    ),
            ),
    );
}

/// `StopwatchCommand`.
pub(super) fn register_stopwatch(d: &mut Dispatcher<CommandId>) {
    let op = CommandId::Stopwatch;
    let id = || argument("id", ArgumentType::Identifier);
    d.register(
        literal("stopwatch")
            .requires(level::GAMEMASTERS)
            .then(literal("create").then(id().executes(op(StopwatchOp::Create))))
            .then(
                literal("query").then(
                    id().suggests(suggest::STOPWATCHES)
                        .then(
                            argument(
                                "scale",
                                ArgumentType::Double {
                                    min: None,
                                    max: None,
                                },
                            )
                            .executes(op(StopwatchOp::Query)),
                        )
                        .executes(op(StopwatchOp::Query)),
                ),
            )
            .then(
                literal("restart").then(
                    id().suggests(suggest::STOPWATCHES)
                        .executes(op(StopwatchOp::Restart)),
                ),
            )
            .then(
                literal("remove").then(
                    id().suggests(suggest::STOPWATCHES)
                        .executes(op(StopwatchOp::Remove)),
                ),
            ),
    );
}

fn test_op(op: TestOp) -> CommandId {
    CommandId::Test(op)
}

/// `TestCommand.runWithRetryOptions`: `[<numberOfTimes> [<untilFailed> ...]]`, `more` adds
/// to `<untilFailed>`.
fn with_retries(
    node: Builder<CommandId>,
    op: TestOp,
    more: impl FnOnce(Builder<CommandId>) -> Builder<CommandId>,
) -> Builder<CommandId> {
    node.executes(test_op(op)).then(
        argument(
            "numberOfTimes",
            ArgumentType::Integer {
                min: Some(0),
                max: None,
            },
        )
        .executes(test_op(op))
        .then(more(
            argument("untilFailed", ArgumentType::Bool).executes(test_op(op)),
        )),
    )
}

/// `TestCommand.runWithRetryOptionsAndBuildInfo`: `... [<rotationSteps> [<testsPerRow>]]`.
fn with_build_info(node: Builder<CommandId>, op: TestOp) -> Builder<CommandId> {
    with_retries(node, op, |until| {
        until.then(
            argument("rotationSteps", int())
                .executes(test_op(op))
                .then(argument("testsPerRow", int()).executes(test_op(op))),
        )
    })
}

/// `TestCommand`.
pub(super) fn register_test(d: &mut Dispatcher<CommandId>) {
    let tests = || {
        argument(
            "tests",
            ArgumentType::ResourceSelector {
                registry: "minecraft:test_instance",
            },
        )
        .suggests(suggest::TEST_INSTANCES)
    };
    let plain = |op| with_retries(literal(op_name(op)), op, |b| b);
    let run_failed = with_build_info(
        literal("runfailed").then(with_build_info(
            argument("onlyRequiredTests", ArgumentType::Bool),
            TestOp::RunFailed,
        )),
        TestOp::RunFailed,
    );
    d.register(
        literal("test")
            .requires(level::GAMEMASTERS)
            .then(literal("run").then(with_build_info(tests(), TestOp::Run)))
            .then(
                literal("runmultiple").then(
                    tests()
                        .executes(test_op(TestOp::RunMultiple))
                        .then(argument("amount", int()).executes(test_op(TestOp::RunMultiple))),
                ),
            )
            .then(plain(TestOp::RunThese))
            .then(plain(TestOp::RunClosest))
            .then(plain(TestOp::RunThat))
            .then(run_failed)
            .then(literal("verify").then(tests().executes(test_op(TestOp::Verify))))
            .then(literal("locate").then(tests().executes(test_op(TestOp::Locate))))
            .then(literal("resetclosest").executes(test_op(TestOp::ResetClosest)))
            .then(literal("resetthese").executes(test_op(TestOp::ResetThese)))
            .then(literal("resetthat").executes(test_op(TestOp::ResetThat)))
            .then(literal("clearthat").executes(test_op(TestOp::ClearThat)))
            .then(literal("clearthese").executes(test_op(TestOp::ClearThese)))
            .then(
                literal("clearall")
                    .executes(test_op(TestOp::ClearAll))
                    .then(argument("radius", int()).executes(test_op(TestOp::ClearAll))),
            )
            .then(literal("stop").executes(test_op(TestOp::Stop)))
            .then(
                literal("pos").executes(test_op(TestOp::Pos)).then(
                    argument("var", ArgumentType::String(StringKind::SingleWord))
                        .executes(test_op(TestOp::Pos)),
                ),
            )
            .then(
                literal("create").then(
                    argument("id", ArgumentType::Identifier)
                        .executes(test_op(TestOp::Create))
                        .then(
                            argument("width", int())
                                .executes(test_op(TestOp::Create))
                                .then(argument("height", int()).then(
                                    argument("depth", int()).executes(test_op(TestOp::Create)),
                                )),
                        ),
                ),
            ),
    );
}

fn op_name(op: TestOp) -> &'static str {
    match op {
        TestOp::RunThese => "runthese",
        TestOp::RunClosest => "runclosest",
        _ => "runthat",
    }
}

/// `WaypointCommand`.
pub(super) fn register_waypoint(d: &mut Dispatcher<CommandId>) {
    let op = CommandId::Waypoint;
    d.register(
        literal("waypoint")
            .requires(level::GAMEMASTERS)
            .then(literal("list").executes(op(WaypointOp::List)))
            .then(
                literal("modify").then(
                    argument("waypoint", entity())
                        .then(
                            literal("color")
                                .then(
                                    argument("color", ArgumentType::TeamColor)
                                        .executes(op(WaypointOp::Color)),
                                )
                                .then(
                                    literal("hex").then(
                                        argument("color", ArgumentType::HexColor)
                                            .executes(op(WaypointOp::ColorHex)),
                                    ),
                                )
                                .then(literal("reset").executes(op(WaypointOp::ColorReset))),
                        )
                        .then(
                            literal("style")
                                .then(literal("reset").executes(op(WaypointOp::StyleReset)))
                                .then(
                                    literal("set").then(
                                        argument("style", ArgumentType::Identifier)
                                            .executes(op(WaypointOp::StyleSet)),
                                    ),
                                ),
                        ),
                ),
            ),
    );
}

/// `JfrCommand` (registered when the JVM profiler is available, as on a dedicated server).
pub(super) fn register_jfr(d: &mut Dispatcher<CommandId>) {
    d.register(
        literal("jfr")
            .requires(level::OWNERS)
            .then(literal("start").executes(CommandId::JfrStart))
            .then(literal("stop").executes(CommandId::JfrStop)),
    );
}

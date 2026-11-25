//! Syntax of the commands on the state of a world or the server: defaultgamemode, difficulty,
//! forceload, locate, setworldspawn, spawnpoint, spreadplayers, tick, worldborder.

use vanillite_command::{ArgumentType, Dispatcher, argument, literal};

use super::{CommandId, ForceLoadOp, LocateOp, TickOp, WorldBorderOp, level, suggest};

/// `DefaultGameModeCommands`.
pub(super) fn register_default_game_mode(d: &mut Dispatcher<CommandId>) {
    d.register(
        literal("defaultgamemode")
            .requires(level::GAMEMASTERS)
            .then(
                argument("gamemode", ArgumentType::GameMode).executes(CommandId::DefaultGameMode),
            ),
    );
}

/// `DifficultyCommand`: one literal per `Difficulty`, by id.
pub(super) fn register_difficulty(d: &mut Dispatcher<CommandId>) {
    let mut node = literal("difficulty");
    for (id, name) in (0u8..).zip(["peaceful", "easy", "normal", "hard"]) {
        node = node.then(literal(name).executes(CommandId::DifficultySet(id)));
    }
    d.register(
        node.requires(level::GAMEMASTERS)
            .executes(CommandId::DifficultyQuery),
    );
}

/// `ForceLoadCommand`.
pub(super) fn register_force_load(d: &mut Dispatcher<CommandId>) {
    let change = |name: &str, op: ForceLoadOp| {
        literal(name).then(
            argument("from", ArgumentType::ColumnPos)
                .executes(CommandId::ForceLoad(op))
                .then(argument("to", ArgumentType::ColumnPos).executes(CommandId::ForceLoad(op))),
        )
    };
    d.register(
        literal("forceload")
            .requires(level::GAMEMASTERS)
            .then(change("add", ForceLoadOp::Add))
            .then(
                change("remove", ForceLoadOp::Remove)
                    .then(literal("all").executes(CommandId::ForceLoad(ForceLoadOp::RemoveAll))),
            )
            .then(
                literal("query")
                    .executes(CommandId::ForceLoad(ForceLoadOp::List))
                    .then(
                        argument("pos", ArgumentType::ColumnPos)
                            .executes(CommandId::ForceLoad(ForceLoadOp::Query)),
                    ),
            ),
    );
}

/// `LocateCommand`.
pub(super) fn register_locate(d: &mut Dispatcher<CommandId>) {
    d.register(
        literal("locate")
            .requires(level::GAMEMASTERS)
            .then(
                literal("structure").then(
                    argument(
                        "structure",
                        ArgumentType::ResourceOrTagKey {
                            registry: "minecraft:worldgen/structure",
                        },
                    )
                    .executes(CommandId::Locate(LocateOp::Structure)),
                ),
            )
            .then(
                literal("biome").then(
                    argument(
                        "biome",
                        ArgumentType::ResourceOrTag {
                            registry: "minecraft:worldgen/biome",
                        },
                    )
                    .executes(CommandId::Locate(LocateOp::Biome)),
                ),
            )
            .then(
                literal("poi").then(
                    argument(
                        "poi",
                        ArgumentType::ResourceOrTag {
                            registry: "minecraft:point_of_interest_type",
                        },
                    )
                    .executes(CommandId::Locate(LocateOp::Poi)),
                ),
            ),
    );
}

/// `SetSpawnCommand`.
pub(super) fn register_spawn_point(d: &mut Dispatcher<CommandId>) {
    d.register(
        literal("spawnpoint")
            .requires(level::GAMEMASTERS)
            .executes(CommandId::SpawnPoint)
            .then(
                argument(
                    "targets",
                    ArgumentType::Entity {
                        single: false,
                        players_only: true,
                    },
                )
                .executes(CommandId::SpawnPoint)
                .then(
                    argument("pos", ArgumentType::BlockPos)
                        .executes(CommandId::SpawnPoint)
                        .then(
                            argument("rotation", ArgumentType::Rotation)
                                .executes(CommandId::SpawnPoint),
                        ),
                ),
            ),
    );
}

/// `SetWorldSpawnCommand`.
pub(super) fn register_set_world_spawn(d: &mut Dispatcher<CommandId>) {
    d.register(
        literal("setworldspawn")
            .requires(level::GAMEMASTERS)
            .executes(CommandId::SetWorldSpawn)
            .then(
                argument("pos", ArgumentType::BlockPos)
                    .executes(CommandId::SetWorldSpawn)
                    .then(
                        argument("rotation", ArgumentType::Rotation)
                            .executes(CommandId::SetWorldSpawn),
                    ),
            ),
    );
}

/// `SpreadPlayersCommand`.
pub(super) fn register_spread_players(d: &mut Dispatcher<CommandId>) {
    let teams = || {
        argument("respectTeams", ArgumentType::Bool).then(
            argument(
                "targets",
                ArgumentType::Entity {
                    single: false,
                    players_only: false,
                },
            )
            .executes(CommandId::SpreadPlayers),
        )
    };
    d.register(
        literal("spreadplayers").requires(level::GAMEMASTERS).then(
            argument("center", ArgumentType::Vec2 { center: true }).then(
                argument(
                    "spreadDistance",
                    ArgumentType::Float {
                        min: Some(0.0),
                        max: None,
                    },
                )
                .then(
                    argument(
                        "maxRange",
                        ArgumentType::Float {
                            min: Some(1.0),
                            max: None,
                        },
                    )
                    .then(teams())
                    .then(
                        literal("under").then(
                            argument(
                                "maxHeight",
                                ArgumentType::Integer {
                                    min: None,
                                    max: None,
                                },
                            )
                            .then(teams()),
                        ),
                    ),
                ),
            ),
        ),
    );
}

/// `TickCommand`.
pub(super) fn register_tick(d: &mut Dispatcher<CommandId>) {
    let tick = CommandId::Tick;
    d.register(
        literal("tick")
            .requires(level::ADMINS)
            .then(literal("query").executes(tick(TickOp::Query)))
            .then(
                literal("rate").then(
                    argument(
                        "rate",
                        ArgumentType::Float {
                            min: Some(1.0),
                            max: Some(10000.0),
                        },
                    )
                    .suggests(suggest::TICK_RATE)
                    .executes(tick(TickOp::Rate)),
                ),
            )
            .then(
                literal("step")
                    .executes(tick(TickOp::Step))
                    .then(literal("stop").executes(tick(TickOp::StepStop)))
                    .then(
                        argument("time", ArgumentType::Time { min: 1 })
                            .suggests(suggest::TICK_STEP)
                            .executes(tick(TickOp::Step)),
                    ),
            )
            .then(
                literal("sprint")
                    .then(literal("stop").executes(tick(TickOp::SprintStop)))
                    .then(
                        argument("time", ArgumentType::Time { min: 1 })
                            .suggests(suggest::TICK_SPRINT)
                            .executes(tick(TickOp::Sprint)),
                    ),
            )
            .then(literal("unfreeze").executes(tick(TickOp::Unfreeze)))
            .then(literal("freeze").executes(tick(TickOp::Freeze))),
    );
}

/// `WorldBorderCommand`.
pub(super) fn register_world_border(d: &mut Dispatcher<CommandId>) {
    let border = CommandId::WorldBorder;
    // `WorldBorder.MAX_SIZE` (`5.999997E7F`).
    let max = 59_999_968.0;
    let size = |name: &str, op: WorldBorderOp| {
        literal(name).then(
            argument(
                "distance",
                ArgumentType::Double {
                    min: Some(-max),
                    max: Some(max),
                },
            )
            .executes(border(op))
            .then(argument("time", ArgumentType::Time { min: 0 }).executes(border(op))),
        )
    };
    let non_negative = ArgumentType::Float {
        min: Some(0.0),
        max: None,
    };
    d.register(
        literal("worldborder")
            .requires(level::GAMEMASTERS)
            .then(size("add", WorldBorderOp::Add))
            .then(size("set", WorldBorderOp::Set))
            .then(
                literal("center").then(
                    argument("pos", ArgumentType::Vec2 { center: true })
                        .executes(border(WorldBorderOp::Center)),
                ),
            )
            .then(
                literal("damage")
                    .then(
                        literal("amount").then(
                            argument("damagePerBlock", non_negative.clone())
                                .executes(border(WorldBorderOp::DamageAmount)),
                        ),
                    )
                    .then(
                        literal("buffer").then(
                            argument("distance", non_negative)
                                .executes(border(WorldBorderOp::DamageBuffer)),
                        ),
                    ),
            )
            .then(literal("get").executes(border(WorldBorderOp::Get)))
            .then(
                literal("warning")
                    .then(
                        literal("distance").then(
                            argument(
                                "distance",
                                ArgumentType::Integer {
                                    min: Some(0),
                                    max: None,
                                },
                            )
                            .executes(border(WorldBorderOp::WarningDistance)),
                        ),
                    )
                    .then(
                        literal("time").then(
                            argument("time", ArgumentType::Time { min: 0 })
                                .executes(border(WorldBorderOp::WarningTime)),
                        ),
                    ),
            ),
    );
}

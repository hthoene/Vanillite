//! Syntax of `execute`, `function`, `return` and `scoreboard`.

use vanillite_command::{ArgumentType, Builder, Dispatcher, NodeId, StringKind, argument, literal};

use super::{
    CommandId, CompareOp, DataKind, ExecuteCondition, ExecuteOp, NumType, Relation,
    ScoreboardOp, StoreTarget, level, suggest,
};

fn entities() -> ArgumentType {
    ArgumentType::Entity {
        single: false,
        players_only: false,
    }
}

fn entity() -> ArgumentType {
    ArgumentType::Entity {
        single: true,
        players_only: false,
    }
}

fn holders() -> ArgumentType {
    ArgumentType::ScoreHolder { multiple: true }
}

fn holder() -> ArgumentType {
    ArgumentType::ScoreHolder { multiple: false }
}

fn exec(op: ExecuteOp) -> CommandId {
    CommandId::Execute(op)
}

/// A condition node: forks to `execute` when it passes, or ends the command.
fn conditional(
    builder: Builder<CommandId>,
    execute: NodeId,
    condition: ExecuteCondition,
    unless: bool,
) -> Builder<CommandId> {
    let id = exec(ExecuteOp::Test { condition, unless });
    builder.fork(execute, id).executes(id)
}

/// `DataCommands.SOURCE_PROVIDERS` / `TARGET_PROVIDERS` around `then`.
fn data_sources(
    name: &str,
    then: impl Fn(DataKind) -> Builder<CommandId>,
    entity_type: ArgumentType,
) -> [Builder<CommandId>; 3] {
    [
        literal("entity").then(argument(name, entity_type).then(then(DataKind::Entity))),
        literal("block").then(argument(name, ArgumentType::BlockPos).then(then(DataKind::Block))),
        literal("storage").then(
            argument(name, ArgumentType::Identifier)
                .suggests(suggest::STORAGE)
                .then(then(DataKind::Storage)),
        ),
    ]
}

fn add_conditionals(
    literal_node: Builder<CommandId>,
    execute: NodeId,
    unless: bool,
) -> Builder<CommandId> {
    use ExecuteCondition as C;
    let compare = |op: &str, cmp: CompareOp| {
        literal(op).then(
            argument("source", holder())
                .suggests(suggest::SCORE_HOLDERS)
                .then(conditional(
                    argument("sourceObjective", ArgumentType::Objective),
                    execute,
                    C::Score(cmp),
                    unless,
                )),
        )
    };
    let mut l = literal_node
        .then(literal("block").then(argument("pos", ArgumentType::BlockPos).then(conditional(
            argument("block", ArgumentType::BlockPredicate),
            execute,
            C::Block,
            unless,
        ))))
        .then(literal("biome").then(argument("pos", ArgumentType::BlockPos).then(conditional(
            argument(
                "biome",
                ArgumentType::ResourceOrTag {
                    registry: "minecraft:worldgen/biome",
                },
            ),
            execute,
            C::Biome,
            unless,
        ))))
        .then(literal("loaded").then(conditional(
            argument("pos", ArgumentType::BlockPos),
            execute,
            C::Loaded,
            unless,
        )))
        .then(literal("dimension").then(conditional(
            argument("dimension", ArgumentType::Dimension),
            execute,
            C::Dimension,
            unless,
        )))
        .then(
            literal("score").then(
                argument("target", holder())
                    .suggests(suggest::SCORE_HOLDERS)
                    .then(
                        argument("targetObjective", ArgumentType::Objective)
                            .then(compare("=", CompareOp::Eq))
                            .then(compare("<", CompareOp::Lt))
                            .then(compare("<=", CompareOp::Le))
                            .then(compare(">", CompareOp::Gt))
                            .then(compare(">=", CompareOp::Ge))
                            .then(literal("matches").then(conditional(
                                argument("range", ArgumentType::IntRange),
                                execute,
                                C::ScoreMatches,
                                unless,
                            ))),
                    ),
            ),
        )
        .then(
            literal("blocks").then(
                argument("start", ArgumentType::BlockPos).then(
                    argument("end", ArgumentType::BlockPos).then(
                        argument("destination", ArgumentType::BlockPos)
                            .then(conditional(literal("all"), execute, C::Blocks(false), unless))
                            .then(conditional(literal("masked"), execute, C::Blocks(true), unless)),
                    ),
                ),
            ),
        )
        .then(literal("entity").then(conditional(
            argument("entities", entities()),
            execute,
            C::Entity,
            unless,
        )))
        .then(literal("predicate").then(conditional(
            argument("predicate", ArgumentType::LootPredicate),
            execute,
            C::Predicate,
            unless,
        )));
    let function = if unless {
        CommandId::ExecuteUnlessFunction
    } else {
        CommandId::ExecuteIfFunction
    };
    l = l
        .then(
            literal("function").then(
                argument("name", ArgumentType::Function)
                    .suggests(suggest::FUNCTIONS)
                    .fork(execute, function),
            ),
        )
        .then(
            literal("stopwatch").then(
                argument("id", ArgumentType::Identifier)
                    .suggests(suggest::STOPWATCHES)
                    .then(conditional(
                        argument("range", ArgumentType::FloatRange),
                        execute,
                        C::Stopwatch,
                        unless,
                    )),
            ),
        );
    let mut data = literal("data");
    for b in data_sources(
        "source",
        |kind| {
            conditional(
                argument("path", ArgumentType::NbtPath),
                execute,
                C::Data(kind),
                unless,
            )
        },
        entity(),
    ) {
        data = data.then(b);
    }
    l = l.then(data);
    let items = |slots_only: bool| {
        let then = |kind: DataKind| {
            if slots_only {
                conditional(
                    argument("slots", ArgumentType::SlotSource),
                    execute,
                    C::Slots(kind),
                    unless,
                )
            } else {
                argument("slots", ArgumentType::SlotSource).then(conditional(
                    argument("item_predicate", ArgumentType::ItemPredicate),
                    execute,
                    C::Items(kind),
                    unless,
                ))
            }
        };
        literal(if slots_only { "slots" } else { "items" })
            .then(literal("entity").then(argument("source", entities()).then(then(DataKind::Entity))))
            .then(
                literal("block")
                    .then(argument("source", ArgumentType::BlockPos).then(then(DataKind::Block))),
            )
    };
    l.then(items(false)).then(items(true))
}

fn wrap_stores(literal_node: Builder<CommandId>, execute: NodeId, result: bool) -> Builder<CommandId> {
    let store = |target: StoreTarget| exec(ExecuteOp::Store { target, result });
    let mut l = literal_node
        .then(
            literal("score").then(
                argument("targets", holders())
                    .suggests(suggest::SCORE_HOLDERS)
                    .then(
                        argument("objective", ArgumentType::Objective)
                            .redirect_with(execute, store(StoreTarget::Score)),
                    ),
            ),
        )
        .then(
            literal("bossbar").then(
                argument("id", ArgumentType::Identifier)
                    .suggests(suggest::BOSSBARS)
                    .then(literal("value").redirect_with(execute, store(StoreTarget::BossbarValue)))
                    .then(literal("max").redirect_with(execute, store(StoreTarget::BossbarMax))),
            ),
        );
    let path = |kind: DataKind| {
        let mut p = argument("path", ArgumentType::NbtPath);
        for (name, ty) in [
            ("int", NumType::Int),
            ("float", NumType::Float),
            ("short", NumType::Short),
            ("long", NumType::Long),
            ("double", NumType::Double),
            ("byte", NumType::Byte),
        ] {
            p = p.then(
                literal(name).then(
                    argument(
                        "scale",
                        ArgumentType::Double {
                            min: None,
                            max: None,
                        },
                    )
                    .redirect_with(execute, store(StoreTarget::Data(kind, ty))),
                ),
            );
        }
        p
    };
    for b in data_sources("target", path, entity()) {
        l = l.then(b);
    }
    l
}

/// `ExecuteCommand.register`.
pub(super) fn register_execute(d: &mut Dispatcher<CommandId>) {
    let execute = d.register(literal("execute").requires(level::GAMEMASTERS));
    let relation = |name: &str, r: Relation| literal(name).fork(execute, exec(ExecuteOp::On(r)));
    d.register(
        literal("execute")
            .requires(level::GAMEMASTERS)
            .then(literal("run").redirect(Dispatcher::<CommandId>::ROOT))
            .then(add_conditionals(literal("if"), execute, false))
            .then(add_conditionals(literal("unless"), execute, true))
            .then(literal("as").then(argument("targets", entities()).fork(execute, exec(ExecuteOp::As))))
            .then(literal("at").then(argument("targets", entities()).fork(execute, exec(ExecuteOp::At))))
            .then(
                literal("store")
                    .then(wrap_stores(literal("result"), execute, true))
                    .then(wrap_stores(literal("success"), execute, false)),
            )
            .then(
                literal("positioned")
                    .then(
                        argument("pos", ArgumentType::Vec3 { center: true })
                            .redirect_with(execute, exec(ExecuteOp::Positioned)),
                    )
                    .then(literal("as").then(
                        argument("targets", entities()).fork(execute, exec(ExecuteOp::PositionedAs)),
                    ))
                    .then(literal("over").then(
                        argument("heightmap", ArgumentType::Heightmap)
                            .redirect_with(execute, exec(ExecuteOp::PositionedOver)),
                    )),
            )
            .then(
                literal("rotated")
                    .then(
                        argument("rot", ArgumentType::Rotation)
                            .redirect_with(execute, exec(ExecuteOp::Rotated)),
                    )
                    .then(literal("as").then(
                        argument("targets", entities()).fork(execute, exec(ExecuteOp::RotatedAs)),
                    )),
            )
            .then(
                literal("facing")
                    .then(literal("entity").then(argument("targets", entities()).then(
                        argument("anchor", ArgumentType::EntityAnchor)
                            .fork(execute, exec(ExecuteOp::FacingEntity)),
                    )))
                    .then(
                        argument("pos", ArgumentType::Vec3 { center: true })
                            .redirect_with(execute, exec(ExecuteOp::Facing)),
                    ),
            )
            .then(literal("align").then(
                argument("axes", ArgumentType::Swizzle).redirect_with(execute, exec(ExecuteOp::Align)),
            ))
            .then(literal("anchored").then(
                argument("anchor", ArgumentType::EntityAnchor)
                    .redirect_with(execute, exec(ExecuteOp::Anchored)),
            ))
            .then(literal("in").then(
                argument("dimension", ArgumentType::Dimension)
                    .redirect_with(execute, exec(ExecuteOp::In)),
            ))
            .then(
                literal("summon").then(
                    argument(
                        "entity",
                        ArgumentType::Resource {
                            registry: "minecraft:entity_type",
                        },
                    )
                    .suggests(suggest::SUMMONABLE_ENTITIES)
                    .redirect_with(execute, exec(ExecuteOp::Summon)),
                ),
            )
            .then(
                literal("on")
                    .then(relation("owner", Relation::Owner))
                    .then(relation("leasher", Relation::Leasher))
                    .then(relation("target", Relation::Target))
                    .then(relation("attacker", Relation::Attacker))
                    .then(relation("vehicle", Relation::Vehicle))
                    .then(relation("controller", Relation::Controller))
                    .then(relation("origin", Relation::Origin))
                    .then(relation("passengers", Relation::Passengers)),
            ),
    );
}

/// `FunctionCommand.register`.
pub(super) fn register_function(d: &mut Dispatcher<CommandId>) {
    let mut with = literal("with");
    for b in data_sources(
        "source",
        |_| argument("path", ArgumentType::NbtPath).executes(CommandId::FunctionWithPath),
        entity(),
    ) {
        with = with.then(b);
    }
    // The source nodes themselves run the function with the whole compound.
    let with = mark_sources_executable(with);
    d.register(
        literal("function").requires(level::GAMEMASTERS).then(
            argument("name", ArgumentType::Function)
                .suggests(suggest::FUNCTIONS)
                .executes(CommandId::Function)
                .then(
                    argument("arguments", ArgumentType::NbtCompound)
                        .executes(CommandId::FunctionWithArguments),
                )
                .then(with),
        ),
    );
}

/// `with <entity|block|storage> <source>` run with the whole compound.
fn mark_sources_executable(with: Builder<CommandId>) -> Builder<CommandId> {
    with.map_children(|kind| kind.map_children(|source| source.executes(CommandId::FunctionWith)))
}

/// `ReturnCommand.register`.
pub(super) fn register_return(d: &mut Dispatcher<CommandId>) {
    d.register(
        literal("return")
            .requires(level::GAMEMASTERS)
            .then(
                argument(
                    "value",
                    ArgumentType::Integer {
                        min: None,
                        max: None,
                    },
                )
                .executes(CommandId::ReturnValue),
            )
            .then(literal("fail").executes(CommandId::ReturnFail))
            .then(
                literal("run").redirect_with(Dispatcher::<CommandId>::ROOT, CommandId::ReturnRun),
            ),
    );
}

fn sb(op: ScoreboardOp) -> CommandId {
    CommandId::Scoreboard(op)
}

/// `ScoreboardCommand.register`.
pub(super) fn register_scoreboard(d: &mut Dispatcher<CommandId>) {
    use ScoreboardOp as S;
    let int = |min: Option<i32>| ArgumentType::Integer { min, max: None };
    let number_format = |blank: S, fixed: S, styled: S| {
        [
            literal("blank").executes(sb(blank)),
            literal("fixed")
                .then(argument("contents", ArgumentType::Component).executes(sb(fixed))),
            literal("styled").then(argument("style", ArgumentType::Style).executes(sb(styled))),
        ]
    };
    let [ob, of, os] = number_format(
        S::ObjectiveFormatBlank,
        S::ObjectiveFormatFixed,
        S::ObjectiveFormatStyled,
    );
    let [pb, pf, ps] = number_format(
        S::PlayerFormatBlank,
        S::PlayerFormatFixed,
        S::PlayerFormatStyled,
    );
    d.register(
        literal("scoreboard")
            .requires(level::GAMEMASTERS)
            .then(
                literal("objectives")
                    .then(literal("list").executes(sb(S::ObjectivesList)))
                    .then(
                        literal("add").then(
                            argument("objective", ArgumentType::String(StringKind::SingleWord))
                                .then(
                                    argument("criteria", ArgumentType::ObjectiveCriteria)
                                        .executes(sb(S::ObjectivesAdd))
                                        .then(
                                            argument("displayName", ArgumentType::Component)
                                                .executes(sb(S::ObjectivesAdd)),
                                        ),
                                ),
                        ),
                    )
                    .then(
                        literal("modify").then(
                            argument("objective", ArgumentType::Objective)
                                .then(literal("displayname").then(
                                    argument("displayName", ArgumentType::Component)
                                        .executes(sb(S::ObjectiveDisplayName)),
                                ))
                                .then(
                                    literal("rendertype")
                                        .then(literal("integer").executes(sb(S::RenderInteger)))
                                        .then(literal("hearts").executes(sb(S::RenderHearts))),
                                )
                                .then(literal("displayautoupdate").then(
                                    argument("value", ArgumentType::Bool)
                                        .executes(sb(S::DisplayAutoUpdate)),
                                ))
                                .then(
                                    literal("numberformat")
                                        .executes(sb(S::ObjectiveFormatClear))
                                        .then(ob)
                                        .then(of)
                                        .then(os),
                                ),
                        ),
                    )
                    .then(literal("remove").then(
                        argument("objective", ArgumentType::Objective)
                            .executes(sb(S::ObjectivesRemove)),
                    ))
                    .then(
                        literal("setdisplay").then(
                            argument("slot", ArgumentType::ScoreboardSlot)
                                .executes(sb(S::SetDisplayClear))
                                .then(
                                    argument("objective", ArgumentType::Objective)
                                        .executes(sb(S::SetDisplay)),
                                ),
                        ),
                    ),
            )
            .then(
                literal("players")
                    .then(
                        literal("list").executes(sb(S::PlayersList)).then(
                            argument("target", holder())
                                .suggests(suggest::SCORE_HOLDERS)
                                .executes(sb(S::PlayersListTarget)),
                        ),
                    )
                    .then(literal("set").then(
                        argument("targets", holders()).suggests(suggest::SCORE_HOLDERS).then(
                            argument("objective", ArgumentType::Objective)
                                .then(argument("score", int(None)).executes(sb(S::Set))),
                        ),
                    ))
                    .then(literal("get").then(
                        argument("target", holder()).suggests(suggest::SCORE_HOLDERS).then(
                            argument("objective", ArgumentType::Objective).executes(sb(S::Get)),
                        ),
                    ))
                    .then(literal("add").then(
                        argument("targets", holders()).suggests(suggest::SCORE_HOLDERS).then(
                            argument("objective", ArgumentType::Objective)
                                .then(argument("score", int(Some(0))).executes(sb(S::Add))),
                        ),
                    ))
                    .then(literal("remove").then(
                        argument("targets", holders()).suggests(suggest::SCORE_HOLDERS).then(
                            argument("objective", ArgumentType::Objective)
                                .then(argument("score", int(Some(0))).executes(sb(S::Remove))),
                        ),
                    ))
                    .then(
                        literal("reset").then(
                            argument("targets", holders())
                                .suggests(suggest::SCORE_HOLDERS)
                                .executes(sb(S::ResetAll))
                                .then(
                                    argument("objective", ArgumentType::Objective)
                                        .executes(sb(S::Reset)),
                                ),
                        ),
                    )
                    .then(literal("enable").then(
                        argument("targets", holders()).suggests(suggest::SCORE_HOLDERS).then(
                            argument("objective", ArgumentType::Objective)
                                .suggests(suggest::TRIGGER_OBJECTIVES)
                                .executes(sb(S::Enable)),
                        ),
                    ))
                    .then(
                        literal("display")
                            .then(literal("name").then(
                                argument("targets", holders())
                                    .suggests(suggest::SCORE_HOLDERS)
                                    .then(
                                        argument("objective", ArgumentType::Objective)
                                            .executes(sb(S::DisplayNameClear))
                                            .then(
                                                argument("name", ArgumentType::Component)
                                                    .executes(sb(S::DisplayName)),
                                            ),
                                    ),
                            ))
                            .then(literal("numberformat").then(
                                argument("targets", holders())
                                    .suggests(suggest::SCORE_HOLDERS)
                                    .then(
                                        argument("objective", ArgumentType::Objective)
                                            .executes(sb(S::PlayerFormatClear))
                                            .then(pb)
                                            .then(pf)
                                            .then(ps),
                                    ),
                            )),
                    )
                    .then(
                        literal("operation").then(
                            argument("targets", holders())
                                .suggests(suggest::SCORE_HOLDERS)
                                .then(
                                    argument("targetObjective", ArgumentType::Objective).then(
                                        argument("operation", ArgumentType::Operation).then(
                                            argument("source", holders())
                                                .suggests(suggest::SCORE_HOLDERS)
                                                .then(
                                                    argument(
                                                        "sourceObjective",
                                                        ArgumentType::Objective,
                                                    )
                                                    .executes(sb(S::Operation)),
                                                ),
                                        ),
                                    ),
                                ),
                        ),
                    ),
            ),
    );
}

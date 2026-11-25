//! Syntax of the entity commands: bossbar, damage, effect, swing, ride, rotate, summon.

use vanillite_command::{ArgumentType, Builder, Dispatcher, argument, literal};

use super::{BossbarOp, CommandId, level, suggest};

fn entity() -> ArgumentType {
    ArgumentType::Entity {
        single: true,
        players_only: false,
    }
}

fn entities() -> ArgumentType {
    ArgumentType::Entity {
        single: false,
        players_only: false,
    }
}

fn players() -> ArgumentType {
    ArgumentType::Entity {
        single: false,
        players_only: true,
    }
}

fn vec3() -> ArgumentType {
    ArgumentType::Vec3 { center: true }
}

/// `BossEvent.BossBarColor` in id order.
const BOSSBAR_COLORS: [&str; 7] = ["pink", "blue", "red", "green", "yellow", "purple", "white"];

/// `BossEvent.BossBarOverlay` in id order.
const BOSSBAR_STYLES: [&str; 5] = [
    "progress",
    "notched_6",
    "notched_10",
    "notched_12",
    "notched_20",
];

/// `BossBarCommands`.
pub(super) fn register_bossbar(d: &mut Dispatcher<CommandId>) {
    let op = CommandId::Bossbar;
    let id = || argument("id", ArgumentType::Identifier);
    let mut color = literal("color");
    for (i, name) in (0u8..).zip(BOSSBAR_COLORS) {
        color = color.then(literal(name).executes(op(BossbarOp::SetColor(i))));
    }
    let mut style = literal("style");
    for (i, name) in (0u8..).zip(BOSSBAR_STYLES) {
        style = style.then(literal(name).executes(op(BossbarOp::SetStyle(i))));
    }
    d.register(
        literal("bossbar")
            .requires(level::GAMEMASTERS)
            .then(literal("add").then(
                id().then(argument("name", ArgumentType::Component).executes(op(BossbarOp::Add))),
            ))
            .then(
                literal("remove").then(
                    id().suggests(suggest::BOSSBARS)
                        .executes(op(BossbarOp::Remove)),
                ),
            )
            .then(literal("list").executes(op(BossbarOp::List)))
            .then(
                literal("set").then(
                    id().suggests(suggest::BOSSBARS)
                        .then(
                            literal("name").then(
                                argument("name", ArgumentType::Component)
                                    .executes(op(BossbarOp::SetName)),
                            ),
                        )
                        .then(color)
                        .then(style)
                        .then(
                            literal("value").then(
                                argument(
                                    "value",
                                    ArgumentType::Integer {
                                        min: Some(0),
                                        max: None,
                                    },
                                )
                                .executes(op(BossbarOp::SetValue)),
                            ),
                        )
                        .then(
                            literal("max").then(
                                argument(
                                    "max",
                                    ArgumentType::Integer {
                                        min: Some(1),
                                        max: None,
                                    },
                                )
                                .executes(op(BossbarOp::SetMax)),
                            ),
                        )
                        .then(
                            literal("visible").then(
                                argument("visible", ArgumentType::Bool)
                                    .executes(op(BossbarOp::SetVisible)),
                            ),
                        )
                        .then(literal("players").executes(op(BossbarOp::SetPlayers)).then(
                            argument("targets", players()).executes(op(BossbarOp::SetPlayers)),
                        )),
                ),
            )
            .then(
                literal("get").then(
                    id().suggests(suggest::BOSSBARS)
                        .then(literal("value").executes(op(BossbarOp::GetValue)))
                        .then(literal("max").executes(op(BossbarOp::GetMax)))
                        .then(literal("visible").executes(op(BossbarOp::GetVisible)))
                        .then(literal("players").executes(op(BossbarOp::GetPlayers))),
                ),
            ),
    );
}

/// `DamageCommand`.
pub(super) fn register_damage(d: &mut Dispatcher<CommandId>) {
    let id = CommandId::Damage;
    d.register(
        literal("damage").requires(level::GAMEMASTERS).then(
            argument("target", entity()).then(
                argument(
                    "amount",
                    ArgumentType::Float {
                        min: Some(0.0),
                        max: None,
                    },
                )
                .executes(id)
                .then(
                    argument(
                        "damageType",
                        ArgumentType::Resource {
                            registry: "minecraft:damage_type",
                        },
                    )
                    .executes(id)
                    .then(literal("at").then(argument("location", vec3()).executes(id)))
                    .then(
                        literal("by").then(
                            argument("entity", entity()).executes(id).then(
                                literal("from").then(argument("cause", entity()).executes(id)),
                            ),
                        ),
                    ),
                ),
            ),
        ),
    );
}

/// `<amplifier> [<hideParticles>]` of `effect give`.
fn amplifier(id: CommandId) -> Builder<CommandId> {
    argument(
        "amplifier",
        ArgumentType::Integer {
            min: Some(0),
            max: Some(255),
        },
    )
    .executes(id)
    .then(argument("hideParticles", ArgumentType::Bool).executes(id))
}

/// `EffectCommands`.
pub(super) fn register_effect(d: &mut Dispatcher<CommandId>) {
    let effect = || {
        argument(
            "effect",
            ArgumentType::Resource {
                registry: "minecraft:mob_effect",
            },
        )
    };
    let give = CommandId::EffectGive { infinite: false };
    let infinite = CommandId::EffectGive { infinite: true };
    d.register(
        literal("effect")
            .requires(level::GAMEMASTERS)
            .then(
                literal("clear").executes(CommandId::EffectClear).then(
                    argument("targets", entities())
                        .executes(CommandId::EffectClear)
                        .then(effect().executes(CommandId::EffectClear)),
                ),
            )
            .then(
                literal("give").then(
                    argument("targets", entities()).then(
                        effect()
                            .executes(give)
                            .then(
                                argument(
                                    "seconds",
                                    ArgumentType::Integer {
                                        min: Some(1),
                                        max: Some(1_000_000),
                                    },
                                )
                                .executes(give)
                                .then(amplifier(give)),
                            )
                            .then(
                                literal("infinite")
                                    .executes(infinite)
                                    .then(amplifier(infinite)),
                            ),
                    ),
                ),
            ),
    );
}

/// `SwingCommand.handSwing`.
fn hand_swing(name: &str, offhand: bool) -> Builder<CommandId> {
    let id = CommandId::Swing { offhand };
    literal(name).executes(id).then(
        argument("animation", ArgumentType::SwingAnimation)
            .executes(id)
            .then(argument("duration", ArgumentType::Time { min: 1 }).executes(id)),
    )
}

/// `SwingCommand`.
pub(super) fn register_swing(d: &mut Dispatcher<CommandId>) {
    let main = CommandId::Swing { offhand: false };
    d.register(
        literal("swing")
            .requires(level::GAMEMASTERS)
            .executes(main)
            .then(
                argument("targets", entities())
                    .executes(main)
                    .then(hand_swing("mainhand", false))
                    .then(hand_swing("offhand", true)),
            ),
    );
}

/// `RideCommand`.
pub(super) fn register_ride(d: &mut Dispatcher<CommandId>) {
    d.register(
        literal("ride").requires(level::GAMEMASTERS).then(
            argument("target", entity())
                .then(
                    literal("mount")
                        .then(argument("vehicle", entity()).executes(CommandId::RideMount)),
                )
                .then(literal("dismount").executes(CommandId::RideDismount)),
        ),
    );
}

/// `RotateCommand`.
pub(super) fn register_rotate(d: &mut Dispatcher<CommandId>) {
    let id = CommandId::Rotate;
    d.register(
        literal("rotate").requires(level::GAMEMASTERS).then(
            argument("target", entity())
                .then(argument("rotation", ArgumentType::Rotation).executes(id))
                .then(
                    literal("facing")
                        .then(literal("entity").then(
                            argument("facingEntity", entity()).executes(id).then(
                                argument("facingAnchor", ArgumentType::EntityAnchor).executes(id),
                            ),
                        ))
                        .then(argument("facingLocation", vec3()).executes(id)),
                ),
        ),
    );
}

/// `SummonCommand`.
pub(super) fn register_summon(d: &mut Dispatcher<CommandId>) {
    let id = CommandId::Summon;
    d.register(
        literal("summon").requires(level::GAMEMASTERS).then(
            argument(
                "entity",
                ArgumentType::Resource {
                    registry: "minecraft:entity_type",
                },
            )
            .suggests(suggest::SUMMONABLE_ENTITIES)
            .executes(id)
            .then(
                argument("pos", vec3())
                    .executes(id)
                    .then(argument("nbt", ArgumentType::NbtCompound).executes(id)),
            ),
        ),
    );
}

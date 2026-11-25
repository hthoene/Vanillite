//! Syntax of the client effect commands: particle, playsound, posteffect, stopsound.

use vanillite_command::{ArgumentType, Builder, Dispatcher, argument, literal};

use super::{CommandId, PostEffectOp, level, suggest};

fn players() -> ArgumentType {
    ArgumentType::Entity {
        single: false,
        players_only: true,
    }
}

/// `SoundSource.getName` in id order.
pub const SOUND_SOURCES: [&str; 11] = [
    "master", "music", "record", "weather", "block", "hostile", "neutral", "player", "ambient",
    "voice", "ui",
];

fn sound() -> Builder<CommandId> {
    argument("sound", ArgumentType::Identifier).suggests(suggest::AVAILABLE_SOUNDS)
}

fn float(min: f32, max: Option<f32>) -> ArgumentType {
    ArgumentType::Float {
        min: Some(min),
        max,
    }
}

/// `ParticleCommand`.
pub(super) fn register_particle(d: &mut Dispatcher<CommandId>) {
    let normal = CommandId::Particle { force: false };
    let force = CommandId::Particle { force: true };
    let mode = |name: &str, id: CommandId| {
        literal(name)
            .executes(id)
            .then(argument("viewers", players()).executes(id))
    };
    d.register(
        literal("particle").requires(level::GAMEMASTERS).then(
            argument("name", ArgumentType::Particle)
                .executes(normal)
                .then(
                    argument("pos", ArgumentType::Vec3 { center: true })
                        .executes(normal)
                        .then(
                            argument("delta", ArgumentType::Vec3 { center: false }).then(
                                argument("speed", float(0.0, None)).then(
                                    argument(
                                        "count",
                                        ArgumentType::Integer {
                                            min: Some(0),
                                            max: None,
                                        },
                                    )
                                    .executes(normal)
                                    .then(mode("force", force))
                                    .then(mode("normal", normal)),
                                ),
                            ),
                        ),
                ),
        ),
    );
}

/// `PlaySoundCommand.source`.
fn play_source(id: CommandId, name: &str) -> Builder<CommandId> {
    literal(name).executes(id).then(
        argument("targets", players()).executes(id).then(
            argument("pos", ArgumentType::Vec3 { center: true })
                .executes(id)
                .then(
                    argument("volume", float(0.0, None)).executes(id).then(
                        argument("pitch", float(0.0, Some(2.0)))
                            .executes(id)
                            .then(argument("minVolume", float(0.0, Some(1.0))).executes(id)),
                    ),
                ),
        ),
    )
}

/// `PlaySoundCommand`.
pub(super) fn register_playsound(d: &mut Dispatcher<CommandId>) {
    let mut name = sound().executes(CommandId::PlaySound(0));
    for (i, source) in (0u8..).zip(SOUND_SOURCES) {
        name = name.then(play_source(CommandId::PlaySound(i), source));
    }
    d.register(literal("playsound").requires(level::GAMEMASTERS).then(name));
}

/// `PostEffectCommand`.
pub(super) fn register_posteffect(d: &mut Dispatcher<CommandId>) {
    let op = CommandId::PostEffect;
    let posteffect = |id: CommandId| {
        argument("posteffect", ArgumentType::Identifier)
            .suggests(suggest::POST_EFFECTS)
            .executes(id)
    };
    d.register(
        literal("posteffect")
            .requires(level::GAMEMASTERS)
            .then(
                literal("add")
                    .then(argument("targets", players()).then(posteffect(op(PostEffectOp::Add)))),
            )
            .then(
                literal("clear")
                    .then(argument("targets", players()).executes(op(PostEffectOp::Clear))),
            )
            .then(
                literal("list").then(
                    argument(
                        "target",
                        ArgumentType::Entity {
                            single: true,
                            players_only: true,
                        },
                    )
                    .executes(op(PostEffectOp::List)),
                ),
            )
            .then(
                literal("remove").then(
                    argument("targets", players()).then(posteffect(op(PostEffectOp::Remove))),
                ),
            ),
    );
}

/// `StopSoundCommand`.
pub(super) fn register_stopsound(d: &mut Dispatcher<CommandId>) {
    let mut targets = argument("targets", players())
        .executes(CommandId::StopSound(None))
        .then(literal("*").then(sound().executes(CommandId::StopSound(None))));
    for (i, source) in (0u8..).zip(SOUND_SOURCES) {
        let id = CommandId::StopSound(Some(i));
        targets = targets.then(literal(source).executes(id).then(sound().executes(id)));
    }
    d.register(
        literal("stopsound")
            .requires(level::GAMEMASTERS)
            .then(targets),
    );
}

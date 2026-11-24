//! Syntax of the player commands: gamemode, help, list, msg, say, teleport.

use vanillite_command::{ArgumentType, Dispatcher, StringKind, argument, literal};

use super::{CommandId, level};

fn players() -> ArgumentType {
    ArgumentType::Entity {
        single: false,
        players_only: true,
    }
}

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

fn vec3() -> ArgumentType {
    ArgumentType::Vec3 { center: true }
}

/// `GameModeCommand`.
pub(super) fn register_game_mode(d: &mut Dispatcher<CommandId>) {
    d.register(
        literal("gamemode").requires(level::GAMEMASTERS).then(
            argument("gamemode", ArgumentType::GameMode)
                .executes(CommandId::GameModeSelf)
                .then(argument("target", players()).executes(CommandId::GameModeTargets)),
        ),
    );
}

/// `HelpCommand`.
pub(super) fn register_help(d: &mut Dispatcher<CommandId>) {
    d.register(
        literal("help").executes(CommandId::Help).then(
            argument("command", ArgumentType::String(StringKind::GreedyPhrase))
                .executes(CommandId::HelpCommand),
        ),
    );
}

/// `ListPlayersCommand`.
pub(super) fn register_list(d: &mut Dispatcher<CommandId>) {
    d.register(
        literal("list")
            .executes(CommandId::List)
            .then(literal("uuids").executes(CommandId::ListUuids)),
    );
}

/// `MsgCommand`.
pub(super) fn register_msg(d: &mut Dispatcher<CommandId>) {
    let msg = d.register(
        literal("msg").then(
            argument("targets", players())
                .then(argument("message", ArgumentType::Message).executes(CommandId::Msg)),
        ),
    );
    d.register(literal("tell").redirect(msg));
    d.register(literal("w").redirect(msg));
}

/// `SayCommand`.
pub(super) fn register_say(d: &mut Dispatcher<CommandId>) {
    d.register(
        literal("say")
            .requires(level::GAMEMASTERS)
            .then(argument("message", ArgumentType::Message).executes(CommandId::Say)),
    );
}

/// `TeleportCommand`.
pub(super) fn register_teleport(d: &mut Dispatcher<CommandId>) {
    let teleport = d.register(
        literal("teleport")
            .requires(level::GAMEMASTERS)
            .then(argument("location", vec3()).executes(CommandId::TeleportToLocation))
            .then(argument("destination", entity()).executes(CommandId::TeleportToEntity))
            .then(
                argument("targets", entities())
                    .then(
                        argument("location", vec3())
                            .executes(CommandId::TeleportTargetsToLocation)
                            .then(
                                argument("rotation", ArgumentType::Rotation)
                                    .executes(CommandId::TeleportTargetsToLocation),
                            )
                            .then(
                                literal("facing")
                                    .then(
                                        literal("entity").then(
                                            argument("facingEntity", entity())
                                                .executes(CommandId::TeleportTargetsToLocation)
                                                .then(
                                                    argument(
                                                        "facingAnchor",
                                                        ArgumentType::EntityAnchor,
                                                    )
                                                    .executes(CommandId::TeleportTargetsToLocation),
                                                ),
                                        ),
                                    )
                                    .then(
                                        argument("facingLocation", vec3())
                                            .executes(CommandId::TeleportTargetsToLocation),
                                    ),
                            ),
                    )
                    .then(
                        argument("destination", entity())
                            .executes(CommandId::TeleportTargetsToEntity),
                    ),
            ),
    );
    d.register(
        literal("tp")
            .requires(level::GAMEMASTERS)
            .redirect(teleport),
    );
}

//! Syntax of the server commands: kick, op, deop, stop, whitelist.

use vanillite_command::{ArgumentType, Dispatcher, argument, literal};

use super::{CommandId, level, suggest};

/// `KickCommand`.
pub(super) fn register_kick(d: &mut Dispatcher<CommandId>) {
    d.register(
        literal("kick").requires(level::ADMINS).then(
            argument(
                "targets",
                ArgumentType::Entity {
                    single: false,
                    players_only: true,
                },
            )
            .executes(CommandId::Kick)
            .then(argument("reason", ArgumentType::Message).executes(CommandId::Kick)),
        ),
    );
}

/// `DeOpCommands`.
pub(super) fn register_deop(d: &mut Dispatcher<CommandId>) {
    d.register(
        literal("deop").requires(level::ADMINS).then(
            argument("targets", ArgumentType::GameProfile)
                .suggests(suggest::OPS)
                .executes(CommandId::Deop),
        ),
    );
}

/// `OpCommand`.
pub(super) fn register_op(d: &mut Dispatcher<CommandId>) {
    d.register(
        literal("op").requires(level::ADMINS).then(
            argument("targets", ArgumentType::GameProfile)
                .suggests(suggest::OP_CANDIDATES)
                .executes(CommandId::Op),
        ),
    );
}

/// `StopCommand`.
pub(super) fn register_stop(d: &mut Dispatcher<CommandId>) {
    d.register(
        literal("stop")
            .requires(level::OWNERS)
            .executes(CommandId::Stop),
    );
}

/// `WhitelistCommand`.
pub(super) fn register_whitelist(d: &mut Dispatcher<CommandId>) {
    d.register(
        literal("whitelist")
            .requires(level::ADMINS)
            .then(literal("on").executes(CommandId::WhitelistOn))
            .then(literal("off").executes(CommandId::WhitelistOff))
            .then(literal("list").executes(CommandId::WhitelistList))
            .then(
                literal("add").then(
                    argument("targets", ArgumentType::GameProfile)
                        .suggests(suggest::WHITELIST_CANDIDATES)
                        .executes(CommandId::WhitelistAdd),
                ),
            )
            .then(
                literal("remove").then(
                    argument("targets", ArgumentType::GameProfile)
                        .suggests(suggest::WHITELISTED)
                        .executes(CommandId::WhitelistRemove),
                ),
            )
            .then(literal("reload").executes(CommandId::WhitelistReload)),
    );
}

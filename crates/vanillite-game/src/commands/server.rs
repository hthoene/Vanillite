//! Syntax of the server commands: datapack, kick, op, deop, reload, save-all, save-off,
//! save-on, stop, whitelist.

use vanillite_command::{ArgumentType, Dispatcher, StringKind, argument, literal};

use super::{CommandId, DataPackOp, PackPosition, level, suggest};

/// `DataPackCommand`.
pub(super) fn register_datapack(d: &mut Dispatcher<CommandId>) {
    let string = |name: &str| argument(name, ArgumentType::String(StringKind::QuotablePhrase));
    let op = CommandId::DataPack;
    let enable = |position| op(DataPackOp::Enable(position));
    let relative = |name: &str, position| {
        literal(name).then(
            string("existing")
                .suggests(suggest::SELECTED_PACKS)
                .executes(enable(position)),
        )
    };
    d.register(
        literal("datapack")
            .requires(level::GAMEMASTERS)
            .then(
                literal("enable").then(
                    string("name")
                        .suggests(suggest::UNSELECTED_PACKS)
                        .executes(enable(PackPosition::Default))
                        .then(relative("after", PackPosition::After))
                        .then(relative("before", PackPosition::Before))
                        .then(literal("last").executes(enable(PackPosition::Last)))
                        .then(literal("first").executes(enable(PackPosition::First))),
                ),
            )
            .then(
                literal("disable").then(
                    string("name")
                        .suggests(suggest::SELECTED_PACKS)
                        .executes(op(DataPackOp::Disable)),
                ),
            )
            .then(
                literal("list")
                    .executes(op(DataPackOp::List))
                    .then(literal("available").executes(op(DataPackOp::ListAvailable)))
                    .then(literal("enabled").executes(op(DataPackOp::ListEnabled))),
            )
            .then(
                literal("create").requires(level::OWNERS).then(
                    string("id").then(
                        argument("description", ArgumentType::Component)
                            .executes(op(DataPackOp::Create)),
                    ),
                ),
            ),
    );
}

/// `ReloadCommand`.
pub(super) fn register_reload(d: &mut Dispatcher<CommandId>) {
    d.register(
        literal("reload")
            .requires(level::GAMEMASTERS)
            .executes(CommandId::Reload),
    );
}

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

/// `SaveAllCommand`.
pub(super) fn register_save_all(d: &mut Dispatcher<CommandId>) {
    d.register(
        literal("save-all")
            .requires(level::OWNERS)
            .executes(CommandId::SaveAll)
            .then(literal("flush").executes(CommandId::SaveAllFlush)),
    );
}

/// `SaveOffCommand`.
pub(super) fn register_save_off(d: &mut Dispatcher<CommandId>) {
    d.register(
        literal("save-off")
            .requires(level::OWNERS)
            .executes(CommandId::SaveOff),
    );
}

/// `SaveOnCommand`.
pub(super) fn register_save_on(d: &mut Dispatcher<CommandId>) {
    d.register(
        literal("save-on")
            .requires(level::OWNERS)
            .executes(CommandId::SaveOn),
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

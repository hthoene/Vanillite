//! Syntax of the dedicated server's admin commands: ban, ban-ip, banlist, pardon, pardon-ip,
//! perf, setidletimeout, transfer.

use vanillite_command::{ArgumentType, Dispatcher, StringKind, argument, literal};

use super::{BanListKind, CommandId, level, suggest};

/// `BanIpCommands`.
pub(super) fn register_ban_ip(d: &mut Dispatcher<CommandId>) {
    d.register(
        literal("ban-ip").requires(level::ADMINS).then(
            argument("target", ArgumentType::String(StringKind::SingleWord))
                .executes(CommandId::BanIp)
                .then(argument("reason", ArgumentType::Message).executes(CommandId::BanIp)),
        ),
    );
}

/// `BanListCommands`.
pub(super) fn register_ban_list(d: &mut Dispatcher<CommandId>) {
    d.register(
        literal("banlist")
            .requires(level::ADMINS)
            .executes(CommandId::BanList(BanListKind::All))
            .then(literal("ips").executes(CommandId::BanList(BanListKind::Ips)))
            .then(literal("players").executes(CommandId::BanList(BanListKind::Players))),
    );
}

/// `BanPlayerCommands`.
pub(super) fn register_ban(d: &mut Dispatcher<CommandId>) {
    d.register(
        literal("ban").requires(level::ADMINS).then(
            argument("targets", ArgumentType::GameProfile)
                .executes(CommandId::Ban)
                .then(argument("reason", ArgumentType::Message).executes(CommandId::Ban)),
        ),
    );
}

/// `PardonCommand`.
pub(super) fn register_pardon(d: &mut Dispatcher<CommandId>) {
    d.register(
        literal("pardon").requires(level::ADMINS).then(
            argument("targets", ArgumentType::GameProfile)
                .suggests(suggest::BANNED_PLAYERS)
                .executes(CommandId::Pardon),
        ),
    );
}

/// `PardonIpCommand`.
pub(super) fn register_pardon_ip(d: &mut Dispatcher<CommandId>) {
    d.register(
        literal("pardon-ip").requires(level::ADMINS).then(
            argument("target", ArgumentType::String(StringKind::SingleWord))
                .suggests(suggest::BANNED_IPS)
                .executes(CommandId::PardonIp),
        ),
    );
}

/// `PerfCommand`.
pub(super) fn register_perf(d: &mut Dispatcher<CommandId>) {
    d.register(
        literal("perf")
            .requires(level::OWNERS)
            .then(literal("start").executes(CommandId::PerfStart))
            .then(literal("stop").executes(CommandId::PerfStop)),
    );
}

/// `SetPlayerIdleTimeoutCommand`.
pub(super) fn register_set_idle_timeout(d: &mut Dispatcher<CommandId>) {
    d.register(
        literal("setidletimeout").requires(level::ADMINS).then(
            argument(
                "minutes",
                ArgumentType::Integer {
                    min: Some(0),
                    max: None,
                },
            )
            .executes(CommandId::SetIdleTimeout),
        ),
    );
}

/// `TransferCommand`.
pub(super) fn register_transfer(d: &mut Dispatcher<CommandId>) {
    d.register(
        literal("transfer").requires(level::ADMINS).then(
            argument("hostname", ArgumentType::String(StringKind::QuotablePhrase))
                .executes(CommandId::Transfer)
                .then(
                    argument(
                        "port",
                        ArgumentType::Integer {
                            min: Some(1),
                            max: Some(65535),
                        },
                    )
                    .executes(CommandId::Transfer)
                    .then(
                        argument(
                            "players",
                            ArgumentType::Entity {
                                single: false,
                                players_only: true,
                            },
                        )
                        .executes(CommandId::Transfer),
                    ),
                ),
        ),
    );
}

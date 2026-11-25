//! The ban commands (Vanilla's `BanPlayerCommands`, `BanIpCommands`, `BanListCommands`,
//! `PardonCommand`, `PardonIpCommand`) and the ban check at login (`canPlayerLogin`).

use vanillite_command::error::CommandError;
use vanillite_command::{Arg as ErrArg, Parsed};
use vanillite_game::commands::{BanListKind, CommandId};
use vanillite_protocol::text::{Arg, Component};

use super::Env;
use super::source::CommandSource;
use crate::access::{Access, Ban, IpBan, UserBan};
use crate::player_list::Uuid;

fn missing(name: &str) -> CommandError {
    CommandError::new(
        "command.exception",
        vec![ErrArg::Str(format!("missing argument {name}"))],
    )
}

/// `BanListEntry.getReasonMessage`.
fn reason_message(ban: &Ban) -> Component {
    match &ban.reason {
        Some(reason) => Component::text(reason.as_str()),
        None => Component::translatable("multiplayer.disconnect.banned.reason.default", vec![]),
    }
}

/// `InetAddresses.isInetAddress`.
fn is_ip(text: &str) -> bool {
    text.parse::<std::net::IpAddr>().is_ok()
}

/// The disconnect reason of a banned player (`ip` = `None`) or address, if banned
/// (`PlayerList.canPlayerLogin`).
pub(crate) fn ban_reason(access: &Access, uuid: u128, ip: Option<&str>) -> Option<Component> {
    let (ban, key) = match ip {
        None => (access.ban_of(uuid)?.ban, "multiplayer.disconnect.banned"),
        Some(ip) => (
            access.ip_ban_of(ip)?.ban,
            "multiplayer.disconnect.banned_ip",
        ),
    };
    let mut reason = Component::translatable(format!("{key}.reason"), vec![reason_message(&ban)]);
    if let Some(expiry) = ban.expiry_text() {
        reason = reason.append(Component::translatable_args(
            format!("{key}.expiration"),
            vec![Arg::String(expiry)],
        ));
    }
    Some(reason)
}

/// The reason argument as plain text (`MessageArgument.getMessage(...).getString()`).
fn reason(
    env: &Env<'_>,
    source: &CommandSource,
    parsed: &Parsed<CommandId>,
) -> Result<Option<String>, CommandError> {
    parsed
        .args
        .message("reason")
        .map(|m| super::dispatch::resolve_message(env, source, m).map(|c| c.plain()))
        .transpose()
}

/// Runs a ban command.
pub(crate) fn run(
    parsed: &Parsed<CommandId>,
    source: &CommandSource,
    env: &mut Env<'_>,
) -> Result<i32, CommandError> {
    let global = env.global;
    let access = &global.access;
    match parsed.command {
        CommandId::Ban => {
            let profiles = super::dispatch::profiles(env, source, parsed)?;
            let reason = reason(env, source, parsed)?;
            let mut count = 0i32;
            for p in &profiles {
                if access.is_banned(p.uuid) {
                    continue;
                }
                let ban = Ban::now(source.text_name(), reason.clone());
                let message = reason_message(&ban);
                access.ban(UserBan {
                    uuid: Uuid(p.uuid),
                    name: p.name.clone(),
                    ban,
                });
                count = count.saturating_add(1);
                source.send_success(
                    global,
                    Component::translatable(
                        "commands.ban.success",
                        vec![Component::text(p.name.as_str()), message],
                    ),
                    true,
                );
                if let Some((sink, _)) = global.players.connection(Uuid(p.uuid)) {
                    sink.kick(Component::translatable(
                        "multiplayer.disconnect.banned",
                        vec![],
                    ));
                }
            }
            if count == 0 {
                return Err(CommandError::new("commands.ban.failed", vec![]));
            }
            Ok(count)
        }
        CommandId::BanIp => {
            let target = parsed
                .args
                .string("target")
                .ok_or_else(|| missing("target"))?;
            let ip = if is_ip(target) {
                target.clone()
            } else {
                global
                    .players
                    .snapshot()
                    .into_iter()
                    .find(|p| p.name.eq_ignore_ascii_case(target))
                    .map(|p| p.ip)
                    .ok_or_else(|| CommandError::new("commands.banip.invalid", vec![]))?
            };
            if access.is_ip_banned(&ip) {
                return Err(CommandError::new("commands.banip.failed", vec![]));
            }
            let players: Vec<_> = global
                .players
                .snapshot()
                .into_iter()
                .filter(|p| p.ip == ip)
                .collect();
            let ban = Ban::now(source.text_name(), reason(env, source, parsed)?);
            let message = reason_message(&ban);
            access.ban_ip(IpBan {
                ip: ip.clone(),
                ban,
            });
            source.send_success(
                global,
                Component::translatable_args(
                    "commands.banip.success",
                    vec![Arg::String(ip), Arg::from(message)],
                ),
                true,
            );
            let n = i32::try_from(players.len()).unwrap_or(i32::MAX);
            if !players.is_empty() {
                let names = players
                    .iter()
                    .map(|p| vanillite_game::player::join::display_name(&p.name, p.uuid.0))
                    .collect();
                source.send_success(
                    global,
                    Component::translatable_args(
                        "commands.banip.info",
                        vec![Arg::Int(n), Arg::from(super::exec::format_list(names))],
                    ),
                    true,
                );
            }
            for p in &players {
                p.sink.kick(Component::translatable(
                    "multiplayer.disconnect.ip_banned",
                    vec![],
                ));
            }
            Ok(n)
        }
        CommandId::BanList(kind) => {
            // `getEntries` of the `HashMap`s, keyed by UUID and by address.
            let users = || {
                super::teams::java_hash_order(
                    access
                        .bans()
                        .into_iter()
                        .map(|b| (b.uuid.to_string(), b.name, b.ban))
                        .collect(),
                    |(key, _, _)| key.as_str(),
                )
                .into_iter()
                .map(|(_, name, ban)| (name, ban))
                .collect::<Vec<_>>()
            };
            let ips = || {
                super::teams::java_hash_order(
                    access
                        .ip_bans()
                        .into_iter()
                        .map(|b| (b.ip, b.ban))
                        .collect(),
                    |(ip, _)| ip.as_str(),
                )
            };
            let list = match kind {
                BanListKind::All => {
                    let mut all = users();
                    all.extend(ips());
                    all
                }
                BanListKind::Ips => ips(),
                BanListKind::Players => users(),
            };
            let n = i32::try_from(list.len()).unwrap_or(i32::MAX);
            if list.is_empty() {
                source.send_success(
                    global,
                    Component::translatable("commands.banlist.none", vec![]),
                    false,
                );
            } else {
                source.send_success(
                    global,
                    Component::translatable_args("commands.banlist.list", vec![Arg::Int(n)]),
                    false,
                );
                for (name, ban) in &list {
                    source.send_success(
                        global,
                        Component::translatable_args(
                            "commands.banlist.entry",
                            vec![
                                Arg::from(Component::text(name.as_str())),
                                Arg::String(ban.source.clone()),
                                Arg::from(reason_message(ban)),
                            ],
                        ),
                        false,
                    );
                }
            }
            Ok(n)
        }
        CommandId::Pardon => {
            let profiles = super::dispatch::profiles(env, source, parsed)?;
            let mut count = 0i32;
            for p in &profiles {
                if access.pardon(p.uuid) {
                    count = count.saturating_add(1);
                    source.send_success(
                        global,
                        Component::translatable(
                            "commands.pardon.success",
                            vec![Component::text(p.name.as_str())],
                        ),
                        true,
                    );
                }
            }
            if count == 0 {
                return Err(CommandError::new("commands.pardon.failed", vec![]));
            }
            Ok(count)
        }
        CommandId::PardonIp => {
            let ip = parsed
                .args
                .string("target")
                .ok_or_else(|| missing("target"))?;
            if !is_ip(ip) {
                return Err(CommandError::new("commands.pardonip.invalid", vec![]));
            }
            if !access.pardon_ip(ip) {
                return Err(CommandError::new("commands.pardonip.failed", vec![]));
            }
            source.send_success(
                global,
                Component::translatable_args(
                    "commands.pardonip.success",
                    vec![Arg::String(ip.clone())],
                ),
                true,
            );
            Ok(1)
        }
        _ => Err(CommandError::new("command.unknown.command", vec![])),
    }
}

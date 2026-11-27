//! The proxy's commands that players (with permission) and the console share: the player list,
//! moving players, broadcasts and the configuration reload. `/server` itself is the session's.

use std::sync::Arc;

use vanillite_protocol::text::{Color, Component, NamedColor};

use crate::{Control, Shared};

/// Who runs a command.
#[derive(Debug)]
pub(crate) enum Sender<'a> {
    Console,
    Player {
        name: &'a str,
        uuid: &'a str,
        /// The server the player is on.
        server: Option<&'a str>,
    },
}

/// Runs `line` (without `/`) if it is a shared command the sender may use; `None` passes it on
/// to the backend.
pub(crate) fn run(shared: &Shared, sender: &Sender<'_>, line: &str) -> Option<Component> {
    let config = shared.config();
    let mut words = line.split_whitespace();
    let command = words.next()?;
    let allowed = match sender {
        Sender::Console => true,
        Sender::Player { name, uuid, .. } => config.permitted(command, name, uuid),
    };
    if !allowed {
        return None;
    }
    let args: Vec<&str> = words.collect();
    let red = |s: String| Component::text(s).color(Color::Named(NamedColor::Red));
    let yellow = |s: String| Component::text(s).color(Color::Named(NamedColor::Yellow));
    Some(match command {
        "glist" => yellow(player_list(shared, args.first().copied())),
        "send" => match args.as_slice() {
            [who, target] => send(shared, sender, who, target),
            _ => red("Usage: /send <player|all|current> <server>".to_owned()),
        },
        "alert" if args.is_empty() => red("Usage: /alert <message>".to_owned()),
        "alert" => {
            let text = legacy_colors(&format!("&8[&4Alert&8]&r {}", args.join(" ")));
            let n = shared.control_all(|_| true, || Control::Chat(Component::text(text.clone())));
            log::info!("Alert: {}", args.join(" "));
            yellow(format!("Sent the alert to {n} players."))
        }
        "proxy" => match args.first().copied() {
            Some("reload") => match shared.reload() {
                Ok(()) => yellow("Reloaded the configuration.".to_owned()),
                Err(e) => red(format!("The configuration was not reloaded: {e}")),
            },
            Some("info") => yellow(format!(
                "Vanillite proxy {} for Minecraft {}: {} players, servers: {}.",
                env!("CARGO_PKG_VERSION"),
                vanillite_protocol::VERSION_ID,
                shared.players.lock().map_or(0, |p| p.len()),
                config
                    .servers
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
            _ => red("Usage: /proxy <reload|info>".to_owned()),
        },
        _ => return None,
    })
}

/// A console command line; the answer as plain text.
pub(crate) fn console(shared: &Shared, line: &str) -> String {
    let line = line.trim().trim_start_matches('/');
    let mut words = line.split_whitespace();
    match words.next() {
        None => String::new(),
        Some("help") => "Commands: glist [server], send <player|all> <server>, alert <message>, \
                         kick <player> [reason], proxy <reload|info>, end"
            .to_owned(),
        Some("kick") => {
            let Some(name) = words.next() else {
                return "Usage: kick <player> [reason]".to_owned();
            };
            let reason: Vec<&str> = words.collect();
            let reason = if reason.is_empty() {
                "Kicked by an operator.".to_owned()
            } else {
                reason.join(" ")
            };
            let order = Control::Kick(Component::text(legacy_colors(&reason)));
            if shared.control_by_name(name, order) {
                format!("Kicked {name}.")
            } else {
                format!("There is no player named {name}.")
            }
        }
        Some(_) => match run(shared, &Sender::Console, line) {
            Some(out) => strip_codes(&out.plain()),
            None => "Unknown command. Type help for the list.".to_owned(),
        },
    }
}

/// `glist`: the players of every server, or of one, and the total.
pub(crate) fn player_list(shared: &Shared, only: Option<&str>) -> String {
    let config = shared.config();
    let Ok(players) = shared.players.lock() else {
        return String::new();
    };
    let mut out = String::new();
    for server in config.servers.keys() {
        if only.is_some_and(|o| !o.eq_ignore_ascii_case(server)) {
            continue;
        }
        let mut names: Vec<&str> = players
            .values()
            .filter(|p| p.server.as_deref() == Some(server.as_str()))
            .map(|p| p.name.as_str())
            .collect();
        names.sort_unstable();
        out.push_str(&format!(
            "[{server}] ({}): {}\n",
            names.len(),
            names.join(", ")
        ));
    }
    if out.is_empty()
        && let Some(server) = only
    {
        return format!("There is no server named {server}.");
    }
    out.push_str(&format!("{} players are online.", players.len()));
    out
}

/// `send`: moves one player, everybody, or everybody on the sender's server.
fn send(shared: &Shared, sender: &Sender<'_>, who: &str, target: &str) -> Component {
    let config = shared.config();
    let red = |s: String| Component::text(s).color(Color::Named(NamedColor::Red));
    let Some((target, _)) = config.servers.get_key_value(target) else {
        return red(format!("There is no server named {target}."));
    };
    let to: Arc<str> = Arc::from(target.as_str());
    let order = || Control::Send(Arc::clone(&to));
    let n = match who {
        "all" => shared.control_all(|_| true, order),
        "current" => {
            let Sender::Player {
                server: Some(here), ..
            } = sender
            else {
                return red("Only a player on a server can send everybody on it.".to_owned());
            };
            shared.control_all(|p| p.server.as_deref() == Some(*here), order)
        }
        name => {
            if !shared.control_by_name(name, order()) {
                return red(format!("There is no player named {name}."));
            }
            1
        }
    };
    Component::text(format!("Sending {n} players to {target}."))
        .color(Color::Named(NamedColor::Yellow))
}

/// `&` colour and format codes as the `§` codes clients render in plain text.
pub(crate) fn legacy_colors(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '&'
            && let Some(&code) = chars.peek()
            && "0123456789abcdefklmnorABCDEFKLMNOR".contains(code)
        {
            out.push('§');
            out.push(code.to_ascii_lowercase());
            chars.next();
            continue;
        }
        out.push(c);
    }
    out
}

/// Text without `§` codes, for the console.
fn strip_codes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '§' {
            chars.next();
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn colour_codes() {
        assert_eq!(super::legacy_colors("&4Red &zx &"), "§4Red &zx &");
        assert_eq!(super::strip_codes("§4Red§r!"), "Red!");
    }
}

//! Parsing of our commands: Vanilla's results and errors (from captures of a 26.3 server),
//! `/help` output and abuse cases (SEC-04, SEC-11).

use vanillite_command::{CommandError, Dispatcher, Parsed, Source};
use vanillite_game::commands::{self, CommandId};

struct Level(u8);

impl Source for Level {
    fn permission_level(&self) -> u8 {
        self.0
    }

    fn online_player_names(&self) -> Vec<String> {
        vec!["Alice".into(), "Bob".into()]
    }

    fn resource_exists(&self, registry: &str, id: &str) -> bool {
        registry != "minecraft:timeline" || id == "minecraft:day"
    }
}

fn parse(
    d: &Dispatcher<CommandId>,
    input: &str,
    level: u8,
) -> Result<Parsed<CommandId>, CommandError> {
    d.parse_command(input, &Level(level))
}

fn command(input: &str) -> Option<CommandId> {
    parse(&commands::dispatcher(), input, 4)
        .ok()
        .map(|p| p.command)
}

fn error(input: &str, level: u8) -> Option<(&'static str, Option<usize>)> {
    parse(&commands::dispatcher(), input, level)
        .err()
        .map(|e| (e.key, e.cursor))
}

#[test]
fn commands_resolve_like_vanilla() {
    assert_eq!(command("gamemode creative"), Some(CommandId::GameModeSelf));
    assert_eq!(
        command("gamemode survival Bob"),
        Some(CommandId::GameModeTargets)
    );
    assert_eq!(
        command("tp Bob Alice"),
        Some(CommandId::TeleportTargetsToEntity)
    );
    assert_eq!(command("tp 1 2 3"), Some(CommandId::TeleportToLocation));
    assert_eq!(command("tp nobody"), Some(CommandId::TeleportToEntity));
    assert_eq!(
        command("teleport @s ~ ~1 ~ 90 0"),
        Some(CommandId::TeleportTargetsToLocation)
    );
    assert_eq!(
        command("tp @p ~ ~ ~ facing 0 0 0"),
        Some(CommandId::TeleportTargetsToLocation)
    );
    assert_eq!(
        command("tp @e[limit=1,sort=nearest] 0 5 0"),
        Some(CommandId::TeleportTargetsToLocation)
    );
    assert_eq!(
        command("time query gametime"),
        Some(CommandId::TimeQueryGameTime)
    );
    assert_eq!(
        command("time query day"),
        Some(CommandId::TimeQueryTimeline)
    );
    assert_eq!(command("time set 1d"), Some(CommandId::TimeSet));
    assert_eq!(command("whitelist add Bob"), Some(CommandId::WhitelistAdd));
    assert_eq!(command("kick Bob Bye bye"), Some(CommandId::Kick));
    assert_eq!(command("list uuids"), Some(CommandId::ListUuids));
    assert_eq!(command("help gamemode"), Some(CommandId::HelpCommand));
    assert_eq!(command("stop"), Some(CommandId::Stop));
}

#[test]
fn redirects_carry_arguments_and_signable_messages() {
    let d = commands::dispatcher();
    let p = parse(&d, "tell Bob psst now", 0).expect("tell");
    assert_eq!(p.command, CommandId::Msg);
    assert!(p.args.selector("targets").is_some());
    assert_eq!(
        p.args.message("message").map(|m| m.text.as_str()),
        Some("psst now")
    );
    assert_eq!(p.signable, [("message".to_owned(), "psst now".to_owned())]);
    let p = parse(&d, "time set 1d", 4).expect("time");
    assert_eq!(p.args.time("time"), Some(&24000));
    assert!(p.signable.is_empty());
}

#[test]
fn errors_like_vanilla() {
    assert_eq!(error("foo", 4), Some(("command.unknown.command", Some(0))));
    assert_eq!(
        error("gamemode", 4),
        Some(("command.unknown.command", Some(8)))
    );
    assert_eq!(
        error("time query", 4),
        Some(("command.unknown.command", Some(10)))
    );
    assert_eq!(error("say", 4), Some(("command.unknown.command", Some(3))));
    assert_eq!(
        error("say no", 0),
        Some(("command.unknown.command", Some(0)))
    );
    assert_eq!(
        error("gamemode creative", 0),
        Some(("command.unknown.command", Some(0)))
    );
    assert_eq!(
        error("time query daytime", 4),
        Some(("argument.resource.not_found", Some(18)))
    );
    assert_eq!(
        error("gamemode nope", 4),
        Some(("argument.gamemode.invalid", Some(13)))
    );
    assert_eq!(
        error("msg @a hi", 0),
        Some(("argument.entity.selector.not_allowed", Some(4)))
    );
    assert_eq!(
        error("tp @e[limit=0] 1 2 3", 4),
        Some(("command.unknown.argument", Some(3)))
    );
    assert_eq!(
        error("gamemode creative @e[limit=0]", 4),
        Some(("argument.entity.options.limit.toosmall", Some(27)))
    );
    assert_eq!(
        error("gamemode creative @e", 4),
        Some(("argument.player.entities", Some(0)))
    );
}

#[test]
fn help_lines_like_vanilla() {
    let d = commands::dispatcher();
    let op = d.help_lines(None, &Level(4)).expect("help");
    assert_eq!(
        op,
        [
            "/gamemode <gamemode> [<target>]",
            "/help [<command>]",
            "/kick <targets> [<reason>]",
            "/list [uuids]",
            "/msg <targets> <message>",
            "/tell -> msg",
            "/w -> msg",
            "/say <message>",
            "/seed",
            "/teleport (<location>|<destination>|<targets>)",
            "/tp -> teleport",
            "/time (set|add|pause|resume|rate|query|of)",
            "/deop <targets>",
            "/op <targets>",
            "/stop",
            "/whitelist (on|off|list|add|remove|reload)",
        ]
    );
    let player = d.help_lines(None, &Level(0)).expect("help");
    assert_eq!(
        player,
        [
            "/help [<command>]",
            "/list [uuids]",
            "/msg <targets> <message>",
            "/tell -> msg",
            "/w -> msg"
        ]
    );
    let gamemode = d
        .help_lines(Some("gamemode"), &Level(4))
        .expect("help gamemode");
    assert_eq!(gamemode, ["/gamemode <gamemode> [<target>]"]);
    assert!(
        d.help_lines(Some("tp"), &Level(4))
            .expect("help tp")
            .is_empty()
    );
    assert!(d.help_lines(Some("nothing"), &Level(4)).is_err());
}

#[test]
fn hostile_input_fails_cleanly() {
    let d = commands::dispatcher();
    let deep_nbt = format!("tp @e[nbt={}] 0 0 0", "{a:".repeat(50_000));
    assert!(parse(&d, &deep_nbt, 4).is_err());
    let nested = format!("tp @e[{}", "[".repeat(50_000));
    assert!(parse(&d, &nested, 4).is_err());
    let long = format!("msg Bob {}", "x".repeat(40_000));
    assert!(parse(&d, &long, 4).is_err());
    let long_message = format!("say {}", "x".repeat(300));
    assert_eq!(
        parse(&d, &long_message, 4).err().map(|e| e.key),
        Some("argument.message.too_long")
    );
    assert!(parse(&d, "tp 99999999999999999999 0 0", 4).is_ok());
    assert!(parse(&d, "time add 99999999999999999999999d", 4).is_ok());
    assert!(parse(&d, "kick \"unterminated", 4).is_err());
    assert!(parse(&d, "msg Bob \u{1F600}\u{0}\u{7}", 0).is_ok());
    assert!(
        d.suggest(&format!("/tp @e[nbt={}", "{a:".repeat(50_000)), &Level(4))
            .list
            .is_empty()
    );
    // Suggestions for operator commands are not even parsed for players (SEC-11).
    let selector_spam = format!("/tp @e[{}", "tag=a,".repeat(10_000));
    assert!(d.suggest(&selector_spam, &Level(0)).list.is_empty());
}

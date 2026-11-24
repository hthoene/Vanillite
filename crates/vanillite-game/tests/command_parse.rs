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
    assert_eq!(command("save-all"), Some(CommandId::SaveAll));
    assert_eq!(command("save-all flush"), Some(CommandId::SaveAllFlush));
    assert_eq!(command("save-off"), Some(CommandId::SaveOff));
    assert_eq!(command("save-on"), Some(CommandId::SaveOn));
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
            "/execute (run|if|unless|as|at|store|positioned|rotated|facing|align|anchored|in|summon|on)",
            "/clone (<begin>|from)",
            "/defaultgamemode <gamemode>",
            "/difficulty [peaceful|easy|normal|hard]",
            "/fill <from> <to> <block> [outline|hollow|destroy|strict|replace|keep]",
            "/fillbiome <from> <to> <biome> [replace]",
            "/forceload (add|remove|query)",
            "/function <name> [<arguments>|with]",
            "/gamemode <gamemode> [<target>]",
            concat!(
                "/gamerule (command_block_output|minecraft:command_block_output|",
                "mob_explosion_drop_decay|minecraft:mob_explosion_drop_decay|",
                "players_nether_portal_creative_delay|",
                "minecraft:players_nether_portal_creative_delay|immediate_respawn|",
                "minecraft:immediate_respawn|show_advancement_messages|",
                "minecraft:show_advancement_messages|fire_spread_radius_around_player|",
                "minecraft:fire_spread_radius_around_player|max_snow_accumulation_height|",
                "minecraft:max_snow_accumulation_height|fall_damage|minecraft:fall_damage|",
                "mob_griefing|minecraft:mob_griefing|respawn_radius|minecraft:respawn_radius|",
                "spawn_monsters|minecraft:spawn_monsters|elytra_movement_check|",
                "minecraft:elytra_movement_check|advance_time|minecraft:advance_time|",
                "tnt_explosion_drop_decay|minecraft:tnt_explosion_drop_decay|drowning_damage|",
                "minecraft:drowning_damage|show_death_messages|minecraft:show_death_messages|",
                "freeze_damage|minecraft:freeze_damage|forgive_dead_players|",
                "minecraft:forgive_dead_players|max_block_modifications|",
                "minecraft:max_block_modifications|log_admin_commands|",
                "minecraft:log_admin_commands|advance_weather|minecraft:advance_weather|",
                "water_source_conversion|minecraft:water_source_conversion|mob_drops|",
                "minecraft:mob_drops|spawner_blocks_work|minecraft:spawner_blocks_work|",
                "random_tick_speed|minecraft:random_tick_speed|",
                "allow_entering_nether_using_portals|",
                "minecraft:allow_entering_nether_using_portals|players_sleeping_percentage|",
                "minecraft:players_sleeping_percentage|global_sound_events|",
                "minecraft:global_sound_events|keep_inventory|minecraft:keep_inventory|",
                "natural_health_regeneration|minecraft:natural_health_regeneration|",
                "max_command_sequence_length|minecraft:max_command_sequence_length|",
                "spectators_generate_chunks|minecraft:spectators_generate_chunks|",
                "player_movement_check|minecraft:player_movement_check|raids|minecraft:raids|",
                "reduced_debug_info|minecraft:reduced_debug_info|spawn_patrols|",
                "minecraft:spawn_patrols|block_explosion_drop_decay|",
                "minecraft:block_explosion_drop_decay|spread_vines|minecraft:spread_vines|",
                "block_drops|minecraft:block_drops|fire_damage|minecraft:fire_damage|",
                "projectiles_can_break_blocks|minecraft:projectiles_can_break_blocks|",
                "spawn_wardens|minecraft:spawn_wardens|pvp|minecraft:pvp|send_command_feedback|",
                "minecraft:send_command_feedback|spawn_phantoms|minecraft:spawn_phantoms|",
                "locator_bar|minecraft:locator_bar|entity_drops|minecraft:entity_drops|",
                "spawn_mobs|minecraft:spawn_mobs|command_blocks_work|",
                "minecraft:command_blocks_work|tnt_explodes|minecraft:tnt_explodes|",
                "max_entity_cramming|minecraft:max_entity_cramming|universal_anger|",
                "minecraft:universal_anger|spawn_wandering_traders|",
                "minecraft:spawn_wandering_traders|ender_pearls_vanish_on_death|",
                "minecraft:ender_pearls_vanish_on_death|limited_crafting|",
                "minecraft:limited_crafting|players_nether_portal_default_delay|",
                "minecraft:players_nether_portal_default_delay|lava_source_conversion|",
                "minecraft:lava_source_conversion|max_command_forks|minecraft:max_command_forks)",
            ),
            "/help [<command>]",
            "/kick <targets> [<reason>]",
            "/kill [<targets>]",
            "/list [uuids]",
            "/locate (structure|biome|poi)",
            "/msg <targets> <message>",
            "/tell -> msg",
            "/w -> msg",
            "/place (feature|jigsaw|structure|template)",
            "/return (<value>|fail|run)",
            "/say <message>",
            "/scoreboard (objectives|players)",
            "/seed",
            "/setblock <pos> <block> [destroy|keep|replace|strict]",
            "/spawnpoint [<targets>]",
            "/setworldspawn [<pos>]",
            "/spreadplayers <center> <spreadDistance> <maxRange> (<respectTeams>|under)",
            "/teleport (<location>|<destination>|<targets>)",
            "/tp -> teleport",
            "/tick (query|rate|step|sprint|unfreeze|freeze)",
            "/time (set|add|pause|resume|rate|query|of)",
            "/weather (clear|rain|thunder)",
            "/worldborder (add|set|center|damage|get|warning)",
            "/deop <targets>",
            "/op <targets>",
            "/save-all [flush]",
            "/save-off",
            "/save-on",
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

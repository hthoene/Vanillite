//! Runs a parsed command: reads its typed arguments, resolves selectors and messages, and calls
//! the command's implementation in [`super::exec`].

use vanillite_command::error::CommandError;
use vanillite_command::selector::EntitySelector;
use vanillite_command::values::Anchor;
use vanillite_command::{GameProfileArg, Message, Parsed, Source as _};
use vanillite_game::commands::{CommandId, ExecuteOp};
use vanillite_game::player::chat::ChatMessage;
use vanillite_game::player::join::display_name;
use vanillite_protocol::packets::play::clientbound::{LookAtEntity, PlayerLookAt};
use vanillite_protocol::text::Component;

use super::exec::{self, Destination, Profile, config_game_mode, format_list};
use super::select::{self, Target};
use super::source::CommandSource;
use super::survival;
use super::{Env, SignedArguments};
use vanillite_game::survival::rules::Rule;

fn missing(name: &str) -> CommandError {
    CommandError::new(
        "command.exception",
        vec![vanillite_command::Arg::Str(format!(
            "missing argument {name}"
        ))],
    )
}

/// `EntityArgument.getEntities`/`getPlayers` and their single forms.
fn targets(
    env: &Env<'_>,
    source: &CommandSource,
    selector: &EntitySelector,
    _players_only: bool,
) -> Result<Vec<Target>, CommandError> {
    select::many(env, source, selector)
}

fn selector_arg<'a>(
    parsed: &'a Parsed<CommandId>,
    name: &str,
) -> Result<&'a EntitySelector, CommandError> {
    parsed.args.selector(name).ok_or_else(|| missing(name))
}

/// `GameProfileArgument.getGameProfiles`: selected players, or a name of an online player,
/// an operator or a whitelisted player. In offline mode an unknown name gets its offline UUID.
fn profiles(
    env: &Env<'_>,
    source: &CommandSource,
    parsed: &Parsed<CommandId>,
) -> Result<Vec<Profile>, CommandError> {
    let arg = parsed
        .args
        .game_profile("targets")
        .ok_or_else(|| missing("targets"))?;
    match arg {
        GameProfileArg::Selector(selector) => Ok(targets(env, source, selector, true)?
            .into_iter()
            .map(|t| Profile {
                uuid: t.uuid,
                name: t.name,
            })
            .collect()),
        GameProfileArg::Name(name) => {
            let global = env.global;
            let online = global
                .players
                .snapshot()
                .into_iter()
                .find(|p| p.name.eq_ignore_ascii_case(name))
                .map(|p| Profile {
                    uuid: p.uuid.0,
                    name: p.name,
                });
            let known = || {
                global
                    .access
                    .ops()
                    .into_iter()
                    .find(|o| o.name.eq_ignore_ascii_case(name))
                    .map(|o| Profile {
                        uuid: o.uuid.0,
                        name: o.name,
                    })
            };
            let offline = || {
                (!global.online_mode).then(|| Profile {
                    uuid: vanillite_protocol::types::Uuid::offline(name).0,
                    name: name.clone(),
                })
            };
            online
                .or_else(known)
                .or_else(offline)
                .map(|p| vec![p])
                .ok_or_else(|| CommandError::new("argument.player.unknown", vec![]))
        }
    }
}

/// `MessageArgument.Message.resolveComponent`: the text with its selectors replaced by the
/// names they select.
fn resolve_message(
    env: &Env<'_>,
    source: &CommandSource,
    message: &Message,
) -> Result<Component, CommandError> {
    if message.parts.is_empty() || !source.allows_selectors() {
        return Ok(Component::text(message.text.as_str()));
    }
    let text = message.text.as_str();
    let first = message.parts.first().map_or(0, |p| p.start);
    let mut out = Component::text(text.get(..first).unwrap_or(""));
    let mut previous = first;
    for part in &message.parts {
        let names = select::select(&part.selector, source, &env.level.entities, env.global)
            .into_iter()
            .map(|t| display_name(&t.name, t.uuid))
            .collect();
        if previous < part.start {
            out = out.append(Component::text(
                text.get(previous..part.start).unwrap_or(""),
            ));
        }
        out = out.append(format_list(names));
        previous = part.end;
    }
    if previous < text.len() {
        out = out.append(Component::text(text.get(previous..).unwrap_or("")));
    }
    Ok(out)
}

/// `MessageArgument.resolveChatMessage`: the signed message with the resolved text as unsigned
/// content, or a system message when the argument is not signed.
fn chat_message(
    env: &Env<'_>,
    source: &CommandSource,
    parsed: &Parsed<CommandId>,
    signed: &SignedArguments,
) -> Result<ChatMessage, CommandError> {
    let message = parsed
        .args
        .message("message")
        .ok_or_else(|| missing("message"))?;
    let formatted = resolve_message(env, source, message)?;
    let mut out = match signed.get("message") {
        Some(m) => m.clone(),
        None => ChatMessage::system(message.text.clone()),
    };
    // `withUnsignedContent`: kept only when it differs from the plain signed text.
    if formatted != Component::text(out.body.content.as_str()) {
        out.unsigned_content = Some(formatted);
    }
    Ok(out)
}

fn require_player(source: &CommandSource) -> Result<(), CommandError> {
    match source.uuid() {
        Some(_) => Ok(()),
        None => Err(CommandError::new("permissions.requires.player", vec![])),
    }
}

/// The source itself as a target.
fn self_target(env: &Env<'_>, source: &CommandSource) -> Result<Target, CommandError> {
    let own = source
        .entity
        .as_ref()
        .ok_or_else(|| CommandError::new("permissions.requires.entity", vec![]))?;
    let found = match own.id.filter(|_| own.world == env.world) {
        Some(id) => select::local(&env.level.entities, env.world, id),
        None => select::online_players(&env.level.entities, env.global, source.world)
            .into_iter()
            .find(|t| t.uuid == own.uuid),
    };
    found.ok_or_else(|| CommandError::new("permissions.requires.entity", vec![]))
}

/// `LookAt.perform` for players: `player_look_at` from the eyes.
fn look_at(
    env: &Env<'_>,
    source: &CommandSource,
    targets: &[Target],
    parsed: &Parsed<CommandId>,
) -> Result<(), CommandError> {
    let packet = if let Some(location) = parsed.args.coordinates("facingLocation") {
        let p = exec::resolve_position(source, location);
        Some((p, None))
    } else if parsed.args.has("facingEntity") {
        let to = targets_single(env, source, selector_arg(parsed, "facingEntity")?)?;
        let anchor = match parsed.args.anchor("facingAnchor") {
            Some(Anchor::Eyes) => 1,
            _ => 0,
        };
        let network_id = to
            .entity
            .and_then(|id| env.level.entities.store.get(id))
            .map(|e| e.network_id().0);
        let pos = to.place.map_or(source.pos, |(p, _, _)| p);
        Some((
            pos,
            network_id.map(|entity_id| LookAtEntity { entity_id, anchor }),
        ))
    } else {
        None
    };
    let Some((pos, entity)) = packet else {
        return Ok(());
    };
    for target in targets {
        if let Some((sink, _)) = env
            .global
            .players
            .connection(crate::player_list::Uuid(target.uuid))
        {
            sink.send(&PlayerLookAt {
                from_anchor: 0,
                x: pos.x,
                y: pos.y,
                z: pos.z,
                entity,
            });
        }
    }
    Ok(())
}

fn targets_single(
    env: &Env<'_>,
    source: &CommandSource,
    selector: &EntitySelector,
) -> Result<Target, CommandError> {
    select::single(env, source, selector)
}

/// Executes a parsed command; errors are shown to the source by the caller.
pub(crate) fn execute(
    source: &CommandSource,
    env: &mut Env<'_>,
    parsed: &Parsed<CommandId>,
    signed: &SignedArguments,
) -> Result<i32, CommandError> {
    let global = env.global;
    match parsed.command {
        CommandId::GameModeSelf | CommandId::GameModeTargets => {
            let mode = parsed
                .args
                .game_mode("gamemode")
                .ok_or_else(|| missing("gamemode"))?;
            let targets = if parsed.command == CommandId::GameModeSelf {
                require_player(source)?;
                vec![self_target(env, source)?]
            } else {
                targets(env, source, selector_arg(parsed, "target")?, true)?
            };
            Ok(exec::game_mode(
                source,
                &mut env.level.entities,
                global,
                targets,
                config_game_mode(*mode),
            ))
        }
        CommandId::Help | CommandId::HelpCommand => {
            let command = parsed.args.string("command").map(String::as_str);
            let lines = global.commands.help_lines(command, source)?;
            for line in &lines {
                source.send_success(global, Component::text(line.as_str()), false);
            }
            Ok(i32::try_from(lines.len()).unwrap_or(i32::MAX))
        }
        CommandId::Kick => {
            let targets = targets(env, source, selector_arg(parsed, "targets")?, true)?;
            let reason = match parsed.args.message("reason") {
                Some(message) => resolve_message(env, source, message)?,
                None => Component::translatable("multiplayer.disconnect.kicked", vec![]),
            };
            Ok(exec::kick(source, global, &targets, reason))
        }
        CommandId::List => Ok(exec::list(source, global, false)),
        CommandId::ListUuids => Ok(exec::list(source, global, true)),
        CommandId::Msg => {
            let targets = targets(env, source, selector_arg(parsed, "targets")?, true)?;
            let message = chat_message(env, source, parsed, signed)?;
            Ok(exec::msg(source, global, &targets, &message))
        }
        CommandId::Say => {
            let message = chat_message(env, source, parsed, signed)?;
            exec::say(source, global, &message);
            Ok(1)
        }
        CommandId::Seed => Ok(exec::seed(source, global, global.seed)),
        CommandId::TeleportToLocation
        | CommandId::TeleportToEntity
        | CommandId::TeleportTargetsToLocation
        | CommandId::TeleportTargetsToEntity => {
            let victims = match parsed.command {
                CommandId::TeleportToLocation | CommandId::TeleportToEntity => {
                    vec![self_target(env, source)?]
                }
                _ => targets(env, source, selector_arg(parsed, "targets")?, false)?,
            };
            let local: Vec<Target> = victims.into_iter().filter(|t| t.entity.is_some()).collect();
            let destination = match parsed.command {
                CommandId::TeleportToEntity | CommandId::TeleportTargetsToEntity => {
                    Destination::Entity(targets_single(
                        env,
                        source,
                        selector_arg(parsed, "destination")?,
                    )?)
                }
                _ => Destination::Position {
                    coordinates: *parsed
                        .args
                        .coordinates("location")
                        .ok_or_else(|| missing("location"))?,
                    rotation: parsed.args.coordinates("rotation").copied(),
                },
            };
            let count = exec::teleport(
                source,
                &mut env.level.entities,
                global,
                local.clone(),
                destination,
            )?;
            look_at(env, source, &local, parsed)?;
            Ok(count)
        }
        CommandId::TimeQueryGameTime => Ok(exec::time_gametime(
            source,
            global,
            global.level.game_time(),
        )),
        CommandId::TimeQueryTime
        | CommandId::TimeQueryTimeline
        | CommandId::TimeQueryRepetition
        | CommandId::TimeSet
        | CommandId::TimeSetMarker
        | CommandId::TimeAdd
        | CommandId::TimePause
        | CommandId::TimeResume
        | CommandId::TimeRate => {
            let clock = survival::clock(
                source,
                global,
                parsed.args.identifier("clock").map(String::as_str),
            )?;
            match parsed.command {
                CommandId::TimeQueryTime => Ok(survival::time_query(source, global, clock)),
                CommandId::TimeQueryTimeline | CommandId::TimeQueryRepetition => {
                    let timeline = parsed
                        .args
                        .identifier("timeline")
                        .ok_or_else(|| missing("timeline"))?;
                    survival::time_timeline(
                        source,
                        global,
                        clock,
                        timeline,
                        parsed.command == CommandId::TimeQueryRepetition,
                    )
                }
                CommandId::TimeSet => {
                    let time = parsed.args.time("time").ok_or_else(|| missing("time"))?;
                    survival::time_set(source, global, clock, *time)
                }
                CommandId::TimeSetMarker => {
                    let marker = parsed
                        .args
                        .identifier("timemarker")
                        .ok_or_else(|| missing("timemarker"))?;
                    survival::time_set_marker(source, global, clock, marker)
                }
                CommandId::TimeAdd => {
                    let time = parsed.args.time("time").ok_or_else(|| missing("time"))?;
                    Ok(survival::time_add(source, global, clock, *time))
                }
                CommandId::TimePause => survival::time_pause(source, global, clock, true),
                CommandId::TimeResume => survival::time_pause(source, global, clock, false),
                _ => {
                    let rate = parsed.args.float("rate").ok_or_else(|| missing("rate"))?;
                    survival::time_rate(source, global, clock, *rate)
                }
            }
        }
        CommandId::GameRuleQuery(index) => Ok(survival::game_rule_query(
            source,
            global,
            Rule(usize::from(index)),
        )),
        CommandId::GameRuleSet(index) => {
            let rule = Rule(usize::from(index));
            let value = match (parsed.args.bool("value"), parsed.args.int("value")) {
                (Some(b), _) => i32::from(*b),
                (None, Some(i)) => *i,
                (None, None) => return Err(missing("value")),
            };
            survival::game_rule_set(source, global, rule, value)
        }
        CommandId::KillSelf | CommandId::KillTargets => {
            let victims = if parsed.command == CommandId::KillSelf {
                vec![self_target(env, source)?]
            } else {
                targets(env, source, selector_arg(parsed, "targets")?, false)?
            };
            Ok(survival::kill(source, env, &victims))
        }
        CommandId::WeatherClear | CommandId::WeatherRain | CommandId::WeatherThunder => {
            let kind = match parsed.command {
                CommandId::WeatherClear => survival::Weather::Clear,
                CommandId::WeatherRain => survival::Weather::Rain,
                _ => survival::Weather::Thunder,
            };
            let duration = parsed.args.time("duration").copied();
            Ok(survival::weather(source, global, kind, duration))
        }
        CommandId::Op => {
            let profiles = profiles(env, source, parsed)?;
            exec::op(source, global, &profiles)
        }
        CommandId::Deop => {
            let profiles = profiles(env, source, parsed)?;
            exec::deop(source, global, &profiles)
        }
        CommandId::SaveAll => Ok(exec::save_all(source, global, false)),
        CommandId::SaveAllFlush => Ok(exec::save_all(source, global, true)),
        CommandId::SaveOff => exec::save_switch(source, global, false),
        CommandId::SaveOn => exec::save_switch(source, global, true),
        CommandId::Stop => Ok(exec::stop(source, global)),
        CommandId::WhitelistOn => exec::whitelist_switch(source, global, true),
        CommandId::WhitelistOff => exec::whitelist_switch(source, global, false),
        CommandId::WhitelistList => Ok(exec::whitelist_list(source, global)),
        CommandId::WhitelistAdd => {
            let profiles = profiles(env, source, parsed)?;
            exec::whitelist_change(source, global, &profiles, true)
        }
        CommandId::WhitelistRemove => {
            let profiles = profiles(env, source, parsed)?;
            exec::whitelist_change(source, global, &profiles, false)
        }
        CommandId::WhitelistReload => Ok(exec::whitelist_reload(source, global)),
        CommandId::Execute(ExecuteOp::Test { condition, unless }) => {
            super::execute::run_condition(condition, unless, &parsed.args, source, env)
        }
        CommandId::Scoreboard(op) => super::scores::run(op, &parsed.args, source, env),
        CommandId::SetBlock(mode) => super::blocks_cmd::set_block(mode, &parsed.args, source, env),
        CommandId::Fill(mode) => super::blocks_cmd::fill(mode, &parsed.args, source, env),
        CommandId::Clone {
            filter,
            mode,
            strict,
        } => super::blocks_cmd::clone((filter, mode, strict), &parsed.args, source, env),
        CommandId::FillBiome => super::blocks_cmd::fill_biome(&parsed.args, source, env),
        CommandId::ForceLoad(op) => super::world_cmd::force_load(op, &parsed.args, source, env),
        CommandId::SetWorldSpawn => super::world_cmd::set_world_spawn(&parsed.args, source, env),
        CommandId::SpawnPoint => {
            let targets = if parsed.args.has("targets") {
                targets(env, source, selector_arg(parsed, "targets")?, true)?
            } else {
                require_player(source)?;
                vec![self_target(env, source)?]
            };
            super::world_cmd::spawn_point(&targets, &parsed.args, source, env)
        }
        CommandId::DifficultyQuery => Ok(super::world_cmd::difficulty_query(source, global)),
        CommandId::DifficultySet(id) => super::world_cmd::difficulty_set(source, global, id),
        CommandId::DefaultGameMode => {
            super::world_cmd::default_game_mode(&parsed.args, source, env)
        }
        CommandId::SpreadPlayers => {
            let targets = targets(env, source, selector_arg(parsed, "targets")?, false)?;
            super::world_cmd::spread_players(targets, &parsed.args, source, env)
        }
        CommandId::Tick(op) => super::world_cmd::tick(op, &parsed.args, source, global),
        CommandId::WorldBorder(op) => super::border_cmd::run(op, &parsed.args, source, env),
        CommandId::Locate(op) => super::locate::locate(op, &parsed.args, source, env),
        CommandId::Place(op) => super::locate::place(op, &parsed.args, source, env),
        // Redirect modifiers never end a command; the engine runs `function` and `return`.
        CommandId::Execute(_)
        | CommandId::ExecuteIfFunction
        | CommandId::ExecuteUnlessFunction
        | CommandId::Function
        | CommandId::FunctionWithArguments
        | CommandId::FunctionWith
        | CommandId::FunctionWithPath
        | CommandId::ReturnValue
        | CommandId::ReturnFail
        | CommandId::ReturnRun => Err(CommandError::new("command.unknown.command", vec![])),
    }
}

//! Commands of players and the console: parsing through the command tree with the source's
//! permission (SEC-11), signed message arguments, execution with Vanilla's feedback, tab
//! completion and the command tree packet. Permissions are checked centrally here: the tree
//! only offers and parses nodes the source may use, and every command node carries its
//! required level (SEC-12).

mod advancement_cmd;
mod bans;
mod blocks_cmd;
mod bossbar_cmd;
mod border_cmd;
mod chat_cmd;
mod data;
mod data_cmd;
mod dispatch;
mod engine;
mod entities_cmd;
mod exec;
mod execute;
pub(crate) mod functions;
mod items;
mod locate;
mod nbt_ops;
mod pack_cmd;
mod particle;
mod player_data;
mod schedule;
mod scores;
mod select;
mod sound;
mod source;
mod survival;
mod system_cmd;
pub(crate) mod teams;
mod text;
mod tracker;
mod world_cmd;

use std::collections::HashMap;
use std::sync::Arc;

use vanillite_command::error::CommandError;
use vanillite_game::commands::CommandId;
use vanillite_game::entities::Entities;
use vanillite_game::player::chat::{ChainError, ChatMessage, MessageBody};
use vanillite_game::player::{PacketSink, SignedCommand};
use vanillite_protocol::packets::play::clientbound::{Commands, SystemChat};
use vanillite_protocol::text::{Arg, ClickEvent, Color, Component, NamedColor};
use vanillite_world::entity::EntityId;

pub(crate) use bans::ban_reason;
pub(crate) use exec::{find_player, permission_event};
pub(crate) use player_data::add_experience;
pub(crate) use sound::send_post_effects;
pub(crate) use source::{CommandSource, SourceKind};
pub(crate) use world_cmd::sprint_finished;

use crate::global::Global;
use crate::world::{ServerWorld, WorldId};

/// The command tree of the server: built once, shared by all worlds.
pub(crate) type CommandDispatcher = vanillite_command::Dispatcher<CommandId>;

/// Builds the dispatcher with the configured limits.
pub(crate) fn build_dispatcher(limits: vanillite_command::limits::Limits) -> CommandDispatcher {
    let mut dispatcher = CommandDispatcher::new();
    dispatcher.set_limits(limits);
    vanillite_game::commands::register(&mut dispatcher);
    dispatcher
}

/// What a command execution may touch: the world it runs on and the server.
pub(crate) struct Env<'a> {
    pub(crate) level: &'a mut vanillite_game::level::Level,
    pub(crate) global: &'a Global,
    /// The world of `level`.
    pub(crate) world: WorldId,
}

/// Loads the functions and advancements of the selected data packs once the server exists
/// (`ServerFunctionLibrary.reload`, `ServerAdvancementManager`).
pub(crate) fn load_game_systems(global: &Global, packs: &[vanillite_datapack::OpenedPack]) {
    global.systems.set_functions(functions::load(global, packs));
    global.systems.load(&global.level.data);
    let roots: Vec<std::path::PathBuf> = packs.iter().flat_map(|p| p.layers.clone()).collect();
    let files = functions::list(&roots, "advancement", ".json")
        .into_iter()
        .filter_map(|(id, file)| match std::fs::read_to_string(&file) {
            Ok(text) => Some((id, text)),
            Err(e) => {
                log::error!("Couldn't read advancement {id}: {e}");
                None
            }
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    global
        .systems
        .set_advancements(vanillite_game::advancements::Advancements::load(&files));
}

/// `ServerFunctionManager.tick` on the overworld.
pub(crate) fn tick_functions(
    level: &mut vanillite_game::level::Level,
    global: &Global,
    world: WorldId,
) {
    let mut env = Env {
        level,
        global,
        world,
    };
    functions::tick(&mut env);
}

/// The scheduled functions due now (`TimerQueue.tick` in the overworld's `tickTime`).
pub(crate) fn tick_scheduled(
    level: &mut vanillite_game::level::Level,
    global: &Global,
    world: WorldId,
) {
    let mut env = Env {
        level,
        global,
        world,
    };
    schedule::tick(&mut env);
}

/// Runs a parsed line for a source in a new execution (`Commands.executeCommandInContext`).
fn run_line(env: &mut Env<'_>, line: engine::Line, source: CommandSource) {
    let mut engine = functions::new_engine(env.global);
    engine.queue_command(line, source, engine::Callback::Empty);
    engine.run(env);
}

/// The permission level of a player: its operator level or 0.
pub(crate) fn permission_level(global: &Global, uuid: u128) -> u8 {
    global.access.op_level(uuid).unwrap_or(0)
}

/// The source of a player of this world.
pub(crate) fn player_source(
    entities: &Entities,
    global: &Global,
    id: EntityId,
    world: WorldId,
) -> Option<CommandSource> {
    let entity = entities.store.get(id)?;
    let player = entities.player(id)?;
    let uuid = entity.uuid();
    let name = player.profile.name.clone();
    let target = select::local(entities, world, id)?;
    Some(CommandSource::new(
        global.arc()?,
        SourceKind::Player {
            uuid,
            name,
            sink: Arc::clone(&player.connection.sink),
        },
        permission_level(global, uuid),
        world,
        entity.pos(),
        (entity.rot().y_rot, entity.rot().x_rot),
        Some(target.source_entity()),
    ))
}

/// The tree packet for a source (`Commands.sendCommands`).
pub(crate) fn tree_packet(global: &Global, source: &CommandSource) -> Commands {
    Commands(global.commands.to_tree(source))
}

/// Sends a joining player its permission level and command tree.
pub(crate) fn send_permissions(
    global: &Global,
    sink: &dyn PacketSink,
    entity_id: i32,
    source: &CommandSource,
) {
    sink.send(&permission_event(entity_id, source.level));
    sink.send(&tree_packet(global, source));
}

/// `PlayerList.sendPlayerPermissionLevel` after `op`/`deop`: on the player's world.
pub(crate) fn permissions_changed(global: &Global, uuid: u128) {
    let Some(world) = exec::world_of(global, uuid) else {
        return;
    };
    let Some(global_arc) = global.arc() else {
        return;
    };
    global.run_in_world(world, move |w: &mut ServerWorld| {
        let entities = w.entities();
        let Some(id) = exec::find_player(entities, uuid) else {
            return;
        };
        let (Some(source), Some(entity)) = (
            player_source(entities, &global_arc, id, w.id()),
            entities.store.get(id),
        ) else {
            return;
        };
        if let SourceKind::Player { sink, .. } = &source.kind {
            send_permissions(&global_arc, sink.as_ref(), entity.network_id().0, &source);
        }
    });
}

/// Vanilla's two error lines: the message and, for syntax errors, the context with a click
/// that suggests the command again (`Commands.performCommand`).
fn send_error(source: &CommandSource, global: &Global, error: &CommandError, command: &str) {
    source.send_failure(global, error_message(error));
    let Some(context) = error.context() else {
        return;
    };
    let mut line = Component::empty()
        .color(Color::Named(NamedColor::Gray))
        .click(ClickEvent::SuggestCommand(format!("/{command}")));
    if context.ellipsis {
        line = line.append(Component::text("..."));
    }
    line = line.append(Component::text(context.before));
    if !context.after.is_empty() {
        line = line.append(
            Component::text(context.after)
                .color(Color::Named(NamedColor::Red))
                .underlined(true),
        );
    }
    line = line.append(
        Component::translatable("command.context.here", vec![])
            .color(Color::Named(NamedColor::Red))
            .italic(true),
    );
    source.send_failure(global, line);
}

/// The error message with Vanilla's primitive translation arguments.
pub(crate) fn error_message(error: &CommandError) -> Component {
    use vanillite_command::error::Arg as E;
    let args = error
        .args
        .iter()
        .map(|a| match a {
            E::Str(s) => Arg::String(s.clone()),
            E::Int(v) => Arg::Int(*v),
            E::Long(v) => Arg::Long(*v),
            E::Float(v) => Arg::Float(*v),
            E::Double(v) => Arg::Double(*v),
            E::Component(c) => Arg::from((**c).clone()),
        })
        .collect();
    Component::translatable_args(error.key, args)
}

/// Signed arguments of a command, by name (`CommandSigningContext.SignedArguments`).
pub(crate) type SignedArguments = HashMap<String, ChatMessage>;

/// A player runs a command line (without the slash): `performUnsignedChatCommand` or
/// `performSignedChatCommand`, then the spam check. Returns a kick reason.
pub(crate) fn run_player_command(
    level: &mut vanillite_game::level::Level,
    global: &Global,
    id: EntityId,
    world: WorldId,
    command: &str,
    signed: Option<SignedCommand>,
) -> Option<Component> {
    let entities = &mut level.entities;
    let source = player_source(entities, global, id, world)?;
    let uuid = source.uuid()?;
    let name = source.text_name().to_owned();
    let parse = global.commands.parse(command, &source);
    let signable = parse.signable_arguments(&global.commands);
    let signed_arguments = {
        let player = entities.player_mut(id)?;
        let sink = Arc::clone(&player.connection.sink);
        let chain = &mut player.connection.chat.chain;
        let enforce = global.chat.enforce_secure;
        let result = match &signed {
            None if enforce && !signable.is_empty() => {
                log::error!(
                    "Received unsigned command packet from {name}, but the command requires signable arguments: {command}"
                );
                Err(ChainError::InvalidCommandSignature)
            }
            None => Ok(SignedArguments::new()),
            Some(signed) => {
                collect_signed_arguments(chain, uuid, signed, &signable, enforce, command)
            }
        };
        match result {
            Ok(arguments) => arguments,
            Err(e) => {
                if e == ChainError::InvalidCommandSignature {
                    sink.send(&SystemChat {
                        content: Component::translatable(e.key(), vec![])
                            .color(Color::Named(NamedColor::Red)),
                        overlay: false,
                    });
                } else {
                    crate::chat::decode_failure(sink.as_ref(), &name, e);
                }
                return None;
            }
        }
    };
    if !global.plugins.player_command(uuid, &name, command) {
        return None;
    }
    log::info!("{name} issued server command: /{command}");
    let mut env = Env {
        level,
        global,
        world,
    };
    match parse.finish(&global.commands) {
        Ok(parsed) => {
            let line = engine::Line {
                parsed: Arc::new(parsed),
                signed: Some(Arc::new(signed_arguments)),
            };
            run_line(&mut env, line, source);
        }
        Err(e) => send_error(&source, global, &e, command),
    }
    // `detectCommandRateSpam`.
    let player = env.level.entities.player_mut(id)?;
    let spam = &mut player.connection.chat.command_spam;
    spam.increment();
    (!spam.is_under_threshold() && global.access.op_level(uuid).is_none())
        .then(|| Component::translatable("disconnect.spam", vec![]))
}

/// `collectSignedArguments`: every signed argument must be one the command has, and every
/// signable argument must be signed.
fn collect_signed_arguments(
    chain: &mut vanillite_game::player::chat::MessageChain,
    uuid: u128,
    signed: &SignedCommand,
    signable: &[(String, String)],
    enforce: bool,
    command: &str,
) -> Result<SignedArguments, ChainError> {
    let mut out = SignedArguments::new();
    if signed.signatures.is_empty() {
        for (name, value) in signable {
            let message =
                chain.unpack(uuid, None, MessageBody::unsigned(value.clone()), enforce)?;
            out.insert(name.clone(), message);
        }
        return Ok(out);
    }
    let mismatch = || {
        log::error!("Signed command mismatch between server and client ('{command}')");
        ChainError::InvalidCommandSignature
    };
    for argument in &signed.signatures {
        let Some((name, value)) = signable.iter().find(|(n, _)| *n == argument.name) else {
            chain.set_broken();
            return Err(mismatch());
        };
        let body = MessageBody {
            content: value.clone(),
            timestamp: signed.timestamp,
            salt: signed.salt,
            last_seen: signed.last_seen.clone(),
        };
        let message = chain.unpack(uuid, Some(argument.signature.clone()), body, enforce)?;
        out.insert(name.clone(), message);
    }
    if signable.iter().any(|(n, _)| !out.contains_key(n)) {
        return Err(mismatch());
    }
    Ok(out)
}

/// A console command, run on the overworld.
pub(crate) fn run_console_command(world: &mut ServerWorld, global: &Global, line: &str) {
    let command = line.trim().trim_start_matches('/');
    if command.is_empty() {
        return;
    }
    let Some(arc) = global.arc() else {
        return;
    };
    let source = CommandSource::new(
        arc,
        SourceKind::Console,
        4,
        world.id(),
        global.systems.spawn(),
        (0.0, 0.0),
        None,
    );
    let id = world.id();
    let mut env = Env {
        level: world.level_mut(),
        global,
        world: id,
    };
    match global.commands.parse_command(command, &source) {
        Ok(parsed) => {
            let line = engine::Line {
                parsed: Arc::new(parsed),
                signed: None,
            };
            run_line(&mut env, line, source);
        }
        Err(e) => {
            log::warn!("{}", error_message(&e).plain());
            if let Some(context) = e.context() {
                log::warn!(
                    "{}{}{}<--[HERE]",
                    if context.ellipsis { "..." } else { "" },
                    context.before,
                    context.after
                );
            }
        }
    }
}

/// Answers the latest tab completion request of each player of this world, once per tick
/// (`ServerCommandSuggestionsProvider`).
pub(crate) fn answer_suggestions(entities: &mut Entities, global: &Global, world: WorldId) {
    let pending: Vec<(EntityId, i32, String)> = entities
        .players
        .iter_mut()
        .filter_map(|(id, p)| {
            p.connection
                .chat
                .suggestion
                .take()
                .map(|(rid, cmd)| (id, rid, cmd))
        })
        .collect();
    for (id, request, line) in pending {
        let Some(source) = player_source(entities, global, id, world) else {
            continue;
        };
        let max = global.commands.limits().max_suggestions;
        let packet = global
            .commands
            .suggest(&line, &source)
            .to_packet(request, &line, max);
        if let SourceKind::Player { sink, .. } = &source.kind {
            sink.send(&packet);
        }
    }
}

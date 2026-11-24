//! Commands of players and the console: parsing through the command tree with the source's
//! permission (SEC-11), signed message arguments, execution with Vanilla's feedback, tab
//! completion and the command tree packet. Permissions are checked centrally here: the tree
//! only offers and parses nodes the source may use, and every command node carries its
//! required level (SEC-12).

mod dispatch;
mod exec;
mod select;
mod source;

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

pub(crate) use exec::permission_event;
pub(crate) use source::{CommandSource, SourceKind};

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

/// What a command execution may touch: the source's world and the server.
pub(crate) struct Env<'a> {
    pub(crate) entities: &'a mut Entities,
    pub(crate) global: &'a Global,
    /// Ticks the world has run (game time).
    pub(crate) game_time: i64,
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
    Some(CommandSource {
        global: global.arc()?,
        kind: SourceKind::Player {
            uuid,
            name: player.profile.name.clone(),
            sink: Arc::clone(&player.connection.sink),
        },
        level: permission_level(global, uuid),
        world,
        pos: entity.pos(),
        rot: (entity.rot().y_rot, entity.rot().x_rot),
    })
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
fn error_message(error: &CommandError) -> Component {
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
    entities: &mut Entities,
    global: &Global,
    id: EntityId,
    world: WorldId,
    game_time: i64,
    command: &str,
    signed: Option<SignedCommand>,
) -> Option<Component> {
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
    log::info!("{name} issued server command: /{command}");
    let mut env = Env {
        entities,
        global,
        game_time,
    };
    match parse.finish(&global.commands) {
        Ok(parsed) => {
            if let Err(e) = dispatch::execute(&source, &mut env, &parsed, &signed_arguments) {
                send_error(&source, global, &e, command);
            }
        }
        Err(e) => send_error(&source, global, &e, command),
    }
    // `detectCommandRateSpam`.
    let player = env.entities.player_mut(id)?;
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
    let source = CommandSource {
        global: arc,
        kind: SourceKind::Console,
        level: 4,
        world: world.id(),
        pos: vanillite_base::Vec3::ZERO,
        rot: (0.0, 0.0),
    };
    let game_time = i64::try_from(world.ticks()).unwrap_or(i64::MAX);
    let mut env = Env {
        entities: world.entities_mut(),
        global,
        game_time,
    };
    match global.commands.parse_command(command, &source) {
        Ok(parsed) => {
            if let Err(e) = dispatch::execute(&source, &mut env, &parsed, &SignedArguments::new()) {
                log::warn!("{}", error_message(&e).plain());
            }
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

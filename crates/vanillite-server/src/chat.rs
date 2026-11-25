//! Chat and command requests of players, handled on their world thread after their packets
//! (Vanilla's `ServerGamePacketListenerImpl.handleChat` and friends, `PlayerList.broadcastChatMessage`).

use vanillite_game::entities::Entities;
use vanillite_game::player::chat::{ChainError, ChatMessage, MessageBody, now_millis};
use vanillite_game::player::{ChatRequest, PacketSink};
use vanillite_protocol::chat::{ChatSessionData, ChatTypeBound};
use vanillite_protocol::packets::play::clientbound::{PlayerInfoUpdate, SystemChat, info_action};
use vanillite_protocol::text::{Color, Component, NamedColor};
use vanillite_world::entity::EntityId;

use crate::commands::BookAt;
use crate::global::Global;
use crate::player_list::Uuid;

/// Handles the queued chat requests of a player in order. Players to kick are added to
/// `leaving`.
pub(crate) fn handle_requests(
    level: &mut vanillite_game::level::Level,
    global: &Global,
    id: EntityId,
    world: crate::world::WorldId,
    leaving: &mut Vec<(EntityId, Option<Box<Component>>)>,
) {
    let entities = &mut level.entities;
    let Some(uuid) = entities.store.get(id).map(|e| e.uuid()) else {
        return;
    };
    let requests = match entities.player_mut(id) {
        Some(player) => std::mem::take(&mut player.connection.chat.requests),
        None => return,
    };
    for request in requests {
        let kicked = match request {
            ChatRequest::Chat {
                message,
                timestamp,
                salt,
                signature,
                last_seen,
            } => {
                let body = MessageBody {
                    content: message,
                    timestamp,
                    salt,
                    last_seen,
                };
                chat(&mut level.entities, global, id, uuid, signature, body)
            }
            ChatRequest::Session(data) => {
                session(global, uuid, data);
                None
            }
            ChatRequest::Admin(request) => {
                crate::commands::handle_admin(level, global, id, world, request);
                None
            }
            ChatRequest::Command { command, signed } => {
                crate::commands::run_player_command(level, global, id, world, &command, signed)
            }
            ChatRequest::OpenBook(hand) => {
                crate::commands::resolve_book(level, global, id, world, BookAt::Hand(hand));
                None
            }
            ChatRequest::ResolveLecternBook(pos) => {
                crate::commands::resolve_book(level, global, id, world, BookAt::Lectern(pos));
                None
            }
        };
        if let Some(reason) = kicked {
            leaving.push((id, Some(Box::new(reason))));
            return;
        }
    }
}

/// `handleChat` after the last-seen update: unpack the message from the chain and broadcast it.
/// Returns a kick reason when the player spams.
fn chat(
    entities: &mut Entities,
    global: &Global,
    id: EntityId,
    uuid: u128,
    signature: Option<vanillite_protocol::chat::MessageSignature>,
    body: MessageBody,
) -> Option<Component> {
    let player = entities.player_mut(id)?;
    let name = player.profile.name.clone();
    // Virtual connections (Bedrock players) cannot sign; their identity is vouched for.
    let enforce = global.chat.enforce_secure && !player.connection.is_virtual;
    let unpacked = player
        .connection
        .chat
        .chain
        .unpack(uuid, signature, body, enforce);
    let message = match unpacked {
        Ok(message) => message,
        Err(e) => {
            decode_failure(player.connection.sink.as_ref(), &name, e);
            return None;
        }
    };
    if global.plugins.wants(vanillite_abi::event::PLAYER_CHAT)
        && !global
            .plugins
            .player_chat(uuid, &name, &message.decorated_content().plain())
    {
        return None;
    }
    let chat_type = ChatTypeBound {
        chat_type: global.chat_types.chat,
        name: crate::scoreboard::display_name(global, &name, uuid),
        target_name: None,
    };
    broadcast_chat(global, &message, &chat_type, "chat.type.text");
    let spam = &mut player.connection.chat.chat_spam;
    spam.increment();
    (!spam.is_under_threshold() && global.access.op_level(uuid).is_none())
        .then(|| Component::translatable("disconnect.spam", vec![]))
}

/// `handleMessageDecodeFailure`: logged and shown to the sender in red.
pub(crate) fn decode_failure(sink: &dyn PacketSink, name: &str, error: ChainError) {
    log::warn!(
        "Failed to update secure chat state for {name}: '{}'",
        error.key()
    );
    sink.send(&SystemChat {
        content: Component::translatable(error.key(), vec![]).color(Color::Named(NamedColor::Red)),
        overlay: false,
    });
}

/// `PlayerList.broadcastChatMessage`: logs the message like Vanilla and sends it to everyone.
/// `log_key` is the chat type's translation key for the log line.
pub(crate) fn broadcast_chat(
    global: &Global,
    message: &ChatMessage,
    chat_type: &ChatTypeBound,
    log_key: &str,
) {
    let content = message.decorated_content().plain();
    let line = decorate_for_log(
        log_key,
        &chat_type.name.plain(),
        chat_type.target_name.as_ref().map(Component::plain),
        &content,
    );
    if message.is_trusted(now_millis()) {
        log::info!("{line}");
    } else {
        log::info!("[Not Secure] {line}");
    }
    global
        .players
        .broadcast_chat(message, chat_type, global.chat.max_pending_messages);
}

/// The English text of a chat type decoration, for the server log (`ChatType.Bound.decorate`
/// rendered with `en_us`).
fn decorate_for_log(key: &str, sender: &str, target: Option<String>, content: &str) -> String {
    let target = target.unwrap_or_default();
    match key {
        "chat.type.announcement" => format!("[{sender}] {content}"),
        "chat.type.emote" => format!("* {sender} {content}"),
        "commands.message.display.incoming" => {
            format!("{sender} whispers to you: {content}")
        }
        "commands.message.display.outgoing" => format!("You whisper to {target}: {content}"),
        _ => format!("<{sender}> {content}"),
    }
}

/// `resetPlayerChatState`: the session goes into the player's tab list entry and to everyone.
fn session(global: &Global, uuid: u128, data: ChatSessionData) {
    global
        .players
        .update_info(Uuid(uuid), |e| e.chat_session = Some(data));
    if let Some(entry) = global
        .players
        .info_entries()
        .into_iter()
        .find(|e| e.uuid == uuid)
    {
        let update = PlayerInfoUpdate {
            actions: info_action::INITIALIZE_CHAT,
            ..vanillite_game::player::join::player_info_add([&entry])
        };
        global.players.broadcast(&update, None);
    }
}

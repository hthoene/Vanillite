//! A player's connection as the world sees it, and the play packet handlers of milestone 1
//! (Vanilla's `ServerGamePacketListenerImpl`).

use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use vanillite_base::Vec3;
use vanillite_protocol::Inbound;
use vanillite_protocol::chat::{
    ArgumentSignature, ChatSessionData, LastSeenUpdate, MessageSignature,
};
use vanillite_protocol::packets::common::ChatVisibility;
use vanillite_protocol::packets::common::{ClientInfo, MainHand as WireMainHand};
use vanillite_protocol::packets::play::clientbound::SystemChat;
use vanillite_protocol::packets::play::serverbound::{ClientCommandAction, PlayerAction};
use vanillite_protocol::sets::ServerboundPlay;
use vanillite_protocol::text::{Color, Component, NamedColor};

use super::chat::{
    ChatInbox, ChatSession, ChatSettings, MessageChain, TickThrottler, is_chat_message_illegal,
};
use super::chunks::PlayerChunks;
use super::movement::{Body, Kick, MoveContext, MoveRequest, Movement};
use super::sink::PacketSink;
use super::{ClientInformation, MainHand};

/// The state of a player that its packets change. The caller copies it from the entity store
/// and player data before handling packets and writes changed values back.
#[derive(Clone, Debug, PartialEq)]
pub struct PlayerState {
    /// Position, rotation and movement flags.
    pub body: Body,
    /// The sneak key is held (`setShiftKeyDown`).
    pub shift_key_down: bool,
    /// Sprinting.
    pub sprinting: bool,
    /// New client settings, when the client sent them.
    pub client: Option<ClientInformation>,
    /// The client asked to respawn (`client_command` `PERFORM_RESPAWN`).
    pub respawn: bool,
}

/// How handling a player's packets ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Handled {
    /// All queued packets (up to the budget) were handled.
    Ok,
    /// The connection is gone; the player leaves.
    Disconnected,
    /// The player must be kicked with this reason; the player leaves.
    Kick(Box<Component>),
}

/// The signed part of a `chat_command_signed` packet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedCommand {
    /// Epoch milliseconds.
    pub timestamp: i64,
    /// Salt.
    pub salt: i64,
    /// Signatures of the message arguments.
    pub signatures: Vec<ArgumentSignature>,
    /// The signatures the client had seen.
    pub last_seen: Vec<MessageSignature>,
}

/// A chat or command request of a player, handled by the server after the player's packets in
/// packet order (Vanilla runs them on the server thread through `chatMessageChain`).
#[derive(Clone, Debug)]
pub enum ChatRequest {
    /// A chat message; its last-seen update is already applied.
    Chat {
        /// The text (at most 256 characters, no illegal characters).
        message: String,
        /// Epoch milliseconds.
        timestamp: i64,
        /// Salt.
        salt: i64,
        /// The signature, if the client signed it.
        signature: Option<MessageSignature>,
        /// The signatures the client had seen.
        last_seen: Vec<MessageSignature>,
    },
    /// A command without the leading slash.
    Command {
        /// The command line.
        command: String,
        /// Signature data of `chat_command_signed`.
        signed: Option<SignedCommand>,
    },
    /// The player started a new, validated chat session; other players learn it through the
    /// tab list.
    Session(ChatSessionData),
}

/// A player's chat state.
#[derive(Debug)]
pub struct PlayerChatState {
    /// The sender side: session and signed message chain.
    pub chain: MessageChain,
    /// The receiver side, shared with everyone who sends chat to this player.
    pub inbox: Arc<Mutex<ChatInbox>>,
    /// `chatSpamThrottler`.
    pub chat_spam: TickThrottler,
    /// `commandSpamThrottler`.
    pub command_spam: TickThrottler,
    /// The latest tab completion request (Vanilla answers the latest one, one per tick).
    pub suggestion: Option<(i32, String)>,
    /// Requests of this tick, in packet order.
    pub requests: Vec<ChatRequest>,
}

impl PlayerChatState {
    /// The state of a joining player with its receiver side.
    pub fn new(settings: &ChatSettings, inbox: Arc<Mutex<ChatInbox>>) -> PlayerChatState {
        PlayerChatState {
            chain: MessageChain::default(),
            inbox,
            chat_spam: TickThrottler::new(20, settings.chat_spam_threshold),
            command_spam: TickThrottler::new(20, settings.command_spam_threshold),
            suggestion: None,
            requests: Vec::new(),
        }
    }

    /// Locks the receiver side.
    pub fn inbox(&self) -> MutexGuard<'_, ChatInbox> {
        self.inbox.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// What the chat handlers need from the server.
#[derive(Clone, Copy, Debug)]
pub struct ChatContext<'a> {
    /// The player's UUID.
    pub uuid: u128,
    /// The player's name, for logs.
    pub name: &'a str,
    /// Server chat settings.
    pub settings: &'a ChatSettings,
}

/// A player's connection: packet queues and per-connection protocol state.
pub struct Connection {
    /// Clientbound packets.
    pub sink: Arc<dyn PacketSink>,
    inbound: Receiver<Inbound>,
    /// Movement validation and teleports.
    pub movement: Movement,
    /// Chunk view and sending.
    pub chunks: PlayerChunks,
    /// Chat and commands.
    pub chat: PlayerChatState,
    /// Network ids of the entities the player attacked since the last tick (`attack`); the
    /// world resolves them with the entities.
    pub attacks: Vec<i32>,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("movement", &self.movement)
            .field("chunks", &self.chunks)
            .field("chat", &self.chat)
            .finish_non_exhaustive()
    }
}

/// `ChunkMap.getPlayerViewDistance`: the client's view distance clamped to 2 and the
/// server's view distance (SEC-19).
pub fn effective_view_distance(client: &ClientInformation, server_view_distance: i32) -> i32 {
    i32::from(client.view_distance).clamp(2, server_view_distance.max(2))
}

impl From<&ClientInfo> for ClientInformation {
    /// Takes over what the server uses; a negative view distance counts as 0 (it is clamped
    /// to at least 2 where it is used).
    fn from(info: &ClientInfo) -> ClientInformation {
        ClientInformation {
            language: info.language.clone(),
            view_distance: u8::try_from(info.view_distance).unwrap_or(0),
            model_customisation: info.model_customisation,
            main_hand: match info.main_hand {
                WireMainHand::Left => MainHand::Left,
                WireMainHand::Right => MainHand::Right,
            },
            chat_visibility: info.chat_visibility,
            allows_listing: info.allows_listing,
        }
    }
}

impl Connection {
    /// A connection for a player that joins at `pos`.
    pub fn new(
        sink: Arc<dyn PacketSink>,
        inbound: Receiver<Inbound>,
        pos: Vec3,
        memory_connection: bool,
        chat: &ChatSettings,
        inbox: Arc<Mutex<ChatInbox>>,
    ) -> Connection {
        Connection {
            sink,
            inbound,
            movement: Movement::new(pos),
            chunks: PlayerChunks::new(memory_connection),
            chat: PlayerChatState::new(chat, inbox),
            attacks: Vec::new(),
        }
    }

    /// Handles at most `budget` queued packets in arrival order (SEC-16/17: the rest waits for
    /// the next tick; the network layer bounds the queue).
    pub fn handle_packets(
        &mut self,
        state: &mut PlayerState,
        ctx: MoveContext<'_>,
        chat: ChatContext<'_>,
        budget: usize,
    ) -> Handled {
        for _ in 0..budget {
            let packet = match self.inbound.try_recv() {
                Ok(inbound) => inbound.packet,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Handled::Disconnected,
            };
            if let Err(Kick(reason)) = self.handle(packet, state, ctx, chat) {
                return Handled::Kick(reason);
            }
        }
        if self.sink.is_closed() {
            Handled::Disconnected
        } else {
            Handled::Ok
        }
    }

    fn handle(
        &mut self,
        packet: ServerboundPlay,
        state: &mut PlayerState,
        ctx: MoveContext<'_>,
        chat: ChatContext<'_>,
    ) -> Result<(), Kick> {
        let sink = self.sink.as_ref();
        let mv =
            |pos: Option<(f64, f64, f64)>,
             rot,
             flags: vanillite_protocol::packets::play::serverbound::MoveFlags| {
                MoveRequest {
                    pos: pos.map(|(x, y, z)| Vec3::new(x, y, z)),
                    rot,
                    on_ground: flags.on_ground,
                    horizontal_collision: flags.horizontal_collision,
                }
            };
        match packet {
            ServerboundPlay::MovePlayerPos(p) => self.movement.handle_move(
                mv(Some((p.x, p.y, p.z)), None, p.flags),
                &mut state.body,
                ctx,
                sink,
            )?,
            ServerboundPlay::MovePlayerPosRot(p) => self.movement.handle_move(
                mv(Some((p.x, p.y, p.z)), Some((p.y_rot, p.x_rot)), p.flags),
                &mut state.body,
                ctx,
                sink,
            )?,
            ServerboundPlay::MovePlayerRot(p) => self.movement.handle_move(
                mv(None, Some((p.y_rot, p.x_rot)), p.flags),
                &mut state.body,
                ctx,
                sink,
            )?,
            ServerboundPlay::MovePlayerStatusOnly(p) => {
                self.movement
                    .handle_move(mv(None, None, p.flags), &mut state.body, ctx, sink)?;
            }
            ServerboundPlay::AcceptTeleportation(p) => {
                self.movement
                    .accept_teleport(&p, &mut state.body, ctx, sink)?;
            }
            ServerboundPlay::ClientTickEnd(_) => self.movement.client_tick_end(),
            ServerboundPlay::ClientCommand(p) => {
                // Statistics and game rule values come later.
                if p.action == ClientCommandAction::PerformRespawn {
                    state.respawn = true;
                }
            }
            ServerboundPlay::PlayerLoaded(_) => self.movement.mark_client_loaded(),
            ServerboundPlay::ChunkBatchReceived(p) => {
                self.chunks.batch_received(p.desired_chunks_per_tick);
            }
            ServerboundPlay::ClientInformation(p) => {
                let info = ClientInformation::from(&p.0);
                self.chat.inbox().visibility = info.chat_visibility;
                state.client = Some(info);
            }
            ServerboundPlay::Chat(p) => {
                let last_seen = self.apply_last_seen(&p.last_seen, chat)?;
                if self.accept_chat(&p.message, false)? {
                    self.chat.requests.push(ChatRequest::Chat {
                        message: p.message,
                        timestamp: p.timestamp,
                        salt: p.salt,
                        signature: p.signature,
                        last_seen,
                    });
                }
            }
            ServerboundPlay::ChatCommand(p) => {
                if self.accept_chat(&p.command, true)? {
                    self.chat.requests.push(ChatRequest::Command {
                        command: p.command,
                        signed: None,
                    });
                }
            }
            ServerboundPlay::ChatCommandSigned(p) => {
                let last_seen = self.apply_last_seen(&p.last_seen, chat)?;
                if self.accept_chat(&p.command, true)? {
                    self.chat.requests.push(ChatRequest::Command {
                        command: p.command,
                        signed: Some(SignedCommand {
                            timestamp: p.timestamp,
                            salt: p.salt,
                            signatures: p.argument_signatures,
                            last_seen,
                        }),
                    });
                }
            }
            ServerboundPlay::ChatAck(p) => {
                let applied = self.chat.inbox().last_seen.apply_offset(p.offset);
                if let Err(e) = applied {
                    log::error!(
                        "Failed to validate message acknowledgement offset from {}: {e}",
                        chat.name
                    );
                    return Err(chat_validation_failed());
                }
            }
            ServerboundPlay::ChatSessionUpdate(p) => self.update_chat_session(p.0, chat)?,
            ServerboundPlay::CommandSuggestion(p) => {
                self.chat.suggestion = Some((p.id, p.command));
            }
            ServerboundPlay::PlayerInput(p) => {
                if self.movement.has_client_loaded() {
                    state.shift_key_down = p.0.shift;
                }
            }
            ServerboundPlay::Attack(p) => {
                // `handleAttack`: bounded like the other per-tick packet budgets.
                if self.movement.has_client_loaded() && self.attacks.len() < 16 {
                    self.attacks.push(p.entity_id);
                }
            }
            ServerboundPlay::PlayerCommand(p) => {
                if self.movement.has_client_loaded() {
                    match p.action {
                        PlayerAction::StartSprinting => state.sprinting = true,
                        PlayerAction::StopSprinting => state.sprinting = false,
                        // Sleeping, vehicles, inventories and elytra come later.
                        PlayerAction::StopSleeping
                        | PlayerAction::StartRidingJump
                        | PlayerAction::StopRidingJump
                        | PlayerAction::OpenInventory
                        | PlayerAction::StartFallFlying => {}
                    }
                }
            }
            // The server never asks the client to reconfigure yet.
            ServerboundPlay::ConfigurationAcknowledged(_) => {
                return Err(Kick(Box::new(Component::translatable(
                    "multiplayer.disconnect.invalid_packet",
                    vec![],
                ))));
            }
            // Keep-alives and ping requests are answered by the network task; cookies, custom
            // payloads, pongs and resource packs come later.
            ServerboundPlay::CookieResponse(_)
            | ServerboundPlay::CustomClickAction(_)
            | ServerboundPlay::CustomPayload(_)
            | ServerboundPlay::KeepAlive(_)
            | ServerboundPlay::PingRequest(_)
            | ServerboundPlay::Pong(_)
            | ServerboundPlay::ResourcePack(_)
            | ServerboundPlay::Unhandled { .. } => {}
        }
        Ok(())
    }

    /// `unpackAndApplyLastSeen`: a broken acknowledgement kicks the player.
    fn apply_last_seen(
        &self,
        update: &LastSeenUpdate,
        chat: ChatContext<'_>,
    ) -> Result<Vec<MessageSignature>, Kick> {
        let applied = self.chat.inbox().last_seen.apply_update(update);
        applied.map_err(|e| {
            log::error!(
                "Failed to validate message acknowledgements from {}: {e}",
                chat.name
            );
            chat_validation_failed()
        })
    }

    /// `tryHandleChat`: illegal characters kick; chat (not commands) is refused while the
    /// player hides chat. Returns whether the request goes on.
    fn accept_chat(&self, message: &str, is_command: bool) -> Result<bool, Kick> {
        if is_chat_message_illegal(message) {
            return Err(Kick(Box::new(Component::translatable(
                "multiplayer.disconnect.illegal_characters",
                vec![],
            ))));
        }
        if !is_command && self.chat.inbox().visibility == ChatVisibility::Hidden {
            self.sink.send(&SystemChat {
                content: Component::translatable("chat.disabled.options", vec![])
                    .color(Color::Named(NamedColor::Red)),
                overlay: false,
            });
            return Ok(false);
        }
        Ok(true)
    }

    /// `handleChatSessionUpdate`.
    fn update_chat_session(
        &mut self,
        data: ChatSessionData,
        chat: ChatContext<'_>,
    ) -> Result<(), Kick> {
        let old = self.chat.chain.session().map(|s| &s.data);
        let same_key = old.is_some_and(|o| {
            o.expires_at == data.expires_at
                && o.public_key == data.public_key
                && o.key_signature == data.key_signature
        });
        if same_key {
            return Ok(());
        }
        if old.is_some_and(|o| data.expires_at < o.expires_at) {
            return Err(Kick(Box::new(Component::translatable(
                "multiplayer.disconnect.expired_public_key",
                vec![],
            ))));
        }
        if chat.settings.keys.is_empty() {
            log::warn!(
                "Ignoring chat session from {} due to missing Services public key",
                chat.name
            );
            return Ok(());
        }
        match ChatSession::validate(data, chat.uuid, &chat.settings.keys) {
            Ok(session) => {
                let data = session.data.clone();
                self.chat.chain.reset(session);
                self.chat.requests.push(ChatRequest::Session(data));
                Ok(())
            }
            Err(kick) => {
                log::error!("Failed to validate profile key of {}", chat.name);
                Err(kick)
            }
        }
    }
}

/// `multiplayer.disconnect.chat_validation_failed`.
fn chat_validation_failed() -> Kick {
    Kick(Box::new(Component::translatable(
        "multiplayer.disconnect.chat_validation_failed",
        vec![],
    )))
}

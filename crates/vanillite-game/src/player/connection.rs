//! A player's connection as the world sees it, and the play packet handlers of milestone 1
//! (Vanilla's `ServerGamePacketListenerImpl`).

use std::sync::mpsc::TryRecvError;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use vanillite_base::Vec3;
use vanillite_protocol::InboundReceiver;
use vanillite_protocol::chat::{
    ArgumentSignature, ChatSessionData, LastSeenUpdate, MessageSignature,
};
use vanillite_protocol::packets::common::ChatVisibility;
use vanillite_protocol::packets::common::{ClientInfo, MainHand as WireMainHand};
use vanillite_protocol::packets::play::clientbound::SystemChat;
use vanillite_protocol::packets::play::serverbound::{
    ClientCommandAction, PlayerAction, SeenAdvancementsAction,
};
use vanillite_protocol::sets::ServerboundPlay;
use vanillite_protocol::text::{Color, Component, NamedColor};

use super::chat::{
    ChatInbox, ChatSession, ChatSettings, MessageChain, TickThrottler, is_chat_message_illegal,
};
use super::chunks::PlayerChunks;
use super::movement::{Body, Kick, MoveContext, MoveRequest, Movement};
use super::play::{Digging, PlayContext};
use super::sink::PacketSink;
use super::{ClientInformation, MainHand};
use crate::inventory::ClickContext;
use vanillite_base::BlockPos;
use vanillite_protocol::packets::play::clientbound::SetHeldSlot;
use vanillite_protocol::packets::play::serverbound::{DigAction, Hand};

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

/// An attack or interaction of a player with an entity, by network id.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EntityAction {
    /// `attack`.
    Attack(i32),
    /// `interact`: hand, hit location relative to the entity, sneaking.
    Interact {
        entity: i32,
        hand: Hand,
        location: Vec3,
        sneaking: bool,
    },
    /// `use_item` (after the block handling): items used on air that need the entities
    /// (boats, carrot on a stick, shields, eating).
    UseItem(Hand),
    /// `use_item_on` with an item that acts on entities (spawn eggs, leads on fences).
    UseItemOn {
        hand: Hand,
        pos: vanillite_base::BlockPos,
        face: vanillite_base::Direction,
    },
    /// `player_action` `RELEASE_USE_ITEM`.
    ReleaseUseItem,
    /// `player_command` `OPEN_INVENTORY` while riding: the vehicle's inventory opens
    /// (`HasCustomInventoryScreen`: chest boats).
    OpenVehicleInventory,
}

/// What a player sends to steer its vehicle.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct VehicleInput {
    /// The last `player_input`.
    pub input: vanillite_protocol::packets::play::serverbound::Input,
    /// `paddle_boat`: left and right paddle.
    pub paddles: (bool, bool),
    /// The last `move_vehicle` since the world took it: position, yaw, pitch, on ground.
    pub moved: Option<(Vec3, f32, f32, bool)>,
    /// `player_command` `START_RIDING_JUMP` with its strength, taken by the world.
    pub jump: Option<i32>,
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
    inbound: InboundReceiver,
    /// Movement validation and teleports.
    pub movement: Movement,
    /// Chunk view and sending.
    pub chunks: PlayerChunks,
    /// Chat and commands.
    pub chat: PlayerChatState,
    /// Attacks and interactions with entities since the last tick (`attack`, `interact`), in
    /// packet order; the world resolves them with the entities.
    pub entity_actions: Vec<EntityAction>,
    /// Vehicle input: the last `player_input`, `paddle_boat` and `move_vehicle`.
    pub vehicle: VehicleInput,
    /// Digging, placing and item handling.
    pub play: Digging,
    /// A virtual connection (test bot, Bedrock player). These have no chat session, so their
    /// chat is accepted unsigned even when the server enforces secure chat.
    pub is_virtual: bool,
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
        inbound: InboundReceiver,
        pos: Vec3,
        memory_connection: bool,
        is_virtual: bool,
        chat: &ChatSettings,
        inbox: Arc<Mutex<ChatInbox>>,
    ) -> Connection {
        Connection {
            sink,
            inbound,
            movement: Movement::new(pos),
            chunks: PlayerChunks::new(memory_connection),
            chat: PlayerChatState::new(chat, inbox),
            entity_actions: Vec::new(),
            vehicle: VehicleInput::default(),
            play: Digging::new(),
            is_virtual,
        }
    }

    /// Handles at most `budget` queued packets in arrival order (SEC-16/17: the rest waits for
    /// the next tick; the network layer bounds the queue).
    pub fn handle_packets(
        &mut self,
        state: &mut PlayerState,
        ctx: MoveContext<'_>,
        chat: ChatContext<'_>,
        play: &mut PlayContext<'_>,
        budget: usize,
    ) -> Handled {
        for _ in 0..budget {
            let packet = match self.inbound.try_recv() {
                Ok(inbound) => inbound.packet,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Handled::Disconnected,
            };
            play.actor.pos = state.body.pos;
            play.actor.y_rot = state.body.y_rot;
            play.actor.x_rot = state.body.x_rot;
            play.actor.on_ground = state.body.on_ground;
            play.actor.sneaking = state.shift_key_down;
            if let Err(Kick(reason)) = self.handle(packet, state, ctx, chat, play) {
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
        play: &mut PlayContext<'_>,
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
            ServerboundPlay::ClientCommand(p) => match p.action {
                ClientCommandAction::PerformRespawn => state.respawn = true,
                // `sendStats`.
                ClientCommandAction::RequestStats => {
                    if let Some(stats) = play.stats.as_deref_mut() {
                        sink.send(&stats.award_packet());
                    }
                }
                // Game rule values come later.
                ClientCommandAction::RequestGameRuleValues => {}
            },
            ServerboundPlay::SeenAdvancements(p) => {
                // `handleSeenAdvancements`: an opened tab of a known advancement.
                if p.0.action == SeenAdvancementsAction::OpenedTab
                    && let Some(tab) = &p.0.tab
                    && let Some(advancements) = play.advancements.as_deref_mut()
                    && advancements.advancements().index(tab.as_str()).is_some()
                    && let Some(packet) = advancements.select_tab(Some(tab.as_str()))
                {
                    sink.send(&packet);
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
                    self.vehicle.input = p.0;
                }
            }
            ServerboundPlay::Attack(p) => {
                // `handleAttack`: bounded like the other per-tick packet budgets.
                if self.movement.has_client_loaded() && self.entity_actions.len() < 16 {
                    self.entity_actions.push(EntityAction::Attack(p.entity_id));
                }
            }
            ServerboundPlay::Interact(p) => {
                // `handleInteract`: sets the sneak state, then interacts.
                if self.movement.has_client_loaded() {
                    state.shift_key_down = p.using_secondary_action;
                    let l = p.location.0;
                    if self.entity_actions.len() < 16
                        && l.x.is_finite()
                        && l.y.is_finite()
                        && l.z.is_finite()
                    {
                        self.entity_actions.push(EntityAction::Interact {
                            entity: p.entity_id,
                            hand: p.hand,
                            location: Vec3::new(l.x, l.y, l.z),
                            sneaking: p.using_secondary_action,
                        });
                    }
                }
            }
            ServerboundPlay::MoveVehicle(p) => {
                if self.movement.has_client_loaded() {
                    self.vehicle.moved =
                        Some((Vec3::new(p.x, p.y, p.z), p.y_rot, p.x_rot, p.on_ground));
                }
            }
            ServerboundPlay::PaddleBoat(p) => self.vehicle.paddles = (p.left, p.right),
            ServerboundPlay::PlayerCommand(p) => {
                if self.movement.has_client_loaded() {
                    match p.action {
                        PlayerAction::StartSprinting => state.sprinting = true,
                        PlayerAction::StopSprinting => state.sprinting = false,
                        // Sleeping, vehicles, inventories and elytra come later.
                        PlayerAction::StartRidingJump => {
                            self.vehicle.jump = Some(p.data);
                        }
                        PlayerAction::OpenInventory => {
                            if self.entity_actions.len() < 16 {
                                self.entity_actions.push(EntityAction::OpenVehicleInventory);
                            }
                        }
                        PlayerAction::StopSleeping
                        | PlayerAction::StopRidingJump
                        | PlayerAction::StartFallFlying => {}
                    }
                }
            }
            ServerboundPlay::PlayerAction(p) => {
                if self.movement.has_client_loaded() {
                    self.handle_player_action(p, play);
                }
            }
            ServerboundPlay::UseItemOn(p) => {
                if self.movement.has_client_loaded() {
                    let stack = match p.hand {
                        Hand::MainHand => play.inventory.selected_item(),
                        Hand::OffHand => play
                            .inventory
                            .equipment(crate::item::EquipmentSlot::OffHand),
                    };
                    let name = crate::item::item_id(stack).name();
                    let entity_item = name.ends_with("_spawn_egg")
                        || name == "minecraft:lead"
                        || name.ends_with("minecart");
                    self.play.use_item_on(play, &p, self.sink.as_ref());
                    if entity_item && self.entity_actions.len() < 16 {
                        self.entity_actions.push(EntityAction::UseItemOn {
                            hand: p.hand,
                            pos: vanillite_base::BlockPos::new(p.pos.x, p.pos.y, p.pos.z),
                            face: crate::player::play::direction_wrapped(p.face),
                        });
                    }
                }
            }
            ServerboundPlay::UseItem(p) => {
                if self.movement.has_client_loaded() {
                    self.play.use_item(p.sequence);
                    if self.entity_actions.len() < 16 {
                        self.entity_actions.push(EntityAction::UseItem(p.hand));
                    }
                }
            }
            ServerboundPlay::Punch(_) => play.swings.push(Hand::MainHand),
            ServerboundPlay::SetCarriedItem(p) => match usize::try_from(p.slot) {
                Ok(slot) if slot < crate::inventory::HOTBAR => play.inventory.selected = slot,
                _ => log::warn!("{} tried to set an invalid carried item", chat.name),
            },
            ServerboundPlay::SetCreativeModeSlot(p) => {
                self.play.set_creative_slot(play, p.slot, p.item);
                play.inventory.broadcast_changes(self.sink.as_ref());
            }
            ServerboundPlay::ContainerClick(p) => self.container_click(p, play),
            ServerboundPlay::PlaceRecipe(p) => self.place_recipe(&p, play),
            ServerboundPlay::RecipeBookSeenRecipe(p) => {
                if let Some(book) = play.recipe_book.as_deref_mut() {
                    book.remove_highlight(&play.data.recipes.book, p.recipe);
                }
            }
            ServerboundPlay::RecipeBookChangeSettings(p) => {
                if let Some(book) = play.recipe_book.as_deref_mut() {
                    book.set_settings(p.book_type, p.open, p.filtering);
                }
            }
            ServerboundPlay::ContainerClose(_) => {
                // `doCloseContainer`: whatever menu is open (Vanilla does not check the id).
                let infinite = play.actor.game_mode == vanillite_config::GameMode::Creative;
                if play.inventory.container.is_some() {
                    let who = crate::menu::container::Opener {
                        spectator: play.actor.game_mode == vanillite_config::GameMode::Spectator,
                        infinite,
                        disconnected: false,
                    };
                    if let Some(level) = play.level.level_mut() {
                        crate::menu::container::close(play.inventory, level, who, play.drops, None);
                    }
                } else {
                    play.inventory.close(infinite, play.drops);
                }
            }
            ServerboundPlay::PickItemFromBlock(p) => self.pick_item_from_block(p.pos, play),
            ServerboundPlay::ContainerButtonClick(p) => {
                self.container_button(p.container_id, p.button_id, play)
            }
            ServerboundPlay::RenameItem(p) => self.rename_item(&p.name, play),
            ServerboundPlay::ContainerSlotStateChanged(p) => {
                self.container_slot_state(p.slot_id, p.container_id, p.new_state, play);
            }
            ServerboundPlay::SignUpdate(p) => {
                // `handleSignUpdate`: the editing player sets one side of a loaded sign.
                let pos = BlockPos::new(p.pos.x, p.pos.y, p.pos.z);
                let uuid = play.actor.uuid;
                if let Some(level) = play.level.level_mut()
                    && level.is_loaded(pos)
                {
                    crate::block::sign::update_text(level, pos, uuid, p.front, &p.lines);
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

    /// `handlePlayerAction`.
    fn handle_player_action(
        &mut self,
        p: vanillite_protocol::packets::play::serverbound::PlayerActionPacket,
        play: &mut PlayContext<'_>,
    ) {
        let spectator = play.actor.game_mode == vanillite_config::GameMode::Spectator;
        match p.action {
            DigAction::SwapItemWithOffhand => {
                if !spectator {
                    play.inventory.swap_hands();
                }
            }
            DigAction::DropItem | DigAction::DropAllItems => {
                self.play
                    .drop_selected(play, p.action == DigAction::DropAllItems);
                play.swings.push(Hand::MainHand);
            }
            DigAction::ReleaseUseItem => {
                if self.entity_actions.len() < 16 {
                    self.entity_actions.push(EntityAction::ReleaseUseItem);
                }
            }
            DigAction::Stab => {}
            DigAction::StartDestroyBlock
            | DigAction::ChangeDestroyDirection
            | DigAction::AbortDestroyBlock
            | DigAction::StopDestroyBlock => {
                let pos = BlockPos::new(p.pos.x, p.pos.y, p.pos.z);
                self.play.handle_action(
                    play,
                    p.action,
                    pos,
                    p.direction,
                    p.sequence,
                    self.sink.as_ref(),
                );
            }
        }
    }

    /// `handleContainerClick` for the open container menu.
    fn container_menu_click(
        &mut self,
        p: vanillite_protocol::packets::play::serverbound::ContainerClick,
        play: &mut PlayContext<'_>,
    ) {
        let sink = self.sink.as_ref();
        let actor = play.actor;
        let PlayContext {
            level,
            inventory,
            drops,
            xp,
            ..
        } = play;
        let Some(level) = level.level_mut() else {
            return;
        };
        if actor.game_mode == vanillite_config::GameMode::Spectator {
            crate::menu::container::with_view(inventory, level, actor.pos, None, |view| {
                crate::menu::send_all(view, sink);
            });
            return;
        }
        let in_range = |pos| actor.within_block_range(pos, 4.0);
        if !crate::menu::container::still_valid(inventory, level, &in_range, &|b| {
            actor.within_entity_range(b, 4.0)
        }) {
            log::debug!("A player interacted with an invalid menu");
            return;
        }
        let ctx = ClickContext {
            infinite: actor.game_mode == vanillite_config::GameMode::Creative,
        };
        crate::menu::container::with_view(inventory, level, actor.pos, xp.as_deref_mut(), |view| {
            if !crate::menu::is_valid_slot_index(view, p.slot) {
                log::debug!("A player clicked invalid slot index {}", p.slot);
                return;
            }
            let full_resync = p.state_id != view.menu.state.state_id();
            view.menu.state.suppress_remote = true;
            crate::menu::click(view, p.slot, p.button, p.input, ctx, drops);
            view.flush_result_updates(sink);
            crate::menu::receive_predictions(&mut view.menu.state, &p.changed, &p.carried);
            view.menu.state.suppress_remote = false;
            if full_resync {
                crate::menu::broadcast_full_state(view, sink);
            } else {
                crate::menu::broadcast_changes(view, sink);
            }
        });
    }

    /// `handlePlaceRecipe`: a known recipe from the recipe book into the open crafting grid or
    /// furnace; a ghost recipe when items are missing.
    fn place_recipe(
        &mut self,
        p: &vanillite_protocol::packets::play::serverbound::PlaceRecipe,
        play: &mut PlayContext<'_>,
    ) {
        use crate::menu::container::MenuKind;
        use crate::recipe::book::RecipeRef;
        use crate::recipe::place::{self, Grid};
        let sink = self.sink.as_ref();
        let actor = play.actor;
        if actor.game_mode == vanillite_config::GameMode::Spectator {
            return;
        }
        let recipes = &play.data.recipes;
        let Some(entry) = recipes.book.entry(p.recipe) else {
            return;
        };
        if !play
            .recipe_book
            .as_deref()
            .is_some_and(|b| b.contains(&entry.id))
        {
            return;
        }
        let Some(placement) = recipes.placement(entry.recipe) else {
            log::debug!("A player tried to place impossible recipe {}", entry.id);
            return;
        };
        let creative = actor.game_mode == vanillite_config::GameMode::Creative;
        let open_id = play
            .inventory
            .container
            .as_ref()
            .map_or(0, |m| m.state.container_id);
        if open_id != p.container_id {
            return;
        }
        let grid_of = |w: usize, h: usize, first: usize| {
            let input: Vec<usize> = (first..first.saturating_add(w.saturating_mul(h))).collect();
            let mut content = vec![0];
            content.extend_from_slice(&input);
            Grid {
                width: w,
                height: h,
                clear: input.clone(),
                account: input.clone(),
                input,
                clear_content: content,
                account_full: false,
                suppress: false,
                content_direct: true,
            }
        };
        let ghost = if play.inventory.container.is_none() {
            let RecipeRef::Crafting(index) = entry.recipe else {
                return;
            };
            let input = crate::recipe::CraftingInput::of(2, 2, &play.inventory.craft);
            let matches = recipes.crafting_matches(index, &input);
            let grid = grid_of(2, 2, 1);
            let ghost = place::place(
                &mut *play.inventory,
                &grid,
                &placement,
                matches,
                p.use_max_items,
                creative,
                play.drops,
            );
            play.inventory.flush_result_updates(sink);
            ghost
        } else {
            let PlayContext {
                level,
                inventory,
                drops,
                xp,
                ..
            } = play;
            let Some(level) = level.level_mut() else {
                return;
            };
            let in_range = |pos| actor.within_block_range(pos, 4.0);
            if !crate::menu::container::still_valid(inventory, level, &in_range, &|b| {
                actor.within_entity_range(b, 4.0)
            }) {
                log::debug!("A player interacted with an invalid menu");
                return;
            }
            crate::menu::container::with_view(
                inventory,
                level,
                actor.pos,
                xp.as_deref_mut(),
                |view| {
                    let (grid, matches) = match (view.menu.kind, entry.recipe) {
                        (MenuKind::Crafting, RecipeRef::Crafting(index)) => {
                            let cells = view.menu.local.get(1..10).unwrap_or_default();
                            let input = crate::recipe::CraftingInput::of(3, 3, cells);
                            let mut grid = grid_of(3, 3, 1);
                            grid.suppress = true;
                            (grid, recipes.crafting_matches(index, &input))
                        }
                        (MenuKind::Furnace(_), RecipeRef::Cooking(index)) => {
                            let matches = recipes.cooking.get(index).is_some_and(|r| {
                                r.ingredient.test(crate::menu::MenuSlots::item(view, 0))
                            });
                            let grid = Grid {
                                width: 1,
                                height: 1,
                                input: vec![0],
                                clear: vec![0, 2],
                                clear_content: vec![0, 2],
                                account: vec![0, 1, 2],
                                account_full: true,
                                suppress: false,
                                content_direct: false,
                            };
                            (grid, matches)
                        }
                        _ => return false,
                    };
                    let ghost = place::place(
                        view,
                        &grid,
                        &placement,
                        matches,
                        p.use_max_items,
                        creative,
                        drops,
                    );
                    view.flush_result_updates(sink);
                    ghost
                },
            )
            .unwrap_or(false)
        };
        if ghost && let Some(display) = recipes.book.display(p.recipe) {
            sink.send(
                &vanillite_protocol::packets::play::clientbound::PlaceGhostRecipe {
                    container_id: p.container_id,
                    display,
                },
            );
        }
    }

    /// `handleContainerButtonClick`.
    fn container_button(&mut self, container_id: i32, button: i32, play: &mut PlayContext<'_>) {
        let sink = self.sink.as_ref();
        let actor = play.actor;
        if actor.game_mode == vanillite_config::GameMode::Spectator
            || play
                .inventory
                .container
                .as_ref()
                .is_none_or(|m| m.state.container_id != container_id)
        {
            return;
        }
        let PlayContext {
            level,
            inventory,
            xp,
            drops,
            ..
        } = play;
        let Some(level) = level.level_mut() else {
            return;
        };
        let in_range = |pos| actor.within_block_range(pos, 4.0);
        if !crate::menu::container::still_valid(inventory, level, &in_range, &|b| {
            actor.within_entity_range(b, 4.0)
        }) {
            log::debug!("A player interacted with an invalid menu");
            return;
        }
        let may_build = actor.game_mode != vanillite_config::GameMode::Adventure;
        crate::menu::container::with_view(inventory, level, actor.pos, xp.as_deref_mut(), |view| {
            view.menu.may_build = may_build;
            if view.button(button, drops) {
                crate::menu::broadcast_changes(view, sink);
            }
        });
    }

    /// `handleContainerSlotStateChanged`: a crafter slot is enabled or disabled.
    fn container_slot_state(
        &mut self,
        slot: i32,
        container_id: i32,
        enabled: bool,
        play: &mut PlayContext<'_>,
    ) {
        let actor = play.actor;
        if actor.game_mode == vanillite_config::GameMode::Spectator
            || play
                .inventory
                .container
                .as_ref()
                .is_none_or(|m| m.state.container_id != container_id)
        {
            return;
        }
        let PlayContext {
            level, inventory, ..
        } = play;
        let Some(level) = level.level_mut() else {
            return;
        };
        crate::menu::container::with_view(inventory, level, actor.pos, None, |view| {
            view.crafter_slot_state(slot, enabled);
        });
    }

    /// `handleRenameItem`: the anvil's name field.
    fn rename_item(&mut self, name: &str, play: &mut PlayContext<'_>) {
        let actor = play.actor;
        let PlayContext {
            level,
            inventory,
            xp,
            ..
        } = play;
        let Some(level) = level.level_mut() else {
            return;
        };
        if inventory
            .container
            .as_ref()
            .is_none_or(|m| m.kind != crate::menu::container::MenuKind::Anvil)
        {
            return;
        }
        let in_range = |pos| actor.within_block_range(pos, 4.0);
        if !crate::menu::container::still_valid(inventory, level, &in_range, &|b| {
            actor.within_entity_range(b, 4.0)
        }) {
            log::debug!("A player interacted with an invalid menu");
            return;
        }
        crate::menu::container::with_view(inventory, level, actor.pos, xp.as_deref_mut(), |view| {
            view.menu.state.creative = actor.game_mode == vanillite_config::GameMode::Creative;
            view.rename(name);
        });
    }

    /// `handleContainerClick`: clicks go to the open menu with that id.
    fn container_click(
        &mut self,
        p: vanillite_protocol::packets::play::serverbound::ContainerClick,
        play: &mut PlayContext<'_>,
    ) {
        if let Some(open) = &play.inventory.container {
            if p.container_id == open.state.container_id {
                self.container_menu_click(p, play);
            }
            return;
        }
        if p.container_id != 0 {
            return;
        }
        let sink = self.sink.as_ref();
        if play.actor.game_mode == vanillite_config::GameMode::Spectator {
            play.inventory.send_all(sink);
            return;
        }
        // `isValidSlotIndex`: -1, -999 or a slot of the menu.
        let valid = p.slot == -1
            || p.slot == -999
            || usize::try_from(p.slot).is_ok_and(|s| s < crate::inventory::MENU_SLOTS);
        if !valid {
            log::debug!("A player clicked invalid slot index {}", p.slot);
            return;
        }
        let full_resync = p.state_id != play.inventory.state_id();
        let ctx = ClickContext {
            infinite: play.actor.game_mode == vanillite_config::GameMode::Creative,
        };
        play.inventory.menu.suppress_remote = true;
        play.inventory
            .click(p.slot, p.button, p.input, ctx, play.drops);
        play.inventory.flush_result_updates(sink);
        play.inventory.receive_predictions(&p.changed, &p.carried);
        play.inventory.menu.suppress_remote = false;
        if full_resync {
            play.inventory.broadcast_full_state(sink);
        } else {
            play.inventory.broadcast_changes(sink);
        }
    }

    /// `handlePickItemFromBlock` (middle click).
    fn pick_item_from_block(
        &mut self,
        pos: vanillite_protocol::types::BlockPos,
        play: &mut PlayContext<'_>,
    ) {
        let pos = BlockPos::new(pos.x, pos.y, pos.z);
        if !play.actor.within_block_range(pos, 1.0) || !play.level.is_loaded(pos) {
            return;
        }
        let state = play.level.block(pos);
        let item = state.block().info().item;
        if item == u16::MAX || item == 0 {
            return;
        }
        let stack = crate::item::ItemStack::new(item, 1);
        let infinite = play.actor.game_mode == vanillite_config::GameMode::Creative;
        play.inventory.pick_item(stack, infinite);
        let slot = i32::try_from(play.inventory.selected).unwrap_or(0);
        self.sink.send(&SetHeldSlot { slot });
        play.inventory.broadcast_changes(self.sink.as_ref());
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

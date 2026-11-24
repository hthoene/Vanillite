//! A player's connection as the world sees it, and the play packet handlers of milestone 1
//! (Vanilla's `ServerGamePacketListenerImpl`).

use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError};

use vanillite_base::Vec3;
use vanillite_protocol::Inbound;
use vanillite_protocol::packets::common::{ClientInfo, MainHand as WireMainHand};
use vanillite_protocol::packets::play::serverbound::PlayerAction;
use vanillite_protocol::sets::ServerboundPlay;
use vanillite_protocol::text::Component;

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

/// A player's connection: packet queues and per-connection protocol state.
pub struct Connection {
    /// Clientbound packets.
    pub sink: Arc<dyn PacketSink>,
    inbound: Receiver<Inbound>,
    /// Movement validation and teleports.
    pub movement: Movement,
    /// Chunk view and sending.
    pub chunks: PlayerChunks,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("movement", &self.movement)
            .field("chunks", &self.chunks)
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
    ) -> Connection {
        Connection {
            sink,
            inbound,
            movement: Movement::new(pos),
            chunks: PlayerChunks::new(memory_connection),
        }
    }

    /// Handles at most `budget` queued packets in arrival order (SEC-16/17: the rest waits for
    /// the next tick; the network layer bounds the queue).
    pub fn handle_packets(
        &mut self,
        state: &mut PlayerState,
        ctx: MoveContext<'_>,
        budget: usize,
    ) -> Handled {
        for _ in 0..budget {
            let packet = match self.inbound.try_recv() {
                Ok(inbound) => inbound.packet,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Handled::Disconnected,
            };
            if let Err(Kick(reason)) = self.handle(packet, state, ctx) {
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
            ServerboundPlay::PlayerLoaded(_) => self.movement.mark_client_loaded(),
            ServerboundPlay::ChunkBatchReceived(p) => {
                self.chunks.batch_received(p.desired_chunks_per_tick);
            }
            ServerboundPlay::ClientInformation(p) => {
                state.client = Some(ClientInformation::from(&p.0));
            }
            ServerboundPlay::PlayerInput(p) => {
                if self.movement.has_client_loaded() {
                    state.shift_key_down = p.0.shift;
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
            // Commands, cookies, custom payloads, pongs and resource packs come later.
            ServerboundPlay::ChatCommand(_)
            | ServerboundPlay::CookieResponse(_)
            | ServerboundPlay::CustomClickAction(_)
            | ServerboundPlay::CustomPayload(_)
            | ServerboundPlay::KeepAlive(_)
            | ServerboundPlay::Pong(_)
            | ServerboundPlay::ResourcePack(_)
            | ServerboundPlay::Unhandled { .. } => {}
        }
        Ok(())
    }
}

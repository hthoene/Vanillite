//! Player movement from the client, validated like Vanilla's `ServerGamePacketListenerImpl`
//! (`handleMovePlayer`, `handleAcceptTeleportPacket`, `teleport`, `tickPlayer`).
//!
//! Not yet ported, because they need block collision (milestone 3): the "moved wrongly" check
//! (`player.move` with collisions and `isEntityCollidingWithAnythingNew`) and the kick for
//! floating too long. Their rules (`movement.wrong-movement-check`, `movement.flying-kick`) are
//! declared already. Until then the client's position is accepted when it passes the other
//! checks.

use vanillite_base::Vec3;
use vanillite_config::MovementRules;
use vanillite_protocol::packets::play::clientbound::{PlayerPosition, PositionMoveRotation};
use vanillite_protocol::packets::play::serverbound::AcceptTeleportation;
use vanillite_protocol::text::Component;
use vanillite_protocol::types;

use super::sink::PacketSink;

/// Ticks after which an unconfirmed teleport is sent again.
const TELEPORT_RESEND_TICKS: i32 = 20;
/// Ticks the server waits for `player_loaded` after joining before treating the client as loaded.
const CLIENT_LOADED_TIMEOUT: i32 = 60;

/// The movement-related state of the player's entity as the handlers see it. The caller copies
/// it from the entity store before handling packets and writes changed values back.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Body {
    /// Position.
    pub pos: Vec3,
    /// Velocity (`getDeltaMovement`).
    pub velocity: Vec3,
    /// Yaw in degrees.
    pub y_rot: f32,
    /// Pitch in degrees, -90..=90.
    pub x_rot: f32,
    /// On ground as reported by the client.
    pub on_ground: bool,
    /// Horizontal collision as reported by the client.
    pub horizontal_collision: bool,
    /// Gliding with an elytra.
    pub fall_flying: bool,
}

/// The connection must be closed with this reason.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Kick(pub Box<Component>);

impl Kick {
    /// Vanilla's `multiplayer.disconnect.invalid_player_movement`.
    pub fn invalid_movement() -> Kick {
        Kick(Box::new(Component::translatable(
            "multiplayer.disconnect.invalid_player_movement",
            vec![],
        )))
    }
}

/// One `move_player_*` packet: position and rotation are present depending on its kind.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MoveRequest {
    /// New position (`move_player_pos`, `move_player_pos_rot`).
    pub pos: Option<Vec3>,
    /// New yaw and pitch (`move_player_rot`, `move_player_pos_rot`).
    pub rot: Option<(f32, f32)>,
    /// On ground.
    pub on_ground: bool,
    /// Horizontal collision.
    pub horizontal_collision: bool,
}

/// What the movement checks need from the world.
#[derive(Clone, Copy, Debug)]
pub struct MoveContext<'a> {
    /// The tick rate manager runs normally (not frozen).
    pub runs_normally: bool,
    /// The world's movement rules.
    pub rules: &'a MovementRules,
}

/// Per-connection movement state.
#[derive(Clone, Debug)]
pub struct Movement {
    first_good: Vec3,
    last_good: Vec3,
    received_move_packets: i32,
    known_move_packets: i32,
    received_position_this_tick: bool,
    tick_count: i32,
    awaiting_teleport: i32,
    awaiting_position: Option<Vec3>,
    awaiting_teleport_time: i32,
    client_loaded_timeout: i32,
    waiting_for_respawn: bool,
    /// Moves accepted since the caller took them last (for fall damage and exhaustion).
    pub moves: Vec<crate::survival::player::MoveRecord>,
    /// The player rides a vehicle (`isPassenger`): moves only turn it, the vehicle places it.
    pub passenger: bool,
    /// `lastKnownClientMovement`: the last move of this client tick, zero after a client tick
    /// without one (`getKnownMovement`).
    pub known_movement: Vec3,
    received_movement_this_tick: bool,
}

/// `Mth.wrapDegrees`.
pub fn wrap_degrees(angle: f32) -> f32 {
    let mut a = angle % 360.0;
    if a >= 180.0 {
        a -= 360.0;
    }
    if a < -180.0 {
        a += 360.0;
    }
    a
}

/// `Entity.absSnapRotationTo` (with `setXRot`'s clamp).
fn snap_rotation(body: &mut Body, y_rot: f32, x_rot: f32) {
    if y_rot.is_finite() {
        body.y_rot = y_rot % 360.0;
    }
    if x_rot.is_finite() {
        body.x_rot = (x_rot.clamp(-90.0, 90.0) % 360.0).clamp(-90.0, 90.0);
    }
}

fn contains_invalid_values(pos: Vec3, y_rot: f32, x_rot: f32) -> bool {
    pos.x.is_nan() || pos.y.is_nan() || pos.z.is_nan() || !y_rot.is_finite() || !x_rot.is_finite()
}

fn clamp_horizontal(v: f64) -> f64 {
    v.clamp(-3.0e7, 3.0e7)
}

fn clamp_vertical(v: f64) -> f64 {
    v.clamp(-2.0e7, 2.0e7)
}

impl Movement {
    /// State for a player that just joined at `pos`.
    pub fn new(pos: Vec3) -> Movement {
        Movement {
            first_good: pos,
            last_good: pos,
            received_move_packets: 0,
            known_move_packets: 0,
            received_position_this_tick: false,
            tick_count: 0,
            awaiting_teleport: 0,
            awaiting_position: None,
            awaiting_teleport_time: 0,
            client_loaded_timeout: CLIENT_LOADED_TIMEOUT,
            waiting_for_respawn: false,
            moves: Vec::new(),
            passenger: false,
            known_movement: Vec3::ZERO,
            received_movement_this_tick: false,
        }
    }

    /// `markClientUnloadedAfterDeath`: the client loads the world again after respawning.
    pub fn mark_client_unloaded(&mut self) {
        self.waiting_for_respawn = true;
    }

    /// `restartClientLoadTimerAfterRespawn`.
    pub fn restart_client_load_timer(&mut self) {
        self.waiting_for_respawn = false;
        self.client_loaded_timeout = CLIENT_LOADED_TIMEOUT;
    }

    /// `hasClientLoaded`: `player_loaded` arrived or the timeout ran out.
    pub fn has_client_loaded(&self) -> bool {
        !self.waiting_for_respawn && self.client_loaded_timeout <= 0
    }

    /// `player_loaded`.
    pub fn mark_client_loaded(&mut self) {
        self.client_loaded_timeout = 0;
    }

    /// `tickClientLoadTimeout`, once per player tick.
    pub fn tick_client_load_timeout(&mut self) {
        if self.client_loaded_timeout > 0 {
            self.client_loaded_timeout = self.client_loaded_timeout.saturating_sub(1);
        }
    }

    /// `resetPosition`: the movement checks start from `pos` (after a server-side teleport).
    pub fn reset_position(&mut self, pos: Vec3) {
        self.first_good = pos;
        self.last_good = pos;
    }

    /// The movement part of `tickPlayer`, once per tick before the player's packets of the
    /// next tick are handled.
    pub fn tick(&mut self, body: &Body) {
        self.reset_position(body.pos);
        self.tick_count = self.tick_count.wrapping_add(1);
        self.known_move_packets = self.received_move_packets;
        // TODO(milestone 3): the "floating too long" kick (`movement.flying-kick`) needs
        // `noBlocksAround`, i.e. block collision.
    }

    /// `client_tick_end`.
    pub fn client_tick_end(&mut self) {
        if !self.received_movement_this_tick {
            self.known_movement = Vec3::ZERO;
        }
        self.received_movement_this_tick = false;
        self.received_position_this_tick = false;
    }

    /// `ServerGamePacketListenerImpl.teleport` to an absolute position and rotation.
    pub fn teleport(
        &mut self,
        body: &mut Body,
        pos: Vec3,
        y_rot: f32,
        x_rot: f32,
        sink: &dyn PacketSink,
    ) {
        self.teleport_relative(body, pos, y_rot, x_rot, 0, sink);
    }

    /// `ServerGamePacketListenerImpl.teleport` with [`relative`] bits: each flagged
    /// coordinate or angle of `pos`, `y_rot` and `x_rot` is an offset from the current value.
    /// The client gets the values as they are; the server applies them
    /// (`PositionMoveRotation.calculateAbsolute`). The velocity is cleared except for the
    /// relative delta components.
    ///
    /// [`relative`]: vanillite_protocol::packets::play::clientbound::relative
    pub fn teleport_relative(
        &mut self,
        body: &mut Body,
        pos: Vec3,
        y_rot: f32,
        x_rot: f32,
        relatives: i32,
        sink: &dyn PacketSink,
    ) {
        use vanillite_protocol::packets::play::clientbound::relative;
        let rel = |flag: i32, current: f64, value: f64| {
            if relatives & flag != 0 {
                current + value
            } else {
                value
            }
        };
        let rel_f = |flag: i32, current: f32, value: f32| {
            if relatives & flag != 0 {
                current + value
            } else {
                value
            }
        };
        self.awaiting_teleport_time = self.tick_count;
        self.awaiting_teleport = self.awaiting_teleport.wrapping_add(1);
        if self.awaiting_teleport == i32::MAX {
            self.awaiting_teleport = 0;
        }
        let current = body.pos;
        body.pos = Vec3::new(
            rel(relative::X, current.x, pos.x),
            rel(relative::Y, current.y, pos.y),
            rel(relative::Z, current.z, pos.z),
        );
        body.velocity = Vec3::new(
            rel(relative::DELTA_X, body.velocity.x, 0.0),
            rel(relative::DELTA_Y, body.velocity.y, 0.0),
            rel(relative::DELTA_Z, body.velocity.z, 0.0),
        );
        let (abs_y, abs_x) = (
            rel_f(relative::Y_ROT, body.y_rot, y_rot),
            rel_f(relative::X_ROT, body.x_rot, x_rot),
        );
        snap_rotation(body, abs_y, abs_x);
        self.awaiting_position = Some(body.pos);
        sink.send(&PlayerPosition {
            teleport_id: self.awaiting_teleport,
            change: PositionMoveRotation {
                position: types::Vec3 {
                    x: pos.x,
                    y: pos.y,
                    z: pos.z,
                },
                delta: types::Vec3::default(),
                y_rot,
                x_rot,
            },
            relatives,
        });
    }

    /// `updateAwaitingTeleport`: `true` while a teleport is unconfirmed (it is sent again
    /// after 20 ticks).
    fn update_awaiting_teleport(&mut self, body: &mut Body, sink: &dyn PacketSink) -> bool {
        match self.awaiting_position {
            Some(pos) => {
                if self.tick_count.wrapping_sub(self.awaiting_teleport_time) > TELEPORT_RESEND_TICKS
                {
                    self.awaiting_teleport_time = self.tick_count;
                    let (y_rot, x_rot) = (body.y_rot, body.x_rot);
                    self.teleport(body, pos, y_rot, x_rot, sink);
                }
                true
            }
            None => {
                self.awaiting_teleport_time = self.tick_count;
                false
            }
        }
    }

    /// `handleAcceptTeleportPacket`.
    pub fn accept_teleport(
        &mut self,
        packet: &AcceptTeleportation,
        body: &mut Body,
        ctx: MoveContext<'_>,
        sink: &dyn PacketSink,
    ) -> Result<(), Kick> {
        if packet.id != self.awaiting_teleport {
            return Ok(());
        }
        let reached = Vec3::new(packet.x, packet.y, packet.z);
        let Some(target) = self.awaiting_position else {
            return Err(Kick::invalid_movement());
        };
        if contains_invalid_values(reached, packet.y_rot, packet.x_rot) {
            return Err(Kick::invalid_movement());
        }
        body.pos = Vec3::new(
            clamp_horizontal(target.x),
            target.y,
            clamp_horizontal(target.z),
        );
        self.last_good = target;
        self.awaiting_position = None;
        self.position_change(
            reached,
            packet.y_rot,
            packet.x_rot,
            false,
            false,
            body,
            ctx,
            sink,
        );
        Ok(())
    }

    /// `handleMovePlayer`. Errors close the connection.
    pub fn handle_move(
        &mut self,
        request: MoveRequest,
        body: &mut Body,
        ctx: MoveContext<'_>,
        sink: &dyn PacketSink,
    ) -> Result<(), Kick> {
        let pos = request.pos.unwrap_or(Vec3::ZERO);
        let (req_y, req_x) = request.rot.unwrap_or((0.0, 0.0));
        if contains_invalid_values(pos, req_y, req_x) {
            return Err(Kick::invalid_movement());
        }
        if request.pos.is_some() {
            if self.received_position_this_tick {
                return Err(Kick::invalid_movement());
            }
            self.received_position_this_tick = true;
        }
        if self.tick_count == 0 {
            self.reset_position(body.pos);
        }
        if !self.has_client_loaded() {
            return Ok(());
        }
        let (y_rot, x_rot) = request.rot.unwrap_or((body.y_rot, body.x_rot));
        if self.update_awaiting_teleport(body, sink) || self.passenger {
            snap_rotation(body, wrap_degrees(y_rot), wrap_degrees(x_rot));
        } else {
            self.position_change(
                request.pos.unwrap_or(body.pos),
                y_rot,
                x_rot,
                request.on_ground,
                request.horizontal_collision,
                body,
                ctx,
                sink,
            );
        }
        Ok(())
    }

    fn should_check_movement(ctx: MoveContext<'_>, fall_flying: bool) -> bool {
        ctx.rules.too_fast_check && (!fall_flying || ctx.rules.elytra_too_fast_check)
    }

    /// `handlePlayerPositionChange` without vehicles, sleeping and block collision.
    #[allow(clippy::too_many_arguments)]
    fn position_change(
        &mut self,
        requested: Vec3,
        requested_y_rot: f32,
        requested_x_rot: f32,
        on_ground: bool,
        horizontal_collision: bool,
        body: &mut Body,
        ctx: MoveContext<'_>,
        sink: &dyn PacketSink,
    ) {
        let target = Vec3::new(
            clamp_horizontal(requested.x),
            clamp_vertical(requested.y),
            clamp_horizontal(requested.z),
        );
        let y_rot = wrap_degrees(requested_y_rot);
        let x_rot = wrap_degrees(requested_x_rot);
        let moved = target.distance_to_sqr(self.first_good);
        let expected = body.velocity.length_sqr();
        if ctx.runs_normally {
            self.received_move_packets = self.received_move_packets.wrapping_add(1);
            let mut delta_packets = self
                .received_move_packets
                .wrapping_sub(self.known_move_packets);
            if delta_packets > 5 {
                log::debug!(
                    "Player is sending move packets too frequently ({delta_packets} packets since last tick)"
                );
                delta_packets = 1;
            }
            if Self::should_check_movement(ctx, body.fall_flying) {
                let meters_per_tick: f32 = if body.fall_flying { 300.0 } else { 100.0 };
                let limit = f64::from(
                    meters_per_tick * f32::from(i16::try_from(delta_packets).unwrap_or(1)),
                );
                if moved - expected > limit {
                    let d = Vec3::new(
                        target.x - self.first_good.x,
                        target.y - self.first_good.y,
                        target.z - self.first_good.z,
                    );
                    log::warn!("Player moved too quickly! {},{},{}", d.x, d.y, d.z);
                    let (pos, y, x) = (body.pos, body.y_rot, body.x_rot);
                    self.teleport(body, pos, y, x, sink);
                    return;
                }
            }
        }
        let from = body.pos;
        let was_on_ground = body.on_ground;
        let moved_upwards = target.y - self.last_good.y > 0.0;
        // TODO(milestone 3): `player.move` with block collision and the "moved wrongly" check
        // (`movement.wrong-movement-check`); without collision the move lands on the target.
        body.pos = Vec3::new(
            clamp_horizontal(target.x),
            target.y,
            clamp_horizontal(target.z),
        );
        snap_rotation(body, y_rot, x_rot);
        body.on_ground = on_ground;
        body.horizontal_collision = horizontal_collision;
        self.last_good = body.pos;
        // `handlePlayerKnownMovement`.
        self.known_movement = Vec3::new(
            body.pos.x - from.x,
            body.pos.y - from.y,
            body.pos.z - from.z,
        );
        self.received_movement_this_tick = true;
        self.moves.push(crate::survival::player::MoveRecord {
            from,
            to: body.pos,
            was_on_ground,
            on_ground,
            moved_upwards,
        });
    }
}

/// `ServerPlayer.teleport` within its world (`teleportSameDimension`, `connection.teleport`):
/// the player goes to `pos` with the rotation and the [`relative`] bits of the transition, and
/// its client follows. Returns the new position.
///
/// [`relative`]: vanillite_protocol::packets::play::clientbound::relative
pub fn teleport_within_world(
    ents: &mut crate::entities::Entities,
    id: vanillite_world::entity::EntityId,
    pos: Vec3,
    y_rot: f32,
    x_rot: f32,
    relatives: i32,
) -> Option<Vec3> {
    use vanillite_world::entity::EntityFlags;
    let entity = ents.store.get(id)?;
    let rot = entity.rot();
    let flags = entity.flags();
    let mut body = Body {
        pos: entity.pos(),
        velocity: entity.velocity(),
        y_rot: rot.y_rot,
        x_rot: rot.x_rot,
        on_ground: flags.contains(EntityFlags::ON_GROUND),
        horizontal_collision: false,
        fall_flying: flags.contains(EntityFlags::FALL_FLYING),
    };
    let player = ents.player_mut(id)?;
    let sink = std::sync::Arc::clone(&player.connection.sink);
    player.connection.movement.teleport_relative(
        &mut body,
        pos,
        y_rot,
        x_rot,
        relatives,
        sink.as_ref(),
    );
    player.connection.movement.reset_position(body.pos);
    player.survival.teleported();
    if let Some(mut entity) = ents.store.get_mut(id) {
        entity.set_pos(body.pos);
        entity.set_velocity(body.velocity);
        entity.set_old_pos();
    }
    Some(body.pos)
}

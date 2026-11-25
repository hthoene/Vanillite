//! Play packets of mobs, combat and vehicles: attacks and interactions of a player, riding.

use crate::Packet;
use crate::packets::play::serverbound::Hand;
use crate::types::{LpVec3, VarIntList, Vec3};

/// `attack`: the player hits an entity.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Play, dir = Serverbound, name = "attack", bucket = "interact")]
pub struct Attack {
    #[field(varint)]
    pub entity_id: i32,
}

/// `interact`: the player uses a hand on an entity at a point relative to the entity.
#[derive(Debug, Clone, PartialEq, Packet)]
#[packet(state = Play, dir = Serverbound, name = "interact", bucket = "interact")]
pub struct Interact {
    #[field(varint)]
    pub entity_id: i32,
    pub hand: Hand,
    pub location: LpVec3,
    pub using_secondary_action: bool,
}

/// `move_vehicle` (serverbound): where the client moved the vehicle it controls.
#[derive(Debug, Clone, PartialEq, Packet)]
#[packet(state = Play, dir = Serverbound, name = "move_vehicle", bucket = "movement")]
pub struct MoveVehicle {
    #[field(finite)]
    pub x: f64,
    #[field(finite)]
    pub y: f64,
    #[field(finite)]
    pub z: f64,
    #[field(finite)]
    pub y_rot: f32,
    #[field(finite)]
    pub x_rot: f32,
    pub on_ground: bool,
}

/// `paddle_boat`: which paddles of its boat the client turns.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Play, dir = Serverbound, name = "paddle_boat", bucket = "movement")]
pub struct PaddleBoat {
    pub left: bool,
    pub right: bool,
}

/// `set_passengers`: the passengers of a vehicle.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Play, dir = Clientbound, name = "set_passengers")]
pub struct SetPassengers {
    #[field(varint)]
    pub vehicle: i32,
    #[field(max_len = 1 << 16)]
    pub passengers: VarIntList,
}

/// `set_entity_link`: a leashed entity and its holder (`0` = none).
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Play, dir = Clientbound, name = "set_entity_link")]
pub struct SetEntityLink {
    pub source_id: i32,
    pub dest_id: i32,
}

/// `move_vehicle` (clientbound): the server's position of the vehicle the client controls.
#[derive(Debug, Clone, PartialEq, Packet)]
#[packet(state = Play, dir = Clientbound, name = "move_vehicle")]
pub struct MoveVehicleBack {
    pub position: Vec3,
    pub y_rot: f32,
    pub x_rot: f32,
}

/// `animate`: an entity animation (4 = critical hit, 5 = magic critical hit).
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Play, dir = Clientbound, name = "animate")]
pub struct Animate {
    #[field(varint)]
    pub entity_id: i32,
    pub action: u8,
}

/// `cooldown`: an item group the client may not use for `duration` ticks.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Play, dir = Clientbound, name = "cooldown")]
pub struct Cooldown {
    #[field(max_len = 32767)]
    pub group: String,
    #[field(varint)]
    pub duration: i32,
}

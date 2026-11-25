//! Play packets of mobs and combat: the attack of a player.

use crate::Packet;

/// `attack`: the player hits an entity.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Play, dir = Serverbound, name = "attack", bucket = "interact")]
pub struct Attack {
    #[field(varint)]
    pub entity_id: i32,
}

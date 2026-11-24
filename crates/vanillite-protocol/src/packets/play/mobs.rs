//! Play packets of mobs and combat: the attack of a player and explosions.

use crate::Packet;
use crate::codec::{Decode, Encode, Reader, Result, write_varint};
use crate::types::Vec3;

use super::clientbound::SoundEvent;

/// `attack`: the player hits an entity.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Play, dir = Serverbound, name = "attack", bucket = "interact")]
pub struct Attack {
    #[field(varint)]
    pub entity_id: i32,
}

/// `explode`: the explosion effects, and the knockback of the receiving player if it was hit.
/// The particle is a particle type without options; no block particles are sent.
#[derive(Debug, Clone, PartialEq)]
pub struct Explode {
    pub center: Vec3,
    pub radius: f32,
    pub block_count: i32,
    pub player_knockback: Option<Vec3>,
    /// Registry id of the explosion particle (`minecraft:explosion` or
    /// `minecraft:explosion_emitter`).
    pub particle: i32,
    pub sound: SoundEvent,
}

impl Encode for Explode {
    fn encode(&self, out: &mut Vec<u8>) {
        self.center.encode(out);
        out.extend_from_slice(&self.radius.to_be_bytes());
        out.extend_from_slice(&self.block_count.to_be_bytes());
        self.player_knockback.encode(out);
        write_varint(out, self.particle);
        self.sound.encode(out);
        // `blockParticles`: an empty weighted list.
        write_varint(out, 0);
        // `playSound`.
        out.push(1);
    }
}

impl Decode for Explode {
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        let center = Vec3::decode(r)?;
        let radius = f32::decode(r)?;
        let block_count = i32::decode(r)?;
        let player_knockback = Option::<Vec3>::decode(r)?;
        let particle = r.varint()?;
        let sound = SoundEvent::decode(r)?;
        let _block_particles = r.varint()?;
        let _play_sound = r.u8()?;
        Ok(Explode {
            center,
            radius,
            block_count,
            player_knockback,
            particle,
            sound,
        })
    }
}

impl Packet for Explode {
    const ID: i32 = crate::ids::play::clientbound::EXPLODE;
    const NAME: &'static str = "explode";
    const STATE: crate::State = crate::State::Play;
    const DIRECTION: crate::Direction = crate::Direction::Clientbound;
    const COST: u16 = 1;
    const BUCKET: crate::Bucket = crate::Bucket::Default;
}

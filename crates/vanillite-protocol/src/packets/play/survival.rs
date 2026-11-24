//! Clientbound play packets of survival gameplay: time, weather sounds, health, food,
//! experience, damage, death and respawn.

use crate::Packet;
use crate::codec::{Decode, Encode, Error, Reader, Result, write_varint, write_varlong};
use crate::text::Component;
use crate::types::Vec3;

use super::clientbound::SpawnInfo;

/// A registered sound event (`Holder<SoundEvent>` by reference): the registry id, sent as
/// `id + 1`. Direct (inline) sound events are not used by the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SoundEvent(pub i32);

impl Encode for SoundEvent {
    fn encode(&self, out: &mut Vec<u8>) {
        write_varint(out, self.0.saturating_add(1));
    }
}

impl Decode for SoundEvent {
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        match r.varint()? {
            0 => Err(Error::Invalid("direct sound event")),
            id => Ok(SoundEvent(id.saturating_sub(1))),
        }
    }
}

crate::varint_enum! {
    /// `SoundSource`.
    pub enum SoundSource {
        Master,
        Music,
        Records,
        Weather,
        Blocks,
        Hostile,
        Neutral,
        Players,
        Ambient,
        Voice,
        Ui,
    }
}

/// One clock's state in `set_time` (`ClockNetworkState`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClockUpdate {
    /// Network id of the clock in `minecraft:world_clock`.
    pub clock: i32,
    pub total_ticks: i64,
    pub partial_tick: f32,
    /// Ticks per tick; `0` while the clock stands still.
    pub rate: f32,
}

/// `set_time`: the game time and the clocks that changed (all of them for a full sync).
#[derive(Debug, Clone, PartialEq)]
pub struct SetTime {
    pub game_time: i64,
    pub clocks: Vec<ClockUpdate>,
}

impl Encode for SetTime {
    fn encode(&self, out: &mut Vec<u8>) {
        self.game_time.encode(out);
        write_varint(out, i32::try_from(self.clocks.len()).unwrap_or(i32::MAX));
        for c in &self.clocks {
            write_varint(out, c.clock);
            write_varlong(out, c.total_ticks);
            c.partial_tick.encode(out);
            c.rate.encode(out);
        }
    }
}

impl Decode for SetTime {
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        let game_time = r.i64()?;
        let n = r.len_prefix("clock updates", 1024, 4)?;
        let mut clocks = Vec::with_capacity(n);
        for _ in 0..n {
            clocks.push(ClockUpdate {
                clock: r.varint()?,
                total_ticks: r.varlong()?,
                partial_tick: f32::decode(r)?,
                rate: f32::decode(r)?,
            });
        }
        Ok(SetTime { game_time, clocks })
    }
}

/// `set_health`.
#[derive(Debug, Clone, PartialEq, Packet)]
#[packet(state = Play, dir = Clientbound, name = "set_health")]
pub struct SetHealth {
    pub health: f32,
    #[field(varint)]
    pub food: i32,
    pub saturation: f32,
}

/// `set_experience`.
#[derive(Debug, Clone, PartialEq, Packet)]
#[packet(state = Play, dir = Clientbound, name = "set_experience")]
pub struct SetExperience {
    pub progress: f32,
    #[field(varint)]
    pub level: i32,
    #[field(varint)]
    pub total: i32,
}

/// `change_difficulty`.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Play, dir = Clientbound, name = "change_difficulty")]
pub struct ChangeDifficulty {
    /// `Difficulty.getId`: 0 peaceful, 1 easy, 2 normal, 3 hard.
    #[field(varint)]
    pub difficulty: i32,
    pub locked: bool,
}

/// `respawn`. `data_to_keep`: 1 keeps attributes, 2 keeps entity data.
#[derive(Debug, Clone, PartialEq)]
pub struct Respawn {
    pub spawn: SpawnInfo,
    pub data_to_keep: u8,
}

impl Encode for Respawn {
    fn encode(&self, out: &mut Vec<u8>) {
        self.spawn.encode(out);
        out.push(self.data_to_keep);
    }
}

impl Decode for Respawn {
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        Ok(Respawn {
            spawn: SpawnInfo::decode(r)?,
            data_to_keep: r.u8()?,
        })
    }
}

/// `player_combat_kill`: opens the death screen with the death message.
#[derive(Debug, Clone, PartialEq, Packet)]
#[packet(state = Play, dir = Clientbound, name = "player_combat_kill")]
pub struct PlayerCombatKill {
    #[field(varint)]
    pub player_id: i32,
    #[field(max_len = 2_097_152)]
    pub message: Component,
}

/// `damage_event`. Entity ids of the cause and the direct entity are `-1` when absent.
#[derive(Debug, Clone, PartialEq)]
pub struct DamageEvent {
    pub entity_id: i32,
    /// Network id of the damage type in `minecraft:damage_type`.
    pub source_type: i32,
    pub source_cause_id: i32,
    pub source_direct_id: i32,
    pub source_position: Option<Vec3>,
}

impl Encode for DamageEvent {
    fn encode(&self, out: &mut Vec<u8>) {
        write_varint(out, self.entity_id);
        write_varint(out, self.source_type);
        write_varint(out, self.source_cause_id.saturating_add(1));
        write_varint(out, self.source_direct_id.saturating_add(1));
        self.source_position.encode(out);
    }
}

impl Decode for DamageEvent {
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        Ok(DamageEvent {
            entity_id: r.varint()?,
            source_type: r.varint()?,
            source_cause_id: r.varint()?.saturating_sub(1),
            source_direct_id: r.varint()?.saturating_sub(1),
            source_position: Option::<Vec3>::decode(r)?,
        })
    }
}

/// `hurt_animation`.
#[derive(Debug, Clone, PartialEq, Packet)]
#[packet(state = Play, dir = Clientbound, name = "hurt_animation")]
pub struct HurtAnimation {
    #[field(varint)]
    pub entity_id: i32,
    pub yaw: f32,
}

/// `sound`: a sound at a position (coordinates × 8).
#[derive(Debug, Clone, PartialEq, Packet)]
#[packet(state = Play, dir = Clientbound, name = "sound")]
pub struct Sound {
    pub sound: SoundEvent,
    pub source: SoundSource,
    pub x: i32,
    pub y: i32,
    pub z: i32,
    pub volume: f32,
    pub pitch: f32,
    pub seed: i64,
}

impl Sound {
    /// `ClientboundSoundPacket`'s constructor: the position in eighths of a block.
    #[allow(clippy::too_many_arguments)]
    pub fn at(
        sound: SoundEvent,
        source: SoundSource,
        x: f64,
        y: f64,
        z: f64,
        volume: f32,
        pitch: f32,
        seed: i64,
    ) -> Sound {
        // `(int)(x * 8.0)`: truncation, saturating like Java's cast.
        #[allow(clippy::as_conversions, clippy::cast_possible_truncation)]
        let eighths = |v: f64| (v * 8.0) as i32;
        Sound {
            sound,
            source,
            x: eighths(x),
            y: eighths(y),
            z: eighths(z),
            volume,
            pitch,
            seed,
        }
    }
}

macro_rules! manual_packet {
    ($ty:ident, $id:ident, $name:literal) => {
        impl Packet for $ty {
            const ID: i32 = crate::ids::play::clientbound::$id;
            const NAME: &'static str = $name;
            const STATE: crate::State = crate::State::Play;
            const DIRECTION: crate::Direction = crate::Direction::Clientbound;
            const COST: u16 = 1;
            const BUCKET: crate::Bucket = crate::Bucket::Default;
        }
    };
}

manual_packet!(SetTime, SET_TIME, "set_time");
manual_packet!(Respawn, RESPAWN, "respawn");
manual_packet!(DamageEvent, DAMAGE_EVENT, "damage_event");

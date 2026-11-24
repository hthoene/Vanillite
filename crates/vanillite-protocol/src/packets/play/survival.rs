//! Clientbound play packets of survival gameplay: time, weather sounds, health, food,
//! experience, damage, death and respawn; and the sounds, particles and rotations of commands.

use crate::Packet;
use crate::codec::{Decode, Encode, Error, Reader, Result, write_varint, write_varlong};
use crate::text::Component;
use crate::types::{Identifier, Vec3};

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

/// `sound` with a direct sound event (`Holder.direct(SoundEvent.createVariableRangeEvent(id))`),
/// as `/playsound` sends it. The position is in eighths of a block.
#[derive(Debug, Clone, PartialEq)]
pub struct SoundDirect {
    pub sound: Identifier,
    /// A fixed audible range; `None` for variable range events.
    pub fixed_range: Option<f32>,
    pub source: SoundSource,
    pub x: i32,
    pub y: i32,
    pub z: i32,
    pub volume: f32,
    pub pitch: f32,
    pub seed: i64,
}

impl Encode for SoundDirect {
    fn encode(&self, out: &mut Vec<u8>) {
        write_varint(out, 0);
        self.sound.encode(out);
        self.fixed_range.encode(out);
        self.source.encode(out);
        for v in [self.x, self.y, self.z] {
            v.encode(out);
        }
        self.volume.encode(out);
        self.pitch.encode(out);
        self.seed.encode(out);
    }
}

impl Decode for SoundDirect {
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        if r.varint()? != 0 {
            return Err(Error::Invalid("registry sound event"));
        }
        Ok(SoundDirect {
            sound: Identifier::decode(r)?,
            fixed_range: Option::<f32>::decode(r)?,
            source: SoundSource::decode(r)?,
            x: r.i32()?,
            y: r.i32()?,
            z: r.i32()?,
            volume: r.f32()?,
            pitch: r.f32()?,
            seed: r.i64()?,
        })
    }
}

/// `stop_sound` (`ClientboundStopSoundPacket`): stops sounds by source and/or name; neither
/// stops all sounds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopSound {
    pub source: Option<SoundSource>,
    pub sound: Option<Identifier>,
}

impl Encode for StopSound {
    fn encode(&self, out: &mut Vec<u8>) {
        let flags = u8::from(self.source.is_some()) | (u8::from(self.sound.is_some()) << 1);
        out.push(flags);
        if let Some(source) = &self.source {
            source.encode(out);
        }
        if let Some(sound) = &self.sound {
            sound.encode(out);
        }
    }
}

impl Decode for StopSound {
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        let flags = r.u8()?;
        let source = if flags & 1 != 0 {
            Some(SoundSource::decode(r)?)
        } else {
            None
        };
        let sound = if flags & 2 != 0 {
            Some(Identifier::decode(r)?)
        } else {
            None
        };
        Ok(StopSound { source, sound })
    }
}

/// `player_rotation` (`ClientboundPlayerRotationPacket`): turns the client's player.
#[derive(Debug, Clone, PartialEq, Packet)]
#[packet(state = Play, dir = Clientbound, name = "player_rotation")]
pub struct PlayerRotation {
    pub y_rot: f32,
    pub relative_y: bool,
    pub x_rot: f32,
    pub relative_x: bool,
}

/// A particle with its options in their network form (`ParticleTypes.STREAM_CODEC`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Particle {
    /// Id in `minecraft:particle_type`.
    pub kind: i32,
    /// The type's options as written by its stream codec (empty for simple particles).
    pub options: Vec<u8>,
}

impl Particle {
    /// Reads the options of `kind`. Item particles (a full item stack) are not decoded.
    fn decode_options(kind: i32, r: &mut Reader<'_>) -> Result<Vec<u8>> {
        let name = u16::try_from(kind)
            .ok()
            .and_then(|id| vanillite_data::registry::PARTICLE_TYPE.name(id))
            .ok_or(Error::Invalid("particle type"))?;
        let path = name.strip_prefix("minecraft:").unwrap_or(name);
        let start = r.rest();
        match path {
            "block" | "block_marker" | "falling_dust" | "dust_pillar" | "block_crumble"
            | "shriek" => {
                r.varint()?;
            }
            "entity_effect" | "tinted_leaves" | "flash" | "dragon_breath" | "sculk_charge"
            | "geyser" | "geyser_plume" => r.skip(4)?,
            "dust" | "effect" | "instant_effect" | "geyser_base" | "geyser_poof" => r.skip(8)?,
            "dust_color_transition" => r.skip(12)?,
            "trail" => {
                r.skip(28)?;
                r.varint()?;
            }
            "vibration" => {
                match r.varint()? {
                    0 => r.skip(8)?,
                    _ => {
                        r.varint()?;
                        r.skip(4)?;
                    }
                }
                r.varint()?;
            }
            "item" => return Err(Error::Invalid("item particle")),
            _ => {}
        }
        let used = start.len().saturating_sub(r.rest().len());
        Ok(start.get(..used).unwrap_or_default().to_vec())
    }
}

/// `level_particles` (`ClientboundLevelParticlesPacket`).
#[derive(Debug, Clone, PartialEq)]
pub struct LevelParticles {
    pub particle: Particle,
    pub override_limiter: bool,
    pub always_show: bool,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub x_dist: f32,
    pub y_dist: f32,
    pub z_dist: f32,
    pub x_max_speed: f32,
    pub y_max_speed: f32,
    pub z_max_speed: f32,
    pub count: i32,
    /// `RandomizationType` id: 0 default, 1 alternative, 2 alternative with speed.
    pub randomization: i32,
}

impl Encode for LevelParticles {
    fn encode(&self, out: &mut Vec<u8>) {
        write_varint(out, self.particle.kind);
        out.extend_from_slice(&self.particle.options);
        self.override_limiter.encode(out);
        self.always_show.encode(out);
        for v in [self.x, self.y, self.z] {
            v.encode(out);
        }
        for v in [
            self.x_dist,
            self.y_dist,
            self.z_dist,
            self.x_max_speed,
            self.y_max_speed,
            self.z_max_speed,
        ] {
            v.encode(out);
        }
        write_varint(out, self.count);
        write_varint(out, self.randomization);
    }
}

impl Decode for LevelParticles {
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        let kind = r.varint()?;
        let options = Particle::decode_options(kind, r)?;
        Ok(LevelParticles {
            particle: Particle { kind, options },
            override_limiter: bool::decode(r)?,
            always_show: bool::decode(r)?,
            x: r.f64()?,
            y: r.f64()?,
            z: r.f64()?,
            x_dist: r.f32()?,
            y_dist: r.f32()?,
            z_dist: r.f32()?,
            x_max_speed: r.f32()?,
            y_max_speed: r.f32()?,
            z_max_speed: r.f32()?,
            count: r.varint()?,
            randomization: r.varint()?,
        })
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
manual_packet!(SoundDirect, SOUND, "sound");
manual_packet!(StopSound, STOP_SOUND, "stop_sound");
manual_packet!(LevelParticles, LEVEL_PARTICLES, "level_particles");

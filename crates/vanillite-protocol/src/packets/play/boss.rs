//! Clientbound play packets of bosses and status effects: boss bars and mob effects.

use crate::Packet;
use crate::codec::{Decode, DecodeLimited, Encode, Error, Reader, Result, write_varint};
use crate::text::Component;
use crate::types::Uuid;

/// `update_mob_effect`: a status effect of an entity started or changed.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Play, dir = Clientbound, name = "update_mob_effect")]
pub struct UpdateMobEffect {
    #[field(varint)]
    pub entity_id: i32,
    /// Registry id in `minecraft:mob_effect`.
    #[field(varint)]
    pub effect: i32,
    #[field(varint)]
    pub amplifier: i32,
    /// Ticks left, `-1` for infinite.
    #[field(varint)]
    pub duration: i32,
    /// `1` ambient, `2` visible, `4` show icon, `8` blend.
    pub flags: u8,
}

/// `remove_mob_effect`.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Play, dir = Clientbound, name = "remove_mob_effect")]
pub struct RemoveMobEffect {
    #[field(varint)]
    pub entity_id: i32,
    #[field(varint)]
    pub effect: i32,
}

/// `BossEvent.BossBarColor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BossBarColor {
    #[default]
    Pink,
    Blue,
    Red,
    Green,
    Yellow,
    Purple,
    White,
}

/// `BossEvent.BossBarOverlay`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BossBarOverlay {
    #[default]
    Progress,
    Notched6,
    Notched10,
    Notched12,
    Notched20,
}

/// The boss bar properties (`darkenScreen`, `playBossMusic`, `createWorldFog`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BossBarFlags {
    pub darken_screen: bool,
    pub play_music: bool,
    pub create_fog: bool,
}

impl BossBarFlags {
    fn byte(self) -> u8 {
        u8::from(self.darken_screen)
            | u8::from(self.play_music) << 1
            | u8::from(self.create_fog) << 2
    }

    fn of(b: u8) -> Self {
        BossBarFlags {
            darken_screen: b & 1 != 0,
            play_music: b & 2 != 0,
            create_fog: b & 4 != 0,
        }
    }
}

/// One operation of a `boss_event` packet.
#[derive(Debug, Clone, PartialEq)]
pub enum BossEventOperation {
    Add {
        name: Component,
        progress: f32,
        color: BossBarColor,
        overlay: BossBarOverlay,
        flags: BossBarFlags,
    },
    Remove,
    UpdateProgress(f32),
    UpdateName(Component),
    UpdateStyle {
        color: BossBarColor,
        overlay: BossBarOverlay,
    },
    UpdateProperties(BossBarFlags),
}

/// `boss_event`: adds, changes or removes a boss bar of the client.
#[derive(Debug, Clone, PartialEq)]
pub struct BossEvent {
    pub id: Uuid,
    pub operation: BossEventOperation,
}

const COLORS: [BossBarColor; 7] = [
    BossBarColor::Pink,
    BossBarColor::Blue,
    BossBarColor::Red,
    BossBarColor::Green,
    BossBarColor::Yellow,
    BossBarColor::Purple,
    BossBarColor::White,
];
const OVERLAYS: [BossBarOverlay; 5] = [
    BossBarOverlay::Progress,
    BossBarOverlay::Notched6,
    BossBarOverlay::Notched10,
    BossBarOverlay::Notched12,
    BossBarOverlay::Notched20,
];

#[allow(clippy::as_conversions)]
fn write_style(out: &mut Vec<u8>, color: BossBarColor, overlay: BossBarOverlay) {
    write_varint(out, color as i32);
    write_varint(out, overlay as i32);
}

fn read_style(r: &mut Reader<'_>) -> Result<(BossBarColor, BossBarOverlay)> {
    let pick = |id: u32| usize::try_from(id).unwrap_or(usize::MAX);
    let color = COLORS.get(pick(r.enum_id(7, "boss bar color")?));
    let overlay = OVERLAYS.get(pick(r.enum_id(5, "boss bar overlay")?));
    match (color, overlay) {
        (Some(&c), Some(&o)) => Ok((c, o)),
        _ => Err(Error::Invalid("boss bar style")),
    }
}

impl Encode for BossEvent {
    fn encode(&self, out: &mut Vec<u8>) {
        self.id.encode(out);
        match &self.operation {
            BossEventOperation::Add {
                name,
                progress,
                color,
                overlay,
                flags,
            } => {
                write_varint(out, 0);
                name.encode(out);
                out.extend_from_slice(&progress.to_be_bytes());
                write_style(out, *color, *overlay);
                out.push(flags.byte());
            }
            BossEventOperation::Remove => write_varint(out, 1),
            BossEventOperation::UpdateProgress(p) => {
                write_varint(out, 2);
                out.extend_from_slice(&p.to_be_bytes());
            }
            BossEventOperation::UpdateName(name) => {
                write_varint(out, 3);
                name.encode(out);
            }
            BossEventOperation::UpdateStyle { color, overlay } => {
                write_varint(out, 4);
                write_style(out, *color, *overlay);
            }
            BossEventOperation::UpdateProperties(flags) => {
                write_varint(out, 5);
                out.push(flags.byte());
            }
        }
    }
}

const MAX_NAME: usize = 2_097_152;

impl Decode for BossEvent {
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        let id = Uuid::decode(r)?;
        let operation = match r.varint()? {
            0 => {
                let name = Component::decode_limited(r, MAX_NAME)?;
                let progress = r.f32()?;
                let (color, overlay) = read_style(r)?;
                BossEventOperation::Add {
                    name,
                    progress,
                    color,
                    overlay,
                    flags: BossBarFlags::of(r.u8()?),
                }
            }
            1 => BossEventOperation::Remove,
            2 => BossEventOperation::UpdateProgress(r.f32()?),
            3 => BossEventOperation::UpdateName(Component::decode_limited(r, MAX_NAME)?),
            4 => {
                let (color, overlay) = read_style(r)?;
                BossEventOperation::UpdateStyle { color, overlay }
            }
            5 => BossEventOperation::UpdateProperties(BossBarFlags::of(r.u8()?)),
            _ => return Err(Error::Invalid("boss event operation")),
        };
        Ok(BossEvent { id, operation })
    }
}

impl Packet for BossEvent {
    const ID: i32 = crate::ids::play::clientbound::BOSS_EVENT;
    const NAME: &'static str = "boss_event";
    const STATE: crate::State = crate::State::Play;
    const DIRECTION: crate::Direction = crate::Direction::Clientbound;
    const COST: u16 = 1;
    const BUCKET: crate::Bucket = crate::Bucket::Default;
}

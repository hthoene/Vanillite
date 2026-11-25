//! Survival gameplay: health, damage, hunger, air, fire, death, experience, world clocks,
//! weather and game rules (Vanilla 26.3).
//!
//! The per-player logic works on the entity's core columns and its [`PlayerData`]; what it sends
//! to other players goes into an [`Effect`] list that the world flushes after the player loop
//! (viewers are looked up through the section index, so a broadcast never scans all players).
//!
//! [`PlayerData`]: crate::player::PlayerData

pub mod blocks;
pub mod clock;
pub mod combat;
pub mod damage;
pub mod food;
pub mod player;
pub mod rules;
pub mod weather;

use std::sync::Arc;

use vanillite_base::Vec3;
use vanillite_protocol::SharedPacket;
use vanillite_protocol::packets::play::clientbound::{Sound, SoundEvent, SoundSource};
use vanillite_protocol::text::Component;
use vanillite_world::entity::EntityId;

/// Something a world sends to other players after its player loop.
#[derive(Clone, Debug)]
pub enum Effect {
    /// To the players that track `entity`, and to `entity` itself when `with_self` is set
    /// (`sendToTrackingPlayersAndSelf`).
    Tracking {
        entity: EntityId,
        with_self: bool,
        packet: Arc<SharedPacket>,
    },
    /// To the players of this world within `range` blocks of `pos`, except `except`
    /// (`PlayerList.broadcast`).
    Nearby {
        pos: Vec3,
        range: f64,
        except: Option<EntityId>,
        packet: Arc<SharedPacket>,
    },
    /// A system message to every player of the server.
    Broadcast(Component),
    /// `ServerPlayer.die`'s scoreboard part: the death message for the players the victim's
    /// team allows, the death count and the kill scores of the killer (`getKillCredit`).
    Died {
        victim: EntityId,
        killer: Option<EntityId>,
        message: Option<Component>,
    },
    /// To one player.
    To {
        player: EntityId,
        packet: Arc<SharedPacket>,
    },
    /// A new item entity (a dropped stack): its UUID, position, motion, yaw and pickup delay.
    DropItem {
        uuid: u128,
        pos: Vec3,
        velocity: Vec3,
        y_rot: f32,
        pickup_delay: i16,
        stack: crate::item::ItemStack,
    },
}

/// The registry id of a sound event, `0` if unknown.
pub fn sound_event(name: &str) -> SoundEvent {
    SoundEvent(
        vanillite_data::registry::SOUND_EVENT
            .id(name)
            .map_or(0, i32::from),
    )
}

/// `Level.playSound(except, x, y, z, sound, source, volume, pitch)`: heard within 16 blocks, or
/// `16 * volume` for louder sounds.
#[allow(clippy::too_many_arguments)]
pub fn play_sound(
    out: &mut Vec<Effect>,
    except: Option<EntityId>,
    pos: Vec3,
    sound: &str,
    source: SoundSource,
    volume: f32,
    pitch: f32,
    seed: i64,
) {
    let packet = SharedPacket::of(&Sound::at(
        sound_event(sound),
        source,
        pos.x,
        pos.y,
        pos.z,
        volume,
        pitch,
        seed,
    ));
    out.push(Effect::Nearby {
        pos,
        range: f64::from(if volume > 1.0 { 16.0 * volume } else { 16.0 }),
        except,
        packet,
    });
}

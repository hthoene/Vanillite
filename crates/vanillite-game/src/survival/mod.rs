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
pub mod inside;
pub mod player;
pub mod respawn;
pub mod rules;
pub mod shoulder;
pub mod sleep;
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
        /// The killer's type when the killer may be gone before the effect is delivered (a
        /// creeper that exploded): `ENTITY_KILLED_BY`.
        killer_kind: Option<vanillite_data::entity::EntityTypeId>,
        message: Option<Component>,
    },
    /// To one player.
    To {
        player: EntityId,
        packet: Arc<SharedPacket>,
    },
    /// A new item entity (a dropped stack): its UUID, position, motion, yaw, pickup delay and
    /// the state of its own random.
    DropItem {
        uuid: u128,
        random: i64,
        pos: Vec3,
        velocity: Vec3,
        y_rot: f32,
        pickup_delay: i16,
        stack: crate::item::ItemStack,
    },
    /// Experience orbs (`ExperienceOrb.award`).
    Experience { pos: Vec3, amount: i32 },
    /// A mob an effect releases (`EntityType.create(level, TRIGGERED)` + `addFreshEntity`):
    /// silverfish of Infested, slimes of Oozing. Kinds the mob system does not simulate yet
    /// are skipped.
    SpawnMob {
        kind: vanillite_data::entity::EntityTypeId,
        pos: Vec3,
        y_rot: f32,
        velocity: Vec3,
    },
    /// `gameEvent(event, pos, context)`: posted to the world's vibration listeners once the
    /// world handles its effects.
    GameEvent {
        event: u16,
        pos: Vec3,
        context: crate::level::game_event::GameEventContext,
    },
    /// The player with this UUID died: with `ender_pearls_vanish_on_death` its ender pearls in
    /// the other worlds vanish (the pearls of its own world see it themselves).
    PearlsVanish(u128),
    /// An entity from a player's shoulder back in the world (`respawnEntityOnShoulder`): its
    /// NBT, the position and the player that becomes its owner.
    RespawnShoulder {
        tag: Box<vanillite_nbt::Compound>,
        pos: Vec3,
        owner: u128,
    },
}

/// Queues `gameEvent(event, pos, Context.of(source))`.
pub fn game_event(out: &mut Vec<Effect>, event: u16, pos: Vec3, source: Option<EntityId>) {
    out.push(Effect::GameEvent {
        event,
        pos,
        context: crate::level::game_event::GameEventContext::of(source),
    });
}

/// The seed of a sound (`Level.soundSeedGenerator`, a random of its own that Vanilla never
/// seeds): sounds draw nothing from the level's or an entity's random.
pub fn sound_seed() -> i64 {
    use vanillite_base::random::{LegacyRandom, RandomSource};
    thread_local! {
        static SEEDS: std::cell::RefCell<LegacyRandom> = std::cell::RefCell::new(LegacyRandom::new(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos() as i64),
        ));
    }
    SEEDS.with_borrow_mut(RandomSource::next_long)
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

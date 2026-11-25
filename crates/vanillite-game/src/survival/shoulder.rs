//! Entities on a player's shoulders (`ServerPlayer.shoulderEntityLeft/Right`): the parrots
//! that landed there as entity NBT, shown to clients by their variant
//! (`DATA_SHOULDER_PARROT_LEFT/RIGHT`), and how they come back into the world
//! (`removeEntitiesOnShoulder`, `respawnEntityOnShoulder`).

use vanillite_base::Vec3;
use vanillite_base::random::RandomSource;
use vanillite_nbt::{Compound, Tag};
use vanillite_world::entity::EntityMut;

use super::Effect;
use super::player::Env;
use crate::player::PlayerData;

/// The entities on a player's shoulders.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Shoulders {
    /// `shoulderEntityLeft` (empty: none).
    pub left: Compound,
    /// `shoulderEntityRight` (empty: none).
    pub right: Compound,
    /// `timeEntitySatOnShoulder`: the game time the last one landed.
    pub sat_at: i64,
}

impl Shoulders {
    /// `ShoulderEntityLeft` and `ShoulderEntityRight` of a player data file.
    pub fn load(data: &Compound) -> Shoulders {
        let part = |key: &str| match data.get(key) {
            Some(Tag::Compound(c)) => c.clone(),
            _ => Compound::new(),
        };
        Shoulders {
            left: part("ShoulderEntityLeft"),
            right: part("ShoulderEntityRight"),
            sat_at: 0,
        }
    }

    /// Writes the non-empty shoulders.
    pub fn save(&self, out: &mut Compound) {
        out.remove("ShoulderEntityLeft");
        out.remove("ShoulderEntityRight");
        if !self.left.is_empty() {
            out.insert("ShoulderEntityLeft", Tag::Compound(self.left.clone()));
        }
        if !self.right.is_empty() {
            out.insert("ShoulderEntityRight", Tag::Compound(self.right.clone()));
        }
    }

    /// Whether neither shoulder carries an entity.
    pub fn is_empty(&self) -> bool {
        self.left.is_empty() && self.right.is_empty()
    }

    /// `DATA_SHOULDER_PARROT_LEFT` (`extractParrotVariant`).
    pub fn parrot_left(&self) -> Option<i32> {
        parrot_variant(&self.left)
    }

    /// `DATA_SHOULDER_PARROT_RIGHT`.
    pub fn parrot_right(&self) -> Option<i32> {
        parrot_variant(&self.right)
    }

    /// `setEntityOnShoulder` once the player may carry one: the left shoulder first, then the
    /// right. Returns whether the entity found a place.
    pub fn put(&mut self, tag: Compound, game_time: i64) -> bool {
        let slot = if self.left.is_empty() {
            &mut self.left
        } else if self.right.is_empty() {
            &mut self.right
        } else {
            return false;
        };
        *slot = tag;
        self.sat_at = game_time;
        true
    }
}

/// `extractParrotVariant`: the variant of a parrot's NBT (`Variant`, clamped to 0–4).
fn parrot_variant(tag: &Compound) -> Option<i32> {
    match tag.get("id") {
        Some(Tag::String(id)) if id == "minecraft:parrot" || id == "parrot" => {}
        _ => return None,
    }
    let v = tag.get("Variant").and_then(Tag::as_i64).unwrap_or(0);
    Some(i32::try_from(v.clamp(0, 4)).unwrap_or(0))
}

/// `ServerPlayer.removeEntitiesOnShoulder`: entities that sat for more than a second come
/// back into the world 0.7 blocks above the player, owned by it.
pub fn remove_entities(entity: &mut EntityMut<'_>, player: &mut PlayerData, env: &mut Env<'_>) {
    let shoulders = &mut player.survival.shoulders;
    if shoulders.is_empty() || shoulders.sat_at + 20 >= env.game_time {
        return;
    }
    let owner = entity.as_ref().uuid();
    let p = entity.as_ref().pos();
    #[allow(clippy::cast_lossless)]
    let pos = Vec3::new(p.x, p.y + f64::from(0.7_f32), p.z);
    for tag in [
        std::mem::take(&mut shoulders.left),
        std::mem::take(&mut shoulders.right),
    ] {
        if !tag.is_empty() {
            env.out.push(Effect::RespawnShoulder {
                tag: Box::new(tag),
                pos,
                owner,
            });
        }
    }
    entity.mark_data_dirty();
}

/// `ServerPlayer.handleShoulderEntities`: an occasional parrot sound (one in 200 ticks per
/// non-silent shoulder entity; the imitation of nearby mobs is not done here), and the
/// shoulders empty when the player falls, is in water, sleeps or spectates.
pub fn handle(
    entity: &mut EntityMut<'_>,
    player: &mut PlayerData,
    env: &mut Env<'_>,
    sleeping: bool,
) {
    if player.survival.shoulders.is_empty() {
        return;
    }
    let pos = entity.as_ref().pos();
    for left in [true, false] {
        let s = &player.survival.shoulders;
        let tag = if left { &s.left } else { &s.right };
        if tag.is_empty() || matches!(tag.get("Silent"), Some(Tag::Byte(b)) if *b != 0) {
            continue;
        }
        if env.random.next_int_bounded(200) != 0 || parrot_variant(tag).is_none() {
            continue;
        }
        // `Parrot.getAmbient(level, random)` and `getPitch(random)`.
        let sound = crate::mob::fam::companion::parrot::ambient(env.difficulty, &mut *env.random);
        let pitch = (env.random.next_float() - env.random.next_float()) * 0.2 + 1.0;
        let seed = crate::survival::sound_seed();
        crate::survival::play_sound(
            env.out,
            None,
            pos,
            sound,
            vanillite_protocol::packets::play::clientbound::SoundSource::Players,
            1.0,
            pitch,
            seed,
        );
    }
    // `setGameMode(SPECTATOR)` drops them too; here at the next tick.
    let spectator = player.game_mode == vanillite_config::GameMode::Spectator;
    let s = &player.survival;
    if s.fall_distance > 0.5 || s.fluids.in_water() || sleeping || spectator {
        remove_entities(entity, player, env);
    }
}

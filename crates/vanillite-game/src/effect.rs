//! Status effects of living entities (`MobEffectInstance` in `LivingEntity.activeEffects`):
//! adding and merging, the per-tick effect application and expiry, the effect packets of
//! players and the `active_effects` NBT list.
//!
//! The effects that act every tick (regeneration, poison, wither) are reported as
//! [`EffectTick`]s for the caller to apply with its own damage and heal path; effects that
//! change other values (mining fatigue, darkness, slowness) are read with [`MobEffects::get`]
//! where those values are computed.

use std::sync::Arc;

use vanillite_nbt::{Compound, Tag};
use vanillite_protocol::SharedPacket;
use vanillite_protocol::packets::play::clientbound::{RemoveMobEffect, UpdateMobEffect};

/// Registry id of a mob effect (`minecraft:mob_effect`), `u16::MAX` if unknown.
pub fn id(name: &str) -> u16 {
    vanillite_data::registry::MOB_EFFECT
        .id(name)
        .unwrap_or(u16::MAX)
}

/// Name of a mob effect id.
pub fn name(effect: u16) -> Option<&'static str> {
    vanillite_data::registry::MOB_EFFECT.name(effect)
}

macro_rules! effect_ids {
    ($($konst:ident = $name:literal),* $(,)?) => {
        /// Ids of the effects the server acts on.
        pub mod ids {
            $(
                #[doc = concat!("`", $name, "`.")]
                pub fn $konst() -> u16 {
                    static ID: std::sync::OnceLock<u16> = std::sync::OnceLock::new();
                    *ID.get_or_init(|| super::id($name))
                }
            )*
        }
    };
}

effect_ids! {
    speed = "minecraft:speed",
    slowness = "minecraft:slowness",
    haste = "minecraft:haste",
    mining_fatigue = "minecraft:mining_fatigue",
    regeneration = "minecraft:regeneration",
    poison = "minecraft:poison",
    wither = "minecraft:wither",
    darkness = "minecraft:darkness",
    blindness = "minecraft:blindness",
    invisibility = "minecraft:invisibility",
    resistance = "minecraft:resistance",
    fire_resistance = "minecraft:fire_resistance",
    trial_omen = "minecraft:trial_omen",
    bad_omen = "minecraft:bad_omen",
    raid_omen = "minecraft:raid_omen",
}

/// Infinite duration (`MobEffectInstance.INFINITE_DURATION`).
pub const INFINITE: i32 = -1;

/// One active effect (`MobEffectInstance`; hidden lower effects are not kept).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EffectInstance {
    pub effect: u16,
    pub amplifier: i32,
    /// Ticks left, [`INFINITE`] for infinite.
    pub duration: i32,
    pub ambient: bool,
    pub visible: bool,
    pub show_icon: bool,
}

impl EffectInstance {
    /// `new MobEffectInstance(effect, duration, amplifier)`: visible, not ambient, with icon.
    pub fn new(effect: u16, duration: i32, amplifier: i32) -> EffectInstance {
        EffectInstance {
            effect,
            amplifier,
            duration,
            ambient: false,
            visible: true,
            show_icon: true,
        }
    }

    /// `new MobEffectInstance(effect, duration, amplifier, ambient, visible)`.
    pub fn with_flags(
        effect: u16,
        duration: i32,
        amplifier: i32,
        ambient: bool,
        visible: bool,
    ) -> EffectInstance {
        EffectInstance {
            effect,
            amplifier,
            duration,
            ambient,
            visible,
            show_icon: visible,
        }
    }

    /// `isInfiniteDuration`.
    pub fn is_infinite(&self) -> bool {
        self.duration == INFINITE
    }

    /// `endsWithin(ticks)`.
    pub fn ends_within(&self, ticks: i32) -> bool {
        !self.is_infinite() && self.duration <= ticks
    }

    /// `isShorterDurationThan(other)`.
    fn is_shorter_than(&self, other: &EffectInstance) -> bool {
        !self.is_infinite() && (self.duration < other.duration || other.is_infinite())
    }

    /// `MobEffectInstance.update(takeOver)` without the hidden effect chain; whether anything
    /// changed.
    fn update(&mut self, other: &EffectInstance) -> bool {
        let mut changed = false;
        if other.amplifier > self.amplifier {
            self.amplifier = other.amplifier;
            self.duration = other.duration;
            changed = true;
        } else if self.is_shorter_than(other) && other.amplifier == self.amplifier {
            self.duration = other.duration;
            changed = true;
        }
        if !other.ambient && self.ambient || changed {
            self.ambient = other.ambient;
            changed = true;
        }
        if other.visible != self.visible {
            self.visible = other.visible;
            changed = true;
        }
        if other.show_icon != self.show_icon {
            self.show_icon = other.show_icon;
            changed = true;
        }
        changed
    }

    /// The flags byte of `update_mob_effect`.
    pub fn packet_flags(&self) -> u8 {
        u8::from(self.ambient) | u8::from(self.visible) << 1 | u8::from(self.show_icon) << 2
    }

    /// `update_mob_effect` of this effect for the entity with `network_id`.
    pub fn update_packet(&self, network_id: i32, blend: bool) -> Arc<SharedPacket> {
        SharedPacket::of(&UpdateMobEffect {
            entity_id: network_id,
            effect: i32::from(self.effect),
            amplifier: self.amplifier,
            duration: self.duration,
            flags: self.packet_flags() | if blend { 8 } else { 0 },
        })
    }

    /// The `active_effects` entry (`MobEffectInstance.CODEC`).
    pub fn save(&self) -> Option<Compound> {
        let mut c = Compound::new();
        c.insert("id", Tag::String(name(self.effect)?.into()));
        if self.amplifier != 0 {
            c.insert(
                "amplifier",
                Tag::Byte(self.amplifier.clamp(0, 255) as u8 as i8),
            );
        }
        if self.duration != 0 {
            c.insert("duration", Tag::Int(self.duration));
        }
        if self.ambient {
            c.insert("ambient", Tag::Byte(1));
        }
        if !self.visible {
            c.insert("show_particles", Tag::Byte(0));
        }
        if self.show_icon != self.visible {
            c.insert("show_icon", Tag::Byte(i8::from(self.show_icon)));
        }
        Some(c)
    }

    /// Reads an `active_effects` entry.
    pub fn load(c: &Compound) -> Option<EffectInstance> {
        let Some(Tag::String(n)) = c.get("id") else {
            return None;
        };
        let effect = vanillite_data::registry::MOB_EFFECT.id(n)?;
        let byte = |k: &str, d: bool| c.get(k).and_then(Tag::as_i64).map_or(d, |v| v != 0);
        let amplifier = c
            .get("amplifier")
            .and_then(Tag::as_i64)
            .map_or(0, |v| (v as u8).into());
        let duration = c
            .get("duration")
            .and_then(Tag::as_i64)
            .and_then(|v| i32::try_from(v).ok())
            .unwrap_or(0);
        let visible = byte("show_particles", true);
        Some(EffectInstance {
            effect,
            amplifier,
            duration,
            ambient: byte("ambient", false),
            visible,
            show_icon: byte("show_icon", visible),
        })
    }
}

/// What the effects of one tick do to their entity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EffectTick {
    /// `hurtServer(magic, amount)` (poison above one health point).
    Magic(f32),
    /// `hurtServer(wither, amount)`.
    Wither(f32),
    /// `heal(amount)` (regeneration below max health).
    Heal(f32),
    /// The effect ran out (`onEffectsRemoved`).
    Removed(EffectInstance),
    /// `onEffectUpdated(effect, false)`: the periodic refresh every 600 ticks.
    Refresh(EffectInstance),
}

/// The active effects of a living entity (`activeEffects`), in insertion order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MobEffects {
    active: Vec<EffectInstance>,
}

/// What [`MobEffects::add`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Added {
    /// `canBeAffected` refused it.
    Refused,
    /// A new effect (`onEffectAdded`).
    New,
    /// An existing effect changed (`onEffectUpdated`).
    Updated,
    /// Nothing changed.
    Unchanged,
}

impl MobEffects {
    /// Whether no effect is active.
    pub fn is_empty(&self) -> bool {
        self.active.is_empty()
    }

    /// The active effects.
    pub fn iter(&self) -> impl Iterator<Item = &EffectInstance> {
        self.active.iter()
    }

    /// `getEffect(effect)`.
    pub fn get(&self, effect: u16) -> Option<&EffectInstance> {
        self.active.iter().find(|e| e.effect == effect)
    }

    /// `hasEffect(effect)`.
    pub fn has(&self, effect: u16) -> bool {
        self.get(effect).is_some()
    }

    /// The amplifier of an active effect.
    pub fn amplifier(&self, effect: u16) -> Option<i32> {
        self.get(effect).map(|e| e.amplifier)
    }

    /// `addEffect(newEffect)` after `canBeAffected` (`can_be_affected` from the caller).
    pub fn add(&mut self, new: EffectInstance, can_be_affected: bool) -> Added {
        if !can_be_affected {
            return Added::Refused;
        }
        match self.active.iter_mut().find(|e| e.effect == new.effect) {
            None => {
                self.active.push(new);
                Added::New
            }
            Some(e) => {
                if e.update(&new) {
                    Added::Updated
                } else {
                    Added::Unchanged
                }
            }
        }
    }

    /// `removeEffect(effect)`.
    pub fn remove(&mut self, effect: u16) -> Option<EffectInstance> {
        let i = self.active.iter().position(|e| e.effect == effect)?;
        Some(self.active.remove(i))
    }

    /// `removeAllEffects`.
    pub fn clear(&mut self) -> Vec<EffectInstance> {
        std::mem::take(&mut self.active)
    }

    /// `tickEffects` on the server: applies the effects that act this tick and counts the
    /// durations down. `health` and `max_health` are the entity's current values (the
    /// conditions of poison and regeneration); `tick_count` drives infinite effects.
    pub fn tick(
        &mut self,
        tick_count: i32,
        health: f32,
        max_health: f32,
        out: &mut Vec<EffectTick>,
    ) {
        if self.active.is_empty() {
            return;
        }
        let mut i = 0;
        while i < self.active.len() {
            let e = &mut self.active[i];
            if e.duration == 0 {
                out.push(EffectTick::Removed(self.active.remove(i)));
                continue;
            }
            let count = if e.is_infinite() {
                tick_count
            } else {
                e.duration
            };
            if let Some(interval) = tick_interval(e.effect, e.amplifier)
                && (interval <= 0 || count % interval == 0)
            {
                if e.effect == ids::regeneration() {
                    if health < max_health {
                        out.push(EffectTick::Heal(1.0));
                    }
                } else if e.effect == ids::poison() {
                    if health > 1.0 {
                        out.push(EffectTick::Magic(1.0));
                    }
                } else if e.effect == ids::wither() {
                    out.push(EffectTick::Wither(1.0));
                }
            }
            if !e.is_infinite() {
                e.duration -= 1;
            }
            if e.duration == 0 {
                out.push(EffectTick::Removed(self.active.remove(i)));
                continue;
            }
            if e.duration % 600 == 0 {
                out.push(EffectTick::Refresh(*e));
            }
            i += 1;
        }
    }

    /// The `active_effects` list, `None` when empty.
    pub fn save(&self) -> Option<Tag> {
        if self.active.is_empty() {
            return None;
        }
        Some(Tag::List(vanillite_nbt::List(
            self.active
                .iter()
                .filter_map(EffectInstance::save)
                .map(Tag::Compound)
                .collect(),
        )))
    }

    /// Reads an `active_effects` list.
    pub fn load(tag: Option<&Tag>) -> MobEffects {
        let mut effects = MobEffects::default();
        if let Some(Tag::List(list)) = tag {
            for t in &list.0 {
                if let Tag::Compound(c) = t
                    && let Some(e) = EffectInstance::load(c)
                {
                    effects.active.push(e);
                }
            }
        }
        effects
    }
}

/// `shouldApplyEffectTickThisTick`: the interval of the effects that act periodically.
fn tick_interval(effect: u16, amplifier: i32) -> Option<i32> {
    let base = if effect == ids::regeneration() {
        50
    } else if effect == ids::poison() {
        25
    } else if effect == ids::wither() {
        40
    } else {
        return None;
    };
    Some(if (0..32).contains(&amplifier) {
        base >> amplifier
    } else {
        0
    })
}

/// `remove_mob_effect` of `effect` for the entity with `network_id`.
pub fn remove_packet(network_id: i32, effect: u16) -> Arc<SharedPacket> {
    SharedPacket::of(&RemoveMobEffect {
        entity_id: network_id,
        effect: i32::from(effect),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wither_applies_every_40_ticks_and_expires() {
        let mut fx = MobEffects::default();
        assert_eq!(
            fx.add(EffectInstance::new(ids::wither(), 80, 0), true),
            Added::New
        );
        let mut out = Vec::new();
        for _ in 0..80 {
            fx.tick(0, 20.0, 20.0, &mut out);
        }
        let hurts = out
            .iter()
            .filter(|t| matches!(t, EffectTick::Wither(_)))
            .count();
        assert_eq!(hurts, 2);
        assert!(matches!(out.last(), Some(EffectTick::Removed(_))));
        assert!(fx.is_empty());
    }

    #[test]
    fn stronger_effect_replaces_weaker() {
        let mut fx = MobEffects::default();
        fx.add(EffectInstance::new(ids::mining_fatigue(), 6000, 0), true);
        assert_eq!(
            fx.add(EffectInstance::new(ids::mining_fatigue(), 6000, 2), true),
            Added::Updated
        );
        assert_eq!(fx.amplifier(ids::mining_fatigue()), Some(2));
        assert_eq!(
            fx.add(EffectInstance::new(ids::mining_fatigue(), 6000, 2), true),
            Added::Unchanged
        );
    }
}

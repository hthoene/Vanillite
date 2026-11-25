//! `MobEffectInstance`: one active effect with its chain of hidden (weaker, longer) effects.

use vanillite_nbt::{Compound, List, Tag};
use vanillite_protocol::codec::{Reader, write_varint};

use super::MobEffect;

/// `MobEffectInstance.INFINITE_DURATION`.
pub const INFINITE: i32 = -1;

/// `MobEffectInstance`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectInstance {
    pub effect: MobEffect,
    pub duration: i32,
    pub amplifier: u8,
    pub ambient: bool,
    /// `visible`: shows particles.
    pub visible: bool,
    pub show_icon: bool,
    /// `hiddenEffect`: what remains when this one runs out.
    pub hidden: Option<Box<EffectInstance>>,
}

impl EffectInstance {
    /// `new MobEffectInstance(effect, duration, amplifier)`: not ambient, visible, with icon.
    pub fn new(effect: MobEffect, duration: i32, amplifier: i32) -> EffectInstance {
        EffectInstance::with_flags(effect, duration, amplifier, false, true, true)
    }

    /// `new MobEffectInstance(effect, duration, amplifier, ambient, visible, showIcon)`; the
    /// amplifier is clamped to `0..=255`.
    pub fn with_flags(
        effect: MobEffect,
        duration: i32,
        amplifier: i32,
        ambient: bool,
        visible: bool,
        show_icon: bool,
    ) -> EffectInstance {
        EffectInstance {
            effect,
            duration,
            amplifier: u8::try_from(amplifier.clamp(0, 255)).unwrap_or(0),
            ambient,
            visible,
            show_icon,
            hidden: None,
        }
    }

    /// The amplifier as Vanilla's `int`.
    pub fn amplifier(&self) -> i32 {
        i32::from(self.amplifier)
    }

    /// `isInfiniteDuration`.
    pub fn is_infinite(&self) -> bool {
        self.duration == INFINITE
    }

    /// `endsWithin(ticks)`.
    pub fn ends_within(&self, ticks: i32) -> bool {
        !self.is_infinite() && self.duration <= ticks
    }

    /// `hasRemainingDuration`.
    pub fn has_remaining_duration(&self) -> bool {
        self.is_infinite() || self.duration > 0
    }

    fn is_shorter_than(&self, other: &EffectInstance) -> bool {
        !self.is_infinite() && (self.duration < other.duration || other.is_infinite())
    }

    /// A copy without the hidden chain (`new MobEffectInstance(copy)`).
    fn copy_details(&self) -> EffectInstance {
        EffectInstance {
            hidden: None,
            ..self.clone()
        }
    }

    fn set_details_from(&mut self, other: &EffectInstance) {
        self.duration = other.duration;
        self.amplifier = other.amplifier;
        self.ambient = other.ambient;
        self.visible = other.visible;
        self.show_icon = other.show_icon;
    }

    /// `update(takeOver)`: merges a new instance of the same effect; returns whether the
    /// visible state changed (amplifier, duration or flags).
    pub fn update(&mut self, take_over: &EffectInstance) -> bool {
        let mut changed = false;
        if take_over.amplifier > self.amplifier {
            if take_over.is_shorter_than(self) {
                let previous = self.hidden.take();
                let mut hidden = self.copy_details();
                hidden.hidden = previous;
                self.hidden = Some(Box::new(hidden));
            }
            self.amplifier = take_over.amplifier;
            self.duration = take_over.duration;
            changed = true;
        } else if self.is_shorter_than(take_over) {
            if take_over.amplifier == self.amplifier {
                self.duration = take_over.duration;
                changed = true;
            } else if let Some(hidden) = &mut self.hidden {
                hidden.update(take_over);
            } else {
                self.hidden = Some(Box::new(take_over.copy_details()));
            }
        }
        if (!take_over.ambient && self.ambient) || changed {
            self.ambient = take_over.ambient;
            changed = true;
        }
        if take_over.visible != self.visible {
            self.visible = take_over.visible;
            changed = true;
        }
        if take_over.show_icon != self.show_icon {
            self.show_icon = take_over.show_icon;
            changed = true;
        }
        changed
    }

    /// `mapDuration`: infinite and zero durations stay.
    pub fn map_duration(&self, f: impl FnOnce(i32) -> i32) -> i32 {
        if self.is_infinite() || self.duration == 0 {
            self.duration
        } else {
            f(self.duration)
        }
    }

    /// `withScaledDuration(scale)`: at least one tick.
    pub fn with_scaled_duration(&self, scale: f32) -> EffectInstance {
        let mut copy = self.copy_details();
        #[allow(clippy::cast_possible_truncation, clippy::as_conversions)]
        let d = self.map_duration(|d| ((d as f32 * scale).floor() as i32).max(1));
        copy.duration = d;
        copy
    }

    /// `tickDownDuration`, with the hidden chain.
    pub fn tick_down(&mut self) {
        if let Some(hidden) = &mut self.hidden {
            hidden.tick_down();
        }
        self.duration = self.map_duration(|d| d.wrapping_sub(1));
    }

    /// `downgradeToHiddenEffect`: a run-out effect takes over its hidden one.
    pub fn downgrade_to_hidden(&mut self) -> bool {
        if self.duration != 0 {
            return false;
        }
        let Some(hidden) = self.hidden.take() else {
            return false;
        };
        let hidden = *hidden;
        self.set_details_from(&hidden);
        self.hidden = hidden.hidden;
        true
    }

    /// `MobEffectInstance.CODEC` as NBT: `id` plus the details.
    pub fn to_nbt(&self) -> Compound {
        let mut c = Compound::new();
        c.insert("id", self.effect.key());
        self.details_to_nbt(&mut c);
        c
    }

    fn details_to_nbt(&self, c: &mut Compound) {
        // `optionalFieldOf` writes nothing for the default values.
        if self.amplifier != 0 {
            c.insert("amplifier", i8::from_ne_bytes([self.amplifier]));
        }
        if self.duration != 0 {
            c.insert("duration", self.duration);
        }
        if self.ambient {
            c.insert("ambient", true);
        }
        if !self.visible {
            c.insert("show_particles", false);
        }
        c.insert("show_icon", self.show_icon);
        if let Some(hidden) = &self.hidden {
            let mut h = Compound::new();
            hidden.details_to_nbt(&mut h);
            c.insert("hidden_effect", h);
        }
    }

    /// An instance from its NBT; `None` for an unknown effect id.
    pub fn from_nbt(c: &Compound) -> Option<EffectInstance> {
        let effect = MobEffect::by_name(c.get("id")?.as_str()?)?;
        Some(EffectInstance::details_from_nbt(effect, c, 0))
    }

    fn details_from_nbt(effect: MobEffect, c: &Compound, depth: u32) -> EffectInstance {
        let int = |key: &str| c.get(key).and_then(Tag::as_i64);
        let flag = |key: &str| int(key).map(|v| v != 0);
        // `ExtraCodecs.UNSIGNED_BYTE`: the byte read as `0..=255`.
        let amplifier = int("amplifier").map_or(0, |v| v & 0xFF);
        let visible = flag("show_particles").unwrap_or(true);
        let mut instance = EffectInstance::with_flags(
            effect,
            int("duration")
                .and_then(|v| i32::try_from(v).ok())
                .unwrap_or(0),
            i32::try_from(amplifier).unwrap_or(0),
            flag("ambient").unwrap_or(false),
            visible,
            flag("show_icon").unwrap_or(visible),
        );
        // The hidden chain is bounded: a crafted file cannot recurse without end.
        if depth < 16
            && let Some(Tag::Compound(h)) = c.get("hidden_effect")
        {
            instance.hidden = Some(Box::new(EffectInstance::details_from_nbt(
                effect,
                h,
                depth + 1,
            )));
        }
        instance
    }

    /// `MobEffectInstance.STREAM_CODEC`: effect id, then the details with the hidden chain.
    pub fn encode(&self, out: &mut Vec<u8>) {
        write_varint(out, i32::from(self.effect.0));
        self.encode_details(out);
    }

    fn encode_details(&self, out: &mut Vec<u8>) {
        write_varint(out, self.amplifier());
        write_varint(out, self.duration);
        out.push(u8::from(self.ambient));
        out.push(u8::from(self.visible));
        out.push(u8::from(self.show_icon));
        match &self.hidden {
            Some(h) => {
                out.push(1);
                h.encode_details(out);
            }
            None => out.push(0),
        }
    }

    /// Reads a wire instance (component values were validated when they came in).
    pub fn decode(r: &mut Reader<'_>) -> Option<EffectInstance> {
        let effect = MobEffect::from_id(r.varint().ok()?)?;
        EffectInstance::decode_details(effect, r, 0)
    }

    fn decode_details(effect: MobEffect, r: &mut Reader<'_>, depth: u32) -> Option<EffectInstance> {
        let amplifier = r.varint().ok()?;
        let duration = r.varint().ok()?;
        let ambient = r.u8().ok()? != 0;
        let visible = r.u8().ok()? != 0;
        let show_icon = r.u8().ok()? != 0;
        let mut instance =
            EffectInstance::with_flags(effect, duration, amplifier, ambient, visible, show_icon);
        if r.u8().ok()? != 0 {
            if depth >= 16 {
                return None;
            }
            instance.hidden = Some(Box::new(EffectInstance::decode_details(
                effect,
                r,
                depth + 1,
            )?));
        }
        Some(instance)
    }
}

/// The `active_effects` list of an entity.
pub fn list_to_nbt<'a>(effects: impl Iterator<Item = &'a EffectInstance>) -> Tag {
    Tag::List(List(effects.map(|e| Tag::Compound(e.to_nbt())).collect()))
}

/// The instances of an `active_effects` list; unknown effects are dropped.
pub fn list_from_nbt(tag: Option<&Tag>) -> Vec<EffectInstance> {
    let Some(Tag::List(list)) = tag else {
        return Vec::new();
    };
    list.0
        .iter()
        .filter_map(Tag::as_compound)
        .filter_map(EffectInstance::from_nbt)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stronger_shorter_effect_hides_the_weaker_one() {
        let mut speed = EffectInstance::new(MobEffect::SPEED, 600, 0);
        assert!(speed.update(&EffectInstance::new(MobEffect::SPEED, 100, 1)));
        assert_eq!((speed.amplifier, speed.duration), (1, 100));
        let hidden = speed.hidden.as_deref().cloned();
        assert_eq!(hidden.map(|h| (h.amplifier, h.duration)), Some((0, 600)));
        for _ in 0..100 {
            speed.tick_down();
            speed.downgrade_to_hidden();
        }
        assert_eq!((speed.amplifier, speed.duration), (0, 500));
        assert!(speed.hidden.is_none());
    }

    #[test]
    fn weaker_longer_effect_becomes_hidden() {
        let mut regen = EffectInstance::new(MobEffect::REGENERATION, 100, 1);
        assert!(!regen.update(&EffectInstance::new(MobEffect::REGENERATION, 900, 0)));
        assert_eq!(regen.hidden.as_ref().map(|h| h.duration), Some(900));
        assert!(regen.update(&EffectInstance::new(MobEffect::REGENERATION, 200, 1)));
        assert_eq!(regen.duration, 200);
    }

    #[test]
    fn nbt_and_wire_round_trip() {
        let mut e = EffectInstance::with_flags(MobEffect::POISON, 400, 200, true, false, true);
        e.hidden = Some(Box::new(EffectInstance::new(MobEffect::POISON, 900, 0)));
        let back = EffectInstance::from_nbt(&e.to_nbt());
        assert_eq!(back.as_ref(), Some(&e));
        let mut wire = Vec::new();
        e.encode(&mut wire);
        let mut r = Reader::new(&wire);
        assert_eq!(EffectInstance::decode(&mut r), Some(e));
    }

    #[test]
    fn infinite_effects_never_count_down() {
        let mut e = EffectInstance::new(MobEffect::NIGHT_VISION, INFINITE, 0);
        e.tick_down();
        assert!(e.is_infinite());
        assert_eq!(e.with_scaled_duration(0.25).duration, INFINITE);
        let short = EffectInstance::new(MobEffect::POISON, 3, 0).with_scaled_duration(0.1);
        assert_eq!(short.duration, 1);
    }
}

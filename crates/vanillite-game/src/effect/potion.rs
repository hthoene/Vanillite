//! Potions (`Potions`, the `minecraft:potion` registry) and the item components that carry
//! effects: `potion_contents`, `potion_duration_scale` and `suspicious_stew_effects`.

use vanillite_protocol::codec::{Reader, write_varint};
use vanillite_protocol::components::ids;

use super::{EffectInstance, MobEffect};
use crate::item::ItemStack;

/// `PotionContents.BASE_POTION_COLOR`.
pub const BASE_COLOR: i32 = -13_083_194;

type E = (MobEffect, i32, i32);

/// `Potions` in registry order: the effects (effect, duration, amplifier) of each potion.
static POTIONS: [&[E]; 46] = {
    use MobEffect as M;
    [
        &[],                                               // water
        &[],                                               // mundane
        &[],                                               // thick
        &[],                                               // awkward
        &[(M::NIGHT_VISION, 3600, 0)],                     // night_vision
        &[(M::NIGHT_VISION, 9600, 0)],                     // long_night_vision
        &[(M::INVISIBILITY, 3600, 0)],                     // invisibility
        &[(M::INVISIBILITY, 9600, 0)],                     // long_invisibility
        &[(M::JUMP_BOOST, 3600, 0)],                       // leaping
        &[(M::JUMP_BOOST, 9600, 0)],                       // long_leaping
        &[(M::JUMP_BOOST, 1800, 1)],                       // strong_leaping
        &[(M::FIRE_RESISTANCE, 3600, 0)],                  // fire_resistance
        &[(M::FIRE_RESISTANCE, 9600, 0)],                  // long_fire_resistance
        &[(M::SPEED, 3600, 0)],                            // swiftness
        &[(M::SPEED, 9600, 0)],                            // long_swiftness
        &[(M::SPEED, 1800, 1)],                            // strong_swiftness
        &[(M::SLOWNESS, 1800, 0)],                         // slowness
        &[(M::SLOWNESS, 4800, 0)],                         // long_slowness
        &[(M::SLOWNESS, 400, 3)],                          // strong_slowness
        &[(M::SLOWNESS, 400, 3), (M::RESISTANCE, 400, 2)], // turtle_master
        &[(M::SLOWNESS, 800, 3), (M::RESISTANCE, 800, 2)], // long_turtle_master
        &[(M::SLOWNESS, 400, 5), (M::RESISTANCE, 400, 3)], // strong_turtle_master
        &[(M::WATER_BREATHING, 3600, 0)],                  // water_breathing
        &[(M::WATER_BREATHING, 9600, 0)],                  // long_water_breathing
        &[(M::INSTANT_HEALTH, 1, 0)],                      // healing
        &[(M::INSTANT_HEALTH, 1, 1)],                      // strong_healing
        &[(M::INSTANT_DAMAGE, 1, 0)],                      // harming
        &[(M::INSTANT_DAMAGE, 1, 1)],                      // strong_harming
        &[(M::POISON, 900, 0)],                            // poison
        &[(M::POISON, 1800, 0)],                           // long_poison
        &[(M::POISON, 432, 1)],                            // strong_poison
        &[(M::REGENERATION, 900, 0)],                      // regeneration
        &[(M::REGENERATION, 1800, 0)],                     // long_regeneration
        &[(M::REGENERATION, 450, 1)],                      // strong_regeneration
        &[(M::STRENGTH, 3600, 0)],                         // strength
        &[(M::STRENGTH, 9600, 0)],                         // long_strength
        &[(M::STRENGTH, 1800, 1)],                         // strong_strength
        &[(M::WEAKNESS, 1800, 0)],                         // weakness
        &[(M::WEAKNESS, 4800, 0)],                         // long_weakness
        &[(M::LUCK, 6000, 0)],                             // luck
        &[(M::SLOW_FALLING, 1800, 0)],                     // slow_falling
        &[(M::SLOW_FALLING, 4800, 0)],                     // long_slow_falling
        &[(M::WIND_CHARGED, 3600, 0)],                     // wind_charged
        &[(M::WEAVING, 3600, 0)],                          // weaving
        &[(M::OOZING, 3600, 0)],                           // oozing
        &[(M::INFESTED, 3600, 0)],                         // infested
    ]
};

/// The effects of the potion with registry id `id`.
pub fn potion_effects(id: u16) -> Vec<EffectInstance> {
    POTIONS
        .get(usize::from(id))
        .map(|list| {
            list.iter()
                .map(|&(e, d, a)| EffectInstance::new(e, d, a))
                .collect()
        })
        .unwrap_or_default()
}

/// `PotionContents`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PotionContents {
    pub potion: Option<u16>,
    pub custom_color: Option<i32>,
    pub custom_effects: Vec<EffectInstance>,
    pub custom_name: Option<String>,
}

impl PotionContents {
    /// The contents of a potion.
    pub fn of(potion: u16) -> PotionContents {
        PotionContents {
            potion: Some(potion),
            ..PotionContents::default()
        }
    }

    /// A stack's `potion_contents` (`EMPTY` without the component).
    pub fn of_stack(stack: &ItemStack) -> PotionContents {
        stack
            .component(ids::POTION_CONTENTS)
            .and_then(PotionContents::decode)
            .unwrap_or_default()
    }

    /// `PotionContents.CODEC` as NBT (the `potion_contents` of a saved cloud).
    pub fn to_nbt(&self) -> vanillite_nbt::Compound {
        use vanillite_nbt::Tag;
        let mut c = vanillite_nbt::Compound::new();
        if let Some(name) = self
            .potion
            .and_then(|p| vanillite_data::registry::POTION.name(p))
        {
            c.insert("potion", Tag::String(name.into()));
        }
        if let Some(color) = self.custom_color {
            c.insert("custom_color", Tag::Int(color));
        }
        if !self.custom_effects.is_empty() {
            c.insert(
                "custom_effects",
                super::instance::list_to_nbt(self.custom_effects.iter()),
            );
        }
        if let Some(name) = &self.custom_name {
            c.insert("custom_name", Tag::String(name.clone()));
        }
        c
    }

    /// Reads `PotionContents.CODEC` NBT; unknown potions and effects are dropped.
    pub fn from_nbt(c: &vanillite_nbt::Compound) -> PotionContents {
        use vanillite_nbt::Tag;
        let potion = match c.get("potion") {
            Some(Tag::String(name)) => vanillite_data::registry::POTION.id(name),
            _ => None,
        };
        let custom_color = match c.get("custom_color") {
            Some(Tag::Int(color)) => Some(*color),
            _ => None,
        };
        let custom_name = match c.get("custom_name") {
            Some(Tag::String(name)) => Some(name.clone()),
            _ => None,
        };
        PotionContents {
            potion,
            custom_color,
            custom_effects: super::instance::list_from_nbt(c.get("custom_effects")),
            custom_name,
        }
    }

    /// `PotionContents.STREAM_CODEC` from component bytes.
    pub fn decode(bytes: &[u8]) -> Option<PotionContents> {
        let mut r = Reader::new(bytes);
        let potion = if r.u8().ok()? != 0 {
            Some(u16::try_from(r.varint().ok()?).ok()?)
        } else {
            None
        };
        let custom_color = if r.u8().ok()? != 0 {
            Some(r.i32().ok()?)
        } else {
            None
        };
        let count = usize::try_from(r.varint().ok()?).ok()?;
        let mut custom_effects = Vec::with_capacity(count.min(64));
        for _ in 0..count {
            custom_effects.push(EffectInstance::decode(&mut r)?);
        }
        let custom_name = if r.u8().ok()? != 0 {
            Some(r.string(32_767).ok()?.to_owned())
        } else {
            None
        };
        Some(PotionContents {
            potion,
            custom_color,
            custom_effects,
            custom_name,
        })
    }

    /// `PotionContents.STREAM_CODEC` as component bytes.
    pub fn encode(&self) -> Box<[u8]> {
        let mut out = Vec::with_capacity(8);
        match self.potion {
            Some(p) => {
                out.push(1);
                write_varint(&mut out, i32::from(p));
            }
            None => out.push(0),
        }
        match self.custom_color {
            Some(c) => {
                out.push(1);
                out.extend_from_slice(&c.to_be_bytes());
            }
            None => out.push(0),
        }
        write_varint(
            &mut out,
            i32::try_from(self.custom_effects.len()).unwrap_or(0),
        );
        for e in &self.custom_effects {
            e.encode(&mut out);
        }
        match &self.custom_name {
            Some(n) => {
                out.push(1);
                write_varint(&mut out, i32::try_from(n.len()).unwrap_or(0));
                out.extend_from_slice(n.as_bytes());
            }
            None => out.push(0),
        }
        out.into_boxed_slice()
    }

    /// `getAllEffects`: the potion's effects, then the custom ones.
    pub fn all_effects(&self) -> Vec<EffectInstance> {
        let mut out = self.potion.map(potion_effects).unwrap_or_default();
        out.extend(self.custom_effects.iter().cloned());
        out
    }

    /// `forEachEffect(consumer, durationScale)`.
    pub fn scaled_effects(&self, scale: f32) -> Vec<EffectInstance> {
        self.all_effects()
            .iter()
            .map(|e| e.with_scaled_duration(scale))
            .collect()
    }

    /// `hasEffects`.
    pub fn has_effects(&self) -> bool {
        !self.custom_effects.is_empty()
            || self
                .potion
                .is_some_and(|p| POTIONS.get(usize::from(p)).is_some_and(|l| !l.is_empty()))
    }

    /// `getColor()`: the custom colour, the mix of the visible effects or the base colour.
    pub fn color(&self) -> i32 {
        self.color_or(BASE_COLOR)
    }

    /// `getColorOr(default)`.
    pub fn color_or(&self, default: i32) -> i32 {
        if let Some(c) = self.custom_color {
            return c;
        }
        color_of(&self.all_effects()).unwrap_or(default)
    }

    /// Whether this is the water potion without custom effects (`is(Potions.WATER)`).
    pub fn is_water(&self) -> bool {
        self.potion == water_id() && self.custom_effects.is_empty()
    }
}

/// The registry id of `minecraft:water`.
pub fn water_id() -> Option<u16> {
    vanillite_data::registry::POTION.id("minecraft:water")
}

/// `PotionContents.getColorOptional`: the amplifier-weighted mix of the visible effects.
pub fn color_of(effects: &[EffectInstance]) -> Option<i32> {
    let (mut r, mut g, mut b, mut total) = (0_i32, 0_i32, 0_i32, 0_i32);
    for e in effects.iter().filter(|e| e.visible) {
        let color = e.effect.info().color;
        let weight = e.amplifier() + 1;
        r = r.wrapping_add(weight.wrapping_mul((color >> 16) & 0xFF));
        g = g.wrapping_add(weight.wrapping_mul((color >> 8) & 0xFF));
        b = b.wrapping_add(weight.wrapping_mul(color & 0xFF));
        total = total.wrapping_add(weight);
    }
    if total == 0 {
        return None;
    }
    // `ARGB.color(r, g, b)`: opaque.
    let argb = (0xFF_u32 << 24)
        | (u32::from_ne_bytes((r / total).to_ne_bytes()) & 0xFF) << 16
        | (u32::from_ne_bytes((g / total).to_ne_bytes()) & 0xFF) << 8
        | (u32::from_ne_bytes((b / total).to_ne_bytes()) & 0xFF);
    Some(i32::from_ne_bytes(argb.to_ne_bytes()))
}

/// `potion_duration_scale` of a stack: the component, else the item default (0.25 for
/// lingering potions, 0.125 for tipped arrows, 1 otherwise).
pub fn duration_scale(stack: &ItemStack) -> f32 {
    if let Some(bytes) = stack
        .component(ids::POTION_DURATION_SCALE)
        .and_then(|b| b.get(..4))
        .and_then(|b| <[u8; 4]>::try_from(b).ok())
    {
        return f32::from_be_bytes(bytes);
    }
    match vanillite_data::registry::ITEM.name(stack.item) {
        Some("minecraft:lingering_potion") => 0.25,
        Some("minecraft:tipped_arrow") => 0.125,
        _ => 1.0,
    }
}

/// `suspicious_stew_effects`: `(effect, duration)` entries; `SuspiciousStewEffects.Entry
/// .createEffectInstance` gives amplifier 0.
pub fn stew_effects(stack: &ItemStack) -> Vec<EffectInstance> {
    let Some(bytes) = stack.component(ids::SUSPICIOUS_STEW_EFFECTS) else {
        return Vec::new();
    };
    let mut r = Reader::new(bytes);
    let Some(count) = r.varint().ok().and_then(|c| usize::try_from(c).ok()) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(count.min(64));
    for _ in 0..count {
        let (Ok(effect), Ok(duration)) = (r.varint(), r.varint()) else {
            break;
        };
        if let Some(effect) = MobEffect::from_id(effect) {
            out.push(EffectInstance::new(effect, duration, 0));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_follows_the_registry() {
        assert_eq!(vanillite_data::registry::POTION.len(), POTIONS.len());
        let id = |n| vanillite_data::registry::POTION.id(n).unwrap_or(0);
        assert_eq!(
            potion_effects(id("minecraft:strong_turtle_master")),
            vec![
                EffectInstance::new(MobEffect::SLOWNESS, 400, 5),
                EffectInstance::new(MobEffect::RESISTANCE, 400, 3)
            ]
        );
        assert_eq!(
            potion_effects(id("minecraft:infested")),
            vec![EffectInstance::new(MobEffect::INFESTED, 3600, 0)]
        );
    }

    #[test]
    fn contents_round_trip_and_colour() {
        let mut c = PotionContents::of(31);
        c.custom_effects
            .push(EffectInstance::new(MobEffect::SPEED, 100, 1));
        c.custom_name = Some("x".into());
        assert_eq!(PotionContents::decode(&c.encode()), Some(c.clone()));
        // Regeneration (13458603) once, speed (3402751) twice.
        let expected = {
            let mix = |a: i32, b: i32| (a + 2 * b) / 3;
            let (r, g, b) = (
                mix(13_458_603 >> 16 & 0xFF, 3_402_751 >> 16 & 0xFF),
                mix(13_458_603 >> 8 & 0xFF, 3_402_751 >> 8 & 0xFF),
                mix(13_458_603 & 0xFF, 3_402_751 & 0xFF),
            );
            (-16_777_216) | r << 16 | g << 8 | b
        };
        assert_eq!(c.color(), expected);
        assert_eq!(PotionContents::default().color(), BASE_COLOR);
    }
}

//! The item components of eating and drinking: `consumable` (`Consumable`), `food`
//! (`FoodProperties`) and `use_remainder` (`UseRemainder`), read from the stack's patch or,
//! without one, from the item's default components in the generated item data.
//!
//! What consuming does is [`crate::player::using`] for players and
//! [`crate::effect::consume`] for the status effects.

use std::borrow::Cow;

use vanillite_data::item::ConsumeEffectInfo;
use vanillite_protocol::codec::Reader;
use vanillite_protocol::components::ids;

use crate::effect::{EffectInstance, MobEffect};
use crate::item::{self, ItemStack};
use crate::survival::food::FoodProperties;

/// A sound event of a component: a registered name or an inline one.
pub type Sound = Cow<'static, str>;

/// One `ConsumeEffect`.
#[derive(Clone, Debug, PartialEq)]
pub enum ConsumeEffect {
    /// `apply_effects` with its probability.
    Apply(Vec<EffectInstance>, f32),
    /// `remove_effects`.
    Remove(Vec<MobEffect>),
    /// `clear_all_effects`.
    ClearAll,
    /// `teleport_randomly` with its diameter and `directional_particles`.
    Teleport(f32, bool),
    /// `play_sound`.
    PlaySound(Sound),
}

/// The `consumable` component.
#[derive(Clone, Debug, PartialEq)]
pub struct Consumable {
    pub consume_seconds: f32,
    /// `animation` is `drink` (the drink game event and the fixed volume).
    pub drink: bool,
    pub sound: Sound,
    pub has_consume_particles: bool,
    pub effects: Vec<ConsumeEffect>,
}

impl Consumable {
    /// `consumeTicks`.
    #[allow(clippy::cast_possible_truncation)]
    pub fn consume_ticks(&self) -> i32 {
        (self.consume_seconds * 20.0) as i32
    }

    /// `shouldEmitParticlesAndSounds(useItemRemainingTicks)`.
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    pub fn should_emit(&self, remaining: i32) -> bool {
        let total = self.consume_ticks();
        let wait = (total as f32 * 0.218_75) as i32;
        total.saturating_sub(remaining) > wait && remaining % 4 == 0
    }
}

/// The value of a component in the stack's patch.
pub(crate) enum Patched<'a> {
    Value(&'a [u8]),
    /// The patch removes the item's default.
    Removed,
    /// The patch does not touch it: the item's default counts.
    Default,
}

/// The patched value of the component `id`.
pub(crate) fn patched(stack: &ItemStack, id: u16) -> Patched<'_> {
    match stack.patch() {
        Some(p) => match p.get(id) {
            Some(v) => Patched::Value(v),
            None if p.is_removed(id) => Patched::Removed,
            None => Patched::Default,
        },
        None => Patched::Default,
    }
}

pub(crate) fn read_bool(r: &mut Reader<'_>) -> Option<bool> {
    r.u8().ok().map(|b| b != 0)
}

/// `SoundEvent.STREAM_CODEC` (a holder: registry id + 1, or 0 and the inline event).
pub(crate) fn read_sound(r: &mut Reader<'_>) -> Option<Sound> {
    let id = r.varint().ok()?;
    if id == 0 {
        let name = r.string(32767).ok()?.to_owned();
        if read_bool(r)? {
            r.f32().ok()?;
        }
        return Some(Cow::Owned(name));
    }
    let id = u16::try_from(id - 1).ok()?;
    vanillite_data::registry::SOUND_EVENT
        .name(id)
        .map(Cow::Borrowed)
}

pub(crate) fn read_optional_sound(r: &mut Reader<'_>) -> Option<Option<Sound>> {
    if read_bool(r)? {
        read_sound(r).map(Some)
    } else {
        Some(None)
    }
}

/// What `ByteBufCodecs.holderSet` holds.
pub(crate) enum HolderSet {
    /// A tag id (without `#`).
    Tag(String),
    /// Registry ids.
    Ids(Vec<u16>),
}

/// `ByteBufCodecs.holderSet`: 0 and a tag id, or the count + 1 and the ids.
pub(crate) fn read_holder_set(r: &mut Reader<'_>) -> Option<HolderSet> {
    let n = r.varint().ok()?;
    if n == 0 {
        return Some(HolderSet::Tag(r.string(32767).ok()?.to_owned()));
    }
    let n = usize::try_from(n - 1).ok()?;
    let mut ids = Vec::with_capacity(n.min(64));
    for _ in 0..n {
        ids.push(u16::try_from(r.varint().ok()?).ok()?);
    }
    Some(HolderSet::Ids(ids))
}

/// `ConsumeEffect.STREAM_CODEC` as a list; `None` when the bytes do not parse.
pub fn read_effects(r: &mut Reader<'_>) -> Option<Vec<ConsumeEffect>> {
    let count = usize::try_from(r.varint().ok()?).ok()?;
    let registry = &vanillite_data::registry::CONSUME_EFFECT_TYPE;
    let mut out = Vec::with_capacity(count.min(16));
    for _ in 0..count {
        let kind = registry.name(u16::try_from(r.varint().ok()?).ok()?)?;
        out.push(match kind {
            "minecraft:apply_effects" => {
                let n = usize::try_from(r.varint().ok()?).ok()?;
                let mut list = Vec::with_capacity(n.min(64));
                for _ in 0..n {
                    list.push(EffectInstance::decode(r)?);
                }
                ConsumeEffect::Apply(list, r.f32().ok()?)
            }
            "minecraft:remove_effects" => ConsumeEffect::Remove(match read_holder_set(r)? {
                HolderSet::Ids(ids) => ids
                    .into_iter()
                    .filter_map(|id| MobEffect::from_id(i32::from(id)))
                    .collect(),
                // There are no mob effect tags in the data the game keeps.
                HolderSet::Tag(_) => Vec::new(),
            }),
            "minecraft:clear_all_effects" => ConsumeEffect::ClearAll,
            "minecraft:teleport_randomly" => ConsumeEffect::Teleport(r.f32().ok()?, read_bool(r)?),
            "minecraft:play_sound" => ConsumeEffect::PlaySound(read_sound(r)?),
            _ => return None,
        });
    }
    Some(out)
}

/// The consume effects of an item's default component.
pub(crate) fn default_effects(effects: &[ConsumeEffectInfo]) -> Vec<ConsumeEffect> {
    effects
        .iter()
        .map(|e| match *e {
            ConsumeEffectInfo::ApplyEffects {
                effects,
                probability,
            } => ConsumeEffect::Apply(
                effects
                    .iter()
                    .filter_map(|i| {
                        Some(EffectInstance::with_flags(
                            MobEffect::by_name(i.id)?,
                            i.duration,
                            i.amplifier,
                            i.ambient,
                            i.show_particles,
                            i.show_icon,
                        ))
                    })
                    .collect(),
                probability,
            ),
            ConsumeEffectInfo::RemoveEffects(list) => ConsumeEffect::Remove(
                list.iter()
                    .filter_map(|name| MobEffect::by_name(name))
                    .collect(),
            ),
            ConsumeEffectInfo::ClearAllEffects => ConsumeEffect::ClearAll,
            ConsumeEffectInfo::TeleportRandomly(diameter) => {
                ConsumeEffect::Teleport(diameter, true)
            }
            ConsumeEffectInfo::PlaySound(sound) => ConsumeEffect::PlaySound(Cow::Borrowed(sound)),
        })
        .collect()
}

/// `Consumable.STREAM_CODEC`.
fn decode_consumable(v: &[u8]) -> Option<Consumable> {
    let mut r = Reader::new(v);
    Some(Consumable {
        consume_seconds: r.f32().ok()?,
        // `ItemUseAnimation`: 2 is `drink`.
        drink: r.varint().ok()? == 2,
        sound: read_sound(&mut r)?,
        has_consume_particles: read_bool(&mut r)?,
        effects: read_effects(&mut r)?,
    })
}

/// The stack's `consumable` component.
pub fn consumable(stack: &ItemStack) -> Option<Consumable> {
    if stack.is_empty() {
        return None;
    }
    match patched(stack, ids::CONSUMABLE) {
        Patched::Value(v) => decode_consumable(v),
        Patched::Removed => None,
        Patched::Default => {
            let c = item::item_id(stack).info().consumable?;
            Some(Consumable {
                consume_seconds: c.consume_seconds,
                drink: c.animation == "drink",
                sound: Cow::Borrowed(c.sound),
                has_consume_particles: c.has_consume_particles,
                effects: default_effects(c.on_consume_effects),
            })
        }
    }
}

/// Whether the stack has a `consumable` component.
pub fn is_consumable(stack: &ItemStack) -> bool {
    if stack.is_empty() {
        return false;
    }
    match patched(stack, ids::CONSUMABLE) {
        Patched::Value(_) => true,
        Patched::Removed => false,
        Patched::Default => item::item_id(stack).info().consumable.is_some(),
    }
}

/// The stack's `food` component.
pub fn food(stack: &ItemStack) -> Option<FoodProperties> {
    if stack.is_empty() {
        return None;
    }
    match patched(stack, ids::FOOD) {
        Patched::Value(v) => {
            let mut r = Reader::new(v);
            Some(FoodProperties {
                nutrition: r.varint().ok()?,
                saturation: r.f32().ok()?,
                can_always_eat: read_bool(&mut r)?,
            })
        }
        Patched::Removed => None,
        Patched::Default => item::item_id(stack).info().food.map(|f| FoodProperties {
            nutrition: f.nutrition,
            saturation: f.saturation,
            can_always_eat: f.can_always_eat,
        }),
    }
}

/// The stack of the `use_remainder` component (`UseRemainder.convertInto.create()`).
pub fn use_remainder(stack: &ItemStack) -> Option<ItemStack> {
    if stack.is_empty() {
        return None;
    }
    match patched(stack, ids::USE_REMAINDER) {
        Patched::Value(v) => ItemStack::decode_template(&mut Reader::new(v)).ok(),
        Patched::Removed => None,
        Patched::Default => {
            let name = item::item_id(stack).info().use_remainder;
            if name.is_empty() {
                return None;
            }
            vanillite_data::registry::ITEM
                .id(name)
                .map(|id| ItemStack::new(id, 1))
        }
    }
}

/// `ItemStack.causeUseVibration`: `use_effects.interact_vibrations` of the stack.
pub fn use_vibrations(stack: &ItemStack) -> bool {
    match patched(stack, ids::USE_EFFECTS) {
        // `UseEffects.STREAM_CODEC`: can sprint, interact vibrations, speed multiplier.
        Patched::Value(v) => v.get(1).is_some_and(|b| *b != 0),
        Patched::Removed => false,
        Patched::Default => item::item_id(stack).info().use_vibrations,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stack(name: &str) -> ItemStack {
        ItemStack::new(vanillite_data::registry::ITEM.id(name).unwrap_or(0), 1)
    }

    #[test]
    fn defaults_come_from_the_item_data() {
        let apple = consumable(&stack("minecraft:apple")).expect("apple");
        assert_eq!(apple.consume_ticks(), 32);
        assert!(!apple.drink && apple.has_consume_particles);
        assert_eq!(apple.sound, "minecraft:entity.generic.eat");
        assert_eq!(
            consumable(&stack("minecraft:dried_kelp")).map(|c| c.consume_ticks()),
            Some(16)
        );
        let honey = consumable(&stack("minecraft:honey_bottle")).expect("honey");
        assert_eq!(honey.consume_ticks(), 40);
        assert!(honey.drink);
        assert_eq!(
            honey.effects,
            vec![ConsumeEffect::Remove(vec![MobEffect::POISON])]
        );
        assert_eq!(
            consumable(&stack("minecraft:chorus_fruit")).map(|c| c.effects),
            Some(vec![ConsumeEffect::Teleport(16.0, true)])
        );
        // The fish buckets carry `food` without `consumable`.
        assert!(consumable(&stack("minecraft:cod_bucket")).is_none());
        assert!(food(&stack("minecraft:cod_bucket")).is_some());
        assert!(consumable(&stack("minecraft:stick")).is_none());
        assert_eq!(
            use_remainder(&stack("minecraft:mushroom_stew")).map(|s| s.item),
            vanillite_data::registry::ITEM.id("minecraft:bowl")
        );
        assert!(use_remainder(&stack("minecraft:apple")).is_none());
        assert!(use_vibrations(&stack("minecraft:apple")));
        assert!(!use_vibrations(&stack("minecraft:iron_spear")));
    }

    #[test]
    fn the_patch_wins() {
        let mut s = stack("minecraft:stick");
        // 0.5 s, drink, a registered sound, no particles, one `clear_all_effects`.
        let mut v = 0.5_f32.to_be_bytes().to_vec();
        let clear = vanillite_data::registry::CONSUME_EFFECT_TYPE
            .id("minecraft:clear_all_effects")
            .unwrap_or(0);
        v.extend_from_slice(&[2, 1, 0, 1, u8::try_from(clear).unwrap_or(0)]);
        s.patch_mut().set(ids::CONSUMABLE, v.into());
        let c = consumable(&s).expect("patched");
        assert_eq!(c.consume_ticks(), 10);
        assert!(c.drink && !c.has_consume_particles);
        assert_eq!(c.effects, vec![ConsumeEffect::ClearAll]);
    }

    #[test]
    fn teleport_randomly_reads_its_particles_flag() {
        let reg = &vanillite_data::registry::CONSUME_EFFECT_TYPE;
        let id = |n: &str| u8::try_from(reg.id(n).unwrap_or(0)).unwrap_or(0);
        // `teleport_randomly` (8.0, no particles) followed by `clear_all_effects`.
        let mut v = vec![2, id("minecraft:teleport_randomly")];
        v.extend_from_slice(&8.0_f32.to_be_bytes());
        v.extend_from_slice(&[0, id("minecraft:clear_all_effects")]);
        assert_eq!(
            read_effects(&mut Reader::new(&v)),
            Some(vec![
                ConsumeEffect::Teleport(8.0, false),
                ConsumeEffect::ClearAll
            ])
        );
    }
}

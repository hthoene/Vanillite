//! The potions witches drink and throw (`Potions`), as `potion_contents` on potion items.

use vanillite_protocol::codec::write_varint;
use vanillite_protocol::components::ids;

use crate::effect::{EffectInstance, MobEffect};
use crate::item::ItemStack;

/// A potion of the `minecraft:potion` registry that mobs use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Potion {
    WaterBreathing,
    FireResistance,
    Healing,
    Swiftness,
    Harming,
    Slowness,
    Poison,
    Regeneration,
    Weakness,
}

impl Potion {
    pub const ALL: [Potion; 9] = [
        Potion::WaterBreathing,
        Potion::FireResistance,
        Potion::Healing,
        Potion::Swiftness,
        Potion::Harming,
        Potion::Slowness,
        Potion::Poison,
        Potion::Regeneration,
        Potion::Weakness,
    ];

    /// The registry id (`minecraft:healing`).
    pub fn name(self) -> &'static str {
        match self {
            Potion::WaterBreathing => "minecraft:water_breathing",
            Potion::FireResistance => "minecraft:fire_resistance",
            Potion::Healing => "minecraft:healing",
            Potion::Swiftness => "minecraft:swiftness",
            Potion::Harming => "minecraft:harming",
            Potion::Slowness => "minecraft:slowness",
            Potion::Poison => "minecraft:poison",
            Potion::Regeneration => "minecraft:regeneration",
            Potion::Weakness => "minecraft:weakness",
        }
    }

    /// The effects of the potion (`Potions`).
    pub fn effects(self) -> [EffectInstance; 1] {
        let (effect, duration) = match self {
            Potion::WaterBreathing => (MobEffect::WATER_BREATHING, 3600),
            Potion::FireResistance => (MobEffect::FIRE_RESISTANCE, 3600),
            Potion::Healing => (MobEffect::INSTANT_HEALTH, 1),
            Potion::Swiftness => (MobEffect::SPEED, 3600),
            Potion::Harming => (MobEffect::INSTANT_DAMAGE, 1),
            Potion::Slowness => (MobEffect::SLOWNESS, 1800),
            Potion::Poison => (MobEffect::POISON, 900),
            Potion::Regeneration => (MobEffect::REGENERATION, 900),
            Potion::Weakness => (MobEffect::WEAKNESS, 1800),
        };
        [EffectInstance::new(effect, duration, 0)]
    }

    /// Whether its effects are instant (`hasInstantEffects`).
    pub fn is_instant(self) -> bool {
        matches!(self, Potion::Healing | Potion::Harming)
    }

    /// The colour of the potion (`PotionContents.getColor`, for the splash particles).
    pub fn color(self) -> i32 {
        // `MobEffect` colours of the single effect.
        match self {
            Potion::WaterBreathing => 0x98_DA_C0,
            Potion::FireResistance => 0xFF_99_00,
            Potion::Healing => 0xF8_24_23,
            Potion::Swiftness => 0x33_EB_FF,
            Potion::Harming => 0xA9_65_6A,
            Potion::Slowness => 0x8B_AF_E0,
            Potion::Poison => 0x87_A3_63,
            Potion::Regeneration => 0xCD_5C_AB,
            Potion::Weakness => 0x48_4D_48,
        }
    }

    /// The potion of a registry id.
    pub fn by_id(potion: u16) -> Option<Potion> {
        let name = vanillite_data::registry::POTION
            .entries
            .get(usize::from(potion))?;
        Potion::ALL.into_iter().find(|p| p.name() == *name)
    }
}

/// A `potion_contents` value with only a potion.
fn contents(potion: Potion) -> Option<Box<[u8]>> {
    let id = vanillite_data::registry::POTION.id(potion.name())?;
    let mut out = vec![1];
    write_varint(&mut out, i32::from(id));
    // No custom colour, effects or name.
    out.extend_from_slice(&[0, 0, 0]);
    Some(out.into_boxed_slice())
}

/// A potion item (`minecraft:potion`, `minecraft:splash_potion`) with `potion`
/// (`PotionContents.createItemStack`).
pub fn stack(item: &str, potion: Potion) -> ItemStack {
    let mut s = super::super::super::equipment::stack(item);
    if !s.is_empty()
        && let Some(c) = contents(potion)
    {
        s.set_component(ids::POTION_CONTENTS, c);
    }
    s
}

/// The potion of a stack's `potion_contents`, if it is one mobs use.
pub fn of_stack(stack: &ItemStack) -> Option<Potion> {
    Potion::by_id(crate::recipe::potion_of(stack)?)
}

//! The effect side of consuming an item (`Consumable.onConsume`): the consumable listeners of
//! the stack (`PotionContents`, `SuspiciousStewEffects`, `OminousBottleAmplifier`), then the
//! item's `on_consume_effects` (`Consumables`: golden apples, raw chicken, rotten flesh,
//! pufferfish, poisonous potato, spider eye, honey bottle, milk bucket).

use vanillite_data::entity::EntityTypeId;

use super::potion::{self, PotionContents};
use super::{EffectHost, EffectInstance, HurtKind, MobEffect, active};
use crate::item::ItemStack;

/// One `ConsumeEffect` that touches the effects.
#[derive(Clone, Debug, PartialEq)]
pub enum ConsumeEffect {
    /// `apply_effects` with its probability.
    Apply(Vec<EffectInstance>, f32),
    /// `remove_effects`.
    Remove(&'static [MobEffect]),
    /// `clear_all_effects`.
    ClearAll,
}

/// The default `on_consume_effects` of an item (`Consumables`), by item name.
pub fn default_effects(item: &str) -> Vec<ConsumeEffect> {
    use ConsumeEffect::{Apply, ClearAll, Remove};
    use MobEffect as M;
    let one = |e, d, a, p| vec![Apply(vec![EffectInstance::new(e, d, a)], p)];
    match item {
        "minecraft:honey_bottle" => vec![Remove(&[M::POISON])],
        "minecraft:chicken" => one(M::HUNGER, 600, 0, 0.3),
        "minecraft:enchanted_golden_apple" => vec![Apply(
            vec![
                EffectInstance::new(M::REGENERATION, 400, 1),
                EffectInstance::new(M::RESISTANCE, 6000, 0),
                EffectInstance::new(M::FIRE_RESISTANCE, 6000, 0),
                EffectInstance::new(M::ABSORPTION, 2400, 3),
            ],
            1.0,
        )],
        "minecraft:golden_apple" => vec![Apply(
            vec![
                EffectInstance::new(M::REGENERATION, 100, 1),
                EffectInstance::new(M::ABSORPTION, 2400, 0),
            ],
            1.0,
        )],
        "minecraft:poisonous_potato" => one(M::POISON, 100, 0, 0.6),
        "minecraft:pufferfish" => vec![Apply(
            vec![
                EffectInstance::new(M::POISON, 1200, 1),
                EffectInstance::new(M::HUNGER, 300, 2),
                EffectInstance::new(M::NAUSEA, 300, 0),
            ],
            1.0,
        )],
        "minecraft:rotten_flesh" => one(M::HUNGER, 600, 0, 0.8),
        "minecraft:spider_eye" => one(M::POISON, 100, 0, 1.0),
        "minecraft:milk_bucket" => vec![ClearAll],
        _ => Vec::new(),
    }
}

/// `addEffect` with `canBeAffected` for the entity type `kind`.
pub fn add_effect(kind: EntityTypeId, new: EffectInstance, host: &mut dyn EffectHost) -> bool {
    super::can_be_affected(kind, new.effect) && active::add(host, new)
}

/// `PotionContents.applyToLivingEntity(entity, durationScale)`: instant effects act at once
/// (with the drinker as source), the others are added.
pub fn apply_potion(
    kind: EntityTypeId,
    contents: &PotionContents,
    scale: f32,
    host: &mut dyn EffectHost,
    source: HurtKind,
) {
    for e in contents.scaled_effects(scale) {
        if e.effect.is_instantaneous() {
            active::apply_instantaneous(e.effect, host, e.amplifier(), 1.0, source);
        } else {
            add_effect(kind, e, host);
        }
    }
}

/// The effect side of `Consumable.onConsume` for `stack` eaten or drunk by an entity of type
/// `kind`: the stack's listeners, then the item's consume effects. `random` is the entity's
/// `nextFloat` for the probabilities; `drinker` is the source of instant potion damage.
pub fn on_consume(
    stack: &ItemStack,
    kind: EntityTypeId,
    host: &mut dyn EffectHost,
    random: &mut dyn FnMut() -> f32,
    drinker: Option<vanillite_world::entity::EntityId>,
) {
    let name = vanillite_data::registry::ITEM
        .name(stack.item)
        .unwrap_or("minecraft:air");
    // `ConsumableListener`s: potion contents, stew effects, ominous bottle amplifier.
    if name == "minecraft:potion" {
        let contents = PotionContents::of_stack(stack);
        let source = HurtKind::IndirectMagic {
            direct: drinker,
            owner: drinker,
        };
        apply_potion(kind, &contents, potion::duration_scale(stack), host, source);
    }
    for e in potion::stew_effects(stack) {
        add_effect(kind, e, host);
    }
    if name == "minecraft:ominous_bottle" {
        let amplifier = stack
            .component(vanillite_protocol::components::ids::OMINOUS_BOTTLE_AMPLIFIER)
            .and_then(|b| vanillite_protocol::codec::Reader::new(b).varint().ok())
            .unwrap_or(0);
        add_effect(
            kind,
            EffectInstance::with_flags(MobEffect::BAD_OMEN, 120_000, amplifier, false, false, true),
            host,
        );
    }
    for effect in default_effects(name) {
        match effect {
            // `ApplyStatusEffectsConsumeEffect.apply`: one roll for the whole list.
            ConsumeEffect::Apply(list, probability) => {
                if random() >= probability {
                    continue;
                }
                for e in list {
                    add_effect(kind, e, host);
                }
            }
            ConsumeEffect::Remove(list) => {
                for &e in list {
                    host.effects().remove(e);
                }
            }
            ConsumeEffect::ClearAll => {
                host.effects().clear();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Host {
        fx: super::super::ActiveEffects,
        absorption: f32,
    }

    impl EffectHost for Host {
        fn effects(&mut self) -> &mut super::super::ActiveEffects {
            &mut self.fx
        }
        fn health(&self) -> f32 {
            20.0
        }
        fn max_health(&self) -> f32 {
            20.0
        }
        fn heal(&mut self, _amount: f32) {}
        fn hurt(&mut self, _kind: HurtKind, _amount: f32) -> bool {
            false
        }
        fn absorption(&self) -> f32 {
            self.absorption
        }
        fn set_absorption(&mut self, amount: f32) {
            self.absorption = amount;
        }
        fn inverted_heal_and_harm(&self) -> bool {
            false
        }
        fn tick_count(&self) -> i32 {
            0
        }
    }

    fn stack(name: &str) -> ItemStack {
        ItemStack::new(vanillite_data::registry::ITEM.id(name).unwrap_or(0), 1)
    }

    #[test]
    fn golden_apple_then_milk() {
        let player = vanillite_data::entity::ids::PLAYER;
        let mut host = Host::default();
        let mut never = || 0.99_f32;
        on_consume(
            &stack("minecraft:golden_apple"),
            player,
            &mut host,
            &mut never,
            None,
        );
        assert_eq!(host.fx.amplifier(MobEffect::REGENERATION), Some(1));
        assert_eq!(host.absorption, 4.0);
        // Rotten flesh: 80 % chance, the roll 0.99 misses.
        on_consume(
            &stack("minecraft:rotten_flesh"),
            player,
            &mut host,
            &mut never,
            None,
        );
        assert!(!host.fx.has(MobEffect::HUNGER));
        on_consume(
            &stack("minecraft:milk_bucket"),
            player,
            &mut host,
            &mut never,
            None,
        );
        assert!(host.fx.is_empty());
    }

    #[test]
    fn potion_effects_are_added() {
        let player = vanillite_data::entity::ids::PLAYER;
        let mut host = Host::default();
        let mut potion = stack("minecraft:potion");
        let swiftness = vanillite_data::registry::POTION
            .id("minecraft:strong_swiftness")
            .unwrap_or(0);
        potion.patch_mut().set(
            vanillite_protocol::components::ids::POTION_CONTENTS,
            PotionContents::of(swiftness).encode(),
        );
        let mut roll = || 0.0_f32;
        on_consume(&potion, player, &mut host, &mut roll, None);
        let speed = host.fx.get(MobEffect::SPEED).cloned();
        assert_eq!(speed.map(|s| (s.duration, s.amplifier)), Some((1800, 1)));
    }
}

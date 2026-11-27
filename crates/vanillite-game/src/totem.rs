//! Totems of undying (`death_protection`, `LivingEntity.checkTotemDeathProtection`): a living
//! entity about to die from damage that does not bypass invulnerability uses up the first
//! death protection item in its hands, keeps one health point, gets the item's death effects
//! (the totem: no effects, then Regeneration II, Absorption II and Fire Resistance) and shows
//! the totem animation (entity event 35).

use vanillite_protocol::codec::Reader;
use vanillite_protocol::packets::play::clientbound::EntityEvent;
use vanillite_world::entity::EntityId;

use crate::effect::consume::ConsumeEffect;
use crate::effect::{EffectHost, EffectInstance, MobEffect};
use crate::item::{self, EquipmentSlot, ItemStack};
use crate::survival::Effect;
use crate::survival::damage::{DamageKind, DamageTypes, tag};

/// The death effects of a stack's `death_protection` component, `None` without one.
pub fn death_protection(stack: &ItemStack) -> Option<Vec<ConsumeEffect>> {
    if stack.is_empty() {
        return None;
    }
    let id = vanillite_data::registry::DATA_COMPONENT_TYPE.id("minecraft:death_protection")?;
    if let Some(p) = stack.patch() {
        if let Some(v) = p.get(id) {
            return Some(decode_effects(v));
        }
        if p.is_removed(id) {
            return None;
        }
    }
    (item::item_id(stack).name() == "minecraft:totem_of_undying").then(|| {
        vec![
            ConsumeEffect::ClearAll,
            ConsumeEffect::Apply(
                vec![
                    EffectInstance::new(MobEffect::REGENERATION, 900, 1),
                    EffectInstance::new(MobEffect::ABSORPTION, 100, 1),
                    EffectInstance::new(MobEffect::FIRE_RESISTANCE, 800, 0),
                ],
                1.0,
            ),
        ]
    })
}

/// `DeathProtection.STREAM_CODEC`: the consume effects the game applies (`clear_all_effects`,
/// `apply_effects`); decoding stops at the first other one.
fn decode_effects(bytes: &[u8]) -> Vec<ConsumeEffect> {
    let mut out = Vec::new();
    let mut r = Reader::new(bytes);
    let Ok(count) = r.varint() else {
        return out;
    };
    let registry = &vanillite_data::registry::CONSUME_EFFECT_TYPE;
    for _ in 0..count {
        let Some(kind) = r
            .varint()
            .ok()
            .and_then(|v| u16::try_from(v).ok())
            .and_then(|v| registry.name(v))
        else {
            break;
        };
        match kind {
            "minecraft:clear_all_effects" => out.push(ConsumeEffect::ClearAll),
            "minecraft:apply_effects" => {
                let Some(n) = r.varint().ok().and_then(|n| usize::try_from(n).ok()) else {
                    break;
                };
                let list: Option<Vec<EffectInstance>> = (0..n.min(64))
                    .map(|_| EffectInstance::decode(&mut r))
                    .collect();
                let (Some(list), Ok(probability)) = (list, r.f32()) else {
                    break;
                };
                out.push(ConsumeEffect::Apply(list, probability));
            }
            _ => break,
        }
    }
    out
}

/// Whether damage of `kind` may be survived with a totem.
pub fn can_protect(types: &DamageTypes, kind: DamageKind) -> bool {
    !types.is(kind, tag::BYPASSES_INVULNERABILITY)
}

/// The first hand (main, then off hand) that holds a death protection item, with its effects.
pub fn protecting_hand(
    main: &ItemStack,
    off: &ItemStack,
) -> Option<(EquipmentSlot, Vec<ConsumeEffect>)> {
    death_protection(main)
        .map(|e| (EquipmentSlot::MainHand, e))
        .or_else(|| death_protection(off).map(|e| (EquipmentSlot::OffHand, e)))
}

/// `DeathProtection.applyEffects` on an effect host (`random` rolls the probabilities).
pub fn apply_effects(
    effects: &[ConsumeEffect],
    kind: vanillite_data::entity::EntityTypeId,
    host: &mut dyn EffectHost,
    random: &mut dyn FnMut() -> f32,
) {
    for effect in effects {
        match effect {
            ConsumeEffect::ClearAll => {
                host.effects().clear();
            }
            ConsumeEffect::Apply(list, probability) => {
                if random() >= *probability {
                    continue;
                }
                for e in list {
                    crate::effect::consume::add_effect(kind, e.clone(), host);
                }
            }
            ConsumeEffect::Remove(list) => {
                for &e in *list {
                    host.effects().remove(e);
                }
            }
        }
    }
}

/// The totem animation (entity event 35) for the entity and the players tracking it.
pub fn animation(out: &mut Vec<Effect>, entity: EntityId, network_id: i32) {
    out.push(Effect::Tracking {
        entity,
        with_self: true,
        packet: vanillite_protocol::SharedPacket::of(&EntityEvent {
            entity_id: network_id,
            event: 35,
        }),
    });
}

//! Views of live entities for the trigger predicates (`EntityPredicate.createContext`) and the
//! way the game fires a player's triggers (`CriteriaTriggers.X.trigger(player, ...)`): the
//! event is only built when an unfinished criterion of the player listens for its type.

use std::borrow::Cow;

use vanillite_world::entity::{EntityFlags, EntityId, Pose};

use super::predicate::{DamageEvent, DamageView, EffectView, EntityView, PlayerFacts};
use super::trigger::{Event, Snapshot, TriggerKind};
use crate::entities::Entities;
use crate::item::{EquipmentSlot, ItemStack};
use crate::mob::Kind;
use crate::survival::damage::{DamageSource, DamageTypes};

/// Whether `player` has an unfinished criterion of `kind`.
pub fn listens(ents: &Entities, player: EntityId, kind: TriggerKind) -> bool {
    ents.player(player)
        .is_some_and(|p| p.advancements.listens(kind))
}

/// `CriteriaTriggers.X.trigger(player, ...)`: queues the event `make` builds for `player` if
/// anything of the player listens for `kind`.
pub fn fire(
    ents: &mut Entities,
    player: EntityId,
    kind: TriggerKind,
    make: impl FnOnce(&Entities) -> Event,
) {
    if !listens(ents, player, kind) {
        return;
    }
    let event = make(ents);
    if let Some(p) = ents.player_mut(player) {
        p.advancements.trigger(event);
    }
}

/// The view of an entity with its vehicle (and the vehicle's passengers).
pub fn snapshot(ents: &Entities, id: EntityId) -> Option<Snapshot> {
    let mut view = base(ents, id)?;
    if let Some(vehicle) = ents.riding.vehicle_of(id)
        && let Some(mut v) = base(ents, vehicle)
    {
        v.passengers = ents
            .riding
            .passengers_of(vehicle)
            .iter()
            .filter_map(|&p| base(ents, p))
            .collect();
        view.vehicle = Some(Box::new(v));
    }
    view.passengers = ents
        .riding
        .passengers_of(id)
        .iter()
        .filter_map(|&p| base(ents, p))
        .collect();
    Some(Box::new(view))
}

/// The views of entities (`createContext` of each).
pub fn snapshots(ents: &Entities, ids: &[EntityId]) -> Vec<EntityView<'static>> {
    ids.iter()
        .filter_map(|&id| snapshot(ents, id).map(|b| *b))
        .collect()
}

fn effects_of(ents: &Entities, id: EntityId) -> Vec<EffectView> {
    ents.effects(id)
        .map(|fx| {
            fx.iter()
                .map(|e| EffectView {
                    effect: e.effect,
                    amplifier: i32::from(e.amplifier),
                    duration: e.duration,
                    ambient: e.ambient,
                    visible: e.visible,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The equipment in view order (head, chest, legs, feet, body, main hand, off hand).
const VIEW_SLOTS: [EquipmentSlot; 7] = [
    EquipmentSlot::Head,
    EquipmentSlot::Chest,
    EquipmentSlot::Legs,
    EquipmentSlot::Feet,
    EquipmentSlot::Body,
    EquipmentSlot::MainHand,
    EquipmentSlot::OffHand,
];

/// Mobs whose `variant` save field is an exact entity component (`<type>/variant`).
fn has_variant_component(kind: Kind) -> bool {
    matches!(
        kind,
        Kind::Cat
            | Kind::Wolf
            | Kind::Frog
            | Kind::Pig
            | Kind::Cow
            | Kind::Chicken
            | Kind::Nautilus
            | Kind::ZombieNautilus
    )
}

/// The view of one entity without vehicle and passengers.
fn base(ents: &Entities, id: EntityId) -> Option<EntityView<'static>> {
    let e = ents.store.get(id)?;
    let flags = e.flags();
    let mut v = EntityView::new(e.kind().name(), e.pos());
    v.on_ground = flags.contains(EntityFlags::ON_GROUND);
    v.on_fire = flags.contains(EntityFlags::ON_FIRE);
    v.crouching = e.pose() == Pose::Crouching;
    v.sprinting = flags.contains(EntityFlags::SPRINTING);
    v.swimming = flags.contains(EntityFlags::SWIMMING);
    v.fall_flying = flags.contains(EntityFlags::FALL_FLYING);
    if let Some(bolt) = ents.bolt(id) {
        v.lightning_fires = Some(bolt.blocks_set_on_fire);
    }
    if let Some(m) = ents.mob(id) {
        v.baby = m.baby;
        v.on_fire = m.is_on_fire();
        for (slot, s) in v.equipment.iter_mut().zip(VIEW_SLOTS) {
            *slot = Cow::Owned(m.equipment.get(s).clone());
        }
        v.effects = effects_of(ents, id);
        if has_variant_component(m.kind) {
            let tag = crate::mob::nbt::save_mob(&e, m);
            if let Some(variant) = tag.get("variant").and_then(vanillite_nbt::Tag::as_str) {
                let name: &'static str = match m.kind {
                    Kind::Cat => "minecraft:cat/variant",
                    Kind::Wolf => "minecraft:wolf/variant",
                    Kind::Frog => "minecraft:frog/variant",
                    Kind::Pig => "minecraft:pig/variant",
                    Kind::Cow => "minecraft:cow/variant",
                    Kind::Chicken => "minecraft:chicken/variant",
                    _ => "minecraft:zombie_nautilus/variant",
                };
                v.components.push((name, variant.to_owned()));
            }
        }
    } else if let Some(p) = ents.player(id) {
        let inv = &p.inventory;
        let [feet, legs, chest, head] = &inv.armor;
        v.equipment = [
            Cow::Owned(head.clone()),
            Cow::Owned(chest.clone()),
            Cow::Owned(legs.clone()),
            Cow::Owned(feet.clone()),
            Cow::Owned(ItemStack::EMPTY),
            Cow::Owned(inv.selected_item().clone()),
            Cow::Owned(inv.offhand.clone()),
        ];
        v.on_fire = p.survival.fire_ticks > 0;
        v.in_water = p.survival.fluids.in_water();
        v.player = Some(PlayerFacts {
            game_mode: p.game_mode,
            level: p.survival.xp.level,
            stats: None,
        });
        v.effects = effects_of(ents, id);
    }
    Some(v)
}

/// `DamageSource` as the damage source predicates see it.
pub fn damage_view(ents: &Entities, types: &DamageTypes, source: &DamageSource) -> DamageView {
    let t = types.get(source.kind);
    DamageView {
        kind: t.id.clone(),
        tags: t.tags,
        direct: source.direct.and_then(|d| snapshot(ents, d)),
        cause: source.cause.and_then(|c| snapshot(ents, c)),
        is_direct: source.cause == source.direct,
        position: source.position,
    }
}

/// A damage event (`DamagePredicate.matches` arguments).
pub fn damage_event(
    ents: &Entities,
    types: &DamageTypes,
    source: &DamageSource,
    dealt: f32,
    taken: f32,
    blocked: bool,
) -> Box<DamageEvent> {
    Box::new(DamageEvent {
        source: damage_view(ents, types, source),
        dealt,
        taken,
        blocked,
    })
}

//! Targeting tests of brains (`Sensor.isEntityTargetable`, `isEntityAttackable`,
//! `TargetingConditions`) and the lazy line of sight cache of
//! `NearestVisibleLivingEntities`.

use vanillite_config::GameMode;
use vanillite_world::entity::{EntityFlags, EntityId};

use super::super::goals::can_attack;
use super::super::living::has_line_of_sight;
use super::{MemValue, MemoryType, Tick, brain, brain_ref};
use crate::item::EquipmentSlot;

/// `canBeSeenByAnyone`: alive and no spectator.
pub fn can_be_seen(t: &Tick<'_, '_>, id: EntityId) -> bool {
    if let Some(p) = t.ents.player(id) {
        return p.game_mode != GameMode::Spectator && !p.survival.is_dead_or_dying();
    }
    t.ents.mob(id).is_some_and(super::super::MobData::is_alive)
}

/// `getVisibilityPercent`: sneaking players are seen at 80 %, invisible entities by their
/// armour cover.
pub fn visibility_percent(t: &Tick<'_, '_>, id: EntityId) -> f64 {
    let mut v = 1.0;
    if t.ents
        .store
        .get(id)
        .is_some_and(|e| e.flags().contains(EntityFlags::CROUCHING))
    {
        v *= 0.8;
    }
    if let Some(m) = t.ents.mob(id)
        && m.effects.has(crate::effect::MobEffect::INVISIBILITY)
    {
        let worn = [
            EquipmentSlot::Feet,
            EquipmentSlot::Legs,
            EquipmentSlot::Chest,
            EquipmentSlot::Head,
        ]
        .iter()
        .filter(|s| !m.equipment.get(**s).is_empty())
        .count();
        #[allow(clippy::cast_precision_loss)]
        let cover = (worn as f32 / 4.0).max(0.1);
        v *= f64::from(0.7 * cover);
    }
    v
}

/// `TargetingConditions.test` with the mob as the targeter.
pub fn test(
    t: &mut Tick<'_, '_>,
    target: EntityId,
    combat: bool,
    range: f64,
    test_invisible: bool,
    line_of_sight: bool,
) -> bool {
    if target == t.id || !can_be_seen(t, target) {
        return false;
    }
    if combat && !can_attack(t, target) {
        return false;
    }
    if range > 0.0 {
        let modifier = if test_invisible {
            visibility_percent(t, target)
        } else {
            1.0
        };
        let visible = (range * modifier).max(2.0);
        let Some(e) = t.ents.store.get(target) else {
            return false;
        };
        if t.pos().distance_to_sqr(e.pos()) > visible * visible {
            return false;
        }
    }
    !line_of_sight || has_line_of_sight(t, target)
}

/// `Sensor.isEntityTargetable`: non combat, follow range, invisibility ignored for the attack
/// target.
pub fn is_targetable(t: &mut Tick<'_, '_>, target: EntityId) -> bool {
    let range = t.mob_ref().attrs.follow_range;
    let is_target = brain_ref(t).is_entity(MemoryType::AttackTarget, target);
    test(t, target, false, range, !is_target, true)
}

/// `Sensor.isEntityAttackable`.
pub fn is_attackable(t: &mut Tick<'_, '_>, target: EntityId) -> bool {
    let range = t.mob_ref().attrs.follow_range;
    let is_target = brain_ref(t).is_entity(MemoryType::AttackTarget, target);
    test(t, target, true, range, !is_target, true)
}

/// `Sensor.isEntityAttackableIgnoringLineOfSight`.
pub fn is_attackable_ignoring_los(t: &mut Tick<'_, '_>, target: EntityId) -> bool {
    let range = t.mob_ref().attrs.follow_range;
    let is_target = brain_ref(t).is_entity(MemoryType::AttackTarget, target);
    test(t, target, true, range, !is_target, false)
}

/// Takes the visible list out of the brain to test it lazily (and puts it back).
fn with_visible<R>(
    t: &mut Tick<'_, '_>,
    f: impl FnOnce(&mut Tick<'_, '_>, &mut super::Visible) -> R,
) -> Option<R> {
    let mut v = match brain(t).get_mut(MemoryType::NearestVisibleLivingEntities) {
        Some(MemValue::Visible(v)) => std::mem::take(v),
        _ => return None,
    };
    let r = f(t, &mut v);
    if t.exists()
        && let Some(MemValue::Visible(slot)) =
            brain(t).get_mut(MemoryType::NearestVisibleLivingEntities)
    {
        *slot = v;
    }
    Some(r)
}

/// The cached line of sight test of entry `i`.
fn sees(t: &mut Tick<'_, '_>, v: &mut super::Visible, i: usize) -> bool {
    match v.sight[i] {
        1 => true,
        2 => false,
        _ => {
            let id = v.entities[i];
            let ok = is_targetable(t, id);
            v.sight[i] = if ok { 1 } else { 2 };
            ok
        }
    }
}

/// `NearestVisibleLivingEntities.contains(entity)`.
pub fn visible_contains(t: &mut Tick<'_, '_>, id: EntityId) -> bool {
    with_visible(t, |t, v| {
        v.entities
            .iter()
            .position(|&e| e == id)
            .is_some_and(|i| sees(t, v, i))
    })
    .unwrap_or(false)
}

/// `NearestVisibleLivingEntities.findClosest(filter)`.
pub fn visible_find_closest(
    t: &mut Tick<'_, '_>,
    filter: impl Fn(&Tick<'_, '_>, EntityId) -> bool,
) -> Option<EntityId> {
    with_visible(t, |t, v| {
        for i in 0..v.entities.len() {
            let id = v.entities[i];
            if filter(t, id) && sees(t, v, i) {
                return Some(id);
            }
        }
        None
    })
    .flatten()
}

/// `NearestVisibleLivingEntities.findAll(filter)` into `out`.
pub fn visible_find_all(
    t: &mut Tick<'_, '_>,
    filter: impl Fn(&Tick<'_, '_>, EntityId) -> bool,
    out: &mut Vec<EntityId>,
) {
    with_visible(t, |t, v| {
        for i in 0..v.entities.len() {
            let id = v.entities[i];
            if filter(t, id) && sees(t, v, i) {
                out.push(id);
            }
        }
    });
}

/// `BehaviorUtils.entityIsVisible(brain, entity)`.
pub fn entity_is_visible(t: &mut Tick<'_, '_>, id: EntityId) -> bool {
    visible_contains(t, id)
}

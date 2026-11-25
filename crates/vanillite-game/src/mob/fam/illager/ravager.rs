//! Ravagers: trampling leaves, attack and stun cooldowns, the roar.

use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Vec3};
use vanillite_world::entity::{EntityFlags, EntityId};

use super::super::super::common;
use super::super::super::goals::{Goal as MobGoal, MeleeKind};
use super::super::super::kinds::Kind;
use super::super::super::{MobData, Tick, WorldAction, combat, living};
use super::raider::{self, Ignore, LookFilter};
use super::{is_illager, look_at_player, sound, st, st_mut, target_mobs, target_players};
use crate::survival::rules::Rule;

/// `Ravager.registerGoals`.
pub fn register_goals(m: &mut MobData) {
    let g = &mut m.goals;
    let tg = &mut m.targets;
    g.add(4, raider::patrol());
    raider::add_raider_goals(g);
    g.add(0, MobGoal::Float);
    g.add(4, MobGoal::melee(MeleeKind::Plain, 1.0, true));
    g.add(5, MobGoal::stroll(0.4));
    g.add(6, look_at_player(6.0, 0.02));
    g.add(10, raider::look_at_mobs(8.0, 0.02, LookFilter::AnyMob));
    tg.add(2, raider::hurt_by(Ignore::Raiders, true));
    tg.add(3, target_players(true));
    // Baby villagers are spared.
    tg.add(4, target_mobs(true, common::villager));
    tg.add(4, target_mobs(true, common::iron_golem));
}

/// `Ravager.aiStep` after `super.aiStep()`.
pub fn ai_step(t: &mut Tick<'_, '_>) {
    if !t.mob_ref().is_alive() {
        return;
    }
    let immobile = super::super::is_immobile(t.mob_ref());
    let m = t.mob();
    if immobile {
        m.attrs.movement_speed = 0.0;
    } else {
        let max = if m.target.is_some() { 0.35 } else { 0.3 };
        let base = m.attrs.movement_speed;
        m.attrs.movement_speed = base + 0.1 * (max - base);
    }
    if t.mob_ref().horizontal_collision && t.env.rules.bool(Rule::MOB_GRIEFING) {
        let bb = t.aabb().inflate(0.2, 0.2, 0.2);
        let f = vanillite_base::math::floor;
        let mut destroyed = false;
        for x in f(bb.min_x)..=f(bb.max_x) {
            for y in f(bb.min_y)..=f(bb.max_y) {
                for z in f(bb.min_z)..=f(bb.max_z) {
                    let pos = BlockPos::new(x, y, z);
                    if t.level.block(pos).block().name().ends_with("_leaves") {
                        t.cx.actions.push(WorldAction::DestroyBlock(pos, true));
                        destroyed = true;
                    }
                }
            }
        }
        if !destroyed && t.on_ground() {
            let jump = t.mob_ref().attrs.jump_strength;
            let v = t.velocity();
            t.set_velocity(Vec3::new(v.x, jump.max(v.y), v.z));
            t.core_mut().set_flags(EntityFlags::HAS_IMPULSE, true);
        }
    }
    let s = st_mut(t.mob());
    if s.roar_tick > 0 {
        s.roar_tick -= 1;
        if s.roar_tick == 10 {
            roar(t);
        }
    }
    let s = st_mut(t.mob());
    if s.attack_tick > 0 {
        s.attack_tick -= 1;
    }
    if s.stunned_tick > 0 {
        s.stunned_tick -= 1;
        // `stunEffect` rolls its particles.
        let _ = t.mob().random.next_int_bounded(6);
        if st(t.mob_ref()).stunned_tick == 0 {
            sound(t, "minecraft:entity.ravager.roar", 1.0, 1.0);
            st_mut(t.mob()).roar_tick = 20;
        }
    }
}

/// `Ravager.roar`: hurts and throws back everything around it.
fn roar(t: &mut Tick<'_, '_>) {
    let area = t.aabb().inflate(4.0, 4.0, 4.0);
    let mut ids = std::mem::take(&mut t.cx.ids);
    ids.clear();
    t.ents.store.for_each_in(&area, false, |id| ids.push(id));
    let targets: Vec<EntityId> = ids
        .iter()
        .copied()
        .filter(|&id| {
            id != t.id
                && (t.ents.player(id).is_some()
                    || t.ents
                        .mob(id)
                        .is_some_and(|m| m.is_alive() && m.kind != Kind::Ravager))
        })
        .collect();
    t.cx.ids = ids;
    let pos = t.pos();
    for id in targets {
        let illager = t.ents.mob(id).is_some_and(|m| is_illager(m.kind));
        if !illager {
            let source = combat::entity_source(t.ents, t.env.types.mob_attack, t.id, t.id);
            combat::hurt(t.level, t.ents, t.env, t.cx, id, source, 6.0);
            if !t.exists() {
                return;
            }
        }
        if t.ents.player(id).is_none() {
            strong_knockback(t, id, pos);
        }
    }
    let id = t.id;
    living::entity_event(t, id, 69);
}

/// `strongKnockback`: a push away of four blocks per block of distance.
fn strong_knockback(t: &mut Tick<'_, '_>, target: EntityId, from: Vec3) {
    let Some(mut e) = t.ents.store.get_mut(target) else {
        return;
    };
    let p = e.as_ref().pos();
    let (xd, zd) = (p.x - from.x, p.z - from.z);
    let dd = (xd * xd + zd * zd).max(0.001);
    let v = e.as_ref().velocity();
    e.set_velocity(v + Vec3::new(xd / dd * 4.0, 0.2, zd / dd * 4.0));
    e.set_flags(EntityFlags::HAS_IMPULSE, true);
}

/// `Ravager.doHurtTarget`: the attack animation, then the bite.
pub fn do_hurt_target(t: &mut Tick<'_, '_>, target: EntityId) -> bool {
    st_mut(t.mob()).attack_tick = 10;
    let id = t.id;
    living::entity_event(t, id, 4);
    sound(t, "minecraft:entity.ravager.attack", 1.0, 1.0);
    combat::base_mob_attack(t, target)
}

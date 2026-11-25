//! Witches: drinking potions to protect themselves, throwing splash potions, resisting magic.

use vanillite_base::Vec3;
use vanillite_base::random::RandomSource;
use vanillite_world::entity::EntityId;

use super::super::super::common::{self, CommonGoal};
use super::super::super::effects::id;
use super::super::super::goals::Goal as MobGoal;
use super::super::super::missile::{MissileData, MissileKind};
use super::super::super::{MobData, Tick, living, missile, projectile};
use super::potion::{self, Potion};
use super::raider::{self, Ignore};
use super::{Gate, Goal, dirty, fam, is_raider, look_at_player, sound, st, st_mut};
use crate::item::EquipmentSlot;
use crate::survival::damage::{DamageSource, DamageTypes, tag};

/// `Witch.registerGoals`.
pub fn register_goals(m: &mut MobData) {
    let g = &mut m.goals;
    let tg = &mut m.targets;
    g.add(4, raider::patrol());
    g.add(1, MobGoal::Float);
    g.add(2, MobGoal::Common(CommonGoal::ranged(1.0, 60, 60, 10.0)));
    g.add(2, MobGoal::stroll(1.0));
    g.add(3, look_at_player(8.0, 0.02));
    g.add(3, MobGoal::random_look());
    tg.add(1, raider::hurt_by(Ignore::Raiders, false));
    tg.add(2, fam(Goal::WitchHeal));
    tg.add(
        3,
        fam(Goal::Target {
            inner: CommonGoal::nearest_player(true, common::any_player),
            gate: Gate::WitchCanAttack,
        }),
    );
}

/// `NearestHealableRaiderTargetGoal.canUse`: rolls, then needs a raid (none yet).
pub fn heal_can_use(t: &mut Tick<'_, '_>) -> bool {
    if st(t.mob_ref()).heal_cooldown > 0 || !t.mob().random.next_boolean() {
        return false;
    }
    // `hasActiveRaid`: raids are not simulated.
    false
}

/// `Witch.aiStep` before `super.aiStep()`: the heal cooldown, drinking and picking potions.
pub fn ai_step_start(t: &mut Tick<'_, '_>) {
    if !t.mob_ref().is_alive() {
        return;
    }
    st_mut(t.mob()).heal_cooldown -= 1;
    if st(t.mob_ref()).drinking {
        let s = st_mut(t.mob());
        let done = s.using_time <= 0;
        s.using_time -= 1;
        if done {
            s.drinking = false;
            let speed = s.speed_before_drinking.take();
            dirty(t);
            let held = t.mob_ref().equipment.get(EquipmentSlot::MainHand).clone();
            t.mob()
                .equipment
                .set(EquipmentSlot::MainHand, crate::item::ItemStack::default());
            super::super::super::equipment::broadcast(t);
            if let Some(p) = potion::of_stack(&held)
                && crate::item::item_id(&held).name() == "minecraft:potion"
            {
                for e in p.effects() {
                    super::super::super::effects::add(t.mob(), e);
                }
            }
            if let Some(speed) = speed {
                t.mob().attrs.movement_speed = speed;
            }
        }
    } else {
        let (in_water, on_fire, fire_damage, hurt, target) = {
            let m = t.mob_ref();
            let fire = m
                .last_damage_source(t.env.game_time)
                .is_some_and(|k| t.env.types.is(k, tag::IS_FIRE));
            (
                m.fluids.eye_in_water,
                m.is_on_fire(),
                fire,
                m.health < m.max_health(),
                m.target,
            )
        };
        let mut chosen = None;
        let has = |t: &Tick<'_, '_>, e| t.mob_ref().effects.has(e);
        if t.mob().random.next_float() < 0.15 && in_water && !has(t, id::WATER_BREATHING) {
            chosen = Some(Potion::WaterBreathing);
        } else if t.mob().random.next_float() < 0.15
            && (on_fire || fire_damage)
            && !has(t, id::FIRE_RESISTANCE)
        {
            chosen = Some(Potion::FireResistance);
        } else if t.mob().random.next_float() < 0.05 && hurt {
            chosen = Some(Potion::Healing);
        } else if t.mob().random.next_float() < 0.5
            && target.is_some()
            && !has(t, id::SPEED)
            && target
                .and_then(|id| t.other(id))
                .is_some_and(|o| o.pos.distance_to_sqr(t.pos()) > 121.0)
        {
            chosen = Some(Potion::Swiftness);
        }
        if let Some(p) = chosen {
            t.mob().equipment.set(
                EquipmentSlot::MainHand,
                potion::stack("minecraft:potion", p),
            );
            super::super::super::equipment::broadcast(t);
            let s = st_mut(t.mob());
            // `getUseDuration` of a potion: 32 ticks.
            s.using_time = 32;
            s.drinking = true;
            dirty(t);
            if !t.mob_ref().silent {
                let pitch = 0.8 + t.mob().random.next_float() * 0.4;
                sound(t, "minecraft:entity.witch.drink", 1.0, pitch);
            }
            // The drinking speed modifier (-0.25 `ADD_VALUE`).
            let m = t.mob();
            let current = m.attrs.movement_speed;
            let base = *st_mut(m).speed_before_drinking.get_or_insert(current);
            m.attrs.movement_speed = (base - 0.25).max(0.0);
        }
    }
    if t.mob().random.next_float() < 7.5e-4 {
        let id = t.id;
        living::entity_event(t, id, 15);
    }
}

/// `Witch.getDamageAfterMagicAbsorb`: its own potions do nothing, magic 15 %.
pub fn absorb(source: &DamageSource, types: &DamageTypes, damage: f32) -> f32 {
    // `source.getEntity() == this` (its own potions) is skipped where the potion splashes.
    let mut damage = damage;
    let name = types.get(source.kind).id.as_str();
    if matches!(
        name,
        "minecraft:magic"
            | "minecraft:indirect_magic"
            | "minecraft:sonic_boom"
            | "minecraft:thorns"
    ) {
        damage *= 0.15;
    }
    damage
}

/// `Witch.performRangedAttack`: harming, or slowness, poison or weakness, healing for raiders.
pub fn perform_ranged_attack(t: &mut Tick<'_, '_>, target: EntityId) {
    if st(t.mob_ref()).drinking {
        return;
    }
    let Some(o) = t.other(target) else {
        return;
    };
    let v = t
        .ents
        .store
        .get(target)
        .map_or(Vec3::ZERO, |e| e.velocity());
    let p = t.pos();
    let xd = o.pos.x + v.x - p.x;
    let yd = o.eye_y - f64::from(1.1_f32) - p.y;
    let zd = o.pos.z + v.z - p.z;
    let dist = (xd * xd + zd * zd).sqrt();
    let target_mob = t
        .ents
        .mob(target)
        .map(|m| (m.kind, m.health, m.effects.clone()));
    let target_health = target_mob
        .as_ref()
        .map(|x| x.1)
        .or_else(|| t.ents.player(target).map(|p| p.survival.health))
        .unwrap_or(20.0);
    let has = |e| target_mob.as_ref().is_some_and(|x| x.2.has(e));
    let mut chosen = Potion::Harming;
    if target_mob.as_ref().is_some_and(|x| is_raider(x.0)) {
        chosen = if target_health <= 4.0 {
            Potion::Healing
        } else {
            Potion::Regeneration
        };
        t.mob().target = None;
    } else if dist >= 8.0 && !has(id::SLOWNESS) {
        chosen = Potion::Slowness;
    } else if target_health >= 8.0 && !has(id::POISON) {
        chosen = Potion::Poison;
    } else if dist <= 3.0 && !has(id::WEAKNESS) && t.mob().random.next_float() < 0.25 {
        chosen = Potion::Weakness;
    }
    let mut data = MissileData::new(MissileKind::SplashPotion);
    data.item = potion::stack("minecraft:splash_potion", chosen);
    let start = Vec3::new(p.x, t.eye_y() - f64::from(0.1_f32), p.z);
    let power = if dist <= 2.0 { 0.45 } else { 0.75 };
    let (velocity, rot) = projectile::shoot(
        &mut t.mob().random,
        Vec3::new(xd, yd + dist * 0.2, zd),
        power,
        8.0,
    );
    // `shootFromRotation` adds the shooter's motion only for players.
    missile::spawn_from(t, start, velocity, rot, data);
    if !t.mob_ref().silent {
        let pitch = 0.8 + t.mob().random.next_float() * 0.4;
        sound(t, "minecraft:entity.witch.throw", 1.0, pitch);
    }
}

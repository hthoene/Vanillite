//! Pillagers: crossbows (`CrossbowAttackMob`, `RangedCrossbowAttackGoal`).

use vanillite_base::Vec3;
use vanillite_base::random::RandomSource;
use vanillite_world::entity::EntityId;

use super::super::super::common;
use super::super::super::goals::{self, Goal as MobGoal};
use super::super::super::living::has_line_of_sight;
use super::super::super::{MobData, Tick, equipment, nav, projectile};
use super::super::SpawnContext;
use super::raider::{self, Ignore, LookFilter};
use super::{
    Goal, avoid_creaking, dirty, fam, look_at_player, st, st_mut, target_mobs, target_players,
};
use crate::item::EquipmentSlot;
use crate::survival::rules::Difficulty;

/// The crossbow state of `RangedCrossbowAttackGoal`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CrossbowState {
    Uncharged,
    Charging,
    Charged,
    ReadyToAttack,
}

/// `RangedCrossbowAttackGoal(mob, speed, radius)`.
#[derive(Clone, Copy, Debug)]
pub struct Crossbow {
    pub speed: f64,
    pub radius_sqr: f32,
    pub state: CrossbowState,
    pub see_time: i32,
    pub attack_delay: i32,
    pub update_path_delay: i32,
}

/// `Pillager.registerGoals`.
pub fn register_goals(m: &mut MobData) {
    let g = &mut m.goals;
    let tg = &mut m.targets;
    g.add(4, raider::patrol());
    raider::add_raider_goals(g);
    g.add(0, MobGoal::Float);
    g.add(1, avoid_creaking(1.0, 1.2));
    g.add(2, fam(Goal::HoldGround { radius_sqr: 100.0 }));
    g.add(
        3,
        fam(Goal::Crossbow(Crossbow {
            speed: 1.0,
            radius_sqr: 64.0,
            state: CrossbowState::Uncharged,
            see_time: 0,
            attack_delay: 0,
            update_path_delay: 0,
        })),
    );
    g.add(8, raider::stroll(0.6, 120));
    g.add(9, look_at_player(15.0, 1.0));
    g.add(10, raider::look_at_mobs(15.0, 0.02, LookFilter::AnyMob));
    tg.add(1, raider::hurt_by(Ignore::Raiders, true));
    tg.add(2, target_players(true));
    tg.add(3, target_mobs(false, common::villager));
    tg.add(3, target_mobs(true, common::iron_golem));
}

/// `Pillager.finalizeSpawn`: the crossbow, its enchantments (1 in 300 with Piercing).
pub fn finalize_spawn(m: &mut MobData, ctx: &mut SpawnContext<'_>) {
    m.equipment.set(
        EquipmentSlot::MainHand,
        equipment::stack("minecraft:crossbow"),
    );
    // `enchantSpawnedWeapon`, then the Piercing chance.
    equipment::enchant_spawned(m, EquipmentSlot::MainHand, 0.25, ctx.special_multiplier);
    if m.random.next_int_bounded(300) == 0 {
        enchant_piercing(m);
    }
    for slot in [
        EquipmentSlot::Feet,
        EquipmentSlot::Legs,
        EquipmentSlot::Chest,
        EquipmentSlot::Head,
    ] {
        equipment::enchant_spawned(m, slot, 0.5, ctx.special_multiplier);
    }
}

/// `VanillaEnchantmentProviders.PILLAGER_SPAWN_CROSSBOW`: Piercing I.
fn enchant_piercing(m: &mut MobData) {
    let Some(data) = crate::player::play::game_data() else {
        return;
    };
    let Some(&piercing) = data.enchantments.get("minecraft:piercing") else {
        return;
    };
    let mut s = m.equipment.get(EquipmentSlot::MainHand).clone();
    if crate::item::item_id(&s).name() != "minecraft:crossbow" {
        return;
    }
    let id = vanillite_protocol::components::ids::ENCHANTMENTS;
    let mut list = crate::enchant::read(s.component(id));
    match list.iter_mut().find(|x| x.0 == piercing) {
        Some(x) => x.1 = x.1.max(1),
        None => list.push((piercing, 1)),
    }
    s.patch_mut().set(id, crate::enchant::write(&list));
    m.equipment.set(EquipmentSlot::MainHand, s);
}

fn holds_crossbow(m: &MobData) -> bool {
    [EquipmentSlot::MainHand, EquipmentSlot::OffHand]
        .iter()
        .any(|&s| {
            let st = m.equipment.get(s);
            !st.is_empty() && crate::item::item_id(st).name() == "minecraft:crossbow"
        })
}

fn valid_target(t: &Tick<'_, '_>) -> bool {
    t.mob_ref().target.is_some_and(|id| t.other(id).is_some())
}

pub fn crossbow_can_use(t: &mut Tick<'_, '_>) -> bool {
    valid_target(t) && holds_crossbow(t.mob_ref())
}

pub fn crossbow_can_continue(t: &mut Tick<'_, '_>) -> bool {
    valid_target(t)
        && (crossbow_can_use(t) || !t.nav_ref().is_done())
        && holds_crossbow(t.mob_ref())
}

/// `CrossbowItem.getChargeDuration` of the crossbow in use.
fn charge_duration(m: &mut MobData) -> i32 {
    let s = m.equipment.get(crate::crossbow::mob_hand(m)).clone();
    crate::crossbow::charge_duration(&s, &mut m.random)
}

fn set_charging(t: &mut Tick<'_, '_>, on: bool) {
    if st(t.mob_ref()).charging_crossbow != on {
        st_mut(t.mob()).charging_crossbow = on;
        dirty(t);
    }
}

pub fn crossbow_stop(t: &mut Tick<'_, '_>, c: &mut Crossbow) {
    goals::set_aggressive(t, false);
    t.mob().target = None;
    c.see_time = 0;
    if t.mob_ref().using_item >= 0 {
        projectile::stop_using_item(t);
        set_charging(t, false);
        // `getUseItem().set(CHARGED_PROJECTILES, EMPTY)` runs on the empty use item after
        // `stopUsingItem`: the crossbow keeps its load.
    }
}

/// `LivingEntity.updatingUsingItem` of a pillager using its crossbow: `onUseTick` sees the
/// ticks used before this one.
pub fn use_tick(t: &mut Tick<'_, '_>) {
    let using = t.mob_ref().using_item;
    if using < 0 {
        return;
    }
    crossbow_use_tick(t, using - 1);
}

/// `CrossbowItem.onUseTick` after `used` ticks.
fn crossbow_use_tick(t: &mut Tick<'_, '_>, used: i32) {
    let mut flags = st(t.mob_ref()).crossbow_sounds;
    crate::crossbow::mob_use_tick(t, &mut flags, used);
    if t.exists() {
        st_mut(t.mob()).crossbow_sounds = flags;
    }
}

pub fn crossbow_tick(t: &mut Tick<'_, '_>, c: &mut Crossbow) {
    let Some(target) = t.mob_ref().target else {
        return;
    };
    let Some(o) = t.other(target) else {
        return;
    };
    let sees = has_line_of_sight(t, target);
    let saw = c.see_time > 0;
    if sees != saw {
        c.see_time = 0;
    }
    if sees {
        c.see_time += 1;
    } else {
        c.see_time -= 1;
    }
    let dist = t.pos().distance_to_sqr(o.pos);
    let needs_to_move = (dist > f64::from(c.radius_sqr) || c.see_time < 5) && c.attack_delay == 0;
    if needs_to_move {
        c.update_path_delay -= 1;
        if c.update_path_delay <= 0 {
            let speed = if c.state == CrossbowState::Uncharged {
                c.speed
            } else {
                c.speed * 0.5
            };
            nav::move_to_entity(t, o.pos, 1, speed);
            // `PATHFINDING_DELAY_RANGE`: 20 to 40 ticks.
            c.update_path_delay = t.mob().random.next_int_between_inclusive(20, 40);
        }
    } else {
        c.update_path_delay = 0;
        t.nav().stop();
    }
    t.mob()
        .look
        .set_look_at(Vec3::new(o.pos.x, o.eye_y, o.pos.z), 30.0, 30.0);
    match c.state {
        CrossbowState::Uncharged => {
            if !needs_to_move {
                projectile::start_using_item(t);
                c.state = CrossbowState::Charging;
                set_charging(t, true);
            }
        }
        CrossbowState::Charging => {
            if t.mob_ref().using_item < 0 {
                c.state = CrossbowState::Uncharged;
            }
            let pull = t.mob_ref().using_item;
            if pull >= charge_duration(t.mob()) {
                // `releaseUsingItem`: one more use tick (the load), then the use stops.
                crossbow_use_tick(t, pull);
                projectile::stop_using_item(t);
                c.state = CrossbowState::Charged;
                c.attack_delay = 20 + t.mob().random.next_int_bounded(20);
                set_charging(t, false);
            }
        }
        CrossbowState::Charged => {
            c.attack_delay -= 1;
            if c.attack_delay == 0 {
                c.state = CrossbowState::ReadyToAttack;
            }
        }
        CrossbowState::ReadyToAttack => {
            if sees {
                super::super::perform_ranged_attack(t, target, 1.0);
                c.state = CrossbowState::Uncharged;
            }
        }
    }
}

/// `CrossbowAttackMob.performCrossbowAttack(mob, 1.6)`: the loaded crossbow fires at the
/// target.
pub fn perform_crossbow_attack(t: &mut Tick<'_, '_>, _target: EntityId) {
    let target = t.mob_ref().target;
    crate::crossbow::mob_shoot(t, target, crate::crossbow::MOB_ARROW_POWER);
    // `onCrossbowAttackPerformed`.
    t.mob().no_action_time = 0;
    let _ = Difficulty::Peaceful;
}

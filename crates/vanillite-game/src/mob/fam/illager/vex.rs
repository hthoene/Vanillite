//! Vexes: flying through walls, charging their target, a limited life.

use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Vec3};
use vanillite_nbt::{Compound, Tag};
use vanillite_world::entity::EntityFlags;

use super::super::super::goals::{self, Goal as MobGoal, TargetState};
use super::super::super::nav::Operation;
use super::super::super::{MobData, Tick, combat, equipment, living};
use super::super::SpawnContext;
use super::raider::{self, Ignore, LookFilter};
use super::{Goal, State, dirty, fam, look_at_player, sound, st, st_mut, target_players};
use crate::item::EquipmentSlot;
use crate::survival::damage::DamageSource;

/// `Vex.registerGoals`.
pub fn register_goals(m: &mut MobData) {
    let g = &mut m.goals;
    let tg = &mut m.targets;
    g.add(0, MobGoal::Float);
    g.add(4, fam(Goal::VexCharge));
    g.add(8, fam(Goal::VexRandomMove));
    g.add(9, look_at_player(3.0, 1.0));
    g.add(10, raider::look_at_mobs(8.0, 0.02, LookFilter::AnyMob));
    tg.add(1, raider::hurt_by(Ignore::Raiders, true));
    tg.add(2, fam(Goal::VexCopyOwner(TargetState::new(false))));
    tg.add(3, target_players(true));
}

/// `Vex.finalizeSpawn`: an iron sword that never drops.
pub fn finalize_spawn(m: &mut MobData, ctx: &mut SpawnContext<'_>) {
    m.equipment.set(
        EquipmentSlot::MainHand,
        equipment::stack("minecraft:iron_sword"),
    );
    m.equipment.set_drop_chance(EquipmentSlot::MainHand, 0.0);
    equipment::populate_default_enchantments(m, ctx.special_multiplier);
}

/// `Vex.tick` before `super.tick()`.
pub fn before_tick(_t: &mut Tick<'_, '_>) {}

/// `Vex.tick` after `super.tick()`: no gravity from now on (a new vex falls in its first
/// tick), and the limited life starves it.
pub fn after_tick(t: &mut Tick<'_, '_>) {
    t.core_mut().set_flags(EntityFlags::NO_GRAVITY, true);
    let s = st_mut(t.mob());
    let Some(life) = s.life_ticks.as_mut() else {
        return;
    };
    *life -= 1;
    if *life <= 0 {
        *life = 20;
        let source = DamageSource::of(t.env.types.starve);
        let id = t.id;
        combat::hurt(t.level, t.ents, t.env, t.cx, id, source, 1.0);
    }
}

/// `travelInAir` with `noPhysics`: flies through blocks; the gravity only counts in its first
/// tick (no gravity afterwards).
pub fn travel(t: &mut Tick<'_, '_>, input: Vec3) {
    living::move_relative(t, 0.02, input);
    let v = t.velocity();
    let p = t.pos();
    t.core_mut().set_pos(p + v);
    t.core_mut().set_flags(EntityFlags::ON_GROUND, false);
    let v = t.velocity();
    let y = v.y - living::gravity(t);
    t.set_velocity(Vec3::new(
        v.x * f64::from(0.91_f32),
        y * f64::from(0.98_f32),
        v.z * f64::from(0.91_f32),
    ));
}

/// `VexMoveControl.tick`.
pub fn move_control(t: &mut Tick<'_, '_>) {
    let ctl = t.mob_ref().move_ctl;
    if ctl.operation != Operation::MoveTo {
        return;
    }
    let p = t.pos();
    let delta = ctl.wanted - p;
    let len = delta.length();
    let aabb = t.aabb();
    let size =
        ((aabb.max_x - aabb.min_x) + (aabb.max_y - aabb.min_y) + (aabb.max_z - aabb.min_z)) / 3.0;
    if len < size {
        t.mob().move_ctl.operation = Operation::Wait;
        let v = t.velocity();
        t.set_velocity(v * 0.5);
        return;
    }
    let v = t.velocity() + delta * (ctl.speed_modifier * 0.05 / len);
    t.set_velocity(v);
    let (dx, dz) = match t.mob_ref().target.and_then(|id| t.other(id)) {
        Some(o) => (o.pos.x - p.x, o.pos.z - p.z),
        None => (v.x, v.z),
    };
    #[allow(clippy::cast_possible_truncation)]
    let y_rot = -(vanillite_base::math::atan2(dx, dz) as f32) * (180.0 / std::f32::consts::PI);
    let mut rot = t.core().rot();
    rot.y_rot = y_rot;
    t.core_mut().set_rot(rot);
    t.mob().y_body_rot = y_rot;
}

pub fn set_charging(t: &mut Tick<'_, '_>, on: bool) {
    let s = st_mut(t.mob());
    let flags = if on {
        s.vex_flags | 1
    } else {
        s.vex_flags & !1
    };
    if flags != s.vex_flags {
        s.vex_flags = flags;
        dirty(t);
    }
}

fn charging(m: &MobData) -> bool {
    st(m).vex_flags & 1 != 0
}

pub fn charge_can_use(t: &mut Tick<'_, '_>) -> bool {
    let Some(target) = t.mob_ref().target else {
        return false;
    };
    let Some(o) = t.other(target) else {
        return false;
    };
    if t.mob_ref().move_ctl.has_wanted() {
        return false;
    }
    if t.mob().random.next_int_bounded(goals::reduced(7)) != 0 {
        return false;
    }
    t.pos().distance_to_sqr(o.pos) > 4.0
}

pub fn charge_can_continue(t: &mut Tick<'_, '_>) -> bool {
    t.mob_ref().move_ctl.has_wanted()
        && charging(t.mob_ref())
        && t.mob_ref().target.is_some_and(|id| t.other(id).is_some())
}

pub fn charge_start(t: &mut Tick<'_, '_>) {
    if let Some(o) = t.mob_ref().target.and_then(|id| t.other(id)) {
        t.mob()
            .move_ctl
            .set_wanted_position(Vec3::new(o.pos.x, o.eye_y, o.pos.z), 1.0);
    }
    set_charging(t, true);
    sound(t, "minecraft:entity.vex.charge", 1.0, 1.0);
}

pub fn charge_tick(t: &mut Tick<'_, '_>) {
    let Some(target) = t.mob_ref().target else {
        return;
    };
    let Some(o) = t.other(target) else {
        return;
    };
    if t.aabb().intersects(&o.aabb) {
        combat::mob_attack(t, target);
        if t.exists() {
            set_charging(t, false);
        }
    } else if t.pos().distance_to_sqr(o.pos) < 9.0 {
        t.mob()
            .move_ctl
            .set_wanted_position(Vec3::new(o.pos.x, o.eye_y, o.pos.z), 1.0);
    }
}

pub fn random_move_can_use(t: &mut Tick<'_, '_>) -> bool {
    !t.mob_ref().move_ctl.has_wanted() && t.mob().random.next_int_bounded(goals::reduced(7)) == 0
}

/// `VexRandomMoveGoal.tick`: an air block near its bound origin.
pub fn random_move_tick(t: &mut Tick<'_, '_>) {
    let origin = st(t.mob_ref())
        .bound_origin
        .unwrap_or_else(|| t.block_pos());
    for _ in 0..3 {
        let r = &mut t.mob().random;
        let (dx, dy, dz) = (
            r.next_int_bounded(15) - 7,
            r.next_int_bounded(11) - 5,
            r.next_int_bounded(15) - 7,
        );
        let pos = BlockPos::new(origin.x + dx, origin.y + dy, origin.z + dz);
        if t.level.block(pos).is_air() {
            let at = Vec3::new(
                f64::from(pos.x) + 0.5,
                f64::from(pos.y) + 0.5,
                f64::from(pos.z) + 0.5,
            );
            t.mob().move_ctl.set_wanted_position(at, 0.25);
            if t.mob_ref().target.is_none() {
                t.mob().look.set_look_at(at, 180.0, 20.0);
            }
            break;
        }
    }
}

/// The owner (an evoker): the cached entity while it is still that UUID, else looked up near
/// the vex once a second.
fn owner(t: &mut Tick<'_, '_>) -> Option<vanillite_world::entity::EntityId> {
    let uuid = st(t.mob_ref()).owner?;
    if let Some(id) = st(t.mob_ref()).owner_id
        && t.ents.store.get(id).is_some_and(|e| e.uuid() == uuid)
    {
        return Some(id);
    }
    if t.mob_ref().tick_count % 20 != 0 {
        return None;
    }
    let area = t.aabb().inflate(32.0, 32.0, 32.0);
    let mut found = None;
    let ents = &*t.ents;
    ents.store.for_each_in(&area, false, |id| {
        if found.is_none() && ents.store.get(id).is_some_and(|e| e.uuid() == uuid) {
            found = Some(id);
        }
    });
    st_mut(t.mob()).owner_id = found;
    found
}

pub fn copy_owner_can_use(t: &mut Tick<'_, '_>) -> bool {
    let Some(owner) = owner(t) else {
        return false;
    };
    let Some(target) = t.ents.mob(owner).and_then(|m| m.target) else {
        return false;
    };
    // `canAttack(target, forNonCombat ignoring sight)`.
    goals::can_attack(t, target)
}

pub fn copy_owner_start(t: &mut Tick<'_, '_>, state: &mut TargetState) {
    let target = owner(t).and_then(|o| t.ents.mob(o)).and_then(|m| m.target);
    goals::set_target(t, target);
    state.unseen_ticks = 0;
}

pub fn save(s: &State, out: &mut Compound) {
    if let Some(p) = s.bound_origin {
        out.insert("bound_pos", vec![p.x, p.y, p.z]);
    }
    if let Some(life) = s.life_ticks {
        out.insert("life_ticks", life);
    }
    if let Some(u) = s.owner {
        #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
        let ints = vec![
            (u >> 96) as i32,
            (u >> 64) as i32,
            (u >> 32) as i32,
            u as i32,
        ];
        out.insert("owner", ints);
    }
}

pub fn load(s: &mut State, tag: &Compound) {
    s.bound_origin = match tag.get("bound_pos") {
        Some(Tag::IntArray(v)) if v.len() == 3 => Some(BlockPos::new(v[0], v[1], v[2])),
        _ => None,
    };
    s.life_ticks = match tag.get("life_ticks") {
        Some(Tag::Int(v)) => Some(*v),
        _ => None,
    };
    s.owner = super::super::super::nbt::uuid_key_of(tag, "owner");
}

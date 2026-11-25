//! Dolphins (`Dolphin`): they breathe air, dry out on land, swim with swimming players, jump
//! out of the water, play with items and are fed raw fish.
//!
//! `DolphinSwimToTreasureGoal` needs `findNearestMapStructure` (structure locating is not
//! available to mobs yet): a fed dolphin starts it, finds no treasure, gets stuck and forgets
//! the fish like Vanilla does without a structure in range.

use vanillite_base::math::floor;
use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Vec3};
use vanillite_world::entity::{EntityFlags, EntityId, Rotation};

use super::{Goal, item_in_tag, st, st_mut};
use crate::item::EquipmentSlot;
use crate::mob::animal::{Interaction, use_item};
use crate::mob::common::CommonGoal;
use crate::mob::equipment;
use crate::mob::fam::{FamGoal, Interact};
use crate::mob::goals::{Goal as MobGoal, MeleeKind};
use crate::mob::kinds::{self, Kind};
use crate::mob::{MobData, Tick, combat, is_water, living, nav};

fn fam(g: Goal) -> MobGoal {
    MobGoal::Fam(FamGoal::Wildlife(g))
}

fn is_guardian(t: &Tick<'_, '_>, id: EntityId) -> bool {
    t.ents.mob(id).is_some_and(|m| m.kind == Kind::Guardian)
}

/// `Dolphin.registerGoals` (`FollowPlayerRiddenEntityGoal` for boats and nautiluses comes
/// with the navigation's shared goals).
pub(super) fn register_goals(m: &mut MobData) {
    let g = &mut m.goals;
    g.add(0, fam(Goal::BreathAir));
    g.add(0, fam(Goal::TryFindWater));
    g.add(1, fam(Goal::DolphinSwimToTreasure { stuck: false }));
    g.add(
        2,
        fam(Goal::DolphinSwimWithPlayer {
            speed: 4.0,
            player: None,
        }),
    );
    g.add(
        4,
        fam(Goal::Swim {
            speed: 1.0,
            interval: 10,
            fish: false,
            wanted: Vec3::ZERO,
        }),
    );
    g.add(4, MobGoal::random_look());
    g.add(5, MobGoal::look_at_player(6.0));
    g.add(
        5,
        fam(Goal::DolphinJump {
            interval: 10,
            breached: false,
        }),
    );
    g.add(6, MobGoal::melee(MeleeKind::Plain, 1.2, true));
    g.add(7, fam(Goal::DolphinMoveToItem { cooldown: 0 }));
    g.add(8, fam(Goal::DolphinPlayWithItems { cooldown: 0 }));
    g.add(
        9,
        MobGoal::Common(CommonGoal::avoid(is_guardian, 8.0, 1.0, 1.0)),
    );
    m.targets.add(1, MobGoal::hurt_by(true));
}

/// `Dolphin.tick` after `super.tick()`: moistness, drying out and flopping on land.
pub(super) fn after_tick(t: &mut Tick<'_, '_>) {
    if t.mob_ref().no_ai {
        t.mob().air = 4800;
        return;
    }
    let p = t.pos();
    let raining = (t.env.raining_at)(BlockPos::containing(p.x, p.y, p.z));
    if t.mob_ref().fluids.in_water() || raining {
        if st(t.mob_ref()).moistness != 2400 {
            st_mut(t.mob()).moistness = 2400;
            t.core_mut().mark_data_dirty();
        }
        return;
    }
    st_mut(t.mob()).moistness -= 1;
    t.core_mut().mark_data_dirty();
    if st(t.mob_ref()).moistness <= 0 {
        let kind = t.env.types.named("minecraft:dry_out");
        let source = crate::survival::damage::DamageSource::of(kind);
        living::hurt_self(t, source, 1.0);
        if !t.exists() {
            return;
        }
    }
    if t.on_ground() {
        let r = &mut t.mob().random;
        let x = f64::from((r.next_float() * 2.0 - 1.0) * 0.2);
        let z = f64::from((r.next_float() * 2.0 - 1.0) * 0.2);
        let y_rot = r.next_float() * 360.0;
        let v = t.velocity();
        t.set_velocity(Vec3::new(v.x + x, v.y + 0.5, v.z + z));
        let rot = t.core().rot();
        let mut core = t.core_mut();
        core.set_rot(Rotation { y_rot, ..rot });
        core.set_flags(EntityFlags::ON_GROUND, false);
        core.set_flags(EntityFlags::HAS_IMPULSE, true);
    }
}

/// `Dolphin.mobInteract`: raw fish grow babies or make it look for treasure.
pub(super) fn interact(ix: &mut Interact<'_, '_>) -> Interaction {
    if ix.stack.is_empty() || !item_in_tag(ix.stack.item, "minecraft:fishes") {
        return Interaction::Pass;
    }
    combat::mob_sound(ix.ents, ix.env, ix.target, "minecraft:entity.dolphin.eat");
    let Some(m) = ix.ents.mob(ix.target) else {
        return Interaction::Pass;
    };
    if m.baby {
        // `canAgeUp`: `ageUp(getSpeedUpSecondsWhenFeeding(-age), true)`.
        use_item(ix.ents, ix.player, ix.hand);
        if let Some(m) = ix.ents.mob_mut(ix.target) {
            #[allow(clippy::cast_possible_truncation)]
            let seconds = ((-m.age / 20) as f32 * 0.1) as i32;
            let new_age = (m.age + seconds * 20).min(0);
            m.forced_age += new_age - m.age;
            m.age = new_age;
            if m.forced_age_timer == 0 {
                m.forced_age_timer = 40;
            }
        }
    } else {
        if let Some(m) = ix.ents.mob_mut(ix.target) {
            st_mut(m).got_fish = true;
        }
        if let Some(mut e) = ix.ents.store.get_mut(ix.target) {
            e.mark_data_dirty();
        }
        use_item(ix.ents, ix.player, ix.hand);
    }
    Interaction::Success
}

/// `Dolphin.pickUpItem` in `Mob.aiStep`'s looting: an empty mouth takes the whole stack.
pub(super) fn pick_up_items(t: &mut Tick<'_, '_>) {
    if !t
        .mob_ref()
        .equipment
        .get(EquipmentSlot::MainHand)
        .is_empty()
    {
        return;
    }
    let rules = equipment::PickupRules::default();
    equipment::pick_up_items(t, &rules, Some(take_item), (1.0, 0.0, 1.0));
}

fn take_item(t: &mut Tick<'_, '_>, item: EntityId) -> bool {
    if !t
        .mob_ref()
        .equipment
        .get(EquipmentSlot::MainHand)
        .is_empty()
    {
        return false;
    }
    let Some(stack) = t.ents.item(item).map(|d| d.stack.clone()) else {
        return false;
    };
    let count = stack.count;
    let m = t.mob();
    m.equipment.set(EquipmentSlot::MainHand, stack);
    m.equipment.set_guaranteed_drop(EquipmentSlot::MainHand);
    equipment::broadcast(t);
    equipment::take(t, item, count);
    true
}

/// The dolphin's `ALLOWED_ITEMS` in its 8 block box, nearest first in store order.
fn first_item(t: &mut Tick<'_, '_>) -> Option<(EntityId, Vec3)> {
    let area = t.aabb().inflate(8.0, 8.0, 8.0);
    let mut found = None;
    let ents = &*t.ents;
    let level = t.level;
    ents.store.for_each_in(&area, false, |id| {
        if found.is_some() {
            return;
        }
        let (Some(d), Some(e)) = (ents.item(id), ents.store.get(id)) else {
            return;
        };
        let p = e.pos();
        if d.pickup_delay <= 0
            && !d.stack.is_empty()
            && is_water(level.block(BlockPos::containing(p.x, p.y, p.z)))
        {
            found = Some((id, p));
        }
    });
    found
}

/// `ItemGoal.dropItem`: throws the held item forward.
fn drop_item(t: &mut Tick<'_, '_>) -> bool {
    let stack = t.mob_ref().equipment.get(EquipmentSlot::MainHand).clone();
    if stack.is_empty() {
        return false;
    }
    let p = t.pos();
    let rot = t.core().rot();
    let r = &mut t.mob().random;
    let dir = r.next_float() * (std::f32::consts::PI * 2.0);
    let pow2 = 0.02 * r.next_float();
    let rad = |deg: f32| f64::from(deg * (std::f32::consts::PI / 180.0));
    let (sy, cy) = (
        vanillite_base::math::sin(rad(rot.y_rot)),
        vanillite_base::math::cos(rad(rot.y_rot)),
    );
    let (sx, cx) = (
        vanillite_base::math::sin(rad(rot.x_rot)),
        vanillite_base::math::cos(rad(rot.x_rot)),
    );
    let velocity = Vec3::new(
        f64::from(0.3 * -sy * cx + vanillite_base::math::cos(f64::from(dir)) * pow2),
        f64::from(0.3 * sx * 1.5),
        f64::from(0.3 * cy * cx + vanillite_base::math::sin(f64::from(dir)) * pow2),
    );
    let uuid = crate::mob::random_uuid(&mut *t.env.random);
    let y_rot = 0.0;
    let pos = Vec3::new(p.x, t.eye_y() - f64::from(0.3_f32), p.z);
    t.env.out.push(crate::survival::Effect::DropItem {
        uuid,
        pos,
        velocity,
        y_rot,
        pickup_delay: 40,
        stack,
    });
    t.mob()
        .equipment
        .set(EquipmentSlot::MainHand, crate::item::ItemStack::default());
    equipment::broadcast(t);
    true
}

/// `Entity.getMotionDirection`: the horizontal direction of the yaw as (x, z) steps.
fn motion_steps(y_rot: f32) -> (i32, i32) {
    match floor(f64::from(y_rot) / 90.0 + 0.5) & 3 {
        0 => (0, 1),
        1 => (-1, 0),
        2 => (0, -1),
        _ => (1, 0),
    }
}

/// `BreathAirGoal.givesAir`.
fn gives_air(t: &Tick<'_, '_>, pos: BlockPos) -> bool {
    let state = t.level.block(pos);
    (state.fluid_state().is_empty() || state.block() == vanillite_data::block::ids::BUBBLE_COLUMN)
        && state.collision_shape().is_empty()
}

/// `BreathAirGoal.findAirPosition`.
fn find_air(t: &mut Tick<'_, '_>) {
    let b = t.block_pos();
    let mut dest = None;
    for y in b.y..=b.y + 8 {
        let pos = BlockPos::new(b.x, y, b.z);
        if gives_air(t, pos) {
            dest = Some(pos);
            break;
        }
    }
    let p = t.pos();
    let dest = dest.unwrap_or_else(|| BlockPos::containing(p.x, p.y + 8.0, p.z));
    let at = Vec3::new(f64::from(dest.x), f64::from(dest.y + 1), f64::from(dest.z));
    nav::move_to_pos(t, at, 1.0);
}

/// The nearest swimming player within 10 blocks (`SWIM_WITH_PLAYER_TARGETING`).
fn swimming_player(t: &Tick<'_, '_>) -> Option<EntityId> {
    let p = t.pos();
    let area = t.aabb().inflate(10.0, 10.0, 10.0);
    let mut best: Option<(EntityId, f64)> = None;
    let ents = &*t.ents;
    ents.store.for_each_in(&area, true, |id| {
        let (Some(e), Some(pd)) = (ents.store.get(id), ents.player(id)) else {
            return;
        };
        if pd.game_mode == vanillite_config::GameMode::Spectator || pd.survival.is_dead_or_dying() {
            return;
        }
        let d = e.pos().distance_to_sqr(p);
        if d <= 100.0 && best.is_none_or(|(_, b)| d < b) {
            best = Some((id, d));
        }
    });
    best.map(|(id, _)| id)
}

fn is_swimming(t: &Tick<'_, '_>, player: EntityId) -> bool {
    t.ents
        .store
        .get(player)
        .is_some_and(|e| e.flags().contains(EntityFlags::SWIMMING))
}

pub(super) fn can_use(t: &mut Tick<'_, '_>, g: &mut Goal) -> bool {
    match g {
        Goal::BreathAir => t.mob_ref().air < 140,
        Goal::TryFindWater => t.on_ground() && !is_water(t.level.block(t.block_pos())),
        Goal::DolphinSwimToTreasure { .. } => st(t.mob_ref()).got_fish && t.mob_ref().air >= 100,
        Goal::DolphinSwimWithPlayer { player, .. } => {
            *player = swimming_player(t);
            player.is_some_and(|p| is_swimming(t, p) && t.mob_ref().target != Some(p))
        }
        Goal::DolphinJump { interval, .. } => {
            if t.mob().random.next_int_bounded(*interval) != 0 {
                return false;
            }
            let (sx, sz) = motion_steps(t.core().rot().y_rot);
            let b = t.block_pos();
            for i in [0, 1, 4, 5, 6, 7] {
                let next = BlockPos::new(b.x + sx * i, b.y, b.z + sz * i);
                let water = is_water(t.level.block(next));
                let above1 = t
                    .level
                    .block(BlockPos::new(next.x, next.y + 1, next.z))
                    .is_air();
                let above2 = t
                    .level
                    .block(BlockPos::new(next.x, next.y + 2, next.z))
                    .is_air();
                if !water || !above1 || !above2 {
                    return false;
                }
            }
            true
        }
        Goal::DolphinMoveToItem { cooldown } => {
            if *cooldown > t.mob_ref().tick_count {
                return false;
            }
            first_item(t).is_some()
        }
        Goal::DolphinPlayWithItems { .. } => !t
            .mob_ref()
            .equipment
            .get(EquipmentSlot::MainHand)
            .is_empty(),
        _ => false,
    }
}

pub(super) fn can_continue(t: &mut Tick<'_, '_>, g: &mut Goal) -> bool {
    match g {
        Goal::DolphinSwimToTreasure { stuck } => {
            let Some(pos) = st(t.mob_ref()).treasure_pos else {
                return false;
            };
            let p = t.pos();
            let at = BlockPos::containing(f64::from(pos.x), p.y, f64::from(pos.z));
            !super::closer_to_center(at, p, 4.0) && !*stuck && t.mob_ref().air >= 100
        }
        Goal::DolphinSwimWithPlayer { player, .. } => player.is_some_and(|p| {
            is_swimming(t, p)
                && t.other(p)
                    .is_some_and(|o| o.pos.distance_to_sqr(t.pos()) < 256.0)
        }),
        Goal::DolphinJump { .. } => {
            let v = t.velocity();
            let x_rot = t.core().rot().x_rot;
            (v.y * v.y >= f64::from(0.03_f32)
                || x_rot == 0.0
                || x_rot.abs() >= 10.0
                || !t.mob_ref().fluids.in_water())
                && !t.on_ground()
        }
        Goal::TryFindWater => false,
        _ => can_use(t, g),
    }
}

pub(super) fn start(t: &mut Tick<'_, '_>, g: &mut Goal) {
    match g {
        Goal::BreathAir => {
            t.mob().nav.stop();
            find_air(t);
        }
        Goal::TryFindWater => {
            let p = t.pos();
            let (x0, y0, z0) = (floor(p.x - 2.0), floor(p.y - 2.0), floor(p.z - 2.0));
            let (x1, y1, z1) = (floor(p.x + 2.0), t.block_pos().y, floor(p.z + 2.0));
            let mut found = None;
            'search: for y in y0..=y1 {
                for z in z0..=z1 {
                    for x in x0..=x1 {
                        let pos = BlockPos::new(x, y, z);
                        if is_water(t.level.block(pos)) {
                            found = Some(pos);
                            break 'search;
                        }
                    }
                }
            }
            if let Some(pos) = found {
                let at = Vec3::new(f64::from(pos.x), f64::from(pos.y), f64::from(pos.z));
                t.mob().move_ctl.set_wanted_position(at, 1.0);
            }
        }
        Goal::DolphinSwimToTreasure { stuck } => {
            // `findNearestMapStructure(#dolphin_located)` is not available: no treasure.
            t.mob().nav.stop();
            *stuck = true;
            st_mut(t.mob()).treasure_pos = None;
        }
        Goal::DolphinSwimWithPlayer { .. } => {
            // `addEffect(DOLPHINS_GRACE)` on the player: players get effects with #74.
        }
        Goal::DolphinJump { .. } => {
            let (sx, sz) = motion_steps(t.core().rot().y_rot);
            let v = t.velocity();
            t.set_velocity(Vec3::new(
                v.x + f64::from(sx) * 0.6,
                v.y + 0.7,
                v.z + f64::from(sz) * 0.6,
            ));
            t.mob().nav.stop();
        }
        Goal::DolphinMoveToItem { cooldown } => {
            if let Some((_, pos)) = first_item(t) {
                nav::move_to_entity(t, pos, 1, f64::from(1.2_f32));
                kinds::play_sound(t, "minecraft:entity.dolphin.play", 1.0, 1.0);
            }
            *cooldown = 0;
        }
        _ => {}
    }
}

pub(super) fn stop(t: &mut Tick<'_, '_>, g: &mut Goal) {
    match g {
        Goal::DolphinSwimToTreasure { stuck } => {
            let p = t.pos();
            let reached = st(t.mob_ref()).treasure_pos.is_some_and(|pos| {
                let at = BlockPos::containing(f64::from(pos.x), p.y, f64::from(pos.z));
                super::closer_to_center(at, p, 4.0)
            });
            if st(t.mob_ref()).treasure_pos.is_none() || reached || *stuck {
                st_mut(t.mob()).got_fish = false;
                t.core_mut().mark_data_dirty();
            }
        }
        Goal::DolphinSwimWithPlayer { player, .. } => {
            *player = None;
            t.mob().nav.stop();
        }
        Goal::DolphinJump { breached, .. } => {
            *breached = false;
            let rot = t.core().rot();
            t.core_mut().set_rot(Rotation { x_rot: 0.0, ..rot });
        }
        Goal::DolphinPlayWithItems { .. } => {
            drop_item(t);
        }
        Goal::DolphinMoveToItem { cooldown } => stop_move_to_item(t, cooldown),
        _ => {}
    }
}

pub(super) fn tick(t: &mut Tick<'_, '_>, g: &mut Goal) {
    match g {
        Goal::BreathAir => {
            find_air(t);
            let m = t.mob_ref();
            let input = Vec3::new(f64::from(m.xxa), f64::from(m.yya), f64::from(m.zza));
            living::move_relative(t, 0.02, input);
            let v = t.velocity();
            living::move_self(t, v);
        }
        Goal::DolphinSwimWithPlayer { speed, player } => {
            let Some(o) = player.and_then(|p| t.other(p)) else {
                return;
            };
            t.mob()
                .look
                .set_look_at(Vec3::new(o.pos.x, o.eye_y, o.pos.z), 21.0, 1.0);
            if t.pos().distance_to_sqr(o.pos) < 6.25 {
                t.mob().nav.stop();
            } else {
                nav::move_to_entity(t, o.pos, 1, *speed);
            }
        }
        Goal::DolphinJump { breached, .. } => {
            let already = *breached;
            if !already {
                *breached = is_water(t.level.block(t.block_pos()));
            }
            if *breached && !already {
                kinds::play_sound(t, "minecraft:entity.dolphin.jump", 1.0, 1.0);
            }
            let v = t.velocity();
            let rot = t.core().rot();
            if v.y * v.y < f64::from(0.03_f32) && rot.x_rot != 0.0 {
                let x = rot.x_rot + 0.2 * vanillite_base::math::wrap_degrees_f(0.0 - rot.x_rot);
                t.core_mut().set_rot(Rotation { x_rot: x, ..rot });
            } else if v.length() > f64::from(1.0e-5_f32) {
                let h = (v.x * v.x + v.z * v.z).sqrt();
                #[allow(clippy::cast_possible_truncation)]
                let x = ((-v.y).atan2(h) * f64::from(57.295_776_f32)) as f32;
                t.core_mut().set_rot(Rotation { x_rot: x, ..rot });
            }
        }
        Goal::DolphinMoveToItem { .. } => {
            if let Some((_, pos)) = first_item(t) {
                nav::move_to_entity(t, pos, 1, f64::from(1.2_f32));
            }
        }
        Goal::DolphinPlayWithItems { .. } => {
            drop_item(t);
        }
        _ => {}
    }
}

/// `MoveToItemGoal.stop`: drops the item and waits up to 5 seconds.
fn stop_move_to_item(t: &mut Tick<'_, '_>, cooldown: &mut i32) {
    if drop_item(t) {
        *cooldown = t.mob_ref().tick_count + t.mob().random.next_int_bounded(100);
    }
}

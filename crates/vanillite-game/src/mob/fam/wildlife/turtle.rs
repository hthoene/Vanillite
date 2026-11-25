//! Turtles (`Turtle`): they travel the seas, return to their home beach to lay eggs after
//! breeding and drop a scute when a baby grows up.

use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Vec3};
use vanillite_world::entity::Rotation;

use super::{Goal, SEA_LEVEL, block_tags, bright_enough, closer_to_center, rotlerp, st, st_mut};
use crate::level::Level;
use crate::mob::common::MoveToBlock;
use crate::mob::fam::FamGoal;
use crate::mob::goals::{Goal as MobGoal, TemptItems, distance_sqr, reduced};
use crate::mob::nav::Operation;
use crate::mob::{Environment, MobData, Tick, WorldAction, is_water, nav, randompos};
use crate::survival::damage::tag;
use crate::survival::rules::Rule;

fn fam(g: Goal) -> MobGoal {
    MobGoal::Fam(FamGoal::Wildlife(g))
}

/// `Turtle.registerGoals`.
pub(super) fn register_goals(m: &mut MobData) {
    let g = &mut m.goals;
    g.add(
        0,
        fam(Goal::TurtlePanic {
            speed: 1.2,
            pos: Vec3::ZERO,
        }),
    );
    g.add(
        1,
        fam(Goal::TurtleBreed {
            speed: 1.0,
            partner: None,
            love_time: 0,
        }),
    );
    g.add(1, fam(Goal::TurtleLayEgg(MoveToBlock::new(1.0, 16, 1))));
    g.add(2, MobGoal::tempt(1.1, TemptItems::Food));
    let mut to_water = MoveToBlock::new(1.0, 24, 1);
    to_water.vertical_search_start = -1;
    g.add(3, fam(Goal::TurtleGoToWater(to_water)));
    g.add(
        4,
        fam(Goal::TurtleGoHome {
            speed: 1.0,
            stuck: false,
            close_ticks: 0,
        }),
    );
    g.add(
        7,
        fam(Goal::TurtleTravel {
            speed: 1.0,
            stuck: false,
        }),
    );
    g.add(8, MobGoal::look_at_player(8.0));
    g.add(
        9,
        fam(Goal::TurtleStroll {
            speed: 1.0,
            interval: 100,
            wanted: Vec3::ZERO,
        }),
    );
}

/// `TurtleEggBlock.isSand`.
fn is_sand(level: &Level, pos: BlockPos) -> bool {
    block_tags().sand.contains(level.block(pos))
}

/// `TurtleEggBlock.onSand`: sand below.
fn on_sand(level: &Level, pos: BlockPos) -> bool {
    is_sand(level, BlockPos::new(pos.x, pos.y - 1, pos.z))
}

/// `Turtle.checkTurtleSpawnRules`: beaches up to 3 blocks above the sea.
pub(super) fn spawn_rules(level: &Level, pos: BlockPos) -> bool {
    pos.y < SEA_LEVEL + 4 && on_sand(level, pos) && bright_enough(level, pos)
}

/// `Turtle.getWalkTargetValue`: water (unless going home) and sand are best.
pub(super) fn walk_target_value(
    level: &Level,
    env: &Environment,
    pos: BlockPos,
    going_home: bool,
) -> f64 {
    if !going_home && is_water(level.block(pos)) {
        return 10.0;
    }
    if on_sand(level, pos) {
        10.0
    } else {
        f64::from(0.5 - crate::mob::light_magic_value(level, env, pos))
    }
}

/// `Turtle.aiStep`: the egg laying particles; a scute when a baby grows up
/// (`ageBoundaryReached`, `gameplay/turtle_grow`).
pub(super) fn ai_step(t: &mut Tick<'_, '_>, grown: bool) {
    let s = st(t.mob_ref());
    if t.mob_ref().is_alive()
        && s.laying_egg
        && s.lay_egg_counter >= 1
        && s.lay_egg_counter % 5 == 0
    {
        let pos = t.block_pos();
        if on_sand(t.level, pos) {
            let below = t.level.block(BlockPos::new(pos.x, pos.y - 1, pos.z));
            t.cx.actions
                .push(WorldAction::LevelEvent(2001, pos, i32::from(below.0)));
        }
    }
    if grown && t.env.rules.bool(Rule::MOB_DROPS) {
        let scute = crate::mob::equipment::stack("minecraft:turtle_scute");
        crate::mob::equipment::spawn_at_location(t, scute, 0.0);
    }
}

/// `TurtleMoveControl.tick`.
pub(super) fn move_control(t: &mut Tick<'_, '_>) {
    // `updateSpeed`.
    let in_water = t.mob_ref().fluids.in_water();
    if in_water {
        let v = t.velocity();
        t.set_velocity(Vec3::new(v.x, v.y + 0.005, v.z));
        let home = st(t.mob_ref()).home_pos;
        if !closer_to_center(home, t.pos(), 16.0) {
            let m = t.mob();
            m.speed = (m.speed / 2.0).max(0.08);
        }
        if t.mob_ref().baby {
            let m = t.mob();
            m.speed = (m.speed / 3.0).max(0.06);
        }
    } else if t.on_ground() {
        let m = t.mob();
        m.speed = (m.speed / 2.0).max(0.06);
    }
    let ctl = t.mob_ref().move_ctl;
    if ctl.operation != Operation::MoveTo || t.mob_ref().nav.is_done() {
        t.mob().speed = 0.0;
        return;
    }
    let p = t.pos();
    let (xd, mut yd, zd) = (ctl.wanted.x - p.x, ctl.wanted.y - p.y, ctl.wanted.z - p.z);
    let dd = (xd * xd + yd * yd + zd * zd).sqrt();
    if dd < f64::from(1.0e-5_f32) {
        t.mob().speed = 0.0;
        return;
    }
    yd /= dd;
    #[allow(clippy::cast_possible_truncation)]
    let y_rot_d = crate::mob::atan2_deg(zd, xd) as f32 - 90.0;
    let rot = t.core().rot();
    let y = rotlerp(rot.y_rot, y_rot_d, 90.0);
    t.core_mut().set_rot(Rotation { y_rot: y, ..rot });
    #[allow(clippy::cast_possible_truncation)]
    let target = (ctl.speed_modifier * t.mob_ref().attrs.movement_speed) as f32;
    let m = t.mob();
    m.y_body_rot = y;
    m.speed += 0.125 * (target - m.speed);
    let speed = m.speed;
    let v = t.velocity();
    t.set_velocity(Vec3::new(v.x, v.y + f64::from(speed) * yd * 0.1, v.z));
}

/// `PanicGoal.lookForWater(level, mob, range)`: the closest water within the range.
fn look_for_water(t: &Tick<'_, '_>, range: i32) -> Option<BlockPos> {
    let pos = t.block_pos();
    if !t.level.block(pos).collision_shape().is_empty() {
        return None;
    }
    let mut best: Option<(BlockPos, i32)> = None;
    for dy in -1_i32..=1 {
        for dx in -range..=range {
            for dz in -range..=range {
                let d = dx.abs() + dy.abs() + dz.abs();
                if best.is_some_and(|(_, b)| d >= b) {
                    continue;
                }
                let p = BlockPos::new(pos.x + dx, pos.y + dy, pos.z + dz);
                if is_water(t.level.block(p)) {
                    best = Some((p, d));
                }
            }
        }
    }
    best.map(|(p, _)| p)
}

/// `TurtleLayEggGoal.isValidTarget`: sand with air above.
fn egg_target(t: &Tick<'_, '_>, pos: BlockPos) -> bool {
    t.level
        .block(BlockPos::new(pos.x, pos.y + 1, pos.z))
        .is_air()
        && is_sand(t.level, pos)
}

/// `TurtleGoToWaterGoal.isValidTarget`.
fn water_target(t: &Tick<'_, '_>, pos: BlockPos) -> bool {
    t.level.block(pos).block() == vanillite_data::block::ids::WATER
}

fn home_within(t: &Tick<'_, '_>, dist: f64) -> bool {
    closer_to_center(st(t.mob_ref()).home_pos, t.pos(), dist)
}

pub(super) fn can_use(t: &mut Tick<'_, '_>, g: &mut Goal) -> bool {
    let has_egg = st(t.mob_ref()).has_egg;
    let going_home = st(t.mob_ref()).going_home;
    let in_water = t.mob_ref().fluids.in_water();
    match g {
        Goal::TurtlePanic { pos, .. } => {
            let panics = t
                .mob_ref()
                .last_damage_source(t.game_time())
                .is_some_and(|k| t.env.types.is(k, tag::PANIC_CAUSES));
            if !panics {
                return false;
            }
            if let Some(w) = look_for_water(t, 7) {
                *pos = Vec3::new(f64::from(w.x), f64::from(w.y), f64::from(w.z));
                return true;
            }
            match randompos::default_pos(t, 5, 4) {
                Some(p) => {
                    *pos = p;
                    true
                }
                None => false,
            }
        }
        Goal::TurtleBreed { partner, .. } => {
            if t.mob_ref().in_love <= 0 || has_egg {
                return false;
            }
            *partner = crate::mob::animal::free_partner(t);
            partner.is_some()
        }
        Goal::TurtleLayEgg(s) => {
            has_egg
                && home_within(t, 9.0)
                && s.can_use(t, egg_target, MoveToBlock::default_next_start_tick)
        }
        Goal::TurtleGoToWater(s) => {
            let baby = t.mob_ref().baby;
            if (baby && !in_water) || (!going_home && !in_water && !has_egg) {
                s.can_use(t, water_target, MoveToBlock::default_next_start_tick)
            } else {
                false
            }
        }
        Goal::TurtleGoHome { .. } => {
            if t.mob_ref().baby {
                return false;
            }
            if has_egg {
                return true;
            }
            if t.mob().random.next_int_bounded(reduced(700)) != 0 {
                return false;
            }
            !home_within(t, 64.0)
        }
        Goal::TurtleTravel { .. } => !going_home && !has_egg && in_water,
        Goal::TurtleStroll {
            interval, wanted, ..
        } => {
            if in_water || going_home || has_egg {
                return false;
            }
            if t.mob_ref().no_action_time >= 100 {
                return false;
            }
            if t.mob().random.next_int_bounded(reduced(*interval)) != 0 {
                return false;
            }
            match randompos::default_pos(t, 10, 7) {
                Some(p) => {
                    *wanted = p;
                    true
                }
                None => false,
            }
        }
        _ => false,
    }
}

pub(super) fn can_continue(t: &mut Tick<'_, '_>, g: &mut Goal) -> bool {
    let s = st(t.mob_ref()).clone();
    match g {
        Goal::TurtlePanic { .. } | Goal::TurtleStroll { .. } => !t.mob_ref().nav.is_done(),
        Goal::TurtleBreed {
            partner, love_time, ..
        } => {
            partner.is_some_and(|p| {
                t.ents.mob(p).is_some_and(|m| {
                    m.is_alive() && m.in_love > 0 && !crate::mob::animal::is_panicking(m)
                })
            }) && *love_time < 60
        }
        Goal::TurtleLayEgg(m) => m.can_continue(t, egg_target) && s.has_egg && home_within(t, 9.0),
        Goal::TurtleGoToWater(m) => {
            !t.mob_ref().fluids.in_water() && m.try_ticks <= 1200 && water_target(t, m.block_pos)
        }
        Goal::TurtleGoHome {
            stuck, close_ticks, ..
        } => !home_within(t, 7.0) && !*stuck && *close_ticks <= reduced(600),
        Goal::TurtleTravel { stuck, .. } => {
            !t.mob_ref().nav.is_done()
                && !*stuck
                && !s.going_home
                && t.mob_ref().in_love <= 0
                && !s.has_egg
        }
        _ => false,
    }
}

pub(super) fn start(t: &mut Tick<'_, '_>, g: &mut Goal) {
    match g {
        Goal::TurtlePanic { speed, pos } => {
            nav::move_to_pos(t, *pos, *speed);
        }
        Goal::TurtleLayEgg(m) | Goal::TurtleGoToWater(m) => m.start(t),
        Goal::TurtleGoHome {
            stuck, close_ticks, ..
        } => {
            st_mut(t.mob()).going_home = true;
            *stuck = false;
            *close_ticks = 0;
        }
        Goal::TurtleTravel { stuck, .. } => {
            let p = t.pos();
            let r = &mut t.mob().random;
            let xt = r.next_int_bounded(1025) - 512;
            let mut yt = r.next_int_bounded(9) - 4;
            let zt = r.next_int_bounded(1025) - 512;
            if f64::from(yt) + p.y > f64::from(SEA_LEVEL - 1) {
                yt = 0;
            }
            st_mut(t.mob()).travel_pos = Some(BlockPos::containing(
                f64::from(xt) + p.x,
                f64::from(yt) + p.y,
                f64::from(zt) + p.z,
            ));
            *stuck = false;
        }
        Goal::TurtleStroll { speed, wanted, .. } => {
            nav::move_to_pos(t, *wanted, *speed);
        }
        _ => {}
    }
}

pub(super) fn stop(t: &mut Tick<'_, '_>, g: &mut Goal) {
    match g {
        Goal::TurtleGoHome { .. } => st_mut(t.mob()).going_home = false,
        Goal::TurtleTravel { .. } => st_mut(t.mob()).travel_pos = None,
        Goal::TurtleBreed {
            partner, love_time, ..
        } => {
            *partner = None;
            *love_time = 0;
        }
        _ => {}
    }
}

/// Sets `LAYING_EGG` and the egg counter.
fn set_laying_egg(t: &mut Tick<'_, '_>, on: bool) {
    let s = st_mut(t.mob());
    s.lay_egg_counter = i32::from(on);
    s.laying_egg = on;
    t.core_mut().mark_data_dirty();
}

/// `TurtleBreedGoal.breed`: the turtle carries an egg instead of a baby.
fn breed(t: &mut Tick<'_, '_>, partner: vanillite_world::entity::EntityId) {
    st_mut(t.mob()).has_egg = true;
    for id in [t.id, partner] {
        if let Some(m) = t.ents.mob_mut(id) {
            m.age = 6000;
            m.in_love = 0;
            m.love_cause = None;
        }
    }
    t.core_mut().mark_data_dirty();
    if t.env.rules.bool(Rule::MOB_DROPS) {
        let xp = t.mob().random.next_int_bounded(7) + 1;
        let pos = t.pos();
        crate::mob::combat::award_experience(t.env, pos, xp);
    }
}

pub(super) fn tick(t: &mut Tick<'_, '_>, g: &mut Goal) {
    match g {
        Goal::TurtleBreed {
            speed,
            partner,
            love_time,
        } => {
            let Some(p) = *partner else {
                return;
            };
            let Some(o) = t.other(p) else {
                return;
            };
            t.mob()
                .look
                .set_look_at(Vec3::new(o.pos.x, o.eye_y, o.pos.z), 10.0, 40.0);
            nav::move_to_entity(t, o.pos, 0, *speed);
            *love_time += 1;
            if *love_time >= reduced(60) && distance_sqr(t, o.pos) < 9.0 {
                breed(t, p);
            }
        }
        Goal::TurtleLayEgg(m) => {
            let b = m.block_pos;
            m.tick(t, BlockPos::new(b.x, b.y + 1, b.z), 1.0);
            if t.mob_ref().fluids.in_water() || !m.reached_target {
                return;
            }
            let counter = st(t.mob_ref()).lay_egg_counter;
            if counter < 1 {
                set_laying_egg(t, true);
            } else if counter > 200 {
                let pitch = 0.9 + t.env.random.next_float() * 0.2;
                let pos = t.pos();
                let seed = crate::survival::sound_seed();
                crate::survival::play_sound(
                    t.env.out,
                    None,
                    pos,
                    "minecraft:entity.turtle.lay_egg",
                    vanillite_protocol::packets::play::clientbound::SoundSource::Blocks,
                    0.3,
                    pitch,
                    seed,
                );
                let eggs = t.mob().random.next_int_bounded(4) + 1;
                let egg_pos = BlockPos::new(b.x, b.y + 1, b.z);
                let state = vanillite_data::block::ids::TURTLE_EGG.default_state();
                let state = state.with("eggs", &eggs.to_string()).unwrap_or(state);
                t.cx.actions.push(WorldAction::SetBlock(egg_pos, state, 3));
                st_mut(t.mob()).has_egg = false;
                set_laying_egg(t, false);
                t.mob().in_love = 600;
            }
            if st(t.mob_ref()).laying_egg {
                st_mut(t.mob()).lay_egg_counter += 1;
            }
        }
        Goal::TurtleGoToWater(m) => {
            let b = m.block_pos;
            m.tick_with(t, BlockPos::new(b.x, b.y + 1, b.z), 1.0, 160);
        }
        Goal::TurtleGoHome {
            speed,
            stuck,
            close_ticks,
        } => {
            let home = st(t.mob_ref()).home_pos;
            let close = closer_to_center(home, t.pos(), 16.0);
            if close {
                *close_ticks += 1;
            }
            if !t.mob_ref().nav.is_done() {
                return;
            }
            let home_v = randompos::bottom_center(home);
            let pi10 = f64::from(std::f32::consts::PI / 10.0);
            let mut next = randompos::default_pos_towards(t, 16, 3, home_v, pi10)
                .or_else(|| randompos::default_pos_towards(t, 8, 7, home_v, randompos::half_pi()));
            if let Some(n) = next
                && !close
                && t.level.block(BlockPos::containing(n.x, n.y, n.z)).block()
                    != vanillite_data::block::ids::WATER
            {
                next = randompos::default_pos_towards(t, 16, 5, home_v, randompos::half_pi());
            }
            match next {
                Some(n) => {
                    nav::move_to_pos(t, n, *speed);
                }
                None => *stuck = true,
            }
        }
        Goal::TurtleTravel { speed, stuck } => {
            let Some(travel) = st(t.mob_ref()).travel_pos else {
                *stuck = true;
                return;
            };
            if !t.mob_ref().nav.is_done() {
                return;
            }
            let target = randompos::bottom_center(travel);
            let pi10 = f64::from(std::f32::consts::PI / 10.0);
            let mut next = randompos::default_pos_towards(t, 16, 3, target, pi10)
                .or_else(|| randompos::default_pos_towards(t, 8, 7, target, randompos::half_pi()));
            if let Some(n) = next {
                let (xc, zc) = (
                    vanillite_base::math::floor(n.x),
                    vanillite_base::math::floor(n.z),
                );
                let loaded = [(-34, -34), (34, -34), (-34, 34), (34, 34)]
                    .iter()
                    .all(|&(dx, dz)| t.level.is_loaded(BlockPos::new(xc + dx, 0, zc + dz)));
                if !loaded {
                    next = None;
                }
            }
            match next {
                Some(n) => {
                    nav::move_to_pos(t, n, *speed);
                }
                None => *stuck = true,
            }
        }
        _ => {}
    }
}

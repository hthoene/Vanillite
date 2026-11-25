//! Phantoms (`Phantom`): they circle an anchor high above and swoop down on players who have
//! not slept, and the `PhantomSpawner` that brings them.

use vanillite_base::random::{LegacyRandom, RandomSource};
use vanillite_base::{BlockPos, Vec3};
use vanillite_chunk::HeightmapKind;
use vanillite_nbt::{Compound, Tag};
use vanillite_world::entity::{Dimensions, EntityId};

use super::{Goal, State, goal, level_event, sound, state, state_mut};
use crate::level::Level;
use crate::mob::animal::Interaction;
use crate::mob::fam::{Hurt, Interact, SpawnContext, SpawnReason};
use crate::mob::goals::{self, flag};
use crate::mob::nav::{LookKind, MoveKind, NavKind};
use crate::mob::{Environment, Kind, MobData, Tick};
use crate::player::encode::DataWriter;
use crate::survival::rules::Difficulty;

/// `Phantom.AttackPhase`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AttackPhase {
    #[default]
    Circle,
    Swoop,
}

/// The state of a phantom.
#[derive(Clone, Debug)]
pub struct PhantomState {
    /// `ID_SIZE` (0–64).
    pub size: i32,
    pub anchor: Option<BlockPos>,
    pub move_target: Vec3,
    pub phase: AttackPhase,
    /// `PhantomMoveControl.speed`.
    pub speed: f32,
}

impl Default for PhantomState {
    fn default() -> Self {
        PhantomState {
            size: 0,
            anchor: None,
            move_target: Vec3::ZERO,
            phase: AttackPhase::Circle,
            speed: 0.1,
        }
    }
}

fn ps_mut(m: &mut MobData) -> &mut PhantomState {
    let s = state_mut(m);
    if !matches!(s, State::Phantom(_)) {
        *s = State::Phantom(PhantomState::default());
    }
    match s {
        State::Phantom(p) => p,
        _ => unreachable!("the phantom state was just set"),
    }
}

fn ps(m: &MobData) -> PhantomState {
    match state(m) {
        State::Phantom(p) => p.clone(),
        _ => PhantomState::default(),
    }
}

/// `setPhantomSize`: the size sets the attack damage.
pub fn set_size(m: &mut MobData, size: i32) {
    let size = size.clamp(0, 64);
    ps_mut(m).size = size;
    m.attrs.attack_damage = f64::from(6 + size);
}

// --- Properties -------------------------------------------------------------------------

pub fn ambient_sound(_m: &MobData) -> Option<&'static str> {
    Some("minecraft:entity.phantom.ambient")
}

pub fn hurt_sound(_m: &MobData) -> &'static str {
    "minecraft:entity.phantom.hurt"
}

pub fn death_sound(_m: &MobData) -> &'static str {
    "minecraft:entity.phantom.death"
}

pub fn experience_reward(m: &mut MobData) -> i32 {
    super::zombies::equipment_experience(m, 5)
}

/// The type's dimensions scaled by `1 + 0.15 · size`.
pub fn dimensions(m: &MobData) -> Option<Dimensions> {
    let d = Dimensions::of(m.kind.type_id());
    #[allow(clippy::cast_precision_loss)]
    let scale = 1.0 + 0.15 * ps(m).size as f32;
    Some(Dimensions {
        width: d.width * scale,
        height: d.height * scale,
    })
}

/// `Mob.checkMobSpawnRules` (phantoms come from their spawner).
pub fn check_spawn_rules(
    kind: Kind,
    level: &Level,
    _env: &Environment,
    pos: BlockPos,
    _random: &mut LegacyRandom,
    reason: SpawnReason,
) -> bool {
    super::zombies::mob_rules(kind, level, pos, reason)
}

// --- Setup ------------------------------------------------------------------------------

pub fn init(m: &mut MobData) {
    ps_mut(m);
    m.nav.kind = NavKind::None;
    m.move_ctl.kind = MoveKind::Custom;
    m.look.kind = LookKind::Custom;
    set_size(m, 0);
}

pub fn register_goals(m: &mut MobData) {
    m.goals
        .add(1, goal(Goal::PhantomAttackStrategy { next_sweep: 0 }));
    m.goals.add(
        2,
        goal(Goal::PhantomSweep {
            scared_of_cat: false,
            cat_search: 0,
        }),
    );
    m.goals.add(
        3,
        goal(Goal::PhantomCircle {
            angle: 0.0,
            distance: 0.0,
            height: 0.0,
            clockwise: 0.0,
        }),
    );
    m.targets.add(
        1,
        goal(Goal::PhantomTargetPlayer {
            next_scan: goals::reduced(20),
        }),
    );
}

/// `Phantom.finalizeSpawn`: the anchor five blocks above, size 0.
pub fn finalize_spawn(m: &mut MobData, ctx: &mut SpawnContext<'_>) {
    let p = ctx.pos;
    ps_mut(m).anchor = Some(BlockPos::new(p.x, p.y + 5, p.z));
    set_size(m, 0);
}

// --- Tick -------------------------------------------------------------------------------

pub fn before_tick(_t: &mut Tick<'_, '_>) {}

/// `PhantomBodyRotationControl`: the head follows the body, the body the yaw.
pub fn after_tick(t: &mut Tick<'_, '_>) {
    let body = t.mob_ref().y_body_rot;
    let mut rot = t.core().rot();
    rot.y_head_rot = body;
    t.core_mut().set_rot(rot);
    t.mob().y_body_rot = rot.y_rot;
}

pub fn ai_step(_t: &mut Tick<'_, '_>) {}

pub fn custom_server_ai_step(_t: &mut Tick<'_, '_>) {}

/// `PhantomMoveControl.tick`.
pub fn move_control(t: &mut Tick<'_, '_>) -> bool {
    if t.mob_ref().horizontal_collision {
        let mut rot = t.core().rot();
        rot.y_rot += 180.0;
        t.core_mut().set_rot(rot);
        ps_mut(t.mob()).speed = 0.1;
    }
    let target = ps(t.mob_ref()).move_target;
    let p = t.pos();
    let (mut tdx, tdy, mut tdz) = (target.x - p.x, target.y - p.y, target.z - p.z);
    let mut sd = (tdx * tdx + tdz * tdz).sqrt();
    if sd.abs() <= f64::from(1.0e-5_f32) {
        return true;
    }
    let scale = 1.0 - (tdy * f64::from(0.7_f32)).abs() / sd;
    tdx *= scale;
    tdz *= scale;
    sd = (tdx * tdx + tdz * tdz).sqrt();
    let sd2 = (tdx * tdx + tdz * tdz + tdy * tdy).sqrt();
    let mut rot = t.core().rot();
    let prev = rot.y_rot;
    #[allow(clippy::cast_possible_truncation)]
    let angle = tdz.atan2(tdx) as f32;
    let a = vanillite_base::math::wrap_degrees_f(rot.y_rot + 90.0);
    let b = vanillite_base::math::wrap_degrees_f(angle * (180.0 / std::f32::consts::PI));
    rot.y_rot = approach_degrees(a, b, 4.0) - 90.0;
    t.mob().y_body_rot = rot.y_rot;
    let mut speed = ps(t.mob_ref()).speed;
    if degrees_difference_abs(prev, rot.y_rot) < 3.0 {
        speed = approach(speed, 1.8, 0.005 * (1.8 / speed));
    } else {
        speed = approach(speed, 0.2, 0.025);
    }
    ps_mut(t.mob()).speed = speed;
    #[allow(clippy::cast_possible_truncation)]
    let x_rot = -(crate::mob::atan2_deg(-tdy, sd) as f32);
    rot.x_rot = x_rot;
    t.core_mut().set_rot(rot);
    let move_angle = (rot.y_rot + 90.0) * (std::f32::consts::PI / 180.0);
    let s = f64::from(speed);
    let txd = s * f64::from(vanillite_base::math::cos(f64::from(move_angle))) * (tdx / sd2).abs();
    let tzd = s * f64::from(vanillite_base::math::sin(f64::from(move_angle))) * (tdz / sd2).abs();
    let x_rad = f64::from(x_rot * (std::f32::consts::PI / 180.0));
    let tyd = s * f64::from(vanillite_base::math::sin(x_rad)) * (tdy / sd2).abs();
    let v = t.velocity();
    let want = Vec3::new(txd, tyd, tzd);
    t.set_velocity(v + (want - v) * 0.2);
    true
}

/// `Mth.approach`.
fn approach(value: f32, target: f32, step: f32) -> f32 {
    let step = step.abs();
    if value < target {
        (value + step).min(target)
    } else {
        (value - step).max(target)
    }
}

/// `Mth.approachDegrees`.
fn approach_degrees(current: f32, target: f32, step: f32) -> f32 {
    let diff = vanillite_base::math::wrap_degrees_f(target - current);
    approach(current, current + diff, step)
}

/// `Mth.degreesDifferenceAbs`.
fn degrees_difference_abs(a: f32, b: f32) -> f32 {
    vanillite_base::math::wrap_degrees_f(b - a).abs()
}

/// `travelFlying(input, 0.2)`.
pub fn travel(t: &mut Tick<'_, '_>, input: Vec3) -> bool {
    let fluids = t.mob_ref().fluids;
    let (speed, drag) = if fluids.in_water() {
        (0.02, f64::from(0.8_f32))
    } else if fluids.in_lava() {
        (0.02, 0.5)
    } else {
        (0.2, f64::from(0.91_f32))
    };
    crate::mob::living::move_relative(t, speed, input);
    let v = t.velocity();
    crate::mob::living::move_self(t, v);
    if t.exists() {
        let v = t.velocity();
        t.set_velocity(v * drag);
    }
    true
}

// --- Combat ---------------------------------------------------------------------------------

pub fn do_hurt_target(_t: &mut Tick<'_, '_>, _target: EntityId) -> Option<bool> {
    None
}

pub fn hurt_filter(_h: &mut Hurt<'_, '_>, amount: f32) -> Option<f32> {
    Some(amount)
}

pub fn after_hurt(_h: &mut Hurt<'_, '_>, _amount: f32) {}

pub fn on_die(_h: &mut Hurt<'_, '_>) {}

pub fn interact(_ix: &mut Interact<'_, '_>) -> Interaction {
    Interaction::Pass
}

pub fn pick_up_items(t: &mut Tick<'_, '_>) {
    let rules = crate::mob::equipment::PickupRules::default();
    crate::mob::equipment::pick_up_items(t, &rules, None, (1.0, 0.0, 1.0));
}

// --- Client data and storage ------------------------------------------------------------

pub fn encode_data(w: &mut DataWriter<'_>, m: &MobData) {
    w.int(16, ps(m).size, 0);
}

pub fn save(m: &MobData, out: &mut Compound) {
    let s = ps(m);
    if let Some(a) = s.anchor {
        out.insert("anchor_pos", vec![a.x, a.y, a.z]);
    }
    out.insert("size", s.size);
}

pub fn load(m: &mut MobData, tag: &Compound) {
    ps_mut(m).anchor = match tag.get("anchor_pos") {
        Some(Tag::IntArray(v)) if v.len() == 3 => Some(BlockPos::new(v[0], v[1], v[2])),
        _ => None,
    };
    let size = match tag.get("size") {
        Some(Tag::Int(v)) => *v,
        _ => 0,
    };
    set_size(m, size);
}

// --- Goals --------------------------------------------------------------------------------

pub fn goal_flags(g: &Goal) -> u8 {
    match g {
        Goal::PhantomSweep { .. } | Goal::PhantomCircle { .. } => flag::MOVE,
        _ => 0,
    }
}

pub fn goal_every_tick(_g: &Goal) -> bool {
    false
}

/// `TargetingConditions.DEFAULT` (combat, no range) against the target.
fn can_attack(t: &mut Tick<'_, '_>, target: EntityId) -> bool {
    crate::mob::common::test_combat(t, target, -1.0, false)
}

fn target_ok(t: &mut Tick<'_, '_>) -> bool {
    t.mob_ref().target.is_some_and(|id| can_attack(t, id))
}

/// `setAnchorAboveTarget`.
fn set_anchor_above_target(t: &mut Tick<'_, '_>) {
    if ps(t.mob_ref()).anchor.is_none() {
        return;
    }
    let Some(target) = t.mob_ref().target.and_then(|id| t.other(id)) else {
        return;
    };
    let tp = BlockPos::containing(target.pos.x, target.pos.y, target.pos.z);
    let up = 20 + t.mob().random.next_int_bounded(20);
    let mut anchor = BlockPos::new(tp.x, tp.y + up, tp.z);
    let sea = t.cx.env.sea_level;
    if anchor.y < sea {
        anchor = BlockPos::new(anchor.x, sea + 1, anchor.z);
    }
    ps_mut(t.mob()).anchor = Some(anchor);
}

/// `PhantomCircleAroundAnchorGoal.selectNext`.
fn select_next(t: &mut Tick<'_, '_>, angle: &mut f32, distance: f32, height: f32, clockwise: f32) {
    if ps(t.mob_ref()).anchor.is_none() {
        let p = t.block_pos();
        ps_mut(t.mob()).anchor = Some(p);
    }
    *angle += clockwise * 15.0 * (std::f32::consts::PI / 180.0);
    let a = ps(t.mob_ref()).anchor.unwrap_or_default();
    let target = Vec3::new(
        f64::from(a.x) + f64::from(distance * vanillite_base::math::cos(f64::from(*angle))),
        f64::from(a.y) + f64::from(-4.0 + height),
        f64::from(a.z) + f64::from(distance * vanillite_base::math::sin(f64::from(*angle))),
    );
    ps_mut(t.mob()).move_target = target;
}

fn touching_target(t: &Tick<'_, '_>) -> bool {
    ps(t.mob_ref()).move_target.distance_to_sqr(t.pos()) < 4.0
}

pub fn goal_can_use(t: &mut Tick<'_, '_>, g: &mut Goal) -> bool {
    match g {
        Goal::PhantomTargetPlayer { next_scan } => {
            if *next_scan > 0 {
                *next_scan -= 1;
                return false;
            }
            *next_scan = goals::reduced(60);
            let area = t.aabb().inflate(16.0, 64.0, 16.0);
            let mut found = std::mem::take(&mut t.cx.ids);
            found.clear();
            t.ents.store.for_each_in(&area, true, |id| found.push(id));
            let mut players: Vec<(EntityId, f64)> = found
                .iter()
                .filter(|&&id| t.ents.player(id).is_some())
                .filter_map(|&id| t.ents.store.get(id).map(|e| (id, e.pos().y)))
                .collect();
            t.cx.ids = found;
            players.sort_by(|a, b| b.1.total_cmp(&a.1));
            for (id, _) in players {
                if crate::mob::common::test_combat(t, id, 64.0, false) && can_attack(t, id) {
                    t.mob().target = Some(id);
                    return true;
                }
            }
            false
        }
        Goal::PhantomAttackStrategy { .. } => target_ok(t),
        Goal::PhantomSweep { .. } => {
            t.mob_ref().target.is_some() && ps(t.mob_ref()).phase == AttackPhase::Swoop
        }
        Goal::PhantomCircle { .. } => {
            t.mob_ref().target.is_none() || ps(t.mob_ref()).phase == AttackPhase::Circle
        }
        _ => false,
    }
}

pub fn goal_can_continue(t: &mut Tick<'_, '_>, g: &mut Goal) -> bool {
    match g {
        Goal::PhantomTargetPlayer { .. } | Goal::PhantomAttackStrategy { .. } => target_ok(t),
        Goal::PhantomSweep {
            scared_of_cat,
            cat_search,
        } => {
            let Some(target) = t.mob_ref().target else {
                return false;
            };
            if t.other(target).is_none() {
                return false;
            }
            if t.ents.player(target).is_some_and(|p| {
                matches!(
                    p.game_mode,
                    vanillite_config::GameMode::Creative | vanillite_config::GameMode::Spectator
                )
            }) {
                return false;
            }
            if ps(t.mob_ref()).phase != AttackPhase::Swoop {
                return false;
            }
            if t.mob_ref().tick_count > *cat_search {
                *cat_search = t.mob_ref().tick_count + 20;
                let area = t.aabb().inflate(16.0, 16.0, 16.0);
                let mut cats = false;
                t.ents.store.for_each_in(&area, false, |id| {
                    if t.ents
                        .mob(id)
                        .is_some_and(|m| m.kind == Kind::Cat && m.is_alive())
                    {
                        cats = true;
                    }
                });
                // `Cat.hiss` is a client animation of the cat.
                *scared_of_cat = cats;
            }
            !*scared_of_cat
        }
        _ => goal_can_use(t, g),
    }
}

pub fn goal_start(t: &mut Tick<'_, '_>, g: &mut Goal) {
    match g {
        Goal::PhantomAttackStrategy { next_sweep } => {
            *next_sweep = goals::reduced(10);
            ps_mut(t.mob()).phase = AttackPhase::Circle;
            set_anchor_above_target(t);
        }
        Goal::PhantomCircle {
            angle,
            distance,
            height,
            clockwise,
        } => {
            let r = &mut t.mob().random;
            *distance = 5.0 + r.next_float() * 10.0;
            *height = -4.0 + r.next_float() * 9.0;
            *clockwise = if r.next_boolean() { 1.0 } else { -1.0 };
            select_next(t, angle, *distance, *height, *clockwise);
        }
        _ => {}
    }
}

pub fn goal_stop(t: &mut Tick<'_, '_>, g: &mut Goal) {
    match g {
        Goal::PhantomAttackStrategy { .. } => {
            if let Some(a) = ps(t.mob_ref()).anchor {
                let top = surface(t.level, HeightmapKind::MotionBlocking, a.x, a.z);
                let up = 10 + t.mob().random.next_int_bounded(20);
                ps_mut(t.mob()).anchor = Some(BlockPos::new(a.x, top + up, a.z));
            }
        }
        Goal::PhantomSweep { .. } => {
            t.mob().target = None;
            ps_mut(t.mob()).phase = AttackPhase::Circle;
        }
        Goal::PhantomTargetPlayer { .. } => {}
        _ => {}
    }
}

/// `getHeightmapPos(type, pos).getY()`.
fn surface(level: &Level, kind: HeightmapKind, x: i32, z: i32) -> i32 {
    let Some(c) = level
        .chunks
        .chunk(vanillite_base::ChunkPos::new(x >> 4, z >> 4))
    else {
        return level.min_y();
    };
    let Some(h) = c.chunk.heightmaps().get(kind) else {
        return level.min_y();
    };
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_wrap)]
    let top = h.get((x & 15) as u32, (z & 15) as u32) as i32 + c.chunk.min_y();
    top
}

pub fn goal_tick(t: &mut Tick<'_, '_>, g: &mut Goal) {
    match g {
        Goal::PhantomAttackStrategy { next_sweep } => {
            if ps(t.mob_ref()).phase == AttackPhase::Circle {
                *next_sweep -= 1;
                if *next_sweep <= 0 {
                    ps_mut(t.mob()).phase = AttackPhase::Swoop;
                    set_anchor_above_target(t);
                    let secs = 8 + t.mob().random.next_int_bounded(4);
                    *next_sweep = goals::reduced(secs * 20);
                    let pitch = 0.95 + t.mob().random.next_float() * 0.1;
                    sound(t, "minecraft:entity.phantom.swoop", 10.0, pitch);
                }
            }
        }
        Goal::PhantomSweep { .. } => {
            let Some(target) = t.mob_ref().target else {
                return;
            };
            let Some(o) = t.other(target) else {
                return;
            };
            let mid = o.aabb.min_y + (o.aabb.max_y - o.aabb.min_y) * 0.5;
            ps_mut(t.mob()).move_target = Vec3::new(o.pos.x, mid, o.pos.z);
            if t.aabb().inflate(0.2, 0.2, 0.2).intersects(&o.aabb) {
                crate::mob::combat::mob_attack(t, target);
                if !t.exists() {
                    return;
                }
                ps_mut(t.mob()).phase = AttackPhase::Circle;
                if !t.mob_ref().silent {
                    let p = t.block_pos();
                    level_event(t.cx, 1039, p, 0);
                }
            } else if t.mob_ref().horizontal_collision || t.mob_ref().hurt_time > 0 {
                ps_mut(t.mob()).phase = AttackPhase::Circle;
            }
        }
        Goal::PhantomCircle {
            angle,
            distance,
            height,
            clockwise,
        } => {
            if t.mob().random.next_int_bounded(goals::reduced(350)) == 0 {
                *height = -4.0 + t.mob().random.next_float() * 9.0;
            }
            if t.mob().random.next_int_bounded(goals::reduced(250)) == 0 {
                *distance += 1.0;
                if *distance > 15.0 {
                    *distance = 5.0;
                    *clockwise = -*clockwise;
                }
            }
            if t.mob().random.next_int_bounded(goals::reduced(450)) == 0 {
                *angle = t.mob().random.next_float() * 2.0 * std::f32::consts::PI;
                select_next(t, angle, *distance, *height, *clockwise);
            }
            if touching_target(t) {
                select_next(t, angle, *distance, *height, *clockwise);
            }
            let p = t.block_pos();
            let target_y = ps(t.mob_ref()).move_target.y;
            if target_y < t.pos().y && !t.level.block(BlockPos::new(p.x, p.y - 1, p.z)).is_air() {
                *height = height.max(1.0);
                select_next(t, angle, *distance, *height, *clockwise);
            }
            let target_y = ps(t.mob_ref()).move_target.y;
            if target_y > t.pos().y && !t.level.block(BlockPos::new(p.x, p.y + 1, p.z)).is_air() {
                *height = height.min(-1.0);
                select_next(t, angle, *distance, *height, *clockwise);
            }
        }
        _ => {}
    }
}

// --- Spawner --------------------------------------------------------------------------------

/// The state of a world's `PhantomSpawner`.
#[derive(Clone, Copy, Debug, Default)]
pub struct PhantomSpawner {
    pub next_tick: i32,
}

/// `PhantomSpawner.tick`: every one to two minutes, players who have not slept for three
/// days (`time_since_rest`, the `minecraft:time_since_rest` statistic in ticks) may get
/// phantoms 20–34 blocks above them at night. `spawn_enemies` is the world's monster
/// spawning; `random` the level's random.
#[allow(clippy::too_many_arguments)]
pub fn phantom_spawner_tick(
    spawner: &mut PhantomSpawner,
    level: &mut Level,
    env: &Environment,
    difficulty: Difficulty,
    spawn_phantoms: bool,
    spawn_enemies: bool,
    time_since_rest: &dyn Fn(EntityId) -> i32,
) -> usize {
    if !spawn_enemies || !spawn_phantoms {
        return 0;
    }
    spawner.next_tick -= 1;
    if spawner.next_tick > 0 {
        return 0;
    }
    spawner.next_tick += (60 + level.random().next_int_bounded(60)) * 20;
    if env.sky_darken < 5 && env.has_sky_light {
        return 0;
    }
    let players: Vec<(EntityId, Vec3)> = level
        .entities
        .players
        .iter()
        .filter(|(_, p)| p.game_mode != vanillite_config::GameMode::Spectator)
        .filter_map(|(id, _)| level.entities.store.get(id).map(|e| (id, e.pos())))
        .collect();
    let mut spawned = 0;
    for (player, pos) in players {
        let pp = BlockPos::containing(pos.x, pos.y, pos.z);
        if env.has_sky_light && (pp.y < env.sea_level || !crate::mob::can_see_sky(level, pp)) {
            continue;
        }
        // `DifficultyInstance.isHarderThan(random * 3)`.
        let effective = crate::mob::fam::effective_difficulty(difficulty);
        if effective <= level.random().next_float() * 3.0 {
            continue;
        }
        let rest = time_since_rest(player).max(1);
        if level.random().next_int_bounded(rest) < 72000 {
            continue;
        }
        let up = 20 + level.random().next_int_bounded(15);
        let east = -10 + level.random().next_int_bounded(21);
        let south = -10 + level.random().next_int_bounded(21);
        let at = BlockPos::new(pp.x + east, pp.y + up, pp.z + south);
        if !crate::mob::spawn::valid_empty_spawn_block(level.block(at), Kind::Phantom.type_id()) {
            continue;
        }
        let group = 1 + level.random().next_int_bounded(difficulty as i32 + 1);
        for _ in 0..group {
            let seed = crate::mob::new_entity_seed();
            let mut mob = MobData::new(Kind::Phantom, seed);
            let extra = crate::mob::spawn::with_level_random(level, |level, random| {
                crate::mob::spawn::finalize(
                    level,
                    None,
                    env,
                    &mut mob,
                    at,
                    difficulty,
                    0,
                    SpawnReason::Natural,
                    random,
                )
            });
            let p = Vec3::new(f64::from(at.x), f64::from(at.y), f64::from(at.z));
            crate::mob::spawn::add_mob_with(level, mob, p, 0.0, extra);
            spawned += 1;
        }
    }
    spawned
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phantoms_fly_on_their_own() {
        let m = MobData::new(Kind::Phantom, 1);
        assert_eq!(m.nav.kind, NavKind::None);
        assert_eq!(m.move_ctl.kind, MoveKind::Custom);
        assert_eq!(m.attrs.attack_damage, 6.0);
        assert_eq!(m.goals.slots.len(), 3);
    }

    #[test]
    fn size_round_trip() {
        let mut m = MobData::new(Kind::Phantom, 1);
        set_size(&mut m, 5);
        ps_mut(&mut m).anchor = Some(BlockPos::new(1, 80, 2));
        let mut out = Compound::new();
        save(&m, &mut out);
        let mut n = MobData::new(Kind::Phantom, 2);
        load(&mut n, &out);
        assert_eq!(ps(&n).size, 5);
        assert_eq!(n.attrs.attack_damage, 11.0);
        assert_eq!(ps(&n).anchor, Some(BlockPos::new(1, 80, 2)));
        let d = dimensions(&n).expect("dimensions");
        assert!((d.width - 0.9 * 1.75).abs() < 1e-5);
    }

    #[test]
    fn approach_helpers() {
        assert_eq!(approach(0.1, 1.8, 0.5), 0.6);
        assert_eq!(approach(1.0, 0.2, 0.025), 0.975);
        assert!((approach_degrees(170.0, -170.0, 4.0) - 174.0).abs() < 1e-4);
    }
}

//! The ender dragon (`EnderDragon`) with its phases (`DragonPhaseInstance`s), its eight body
//! parts (`EnderDragonPart`), the end crystals, its fireballs and breath clouds, and the End
//! fight around it (`EnderDragonFight`).
//!
//! The dragon flies on its own (`noPhysics`): its `aiStep` replaces the living entity's; the
//! current phase picks a target, the dragon steers towards it and moves its parts after it.
//! The fight is owned by the End world ([`fight::DragonFight`]); the dragon reads what it needs
//! of it from [`MobContext::dragon_fight`] and reports back through
//! [`MobContext::dragon_events`], which the world hands to the fight right after the tick.

pub mod crystal;
pub mod fight;
pub mod live;
pub mod path;
pub mod phase;
#[cfg(test)]
mod sim_tests;

use vanillite_base::random::RandomSource;
use vanillite_base::{Aabb, BlockPos, Vec3};
use vanillite_chunk::HeightmapKind;
use vanillite_data::block::BlockState;
use vanillite_nbt::{Compound, Tag};
use vanillite_world::entity::{EntityFlags, EntityId};

use super::data;
use crate::entities::Entities;
use crate::level::Level;
use crate::mob::{MobContext, Tick, WorldAction, combat};
use crate::survival::damage::{DamageSource, tag};
use crate::survival::player::Env;
use crate::survival::rules::Rule;

pub use crystal::{Cloud, Crystal, Fireball, tick_cloud, tick_crystal, tick_fireball};
pub use fight::{DragonEvent, FightView};
pub use phase::Phase;

/// `DATA_PHASE` (after the mob's own data).
const DATA_PHASE: u8 = 16;

/// The parts in Vanilla's order (`subEntities`): name index, width, height.
pub const PARTS: [(f32, f32); 8] = [
    (1.0, 1.0), // head
    (3.0, 3.0), // neck
    (5.0, 3.0), // body
    (2.0, 2.0), // tail1
    (2.0, 2.0), // tail2
    (2.0, 2.0), // tail3
    (4.0, 2.0), // wing1
    (4.0, 2.0), // wing2
];
pub const HEAD: usize = 0;
pub const NECK: usize = 1;
pub const BODY: usize = 2;
pub const WING1: usize = 6;
pub const WING2: usize = 7;

/// `DragonFlightHistory`: the last 64 heights and yaws.
#[derive(Clone, Debug, PartialEq)]
pub struct FlightHistory {
    samples: [(f64, f32); 64],
    head: i32,
}

impl Default for FlightHistory {
    fn default() -> Self {
        FlightHistory {
            samples: [(0.0, 0.0); 64],
            head: -1,
        }
    }
}

impl FlightHistory {
    /// `record(y, yRot)`.
    pub fn record(&mut self, y: f64, y_rot: f32) {
        if self.head < 0 {
            self.samples = [(y, y_rot); 64];
        }
        self.head += 1;
        if self.head == 64 {
            self.head = 0;
        }
        self.samples[self.head as usize] = (y, y_rot);
    }

    /// `get(delay)`.
    pub fn get(&self, delay: i32) -> (f64, f32) {
        self.samples[((self.head - delay) & 63) as usize]
    }
}

/// The dragon's state.
#[derive(Clone, Debug)]
pub struct State {
    pub phase: Phase,
    pub phases: phase::Phases,
    pub history: FlightHistory,
    /// Part positions (feet).
    pub parts: [Vec3; 8],
    pub flap_time: f32,
    pub o_flap_time: f32,
    pub in_wall: bool,
    pub death_time: i32,
    pub y_rot_a: f32,
    pub nearest_crystal: Option<EntityId>,
    /// `fightOrigin`.
    pub fight_origin: BlockPos,
    /// `dragonFight != null`: this dragon belongs to the world's fight.
    pub in_fight: bool,
    pub sitting_damage_received: f32,
    pub graph: Option<path::Graph>,
    /// The part the next hit lands on (`hurt(part, ...)`); the body when unset.
    pub hit_part: Option<usize>,
    /// Health before the current hit, for the sitting damage.
    health_before: f32,
}

impl Default for State {
    fn default() -> Self {
        State {
            phase: Phase::Hover,
            phases: phase::Phases::default(),
            history: FlightHistory::default(),
            parts: [Vec3::ZERO; 8],
            flap_time: 0.0,
            o_flap_time: 0.0,
            in_wall: false,
            death_time: 0,
            y_rot_a: 0.0,
            nearest_crystal: None,
            fight_origin: BlockPos::new(0, 0, 0),
            in_fight: false,
            sitting_damage_received: 0.0,
            graph: None,
            hit_part: None,
            health_before: 0.0,
        }
    }
}

impl State {
    /// The box of part `i`.
    pub fn part_box(&self, i: usize) -> Aabb {
        let (w, h) = PARTS[i];
        let p = self.parts[i];
        let hw = f64::from(w) / 2.0;
        Aabb::new(
            p.x - hw,
            p.y,
            p.z - hw,
            p.x + hw,
            p.y + f64::from(h),
            p.z + hw,
        )
    }
}

/// The dragon state of the ticking mob.
pub(crate) fn st<'a>(t: &'a mut Tick<'_, '_>) -> &'a mut State {
    t.mob()
        .ext
        .dragon_mut()
        .expect("an ender dragon has dragon state")
}

pub(crate) fn st_ref<'a>(t: &'a Tick<'_, '_>) -> &'a State {
    t.mob_ref()
        .ext
        .dragon()
        .expect("an ender dragon has dragon state")
}

/// The fight this dragon belongs to (`getDragonFight()`).
pub(crate) fn fight(t: &Tick<'_, '_>) -> Option<FightView> {
    if st_ref(t).in_fight {
        t.cx.dragon_fight
    } else {
        None
    }
}

/// `aliveCrystals()` of the dragon's fight, 0 without one.
pub(crate) fn alive_crystals(t: &Tick<'_, '_>) -> i32 {
    fight(t).map_or(0, |f| f.alive_crystals)
}

/// `getHeightmapPos(kind, pos).getY()` (`getHeight`): the lowest y above the column's blocks;
/// the bottom of the world in chunks that are not loaded.
pub fn height(level: &Level, kind: HeightmapKind, x: i32, z: i32) -> i32 {
    let Some(c) = level
        .chunks
        .chunk(vanillite_base::ChunkPos::new(x >> 4, z >> 4))
    else {
        return level.min_y();
    };
    let Some(map) = c.chunk.heightmaps().get(kind) else {
        return level.min_y();
    };
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_wrap)]
    let h = map.get((x & 15) as u32, (z & 15) as u32) as i32 + c.chunk.min_y();
    h
}

/// `EnderDragonFight.getPodiumLocation(origin)` at the surface (`MOTION_BLOCKING_NO_LEAVES`).
pub(crate) fn podium(level: &Level, origin: BlockPos, kind: HeightmapKind) -> BlockPos {
    BlockPos::new(origin.x, height(level, kind, origin.x, origin.z), origin.z)
}

/// `Entity.calculateViewVector(xRot, yRot)`.
pub fn view_vector(x_rot: f32, y_rot: f32) -> Vec3 {
    let f = x_rot * (std::f32::consts::PI / 180.0);
    let g = -y_rot * (std::f32::consts::PI / 180.0);
    let h = vanillite_base::math::cos(f64::from(g));
    let i = vanillite_base::math::sin(f64::from(g));
    let j = vanillite_base::math::cos(f64::from(f));
    let k = vanillite_base::math::sin(f64::from(f));
    Vec3::new(f64::from(i * j), f64::from(-k), f64::from(h * j))
}

fn sin_deg(deg: f32) -> f32 {
    vanillite_base::math::sin(f64::from(deg * (std::f32::consts::PI / 180.0)))
}

fn cos_deg(deg: f32) -> f32 {
    vanillite_base::math::cos(f64::from(deg * (std::f32::consts::PI / 180.0)))
}

/// `Entity.getInputVector(input, speed, yRot)` for `moveRelative`.
fn move_relative(t: &mut Tick<'_, '_>, speed: f32, input: Vec3) {
    let length = input.length_sqr();
    if length < 1.0e-7 {
        return;
    }
    let movement = if length > 1.0 {
        input.normalize()
    } else {
        input
    } * f64::from(speed);
    let y_rot = t.core().rot().y_rot;
    let sin = f64::from(sin_deg(y_rot));
    let cos = f64::from(cos_deg(y_rot));
    let delta = Vec3::new(
        movement.x * cos - movement.z * sin,
        movement.y,
        movement.z * cos + movement.x * sin,
    );
    let v = t.velocity();
    t.set_velocity(v + delta);
}

/// `move(SELF, delta)` of a `noPhysics` entity: the position moves, nothing collides.
fn move_free(t: &mut Tick<'_, '_>, delta: Vec3) {
    let p = t.pos();
    let mut e = t.core_mut();
    e.set_pos(p + delta);
    e.set_flags(EntityFlags::ON_GROUND, false);
    let m = t.mob();
    m.horizontal_collision = false;
    m.vertical_collision = false;
}

fn set_y_rot(t: &mut Tick<'_, '_>, y_rot: f32) {
    let mut rot = t.core().rot();
    rot.y_rot = y_rot;
    rot.y_head_rot = y_rot;
    t.core_mut().set_rot(rot);
}

/// The mob's yaw.
pub(crate) fn y_rot(t: &Tick<'_, '_>) -> f32 {
    t.core().rot().y_rot
}

pub fn before_tick(t: &mut Tick<'_, '_>) {
    let _ = t;
}

pub fn after_tick(t: &mut Tick<'_, '_>) {
    let _ = t;
}

/// `EnderDragon.aiStep` (replaces `LivingEntity.aiStep`).
pub fn ai_step(t: &mut Tick<'_, '_>) {
    // `hasLineOfSight` of the dragon is not cached per tick (no `Sensing`).
    {
        let m = t.mob();
        m.seen.clear();
        m.unseen.clear();
    }
    // Attach to the world's fight when it names this dragon.
    if !st_ref(t).in_fight
        && let Some(view) = t.cx.dragon_fight
        && view.dragon_uuid == Some(t.core().uuid())
    {
        st(t).in_fight = true;
    }
    {
        let s = st(t);
        s.o_flap_time = s.flap_time;
    }
    if t.mob_ref().is_dead_or_dying() {
        // `EXPLOSION` particles at random offsets: the client draws them, the random moves on.
        let r = &mut t.mob().random;
        r.next_float();
        r.next_float();
        r.next_float();
        return;
    }
    check_crystals(t);
    let v = t.velocity();
    let horizontal = (v.x * v.x + v.z * v.z).sqrt();
    #[allow(clippy::cast_possible_truncation)]
    let mut flap_speed = 0.2 / (horizontal as f32 * 10.0 + 1.0);
    #[allow(clippy::cast_possible_truncation)]
    {
        flap_speed *= 2.0_f64.powf(v.y) as f32;
    }
    {
        let sitting = st_ref(t).phase.is_sitting();
        let s = st(t);
        if sitting {
            s.flap_time += 0.1;
        } else if s.in_wall {
            s.flap_time += flap_speed * 0.5;
        } else {
            s.flap_time += flap_speed;
        }
    }
    let yr = vanillite_base::math::wrap_degrees_f(y_rot(t));
    set_y_rot(t, yr);
    if t.mob_ref().no_ai {
        st(t).flap_time = 0.5;
        return;
    }
    {
        let (y, yr) = (t.pos().y, y_rot(t));
        st(t).history.record(y, yr);
    }
    let current = st_ref(t).phase;
    phase::server_tick(t);
    if !t.exists() {
        return;
    }
    if st_ref(t).phase != current {
        phase::server_tick(t);
        if !t.exists() {
            return;
        }
    }
    let phase = st_ref(t).phase;
    if let Some(target) = phase::fly_target(t) {
        let p = t.pos();
        let xdd = target.x - p.x;
        let mut ydd = target.y - p.y;
        let zdd = target.z - p.z;
        let dist_to_target = xdd * xdd + ydd * ydd + zdd * zdd;
        let max = phase.fly_speed();
        let horizontal_dist = (xdd * xdd + zdd * zdd).sqrt();
        if horizontal_dist > 0.0 {
            ydd = (ydd / horizontal_dist).clamp(f64::from(-max), f64::from(max));
        }
        let v = t.velocity();
        t.set_velocity(Vec3::new(v.x, v.y + ydd * 0.01, v.z));
        let yr = vanillite_base::math::wrap_degrees_f(y_rot(t));
        set_y_rot(t, yr);
        let aim = (target - t.pos()).normalize();
        let dir = Vec3::new(
            f64::from(sin_deg(y_rot(t))),
            t.velocity().y,
            f64::from(-cos_deg(y_rot(t))),
        )
        .normalize();
        #[allow(clippy::cast_possible_truncation)]
        let dot = ((dir.dot(aim) as f32 + 0.5) / 1.5).max(0.0);
        if xdd.abs() > 1.0e-5 || zdd.abs() > 1.0e-5 {
            #[allow(clippy::cast_possible_truncation)]
            let atan = xdd.atan2(zdd) as f32;
            let y_rot_d = vanillite_base::math::wrap_degrees_f(
                180.0 - atan * (180.0 / std::f32::consts::PI) - y_rot(t),
            )
            .clamp(-50.0, 50.0);
            let turn = phase::turn_speed(t);
            let s = st(t);
            s.y_rot_a *= 0.8;
            s.y_rot_a += y_rot_d * turn;
            let a = s.y_rot_a;
            let yr = y_rot(t) + a * 0.1;
            set_y_rot(t, yr);
        }
        #[allow(clippy::cast_possible_truncation)]
        let span = (2.0 / (dist_to_target + 1.0)) as f32;
        move_relative(
            t,
            0.06 * (dot * span + (1.0 - span)),
            Vec3::new(0.0, 0.0, -1.0),
        );
        let v = t.velocity();
        if st_ref(t).in_wall {
            move_free(t, v * f64::from(0.8_f32));
        } else {
            move_free(t, v);
        }
        let actual = t.velocity().normalize();
        let slide = 0.8 + 0.15 * (actual.dot(dir) + 1.0) / 2.0;
        let v = t.velocity();
        t.set_velocity(v.multiply(slide, f64::from(0.91_f32), slide));
    }
    let yr = y_rot(t);
    t.mob().y_body_rot = yr;
    move_parts(t);
}

/// Moves the parts after the body (`tickPart`), knocks back and hurts what the wings and the
/// head touch, then breaks the blocks in the head, neck and body (`checkWalls`).
fn move_parts(t: &mut Tick<'_, '_>) {
    let pos = t.pos();
    let (tilt, rot1) = {
        let s = st_ref(t);
        #[allow(clippy::cast_possible_truncation)]
        let tilt = (s.history.get(5).0 - s.history.get(10).0) as f32
            * 10.0
            * (std::f32::consts::PI / 180.0);
        (tilt, y_rot(t) * (std::f32::consts::PI / 180.0))
    };
    let cc_tilt = vanillite_base::math::cos(f64::from(tilt));
    let ss_tilt = vanillite_base::math::sin(f64::from(tilt));
    let ss1 = vanillite_base::math::sin(f64::from(rot1));
    let cc1 = vanillite_base::math::cos(f64::from(rot1));
    let set = |t: &mut Tick<'_, '_>, i: usize, x: f64, y: f64, z: f64| {
        st(t).parts[i] = Vec3::new(pos.x + x, pos.y + y, pos.z + z);
    };
    set(t, BODY, f64::from(ss1 * 0.5), 0.0, f64::from(-cc1 * 0.5));
    set(t, WING1, f64::from(cc1 * 4.5), 2.0, f64::from(ss1 * 4.5));
    set(t, WING2, f64::from(cc1 * -4.5), 2.0, f64::from(ss1 * -4.5));
    if t.mob_ref().hurt_time <= 0 {
        for wing in [WING1, WING2] {
            let area = st_ref(t)
                .part_box(wing)
                .inflate(4.0, 2.0, 4.0)
                .offset(0.0, -2.0, 0.0);
            knock_back(t, area);
            if !t.exists() {
                return;
            }
        }
        for part in [HEAD, NECK] {
            let area = st_ref(t).part_box(part).inflate(1.0, 1.0, 1.0);
            hurt_touching(t, area);
            if !t.exists() {
                return;
            }
        }
    }
    let y_rot_a = st_ref(t).y_rot_a;
    let ss2 = vanillite_base::math::sin(f64::from(rot1 - y_rot_a * 0.01));
    let cc2 = vanillite_base::math::cos(f64::from(rot1 - y_rot_a * 0.01));
    let y_offset = head_y_offset(t);
    set(
        t,
        HEAD,
        f64::from(ss2 * 6.5 * cc_tilt),
        f64::from(y_offset + ss_tilt * 6.5),
        f64::from(-cc2 * 6.5 * cc_tilt),
    );
    set(
        t,
        NECK,
        f64::from(ss2 * 5.5 * cc_tilt),
        f64::from(y_offset + ss_tilt * 5.5),
        f64::from(-cc2 * 5.5 * cc_tilt),
    );
    let p1 = st_ref(t).history.get(5);
    for i in 0..3 {
        let p0 = st_ref(t).history.get(12 + i * 2);
        #[allow(clippy::cast_possible_truncation)]
        let wrapped = vanillite_base::math::wrap_degrees(f64::from(p0.1 - p1.1)) as f32;
        let rot =
            y_rot(t) * (std::f32::consts::PI / 180.0) + wrapped * (std::f32::consts::PI / 180.0);
        let ss = vanillite_base::math::sin(f64::from(rot));
        let cc = vanillite_base::math::cos(f64::from(rot));
        #[allow(clippy::cast_precision_loss)]
        let dd = (i + 1) as f32 * 2.0;
        set(
            t,
            3 + i as usize,
            f64::from(-(ss1 * 1.5 + ss * dd) * cc_tilt),
            p0.0 - p1.0 - f64::from((dd + 1.5) * ss_tilt) + 1.5,
            f64::from((cc1 * 1.5 + cc * dd) * cc_tilt),
        );
    }
    let mut removed = Vec::new();
    let mut in_wall = false;
    for part in [HEAD, NECK, BODY] {
        let b = st_ref(t).part_box(part);
        in_wall |= check_walls(t, b, &mut removed);
    }
    st(t).in_wall = in_wall;
    if st_ref(t).in_fight {
        update_fight(t);
    }
}

/// `getHeadYOffset`.
fn head_y_offset(t: &Tick<'_, '_>) -> f32 {
    let s = st_ref(t);
    if s.phase.is_sitting() {
        -1.0
    } else {
        #[allow(clippy::cast_possible_truncation)]
        let d = (s.history.get(5).0 - s.history.get(0).0) as f32;
        d
    }
}

/// `dragonFight.updateDragon(this)`.
fn update_fight(t: &mut Tick<'_, '_>) {
    let m = t.mob_ref();
    let progress = m.health / m.max_health();
    let uuid = t.core().uuid();
    t.cx.dragon_events
        .push(DragonEvent::Update { uuid, progress });
}

/// `checkCrystals`: heals from the nearest crystal and looks for one every 10 ticks or so.
fn check_crystals(t: &mut Tick<'_, '_>) {
    if let Some(c) = st_ref(t).nearest_crystal {
        let gone = t
            .ents
            .special(c)
            .is_none_or(|d| matches!(d, super::special::SpecialData::EndCrystal(cr) if cr.removed));
        if gone {
            st(t).nearest_crystal = None;
        } else {
            let m = t.mob();
            if m.tick_count % 10 == 0 && m.health < m.max_health() {
                m.health += 1.0;
                t.core_mut().mark_data_dirty();
            }
        }
    }
    if t.mob().random.next_int_bounded(10) == 0 {
        let area = t.aabb().inflate(32.0, 32.0, 32.0);
        let pos = t.pos();
        let mut nearest = None;
        let mut best = f64::MAX;
        let ents = &*t.ents;
        ents.store.for_each_in(&area, false, |id| {
            let Some(e) = ents.store.get(id) else {
                return;
            };
            if e.kind() != vanillite_data::entity::ids::END_CRYSTAL {
                return;
            }
            let d = e.pos().distance_to_sqr(pos);
            if d < best {
                best = d;
                nearest = Some(id);
            }
        });
        st(t).nearest_crystal = nearest;
    }
}

/// Whether `id` is a living entity that is not a creative or spectator player
/// (`NO_CREATIVE_OR_SPECTATOR` on `LivingEntity`).
fn living_target(ents: &Entities, id: EntityId) -> bool {
    if let Some(m) = ents.mob(id) {
        return m.is_alive();
    }
    ents.player(id).is_some_and(|p| {
        !matches!(
            p.game_mode,
            vanillite_config::GameMode::Creative | vanillite_config::GameMode::Spectator
        )
    })
}

/// `knockBack(entities)` of a wing.
fn knock_back(t: &mut Tick<'_, '_>, area: Aabb) {
    let body = st_ref(t).part_box(BODY);
    let xm = (body.min_x + body.max_x) / 2.0;
    let zm = (body.min_z + body.max_z) / 2.0;
    let me = t.id;
    let mut hit = Vec::new();
    t.ents.store.for_each_in(&area, false, |id| {
        if id != me {
            hit.push(id);
        }
    });
    let sitting = st_ref(t).phase.is_sitting();
    for id in hit {
        if !living_target(t.ents, id) {
            continue;
        }
        let is_player = t.ents.player(id).is_some();
        let Some(mut e) = t.ents.store.get_mut(id) else {
            continue;
        };
        let p = e.as_ref().pos();
        let (xd, zd) = (p.x - xm, p.z - zm);
        let dd = (xd * xd + zd * zd).max(0.1);
        let v = e.as_ref().velocity() + Vec3::new(xd / dd * 4.0, f64::from(0.2_f32), zd / dd * 4.0);
        e.set_velocity(v);
        e.set_flags(
            if is_player {
                EntityFlags::HURT_MARKED
            } else {
                EntityFlags::HAS_IMPULSE
            },
            true,
        );
        // `getLastHurtByMobTimestamp() < entity.tickCount - 2`.
        let recently = t.ents.mob(id).is_some_and(|m| {
            m.last_hurt_by_mob.is_some() && m.last_hurt_by_mob_time >= m.tick_count - 2
        });
        if !sitting && !recently {
            let source = combat::entity_source(t.ents, t.env.types.mob_attack, me, me);
            combat::hurt(t.level, t.ents, t.env, t.cx, id, source, 5.0);
            if !t.exists() {
                return;
            }
        }
    }
}

/// `hurt(entities)`: what the head and the neck touch takes 10 damage.
fn hurt_touching(t: &mut Tick<'_, '_>, area: Aabb) {
    let me = t.id;
    let mut hit = Vec::new();
    t.ents.store.for_each_in(&area, false, |id| {
        if id != me {
            hit.push(id);
        }
    });
    for id in hit {
        if !living_target(t.ents, id) {
            continue;
        }
        let source = combat::entity_source(t.ents, t.env.types.mob_attack, me, me);
        combat::hurt(t.level, t.ents, t.env, t.cx, id, source, 10.0);
        if !t.exists() {
            return;
        }
    }
}

/// `checkWalls(box)`: breaks what is not dragon-immune (with `mobGriefing`), else reports the
/// wall. `removed` holds the blocks this tick already broke (they count as air).
fn check_walls(t: &mut Tick<'_, '_>, b: Aabb, removed: &mut Vec<BlockPos>) -> bool {
    let (x0, y0, z0) = (
        vanillite_base::math::floor(b.min_x),
        vanillite_base::math::floor(b.min_y),
        vanillite_base::math::floor(b.min_z),
    );
    let (x1, y1, z1) = (
        vanillite_base::math::floor(b.max_x),
        vanillite_base::math::floor(b.max_y),
        vanillite_base::math::floor(b.max_z),
    );
    let tags = crate::block::block_tags();
    let griefing = t.env.rules.bool(Rule::MOB_GRIEFING);
    let mut hit_wall = false;
    let mut destroyed = false;
    for x in x0..=x1 {
        for y in y0..=y1 {
            for z in z0..=z1 {
                let pos = BlockPos::new(x, y, z);
                if removed.contains(&pos) {
                    continue;
                }
                let state = t.level.block(pos);
                if state.is_air() || tags.dragon_transparent.contains(state) {
                    continue;
                }
                if griefing && !tags.dragon_immune.contains(state) {
                    // `removeBlock(pos, false)`: the block becomes the fluid it holds.
                    let fluid: BlockState = state.fluid_state().block_state();
                    t.cx.actions
                        .push(WorldAction::SetBlock(pos, fluid, crate::level::update::ALL));
                    removed.push(pos);
                    destroyed = true;
                } else {
                    hit_wall = true;
                }
            }
        }
    }
    if destroyed {
        let r = &mut t.mob().random;
        let random_pos = BlockPos::new(
            x0 + r.next_int_bounded(x1 - x0 + 1),
            y0 + r.next_int_bounded(y1 - y0 + 1),
            z0 + r.next_int_bounded(z1 - z0 + 1),
        );
        t.cx.actions
            .push(WorldAction::LevelEvent(2008, random_pos, 0));
    }
    hit_wall
}

/// `EnderDragon.tickDeath`: rises for 200 ticks, drops its experience, then the fight learns
/// of the kill and it is gone.
pub fn tick_death(t: &mut Tick<'_, '_>) -> bool {
    if st_ref(t).in_fight {
        update_fight(t);
    }
    let death_time = {
        let s = st(t);
        s.death_time += 1;
        s.death_time
    };
    if (180..=200).contains(&death_time) {
        // `EXPLOSION_EMITTER` particles at random offsets (client side, from the entity random).
        let r = &mut t.mob().random;
        r.next_float();
        r.next_float();
        r.next_float();
    }
    let xp = if fight(t).is_some_and(|f| !f.previously_killed) {
        12_000
    } else {
        500
    };
    let drops = t.env.rules.bool(Rule::MOB_DROPS);
    #[allow(clippy::cast_possible_truncation)]
    if death_time > 150 && death_time % 5 == 0 && drops {
        let pos = t.pos();
        let amount = (xp as f32 * 0.08).floor() as i32;
        t.env
            .out
            .push(crate::survival::Effect::Experience { pos, amount });
    }
    if death_time == 1 && !t.mob_ref().silent {
        // `globalLevelEvent(1028)`: the dragon's death sound for every player.
        let pos = t.block_pos();
        let packet = vanillite_protocol::SharedPacket::of(
            &vanillite_protocol::packets::play::clientbound::LevelEvent {
                event: 1028,
                pos: protocol_pos(pos),
                data: 0,
                global: true,
            },
        );
        t.env.out.push(crate::survival::Effect::Nearby {
            pos: t.pos(),
            // Every player of this world (Vanilla: of the server).
            range: 1.0e7,
            except: None,
            packet,
        });
    }
    let delta = Vec3::new(0.0, f64::from(0.1_f32), 0.0);
    move_free(t, delta);
    for p in &mut st(t).parts {
        *p = *p + delta;
    }
    if death_time >= 200 {
        if drops {
            let pos = t.pos();
            #[allow(clippy::cast_possible_truncation)]
            let amount = (xp as f32 * 0.2).floor() as i32;
            t.env
                .out
                .push(crate::survival::Effect::Experience { pos, amount });
        }
        if st_ref(t).in_fight {
            let uuid = t.core().uuid();
            t.cx.dragon_events.push(DragonEvent::Killed { uuid });
        }
        t.mob().removed = true;
    }
    true
}

/// A block position on the wire.
pub(crate) fn protocol_pos(pos: BlockPos) -> vanillite_protocol::types::BlockPos {
    vanillite_protocol::types::BlockPos {
        x: pos.x,
        y: pos.y,
        z: pos.z,
    }
}

/// `EnderDragon.hurt(part, source, damage)` up to `reallyHurt`: the damage the living entity
/// takes, `None` when the hit does nothing.
pub fn filter_hurt(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    cx: &mut MobContext,
    target: EntityId,
    source: &DamageSource,
    amount: f32,
) -> Option<f32> {
    let _ = (level, cx);
    let is_arrow_or_charge = source.direct.is_some_and(|d| ents.arrow(d).is_some());
    let cause_is_player = source.cause.is_some_and(|c| ents.player(c).is_some());
    let m = ents.mob_mut(target)?;
    let health = m.health;
    let s = m.ext.dragon_mut()?;
    let part = s.hit_part.take().unwrap_or(BODY);
    if s.phase == Phase::Dying {
        return None;
    }
    let mut damage = phase::on_hurt(s.phase, is_arrow_or_charge, amount);
    damage = part_damage(part, damage);
    if damage < 0.01 {
        return None;
    }
    if !(cause_is_player || env.types.is(source.kind, tag::IS_EXPLOSION)) {
        return None;
    }
    s.health_before = health;
    Some(damage)
}

/// The damage a part passes on: the head and the neck all of it, the others a quarter plus
/// up to one point.
pub fn part_damage(part: usize, damage: f32) -> f32 {
    if part == HEAD || part == NECK {
        damage
    } else {
        damage / 4.0 + damage.min(1.0)
    }
}

/// The experience the dying dragon drops at `death_time` (`first_kill`: the fight's first).
pub fn death_xp(first_kill: bool, death_time: i32) -> i32 {
    let xp: f32 = if first_kill { 12_000.0 } else { 500.0 };
    let mut out = 0;
    #[allow(clippy::cast_possible_truncation)]
    if death_time > 150 && death_time % 5 == 0 {
        out += (xp * 0.08).floor() as i32;
    }
    #[allow(clippy::cast_possible_truncation)]
    if death_time >= 200 {
        out += (xp * 0.2).floor() as i32;
    }
    out
}

/// The sitting damage after a hit: a quarter of the health taken while sitting makes the
/// dragon take off.
pub fn after_hurt(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    cx: &mut MobContext,
    target: EntityId,
) {
    let Some(m) = ents.mob_mut(target) else {
        return;
    };
    let (health, max) = (m.health, m.max_health());
    let Some(s) = m.ext.dragon_mut() else {
        return;
    };
    if s.phase.is_sitting() {
        s.sitting_damage_received += s.health_before - health;
        if s.sitting_damage_received > 0.25 * max {
            s.sitting_damage_received = 0.0;
            let mut t = Tick {
                level,
                ents,
                env,
                cx,
                id: target,
            };
            phase::set_phase(&mut t, Phase::Takeoff);
        }
    }
}

/// `handleKillingBlow`: outside the sitting phases the dragon does not die at once; it keeps
/// one health point and flies to the portal (`DYING`).
pub fn on_death(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    cx: &mut MobContext,
    target: EntityId,
) {
    let Some(m) = ents.mob_mut(target) else {
        return;
    };
    // `die` only marks the dragon dead through `handleKillingBlow`, which it overrides.
    m.dead = false;
    let sitting = m.ext.dragon().is_some_and(|s| s.phase.is_sitting());
    if !sitting {
        m.health = 1.0;
        let mut t = Tick {
            level,
            ents,
            env,
            cx,
            id: target,
        };
        phase::set_phase(&mut t, Phase::Dying);
    }
}

/// Whether knockback leaves the dragon alone (`knockback` while sitting).
pub fn ignores_knockback(ents: &Entities, id: EntityId) -> bool {
    ents.mob(id)
        .and_then(|m| m.ext.dragon())
        .is_some_and(|s| s.phase.is_sitting())
}

/// The part a network id names (`dragon id + 1 ..= + 8`), if `target` is a dragon.
pub fn part_of(ents: &Entities, target: EntityId, network_id: i32) -> Option<usize> {
    let e = ents.store.get(target)?;
    ents.mob(target)?.ext.dragon()?;
    let i = network_id.checked_sub(e.network_id().0)?.checked_sub(1)?;
    usize::try_from(i).ok().filter(|&i| i < 8)
}

/// `onCrystalDestroyed`: the healing crystal's explosion hurts the head; the phase reacts.
pub fn on_crystal_destroyed(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    cx: &mut MobContext,
    dragon: EntityId,
    crystal: EntityId,
    source: &DamageSource,
) {
    let player = match source.cause.filter(|c| ents.player(*c).is_some()) {
        Some(p) => Some(p),
        None => {
            // `getNearestPlayer(CRYSTAL_DESTROY_TARGETING, pos)`: without a targeter the range
            // does not apply; nobody in peaceful.
            let pos = ents.store.get(crystal).map(|e| {
                let p = e.pos();
                Vec3::new(p.x.floor(), p.y.floor(), p.z.floor())
            });
            pos.and_then(|pos| {
                phase::nearest_player(ents, env, pos, None, phase::Targeting::default())
            })
        }
    };
    let nearest = ents
        .mob(dragon)
        .and_then(|m| m.ext.dragon())
        .and_then(|s| s.nearest_crystal);
    if nearest == Some(crystal) {
        if let Some(m) = ents.mob_mut(dragon)
            && let Some(s) = m.ext.dragon_mut()
        {
            s.hit_part = Some(HEAD);
        }
        // `damageSources().explosion(crystal, player)`.
        let mut src = DamageSource::of(if player.is_some() {
            env.types.player_explosion
        } else {
            env.types.explosion
        });
        src.direct = Some(crystal);
        src.cause = player;
        src.position = ents.store.get(crystal).map(|e| e.pos());
        combat::hurt(level, ents, env, cx, dragon, src, 10.0);
    }
    if ents.mob(dragon).is_some() {
        let mut t = Tick {
            level,
            ents,
            env,
            cx,
            id: dragon,
        };
        phase::on_crystal_destroyed_tick(&mut t, player);
    }
}

/// A fresh dragon of the fight at `origin` (`EnderDragonFight.createNewDragon`).
pub fn new_dragon(seed: i64, origin: BlockPos) -> crate::mob::MobData {
    let mut m = crate::mob::MobData::new(crate::mob::Kind::EnderDragon, seed);
    if let Some(s) = m.ext.dragon_mut() {
        s.fight_origin = origin;
        s.in_fight = true;
        phase::set_phase_in(s, Phase::HoldingPattern, &mut Vec::new());
    }
    m
}

pub fn entity_data(out: &mut Vec<u8>, s: &State, all: bool) {
    data::int(out, DATA_PHASE, i32::from(s.phase.id()), 10, all);
}

pub fn save(s: &State, out: &mut Compound) {
    out.insert("DragonPhase", Tag::Int(i32::from(s.phase.id())));
    out.insert("DragonDeathTime", Tag::Int(s.death_time));
    out.insert(
        "sitting_damage_received",
        Tag::Float(s.sitting_damage_received),
    );
}

pub fn load(s: &mut State, tag: &Compound) {
    if let Some(id) = tag.get("DragonPhase").and_then(Tag::as_i64) {
        let phase = Phase::by_id(id);
        phase::set_phase_in(s, phase, &mut Vec::new());
    }
    s.death_time = tag
        .get("DragonDeathTime")
        .and_then(Tag::as_i64)
        .and_then(|v| i32::try_from(v).ok())
        .unwrap_or(0);
    s.sitting_damage_received = match tag.get("sitting_damage_received") {
        Some(Tag::Float(f)) if f.is_finite() => *f,
        _ => 0.0,
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flight_history_wraps() {
        let mut h = FlightHistory::default();
        h.record(10.0, 1.0);
        assert_eq!(h.get(5), (10.0, 1.0));
        for i in 0..70 {
            h.record(f64::from(i), 0.0);
        }
        assert_eq!(h.get(0).0, 69.0);
        assert_eq!(h.get(5).0, 64.0);
    }

    #[test]
    fn view_vector_faces_south_at_zero_yaw() {
        let v = view_vector(0.0, 0.0);
        assert!((v.z - 1.0).abs() < 1e-6 && v.x.abs() < 1e-6);
        let down = view_vector(90.0, 0.0);
        assert!((down.y + 1.0).abs() < 1e-6);
    }
}

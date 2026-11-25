//! The dragon's phases (`EnderDragonPhase`, `DragonPhaseInstance` and its eleven kinds). Each
//! phase keeps its own state between visits, like Vanilla's phase instances.

use vanillite_base::random::{LegacyRandom, RandomSource};
use vanillite_base::{BlockPos, Vec3};
use vanillite_chunk::HeightmapKind;
use vanillite_config::GameMode;
use vanillite_world::entity::{EntityFlags, EntityId};

use super::path::{Node, Path};
use super::{BODY, HEAD, PARTS, State, alive_crystals, fight, podium, st, st_ref, y_rot};
use crate::entities::Entities;
use crate::mob::Tick;
use crate::survival::player::Env;
use crate::survival::rules::Difficulty;

/// `EnderDragonPhase` by id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    HoldingPattern,
    StrafePlayer,
    LandingApproach,
    Landing,
    Takeoff,
    SittingFlaming,
    SittingScanning,
    SittingAttacking,
    ChargingPlayer,
    Dying,
    Hover,
}

impl Phase {
    const ALL: [Phase; 11] = [
        Phase::HoldingPattern,
        Phase::StrafePlayer,
        Phase::LandingApproach,
        Phase::Landing,
        Phase::Takeoff,
        Phase::SittingFlaming,
        Phase::SittingScanning,
        Phase::SittingAttacking,
        Phase::ChargingPlayer,
        Phase::Dying,
        Phase::Hover,
    ];

    /// `getId`.
    pub fn id(self) -> u8 {
        Phase::ALL.iter().position(|&p| p == self).unwrap_or(0) as u8
    }

    /// `getById`: unknown ids are the holding pattern.
    pub fn by_id(id: i64) -> Phase {
        usize::try_from(id)
            .ok()
            .and_then(|i| Phase::ALL.get(i).copied())
            .unwrap_or(Phase::HoldingPattern)
    }

    /// `isSitting`.
    pub fn is_sitting(self) -> bool {
        matches!(
            self,
            Phase::SittingFlaming | Phase::SittingScanning | Phase::SittingAttacking | Phase::Hover
        )
    }

    /// `getFlySpeed`.
    pub fn fly_speed(self) -> f32 {
        match self {
            Phase::Landing => 1.5,
            Phase::ChargingPlayer | Phase::Dying => 3.0,
            Phase::Hover => 1.0,
            _ => 0.6,
        }
    }
}

/// A phase that flies along a path to a target.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Wander {
    pub path: Option<Path>,
    pub target: Option<Vec3>,
    pub clockwise: bool,
}

/// `DragonStrafePlayerPhase`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Strafe {
    pub fireball_charge: i32,
    pub path: Option<Path>,
    pub target: Option<Vec3>,
    pub attack_target: Option<EntityId>,
    pub clockwise: bool,
}

/// `DragonTakeoffPhase`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Takeoff {
    pub first_tick: bool,
    pub path: Option<Path>,
    pub target: Option<Vec3>,
}

/// `DragonSittingFlamingPhase`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Flaming {
    pub ticks: i32,
    pub count: i32,
    pub flame: Option<EntityId>,
}

/// `DragonChargePlayerPhase`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Charge {
    pub target: Option<Vec3>,
    pub time_since: i32,
}

/// `DragonDeathPhase`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Dying {
    pub target: Option<Vec3>,
    pub time: i32,
}

/// The state of every phase.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Phases {
    pub holding: Wander,
    pub strafe: Strafe,
    pub approach: Wander,
    pub landing: Option<Vec3>,
    pub takeoff: Takeoff,
    pub flaming: Flaming,
    pub scanning_time: i32,
    pub attacking_ticks: i32,
    pub charge: Charge,
    pub dying: Dying,
    pub hover: Option<Vec3>,
}

/// `setPhase` on the state alone: ends the old phase, begins the new one. Returns the flame
/// cloud the sitting flame phase leaves behind (to discard).
pub fn set_phase_in(s: &mut State, phase: Phase, discard: &mut Vec<EntityId>) {
    if s.phase == phase {
        return;
    }
    // `end()`.
    if s.phase == Phase::SittingFlaming
        && let Some(flame) = s.phases.flaming.flame.take()
    {
        discard.push(flame);
    }
    s.phase = phase;
    // `begin()`.
    let p = &mut s.phases;
    match phase {
        Phase::HoldingPattern => {
            p.holding.path = None;
            p.holding.target = None;
        }
        Phase::StrafePlayer => {
            p.strafe.fireball_charge = 0;
            p.strafe.target = None;
            p.strafe.path = None;
            p.strafe.attack_target = None;
        }
        Phase::LandingApproach => {
            p.approach.path = None;
            p.approach.target = None;
        }
        Phase::Landing => p.landing = None,
        Phase::Takeoff => {
            p.takeoff.first_tick = true;
            p.takeoff.path = None;
            p.takeoff.target = None;
        }
        Phase::SittingFlaming => {
            p.flaming.ticks = 0;
            p.flaming.count += 1;
        }
        Phase::SittingScanning => p.scanning_time = 0,
        Phase::SittingAttacking => p.attacking_ticks = 0,
        Phase::ChargingPlayer => {
            p.charge.target = None;
            p.charge.time_since = 0;
        }
        Phase::Dying => {
            p.dying.target = None;
            p.dying.time = 0;
        }
        Phase::Hover => p.hover = None,
    }
}

/// `getPhaseManager().setPhase(phase)` of the ticking dragon (`DATA_PHASE` is synched).
pub fn set_phase(t: &mut Tick<'_, '_>, phase: Phase) {
    let mut discard = Vec::new();
    set_phase_in(st(t), phase, &mut discard);
    t.cx.discarded.extend(discard);
    t.core_mut().mark_data_dirty();
}

/// The dragon's random and state together.
fn parts<'a>(t: &'a mut Tick<'_, '_>) -> (&'a mut LegacyRandom, &'a mut State) {
    let m = t.mob();
    let s = m.boss.dragon_mut().expect("dragon state");
    (&mut m.random, s)
}

/// The surface position of the podium (`MOTION_BLOCKING_NO_LEAVES`).
fn egg(t: &Tick<'_, '_>) -> BlockPos {
    podium(
        t.level,
        st_ref(t).fight_origin,
        HeightmapKind::MotionBlockingNoLeaves,
    )
}

/// `Vec3.atBottomCenterOf`.
fn bottom_center(p: BlockPos) -> Vec3 {
    Vec3::new(f64::from(p.x) + 0.5, f64::from(p.y), f64::from(p.z) + 0.5)
}

/// `BlockPos.distToCenterSqr(position)`.
fn dist_to_center_sqr(p: BlockPos, v: Vec3) -> f64 {
    let (dx, dy, dz) = (
        f64::from(p.x) + 0.5 - v.x,
        f64::from(p.y) + 0.5 - v.y,
        f64::from(p.z) + 0.5 - v.z,
    );
    dx * dx + dy * dy + dz * dz
}

/// The graph, built from the terrain on first use.
fn ensure_graph(t: &mut Tick<'_, '_>) {
    if st_ref(t).graph.is_none() {
        let level = t.level;
        let g = super::path::Graph::new(|x, z| {
            super::height(level, HeightmapKind::MotionBlockingNoLeaves, x, z)
        });
        st(t).graph = Some(g);
    }
}

/// Whether the path search may use the outer ring (`dragonFight != null && aliveCrystals > 0`).
fn crystals_alive(t: &Tick<'_, '_>) -> bool {
    fight(t).is_some_and(|f| f.alive_crystals > 0)
}

/// `findClosestNode()` at the dragon.
fn closest_node(t: &mut Tick<'_, '_>) -> usize {
    ensure_graph(t);
    let p = t.pos();
    closest_node_to(t, p.x, p.y, p.z)
}

/// `findClosestNode(x, y, z)`.
fn closest_node_to(t: &mut Tick<'_, '_>, x: f64, y: f64, z: f64) -> usize {
    ensure_graph(t);
    let alive = crystals_alive(t);
    st_ref(t)
        .graph
        .as_ref()
        .map_or(0, |g| g.closest(x, y, z, alive))
}

/// `findPath(start, end, finalNode)`.
fn find_path(t: &mut Tick<'_, '_>, start: usize, end: usize, last: Option<Node>) -> Option<Path> {
    ensure_graph(t);
    let alive = crystals_alive(t);
    st_ref(t)
        .graph
        .as_ref()
        .and_then(|g| g.find_path(start, end, last, alive))
}

/// `navigateToNextPathNode`: the next node with a random height up to 20 above it.
fn navigate(r: &mut LegacyRandom, path: &mut Option<Path>, target: &mut Option<Vec3>) {
    if let Some(p) = path
        && !p.is_done()
    {
        let current = p.next_node();
        p.advance();
        let mut y;
        loop {
            y = f64::from(current.y) + f64::from(r.next_float() * 20.0);
            if y >= f64::from(current.y) {
                break;
            }
        }
        *target = Some(Vec3::new(f64::from(current.x), y, f64::from(current.z)));
    }
}

/// The node index after the holding pattern's step, wrapped into its ring.
fn wrap_ring(index: i32, in_outer: bool) -> usize {
    let i = if in_outer {
        index.rem_euclid(12)
    } else {
        ((index - 12) & 7) + 12
    };
    usize::try_from(i).unwrap_or(0)
}

/// Targeting conditions of the dragon's player searches (`TargetingConditions.forCombat()`).
#[derive(Clone, Copy, Debug, Default)]
pub struct Targeting {
    /// `range`, 0 for none.
    pub range: f64,
    /// The scanning selector: at most this far above or below the dragon.
    pub max_dy: Option<f64>,
}

/// `getNearestPlayer(conditions, targeter, x, y, z)`: the closest player the conditions allow
/// (`targeter` is the searching dragon's id and position, `None` for none).
pub fn nearest_player(
    ents: &Entities,
    env: &Env<'_>,
    from: Vec3,
    targeter: Option<(EntityId, Vec3)>,
    cond: Targeting,
) -> Option<EntityId> {
    let mut best = -1.0;
    let mut result = None;
    for (id, p) in ents.players.iter() {
        let Some(e) = ents.store.get(id) else {
            continue;
        };
        // `canBeSeenByAnyone` and the combat check (`canBeSeenAsEnemy`).
        if p.survival.is_dead_or_dying()
            || matches!(p.game_mode, GameMode::Spectator | GameMode::Creative)
        {
            continue;
        }
        let pos = e.pos();
        match targeter {
            None => {
                if env.difficulty == Difficulty::Peaceful {
                    continue;
                }
            }
            Some((_, at)) => {
                if let Some(dy) = cond.max_dy
                    && (pos.y - at.y).abs() > dy
                {
                    continue;
                }
                if cond.range > 0.0 {
                    let flags = e.flags();
                    let mut visibility = 1.0;
                    if flags.contains(EntityFlags::CROUCHING) {
                        visibility *= 0.8;
                    }
                    if flags.contains(EntityFlags::INVISIBLE)
                        || p.survival
                            .effects
                            .has(crate::effect::MobEffect::INVISIBILITY)
                    {
                        visibility *= 0.7 * 0.1;
                    }
                    let d = (cond.range * visibility).max(2.0);
                    if at.distance_to_sqr(pos) > d * d {
                        continue;
                    }
                }
            }
        }
        let dist = pos.distance_to_sqr(from);
        if best == -1.0 || dist < best {
            best = dist;
            result = Some(id);
        }
    }
    result
}

/// The fly target of the current phase (`getFlyTargetLocation`).
pub fn fly_target(t: &Tick<'_, '_>) -> Option<Vec3> {
    let s = st_ref(t);
    let p = &s.phases;
    match s.phase {
        Phase::HoldingPattern => p.holding.target,
        Phase::StrafePlayer => p.strafe.target,
        Phase::LandingApproach => p.approach.target,
        Phase::Landing => p.landing,
        Phase::Takeoff => p.takeoff.target,
        Phase::ChargingPlayer => p.charge.target,
        Phase::Dying => p.dying.target,
        Phase::Hover => p.hover,
        Phase::SittingFlaming | Phase::SittingScanning | Phase::SittingAttacking => None,
    }
}

/// `getTurnSpeed`.
pub fn turn_speed(t: &Tick<'_, '_>) -> f32 {
    let v = t.velocity();
    #[allow(clippy::cast_possible_truncation)]
    let rot_speed = (v.x * v.x + v.z * v.z).sqrt() as f32 + 1.0;
    let dist = rot_speed.min(40.0);
    if st_ref(t).phase == Phase::Landing {
        dist / rot_speed
    } else {
        0.7 / dist / rot_speed
    }
}

/// `onHurt(source, damage)`: arrows and wind charges bounce off the sitting dragon.
pub fn on_hurt(phase: Phase, arrow: bool, damage: f32) -> f32 {
    let sitting_phase = matches!(
        phase,
        Phase::SittingFlaming | Phase::SittingScanning | Phase::SittingAttacking
    );
    if sitting_phase && arrow { 0.0 } else { damage }
}

/// `onCrystalDestroyed` with the dragon's tick context.
pub fn on_crystal_destroyed_tick(t: &mut Tick<'_, '_>, player: Option<EntityId>) {
    if st_ref(t).phase != Phase::HoldingPattern {
        return;
    }
    if let Some(p) = player
        && can_attack(t.ents, p)
    {
        strafe_player(t, p);
    }
}

/// `dragon.canAttack(target)`: `canBeSeenAsEnemy`.
fn can_attack(ents: &Entities, id: EntityId) -> bool {
    ents.player(id).is_some_and(|p| {
        !p.survival.is_dead_or_dying()
            && !matches!(p.game_mode, GameMode::Spectator | GameMode::Creative)
    }) || ents.mob(id).is_some_and(crate::mob::MobData::is_alive)
}

/// `strafePlayer(player)`.
fn strafe_player(t: &mut Tick<'_, '_>, player: EntityId) {
    set_phase(t, Phase::StrafePlayer);
    set_strafe_target(t, player);
}

/// `DragonStrafePlayerPhase.setTarget`.
fn set_strafe_target(t: &mut Tick<'_, '_>, target: EntityId) {
    let Some(tp) = t.ents.store.get(target).map(|e| e.pos()) else {
        return;
    };
    st(t).phases.strafe.attack_target = Some(target);
    let current = closest_node(t);
    let target_index = closest_node_to(t, tp.x, tp.y, tp.z);
    let fx = vanillite_base::math::floor(tp.x);
    let fz = vanillite_base::math::floor(tp.z);
    let p = t.pos();
    let (xd, zd) = (f64::from(fx) - p.x, f64::from(fz) - p.z);
    let sd = (xd * xd + zd * zd).sqrt();
    let ho = (f64::from(0.4_f32) + sd / 80.0 - 1.0).min(10.0);
    let fy = vanillite_base::math::floor(tp.y + ho);
    let path = find_path(t, current, target_index, Some(Node::new(fx, fy, fz)));
    let (r, s) = parts(t);
    s.phases.strafe.path = path;
    if let Some(p) = &mut s.phases.strafe.path {
        p.advance();
        navigate(r, &mut s.phases.strafe.path, &mut s.phases.strafe.target);
    }
}

/// `doServerTick` of the current phase.
pub fn server_tick(t: &mut Tick<'_, '_>) {
    match st_ref(t).phase {
        Phase::HoldingPattern => holding_tick(t),
        Phase::StrafePlayer => strafe_tick(t),
        Phase::LandingApproach => approach_tick(t),
        Phase::Landing => landing_tick(t),
        Phase::Takeoff => takeoff_tick(t),
        Phase::SittingFlaming => flaming_tick(t),
        Phase::SittingScanning => scanning_tick(t),
        Phase::SittingAttacking => {
            let s = st(t);
            let ticks = s.phases.attacking_ticks;
            s.phases.attacking_ticks += 1;
            if ticks >= 40 {
                set_phase(t, Phase::SittingFlaming);
            }
        }
        Phase::ChargingPlayer => charge_tick(t),
        Phase::Dying => dying_tick(t),
        Phase::Hover => {
            if st_ref(t).phases.hover.is_none() {
                let p = t.pos();
                st(t).phases.hover = Some(p);
            }
        }
    }
}

/// Whether a wandering phase needs its next target (`< 10` or `> 150` blocks away; the
/// dragon never collides).
fn needs_target(t: &Tick<'_, '_>, target: Option<Vec3>) -> bool {
    let d = target.map_or(0.0, |v| v.distance_to_sqr(t.pos()));
    d < 100.0 || d > 22_500.0
}

fn holding_tick(t: &mut Tick<'_, '_>) {
    if !needs_target(t, st_ref(t).phases.holding.target) {
        return;
    }
    // `findNewTarget`.
    if st_ref(t)
        .phases
        .holding
        .path
        .as_ref()
        .is_some_and(Path::is_done)
    {
        let egg = egg(t);
        let crystals = if fight(t).is_some() {
            alive_crystals(t)
        } else {
            0
        };
        if t.mob().random.next_int_bounded(crystals + 3) == 0 {
            set_phase(t, Phase::LandingApproach);
            return;
        }
        let from = Vec3::new(f64::from(egg.x), f64::from(egg.y), f64::from(egg.z));
        let me = (t.id, t.pos());
        let player = nearest_player(t.ents, t.env, from, Some(me), Targeting::default());
        let dist = player
            .and_then(|p| t.ents.store.get(p))
            .map_or(64.0, |e| dist_to_center_sqr(egg, e.pos()) / 512.0);
        if let Some(player) = player {
            #[allow(clippy::cast_possible_truncation)]
            let bound = (dist + 2.0) as i32;
            let r = &mut t.mob().random;
            if r.next_int_bounded(bound) == 0 || r.next_int_bounded(crystals + 2) == 0 {
                strafe_player(t, player);
                return;
            }
        }
    }
    if st_ref(t)
        .phases
        .holding
        .path
        .as_ref()
        .is_none_or(Path::is_done)
    {
        let current = closest_node(t);
        let mut target = i32::try_from(current).unwrap_or(0);
        {
            let (r, s) = parts(t);
            if r.next_int_bounded(8) == 0 {
                s.phases.holding.clockwise = !s.phases.holding.clockwise;
                target += 6;
            }
            if s.phases.holding.clockwise {
                target += 1;
            } else {
                target -= 1;
            }
        }
        // The holding pattern stays on the outer ring while the dragon has its fight
        // (`aliveCrystals() >= 0`).
        let outer = fight(t).is_some();
        let end = wrap_ring(target, outer);
        let path = find_path(t, current, end, None);
        let s = st(t);
        s.phases.holding.path = path;
        if let Some(p) = &mut s.phases.holding.path {
            p.advance();
        }
    }
    let (r, s) = parts(t);
    navigate(r, &mut s.phases.holding.path, &mut s.phases.holding.target);
}

fn strafe_tick(t: &mut Tick<'_, '_>) {
    let Some(target) = st_ref(t).phases.strafe.attack_target else {
        set_phase(t, Phase::HoldingPattern);
        return;
    };
    let Some((tp, th)) = t.ents.store.get(target).map(|e| (e.pos(), e.dims().height)) else {
        set_phase(t, Phase::HoldingPattern);
        return;
    };
    if st_ref(t)
        .phases
        .strafe
        .path
        .as_ref()
        .is_some_and(Path::is_done)
    {
        let p = t.pos();
        let (xd, zd) = (tp.x - p.x, tp.z - p.z);
        let dist = (xd * xd + zd * zd).sqrt();
        let ho = (f64::from(0.4_f32) + dist / 80.0 - 1.0).min(10.0);
        st(t).phases.strafe.target = Some(Vec3::new(tp.x, tp.y + ho, tp.z));
    }
    if needs_target(t, st_ref(t).phases.strafe.target) {
        strafe_find_new_target(t);
    }
    if tp.distance_to_sqr(t.pos()) < 4096.0 {
        if crate::mob::living::has_line_of_sight(t, target) {
            st(t).phases.strafe.fireball_charge += 1;
            let p = t.pos();
            let aim = Vec3::new(tp.x - p.x, 0.0, tp.z - p.z).normalize();
            let yr = y_rot(t);
            let dir = Vec3::new(
                f64::from(super::sin_deg(yr)),
                0.0,
                f64::from(-super::cos_deg(yr)),
            )
            .normalize();
            #[allow(clippy::cast_possible_truncation)]
            let dot = dir.dot(aim) as f32;
            #[allow(clippy::cast_possible_truncation)]
            let angle = (f64::from(dot).acos() * f64::from(57.295_776_f32)) as f32 + 0.5;
            if st_ref(t).phases.strafe.fireball_charge >= 5 && (0.0..10.0).contains(&angle) {
                let view = super::view_vector(0.0, yr);
                let head = st_ref(t).parts[HEAD];
                let start = Vec3::new(
                    head.x - view.x,
                    head.y + f64::from(PARTS[HEAD].1) * 0.5 + 0.5,
                    head.z - view.z,
                );
                let to = Vec3::new(tp.x, tp.y + f64::from(th) * 0.5, tp.z);
                if !t.mob_ref().silent {
                    let pos = t.block_pos();
                    t.cx.actions
                        .push(crate::mob::WorldAction::LevelEvent(1017, pos, 0));
                }
                let owner = t.id;
                super::crystal::spawn_fireball(t.level, t.ents, t.env, owner, start, to - start);
                let s = st(t);
                s.phases.strafe.fireball_charge = 0;
                if let Some(p) = &mut s.phases.strafe.path {
                    p.next = p.nodes.len().max(p.next);
                }
                set_phase(t, Phase::HoldingPattern);
            }
        } else {
            let s = st(t);
            if s.phases.strafe.fireball_charge > 0 {
                s.phases.strafe.fireball_charge -= 1;
            }
        }
    } else {
        let s = st(t);
        if s.phases.strafe.fireball_charge > 0 {
            s.phases.strafe.fireball_charge -= 1;
        }
    }
}

/// `DragonStrafePlayerPhase.findNewTarget`.
fn strafe_find_new_target(t: &mut Tick<'_, '_>) {
    if st_ref(t)
        .phases
        .strafe
        .path
        .as_ref()
        .is_none_or(Path::is_done)
    {
        let current = closest_node(t);
        let mut target = i32::try_from(current).unwrap_or(0);
        {
            let (r, s) = parts(t);
            if r.next_int_bounded(8) == 0 {
                s.phases.strafe.clockwise = !s.phases.strafe.clockwise;
                target += 6;
            }
            if s.phases.strafe.clockwise {
                target += 1;
            } else {
                target -= 1;
            }
        }
        let outer = crystals_alive(t);
        let end = wrap_ring(target, outer);
        let path = find_path(t, current, end, None);
        let s = st(t);
        s.phases.strafe.path = path;
        if let Some(p) = &mut s.phases.strafe.path {
            p.advance();
        }
    }
    let (r, s) = parts(t);
    navigate(r, &mut s.phases.strafe.path, &mut s.phases.strafe.target);
}

fn approach_tick(t: &mut Tick<'_, '_>) {
    if !needs_target(t, st_ref(t).phases.approach.target) {
        return;
    }
    if st_ref(t)
        .phases
        .approach
        .path
        .as_ref()
        .is_none_or(Path::is_done)
    {
        let current = closest_node(t);
        let egg = egg(t);
        let from = Vec3::new(f64::from(egg.x), f64::from(egg.y), f64::from(egg.z));
        let me = (t.id, t.pos());
        let player = nearest_player(t.ents, t.env, from, Some(me), Targeting::default());
        let target = match player.and_then(|p| t.ents.store.get(p)).map(|e| e.pos()) {
            Some(pp) => {
                let aim = Vec3::new(pp.x, 0.0, pp.z).normalize();
                closest_node_to(t, -aim.x * 40.0, 105.0, -aim.z * 40.0)
            }
            None => closest_node_to(t, 40.0, f64::from(egg.y), 0.0),
        };
        let path = find_path(t, current, target, Some(Node::new(egg.x, egg.y, egg.z)));
        let s = st(t);
        s.phases.approach.path = path;
        if let Some(p) = &mut s.phases.approach.path {
            p.advance();
        }
    }
    let (r, s) = parts(t);
    navigate(
        r,
        &mut s.phases.approach.path,
        &mut s.phases.approach.target,
    );
    if st_ref(t)
        .phases
        .approach
        .path
        .as_ref()
        .is_some_and(Path::is_done)
    {
        set_phase(t, Phase::Landing);
    }
}

fn landing_tick(t: &mut Tick<'_, '_>) {
    if st_ref(t).phases.landing.is_none() {
        let egg = egg(t);
        st(t).phases.landing = Some(bottom_center(egg));
    }
    let target = st_ref(t).phases.landing.unwrap_or(Vec3::ZERO);
    if target.distance_to_sqr(t.pos()) < 1.0 {
        st(t).phases.flaming.count = 0;
        set_phase(t, Phase::SittingScanning);
    }
}

fn takeoff_tick(t: &mut Tick<'_, '_>) {
    let s = st_ref(t);
    if !s.phases.takeoff.first_tick && s.phases.takeoff.path.is_some() {
        let egg = egg(t);
        if dist_to_center_sqr(egg, t.pos()) >= 100.0 {
            set_phase(t, Phase::HoldingPattern);
        }
        return;
    }
    st(t).phases.takeoff.first_tick = false;
    // `findNewTarget`.
    let current = closest_node(t);
    let look = head_look_vector(t);
    let mut target =
        i32::try_from(closest_node_to(t, -look.x * 40.0, 105.0, -look.z * 40.0)).unwrap_or(0);
    let outer = crystals_alive(t);
    if !outer {
        target = ((target - 12) & 7) + 12;
    } else {
        target = target.rem_euclid(12);
    }
    let end = usize::try_from(target).unwrap_or(0);
    let path = find_path(t, current, end, None);
    let (r, s) = parts(t);
    s.phases.takeoff.path = path;
    // `navigateToNextPathNode`.
    if let Some(p) = &mut s.phases.takeoff.path {
        p.advance();
        if !p.is_done() {
            let current = p.next_node();
            p.advance();
            let mut y;
            loop {
                y = f64::from(current.y) + f64::from(r.next_float() * 20.0);
                if y >= f64::from(current.y) {
                    break;
                }
            }
            s.phases.takeoff.target =
                Some(Vec3::new(f64::from(current.x), y, f64::from(current.z)));
        }
    }
}

/// `getHeadLookVector(1.0)`.
pub fn head_look_vector(t: &Tick<'_, '_>) -> Vec3 {
    let s = st_ref(t);
    let yr = y_rot(t);
    match s.phase {
        Phase::Landing | Phase::Takeoff => {
            let egg = egg(t);
            #[allow(clippy::cast_possible_truncation)]
            let dist = ((dist_to_center_sqr(egg, t.pos()).sqrt() as f32) / 4.0).max(1.0);
            let y_offset = 6.0 / dist;
            super::view_vector(-y_offset * 1.5 * 5.0, yr)
        }
        p if p.is_sitting() => super::view_vector(-45.0, yr),
        _ => super::view_vector(t.core().rot().x_rot, yr),
    }
}

fn flaming_tick(t: &mut Tick<'_, '_>) {
    let ticks = {
        let s = st(t);
        s.phases.flaming.ticks += 1;
        s.phases.flaming.ticks
    };
    if ticks >= 200 {
        if st_ref(t).phases.flaming.count >= 4 {
            set_phase(t, Phase::Takeoff);
        } else {
            set_phase(t, Phase::SittingScanning);
        }
    } else if ticks == 10 {
        let head = st_ref(t).parts[HEAD];
        let p = t.pos();
        let look = Vec3::new(head.x - p.x, 0.0, head.z - p.z).normalize();
        let x = head.x + look.x * 5.0 / 2.0;
        let z = head.z + look.z * 5.0 / 2.0;
        let initial_y = head.y + f64::from(PARTS[HEAD].1) * 0.5;
        let mut y = initial_y;
        let mut pos = BlockPos::containing(x, initial_y, z);
        while t.level.block(pos).is_air() {
            y -= 1.0;
            if y < 0.0 {
                y = initial_y;
                break;
            }
            pos = BlockPos::containing(x, y, z);
        }
        let y = f64::from(vanillite_base::math::floor(y) + 1);
        let owner = t.id;
        let cloud = super::crystal::spawn_cloud(
            t.level,
            t.ents,
            Vec3::new(x, y, z),
            super::crystal::flame_cloud(Some(owner)),
        );
        st(t).phases.flaming.flame = Some(cloud);
    }
}

fn scanning_tick(t: &mut Tick<'_, '_>) {
    st(t).phases.scanning_time += 1;
    let time = st_ref(t).phases.scanning_time;
    let me = (t.id, t.pos());
    let scan = Targeting {
        range: 20.0,
        max_dy: Some(10.0),
    };
    let target = nearest_player(t.ents, t.env, t.pos(), Some(me), scan);
    if let Some(target) = target {
        if time > 25 {
            set_phase(t, Phase::SittingAttacking);
            return;
        }
        let Some(tp) = t.ents.store.get(target).map(|e| e.pos()) else {
            return;
        };
        let p = t.pos();
        let aim = Vec3::new(tp.x - p.x, 0.0, tp.z - p.z).normalize();
        let yr = y_rot(t);
        let dir = Vec3::new(
            f64::from(super::sin_deg(yr)),
            0.0,
            f64::from(-super::cos_deg(yr)),
        )
        .normalize();
        #[allow(clippy::cast_possible_truncation)]
        let dot = dir.dot(aim) as f32;
        #[allow(clippy::cast_possible_truncation)]
        let angle = (f64::from(dot).acos() * f64::from(57.295_776_f32)) as f32 + 0.5;
        if !(0.0..=10.0).contains(&angle) {
            let head = st_ref(t).parts[HEAD];
            let (xa, za) = (tp.x - head.x, tp.z - head.z);
            let atan = vanillite_base::math::atan2(xa, za);
            let delta = vanillite_base::math::wrap_degrees(
                180.0 - atan * f64::from(57.295_776_f32) - f64::from(yr),
            )
            .clamp(-100.0, 100.0);
            #[allow(clippy::cast_possible_truncation)]
            let mut dist = (xa * xa + za * za).sqrt() as f32 + 1.0;
            let rot_speed = dist;
            if dist > 40.0 {
                dist = 40.0;
            }
            let s = st(t);
            s.y_rot_a *= 0.8;
            #[allow(clippy::cast_possible_truncation)]
            {
                s.y_rot_a += delta as f32 * (0.7 / dist / rot_speed);
            }
            let a = s.y_rot_a;
            super::set_y_rot(t, yr + a);
        }
    } else if time >= 100 {
        let charge = Targeting {
            range: 150.0,
            max_dy: None,
        };
        let target = nearest_player(t.ents, t.env, t.pos(), Some(me), charge);
        set_phase(t, Phase::Takeoff);
        if let Some(tp) = target.and_then(|id| t.ents.store.get(id)).map(|e| e.pos()) {
            set_phase(t, Phase::ChargingPlayer);
            st(t).phases.charge.target = Some(tp);
        }
    }
}

fn charge_tick(t: &mut Tick<'_, '_>) {
    let Some(target) = st_ref(t).phases.charge.target else {
        set_phase(t, Phase::HoldingPattern);
        return;
    };
    let since = st_ref(t).phases.charge.time_since;
    if since > 0 {
        st(t).phases.charge.time_since += 1;
        if since >= 10 {
            set_phase(t, Phase::HoldingPattern);
        }
        return;
    }
    let d = target.distance_to_sqr(t.pos());
    if !(100.0..=22_500.0).contains(&d) {
        st(t).phases.charge.time_since += 1;
    }
}

fn dying_tick(t: &mut Tick<'_, '_>) {
    st(t).phases.dying.time += 1;
    if st_ref(t).phases.dying.target.is_none() {
        let egg = podium(
            t.level,
            st_ref(t).fight_origin,
            HeightmapKind::MotionBlocking,
        );
        st(t).phases.dying.target = Some(bottom_center(egg));
    }
    let target = st_ref(t).phases.dying.target.unwrap_or(Vec3::ZERO);
    let d = target.distance_to_sqr(t.pos());
    let health = if (100.0..=22_500.0).contains(&d) {
        1.0
    } else {
        0.0
    };
    if t.mob_ref().health != health {
        t.mob().health = health;
        t.core_mut().mark_data_dirty();
    }
}

/// The body part index of the main hitbox.
pub const MAIN_PART: usize = BODY;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip() {
        for p in Phase::ALL {
            assert_eq!(Phase::by_id(i64::from(p.id())), p);
        }
        assert_eq!(Phase::Hover.id(), 10);
        assert_eq!(Phase::by_id(99), Phase::HoldingPattern);
    }

    #[test]
    fn transitions_reset_phase_state() {
        let mut s = State::default();
        let mut discard = Vec::new();
        set_phase_in(&mut s, Phase::SittingFlaming, &mut discard);
        assert_eq!(s.phases.flaming.count, 1);
        let mut store = vanillite_world::entity::EntityStore::new();
        let cloud = store.spawn(
            vanillite_world::entity::NewEntity::new(
                vanillite_data::entity::ids::AREA_EFFECT_CLOUD,
                vanillite_world::entity::NetworkId(1),
                1,
                Vec3::ZERO,
            ),
            |_| 0,
        );
        s.phases.flaming.flame = Some(cloud);
        set_phase_in(&mut s, Phase::SittingScanning, &mut discard);
        assert_eq!(
            discard.len(),
            1,
            "leaving the flame phase discards its cloud"
        );
        set_phase_in(&mut s, Phase::SittingFlaming, &mut discard);
        assert_eq!(
            s.phases.flaming.count, 2,
            "the flame count survives between visits"
        );
        assert!(s.phase.is_sitting());
        set_phase_in(&mut s, Phase::Takeoff, &mut discard);
        assert!(s.phases.takeoff.first_tick);
        assert!(!s.phase.is_sitting());
    }

    #[test]
    fn arrows_bounce_off_sitting_dragon() {
        assert_eq!(on_hurt(Phase::SittingScanning, true, 9.0), 0.0);
        assert_eq!(on_hurt(Phase::Hover, true, 9.0), 9.0);
        assert_eq!(on_hurt(Phase::HoldingPattern, true, 9.0), 9.0);
        assert_eq!(on_hurt(Phase::SittingFlaming, false, 9.0), 9.0);
    }

    #[test]
    fn ring_wrapping() {
        assert_eq!(wrap_ring(-1, true), 11);
        assert_eq!(wrap_ring(12, true), 0);
        assert_eq!(wrap_ring(11, false), 19);
        assert_eq!(wrap_ring(20, false), 12);
    }
}

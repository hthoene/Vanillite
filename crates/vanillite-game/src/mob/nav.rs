//! `GroundPathNavigation` and the mob controls (`MoveControl`, `LookControl`, `JumpControl`,
//! `BodyRotationControl`).

use vanillite_base::math::wrap_degrees_f;
use vanillite_base::{BlockPos, Vec3};
use vanillite_data::block::BlockState;
use vanillite_worldgen::behaviour::{Class, class_of};

use super::path::{self, Path, PathMob, PathType, Search};
use super::{Tick, atan2_deg, can_see_sky, is_water};

/// `Mth.degreesDifference`.
pub fn degrees_difference(from: f32, to: f32) -> f32 {
    wrap_degrees_f(to - from)
}

/// `Mth.rotateIfNecessary`.
pub fn rotate_if_necessary(current: f32, target: f32, max: f32) -> f32 {
    let diff = degrees_difference(current, target);
    target - diff.clamp(-max, max)
}

/// `LookControl.rotateTowards`.
fn rotate_towards(from: f32, to: f32, max: f32) -> f32 {
    from + degrees_difference(from, to).clamp(-max, max)
}

/// The state of a mob's `GroundPathNavigation`.
#[derive(Clone, Debug)]
pub struct Navigation {
    pub path: Option<Path>,
    pub speed_modifier: f64,
    tick: i32,
    last_stuck_check: i32,
    last_stuck_check_pos: Vec3,
    timeout_cached_node: BlockPos,
    timeout_timer: i64,
    last_timeout_check: i64,
    timeout_limit: f64,
    max_distance_to_waypoint: f32,
    has_delayed_recomputation: bool,
    time_last_recompute: i64,
    target_pos: Option<BlockPos>,
    reach_range: i32,
    max_visited_nodes: i32,
    required_path_length: f32,
    pub is_stuck: bool,
    pub avoid_sun: bool,
    pub can_float: bool,
    pub can_open_doors: bool,
    pub can_pass_doors: bool,
}

impl Navigation {
    /// The navigation of a mob with the base follow range `follow_range`.
    pub fn new(follow_range: f64) -> Navigation {
        Navigation {
            path: None,
            speed_modifier: 0.0,
            tick: 0,
            last_stuck_check: 0,
            last_stuck_check_pos: Vec3::ZERO,
            timeout_cached_node: BlockPos::ZERO,
            timeout_timer: 0,
            last_timeout_check: 0,
            timeout_limit: 0.0,
            max_distance_to_waypoint: 0.5,
            has_delayed_recomputation: false,
            time_last_recompute: 0,
            target_pos: None,
            reach_range: 0,
            max_visited_nodes: vanillite_base::math::floor(follow_range * 16.0),
            required_path_length: 16.0,
            is_stuck: false,
            avoid_sun: false,
            can_float: false,
            can_open_doors: false,
            can_pass_doors: true,
        }
    }

    /// `isDone`.
    pub fn is_done(&self) -> bool {
        self.path.as_ref().is_none_or(Path::is_done)
    }

    /// `stop`.
    pub fn stop(&mut self) {
        self.path = None;
    }

    fn reset_stuck_timeout(&mut self) {
        self.timeout_cached_node = BlockPos::ZERO;
        self.timeout_timer = 0;
        self.timeout_limit = 0.0;
        self.is_stuck = false;
    }
}

/// `canUpdatePath`: on the ground or in a liquid.
fn can_update_path(t: &Tick<'_, '_>) -> bool {
    let m = t.mob_ref();
    t.on_ground() || m.fluids.in_water() || m.fluids.in_lava()
}

/// `getTempMobPos`.
fn temp_mob_pos(t: &Tick<'_, '_>) -> Vec3 {
    let p = t.pos();
    let m = t.mob_ref();
    let y = if m.fluids.in_water() && m.nav.can_float {
        let mut surface = vanillite_base::math::floor(p.y);
        let mut steps = 0;
        loop {
            let state = t
                .level
                .block(BlockPos::containing(p.x, f64::from(surface), p.z));
            if !is_water(state) {
                break;
            }
            surface += 1;
            steps += 1;
            if steps > 16 {
                surface = vanillite_base::math::floor(p.y);
                break;
            }
        }
        surface
    } else {
        vanillite_base::math::floor(p.y + 0.5)
    };
    Vec3::new(p.x, f64::from(y), p.z)
}

/// The mob as the path finder sees it.
fn path_mob(t: &Tick<'_, '_>) -> PathMob {
    let core = t.core();
    let dims = core.dims();
    let m = t.mob_ref();
    PathMob {
        pos: core.pos(),
        aabb: core.aabb(),
        width: dims.width,
        height: dims.height,
        on_ground: t.on_ground(),
        #[allow(clippy::cast_possible_truncation)]
        max_up_step: m.attrs.step_height as f32,
        max_fall_distance: super::living::max_fall_distance(t),
        in_water: m.fluids.in_water(),
        can_float: m.nav.can_float,
        can_open_doors: m.nav.can_open_doors,
        can_pass_doors: m.nav.can_pass_doors,
    }
}

/// `PathNavigation.createPath(targets, radiusOffset, above, reachRange, maxPathLength)`.
fn create_path(t: &mut Tick<'_, '_>, target: BlockPos, reach_range: i32) -> Option<Path> {
    if t.pos().y < f64::from(t.level.min_y()) || !can_update_path(t) {
        return None;
    }
    {
        let nav = &t.mob_ref().nav;
        if let Some(p) = &nav.path
            && !p.is_done()
            && nav.target_pos == Some(target)
        {
            return Some(p.clone());
        }
    }
    if t.cx.path_budget == 0 {
        return None;
    }
    let mob = path_mob(t);
    let m = t.mob_ref();
    #[allow(clippy::cast_possible_truncation)]
    let max_len = (m.attrs.follow_range as f32).max(m.nav.required_path_length);
    let budget = i32::try_from(t.cx.path_budget).unwrap_or(i32::MAX);
    let search = Search {
        max_path_length: max_len,
        reach_range,
        max_visited_nodes: m.nav.max_visited_nodes.min(budget),
        malus: m.kind.malus(),
        min_y: t.level.min_y(),
    };
    let path = t.cx.finder.find_path(t.level, &mob, &[target], search);
    let visited = t.cx.finder.take_visited();
    if t.cx.path_budget != u64::MAX {
        t.cx.path_budget = t.cx.path_budget.saturating_sub(visited);
    }
    if let Some(p) = &path {
        let nav = &mut t.mob().nav;
        nav.target_pos = Some(p.target);
        nav.reach_range = reach_range;
        nav.reset_stuck_timeout();
    }
    path
}

/// `GroundPathNavigation.findSurfacePosition`.
fn surface_position(t: &Tick<'_, '_>, mut pos: BlockPos) -> BlockPos {
    let level = t.level;
    if level.block(pos).is_air() {
        let mut y = pos.y - 1;
        while y >= level.min_y() && level.block(BlockPos::new(pos.x, y, pos.z)).is_air() {
            y -= 1;
        }
        if y >= level.min_y() {
            return BlockPos::new(pos.x, y + 1, pos.z);
        }
        y = pos.y + 1;
        while y <= level.max_y() && level.block(BlockPos::new(pos.x, y, pos.z)).is_air() {
            y += 1;
        }
        pos = BlockPos::new(pos.x, y, pos.z);
    }
    if !is_solid(level.block(pos)) {
        return pos;
    }
    let mut y = pos.y + 1;
    while y <= level.max_y() && is_solid(level.block(BlockPos::new(pos.x, y, pos.z))) {
        y += 1;
    }
    BlockPos::new(pos.x, y, pos.z)
}

/// `BlockState.isSolid` (legacy solidity).
pub fn is_solid(state: BlockState) -> bool {
    state.has(vanillite_data::block::flags::SOLID)
}

/// `GroundPathNavigation.createPath(pos, reachRange)`.
pub fn create_path_to(t: &mut Tick<'_, '_>, pos: BlockPos, reach_range: i32) -> Option<Path> {
    if !t.level.is_loaded(pos) {
        return None;
    }
    let target = surface_position(t, pos);
    create_path(t, target, reach_range)
}

/// `moveTo(path, speedModifier)`.
pub fn move_to_path(t: &mut Tick<'_, '_>, path: Option<Path>, speed: f64) -> bool {
    let Some(new) = path else {
        t.mob().nav.path = None;
        return false;
    };
    {
        let nav = &mut t.mob().nav;
        let same = nav.path.as_ref().is_some_and(|p| new.same_nodes(p));
        if !same {
            nav.path = Some(new);
        }
        if nav.is_done() {
            return false;
        }
    }
    trim_path(t);
    let pos = temp_mob_pos(t);
    let nav = &mut t.mob().nav;
    if nav.path.as_ref().is_none_or(|p| p.nodes.is_empty()) {
        return false;
    }
    nav.speed_modifier = speed;
    nav.last_stuck_check = nav.tick;
    nav.last_stuck_check_pos = pos;
    true
}

/// `moveTo(x, y, z, speedModifier)`.
pub fn move_to_pos(t: &mut Tick<'_, '_>, pos: Vec3, speed: f64) -> bool {
    let path = create_path_to(t, BlockPos::containing(pos.x, pos.y, pos.z), 1);
    move_to_path(t, path, speed)
}

/// `moveTo(entity, reachRange, speedModifier)`.
pub fn move_to_entity(t: &mut Tick<'_, '_>, target: Vec3, reach: i32, speed: f64) -> bool {
    let path = create_path_to(t, BlockPos::containing(target.x, target.y, target.z), reach);
    path.is_some() && move_to_path(t, path, speed)
}

/// `GroundPathNavigation.trimPath`: with `avoidSun`, the path ends before the first node under
/// the open sky.
fn trim_path(t: &mut Tick<'_, '_>) {
    if !t.mob_ref().nav.avoid_sun {
        return;
    }
    let p = t.pos();
    if can_see_sky(t.level, BlockPos::containing(p.x, p.y + 0.5, p.z)) {
        return;
    }
    let level = t.level;
    let nav = &mut t.mob().nav;
    if let Some(path) = &mut nav.path
        && let Some(i) = path.nodes.iter().position(|n| can_see_sky(level, n.pos()))
    {
        path.nodes.truncate(i);
    }
}

/// `recomputePath`.
fn recompute_path(t: &mut Tick<'_, '_>) {
    let time = t.game_time();
    if time - t.mob_ref().nav.time_last_recompute <= 20 || !can_update_path(t) {
        t.mob().nav.has_delayed_recomputation = true;
        return;
    }
    let Some(target) = t.mob_ref().nav.target_pos else {
        return;
    };
    let reach = t.mob_ref().nav.reach_range;
    t.mob().nav.path = None;
    let path = create_path_to(t, target, reach);
    let nav = &mut t.mob().nav;
    nav.path = path;
    nav.time_last_recompute = time;
    nav.has_delayed_recomputation = false;
}

/// `PathNavigation.tick`.
pub fn tick(t: &mut Tick<'_, '_>) {
    t.mob().nav.tick += 1;
    if t.mob_ref().nav.has_delayed_recomputation {
        recompute_path(t);
    }
    if t.mob_ref().nav.is_done() {
        return;
    }
    let width = t.core().dims().width;
    if can_update_path(t) {
        follow_the_path(t);
    } else {
        let mob_pos = temp_mob_pos(t);
        let on_ground = t.on_ground();
        let nav = &mut t.mob().nav;
        if let Some(path) = &mut nav.path
            && !path.is_done()
        {
            let pos = path.next_entity_pos(width);
            if mob_pos.y > pos.y
                && !on_ground
                && vanillite_base::math::floor(mob_pos.x) == vanillite_base::math::floor(pos.x)
                && vanillite_base::math::floor(mob_pos.z) == vanillite_base::math::floor(pos.z)
            {
                path.next += 1;
            }
        }
    }
    let target = {
        let nav = &t.mob_ref().nav;
        match &nav.path {
            Some(p) if !p.is_done() => Some(p.next_entity_pos(width)),
            _ => None,
        }
    };
    if let Some(target) = target {
        let ground = ground_y(t, target);
        let speed = t.mob_ref().nav.speed_modifier;
        t.mob()
            .move_ctl
            .set_wanted_position(Vec3::new(target.x, ground, target.z), speed);
    }
}

/// `getGroundY`.
fn ground_y(t: &Tick<'_, '_>, target: Vec3) -> f64 {
    let pos = BlockPos::containing(target.x, target.y, target.z);
    if t.level
        .block(BlockPos::new(pos.x, pos.y - 1, pos.z))
        .is_air()
    {
        target.y
    } else {
        path::floor_level(t.level, pos)
    }
}

/// `canCutCorner`.
fn can_cut_corner(kind: PathType) -> bool {
    kind != PathType::FireInNeighbor
        && kind != PathType::DamagingInNeighbor
        && kind != PathType::WalkableDoor
}

/// `followThePath`.
fn follow_the_path(t: &mut Tick<'_, '_>) {
    let mob_pos = temp_mob_pos(t);
    let width = t.core().dims().width;
    let pos = t.pos();
    let advance = {
        let nav = &mut t.mob().nav;
        nav.max_distance_to_waypoint = if width > 0.75 {
            width / 2.0
        } else {
            0.75 - width / 2.0
        };
        let Some(path) = &nav.path else {
            return;
        };
        let Some(node) = path.nodes.get(path.next) else {
            return;
        };
        let max = f64::from(nav.max_distance_to_waypoint);
        let close = (pos.x - (f64::from(node.x) + 0.5)).abs() < max
            && (pos.z - (f64::from(node.z) + 0.5)).abs() < max
            && (pos.y - f64::from(node.y)).abs() < 1.0;
        close || can_cut_corner(node.kind) && should_target_next_node(path, mob_pos, width)
    };
    if advance && let Some(path) = &mut t.mob().nav.path {
        path.next += 1;
    }
    do_stuck_detection(t, mob_pos);
}

/// `shouldTargetNextNodeInDirection` (ground navigation cannot move directly).
#[allow(clippy::neg_cmp_op_on_partial_ord)]
fn should_target_next_node(path: &Path, mob: Vec3, _width: f32) -> bool {
    if path.next + 1 >= path.nodes.len() {
        return false;
    }
    let bottom =
        |n: &path::PathNode| Vec3::new(f64::from(n.x) + 0.5, f64::from(n.y), f64::from(n.z) + 0.5);
    let current = bottom(&path.nodes[path.next]);
    if mob.distance_to_sqr(current) >= 4.0 {
        return false;
    }
    let next = bottom(&path.nodes[path.next + 1]);
    let to_current = current - mob;
    let to_next = next - mob;
    let cur_sqr = to_current.length_sqr();
    let next_sqr = to_next.length_sqr();
    if !(next_sqr < cur_sqr) && !(cur_sqr < 0.5) {
        return false;
    }
    to_next.normalize().dot(to_current.normalize()) < 0.0
}

/// `doStuckDetection`.
fn do_stuck_detection(t: &mut Tick<'_, '_>, mob_pos: Vec3) {
    let time = t.game_time();
    let m = t.mob();
    let speed = m.speed;
    let nav = &mut m.nav;
    if nav.tick - nav.last_stuck_check > 100 {
        let effective = if speed >= 1.0 { speed } else { speed * speed };
        let threshold = f64::from(effective * 100.0 * 0.25);
        if mob_pos.distance_to_sqr(nav.last_stuck_check_pos) < threshold * threshold {
            nav.is_stuck = true;
            nav.stop();
        } else {
            nav.is_stuck = false;
        }
        nav.last_stuck_check = nav.tick;
        nav.last_stuck_check_pos = mob_pos;
    }
    let node = match &nav.path {
        Some(p) if !p.is_done() => p.nodes[p.next].pos(),
        _ => return,
    };
    if node == nav.timeout_cached_node {
        nav.timeout_timer += time - nav.last_timeout_check;
    } else {
        nav.timeout_cached_node = node;
        let center = Vec3::new(
            f64::from(node.x) + 0.5,
            f64::from(node.y),
            f64::from(node.z) + 0.5,
        );
        let dist = mob_pos.distance_to_sqr(center).sqrt();
        nav.timeout_limit = if speed > 0.0 {
            dist / f64::from(speed) * 20.0
        } else {
            0.0
        };
    }
    #[allow(clippy::cast_precision_loss)]
    let timer = nav.timeout_timer as f64;
    if nav.timeout_limit > 0.0 && timer > nav.timeout_limit * 3.0 {
        nav.reset_stuck_timeout();
        nav.stop();
    }
    nav.last_timeout_check = time;
}

/// `MoveControl.Operation`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Operation {
    #[default]
    Wait,
    MoveTo,
    Strafe,
    Jumping,
}

/// `MoveControl`.
#[derive(Clone, Copy, Debug, Default)]
pub struct MoveControl {
    pub wanted: Vec3,
    pub speed_modifier: f64,
    pub strafe_forwards: f32,
    pub strafe_right: f32,
    pub operation: Operation,
}

impl MoveControl {
    /// `setWantedPosition`.
    pub fn set_wanted_position(&mut self, pos: Vec3, speed: f64) {
        self.wanted = pos;
        self.speed_modifier = speed;
        if self.operation != Operation::Jumping {
            self.operation = Operation::MoveTo;
        }
    }

    /// `strafe`.
    pub fn strafe(&mut self, forwards: f32, right: f32) {
        self.operation = Operation::Strafe;
        self.strafe_forwards = forwards;
        self.strafe_right = right;
        self.speed_modifier = 0.25;
    }

    /// `hasWanted`.
    pub fn has_wanted(&self) -> bool {
        self.operation == Operation::MoveTo
    }
}

/// `MoveControl.rotlerp`: turns at most `max` degrees, wrapped into `[0, 360]`.
pub fn rotlerp(a: f32, b: f32, max: f32) -> f32 {
    let diff = wrap_degrees_f(b - a).clamp(-max, max);
    let mut r = a + diff;
    if r < 0.0 {
        r += 360.0;
    } else if r > 360.0 {
        r -= 360.0;
    }
    r
}

/// `MoveControl.tick`.
/// `Mob.setSpeed`: the speed is also the forward input.
fn set_speed(m: &mut super::MobData, speed: f32) {
    m.speed = speed;
    m.zza = speed;
}

pub fn tick_move_control(t: &mut Tick<'_, '_>) {
    let speed_attr = t.mob_ref().attrs.movement_speed;
    let ctl = t.mob_ref().move_ctl;
    match ctl.operation {
        Operation::Strafe => {
            #[allow(clippy::cast_possible_truncation)]
            let speed = speed_attr as f32;
            #[allow(clippy::cast_possible_truncation)]
            let modified = ctl.speed_modifier as f32 * speed;
            let mut xa = ctl.strafe_forwards;
            let mut za = ctl.strafe_right;
            let mut dist = (xa * xa + za * za).sqrt();
            if dist < 1.0 {
                dist = 1.0;
            }
            dist = modified / dist;
            xa *= dist;
            za *= dist;
            let rad = f64::from(t.core().rot().y_rot * super::DEG_TO_RAD);
            let sin = vanillite_base::math::sin(rad);
            let cos = vanillite_base::math::cos(rad);
            let dx = xa * cos - za * sin;
            let dz = za * cos + xa * sin;
            let p = t.pos();
            let at = BlockPos::containing(p.x + f64::from(dx), p.y.floor(), p.z + f64::from(dz));
            let walkable = path::path_type_static(t.level, t.level.min_y(), at.x, at.y, at.z)
                == PathType::Walkable;
            let m = t.mob();
            if !walkable {
                m.move_ctl.strafe_forwards = 1.0;
                m.move_ctl.strafe_right = 0.0;
            }
            m.speed = modified;
            m.zza = m.move_ctl.strafe_forwards;
            m.xxa = m.move_ctl.strafe_right;
            m.move_ctl.operation = Operation::Wait;
        }
        Operation::MoveTo => {
            t.mob().move_ctl.operation = Operation::Wait;
            let p = t.pos();
            let xd = ctl.wanted.x - p.x;
            let zd = ctl.wanted.z - p.z;
            let yd = ctl.wanted.y - p.y;
            let dd = xd * xd + yd * yd + zd * zd;
            if dd < f64::from(2.500_000_3e-7_f32) {
                t.mob().zza = 0.0;
                return;
            }
            #[allow(clippy::cast_possible_truncation)]
            let y_rot_d = atan2_deg(zd, xd) as f32 - 90.0;
            let mut rot = t.core().rot();
            rot.y_rot = rotlerp(rot.y_rot, y_rot_d, 90.0);
            t.core_mut().set_rot(rot);
            let width = t.core().dims().width;
            let step = t.mob_ref().attrs.step_height;
            #[allow(clippy::cast_possible_truncation)]
            set_speed(t.mob(), (ctl.speed_modifier * speed_attr) as f32);
            let pos = t.block_pos();
            let state = t.level.block(pos);
            let shape = state.collision_shape();
            let top = shape.iter().map(|b| b[4]).fold(f64::NEG_INFINITY, f64::max);
            let class = class_of(state);
            let blocked = !shape.is_empty()
                && p.y < top + f64::from(pos.y)
                && class != Class::Door
                && class != Class::Fence;
            if yd > step && xd * xd + zd * zd < f64::from(width.max(1.0)) || blocked {
                t.mob().jump = true;
                t.mob().move_ctl.operation = Operation::Jumping;
            }
        }
        Operation::Jumping => {
            #[allow(clippy::cast_possible_truncation)]
            set_speed(t.mob(), (ctl.speed_modifier * speed_attr) as f32);
            let fluid = {
                let m = t.mob_ref();
                m.fluids.in_water() || m.fluids.in_lava()
            };
            if t.on_ground() || fluid {
                t.mob().move_ctl.operation = Operation::Wait;
            }
        }
        Operation::Wait => {
            t.mob().zza = 0.0;
        }
    }
}

/// `LookControl`.
#[derive(Clone, Copy, Debug, Default)]
pub struct LookControl {
    pub wanted: Vec3,
    pub y_max_rot_speed: f32,
    pub x_max_rot_angle: f32,
    pub cooldown: i32,
}

impl LookControl {
    /// `setLookAt(x, y, z, yMaxRotSpeed, xMaxRotAngle)`.
    pub fn set_look_at(&mut self, at: Vec3, y_max: f32, x_max: f32) {
        self.wanted = at;
        self.y_max_rot_speed = y_max;
        self.x_max_rot_angle = x_max;
        self.cooldown = 2;
    }

    /// `isLookingAtTarget`.
    pub fn is_looking(&self) -> bool {
        self.cooldown > 0
    }
}

/// `setLookAt(x, y, z)` with the mob's head speed (10) and pitch limit (40).
pub fn look_at(t: &mut Tick<'_, '_>, at: Vec3) {
    t.mob().look.set_look_at(at, 10.0, 40.0);
}

/// `LookControl.tick`.
pub fn tick_look_control(t: &mut Tick<'_, '_>) {
    let p = t.pos();
    let eye = t.eye_y();
    let mut rot = t.core().rot();
    rot.x_rot = 0.0;
    let body = t.mob_ref().y_body_rot;
    let look = t.mob_ref().look;
    if look.cooldown > 0 {
        t.mob().look.cooldown -= 1;
        let xd = look.wanted.x - p.x;
        let yd = look.wanted.y - eye;
        let zd = look.wanted.z - p.z;
        if zd.abs() > 1.0e-5 || xd.abs() > 1.0e-5 {
            #[allow(clippy::cast_possible_truncation)]
            let y_rot_d = atan2_deg(zd, xd) as f32 - 90.0;
            rot.y_head_rot = rotate_towards(rot.y_head_rot, y_rot_d, look.y_max_rot_speed);
        }
        let sd = (xd * xd + zd * zd).sqrt();
        if yd.abs() > 1.0e-5 || sd.abs() > 1.0e-5 {
            #[allow(clippy::cast_possible_truncation)]
            let x_rot_d = -(atan2_deg(yd, sd) as f32);
            rot.x_rot = rotate_towards(rot.x_rot, x_rot_d, look.x_max_rot_angle);
        }
    } else {
        rot.y_head_rot = rotate_towards(rot.y_head_rot, body, 10.0);
    }
    if !t.mob_ref().nav.is_done() {
        rot.y_head_rot = rotate_if_necessary(rot.y_head_rot, body, 75.0);
    }
    t.core_mut().set_rot(rot);
}

/// `BodyRotationControl`.
#[derive(Clone, Copy, Debug, Default)]
pub struct BodyRotation {
    head_stable_time: i32,
    last_stable_y_head_rot: f32,
}

/// `BodyRotationControl.clientTick` (run by `Mob.tickHeadTurn` on the server too).
pub fn tick_body_rotation(t: &mut Tick<'_, '_>) {
    let core = t.core();
    let moved = {
        let d = core.pos() - core.old_pos();
        d.x * d.x + d.z * d.z > f64::from(2.500_000_3e-7_f32)
    };
    let mut rot = core.rot();
    let m = t.mob();
    if moved {
        m.y_body_rot = rot.y_rot;
        rot.y_head_rot = rotate_if_necessary(rot.y_head_rot, m.y_body_rot, 75.0);
        m.body.last_stable_y_head_rot = rot.y_head_rot;
        m.body.head_stable_time = 0;
    } else if (rot.y_head_rot - m.body.last_stable_y_head_rot).abs() > 15.0 {
        m.body.head_stable_time = 0;
        m.body.last_stable_y_head_rot = rot.y_head_rot;
        m.y_body_rot = rotate_if_necessary(m.y_body_rot, rot.y_head_rot, 75.0);
    } else {
        m.body.head_stable_time += 1;
        if m.body.head_stable_time > 10 {
            #[allow(clippy::cast_precision_loss)]
            let fraction = ((m.body.head_stable_time - 10) as f32 / 10.0).clamp(0.0, 1.0);
            let remaining = 75.0 * (1.0 - fraction);
            m.y_body_rot = rotate_if_necessary(m.y_body_rot, rot.y_head_rot, remaining);
        }
    }
    t.core_mut().set_rot(rot);
}

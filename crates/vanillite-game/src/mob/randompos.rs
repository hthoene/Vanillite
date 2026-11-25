//! Random target positions of goals (`RandomPos`, `DefaultRandomPos`, `LandRandomPos`,
//! `GoalUtils`) with the mob's home restriction. The air, hover and water variants live next
//! to the navigation that uses them.

use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Vec3};

use super::Tick;
use super::path::{malus_of, path_type_static};

/// `Mth.SQRT_OF_TWO` (a float).
fn sqrt_of_two() -> f64 {
    f64::from(2.0_f32.sqrt())
}

/// `(float) (Math.PI / 2)` as the goals pass it.
pub fn half_pi() -> f64 {
    f64::from(std::f32::consts::FRAC_PI_2)
}

/// `RandomPos.generateRandomPos(supplier, weight)`: the best of 10 candidates, as the bottom
/// centre of its block.
pub fn generate(
    t: &mut Tick<'_, '_>,
    mut candidate: impl FnMut(&mut Tick<'_, '_>) -> Option<BlockPos>,
    weight: impl Fn(&Tick<'_, '_>, BlockPos) -> f64,
) -> Option<Vec3> {
    // A candidate must weigh more than negative infinity.
    let mut best: Option<BlockPos> = None;
    let mut best_weight = f64::NEG_INFINITY;
    for _ in 0..10 {
        let Some(pos) = candidate(t) else {
            continue;
        };
        let value = weight(t, pos);
        if value > best_weight {
            best_weight = value;
            best = Some(pos);
        }
    }
    best.map(bottom_center)
}

/// `Vec3.atBottomCenterOf`.
pub fn bottom_center(p: BlockPos) -> Vec3 {
    Vec3::new(f64::from(p.x) + 0.5, f64::from(p.y), f64::from(p.z) + 0.5)
}

/// `getWalkTargetValue(pos)` of the ticking mob.
pub fn walk_value(t: &Tick<'_, '_>, pos: BlockPos) -> f64 {
    let m = t.mob_ref();
    let value = super::kinds::walk_target_value(m.kind, t.level, &t.cx.env, pos);
    // `Strider.getWalkTargetValue`: in lava, everything off lava is out of the question.
    if m.kind == super::Kind::Strider && value == 0.0 && m.fluids.in_lava() {
        return f64::NEG_INFINITY;
    }
    value
}

/// `RandomPos.generateRandomDirection`.
pub fn random_direction(t: &mut Tick<'_, '_>, h: i32, v: i32) -> BlockPos {
    let r = &mut t.mob().random;
    let x = r.next_int_bounded(2 * h + 1) - h;
    let y = r.next_int_bounded(2 * v + 1) - v;
    let z = r.next_int_bounded(2 * h + 1) - h;
    BlockPos::new(x, y, z)
}

/// `RandomPos.generateRandomDirectionWithinRadians`.
#[allow(clippy::too_many_arguments)]
pub fn direction_within_radians(
    t: &mut Tick<'_, '_>,
    min_h: f64,
    max_h: f64,
    v: i32,
    flying_height: i32,
    x_dir: f64,
    z_dir: f64,
    max_radians: f64,
) -> Option<BlockPos> {
    let r = &mut t.mob().random;
    let center = vanillite_base::math::atan2(z_dir, x_dir) - half_pi();
    let y_radians = center + f64::from(2.0 * r.next_float() - 1.0) * max_radians;
    let dist = (min_h + r.next_double().sqrt() * (max_h - min_h)) * sqrt_of_two();
    let xt = -dist * y_radians.sin();
    let zt = dist * y_radians.cos();
    if xt.abs() > max_h || zt.abs() > max_h {
        return None;
    }
    let yt = r.next_int_bounded(2 * v + 1) - v + flying_height;
    Some(BlockPos::containing(xt, f64::from(yt), zt))
}

/// `GoalUtils.mobRestricted`: the mob has a home it could leave within `h`.
pub fn mob_restricted(t: &Tick<'_, '_>, h: f64) -> bool {
    let m = t.mob_ref();
    if !m.has_restriction() {
        return false;
    }
    let c = m.restrict_center;
    let center = Vec3::new(
        f64::from(c.x) + 0.5,
        f64::from(c.y) + 0.5,
        f64::from(c.z) + 0.5,
    );
    let r = f64::from(m.restrict_radius) + h + 1.0;
    center.distance_to_sqr(t.pos()) < r * r
}

/// `RandomPos.generateRandomPosTowardDirection`: the direction from the mob, pulled towards
/// its home.
pub fn toward_direction(t: &mut Tick<'_, '_>, xz_dist: f64, dir: BlockPos) -> BlockPos {
    let (mut xt, mut zt) = (f64::from(dir.x), f64::from(dir.z));
    let p = t.pos();
    if t.mob_ref().has_restriction() && xz_dist > 1.0 {
        let c = t.mob_ref().restrict_center;
        let r = &mut t.mob().random;
        if p.x > f64::from(c.x) {
            xt -= r.next_double() * xz_dist / 2.0;
        } else {
            xt += r.next_double() * xz_dist / 2.0;
        }
        if p.z > f64::from(c.z) {
            zt -= r.next_double() * xz_dist / 2.0;
        } else {
            zt += r.next_double() * xz_dist / 2.0;
        }
    }
    BlockPos::containing(xt + p.x, f64::from(dir.y) + p.y, zt + p.z)
}

/// `GoalUtils.isRestricted`.
pub fn is_restricted(t: &Tick<'_, '_>, restrict: bool, pos: BlockPos) -> bool {
    restrict && !t.mob_ref().is_within_restriction(pos)
}

/// `GoalUtils.hasMalus`.
pub fn has_malus(t: &Tick<'_, '_>, pos: BlockPos) -> bool {
    let kind = path_type_static(t.level, t.level.min_y(), pos.x, pos.y, pos.z);
    malus_of(super::kinds::mob_malus(t.mob_ref()), kind) != 0.0
}

/// `PathNavigation.isStableDestination` of the mob's navigation.
pub fn is_stable(t: &Tick<'_, '_>, pos: BlockPos) -> bool {
    super::nav::is_stable_destination(t, pos)
}

/// `GoalUtils.isSolid`.
pub fn is_solid(t: &Tick<'_, '_>, pos: BlockPos) -> bool {
    super::nav::is_solid(t.level.block(pos))
}

/// `GoalUtils.isWater`.
pub fn is_water(t: &Tick<'_, '_>, pos: BlockPos) -> bool {
    super::is_water(t.level.block(pos))
}

fn up(p: BlockPos) -> BlockPos {
    BlockPos::new(p.x, p.y + 1, p.z)
}

/// `RandomPos.moveUpOutOfSolid`.
pub fn move_up_out_of_solid(
    pos: BlockPos,
    max_y: i32,
    solid: impl Fn(BlockPos) -> bool,
) -> BlockPos {
    if !solid(pos) {
        return pos;
    }
    let mut p = up(pos);
    while p.y <= max_y && solid(p) {
        p = up(p);
    }
    p
}

/// `RandomPos.moveUpToAboveSolid`.
pub fn move_up_to_above_solid(
    pos: BlockPos,
    above: i32,
    max_y: i32,
    solid: impl Fn(BlockPos) -> bool,
) -> BlockPos {
    if !solid(pos) {
        return pos;
    }
    let mut p = up(pos);
    while p.y <= max_y && solid(p) {
        p = up(p);
    }
    let first = p.y;
    while p.y <= max_y && p.y - first < above {
        p = up(p);
        if solid(p) {
            p = BlockPos::new(p.x, p.y - 1, p.z);
            break;
        }
    }
    p
}

/// `DefaultRandomPos.generateRandomPosTowardDirection`.
fn default_toward(t: &mut Tick<'_, '_>, h: i32, restrict: bool, dir: BlockPos) -> Option<BlockPos> {
    let pos = toward_direction(t, f64::from(h), dir);
    (t.level.in_height(pos.y)
        && !is_restricted(t, restrict, pos)
        && is_stable(t, pos)
        && !has_malus(t, pos))
    .then_some(pos)
}

/// `DefaultRandomPos.getPos(mob, h, v)`.
pub fn default_pos(t: &mut Tick<'_, '_>, h: i32, v: i32) -> Option<Vec3> {
    let restrict = mob_restricted(t, f64::from(h));
    generate(
        t,
        |t| {
            let dir = random_direction(t, h, v);
            default_toward(t, h, restrict, dir)
        },
        walk_value,
    )
}

/// `DefaultRandomPos.getPosTowards(mob, h, v, towards, maxRadians)`.
pub fn default_pos_towards(
    t: &mut Tick<'_, '_>,
    h: i32,
    v: i32,
    towards: Vec3,
    max_radians: f64,
) -> Option<Vec3> {
    let p = t.pos();
    let (dx, dz) = (towards.x - p.x, towards.z - p.z);
    let restrict = mob_restricted(t, f64::from(h));
    generate(
        t,
        |t| {
            let dir = direction_within_radians(t, 0.0, f64::from(h), v, 0, dx, dz, max_radians)?;
            default_toward(t, h, restrict, dir)
        },
        walk_value,
    )
}

/// `DefaultRandomPos.getPosAway(mob, h, v, avoid)`.
pub fn default_pos_away(t: &mut Tick<'_, '_>, h: i32, v: i32, avoid: Vec3) -> Option<Vec3> {
    let p = t.pos();
    let (dx, dz) = (p.x - avoid.x, p.z - avoid.z);
    let restrict = mob_restricted(t, f64::from(h));
    generate(
        t,
        |t| {
            let dir = direction_within_radians(t, 0.0, f64::from(h), v, 0, dx, dz, half_pi())?;
            default_toward(t, h, restrict, dir)
        },
        walk_value,
    )
}

/// `LandRandomPos.generateRandomPosTowardDirection`.
fn land_toward(t: &mut Tick<'_, '_>, h: f64, restrict: bool, dir: BlockPos) -> Option<BlockPos> {
    let pos = toward_direction(t, h, dir);
    (t.level.in_height(pos.y) && !is_restricted(t, restrict, pos) && is_stable(t, pos))
        .then_some(pos)
}

/// `LandRandomPos.movePosUpOutOfSolid`.
pub fn land_move_up(t: &Tick<'_, '_>, pos: BlockPos) -> Option<BlockPos> {
    let pos = move_up_out_of_solid(pos, t.level.max_y(), |p| is_solid(t, p));
    (!is_water(t, pos) && !has_malus(t, pos)).then_some(pos)
}

/// `LandRandomPos.getPos(mob, h, v)` with the walk target value.
pub fn land_pos(t: &mut Tick<'_, '_>, h: i32, v: i32) -> Option<Vec3> {
    land_pos_weighted(t, h, v, walk_value)
}

/// `LandRandomPos.getPos(mob, h, v, weight)`.
pub fn land_pos_weighted(
    t: &mut Tick<'_, '_>,
    h: i32,
    v: i32,
    weight: impl Fn(&Tick<'_, '_>, BlockPos) -> f64,
) -> Option<Vec3> {
    let restrict = mob_restricted(t, f64::from(h));
    generate(
        t,
        |t| {
            let dir = random_direction(t, h, v);
            let pos = land_toward(t, f64::from(h), restrict, dir)?;
            land_move_up(t, pos)
        },
        weight,
    )
}

/// `LandRandomPos.getPosInDirection`.
fn land_pos_in_direction(
    t: &mut Tick<'_, '_>,
    min_h: f64,
    max_h: f64,
    v: i32,
    dir: Vec3,
    restrict: bool,
) -> Option<Vec3> {
    generate(
        t,
        |t| {
            let d = direction_within_radians(t, min_h, max_h, v, 0, dir.x, dir.z, half_pi())?;
            let pos = land_toward(t, max_h, restrict, d)?;
            land_move_up(t, pos)
        },
        walk_value,
    )
}

/// `LandRandomPos.getPosTowards(mob, h, v, towards)`.
pub fn land_pos_towards(t: &mut Tick<'_, '_>, h: i32, v: i32, towards: Vec3) -> Option<Vec3> {
    let p = t.pos();
    let dir = Vec3::new(towards.x - p.x, towards.y - p.y, towards.z - p.z);
    let restrict = mob_restricted(t, f64::from(h));
    land_pos_in_direction(t, 0.0, f64::from(h), v, dir, restrict)
}

/// `LandRandomPos.getPosAway(mob, minH, maxH, v, avoid)`.
pub fn land_pos_away(
    t: &mut Tick<'_, '_>,
    min_h: f64,
    max_h: f64,
    v: i32,
    avoid: Vec3,
) -> Option<Vec3> {
    let p = t.pos();
    let mut dir = Vec3::new(p.x - avoid.x, p.y - avoid.y, p.z - avoid.z);
    if dir.length() == 0.0 {
        let r = &mut t.mob().random;
        dir = Vec3::new(r.next_double() - 0.5, 0.0, r.next_double() - 0.5);
    }
    let restrict = mob_restricted(t, max_h);
    land_pos_in_direction(t, min_h, max_h, v, dir, restrict)
}

/// `AirAndWaterRandomPos.generateRandomPos`: a direction within the angle, moved up out of
/// solid blocks, without malus.
#[allow(clippy::too_many_arguments)]
pub fn air_and_water_candidate(
    t: &mut Tick<'_, '_>,
    h: i32,
    v: i32,
    flying_height: i32,
    x_dir: f64,
    z_dir: f64,
    max_radians: f64,
    restrict: bool,
) -> Option<BlockPos> {
    let dir = direction_within_radians(
        t,
        0.0,
        f64::from(h),
        v,
        flying_height,
        x_dir,
        z_dir,
        max_radians,
    )?;
    let pos = toward_direction(t, f64::from(h), dir);
    if !t.level.in_height(pos.y) || is_restricted(t, restrict, pos) {
        return None;
    }
    let pos = move_up_out_of_solid(pos, t.level.max_y(), |p| is_solid(t, p));
    (!has_malus(t, pos)).then_some(pos)
}

/// `AirAndWaterRandomPos.getPos(mob, h, v, flyingHeight, xDir, zDir, maxRadians)`.
#[allow(clippy::too_many_arguments)]
pub fn air_and_water_pos(
    t: &mut Tick<'_, '_>,
    h: i32,
    v: i32,
    flying_height: i32,
    x_dir: f64,
    z_dir: f64,
    max_radians: f64,
) -> Option<Vec3> {
    let restrict = mob_restricted(t, f64::from(h));
    generate(
        t,
        |t| air_and_water_candidate(t, h, v, flying_height, x_dir, z_dir, max_radians, restrict),
        walk_value,
    )
}

/// `AirRandomPos.getPosTowards(mob, h, v, flyingHeight, towards, maxRadians)`: like
/// [`air_and_water_pos`] towards a point, never in water.
pub fn air_pos_towards(
    t: &mut Tick<'_, '_>,
    h: i32,
    v: i32,
    flying_height: i32,
    towards: Vec3,
    max_radians: f64,
) -> Option<Vec3> {
    let p = t.pos();
    let (dx, dz) = (towards.x - p.x, towards.z - p.z);
    let restrict = mob_restricted(t, f64::from(h));
    generate(
        t,
        |t| {
            let pos =
                air_and_water_candidate(t, h, v, flying_height, dx, dz, max_radians, restrict)?;
            (!is_water(t, pos)).then_some(pos)
        },
        walk_value,
    )
}

/// `WaterAvoidingRandomFlyingGoal.getPosition`: a hover position towards the view direction,
/// else an air or water one.
pub fn water_avoiding_flying_pos(t: &mut Tick<'_, '_>) -> Option<Vec3> {
    // `getViewVector(0)`: the pitch and head yaw of the tick start, unchanged until the goals.
    let rot = t.core().rot();
    let dir = view_vector(rot.x_rot, rot.y_head_rot);
    let half = std::f32::consts::FRAC_PI_2;
    hover_pos(t, 8, 7, dir.x, dir.z, half, 3, 1)
        .or_else(|| air_and_water_pos(t, 8, 4, -2, dir.x, dir.z, f64::from(half)))
}

/// `HoverRandomPos.getPos(mob, h, v, xDir, zDir, maxRadians, hoverMaxHeight,
/// hoverMinHeight)`: a land position lifted 1 to `max_height` blocks above the ground.
#[allow(clippy::too_many_arguments)]
pub fn hover_pos(
    t: &mut Tick<'_, '_>,
    h: i32,
    v: i32,
    x_dir: f64,
    z_dir: f64,
    max_radians: f32,
    max_height: i32,
    min_height: i32,
) -> Option<Vec3> {
    let restrict = mob_restricted(t, f64::from(h));
    generate(
        t,
        |t| {
            let dir = direction_within_radians(
                t,
                0.0,
                f64::from(h),
                v,
                0,
                x_dir,
                z_dir,
                f64::from(max_radians),
            )?;
            let pos = land_toward(t, f64::from(h), restrict, dir)?;
            let above = t.mob().random.next_int_bounded(max_height - min_height + 1) + min_height;
            let pos = move_up_to_above_solid(pos, above, t.level.max_y(), |p| is_solid(t, p));
            (!is_water(t, pos) && !has_malus(t, pos)).then_some(pos)
        },
        walk_value,
    )
}

/// `BehaviorUtils.getRandomSwimmablePos(mob, h, v)`: a default random position, retried up
/// to 10 times until it is pathfindable for swimming.
pub fn swimmable_pos(t: &mut Tick<'_, '_>, h: i32, v: i32) -> Option<Vec3> {
    let swimmable = |t: &Tick<'_, '_>, p: Vec3| {
        super::path::is_pathfindable(
            t.level.block(BlockPos::containing(p.x, p.y, p.z)),
            super::path::Computation::Water,
        )
    };
    let mut target = default_pos(t, h, v);
    let mut count = 0;
    while let Some(p) = target
        && !swimmable(t, p)
    {
        if count >= 10 {
            break;
        }
        count += 1;
        target = default_pos(t, h, v);
    }
    target
}

/// `Entity.getViewVector(0)` (`calculateViewVector(xRot, yRot)`).
pub fn view_vector(x_rot: f32, y_rot: f32) -> Vec3 {
    let x = x_rot * (std::f32::consts::PI / 180.0);
    let y = -y_rot * (std::f32::consts::PI / 180.0);
    let (y_cos, y_sin) = (
        vanillite_base::math::cos(f64::from(y)),
        vanillite_base::math::sin(f64::from(y)),
    );
    let (x_cos, x_sin) = (
        vanillite_base::math::cos(f64::from(x)),
        vanillite_base::math::sin(f64::from(x)),
    );
    Vec3::new(
        f64::from(y_sin * x_cos),
        f64::from(-x_sin),
        f64::from(y_cos * x_cos),
    )
}

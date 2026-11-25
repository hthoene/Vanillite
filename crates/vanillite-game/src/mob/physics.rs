//! Entity movement against blocks: the collision boxes around a box (`BlockCollisions`),
//! `Shapes.collide`, `Entity.collide` with stepping up, and block ray casts (`Level.clip` with
//! `ClipContext.Block.COLLIDER`).

use vanillite_base::math::{floor, frac};
use vanillite_base::{Aabb, BlockPos, Vec3};
use vanillite_data::block::{BlockState, flags, ids};

use crate::level::Level;

/// `Shapes.EPSILON`.
const EPSILON: f64 = 1.0e-7;

/// Read access to the blocks of a world (the level, or a test grid).
pub trait Blocks {
    /// `getBlockState(pos)`.
    fn state(&self, pos: BlockPos) -> BlockState;
}

impl Blocks for Level {
    #[inline]
    fn state(&self, pos: BlockPos) -> BlockState {
        self.block(pos)
    }
}

/// Appends the collision boxes (world coordinates) of the blocks around `area`, like
/// `BlockCollisions`: the cells the box touches plus a ring around it, where ring cells only
/// count with a large collision shape (fences, walls) and edge cells only for moving pistons.
pub fn block_boxes(blocks: &dyn Blocks, area: &Aabb, out: &mut Vec<Aabb>) {
    let min_x = floor(area.min_x - EPSILON) - 1;
    let max_x = floor(area.max_x + EPSILON) + 1;
    let min_y = floor(area.min_y - EPSILON) - 1;
    let max_y = floor(area.max_y + EPSILON) + 1;
    let min_z = floor(area.min_z - EPSILON) - 1;
    let max_z = floor(area.max_z + EPSILON) + 1;
    for x in min_x..=max_x {
        for z in min_z..=max_z {
            for y in min_y..=max_y {
                let edges = i32::from(x == min_x || x == max_x)
                    + i32::from(y == min_y || y == max_y)
                    + i32::from(z == min_z || z == max_z);
                if edges == 3 {
                    continue;
                }
                let pos = BlockPos::new(x, y, z);
                let state = blocks.state(pos);
                if state.is_air() {
                    continue;
                }
                if edges == 1 && !state.has(flags::LARGE_COLLISION_SHAPE) {
                    continue;
                }
                if edges == 2 && state.block() != ids::MOVING_PISTON {
                    continue;
                }
                let (ox, oy, oz) = (f64::from(x), f64::from(y), f64::from(z));
                for b in state.collision_shape() {
                    let shape = Aabb {
                        min_x: ox + b[0],
                        min_y: oy + b[1],
                        min_z: oz + b[2],
                        max_x: ox + b[3],
                        max_y: oy + b[4],
                        max_z: oz + b[5],
                    };
                    if shape.intersects(area) {
                        out.push(shape);
                    }
                }
            }
        }
    }
}

fn axis_min(b: &Aabb, axis: usize) -> f64 {
    match axis {
        0 => b.min_x,
        1 => b.min_y,
        _ => b.min_z,
    }
}

fn axis_max(b: &Aabb, axis: usize) -> f64 {
    match axis {
        0 => b.max_x,
        1 => b.max_y,
        _ => b.max_z,
    }
}

/// `Shapes.collide(axis, aabb, shapes, distance)`: how far `aabb` moves along `axis` (0 = X,
/// 1 = Y, 2 = Z) before it touches one of `boxes`.
pub fn collide_axis(boxes: &[Aabb], aabb: &Aabb, axis: usize, delta: f64) -> f64 {
    if delta.abs() < EPSILON {
        return 0.0;
    }
    let mut delta = delta;
    for b in boxes {
        let overlaps = (0..3).filter(|&a| a != axis).all(|a| {
            axis_max(b, a) - EPSILON > axis_min(aabb, a)
                && axis_min(b, a) + EPSILON < axis_max(aabb, a)
        });
        if !overlaps {
            continue;
        }
        if delta > 0.0 && axis_min(b, axis) >= axis_max(aabb, axis) - EPSILON {
            delta = delta.min(axis_min(b, axis) - axis_max(aabb, axis));
        } else if delta < 0.0 && axis_max(b, axis) <= axis_min(aabb, axis) + EPSILON {
            delta = delta.max(axis_max(b, axis) - axis_min(aabb, axis));
        }
        if delta.abs() < EPSILON {
            return 0.0;
        }
    }
    delta
}

fn offset_axis(aabb: Aabb, axis: usize, d: f64) -> Aabb {
    match axis {
        0 => aabb.offset(d, 0.0, 0.0),
        1 => aabb.offset(0.0, d, 0.0),
        _ => aabb.offset(0.0, 0.0, d),
    }
}

/// `Entity.collideWithShapes`: Y first, then the larger of X and Z (`Direction.axisStepOrder`).
pub fn collide_with_shapes(movement: Vec3, aabb: Aabb, boxes: &[Aabb]) -> Vec3 {
    if boxes.is_empty() {
        return movement;
    }
    let order = if movement.x.abs() < movement.z.abs() {
        [1, 2, 0]
    } else {
        [1, 0, 2]
    };
    let wanted = [movement.x, movement.y, movement.z];
    let mut out = [0.0; 3];
    let mut moved = aabb;
    for axis in order {
        let d = wanted[axis];
        if d != 0.0 {
            let c = collide_axis(boxes, &moved, axis, d);
            out[axis] = c;
            moved = offset_axis(moved, axis, c);
        }
    }
    Vec3::new(out[0], out[1], out[2])
}

/// `Entity.collide`: the movement left after the blocks in the way, stepping up to
/// `max_up_step` blocks when that gets further horizontally. `scratch` is reused between
/// calls.
pub fn collide(
    blocks: &dyn Blocks,
    aabb: Aabb,
    movement: Vec3,
    max_up_step: f32,
    on_ground: bool,
    scratch: &mut Vec<Aabb>,
) -> Vec3 {
    let step = if movement.length_sqr() == 0.0 {
        movement
    } else {
        scratch.clear();
        let area = aabb.expand_towards(movement.x, movement.y, movement.z);
        block_boxes(blocks, &area, scratch);
        collide_with_shapes(movement, aabb, scratch)
    };
    let x_collision = movement.x != step.x;
    let y_collision = movement.y != step.y;
    let z_collision = movement.z != step.z;
    let grounded_after = y_collision && movement.y < 0.0;
    if max_up_step > 0.0 && (grounded_after || on_ground) && (x_collision || z_collision) {
        let grounded = if grounded_after {
            aabb.offset(0.0, step.y, 0.0)
        } else {
            aabb
        };
        let mut area = grounded.expand_towards(movement.x, f64::from(max_up_step), movement.z);
        if !grounded_after {
            area = area.expand_towards(0.0, -1.0e-5, 0.0);
        }
        scratch.clear();
        block_boxes(blocks, &area, scratch);
        // `(float)movementStep.y`.
        #[allow(clippy::cast_possible_truncation)]
        let skip = step.y as f32;
        let mut candidates: Vec<f32> = Vec::with_capacity(4);
        for b in scratch.iter() {
            for coord in [b.min_y, b.max_y] {
                #[allow(clippy::cast_possible_truncation)]
                let relative = (coord - grounded.min_y) as f32;
                if relative < 0.0 || relative == skip || relative > max_up_step {
                    continue;
                }
                if !candidates.contains(&relative) {
                    candidates.push(relative);
                }
            }
        }
        candidates.sort_by(f32::total_cmp);
        for height in candidates {
            let try_step = collide_with_shapes(
                Vec3::new(movement.x, f64::from(height), movement.z),
                grounded,
                scratch,
            );
            let horizontal = |v: Vec3| v.x * v.x + v.z * v.z;
            if horizontal(try_step) > horizontal(step) {
                let to_ground = aabb.min_y - grounded.min_y;
                return Vec3::new(try_step.x, try_step.y - to_ground, try_step.z);
            }
        }
    }
    step
}

/// `noCollision(entity, box)` against blocks only.
pub fn no_collision(blocks: &dyn Blocks, aabb: &Aabb, scratch: &mut Vec<Aabb>) -> bool {
    scratch.clear();
    block_boxes(blocks, aabb, scratch);
    // `block_boxes` keeps boxes that intersect; `noCollision` ignores touching faces.
    !scratch.iter().any(|b| {
        b.max_x - EPSILON > aabb.min_x
            && b.min_x + EPSILON < aabb.max_x
            && b.max_y - EPSILON > aabb.min_y
            && b.min_y + EPSILON < aabb.max_y
            && b.max_z - EPSILON > aabb.min_z
            && b.min_z + EPSILON < aabb.max_z
    })
}

/// `containsAnyLiquid(box)`.
pub fn contains_any_liquid(blocks: &dyn Blocks, aabb: &Aabb) -> bool {
    let (x0, y0, z0) = (floor(aabb.min_x), floor(aabb.min_y), floor(aabb.min_z));
    let (x1, y1, z1) = (
        vanillite_base::math::ceil(aabb.max_x),
        vanillite_base::math::ceil(aabb.max_y),
        vanillite_base::math::ceil(aabb.max_z),
    );
    for x in x0..x1 {
        for y in y0..y1 {
            for z in z0..z1 {
                if !blocks
                    .state(BlockPos::new(x, y, z))
                    .fluid_state()
                    .is_empty()
                {
                    return true;
                }
            }
        }
    }
    false
}

/// Where a segment from `from` to `to` enters `b`, as the fraction of the segment.
fn ray_box(from: Vec3, to: Vec3, b: &Aabb) -> Option<f64> {
    let d = [to.x - from.x, to.y - from.y, to.z - from.z];
    let o = [from.x, from.y, from.z];
    let lo = [b.min_x, b.min_y, b.min_z];
    let hi = [b.max_x, b.max_y, b.max_z];
    let mut t0: f64 = 0.0;
    let mut t1: f64 = 1.0;
    for a in 0..3 {
        if d[a].abs() < 1.0e-12 {
            if o[a] < lo[a] || o[a] > hi[a] {
                return None;
            }
        } else {
            let inv = 1.0 / d[a];
            let (mut near, mut far) = ((lo[a] - o[a]) * inv, (hi[a] - o[a]) * inv);
            if near > far {
                std::mem::swap(&mut near, &mut far);
            }
            t0 = t0.max(near);
            t1 = t1.min(far);
            if t0 > t1 {
                return None;
            }
        }
    }
    Some(t0)
}

/// `Level.clip` with `ClipContext.Block.COLLIDER` and `Fluid.NONE`: the first block whose
/// collision shape the segment hits (`BlockGetter.traverseBlocks`), with the hit point.
pub fn clip(blocks: &dyn Blocks, from: Vec3, to: Vec3) -> Option<(BlockPos, Vec3)> {
    clip_with(blocks, from, to, false)
}

/// `Level.clip` with `ClipContext.Block.COLLIDER` and `Fluid.ANY`: also stops at fluid
/// surfaces (a fluid's shape is its height in the block).
pub fn clip_fluids(blocks: &dyn Blocks, from: Vec3, to: Vec3) -> Option<(BlockPos, Vec3)> {
    clip_with(blocks, from, to, true)
}

fn clip_with(blocks: &dyn Blocks, from: Vec3, to: Vec3, fluids: bool) -> Option<(BlockPos, Vec3)> {
    if from == to {
        return None;
    }
    let lerp = |t: f64, a: f64, b: f64| a + t * (b - a);
    let to_x = lerp(-1.0e-7, to.x, from.x);
    let to_y = lerp(-1.0e-7, to.y, from.y);
    let to_z = lerp(-1.0e-7, to.z, from.z);
    let from_x = lerp(-1.0e-7, from.x, to.x);
    let from_y = lerp(-1.0e-7, from.y, to.y);
    let from_z = lerp(-1.0e-7, from.z, to.z);
    let mut bx = floor(from_x);
    let mut by = floor(from_y);
    let mut bz = floor(from_z);
    let hit = |pos: BlockPos| -> Option<(BlockPos, Vec3)> {
        let state = blocks.state(pos);
        let (ox, oy, oz) = (f64::from(pos.x), f64::from(pos.y), f64::from(pos.z));
        let mut best: Option<f64> = None;
        let fluid = state.fluid_state();
        if fluids && !fluid.is_empty() {
            let above = blocks
                .state(BlockPos::new(pos.x, pos.y + 1, pos.z))
                .fluid_state();
            let height = if !above.is_empty() && above.info().fluid == fluid.info().fluid {
                1.0
            } else {
                f64::from(fluid.info().own_height)
            };
            let shape = Aabb::new(ox, oy, oz, ox + 1.0, oy + height, oz + 1.0);
            best = ray_box(from, to, &shape);
        }
        for b in state.collision_shape() {
            let shape = Aabb {
                min_x: ox + b[0],
                min_y: oy + b[1],
                min_z: oz + b[2],
                max_x: ox + b[3],
                max_y: oy + b[4],
                max_z: oz + b[5],
            };
            if let Some(t) = ray_box(from, to, &shape) {
                best = Some(best.map_or(t, |b: f64| b.min(t)));
            }
        }
        best.map(|t| {
            (
                pos,
                Vec3::new(
                    from.x + (to.x - from.x) * t,
                    from.y + (to.y - from.y) * t,
                    from.z + (to.z - from.z) * t,
                ),
            )
        })
    };
    if let Some(h) = hit(BlockPos::new(bx, by, bz)) {
        return Some(h);
    }
    let (dx, dy, dz) = (to_x - from_x, to_y - from_y, to_z - from_z);
    let sign = |d: f64| -> i32 {
        if d > 0.0 {
            1
        } else if d < 0.0 {
            -1
        } else {
            0
        }
    };
    let (sx, sy, sz) = (sign(dx), sign(dy), sign(dz));
    let delta = |s: i32, d: f64| {
        if s == 0 { f64::MAX } else { f64::from(s) / d }
    };
    let (tdx, tdy, tdz) = (delta(sx, dx), delta(sy, dy), delta(sz, dz));
    let start = |s: i32, td: f64, f: f64| td * if s > 0 { 1.0 - frac(f) } else { frac(f) };
    let mut tx = start(sx, tdx, from_x);
    let mut ty = start(sy, tdy, from_y);
    let mut tz = start(sz, tdz, from_z);
    while tx <= 1.0 || ty <= 1.0 || tz <= 1.0 {
        if tx < ty {
            if tx < tz {
                bx += sx;
                tx += tdx;
            } else {
                bz += sz;
                tz += tdz;
            }
        } else if ty < tz {
            by += sy;
            ty += tdy;
        } else {
            bz += sz;
            tz += tdz;
        }
        if let Some(h) = hit(BlockPos::new(bx, by, bz)) {
            return Some(h);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Floor;
    impl Blocks for Floor {
        fn state(&self, pos: BlockPos) -> BlockState {
            if pos.y < 64 || (pos.y == 64 && pos.x == 2) {
                vanillite_data::block::ids::STONE.default_state()
            } else {
                BlockState::AIR
            }
        }
    }

    fn mob_box(pos: Vec3) -> Aabb {
        Aabb::new(
            pos.x - 0.3,
            pos.y,
            pos.z - 0.3,
            pos.x + 0.3,
            pos.y + 1.8,
            pos.z + 0.3,
        )
    }

    #[test]
    fn lands_on_the_floor() {
        let mut scratch = Vec::new();
        let moved = collide(
            &Floor,
            mob_box(Vec3::new(0.5, 64.5, 0.5)),
            Vec3::new(0.0, -1.0, 0.0),
            0.6,
            false,
            &mut scratch,
        );
        assert!((moved.y + 0.5).abs() < 1.0e-9);
    }

    #[test]
    fn steps_up_a_block_only_with_jump_height() {
        let mut scratch = Vec::new();
        // A full block is higher than the 0.6 step height: the move stops at the wall.
        let moved = collide(
            &Floor,
            mob_box(Vec3::new(1.5, 64.0, 0.5)),
            Vec3::new(0.3, 0.0, 0.0),
            0.6,
            true,
            &mut scratch,
        );
        assert!((moved.x - 0.2).abs() < 1.0e-9);
        assert_eq!(moved.y, 0.0);
    }

    #[test]
    fn clip_hits_the_wall() {
        let hit = clip(&Floor, Vec3::new(0.5, 64.5, 0.5), Vec3::new(4.5, 64.5, 0.5));
        assert_eq!(hit.map(|h| h.0), Some(BlockPos::new(2, 64, 0)));
        assert!(clip(&Floor, Vec3::new(0.5, 65.5, 0.5), Vec3::new(4.5, 65.5, 0.5)).is_none());
    }
}

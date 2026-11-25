//! Item entities (`ItemEntity`): movement with block collisions, merging, despawning and
//! pickup.

use vanillite_base::math::floor;
use vanillite_base::{Aabb, BlockPos, Vec3};
use vanillite_data::block::{BlockState, flags as block_flags};

use crate::item::{self, ItemStack};

/// `INFINITE_PICKUP_DELAY`.
pub const NEVER_PICK_UP: i16 = 32767;
/// `INFINITE_LIFETIME`.
pub const UNLIMITED_LIFETIME: i16 = -32768;

/// The kind data of an item entity.
#[derive(Clone, Debug)]
pub struct ItemData {
    /// The stack (`DATA_ITEM`).
    pub stack: ItemStack,
    /// Ticks lived.
    pub age: i16,
    /// Ticks until it can be picked up.
    pub pickup_delay: i16,
    /// Who threw it.
    pub thrower: Option<u128>,
    /// Only this player may pick it up (`target`).
    pub target: Option<u128>,
    /// `tickCount`.
    pub tick_count: u32,
    /// `health`: explosions and fire wear it down from 5.
    pub health: i16,
}

impl ItemData {
    /// A new item entity's data.
    pub fn new(stack: ItemStack, pickup_delay: i16, thrower: Option<u128>) -> ItemData {
        ItemData {
            stack,
            age: 0,
            pickup_delay,
            thrower,
            target: None,
            tick_count: 0,
            health: 5,
        }
    }

    /// `isMergable`.
    pub fn is_mergable(&self, lifetime: i16) -> bool {
        !self.stack.is_empty()
            && self.pickup_delay != NEVER_PICK_UP
            && self.age != UNLIMITED_LIFETIME
            && self.age < lifetime
            && self.stack.count < item::max_stack_size(&self.stack)
    }
}

/// `ItemEntity.areMergable`.
pub fn are_mergable(a: &ItemStack, b: &ItemStack) -> bool {
    b.count.saturating_add(a.count) <= item::max_stack_size(b) && a.same_item_same_components(b)
}

/// `ItemEntity.merge(toStack, fromStack, 64)`: the new `to` stack; `from` shrinks.
pub fn merge_stacks(to: &ItemStack, from: &mut ItemStack) -> ItemStack {
    merge_stacks_max(to, from, 64)
}

/// `ItemEntity.merge(toStack, fromStack, maxCount)`.
pub fn merge_stacks_max(to: &ItemStack, from: &mut ItemStack, max: i32) -> ItemStack {
    let delta = (item::max_stack_size(to).min(max) - to.count).min(from.count);
    let new_to = to.with_count(to.count + delta);
    from.shrink(delta);
    new_to
}

/// The collision boxes of the blocks around `area` (`Level.getBlockCollisions`, without the
/// world border and entity shapes).
fn block_boxes(area: &Aabb, block: &dyn Fn(BlockPos) -> BlockState, out: &mut Vec<Aabb>) {
    out.clear();
    let x0 = floor(area.min_x - 1.0e-7) - 1;
    let x1 = floor(area.max_x + 1.0e-7) + 1;
    let y0 = floor(area.min_y - 1.0e-7) - 1;
    let y1 = floor(area.max_y + 1.0e-7) + 1;
    let z0 = floor(area.min_z - 1.0e-7) - 1;
    let z1 = floor(area.max_z + 1.0e-7) + 1;
    for x in x0..=x1 {
        for z in z0..=z1 {
            for y in y0..=y1 {
                let pos = BlockPos::new(x, y, z);
                let state = block(pos);
                if state.is_air() {
                    continue;
                }
                let (dx, dy, dz) = (f64::from(x), f64::from(y), f64::from(z));
                for b in state.collision_shape() {
                    let shape = Aabb::new(
                        b[0] + dx,
                        b[1] + dy,
                        b[2] + dz,
                        b[3] + dx,
                        b[4] + dy,
                        b[5] + dz,
                    );
                    if shape.intersects(&area.inflate(1.0e-7, 1.0e-7, 1.0e-7))
                        || touches(&shape, area)
                    {
                        out.push(shape);
                    }
                }
            }
        }
    }
}

fn touches(a: &Aabb, b: &Aabb) -> bool {
    a.min_x <= b.max_x
        && a.max_x >= b.min_x
        && a.min_y <= b.max_y
        && a.max_y >= b.min_y
        && a.min_z <= b.max_z
        && a.max_z >= b.min_z
}

const EPS: f64 = 1.0e-7;

/// `Shapes.collide` along one axis (0 = x, 1 = y, 2 = z).
fn collide_axis(axis: usize, b: &Aabb, shapes: &[Aabb], mut delta: f64) -> f64 {
    let lo = |a: &Aabb, i: usize| [a.min_x, a.min_y, a.min_z][i];
    let hi = |a: &Aabb, i: usize| [a.max_x, a.max_y, a.max_z][i];
    for s in shapes {
        if delta.abs() < EPS {
            return 0.0;
        }
        let overlaps = (0..3)
            .filter(|&i| i != axis)
            .all(|i| hi(s, i) - EPS > lo(b, i) && lo(s, i) + EPS < hi(b, i));
        if !overlaps {
            continue;
        }
        if delta > 0.0 && lo(s, axis) >= hi(b, axis) - EPS {
            delta = delta.min(lo(s, axis) - hi(b, axis));
        } else if delta < 0.0 && hi(s, axis) <= lo(b, axis) + EPS {
            delta = delta.max(hi(s, axis) - lo(b, axis));
        }
    }
    delta
}

/// The result of `Entity.move`.
#[derive(Clone, Copy, Debug)]
pub struct Moved {
    /// The new position.
    pub pos: Vec3,
    /// The movement after collisions.
    pub movement: Vec3,
    /// Collided horizontally on x / z, vertically.
    pub hit_x: bool,
    pub hit_y: bool,
    pub hit_z: bool,
}

/// `Entity.collide` + `move` for an entity of `half_width` and `height` without step-up.
pub fn move_with_collisions(
    pos: Vec3,
    half_width: f64,
    height: f64,
    movement: Vec3,
    block: &dyn Fn(BlockPos) -> BlockState,
    scratch: &mut Vec<Aabb>,
) -> Moved {
    let mut b = Aabb::new(
        pos.x - half_width,
        pos.y,
        pos.z - half_width,
        pos.x + half_width,
        pos.y + height,
        pos.z + half_width,
    );
    let (mut x, mut y, mut z) = (movement.x, movement.y, movement.z);
    if x * x + y * y + z * z > 0.0 {
        block_boxes(&b.expand_towards(x, y, z), block, scratch);
        if y != 0.0 {
            y = collide_axis(1, &b, scratch, y);
            if y != 0.0 {
                b = b.offset(0.0, y, 0.0);
            }
        }
        let z_first = x.abs() < z.abs();
        if z_first && z != 0.0 {
            z = collide_axis(2, &b, scratch, z);
            if z != 0.0 {
                b = b.offset(0.0, 0.0, z);
            }
        }
        if x != 0.0 {
            x = collide_axis(0, &b, scratch, x);
            if !z_first && x != 0.0 {
                b = b.offset(x, 0.0, 0.0);
            }
        }
        if !z_first && z != 0.0 {
            z = collide_axis(2, &b, scratch, z);
        }
    }
    Moved {
        pos: Vec3::new(pos.x + x, pos.y + y, pos.z + z),
        movement: Vec3::new(x, y, z),
        hit_x: x != movement.x,
        hit_y: y != movement.y,
        hit_z: z != movement.z,
    }
}

/// Whether the box collides with any block (`noCollision` negated).
pub fn collides(b: &Aabb, block: &dyn Fn(BlockPos) -> BlockState, scratch: &mut Vec<Aabb>) -> bool {
    block_boxes(b, block, scratch);
    scratch.iter().any(|s| s.intersects(b))
}

/// What one tick of an item entity did.
#[derive(Clone, Copy, Debug, Default)]
pub struct Ticked {
    /// New position.
    pub pos: Vec3,
    /// New velocity.
    pub velocity: Vec3,
    /// On ground after the move.
    pub on_ground: bool,
    /// The velocity changed noticeably (`needsSync`).
    pub needs_sync: bool,
    /// The entity crossed a block boundary (merges every 2 ticks instead of 40).
    pub moved_block: bool,
    /// The entity is gone (empty stack, lifetime over or below the world).
    pub discard: bool,
}

/// Inputs of one item entity tick.
#[derive(Clone, Copy, Debug)]
pub struct TickInput {
    pub pos: Vec3,
    pub velocity: Vec3,
    pub on_ground: bool,
    pub network_id: i32,
    /// Lowest block y of the world.
    pub min_y: i32,
    /// Ticks until an item despawns (`LIFETIME`, 6000 in Vanilla).
    pub lifetime: i16,
}

/// `ItemEntity.tick` without merging (the caller merges with the returned rate).
pub fn tick(
    data: &mut ItemData,
    input: TickInput,
    block: &dyn Fn(BlockPos) -> BlockState,
    random_float: &mut dyn FnMut() -> f32,
    scratch: &mut Vec<Aabb>,
) -> Ticked {
    let mut out = Ticked {
        pos: input.pos,
        velocity: input.velocity,
        on_ground: input.on_ground,
        ..Ticked::default()
    };
    if data.stack.is_empty() || input.pos.y < f64::from(input.min_y) - 64.0 {
        out.discard = true;
        return out;
    }
    data.tick_count = data.tick_count.wrapping_add(1);
    if data.pickup_delay > 0 && data.pickup_delay != NEVER_PICK_UP {
        data.pickup_delay -= 1;
    }
    let old = input.pos;
    let old_velocity = input.velocity;
    let mut v = input.velocity;
    let feet = block(BlockPos::new(floor(old.x), floor(old.y), floor(old.z)));
    let fluid = feet.fluid_state();
    let fluid_id = if fluid.is_empty() {
        0
    } else {
        fluid.info().fluid
    };
    // `minecraft:fluid` ids: 1 flowing water, 2 water, 3 flowing lava, 4 lava.
    if fluid_id == 1 || fluid_id == 2 {
        // `setUnderwaterMovement`.
        v = Vec3::new(
            v.x * 0.99,
            v.y + if v.y < 0.06 { 5.0e-4 } else { 0.0 },
            v.z * 0.99,
        );
    } else if fluid_id == 3 || fluid_id == 4 {
        // `setUnderLavaMovement`.
        v = Vec3::new(
            v.x * 0.95,
            v.y + if v.y < 0.06 { 5.0e-4 } else { 0.0 },
            v.z * 0.95,
        );
    } else {
        v = Vec3::new(v.x, v.y - 0.04, v.z);
    }
    let half = 0.125;
    let height = 0.25;
    let body = Aabb::new(
        old.x - half,
        old.y,
        old.z - half,
        old.x + half,
        old.y + height,
        old.z + half,
    );
    let no_physics = collides(&body.inflate(-1.0e-7, -1.0e-7, -1.0e-7), block, scratch);
    if no_physics {
        v = move_towards_closest_space(
            old,
            (body.min_y + body.max_y) / 2.0,
            v,
            block,
            random_float,
        );
    }
    let horizontal = v.x * v.x + v.z * v.z;
    let skip_move = input.on_ground
        && horizontal <= 1.0e-5
        && !data
            .tick_count
            .wrapping_add(u32::from_ne_bytes(input.network_id.to_ne_bytes()))
            .is_multiple_of(4);
    if !skip_move {
        let moved = if no_physics {
            Moved {
                pos: Vec3::new(old.x + v.x, old.y + v.y, old.z + v.z),
                movement: v,
                hit_x: false,
                hit_y: false,
                hit_z: false,
            }
        } else {
            move_with_collisions(old, half, height, v, block, scratch)
        };
        out.pos = moved.pos;
        out.on_ground = moved.hit_y && v.y < 0.0;
        if moved.hit_x {
            v.x = 0.0;
        }
        if moved.hit_z {
            v.z = 0.0;
        }
        if moved.hit_y {
            v.y = 0.0;
        }
        let air_drag = 0.98;
        let mut ground = air_drag;
        if out.on_ground {
            let below = block(BlockPos::new(
                floor(out.pos.x),
                floor(out.pos.y - 0.999_999),
                floor(out.pos.z),
            ));
            ground = air_drag * f64::from(below.block().info().friction);
        }
        v = Vec3::new(v.x * ground, v.y * air_drag, v.z * ground);
        if out.on_ground && v.y < 0.0 {
            v.y *= -0.5;
        }
    }
    out.velocity = v;
    out.moved_block = floor(old.x) != floor(out.pos.x)
        || floor(old.y) != floor(out.pos.y)
        || floor(old.z) != floor(out.pos.z);
    if data.age != UNLIMITED_LIFETIME {
        data.age = data.age.saturating_add(1);
    }
    let dx = v.x - old_velocity.x;
    let dy = v.y - old_velocity.y;
    let dz = v.z - old_velocity.z;
    out.needs_sync = dx * dx + dy * dy + dz * dz > 0.01;
    if data.age >= input.lifetime && data.age != UNLIMITED_LIFETIME {
        out.discard = true;
    }
    out
}

/// `Entity.moveTowardsClosestSpace`: pushes an entity stuck in a block out of it.
pub(crate) fn move_towards_closest_space(
    pos: Vec3,
    y: f64,
    velocity: Vec3,
    block: &dyn Fn(BlockPos) -> BlockState,
    random_float: &mut dyn FnMut() -> f32,
) -> Vec3 {
    let base = BlockPos::new(floor(pos.x), floor(y), floor(pos.z));
    let delta = [
        pos.x - f64::from(base.x),
        y - f64::from(base.y),
        pos.z - f64::from(base.z),
    ];
    // North, south, west, east, up: (axis, positive).
    let dirs: [(usize, bool, [i32; 3]); 5] = [
        (2, false, [0, 0, -1]),
        (2, true, [0, 0, 1]),
        (0, false, [-1, 0, 0]),
        (0, true, [1, 0, 0]),
        (1, true, [0, 1, 0]),
    ];
    let mut best = (1, true);
    let mut closest = f64::MAX;
    for (axis, positive, [dx, dy, dz]) in dirs {
        let n = BlockPos::new(
            base.x.wrapping_add(dx),
            base.y.wrapping_add(dy),
            base.z.wrapping_add(dz),
        );
        if !block(n).has(block_flags::COLLISION_FULL_BLOCK) {
            let axis_delta = delta.get(axis).copied().unwrap_or(0.0);
            let distance = if positive {
                1.0 - axis_delta
            } else {
                axis_delta
            };
            if distance < closest {
                closest = distance;
                best = (axis, positive);
            }
        }
    }
    let speed = f64::from(random_float() * 0.2 + 0.1);
    let step = if best.1 { 1.0 } else { -1.0 };
    let v = Vec3::new(velocity.x * 0.75, velocity.y * 0.75, velocity.z * 0.75);
    match best.0 {
        0 => Vec3::new(step * speed, v.y, v.z),
        1 => Vec3::new(v.x, step * speed, v.z),
        _ => Vec3::new(v.x, v.y, step * speed),
    }
}

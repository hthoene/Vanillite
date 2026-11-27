//! The movement of minecarts as 26.3 does it by default (`OldMinecartBehavior`; the
//! `minecart_improvements` experiment is not supported): along the rails, off them, and
//! pushing other entities.

use vanillite_base::math::{atan2, cos, floor, sin, wrap_degrees_f};
use vanillite_base::{Aabb, BlockPos, Vec3};
use vanillite_data::block::{BlockState, ids as block_ids};
use vanillite_world::entity::{EntityFlags, EntityId, Rotation};
use vanillite_worldgen::behaviour::StateExt;

use super::{MinecartData, MinecartKind};
use crate::block::is_rail;
use crate::block::rail::RailShape;
use crate::entities::Entities;
use crate::level::Level;
use crate::level::signal::powered;
use crate::mob::{MobContext, WorldAction, physics};
use crate::survival::player::Env;

/// `AbstractMinecart.EXITS`: the two block offsets a rail shape connects, as (x, y, z).
pub fn exits(shape: RailShape) -> ([i32; 3], [i32; 3]) {
    const W: [i32; 3] = [-1, 0, 0];
    const E: [i32; 3] = [1, 0, 0];
    const N: [i32; 3] = [0, 0, -1];
    const S: [i32; 3] = [0, 0, 1];
    const W_BELOW: [i32; 3] = [-1, -1, 0];
    const E_BELOW: [i32; 3] = [1, -1, 0];
    const N_BELOW: [i32; 3] = [0, -1, -1];
    const S_BELOW: [i32; 3] = [0, -1, 1];
    match shape {
        RailShape::NorthSouth => (N, S),
        RailShape::EastWest => (W, E),
        RailShape::AscendingEast => (W_BELOW, E),
        RailShape::AscendingWest => (W, E_BELOW),
        RailShape::AscendingNorth => (N, S_BELOW),
        RailShape::AscendingSouth => (N_BELOW, S),
        RailShape::SouthEast => (S, E),
        RailShape::SouthWest => (S, W),
        RailShape::NorthWest => (N, W),
        RailShape::NorthEast => (N, E),
    }
}

/// `getMaxSpeed` (the furnace minecart is slower).
pub fn max_speed(m: &MinecartData) -> f64 {
    let base = if m.in_water { 0.2 } else { 0.4 };
    if m.kind == MinecartKind::Furnace {
        if m.in_water { base * 0.75 } else { base * 0.5 }
    } else {
        base
    }
}

/// `getDefaultGravity` / `getGravity`.
fn gravity(no_gravity: bool, m: &MinecartData) -> f64 {
    if no_gravity {
        0.0
    } else if m.in_water {
        0.005
    } else {
        0.04
    }
}

/// `Mth.equal(a, b)`: `|b - a| < 1.0E-5F`.
fn mth_equal(a: f64, b: f64) -> bool {
    (b - a).abs() < f64::from(1.0e-5_f32)
}

/// The minecart being ticked.
struct Cart<'a, 'e> {
    level: &'a Level,
    ents: &'a mut Entities,
    env: &'a mut Env<'e>,
    cx: &'a mut MobContext,
    id: EntityId,
}

impl Cart<'_, '_> {
    fn pos(&self) -> Vec3 {
        self.ents.store.get(self.id).map_or(Vec3::ZERO, |e| e.pos())
    }

    fn set_pos(&mut self, pos: Vec3) {
        if let Some(mut e) = self.ents.store.get_mut(self.id) {
            e.set_pos(pos);
        }
    }

    fn velocity(&self) -> Vec3 {
        self.ents
            .store
            .get(self.id)
            .map_or(Vec3::ZERO, |e| e.velocity())
    }

    fn set_velocity(&mut self, v: Vec3) {
        if let Some(mut e) = self.ents.store.get_mut(self.id) {
            e.set_velocity(v);
        }
    }

    fn data(&self) -> &MinecartData {
        self.ents.minecart(self.id).expect("ticked minecart")
    }

    fn data_mut(&mut self) -> &mut MinecartData {
        self.ents.minecart_mut(self.id).expect("ticked minecart")
    }

    fn exists(&self) -> bool {
        self.ents.store.contains(self.id) && !self.cx.discarded.contains(&self.id)
    }

    /// `isVehicle`.
    fn is_vehicle(&self) -> bool {
        !self.ents.riding.passengers_of(self.id).is_empty()
    }

    fn conductor(&self, pos: BlockPos) -> bool {
        Level::is_redstone_conductor(self.level.block(pos))
    }
}

/// `OldMinecartBehavior.tick` on the server.
pub(super) fn tick(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    cx: &mut MobContext,
    id: EntityId,
) {
    let Some(e) = ents.store.get(id) else {
        return;
    };
    let old = e.old_pos();
    let y_rot_o = e.rot().y_rot;
    let no_gravity = e.flags().contains(EntityFlags::NO_GRAVITY);
    let mut c = Cart {
        level,
        ents,
        env,
        cx,
        id,
    };
    // `applyGravity`.
    let g = gravity(no_gravity, c.data());
    if g != 0.0 {
        let v = c.velocity();
        c.set_velocity(v + Vec3::new(0.0, -g, 0.0));
    }
    let rail_pos = current_block_pos_or_rail_below(level, c.pos());
    let state = level.block(rail_pos);
    let on_rails = is_rail(state.block());
    c.data_mut().on_rails = on_rails;
    if on_rails {
        move_along_track(&mut c, rail_pos, state);
        if state.is(block_ids::ACTIVATOR_RAIL) && c.exists() {
            super::activate(c.ents, c.env, c.cx, id, powered(state));
        }
    } else {
        come_off_track(&mut c);
    }
    if !c.exists() {
        return;
    }
    apply_effects_from_blocks(&mut c);
    // The rotation follows the movement; a turn of 170° or more flips it.
    let pos = c.pos();
    let (x_diff, z_diff) = (old.x - pos.x, old.z - pos.z);
    let flipped = c.data().flipped;
    let mut y_rot = c.ents.store.get(id).map_or(0.0, |e| e.rot().y_rot);
    if x_diff * x_diff + z_diff * z_diff > 0.001 {
        #[allow(clippy::cast_possible_truncation)]
        let r = (atan2(z_diff, x_diff) * 180.0 / std::f64::consts::PI) as f32;
        y_rot = r;
        if flipped {
            y_rot += 180.0;
        }
    }
    let rot_diff = f64::from(wrap_degrees_f(y_rot - y_rot_o));
    if !(-170.0..170.0).contains(&rot_diff) {
        y_rot += 180.0;
        let m = c.data_mut();
        m.flipped = !m.flipped;
    }
    y_rot %= 360.0;
    if let Some(mut e) = c.ents.store.get_mut(id) {
        e.set_rot(Rotation {
            y_rot,
            x_rot: 0.0,
            y_head_rot: y_rot,
        });
    }
    push_and_pickup_entities(&mut c);
}

/// `getCurrentBlockPosOrRailBelow`.
fn current_block_pos_or_rail_below(level: &Level, pos: Vec3) -> BlockPos {
    let (x, mut y, z) = (floor(pos.x), floor(pos.y), floor(pos.z));
    if is_rail(level.block(BlockPos::new(x, y - 1, z)).block()) {
        y -= 1;
    }
    BlockPos::new(x, y, z)
}

/// `OldMinecartBehavior.moveAlongTrack`.
#[allow(clippy::too_many_lines)]
fn move_along_track(c: &mut Cart<'_, '_>, pos: BlockPos, state: BlockState) {
    c.data_mut().fall_distance = 0.0;
    let start = c.pos();
    let old_pos = get_pos(c.level, start);
    let mut y = f64::from(pos.y);
    let mut power_track = false;
    let mut halt_track = false;
    if state.is(block_ids::POWERED_RAIL) {
        power_track = powered(state);
        halt_track = !power_track;
    }
    let mut slide = 0.007_812_5;
    if c.data().in_water {
        slide *= 0.2;
    }
    let shape = RailShape::of(state);
    let v = c.velocity();
    match shape {
        RailShape::AscendingEast => {
            c.set_velocity(v + Vec3::new(-slide, 0.0, 0.0));
            y += 1.0;
        }
        RailShape::AscendingWest => {
            c.set_velocity(v + Vec3::new(slide, 0.0, 0.0));
            y += 1.0;
        }
        RailShape::AscendingNorth => {
            c.set_velocity(v + Vec3::new(0.0, 0.0, slide));
            y += 1.0;
        }
        RailShape::AscendingSouth => {
            c.set_velocity(v + Vec3::new(0.0, 0.0, -slide));
            y += 1.0;
        }
        _ => {}
    }
    let movement = c.velocity();
    let (exit0, exit1) = exits(shape);
    let mut xd = f64::from(exit1[0] - exit0[0]);
    let mut zd = f64::from(exit1[2] - exit0[2]);
    let length = (xd * xd + zd * zd).sqrt();
    let flip = movement.x * xd + movement.z * zd;
    if flip < 0.0 {
        xd = -xd;
        zd = -zd;
    }
    let pow = 2.0_f64.min((movement.x * movement.x + movement.z * movement.z).sqrt());
    c.set_velocity(Vec3::new(pow * xd / length, movement.y, pow * zd / length));
    // A player riding the minecart starts it with its movement keys.
    let rider = c.ents.riding.passengers_of(c.id).first().copied();
    let intent = rider.map_or(Vec3::ZERO, |r| move_intent(c.ents, r));
    if rider.is_some_and(|r| c.ents.player(r).is_some()) && intent.length_sqr() > 0.0 {
        let rider_movement = intent.normalize();
        let v = c.velocity();
        let own = v.x * v.x + v.z * v.z;
        if rider_movement.length_sqr() > 0.0 && own < 0.01 {
            c.set_velocity(v + Vec3::new(intent.x * 0.001, 0.0, intent.z * 0.001));
            halt_track = false;
        }
    }
    if halt_track {
        let v = c.velocity();
        let speed = (v.x * v.x + v.z * v.z).sqrt();
        if speed < 0.03 {
            c.set_velocity(Vec3::ZERO);
        } else {
            c.set_velocity(v.multiply(0.5, 0.0, 0.5));
        }
    }
    let (px, pz) = (f64::from(pos.x), f64::from(pos.z));
    let x0 = px + 0.5 + f64::from(exit0[0]) * 0.5;
    let z0 = pz + 0.5 + f64::from(exit0[2]) * 0.5;
    let x1 = px + 0.5 + f64::from(exit1[0]) * 0.5;
    let z1 = pz + 0.5 + f64::from(exit1[2]) * 0.5;
    let xd = x1 - x0;
    let zd = z1 - z0;
    let progress = if xd == 0.0 {
        start.z - pz
    } else if zd == 0.0 {
        start.x - px
    } else {
        let xx = start.x - x0;
        let zz = start.z - z0;
        (xx * xd + zz * zd) * 2.0
    };
    c.set_pos(Vec3::new(x0 + xd * progress, y, z0 + zd * progress));
    let scale = if c.is_vehicle() { 0.75 } else { 1.0 };
    let max = max_speed(c.data());
    let v = c.velocity();
    move_cart(
        c,
        Vec3::new(
            (scale * v.x).clamp(-max, max),
            0.0,
            (scale * v.z).clamp(-max, max),
        ),
    );
    if !c.exists() {
        return;
    }
    let p = c.pos();
    if exit0[1] != 0 && floor(p.x) - pos.x == exit0[0] && floor(p.z) - pos.z == exit0[2] {
        c.set_pos(Vec3::new(p.x, p.y + f64::from(exit0[1]), p.z));
    } else if exit1[1] != 0 && floor(p.x) - pos.x == exit1[0] && floor(p.z) - pos.z == exit1[2] {
        c.set_pos(Vec3::new(p.x, p.y + f64::from(exit1[1]), p.z));
    }
    let slowed = natural_slowdown(c, c.velocity());
    c.set_velocity(slowed);
    let new_pos = get_pos(c.level, c.pos());
    if let (Some(new_pos), Some(old_pos)) = (new_pos, old_pos) {
        let speed = (old_pos.y - new_pos.y) * 0.05;
        let v = c.velocity();
        let other = (v.x * v.x + v.z * v.z).sqrt();
        if other > 0.0 {
            let f = (other + speed) / other;
            c.set_velocity(v.multiply(f, 1.0, f));
        }
        let p = c.pos();
        c.set_pos(Vec3::new(p.x, new_pos.y, p.z));
    }
    let p = c.pos();
    let (xn, zn) = (floor(p.x), floor(p.z));
    if xn != pos.x || zn != pos.z {
        let v = c.velocity();
        let other = (v.x * v.x + v.z * v.z).sqrt();
        c.set_velocity(Vec3::new(
            other * f64::from(xn - pos.x),
            v.y,
            other * f64::from(zn - pos.z),
        ));
    }
    if power_track {
        let v = c.velocity();
        let speed = (v.x * v.x + v.z * v.z).sqrt();
        if speed > 0.01 {
            c.set_velocity(v + Vec3::new(v.x / speed * 0.06, 0.0, v.z / speed * 0.06));
        } else {
            let (mut dx, mut dz) = (v.x, v.z);
            match shape {
                RailShape::EastWest => {
                    if c.conductor(pos.west()) {
                        dx = 0.02;
                    } else if c.conductor(pos.east()) {
                        dx = -0.02;
                    }
                }
                RailShape::NorthSouth => {
                    if c.conductor(pos.north()) {
                        dz = 0.02;
                    } else if c.conductor(pos.south()) {
                        dz = -0.02;
                    }
                }
                _ => return,
            }
            c.set_velocity(Vec3::new(dx, v.y, dz));
        }
    }
}

trait BlockPosExt {
    fn west(self) -> BlockPos;
    fn east(self) -> BlockPos;
    fn north(self) -> BlockPos;
    fn south(self) -> BlockPos;
}

impl BlockPosExt for BlockPos {
    fn west(self) -> BlockPos {
        BlockPos::new(self.x - 1, self.y, self.z)
    }
    fn east(self) -> BlockPos {
        BlockPos::new(self.x + 1, self.y, self.z)
    }
    fn north(self) -> BlockPos {
        BlockPos::new(self.x, self.y, self.z - 1)
    }
    fn south(self) -> BlockPos {
        BlockPos::new(self.x, self.y, self.z + 1)
    }
}

/// `ServerPlayer.getLastClientMoveIntent`: the rider's movement keys turned by its yaw.
fn move_intent(ents: &Entities, rider: EntityId) -> Vec3 {
    let (Some(p), Some(e)) = (ents.player(rider), ents.store.get(rider)) else {
        return Vec3::ZERO;
    };
    let input = p.connection.vehicle.input;
    let axis = |a: bool, b: bool| -> f64 {
        if a == b {
            0.0
        } else if a {
            1.0
        } else {
            -1.0
        }
    };
    let relative = Vec3::new(
        axis(input.left, input.right),
        0.0,
        axis(input.forward, input.backward),
    );
    // `getInputVector(relative, 1, yRot)`.
    let len = relative.length_sqr();
    if len < 1.0e-7 {
        return Vec3::ZERO;
    }
    let movement = if len > 1.0 {
        relative.normalize()
    } else {
        relative
    };
    #[allow(clippy::cast_possible_truncation)]
    let a = e.rot().y_rot * (std::f64::consts::PI / 180.0) as f32;
    let (s, co) = (f64::from(sin(f64::from(a))), f64::from(cos(f64::from(a))));
    Vec3::new(
        movement.x * co - movement.z * s,
        movement.y,
        movement.z * co + movement.x * s,
    )
}

/// `OldMinecartBehavior.getPos(x, y, z)`: the point of the rail track at `p`.
fn get_pos(level: &Level, p: Vec3) -> Option<Vec3> {
    let (xt, mut yt, zt) = (floor(p.x), floor(p.y), floor(p.z));
    if is_rail(level.block(BlockPos::new(xt, yt - 1, zt)).block()) {
        yt -= 1;
    }
    let state = level.block(BlockPos::new(xt, yt, zt));
    if !is_rail(state.block()) {
        return None;
    }
    let (exit0, exit1) = exits(RailShape::of(state));
    let (fx, fy, fz) = (f64::from(xt), f64::from(yt), f64::from(zt));
    let x0 = fx + 0.5 + f64::from(exit0[0]) * 0.5;
    let y0 = fy + 0.0625 + f64::from(exit0[1]) * 0.5;
    let z0 = fz + 0.5 + f64::from(exit0[2]) * 0.5;
    let x1 = fx + 0.5 + f64::from(exit1[0]) * 0.5;
    let y1 = fy + 0.0625 + f64::from(exit1[1]) * 0.5;
    let z1 = fz + 0.5 + f64::from(exit1[2]) * 0.5;
    let xd = x1 - x0;
    let yd = (y1 - y0) * 2.0;
    let zd = z1 - z0;
    let progress = if xd == 0.0 {
        p.z - fz
    } else if zd == 0.0 {
        p.x - fx
    } else {
        let xx = p.x - x0;
        let zz = p.z - z0;
        (xx * xd + zz * zd) * 2.0
    };
    let x = x0 + xd * progress;
    let mut y = y0 + yd * progress;
    let z = z0 + zd * progress;
    if yd < 0.0 {
        y += 1.0;
    } else if yd > 0.0 {
        y += 0.5;
    }
    Some(Vec3::new(x, y, z))
}

/// `applyNaturalSlowdown` of the minecart's kind.
fn natural_slowdown(c: &mut Cart<'_, '_>, movement: Vec3) -> Vec3 {
    let is_vehicle = c.is_vehicle();
    let m = c.data_mut();
    let in_water = m.in_water;
    let base = |movement: Vec3| {
        let f = if is_vehicle { 0.997 } else { 0.96 };
        let v = movement.multiply(f, 0.0, f);
        if in_water { v * f64::from(0.95_f32) } else { v }
    };
    match m.kind {
        MinecartKind::Chest | MinecartKind::Hopper => {
            let mut keep = 0.98_f32;
            if m.loot_table.is_none() {
                let emptiness = 15 - super::container_signal(m);
                #[allow(clippy::cast_precision_loss)]
                let e = emptiness as f32;
                keep += e * 0.001;
            }
            if in_water {
                keep *= 0.95;
            }
            let k = f64::from(keep);
            movement.multiply(k, 0.0, k)
        }
        MinecartKind::Furnace => {
            let v = if m.push.length_sqr() > 1.0e-7 {
                m.push = push_along(m.push, movement);
                let mut v = movement.multiply(0.8, 0.0, 0.8) + m.push;
                if in_water {
                    v = v * 0.1;
                }
                v
            } else {
                movement.multiply(0.98, 0.0, 0.98)
            };
            base(v)
        }
        _ => base(movement),
    }
}

/// `MinecartFurnace.calculateNewPushAlong`: the push turns with the track.
fn push_along(push: Vec3, movement: Vec3) -> Vec3 {
    let h = |v: Vec3| v.x * v.x + v.z * v.z;
    if h(push) > 1.0e-4 && h(movement) > 0.001 {
        // `push.projectedOn(movement).normalize().scale(push.length())`.
        let projected = if movement.length_sqr() == 0.0 {
            movement
        } else {
            movement * push.dot(movement) * (1.0 / movement.length_sqr())
        };
        projected.normalize() * push.length()
    } else {
        push
    }
}

/// `AbstractMinecart.comeOffTrack`.
fn come_off_track(c: &mut Cart<'_, '_>) {
    let max = max_speed(c.data());
    let v = c.velocity();
    let mut v = Vec3::new(v.x.clamp(-max, max), v.y, v.z.clamp(-max, max));
    let on_ground = c
        .ents
        .store
        .get(c.id)
        .is_some_and(|e| e.flags().contains(EntityFlags::ON_GROUND));
    if on_ground {
        v = v * 0.5;
    }
    c.set_velocity(v);
    move_cart(c, v);
    if !c.exists() {
        return;
    }
    let on_ground = c
        .ents
        .store
        .get(c.id)
        .is_some_and(|e| e.flags().contains(EntityFlags::ON_GROUND));
    if !on_ground {
        let v = c.velocity();
        c.set_velocity(v * f64::from(0.95_f32));
    }
}

/// `Entity.collide` of a minecart: blocks and the entities it collides with
/// (`AbstractBoat.canVehicleCollide`: pushable ones not riding with it); no step up.
fn collide(c: &mut Cart<'_, '_>, aabb: Aabb, delta: Vec3) -> Vec3 {
    if delta.length_sqr() == 0.0 {
        return delta;
    }
    let mut boxes = std::mem::take(&mut c.cx.boxes);
    boxes.clear();
    let area = aabb.expand_towards(delta.x, delta.y, delta.z);
    physics::block_boxes(&c.level.view(), &area, &mut boxes);
    physics::border_boxes(&c.level.border, &aabb, &area, &mut boxes);
    let size = (area.max_x - area.min_x + area.max_y - area.min_y + area.max_z - area.min_z) / 3.0;
    if size >= 1.0e-7 {
        let root = root_vehicle(c.ents, c.id);
        let test = area.inflate(1.0e-7, 1.0e-7, 1.0e-7);
        let ents = &*c.ents;
        let id = c.id;
        ents.store.for_each_in(&test, false, |o| {
            if o == id || root_vehicle(ents, o) == root || !pushable(ents, o) {
                return;
            }
            if let Some(e) = ents.store.get(o) {
                boxes.push(e.aabb());
            }
        });
    }
    let out = physics::collide_with_shapes(delta, aabb, &boxes);
    c.cx.boxes = boxes;
    out
}

/// `getRootVehicle`.
fn root_vehicle(ents: &Entities, id: EntityId) -> EntityId {
    let mut root = id;
    while let Some(v) = ents.vehicle_of(root) {
        root = v;
    }
    root
}

/// `isPushable` of the entities minecarts meet: live mobs and players (not spectators),
/// boats and minecarts.
fn pushable(ents: &Entities, id: EntityId) -> bool {
    if let Some(m) = ents.mob(id) {
        return m.is_alive() && crate::decoration::stand::pushable(m);
    }
    if let Some(p) = ents.player(id) {
        return p.game_mode != vanillite_config::GameMode::Spectator
            && !p.survival.is_dead_or_dying();
    }
    ents.boat(id).is_some() || ents.minecart(id).is_some()
}

/// `Entity.move(MoverType.SELF, delta)` of a minecart, then `applyEffectsFromBlocks`
/// (`AbstractMinecart.move` without the experiment).
fn move_cart(c: &mut Cart<'_, '_>, delta: Vec3) {
    let Some(e) = c.ents.store.get(c.id) else {
        return;
    };
    let (pos, aabb) = (e.pos(), e.aabb());
    let no_gravity = e.flags().contains(EntityFlags::NO_GRAVITY);
    let movement = collide(c, aabb, delta);
    let movement_length = movement.length_sqr();
    let mut new_pos = pos;
    if movement_length > 1.0e-7 || delta.length_sqr() - movement_length < 1.0e-7 {
        new_pos = pos + movement;
        c.set_pos(new_pos);
    }
    let x_collision = !mth_equal(delta.x, movement.x);
    let z_collision = !mth_equal(delta.z, movement.z);
    let horizontal = x_collision || z_collision;
    let vertical = delta.y != movement.y;
    let below = vertical && delta.y < 0.0;
    if let Some(mut e) = c.ents.store.get_mut(c.id) {
        e.set_flags(EntityFlags::ON_GROUND, below);
    }
    c.data_mut().horizontal_collision = horizontal;
    // `getOnPosLegacy`.
    let effect_pos = BlockPos::new(
        floor(new_pos.x),
        floor(new_pos.y - f64::from(0.2_f32)),
        floor(new_pos.z),
    );
    let effect_state = c.level.block(effect_pos);
    // `checkFallDamage`.
    let m = c.data_mut();
    if !m.in_water && movement.y < 0.0 {
        #[allow(clippy::cast_possible_truncation)]
        let ya = movement.y as f32;
        m.fall_distance -= f64::from(ya);
    }
    if below {
        let fall = m.fall_distance;
        m.fall_distance = 0.0;
        if fall > 0.0 {
            super::fall_on(c.ents, c.env, c.cx, c.id, fall);
            if !c.exists() {
                return;
            }
        }
    }
    let g = gravity(no_gravity, c.data());
    if vertical || horizontal {
        // `restituteMovementAfterCollisions` with a bounciness of 0.
        let current = c.velocity();
        let mut after = current;
        if x_collision {
            after.x = -current.x * 0.0;
        }
        if z_collision {
            after.z = -current.z * 0.0;
        }
        if vertical {
            let mut restitution = 0.0;
            if below {
                restitution = if -current.y > g
                    && !crate::block::block_tags()
                        .suppresses_bounce
                        .contains(effect_state)
                {
                    bounciness(effect_state)
                } else {
                    0.0
                };
            }
            if restitution > 0.0 {
                // `bounceOn`, then `gameEvent(BOUNCE)`.
                crate::survival::game_event(
                    c.env.out,
                    crate::level::game_event::events().bounce,
                    new_pos,
                    Some(c.id),
                );
            }
            after.y = if restitution > 0.0 {
                let portion = movement.y / current.y;
                let compensation = portion * g;
                // `Mth.lerp(portion, 1, getAirDrag())`.
                let drag = 1.0 + portion * (f64::from(0.95_f32) - 1.0);
                (compensation - current.y) * drag * restitution
            } else {
                (0.0 - current.y) * 1.0 * restitution
            };
        }
        c.set_velocity(after);
    }
    // `getBlockSpeedFactor`: 1 on rails.
    let here_pos = BlockPos::containing(new_pos.x, new_pos.y, new_pos.z);
    let here = c.level.block(here_pos);
    let factor = if is_rail(here.block()) {
        1.0
    } else {
        let mut f = here.block().info().speed_factor;
        if f == 1.0 && !here.is(block_ids::WATER) && !here.is(block_ids::BUBBLE_COLUMN) {
            let below_pos = BlockPos::new(
                floor(new_pos.x),
                floor(new_pos.y - f64::from(0.500_001_f32)),
                floor(new_pos.z),
            );
            f = c.level.block(below_pos).block().info().speed_factor;
        }
        f
    };
    if factor != 1.0 {
        let f = f64::from(factor);
        let v = c.velocity();
        c.set_velocity(v.multiply(f, 1.0, f));
    }
    apply_effects_from_blocks(c);
}

/// `getBlockBounciness` of a non-living entity: slime blocks 0.8, beds and shelf mushrooms
/// 0.6.
fn bounciness(state: BlockState) -> f64 {
    let block = state.block();
    let r: f32 = if block == block_ids::SLIME_BLOCK {
        1.0
    } else if block == block_ids::SHELF_MUSHROOM
        || (block.info().kind == vanillite_data::block::BlockKind::BedBlock
            && block != block_ids::STRAW_BED)
    {
        0.75
    } else {
        0.0
    };
    f64::from(r * 0.8)
}

/// `applyEffectsFromBlocks(position, position)`: the blocks the minecart's box touches react
/// right after the tick (detector rails, pressure plates, tripwire).
fn apply_effects_from_blocks(c: &mut Cart<'_, '_>) {
    let Some(e) = c.ents.store.get(c.id) else {
        return;
    };
    let aabb = e.aabb();
    let eps = 1.0e-5;
    let min = BlockPos::containing(aabb.min_x + eps, aabb.min_y + eps, aabb.min_z + eps);
    let max = BlockPos::containing(aabb.max_x - eps, aabb.max_y - eps, aabb.max_z - eps);
    for z in min.z..=max.z {
        for y in min.y..=max.y {
            for x in min.x..=max.x {
                let pos = BlockPos::new(x, y, z);
                if c.level.has_entity_inside(c.level.block(pos)) {
                    c.cx.actions.push(WorldAction::EntityInside(pos, c.id));
                }
            }
        }
    }
}

/// `OldMinecartBehavior.pushAndPickupEntities`: a moving minecart picks up the mobs it meets;
/// otherwise it pushes the minecarts around it.
fn push_and_pickup_entities(c: &mut Cart<'_, '_>) {
    let Some(e) = c.ents.store.get(c.id) else {
        return;
    };
    let hitbox = e
        .aabb()
        .inflate(f64::from(0.2_f32), 0.0, f64::from(0.2_f32));
    let v = e.velocity();
    let id = c.id;
    let mut near = std::mem::take(&mut c.cx.ids);
    near.clear();
    c.ents.store.for_each_in(&hitbox, false, |o| {
        if o != id {
            near.push(o);
        }
    });
    if c.data().rideable() && v.x * v.x + v.z * v.z >= 0.01 {
        for &o in &near {
            if !pushable(c.ents, o) {
                continue;
            }
            let pick_up = c.ents.player(o).is_none()
                && c.ents.minecart(o).is_none()
                && c.ents
                    .store
                    .get(o)
                    .is_some_and(|e| e.kind() != vanillite_data::entity::ids::IRON_GOLEM)
                && c.ents.mob(o).is_some()
                && !c.is_vehicle()
                && c.ents.vehicle_of(o).is_none();
            if pick_up {
                crate::riding::start_riding(c.ents, c.env, o, id, false);
            } else if c.ents.minecart(o).is_some() {
                push_by(c.ents, o, id);
            } else {
                entity_push(c.ents, o, id);
            }
        }
    } else {
        for &o in &near {
            let rides = c.ents.vehicle_of(o) == Some(id);
            if !rides && c.ents.minecart(o).is_some() {
                push_by(c.ents, o, id);
            }
        }
    }
    c.cx.ids = near;
}

/// `Entity.push(x, y, z)` on an entity the server moves (players move on their clients).
fn add_velocity(ents: &mut Entities, id: EntityId, x: f64, z: f64) {
    if ents.player(id).is_some() {
        return;
    }
    if let Some(mut e) = ents.store.get_mut(id) {
        let v = e.as_ref().velocity();
        e.set_velocity(v + Vec3::new(x, 0.0, z));
        e.set_flags(EntityFlags::HAS_IMPULSE, true);
    }
}

/// `Entity.push(other)` of a mob, player or boat (`this`) by a minecart (`other`).
fn entity_push(ents: &mut Entities, this: EntityId, other: EntityId) {
    if root_vehicle(ents, this) == root_vehicle(ents, other) {
        return;
    }
    let (Some(a), Some(b)) = (ents.store.get(this), ents.store.get(other)) else {
        return;
    };
    let (ap, bp) = (a.pos(), b.pos());
    let mut xa = bp.x - ap.x;
    let mut za = bp.z - ap.z;
    let mut dd = xa.abs().max(za.abs());
    if dd < f64::from(0.01_f32) {
        return;
    }
    dd = dd.sqrt();
    xa /= dd;
    za /= dd;
    let pow = (1.0 / dd).min(1.0);
    xa *= pow;
    za *= pow;
    xa *= f64::from(0.05_f32);
    za *= f64::from(0.05_f32);
    if ents.riding.passengers_of(this).is_empty() && pushable(ents, this) {
        add_velocity(ents, this, -xa, -za);
    }
    if ents.riding.passengers_of(other).is_empty() && pushable(ents, other) {
        add_velocity(ents, other, xa, za);
    }
}

/// `AbstractMinecart.push(entity)`: minecart `cart` is pushed by `other`; two minecarts share
/// their speed, a furnace minecart pushes the other along.
pub fn push_by(ents: &mut Entities, cart: EntityId, other: EntityId) {
    if ents.vehicle_of(other) == Some(cart) {
        return;
    }
    let (Some(a), Some(b)) = (ents.store.get(cart), ents.store.get(other)) else {
        return;
    };
    let (ap, bp) = (a.pos(), b.pos());
    let y_rot = a.rot().y_rot;
    let mut xa = bp.x - ap.x;
    let mut za = bp.z - ap.z;
    let mut dd = xa * xa + za * za;
    if dd < f64::from(1.0e-4_f32) {
        return;
    }
    dd = dd.sqrt();
    xa /= dd;
    za /= dd;
    let pow = (1.0 / dd).min(1.0);
    xa *= pow;
    za *= pow;
    xa *= f64::from(0.1_f32);
    za *= f64::from(0.1_f32);
    xa *= 0.5;
    za *= 0.5;
    if ents.minecart(other).is_none() {
        add_velocity(ents, cart, -xa, -za);
        add_velocity(ents, other, xa / 4.0, za / 4.0);
        return;
    }
    // `pushOtherMinecart`.
    let dir = Vec3::new(bp.x - ap.x, 0.0, bp.z - ap.z).normalize();
    #[allow(clippy::cast_possible_truncation)]
    let a = y_rot * (std::f64::consts::PI / 180.0) as f32;
    let facing = Vec3::new(
        f64::from(cos(f64::from(a))),
        0.0,
        f64::from(sin(f64::from(a))),
    )
    .normalize();
    let dot = dir.dot(facing).abs();
    if dot < f64::from(0.8_f32) {
        return;
    }
    let movement = ents.store.get(cart).map_or(Vec3::ZERO, |e| e.velocity());
    let other_movement = ents.store.get(other).map_or(Vec3::ZERO, |e| e.velocity());
    let furnace = |ents: &Entities, id| ents.minecart(id).is_some_and(MinecartData::furnace);
    let (this_f, other_f) = (furnace(ents, cart), furnace(ents, other));
    let set = |ents: &mut Entities, id: EntityId, v: Vec3| {
        if let Some(mut e) = ents.store.get_mut(id) {
            e.set_velocity(v);
        }
    };
    if other_f && !this_f {
        set(ents, cart, movement.multiply(0.2, 1.0, 0.2));
        add_velocity(ents, cart, other_movement.x - xa, other_movement.z - za);
        set(ents, other, other_movement.multiply(0.95, 1.0, 0.95));
    } else if !other_f && this_f {
        set(ents, other, other_movement.multiply(0.2, 1.0, 0.2));
        add_velocity(ents, other, movement.x + xa, movement.z + za);
        set(ents, cart, movement.multiply(0.95, 1.0, 0.95));
    } else {
        let xdd = (other_movement.x + movement.x) / 2.0;
        let zdd = (other_movement.z + movement.z) / 2.0;
        set(ents, cart, movement.multiply(0.2, 1.0, 0.2));
        add_velocity(ents, cart, xdd - xa, zdd - za);
        set(ents, other, other_movement.multiply(0.2, 1.0, 0.2));
        add_velocity(ents, other, xdd + xa, zdd + za);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exits_of_slopes_go_down_on_the_low_side() {
        let (a, b) = exits(RailShape::AscendingEast);
        assert_eq!(a, [-1, -1, 0]);
        assert_eq!(b, [1, 0, 0]);
    }

    #[test]
    fn furnace_push_turns_with_the_track() {
        let push = Vec3::new(1.0, 0.0, 0.0);
        let turned = push_along(push, Vec3::new(0.1, 0.0, 0.1));
        let half = std::f64::consts::FRAC_1_SQRT_2;
        assert!((turned.x - half).abs() < 1e-9 && (turned.z - half).abs() < 1e-9);
    }
}

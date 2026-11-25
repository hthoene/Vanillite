//! Riding (`Entity.startRiding`, `stopRiding`, `positionRider`, `getDismountLocationForPassenger`)
//! and the moves of player-steered vehicles (`handleMoveVehicle`, `handlePaddleBoat`).

use vanillite_base::math::floor;
use vanillite_base::random::RandomSource;
use vanillite_base::{Aabb, BlockPos, Vec3};
use vanillite_protocol::SharedPacket;
use vanillite_protocol::packets::play::clientbound::{MoveVehicleBack, SetPassengers};
use vanillite_protocol::types::VarIntList;
use vanillite_world::entity::{EntityFlags, EntityId, Pose};

use crate::entities::Entities;
use crate::level::Level;
use crate::mob::{Kind, MobContext, physics};
use crate::survival::Effect;
use crate::survival::player::Env;

/// `getVehicleAttachmentPoint` height of a passenger (`Avatar.DEFAULT_VEHICLE_ATTACHMENT`,
/// `ridingOffset`).
fn vehicle_attachment(ents: &Entities, passenger: EntityId) -> f64 {
    if ents.player(passenger).is_some() {
        return 0.6;
    }
    match ents.mob(passenger).map(|m| m.kind) {
        Some(Kind::Skeleton) => 0.7,
        _ => 0.0,
    }
}

/// `getPassengerAttachmentPoint` of a living vehicle (`passengerAttachments`), scaled for
/// babies.
fn mob_passenger_height(kind: Kind, height: f32) -> f64 {
    match kind {
        Kind::Pig => 0.868_75,
        Kind::Zombie => 2.0125,
        Kind::Spider => 0.765,
        Kind::Cow => 1.368_75,
        Kind::Sheep => 1.2375,
        Kind::Chicken => 0.7,
        Kind::Enderman => 2.806_25,
        Kind::Horse => 1.443_75,
        Kind::Donkey => 1.1125,
        Kind::Mule => 1.2125,
        Kind::SkeletonHorse | Kind::ZombieHorse => 1.318_75,
        Kind::Llama | Kind::TraderLlama => 1.37,
        Kind::Wolf => 0.818_75,
        Kind::Cat => 0.5125,
        Kind::Ocelot => 0.6375,
        Kind::Parrot => 0.4625,
        Kind::Fox => 0.6375,
        // `getBodyAnchorAnimationYOffset` of a standing camel: its height less 0.375.
        Kind::Camel | Kind::CamelHusk => f64::from(height) - 0.375,
        Kind::HappyGhast => 4.0,
        Kind::Nautilus | Kind::ZombieNautilus => 1.1375,
        _ => f64::from(height),
    }
}

/// Sends `set_passengers` of `vehicle` to the players that see it.
fn send_passengers(ents: &Entities, env: &mut Env<'_>, vehicle: EntityId) {
    let Some(v) = ents.store.get(vehicle) else {
        return;
    };
    let passengers = ents
        .riding
        .passengers_of(vehicle)
        .iter()
        .filter_map(|&p| ents.store.get(p).map(|e| e.network_id().0))
        .collect();
    env.out.push(Effect::Tracking {
        entity: vehicle,
        with_self: ents.player(vehicle).is_some(),
        packet: SharedPacket::of(&SetPassengers {
            vehicle: v.network_id().0,
            passengers: VarIntList(passengers),
        }),
    });
}

/// The pairing packet of a vehicle with passengers.
pub fn passengers_packet(ents: &Entities, vehicle: EntityId) -> Option<SetPassengers> {
    let list = ents.riding.passengers_of(vehicle);
    if list.is_empty() {
        return None;
    }
    Some(SetPassengers {
        vehicle: ents.store.get(vehicle)?.network_id().0,
        passengers: VarIntList(
            list.iter()
                .filter_map(|&p| ents.store.get(p).map(|e| e.network_id().0))
                .collect(),
        ),
    })
}

/// `canAddPassenger` of a vehicle.
fn can_add_passenger(ents: &Entities, vehicle: EntityId) -> bool {
    let count = ents.riding.passengers_of(vehicle).len();
    if let Some(b) = ents.boat(vehicle) {
        return count < b.kind.max_passengers();
    }
    if let Some(m) = ents.mob(vehicle) {
        return count < crate::mob::fam::max_passengers(m);
    }
    count == 0
}

/// `startRiding(vehicle, force)`: `passenger` gets on `vehicle`. Returns whether it did.
pub fn start_riding(
    ents: &mut Entities,
    env: &mut Env<'_>,
    passenger: EntityId,
    vehicle: EntityId,
    force: bool,
) -> bool {
    if passenger == vehicle || ents.vehicle_of(passenger) == Some(vehicle) {
        return false;
    }
    // No loops: the vehicle must not already ride the passenger.
    let mut up = Some(vehicle);
    while let Some(v) = up {
        if v == passenger {
            return false;
        }
        up = ents.vehicle_of(v);
    }
    if !force && !can_add_passenger(ents, vehicle) {
        return false;
    }
    if ents.vehicle_of(passenger).is_some() {
        stop_riding(ents, env, passenger);
    }
    let is_player = ents.player(passenger).is_some();
    if let Some(mut e) = ents.store.get_mut(passenger) {
        e.set_pose(Pose::Standing);
        e.set_flags(EntityFlags::PASSENGER, true);
    }
    ents.riding.add(passenger, vehicle, is_player);
    if let Some(p) = ents.player_mut(passenger) {
        p.connection.movement.passenger = true;
    }
    if let Some(m) = ents.mob_mut(passenger) {
        m.nav.stop();
    }
    position_riders_of(ents, vehicle);
    send_passengers(ents, env, vehicle);
    true
}

/// `stopRiding`: `passenger` gets off and is placed at the dismount location. Returns the
/// old vehicle.
pub fn stop_riding(
    ents: &mut Entities,
    env: &mut Env<'_>,
    passenger: EntityId,
) -> Option<EntityId> {
    let vehicle = ents.riding.remove(passenger)?;
    if let Some(mut e) = ents.store.get_mut(passenger) {
        e.set_flags(EntityFlags::PASSENGER, false);
    }
    if let Some(p) = ents.player_mut(passenger) {
        p.connection.movement.passenger = false;
    }
    send_passengers(ents, env, vehicle);
    let alive = ents.store.contains(passenger);
    if alive && let Some(target) = dismount_location(ents, env, vehicle, passenger) {
        dismount_to(ents, passenger, target);
    }
    Some(vehicle)
}

/// `ejectPassengers`.
pub fn eject_passengers(ents: &mut Entities, env: &mut Env<'_>, vehicle: EntityId) {
    let list: Vec<EntityId> = ents.riding.passengers_of(vehicle).to_vec();
    for p in list.into_iter().rev() {
        stop_riding(ents, env, p);
    }
}

/// `dismountTo`: players are teleported, others placed.
fn dismount_to(ents: &mut Entities, passenger: EntityId, target: Vec3) {
    let Some(e) = ents.store.get(passenger) else {
        return;
    };
    let rot = e.rot();
    let velocity = e.velocity();
    let on_ground = e.flags().contains(EntityFlags::ON_GROUND);
    let mut body = crate::player::Body {
        pos: e.pos(),
        velocity,
        y_rot: rot.y_rot,
        x_rot: rot.x_rot,
        on_ground,
        horizontal_collision: false,
        fall_flying: false,
    };
    if let Some(p) = ents.player_mut(passenger) {
        let sink = std::sync::Arc::clone(&p.connection.sink);
        p.connection
            .movement
            .teleport(&mut body, target, rot.y_rot, rot.x_rot, sink.as_ref());
    }
    if let Some(mut e) = ents.store.get_mut(passenger) {
        e.set_pos(target);
    }
}

/// The blocks of the survival environment as collision blocks (unloaded = air).
struct EnvBlocks<'a>(&'a dyn crate::survival::blocks::BlockAccess);

impl physics::Blocks for EnvBlocks<'_> {
    fn state(&self, pos: BlockPos) -> vanillite_data::block::BlockState {
        self.0
            .block_state(pos)
            .unwrap_or(vanillite_data::block::BlockState::AIR)
    }
}

/// `BlockGetter.getBlockFloorHeight(pos)`.
fn block_floor_height(level: &dyn physics::Blocks, pos: BlockPos) -> f64 {
    let top = |p: BlockPos| {
        level
            .state(p)
            .collision_shape()
            .iter()
            .map(|b| b[4])
            .reduce(f64::max)
    };
    match top(pos) {
        Some(h) => h,
        None => match top(BlockPos::new(pos.x, pos.y - 1, pos.z)) {
            Some(h) if h >= 1.0 => h - 1.0,
            _ => f64::NEG_INFINITY,
        },
    }
}

fn floor_valid(h: f64) -> bool {
    h.is_finite() && h < 1.0
}

/// `DismountHelper.canDismountTo`: the passenger's box at `target` is free of blocks.
fn can_dismount_to(
    level: &dyn physics::Blocks,
    width: f64,
    height: f64,
    target: Vec3,
    scratch: &mut Vec<Aabb>,
) -> bool {
    let half = width / 2.0;
    let b = Aabb::new(
        target.x - half,
        target.y,
        target.z - half,
        target.x + half,
        target.y + height,
        target.z + half,
    );
    physics::no_collision(level, &b, scratch)
}

/// `getDismountLocationForPassenger`.
fn dismount_location(
    ents: &Entities,
    env: &Env<'_>,
    vehicle: EntityId,
    passenger: EntityId,
) -> Option<Vec3> {
    let v = ents.store.get(vehicle)?;
    let p = ents.store.get(passenger)?;
    let level = &EnvBlocks(env.blocks);
    let vpos = v.pos();
    let fallback = Vec3::new(vpos.x, v.aabb().max_y, vpos.z);
    let (pw, ph) = (f64::from(p.dims().width), f64::from(p.dims().height));
    let mut scratch = Vec::new();
    if let Some(m) = ents.minecart(vehicle) {
        let flipped = m.flipped;
        if let Some(t) = minecart_dismount(ents, level, vehicle, passenger, flipped) {
            return Some(t);
        }
        return Some(fallback);
    }
    if ents.boat(vehicle).is_some() {
        // `getCollisionHorizontalEscapeVector(width * sqrt2, passengerWidth, passengerYRot)`.
        let width = f64::from(v.dims().width) * std::f64::consts::SQRT_2;
        let d = (width + pw + 1.0e-5) / 2.0;
        let a = p.rot().y_rot * std::f32::consts::PI / 180.0;
        let (xd, zd) = (f64::from(-a.sin()), f64::from(a.cos()));
        let d2 = xd.abs().max(zd.abs());
        let (tx, tz) = (vpos.x + xd * d / d2, vpos.z + zd * d / d2);
        let target = BlockPos::containing(tx, v.aabb().max_y, tz);
        let below = BlockPos::new(target.x, target.y - 1, target.z);
        if !crate::mob::is_water(physics::Blocks::state(level, below)) {
            for pos in [target, below] {
                let h = block_floor_height(level, pos);
                if floor_valid(h) {
                    let t = Vec3::new(tx, f64::from(pos.y) + h, tz);
                    if can_dismount_to(level, pw, ph, t, &mut scratch) {
                        return Some(t);
                    }
                }
            }
        }
        return Some(fallback);
    }
    // `LivingEntity.getDismountLocationForPassenger`: the blocks around the vehicle in
    // `DismountHelper.offsetsForDirection(getMotionDirection())` order.
    let yaw = f64::from(v.rot().y_rot);
    let dir = floor(yaw / 90.0 + 0.5) & 3;
    // `Direction.from2DDataValue`: south, west, north, east.
    let (dx, dz) = [(0, 1), (-1, 0), (0, -1), (1, 0)][usize::try_from(dir).unwrap_or(0)];
    // Clockwise of the direction.
    let (cx, cz) = (-dz, dx);
    let offsets = [
        (cx, cz),
        (-cx, -cz),
        (-dx + cx, -dz + cz),
        (-dx - cx, -dz - cz),
        (dx + cx, dz + cz),
        (dx - cx, dz - cz),
        (-dx, -dz),
        (dx, dz),
    ];
    let base = BlockPos::containing(vpos.x, vpos.y, vpos.z);
    for (ox, oz) in offsets {
        let pos = BlockPos::new(base.x + ox, base.y, base.z + oz);
        let h = block_floor_height(level, pos);
        if floor_valid(h) {
            let t = Vec3::new(
                f64::from(pos.x) + 0.5,
                f64::from(pos.y) + h,
                f64::from(pos.z) + 0.5,
            );
            if can_dismount_to(level, pw, ph, t, &mut scratch) {
                return Some(t);
            }
        }
    }
    Some(fallback)
}

/// `AbstractMinecart.getDismountLocationForPassenger`: the blocks beside the minecart's
/// direction of travel, at its height, one up and one down, for each pose the passenger can
/// take.
fn minecart_dismount(
    ents: &Entities,
    level: &dyn physics::Blocks,
    vehicle: EntityId,
    passenger: EntityId,
    flipped: bool,
) -> Option<Vec3> {
    let v = ents.store.get(vehicle)?;
    let p = ents.store.get(passenger)?;
    // `getMotionDirection`: the clockwise of the facing (of its opposite when flipped).
    let yaw = f64::from(v.rot().y_rot);
    let mut dir = (floor(yaw / 90.0 + 0.5) & 3) as usize;
    if flipped {
        dir = (dir + 2) & 3;
    }
    // `Direction.from2DDataValue`: south, west, north, east; clockwise is the next one.
    const STEPS: [(i32, i32); 4] = [(0, 1), (-1, 0), (0, -1), (1, 0)];
    let (dx, dz) = STEPS[(dir + 1) & 3];
    // `DismountHelper.offsetsForDirection(forward)`.
    let (rx, rz) = (-dz, dx);
    let (lx, lz) = (-rx, -rz);
    let (bx, bz) = (-dx, -dz);
    let offsets = [
        (rx, rz),
        (lx, lz),
        (bx + rx, bz + rz),
        (bx + lx, bz + lz),
        (dx + rx, dz + rz),
        (dx + lx, dz + lz),
        (bx, bz),
        (dx, dz),
    ];
    let base = BlockPos::containing(v.pos().x, v.pos().y, v.pos().z);
    // `getDismountPoses` with their heights (`POSE_DISMOUNT_HEIGHTS`).
    const ALL: &[i32] = &[0, 1, -1];
    let poses: Vec<(f64, f64, &[i32])> = if ents.player(passenger).is_some() {
        vec![(0.6, 1.8, ALL), (0.6, 1.5, ALL), (0.6, 0.6, &[0, 1])]
    } else {
        let d = p.dims();
        vec![(f64::from(d.width), f64::from(d.height), ALL)]
    };
    let mut scratch = Vec::new();
    for (width, height, heights) in poses {
        let reach = width.min(1.0);
        for &oy in heights {
            for &(ox, oz) in &offsets {
                let pos = BlockPos::new(base.x + ox, base.y + oy, base.z + oz);
                let h = block_floor_height(level, pos);
                if !floor_valid(h) {
                    continue;
                }
                let t = Vec3::new(
                    f64::from(pos.x) + 0.5,
                    f64::from(pos.y) + h,
                    f64::from(pos.z) + 0.5,
                );
                if can_dismount_to(level, reach, height, t, &mut scratch) {
                    return Some(t);
                }
            }
        }
    }
    None
}

/// `positionRider` of every passenger of `vehicle`.
pub fn position_riders_of(ents: &mut Entities, vehicle: EntityId) {
    let list: Vec<EntityId> = ents.riding.passengers_of(vehicle).to_vec();
    let Some(v) = ents.store.get(vehicle) else {
        return;
    };
    let vpos = v.pos();
    let y_rot = v.rot().y_rot;
    let height = v.dims().height;
    let count = list.len();
    for (i, &p) in list.iter().enumerate() {
        let attach = if let Some(b) = ents.boat(vehicle) {
            let animal = ents.mob(p).is_some_and(|m| !m.kind.is_monster());
            crate::vehicle::passenger_offset(b, y_rot, count, i, animal)
        } else if ents.minecart(vehicle).is_some() {
            Vec3::new(0.0, crate::minecart::passenger_height(ents, p), 0.0)
        } else if let Some(m) = ents.mob(vehicle) {
            let scale = if m.baby { 0.5 } else { 1.0 };
            Vec3::new(0.0, mob_passenger_height(m.kind, height) * scale, 0.0)
        } else {
            Vec3::new(0.0, f64::from(height), 0.0)
        };
        let target = vpos + attach - Vec3::new(0.0, vehicle_attachment(ents, p), 0.0);
        if let Some(mut e) = ents.store.get_mut(p) {
            e.set_pos(target);
        }
    }
}

/// Places the passengers of every vehicle after the vehicles moved (`rideTick`).
pub fn position_all_riders(ents: &mut Entities) {
    if ents.riding.is_empty() {
        return;
    }
    let vehicles = ents.riding.vehicles();
    for v in vehicles {
        position_riders_of(ents, v);
    }
}

/// Whether `player` controls `vehicle` (the first passenger of a boat or a saddled pig).
pub fn controls(ents: &Entities, player: EntityId, vehicle: EntityId) -> bool {
    if ents.riding.passengers_of(vehicle).first() != Some(&player) {
        return false;
    }
    if ents.boat(vehicle).is_some() {
        return true;
    }
    if crate::mob::fam::rider_controls(ents, player, vehicle) {
        return true;
    }
    ents.mob(vehicle).is_some_and(|m| {
        (m.saddled && m.kind == Kind::Pig && holds_carrot_on_a_stick(ents, player))
            || (m.kind == Kind::Strider
                && !m
                    .equipment
                    .get(crate::item::EquipmentSlot::Saddle)
                    .is_empty()
                && holds_item(
                    ents,
                    player,
                    vanillite_data::item::ids::WARPED_FUNGUS_ON_A_STICK.0,
                ))
            || (m.kind.family() != crate::mob::Family::Classic
                && crate::mob::fam::rider_controls_mob(m))
    })
}

/// Whether a player holds `item` in either hand.
fn holds_item(ents: &Entities, player: EntityId, item: u16) -> bool {
    let Some(p) = ents.player(player) else {
        return false;
    };
    [
        crate::item::EquipmentSlot::MainHand,
        crate::item::EquipmentSlot::OffHand,
    ]
    .iter()
    .any(|&s| p.inventory.equipment(s).item == item)
}

/// `Pig.getControllingPassenger`: the player holds a carrot on a stick in either hand.
fn holds_carrot_on_a_stick(ents: &Entities, player: EntityId) -> bool {
    let Some(p) = ents.player(player) else {
        return false;
    };
    let id = vanillite_data::item::ids::CARROT_ON_A_STICK.0;
    [
        crate::item::EquipmentSlot::MainHand,
        crate::item::EquipmentSlot::OffHand,
    ]
    .iter()
    .any(|&s| p.inventory.equipment(s).item == id)
}

/// `handleMoveVehicle`: the vehicle the player steers moves to where its client put it, unless
/// that is too far or into blocks; then the client gets the server's position back.
#[allow(clippy::too_many_arguments)]
pub fn move_vehicle(
    level: &Level,
    ents: &mut Entities,
    cx: &mut MobContext,
    player: EntityId,
    target: Vec3,
    y_rot: f32,
    x_rot: f32,
    on_ground: bool,
) {
    let Some(vehicle) = ents.vehicle_of(player) else {
        return;
    };
    if ents.vehicle_of(vehicle).is_some() || !controls(ents, player, vehicle) {
        return;
    }
    let Some(v) = ents.store.get(vehicle) else {
        return;
    };
    let old = v.pos();
    let old_box = v.aabb();
    let rot = v.rot();
    let velocity = v.velocity();
    let send_back = |ents: &mut Entities| {
        if let Some(p) = ents.player(player) {
            p.connection.sink.send(&MoveVehicleBack {
                position: vanillite_protocol::types::Vec3 {
                    x: old.x,
                    y: old.y,
                    z: old.z,
                },
                y_rot: rot.y_rot,
                x_rot: rot.x_rot,
            });
        }
    };
    let delta = target - old;
    if delta.length_sqr() - velocity.length_sqr() > 100.0 {
        send_back(ents);
        return;
    }
    let step = if ents.mob(vehicle).is_some() {
        1.0
    } else {
        0.0
    };
    let was_on_ground = v.flags().contains(EntityFlags::ON_GROUND);
    let on_lava = ents.mob(vehicle).is_some_and(|m| m.nav.stands_on_lava);
    let moved = physics::collide_for(
        level,
        old_box,
        delta,
        step,
        was_on_ground,
        &mut cx.boxes,
        on_lava,
    );
    let mut err = target - (old + moved);
    if err.y > -0.5 && err.y < 0.5 {
        err.y = 0.0;
    }
    let wrong = err.length_sqr() > 0.0625;
    if wrong && physics::no_collision(level, &old_box, &mut cx.boxes) {
        send_back(ents);
        return;
    }
    if let Some(mut e) = ents.store.get_mut(vehicle) {
        e.set_pos(target);
        e.set_rot(vanillite_world::entity::Rotation {
            y_rot,
            x_rot,
            y_head_rot: y_rot,
        });
        e.set_flags(EntityFlags::ON_GROUND, on_ground);
    }
    if let Some(m) = ents.mob_mut(vehicle) {
        m.y_body_rot = y_rot;
    }
    position_riders_of(ents, vehicle);
}

/// `handlePaddleBoat`.
pub fn paddle_boat(ents: &mut Entities, player: EntityId, left: bool, right: bool) {
    let Some(vehicle) = ents.vehicle_of(player) else {
        return;
    };
    let changed = ents.boat_mut(vehicle).is_some_and(|b| {
        let changed = b.paddles != (left, right);
        b.paddles = (left, right);
        changed
    });
    if changed && let Some(mut e) = ents.store.get_mut(vehicle) {
        e.mark_data_dirty();
    }
}

/// `FoodOnAStickItem.use`: the pig the player steers gets a boost; the carrot on a stick
/// loses 7 durability and turns into a fishing rod when it breaks.
pub fn boost(
    ents: &mut Entities,
    env: &mut Env<'_>,
    player: EntityId,
    stack: &mut crate::item::ItemStack,
    creative: bool,
) -> bool {
    let Some(vehicle) = ents.vehicle_of(player) else {
        return false;
    };
    if !controls(ents, player, vehicle) {
        return false;
    }
    let Some(m) = ents.mob_mut(vehicle) else {
        return false;
    };
    if m.boosting {
        return false;
    }
    m.boosting = true;
    m.boost_time = 0;
    m.boost_total = m.random.next_int_bounded(841) + 140;
    if let Some(mut e) = ents.store.get_mut(vehicle) {
        e.mark_data_dirty();
    }
    if creative {
        return true;
    }
    let unbreaking = crate::player::play::game_data()
        .map_or(0, |d| d.enchantment_level(stack, "minecraft:unbreaking"));
    let mut n = 0;
    for _ in 0..7 {
        #[allow(clippy::cast_precision_loss)]
        let chance = 1.0 / (unbreaking.max(0) as f32 + 1.0);
        if unbreaking <= 0 || env.random.next_float() < chance {
            n += 1;
        }
    }
    let d = crate::item::damage(stack) + n;
    if d >= crate::item::max_damage(stack) {
        // `hurtAndConvertOnBreak(7, FISHING_ROD)`: the components stay, the damage resets.
        let mut rod = stack.clone();
        rod.item = vanillite_data::item::ids::FISHING_ROD.0;
        rod.count = 1;
        crate::item::set_damage(&mut rod, 0);
        *stack = rod;
    } else {
        crate::item::set_damage(stack, d);
    }
    true
}

/// `player_command` `START_RIDING_JUMP`: the jump of a mount the player rides
/// (`PlayerRideableJumping.handleStartJump`).
pub fn riding_jump(ents: &mut Entities, env: &mut Env<'_>, player: EntityId, strength: i32) {
    let Some(vehicle) = ents.vehicle_of(player) else {
        return;
    };
    if strength > 0 {
        crate::mob::fam::rider_jump(ents, env, vehicle, strength);
    }
}

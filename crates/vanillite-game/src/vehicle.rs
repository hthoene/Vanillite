//! Boats and rafts (`AbstractBoat`, `VehicleEntity`): placing them, their floating physics
//! while no player steers them, breaking them, and the rider positions. A player steers its
//! boat on its own client (`move_vehicle`, `paddle_boat`); the server only checks and applies
//! those moves (see [`crate::riding`]).

use vanillite_base::math::{ceil, floor};
use vanillite_base::{Aabb, BlockPos, Vec3};
use vanillite_data::entity::EntityTypeId;
use vanillite_world::entity::{EntityFlags, EntityId, NetworkIds, NewEntity, Rotation};

use crate::entities::Entities;
use crate::item::ItemStack;
use crate::level::Level;
use crate::mob::{MobContext, is_water, physics};
use crate::survival::Effect;
use crate::survival::damage::DamageSource;
use crate::survival::player::Env;
use crate::survival::rules::Rule;

/// A boat type: its entity type, whether it is a raft and whether it has a chest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoatKind {
    pub kind: EntityTypeId,
    pub raft: bool,
    pub chest: bool,
}

impl BoatKind {
    /// The boat kind of an entity type (`*_boat`, `*_chest_boat`, `*_raft`, `*_chest_raft`).
    pub fn of(kind: EntityTypeId) -> Option<BoatKind> {
        let name = kind.info().name;
        let raft = name.ends_with("_raft");
        if !raft && !name.ends_with("_boat") {
            return None;
        }
        Some(BoatKind {
            kind,
            raft,
            chest: name.ends_with("_chest_boat") || name.ends_with("_chest_raft"),
        })
    }

    /// The item it drops (`getDropItem`): the item of the same name.
    pub fn drop_item(self) -> Option<u16> {
        vanillite_data::registry::ITEM.id(self.kind.info().name)
    }

    /// `getMaxPassengers`.
    pub fn max_passengers(self) -> usize {
        if self.chest { 1 } else { 2 }
    }

    /// `rideHeight(dimensions)`.
    pub fn ride_height(self) -> f64 {
        let h = self.kind.info().height;
        f64::from(if self.raft { h * 0.888_888_9 } else { h / 3.0 })
    }

    /// `getSinglePassengerXOffset`: chest boats seat their rider forward.
    pub fn single_passenger_offset(self) -> f32 {
        if self.chest { 0.15 } else { 0.0 }
    }
}

/// `AbstractBoat.Status`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Status {
    InWater,
    UnderWater,
    UnderFlowingWater,
    OnLand,
    #[default]
    InAir,
}

/// The kind data of a boat.
#[derive(Clone, Debug, PartialEq)]
pub struct BoatData {
    pub kind: BoatKind,
    pub status: Status,
    pub old_status: Status,
    pub out_of_control_ticks: f32,
    pub delta_rotation: f32,
    pub water_level: f64,
    pub land_friction: f32,
    pub last_yd: f64,
    /// `DATA_ID_HURT`, `DATA_ID_HURTDIR`, `DATA_ID_DAMAGE`.
    pub hurt_time: i32,
    pub hurt_dir: i32,
    pub damage: f32,
    /// `DATA_ID_PADDLE_LEFT` / `RIGHT`.
    pub paddles: (bool, bool),
    pub fall_distance: f64,
    /// Custom name (the `custom_name` component of the item it came from), encoded.
    pub custom_name: Option<Box<[u8]>>,
    pub tick_count: i32,
    /// The 27 slots of a chest boat or raft (empty for the others).
    pub items: Vec<ItemStack>,
    /// `LootTable` and `LootTableSeed` of a chest boat until the loot is rolled
    /// ([`crate::block::chest_loot::unpack_entity`]).
    pub loot_table: Option<(String, i64)>,
}

impl BoatData {
    /// A new boat.
    pub fn new(kind: BoatKind) -> BoatData {
        BoatData {
            kind,
            status: Status::InAir,
            old_status: Status::InAir,
            out_of_control_ticks: 0.0,
            delta_rotation: 0.0,
            water_level: 0.0,
            land_friction: 0.0,
            last_yd: 0.0,
            hurt_time: 0,
            hurt_dir: 1,
            damage: 0.0,
            paddles: (false, false),
            fall_distance: 0.0,
            custom_name: None,
            tick_count: 0,
            items: if kind.chest {
                vec![ItemStack::EMPTY; 27]
            } else {
                Vec::new()
            },
            loot_table: None,
        }
    }
}

/// Adds a boat (`EntityType.create` + `setInitialPos`).
pub fn spawn_boat(
    ents: &mut Entities,
    network_ids: &NetworkIds,
    uuid: u128,
    pos: Vec3,
    y_rot: f32,
    data: BoatData,
) -> EntityId {
    let mut new = NewEntity::new(data.kind.kind, network_ids.next(), uuid, pos);
    new.rot = Rotation {
        y_rot,
        x_rot: 0.0,
        y_head_rot: y_rot,
    };
    let network_id = new.network_id.0;
    let boats = &mut ents.boats;
    let id = ents.store.spawn(new, |id| boats.push(id, data));
    ents.by_network_id.insert(network_id, id);
    id
}

impl Entities {
    /// The boat data of an entity.
    pub fn boat(&self, id: EntityId) -> Option<&BoatData> {
        let e = self.store.get(id)?;
        BoatKind::of(e.kind())?;
        self.boats.get(e.kind_row())
    }

    /// The boat data of an entity, mutably.
    pub fn boat_mut(&mut self, id: EntityId) -> Option<&mut BoatData> {
        let e = self.store.get(id)?;
        BoatKind::of(e.kind())?;
        let row = e.kind_row();
        self.boats.get_mut(row)
    }
}

/// `FluidState.getHeight` of water at `pos`, `None` without water.
fn water_height(level: &Level, pos: BlockPos) -> Option<(f32, bool)> {
    let state = level.block(pos);
    if !is_water(state) {
        return None;
    }
    let above = level.block(BlockPos::new(pos.x, pos.y + 1, pos.z));
    let f = state.fluid_state();
    let h = if is_water(above) {
        1.0
    } else {
        f.info().own_height
    };
    Some((h, f.info().source))
}

/// `isUnderwater`.
fn underwater_status(level: &Level, aabb: &Aabb) -> Option<Status> {
    let max_y = aabb.max_y + 0.001;
    let (x0, x1) = (floor(aabb.min_x), ceil(aabb.max_x));
    let (y0, y1) = (floor(aabb.max_y), ceil(max_y));
    let (z0, z1) = (floor(aabb.min_z), ceil(aabb.max_z));
    let mut under = false;
    for x in x0..x1 {
        for y in y0..y1 {
            for z in z0..z1 {
                if let Some((h, source)) = water_height(level, BlockPos::new(x, y, z))
                    && max_y < f64::from(y) + f64::from(h)
                {
                    if !source {
                        return Some(Status::UnderFlowingWater);
                    }
                    under = true;
                }
            }
        }
    }
    under.then_some(Status::UnderWater)
}

/// `checkInWater`: sets the water level.
fn check_in_water(level: &Level, aabb: &Aabb, b: &mut BoatData) -> bool {
    let (x0, x1) = (floor(aabb.min_x), ceil(aabb.max_x));
    let (y0, y1) = (floor(aabb.min_y), ceil(aabb.min_y + 0.001));
    let (z0, z1) = (floor(aabb.min_z), ceil(aabb.max_z));
    let mut in_water = false;
    b.water_level = -f64::MAX;
    for x in x0..x1 {
        for y in y0..y1 {
            for z in z0..z1 {
                if let Some((h, _)) = water_height(level, BlockPos::new(x, y, z)) {
                    let height = f64::from(y as f32 + h);
                    b.water_level = b.water_level.max(height);
                    in_water |= aabb.min_y < height;
                }
            }
        }
    }
    in_water
}

/// `getGroundFriction`: the mean friction of the blocks under the boat.
fn ground_friction(level: &Level, aabb: &Aabb) -> f32 {
    let bx = Aabb::new(
        aabb.min_x,
        aabb.min_y - 0.001,
        aabb.min_z,
        aabb.max_x,
        aabb.min_y,
        aabb.max_z,
    );
    let (x0, x1) = (floor(bx.min_x) - 1, ceil(bx.max_x) + 1);
    let (y0, y1) = (floor(bx.min_y) - 1, ceil(bx.max_y) + 1);
    let (z0, z1) = (floor(bx.min_z) - 1, ceil(bx.max_z) + 1);
    let lily = vanillite_data::block::ids::LILY_PAD;
    let mut friction = 0.0;
    let mut count = 0;
    for x in x0..x1 {
        for z in z0..z1 {
            let edges = i32::from(x == x0 || x == x1 - 1) + i32::from(z == z0 || z == z1 - 1);
            if edges == 2 {
                continue;
            }
            for y in y0..y1 {
                if edges > 0 && (y == y0 || y == y1 - 1) {
                    continue;
                }
                let pos = BlockPos::new(x, y, z);
                let state = level.block(pos);
                if state.block() == lily {
                    continue;
                }
                let (ox, oy, oz) = (f64::from(x), f64::from(y), f64::from(z));
                let hit = state.collision_shape().iter().any(|b| {
                    Aabb::new(
                        ox + b[0],
                        oy + b[1],
                        oz + b[2],
                        ox + b[3],
                        oy + b[4],
                        oz + b[5],
                    )
                    .intersects(&bx)
                });
                if hit {
                    friction += state.block().info().friction;
                    count += 1;
                }
            }
        }
    }
    #[allow(clippy::cast_precision_loss)]
    let f = friction / count as f32;
    f
}

/// `getWaterLevelAbove`.
fn water_level_above(level: &Level, aabb: &Aabb, last_yd: f64) -> f32 {
    let (x0, x1) = (floor(aabb.min_x), ceil(aabb.max_x));
    let (y0, y1) = (floor(aabb.max_y), ceil(aabb.max_y - last_yd));
    let (z0, z1) = (floor(aabb.min_z), ceil(aabb.max_z));
    'outer: for y in y0..y1 {
        let mut height = 0.0_f32;
        for x in x0..x1 {
            for z in z0..z1 {
                if let Some((h, _)) = water_height(level, BlockPos::new(x, y, z)) {
                    height = height.max(h);
                }
                if height >= 1.0 {
                    continue 'outer;
                }
            }
        }
        if height < 1.0 {
            #[allow(clippy::cast_precision_loss)]
            return y as f32 + height;
        }
    }
    #[allow(clippy::cast_precision_loss)]
    let r = (y1 + 1) as f32;
    r
}

/// `getStatus`.
fn status(level: &Level, aabb: &Aabb, b: &mut BoatData) -> Status {
    if let Some(s) = underwater_status(level, aabb) {
        b.water_level = aabb.max_y;
        return s;
    }
    if check_in_water(level, aabb, b) {
        return Status::InWater;
    }
    let f = ground_friction(level, aabb);
    if f > 0.0 {
        b.land_friction = f;
        Status::OnLand
    } else {
        Status::InAir
    }
}

/// Whether a player controls the boat (`getControllingPassenger() instanceof Player`).
pub fn player_controlled(ents: &Entities, id: EntityId) -> bool {
    ents.riding
        .passengers_of(id)
        .first()
        .is_some_and(|&p| ents.player(p).is_some())
}

/// `AbstractBoat.tick` on the server.
pub fn tick_boat(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    cx: &mut MobContext,
    id: EntityId,
) {
    let Some(e) = ents.store.get(id) else {
        return;
    };
    let aabb = e.aabb();
    let controlled = player_controlled(ents, id);
    let Some(b) = ents.boat_mut(id) else {
        return;
    };
    b.tick_count += 1;
    b.old_status = b.status;
    b.status = status(level, &aabb, b);
    if matches!(b.status, Status::UnderWater | Status::UnderFlowingWater) {
        b.out_of_control_ticks += 1.0;
    } else {
        b.out_of_control_ticks = 0.0;
    }
    let eject = b.out_of_control_ticks >= 60.0;
    let mut dirty = false;
    if b.hurt_time > 0 {
        b.hurt_time -= 1;
        dirty = true;
    }
    if b.damage > 0.0 {
        b.damage -= 1.0;
        dirty = true;
    }
    if eject {
        crate::riding::eject_passengers(ents, env, id);
    }
    if controlled {
        // The rider's client moves the boat.
        if let Some(mut e) = ents.store.get_mut(id) {
            e.set_velocity(Vec3::ZERO);
        }
        if let Some(b) = ents.boat_mut(id) {
            b.delta_rotation = 0.0;
        }
    } else {
        if let Some(b) = ents.boat_mut(id)
            && b.paddles != (false, false)
        {
            b.paddles = (false, false);
            dirty = true;
        }
        float_boat(level, ents, cx, id);
        let v = ents.store.get(id).map_or(Vec3::ZERO, |e| e.velocity());
        move_boat(level, ents, cx, id, v);
    }
    if dirty && let Some(mut e) = ents.store.get_mut(id) {
        e.mark_data_dirty();
    }
    // `applyEffectsFromBlocks`: lily pads break, plates and tripwire react, on the boat's way
    // this tick.
    if let Some(e) = ents.store.get(id) {
        let way = [crate::survival::inside::Movement::plain(
            e.old_pos(),
            e.pos(),
        )];
        crate::block::plate::for_each_inside(level, e.dims(), &way, |pos| {
            cx.actions
                .push(crate::mob::WorldAction::EntityInside(pos, id));
        });
    }
    // Living entities pushed against a boat no player steers get in; the others are pushed.
    board_nearby(ents, env, id, !controlled);
}

/// The pushable entities next to the boat: mobs board it when there is room (and
/// `add_passengers`), else the boat and the mob push each other (`Entity.push(entity)`).
fn board_nearby(ents: &mut Entities, env: &mut Env<'_>, id: EntityId, add_passengers: bool) {
    let Some(e) = ents.store.get(id) else {
        return;
    };
    let area = e.aabb().inflate(0.2, -0.01, 0.2);
    let width = e.dims().width;
    let Some(max) = ents.boat(id).map(|b| b.kind.max_passengers()) else {
        return;
    };
    let mut near = Vec::new();
    ents.store.for_each_in(&area, false, |o| {
        if o != id {
            near.push(o);
        }
    });
    for o in near {
        let Some(m) = ents.mob(o) else {
            continue;
        };
        // `EntitySelector.pushableBy`: alive, pushable mobs (bats are not).
        if !m.is_alive() || m.kind == crate::mob::Kind::Bat || ents.vehicle_of(o) == Some(id) {
            continue;
        }
        let fits = ents.store.get(o).is_some_and(|e| e.dims().width < width);
        let boards = add_passengers
            && ents.riding.passengers_of(id).len() < max
            && ents.vehicle_of(o).is_none()
            && fits
            && !crate::effect::entity_in_tag(
                m.kind.type_id(),
                "minecraft:cannot_be_pushed_onto_boats",
            );
        if boards {
            crate::riding::start_riding(ents, env, o, id, false);
        } else {
            push_apart(ents, id, o);
        }
    }
}

/// `Entity.push(entity)` of a boat and a mob: both are pushed apart (5 % of the unit direction
/// along the larger axis), unless they carry passengers.
pub(crate) fn push_apart(ents: &mut Entities, boat: EntityId, other: EntityId) {
    let (Some(a), Some(b)) = (ents.store.get(boat), ents.store.get(other)) else {
        return;
    };
    let (pa, pb) = (a.pos(), b.pos());
    let (mut xa, mut za) = (pb.x - pa.x, pb.z - pa.z);
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
    let push = |ents: &mut Entities, id: EntityId, x: f64, z: f64| {
        if ents.riding.passengers_of(id).is_empty()
            && let Some(mut e) = ents.store.get_mut(id)
        {
            let v = e.as_ref().velocity();
            e.set_velocity(Vec3::new(v.x + x, v.y, v.z + z));
        }
    };
    push(ents, boat, -xa, -za);
    push(ents, other, xa, za);
}

/// `floatBoat`.
fn float_boat(level: &Level, ents: &mut Entities, cx: &mut MobContext, id: EntityId) {
    let Some(e) = ents.store.get(id) else {
        return;
    };
    let pos = e.pos();
    let aabb = e.aabb();
    let height = f64::from(e.dims().height);
    let gravity = if e.flags().contains(EntityFlags::NO_GRAVITY) {
        0.0
    } else {
        0.04
    };
    let mut v = e.velocity();
    let Some(b) = ents.boat_mut(id) else {
        return;
    };
    let mut vspeed = -gravity;
    let mut buoyancy = 0.0;
    let inv_friction: f32;
    if b.old_status == Status::InAir && b.status != Status::InAir && b.status != Status::OnLand {
        b.water_level = pos.y + height;
        let target_y = f64::from(water_level_above(level, &aabb, b.last_yd)) - height + 0.101;
        let moved = aabb.offset(0.0, target_y - pos.y, 0.0);
        b.status = Status::InWater;
        if physics::no_collision(&level.view(), &moved, &mut cx.boxes) {
            b.last_yd = 0.0;
            if let Some(mut e) = ents.store.get_mut(id) {
                e.set_pos(Vec3::new(pos.x, target_y, pos.z));
                e.set_velocity(v.multiply(1.0, 0.0, 1.0));
            }
        }
        return;
    }
    match b.status {
        Status::InWater => {
            buoyancy = (b.water_level - pos.y) / height;
            inv_friction = 0.9;
        }
        Status::UnderFlowingWater => {
            vspeed = -7.0e-4;
            inv_friction = 0.9;
        }
        Status::UnderWater => {
            buoyancy = 0.01;
            inv_friction = 0.45;
        }
        Status::InAir => inv_friction = 0.9,
        Status::OnLand => inv_friction = b.land_friction,
    }
    let f = f64::from(inv_friction);
    v = Vec3::new(v.x * f, v.y + vspeed, v.z * f);
    b.delta_rotation *= inv_friction;
    if buoyancy > 0.0 {
        v = Vec3::new(v.x, (v.y + buoyancy * (0.04 / 0.65)) * 0.75, v.z);
    }
    if let Some(mut e) = ents.store.get_mut(id) {
        e.set_velocity(v);
    }
}

/// `move(MoverType.SELF, delta)` of a boat with `checkFallDamage`.
fn move_boat(level: &Level, ents: &mut Entities, cx: &mut MobContext, id: EntityId, delta: Vec3) {
    let Some(e) = ents.store.get(id) else {
        return;
    };
    let aabb = e.aabb();
    let on_ground = e.flags().contains(EntityFlags::ON_GROUND);
    let pos = e.pos();
    let movement = physics::collide(&level.view(), aabb, delta, 0.0, on_ground, &mut cx.boxes);
    let mut v = delta;
    if movement.x != delta.x {
        v.x = 0.0;
    }
    if movement.z != delta.z {
        v.z = 0.0;
    }
    let landed = delta.y != movement.y && delta.y < 0.0;
    if delta.y != movement.y {
        v.y = 0.0;
    }
    if let Some(mut e) = ents.store.get_mut(id) {
        // A tiny move that nothing clipped still happens.
        let length = movement.length_sqr();
        if length > 1.0e-7 || delta.length_sqr() - length < 1.0e-7 {
            e.set_pos(pos + movement);
        }
        e.set_flags(EntityFlags::ON_GROUND, landed);
        e.set_velocity(v);
    }
    let below_water = is_water(level.block(BlockPos::containing(pos.x, pos.y - 1.0, pos.z)));
    if let Some(b) = ents.boat_mut(id) {
        b.last_yd = delta.y;
        if landed {
            b.fall_distance = 0.0;
        } else if !below_water && movement.y < 0.0 {
            b.fall_distance -= movement.y;
        }
    }
}

/// `VehicleEntity.hurtServer`: hits shake the boat; enough damage, or any hit of a creative
/// player, breaks it. Returns whether the hit counted.
pub fn hurt_boat(
    ents: &mut Entities,
    env: &mut Env<'_>,
    cx: &mut MobContext,
    id: EntityId,
    source: DamageSource,
    amount: f32,
) -> bool {
    let creative = source
        .cause
        .and_then(|c| ents.player(c))
        .is_some_and(|p| p.game_mode == vanillite_config::GameMode::Creative);
    if cx.discarded.contains(&id) {
        return true;
    }
    let pos = ents.store.get(id).map(|e| e.pos());
    crate::minecart::damage_game_event(ents, cx, id, source.cause);
    let Some(b) = ents.boat_mut(id) else {
        return false;
    };
    b.hurt_dir = -b.hurt_dir;
    b.hurt_time = 10;
    b.damage += amount * 10.0;
    let destroy = !creative && b.damage > 40.0;
    let drop = b.kind.drop_item();
    let name = b.custom_name.clone();
    let items = if destroy {
        std::mem::take(&mut b.items)
    } else {
        Vec::new()
    };
    let loot = if destroy { b.loot_table.take() } else { None };
    if let Some(mut e) = ents.store.get_mut(id) {
        e.mark_data_dirty();
        e.set_flags(EntityFlags::HURT_MARKED, true);
    }
    if creative || destroy {
        crate::riding::eject_passengers(ents, env, id);
        if let Some(mut e) = ents.store.get_mut(id) {
            e.set_flags(EntityFlags::HURT_MARKED, false);
        }
        cx.discarded.push(id);
    }
    if let (Some((table, seed)), Some(pos)) = (loot.filter(|_| !items.is_empty()), pos) {
        // The loot is rolled first, which needs the world.
        let entity_drops = env.rules.bool(Rule::ENTITY_DROPS);
        let stack = drop.map(|item| {
            let mut stack = ItemStack::new(item, 1);
            if let Some(n) = name {
                stack
                    .patch_mut()
                    .set(vanillite_protocol::components::ids::CUSTOM_NAME, n);
            }
            stack
        });
        cx.actions
            .push(crate::mob::WorldAction::DestroyLootVehicle(Box::new(
                crate::minecart::LootVehicleDrop {
                    pos,
                    table,
                    seed,
                    slots: items.len(),
                    drop: stack,
                    entity_drops,
                },
            )));
        return true;
    }
    // `kill` → `remove(KILLED)`: a chest boat drops its contents first.
    if let Some(pos) = pos
        && !items.is_empty()
    {
        crate::minecart::drop_contents(env, pos, &items);
    }
    if destroy
        && env.rules.bool(Rule::ENTITY_DROPS)
        && let (Some(item), Some(pos)) = (drop, pos)
    {
        let mut stack = ItemStack::new(item, 1);
        if let Some(n) = name {
            stack
                .patch_mut()
                .set(vanillite_protocol::components::ids::CUSTOM_NAME, n);
        }
        let (uuid, y_rot, velocity, item_random) = crate::mob::new_item_entity();
        env.out.push(Effect::DropItem {
            random: item_random,
            uuid,
            pos,
            velocity,
            y_rot,
            pickup_delay: 10,
            stack,
        });
        if !items.is_empty() {
            // `chestVehicleDestroyed` drops the (now empty) contents once more.
            crate::minecart::drop_contents(env, pos, &vec![ItemStack::EMPTY; items.len()]);
        }
    }
    true
}

/// `getPassengerAttachmentPoint` of a boat for its passenger number `index`.
pub fn passenger_offset(
    b: &BoatData,
    y_rot: f32,
    count: usize,
    index: usize,
    animal: bool,
) -> Vec3 {
    let mut offset = b.kind.single_passenger_offset();
    if count > 1 {
        offset = if index == 0 { 0.2 } else { -0.6 };
        if animal {
            offset += 0.2;
        }
    }
    // `new Vec3(0, rideHeight, offset).yRot(-yRot * PI / 180)`.
    let a = -y_rot * std::f32::consts::PI / 180.0;
    let (sin, cos) = (f64::from(a.sin()), f64::from(a.cos()));
    let z = f64::from(offset);
    Vec3::new(z * sin, b.kind.ride_height(), z * cos)
}

/// `BoatItem.use`: places a boat where the player looks at water or a block, facing like the
/// player. Returns whether the item was used (the stack shrinks unless `infinite`).
#[allow(clippy::too_many_arguments)]
pub fn use_boat_item(
    level: &Level,
    ents: &mut Entities,
    network_ids: &NetworkIds,
    player: EntityId,
    stack: &mut ItemStack,
    infinite: bool,
    reach: f64,
) -> bool {
    let item_name = crate::item::item_id(stack).name();
    let Some(kind) = vanillite_data::registry::ENTITY_TYPE
        .id(item_name)
        .map(EntityTypeId)
        .and_then(BoatKind::of)
    else {
        return false;
    };
    let Some(e) = ents.store.get(player) else {
        return false;
    };
    let rot = e.rot();
    let eye = e.pos() + Vec3::new(0.0, f64::from(e.kind().info().eye_height), 0.0);
    let (yr, xr) = (
        f64::from(rot.y_rot) * std::f64::consts::PI / 180.0,
        f64::from(rot.x_rot) * std::f64::consts::PI / 180.0,
    );
    let look = Vec3::new(-yr.sin() * xr.cos(), -xr.sin(), yr.cos() * xr.cos());
    let to = eye + look * reach;
    let Some((_, hit)) = physics::clip_fluids(level, eye, to) else {
        return false;
    };
    // A pickable entity around the player's eyes blocks it.
    let view = e
        .aabb()
        .expand_towards(look.x * 5.0, look.y * 5.0, look.z * 5.0)
        .inflate(1.0, 1.0, 1.0);
    let mut blocked = false;
    ents.store.for_each_in(&view, false, |o| {
        let pickable = ents.mob(o).is_some() || ents.boat(o).is_some();
        if o != player
            && pickable
            && ents
                .store
                .get(o)
                .is_some_and(|oe| oe.aabb().contains(eye.x, eye.y, eye.z))
        {
            blocked = true;
        }
    });
    if blocked {
        return false;
    }
    let info = kind.kind.info();
    let half = f64::from(info.width) / 2.0;
    let boat_box = Aabb::new(
        hit.x - half,
        hit.y,
        hit.z - half,
        hit.x + half,
        hit.y + f64::from(info.height),
        hit.z + half,
    );
    let mut scratch = Vec::new();
    if !physics::no_collision(&level.view(), &boat_box, &mut scratch) {
        return false;
    }
    let mut data = BoatData::new(kind);
    data.custom_name = stack
        .component(vanillite_protocol::components::ids::CUSTOM_NAME)
        .map(Into::into);
    let uuid = crate::mob::new_entity_uuid();
    let id = spawn_boat(ents, network_ids, uuid, hit, rot.y_rot, data);
    if !ents.spawned(id, vanillite_abi::spawn_reason::DEFAULT, 0) {
        return false;
    }
    if !infinite {
        stack.shrink(1);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use vanillite_data::entity::ids;

    #[test]
    fn boat_kinds() {
        let oak = BoatKind::of(ids::OAK_BOAT).expect("boat");
        assert!(!oak.raft && !oak.chest);
        assert_eq!(oak.max_passengers(), 2);
        let raft = BoatKind::of(ids::BAMBOO_CHEST_RAFT).expect("raft");
        assert!(raft.raft && raft.chest);
        assert_eq!(raft.max_passengers(), 1);
        assert!(BoatKind::of(ids::PIG).is_none());
        assert_eq!(
            oak.drop_item(),
            vanillite_data::registry::ITEM.id("minecraft:oak_boat")
        );
    }

    #[test]
    fn two_passengers_sit_front_and_back() {
        let b = BoatData::new(BoatKind::of(ids::OAK_BOAT).expect("boat"));
        let front = passenger_offset(&b, 0.0, 2, 0, false);
        let back = passenger_offset(&b, 0.0, 2, 1, false);
        assert!((front.z - 0.2).abs() < 1e-6 && (back.z + 0.6).abs() < 1e-6);
        assert!((front.y - 0.5625 / 3.0).abs() < 1e-6);
        // Facing east (yaw -90) the seats lie along +x.
        let east = passenger_offset(&b, -90.0, 2, 0, false);
        assert!((east.x - 0.2).abs() < 1e-6, "{east:?}");
    }
}

//! End crystals (`EndCrystal`), dragon fireballs (`DragonFireball` with
//! `AbstractHurtingProjectile`'s flight) and the dragon's breath clouds (`AreaEffectCloud`).

use vanillite_base::{Aabb, BlockPos, Vec3};
use vanillite_data::block::ids;
use vanillite_data::entity::{EntityTypeId, ids as entity_ids};
use vanillite_nbt::{Compound, List, Tag};
use vanillite_world::entity::{Dimensions, EntityId, NewEntity, Rotation};

use super::super::data;
use super::super::special::{SpecialData, SpecialTick};
use super::fight::DragonEvent;
use crate::entities::Entities;
use crate::level::Level;
use crate::mob::physics::{self, Blocks};
use crate::mob::{MobContext, WorldAction, combat};
use crate::survival::damage::{DamageSource, tag};
use crate::survival::player::Env;

/// The kind data of an `end_crystal`.
#[derive(Clone, Debug, PartialEq)]
pub struct Crystal {
    /// `DATA_BEAM_TARGET`.
    pub beam_target: Option<BlockPos>,
    /// `DATA_SHOW_BOTTOM`.
    pub show_bottom: bool,
    /// `time` (client animation; counted like Vanilla).
    pub time: i32,
    /// `invulnerable` (`setPermanentlyInvulnerable`).
    pub invulnerable: bool,
    /// Removed during this tick (`isRemoved`).
    pub removed: bool,
}

impl Default for Crystal {
    fn default() -> Self {
        Crystal {
            beam_target: None,
            show_bottom: true,
            time: 0,
            invulnerable: false,
            removed: false,
        }
    }
}

/// The kind data of a `dragon_fireball`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Fireball {
    pub owner: Option<EntityId>,
    pub owner_uuid: Option<u128>,
    /// `accelerationPower`.
    pub acceleration_power: f64,
    /// `leftOwner`.
    pub left_owner: bool,
    pub tick_count: i32,
}

/// The kind data of an `area_effect_cloud` as the dragon makes them: a dragon's breath
/// particle and an instant damage effect.
#[derive(Clone, Debug, PartialEq)]
pub struct Cloud {
    pub owner: Option<EntityId>,
    pub owner_uuid: Option<u128>,
    pub radius: f32,
    pub waiting: bool,
    pub duration: i32,
    pub wait_time: i32,
    pub reapplication_delay: i32,
    pub duration_on_use: i32,
    pub radius_on_use: f32,
    pub radius_per_tick: f32,
    pub potion_duration_scale: f32,
    /// The instant damage amplifier, `None` without an effect.
    pub harm_amplifier: Option<i32>,
    /// The dragon's breath particle (else the default effect particle).
    pub dragon_breath: bool,
    pub tick_count: i32,
    /// Entities and the tick they may be affected again.
    pub victims: Vec<(EntityId, i32)>,
}

impl Default for Cloud {
    fn default() -> Self {
        Cloud {
            owner: None,
            owner_uuid: None,
            radius: 3.0,
            waiting: false,
            duration: -1,
            wait_time: 20,
            reapplication_delay: 20,
            duration_on_use: 0,
            radius_on_use: 0.0,
            radius_per_tick: 0.0,
            potion_duration_scale: 1.0,
            harm_amplifier: None,
            dragon_breath: false,
            tick_count: 0,
            victims: Vec::new(),
        }
    }
}

impl Cloud {
    /// The cloud of the sitting flame phase.
    pub fn dragon_flame(owner: Option<EntityId>) -> Cloud {
        Cloud {
            owner,
            radius: 5.0,
            duration: 200,
            dragon_breath: true,
            potion_duration_scale: 0.25,
            harm_amplifier: Some(0),
            ..Cloud::default()
        }
    }

    /// The cloud a dragon fireball leaves (`DragonFireball.onHit`).
    pub fn fireball(owner: Option<EntityId>) -> Cloud {
        Cloud {
            owner,
            radius: 3.0,
            duration: 600,
            radius_per_tick: (7.0 - 3.0) / 600.0,
            dragon_breath: true,
            potion_duration_scale: 0.25,
            harm_amplifier: Some(1),
            ..Cloud::default()
        }
    }

    fn dims(&self) -> Dimensions {
        Dimensions {
            width: self.radius * 2.0,
            height: 0.5,
        }
    }
}

/// The UUID of a new non-living entity (its seed from the entity seed stream).
fn new_uuid(_env: &mut Env<'_>) -> u128 {
    crate::mob::new_entity_uuid()
}

/// Adds an end crystal at `pos` (`new EndCrystal(level, x, y, z)`; its yaw is random).
pub fn spawn_crystal(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    pos: Vec3,
    y_rot: f32,
    crystal: Crystal,
) -> EntityId {
    let mut new = NewEntity::new(
        entity_ids::END_CRYSTAL,
        level.network_ids().next(),
        new_uuid(env),
        pos,
    );
    new.rot = Rotation {
        y_rot,
        x_rot: 0.0,
        y_head_rot: y_rot,
    };
    ents.spawn_special(new, SpecialData::EndCrystal(Box::new(crystal)))
}

/// Adds a dragon fireball at `start` flying along `direction` (`new DragonFireball(level, mob,
/// direction)` then `snapTo(start, 0, 0)`).
pub fn spawn_fireball(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    owner: EntityId,
    start: Vec3,
    direction: Vec3,
) -> EntityId {
    let mut new = NewEntity::new(
        entity_ids::DRAGON_FIREBALL,
        level.network_ids().next(),
        new_uuid(env),
        start,
    );
    new.velocity = direction.normalize() * 0.1;
    new.rot = Rotation::default();
    let owner_uuid = ents.store.get(owner).map(|e| e.uuid());
    ents.spawn_special(
        new,
        SpecialData::DragonFireball(Box::new(Fireball {
            owner: Some(owner),
            owner_uuid,
            acceleration_power: 0.1,
            left_owner: false,
            tick_count: 0,
        })),
    )
}

/// Adds an area effect cloud at `pos`.
pub fn spawn_cloud(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    pos: Vec3,
    mut cloud: Cloud,
) -> EntityId {
    cloud.owner_uuid = cloud
        .owner
        .and_then(|o| ents.store.get(o))
        .map(|e| e.uuid());
    let mut new = NewEntity::new(
        entity_ids::AREA_EFFECT_CLOUD,
        level.network_ids().next(),
        new_uuid(env),
        pos,
    );
    new.rot = Rotation::default();
    let dims = cloud.dims();
    let id = ents.spawn_special(new, SpecialData::AreaEffectCloud(Box::new(cloud)));
    if let Some(mut e) = ents.store.get_mut(id) {
        e.set_dims(dims);
    }
    id
}

fn crystal_mut(ents: &mut Entities, id: EntityId) -> Option<&mut Crystal> {
    match ents.special_mut(id)? {
        SpecialData::EndCrystal(c) => Some(c),
        _ => None,
    }
}

/// `EndCrystal.tick`: in the dragon's world the crystal keeps a fire burning below it.
pub fn tick_crystal(t: &mut SpecialTick<'_, '_>) {
    let id = t.id;
    let Some(c) = crystal_mut(t.ents, id) else {
        return;
    };
    c.time += 1;
    if t.cx.dragon_fight.is_none() {
        return;
    }
    let Some(e) = t.ents.store.get(id) else {
        return;
    };
    let p = e.pos();
    let pos = BlockPos::containing(p.x, p.y, p.z);
    if t.level.block(pos).is_air() {
        // `BaseFireBlock.getState`: soul fire on soul fire bases.
        let below = t.level.block(pos.offset(0, -1, 0));
        let fire = if crate::block::block_tags()
            .soul_fire_base_blocks
            .contains(below)
        {
            ids::SOUL_FIRE.default_state()
        } else {
            ids::FIRE.default_state()
        };
        t.cx.actions
            .push(WorldAction::SetBlock(pos, fire, crate::level::update::ALL));
    }
}

/// `EndCrystal.hurtServer`: any hit (but the dragon's) destroys it with an explosion of 6.
pub fn hurt_crystal(
    ents: &mut Entities,
    env: &mut Env<'_>,
    cx: &mut MobContext,
    id: EntityId,
    source: &DamageSource,
) -> bool {
    let Some(e) = ents.store.get(id) else {
        return false;
    };
    let pos = e.pos();
    let creative_cause = source
        .cause
        .and_then(|c| ents.player(c))
        .is_some_and(|p| p.game_mode == vanillite_config::GameMode::Creative);
    let fire_immune = e.kind().info().fire_immune;
    let Some(c) = crystal_mut(ents, id) else {
        return false;
    };
    // `isInvulnerableToBase`.
    if c.removed
        || c.invulnerable
            && !env.types.is(source.kind, tag::BYPASSES_INVULNERABILITY)
            && !creative_cause
        || env.types.is(source.kind, tag::IS_FIRE) && fire_immune
    {
        return false;
    }
    let cause_is_dragon = source.cause.is_some_and(|c| {
        ents.mob(c)
            .is_some_and(|m| m.kind == crate::mob::Kind::EnderDragon)
    });
    if cause_is_dragon {
        return false;
    }
    let Some(c) = crystal_mut(ents, id) else {
        return false;
    };
    c.removed = true;
    cx.discarded.push(id);
    if !env.types.is(source.kind, tag::IS_EXPLOSION) {
        cx.actions.push(WorldAction::ExplodeWith {
            source: Some(id),
            indirect: source.cause,
            pos,
            radius: 6.0,
            blocks: true,
        });
    }
    let uuid = ents.store.get(id).map_or(0, |e| e.uuid());
    cx.dragon_events.push(DragonEvent::CrystalDestroyed {
        crystal: id,
        uuid,
        pos: BlockPos::containing(pos.x, pos.y, pos.z),
        source: *source,
    });
    true
}

/// Where the segment `from`→`to` enters `b`, as a fraction.
fn segment_box(from: Vec3, to: Vec3, b: &Aabb) -> Option<f64> {
    let d = [to.x - from.x, to.y - from.y, to.z - from.z];
    let o = [from.x, from.y, from.z];
    let lo = [b.min_x, b.min_y, b.min_z];
    let hi = [b.max_x, b.max_y, b.max_z];
    let (mut t0, mut t1) = (0.0_f64, 1.0_f64);
    for a in 0..3 {
        if d[a].abs() < 1.0e-12 {
            if o[a] < lo[a] || o[a] > hi[a] {
                return None;
            }
        } else {
            let inv = 1.0 / d[a];
            let (mut n, mut f) = ((lo[a] - o[a]) * inv, (hi[a] - o[a]) * inv);
            if n > f {
                std::mem::swap(&mut n, &mut f);
            }
            t0 = t0.max(n);
            t1 = t1.min(f);
            if t0 > t1 {
                return None;
            }
        }
    }
    Some(t0)
}

fn fireball_mut(ents: &mut Entities, id: EntityId) -> Option<&mut Fireball> {
    match ents.special_mut(id)? {
        SpecialData::DragonFireball(f) => Some(f),
        _ => None,
    }
}

/// Whether a projectile may hit `id` (`canHitEntity`: alive, pickable and not `noPhysics`).
fn can_hit(ents: &Entities, id: EntityId) -> bool {
    if let Some(m) = ents.mob(id) {
        return m.is_alive() && m.kind != crate::mob::Kind::EnderDragon;
    }
    if let Some(p) = ents.player(id) {
        return !p.survival.is_dead_or_dying()
            && p.game_mode != vanillite_config::GameMode::Spectator;
    }
    matches!(ents.special(id), Some(SpecialData::EndCrystal(c)) if !c.removed)
}

/// `AbstractHurtingProjectile.tick` of a dragon fireball.
pub fn tick_fireball(t: &mut SpecialTick<'_, '_>) {
    let id = t.id;
    let Some(e) = t.ents.store.get(id) else {
        return;
    };
    let (pos, aabb, mut v) = (e.pos(), e.aabb(), e.velocity());
    let Some(f) = fireball_mut(t.ents, id) else {
        return;
    };
    f.tick_count += 1;
    let (owner, power, tick_count) = (f.owner, f.acceleration_power, f.tick_count);
    // `applyInertia`.
    let in_water = crate::mob::is_water(t.level.block(BlockPos::containing(pos.x, pos.y, pos.z)));
    let inertia = if in_water { 0.8_f32 } else { 0.95_f32 };
    v = (v + v.normalize() * power) * f64::from(inertia);
    if !t.level.is_loaded(BlockPos::containing(pos.x, pos.y, pos.z)) {
        t.cx.discarded.push(id);
        return;
    }
    let to = pos + v;
    let block_hit = physics::clip(t.level as &dyn Blocks, pos, to);
    let end = block_hit.map_or(to, |(_, p)| p);
    // `getEntityHitResult` with the margin of young projectiles.
    #[allow(clippy::cast_precision_loss)]
    let margin = f64::from(((tick_count - 2) as f32 / 20.0).clamp(0.0, 0.3));
    let area = aabb.expand_towards(v.x, v.y, v.z).inflate(1.0, 1.0, 1.0);
    let mut candidates = Vec::new();
    t.ents.store.for_each_in(&area, false, |other| {
        if other != id {
            candidates.push(other);
        }
    });
    // `checkLeftOwner`.
    let left_owner = fireball_mut(t.ents, id).is_some_and(|f| f.left_owner) || {
        let probe = aabb.expand_towards(v.x, v.y, v.z).inflate(1.0, 1.0, 1.0);
        !owner.is_some_and(|o| {
            t.ents
                .store
                .get(o)
                .is_some_and(|oe| oe.aabb().intersects(&probe))
        })
    };
    let mut best: Option<(EntityId, f64, Vec3)> = None;
    for other in candidates {
        if !can_hit(t.ents, other) || (!left_owner && Some(other) == owner) {
            continue;
        }
        let Some(oe) = t.ents.store.get(other) else {
            continue;
        };
        let grown = oe.aabb().inflate(margin, margin, margin);
        if let Some(frac) = segment_box(pos, end, &grown) {
            let at = pos + (end - pos) * frac;
            let d = pos.distance_to_sqr(at);
            if best.is_none_or(|(_, bd, _)| d < bd) {
                best = Some((other, d, at));
            }
        }
    }
    let hit = best
        .map(|(e, _, at)| (Some(e), at))
        .or(block_hit.map(|(_, at)| (None, at)));
    let new_pos = hit.map_or(to, |(_, at)| at);
    if let Some(mut e) = t.ents.store.get_mut(id) {
        e.set_pos(new_pos);
        e.set_velocity(v);
        // `rotateTowardsMovement(0.2)`.
        let mut rot = e.as_ref().rot();
        let hd = (v.x * v.x + v.z * v.z).sqrt();
        #[allow(clippy::cast_possible_truncation)]
        {
            let x_rot = crate::mob::atan2_deg(v.y, hd) as f32;
            let y_rot = crate::mob::atan2_deg(v.x, v.z) as f32;
            rot.x_rot = lerp_rotation(rot.x_rot, x_rot);
            rot.y_rot = lerp_rotation(rot.y_rot, y_rot);
        }
        e.set_rot(rot);
    }
    if let Some(f) = fireball_mut(t.ents, id) {
        f.left_owner = left_owner;
    }
    if let Some((entity, _)) = hit {
        if entity.is_some() && entity == owner {
            // `ownedBy(entity)`: the owner is never hit.
            return;
        }
        fireball_hit(t, new_pos);
    }
}

/// `Projectile.lerpRotation(0.2)`.
fn lerp_rotation(old: f32, new: f32) -> f32 {
    let mut old = old;
    while new - old < -180.0 {
        old -= 360.0;
    }
    while new - old >= 180.0 {
        old += 360.0;
    }
    old + (new - old) * 0.2
}

/// `DragonFireball.onHit`: a breath cloud at the hit (or at a living entity within 4 blocks).
fn fireball_hit(t: &mut SpecialTick<'_, '_>, pos: Vec3) {
    let id = t.id;
    let owner = fireball_mut(t.ents, id).and_then(|f| f.owner);
    let Some(aabb) = t.ents.store.get(id).map(|e| e.aabb()) else {
        return;
    };
    let area = aabb.inflate(4.0, 2.0, 4.0);
    let mut cloud_pos = pos;
    let mut found = None;
    let ents = &*t.ents;
    ents.store.for_each_in(&area, false, |other| {
        if found.is_some() {
            return;
        }
        let living = ents.mob(other).is_some_and(crate::mob::MobData::is_alive)
            || ents.player(other).is_some();
        if !living {
            return;
        }
        if let Some(oe) = ents.store.get(other)
            && pos.distance_to_sqr(oe.pos()) < 16.0
        {
            found = Some(oe.pos());
        }
    });
    if let Some(p) = found {
        cloud_pos = p;
    }
    let block = BlockPos::containing(pos.x, pos.y, pos.z);
    t.cx.actions.push(WorldAction::LevelEvent(2006, block, 1));
    spawn_cloud(t.level, t.ents, t.env, cloud_pos, Cloud::fireball(owner));
    t.cx.discarded.push(id);
}

fn cloud_mut(ents: &mut Entities, id: EntityId) -> Option<&mut Cloud> {
    match ents.special_mut(id)? {
        SpecialData::AreaEffectCloud(c) => Some(c),
        _ => None,
    }
}

/// Whether instant damage heals `id` (`isInvertedHealAndHarm`: the undead).
fn undead(ents: &Entities, id: EntityId) -> bool {
    ents.mob(id).is_some_and(|m| {
        matches!(
            m.kind,
            crate::mob::Kind::Zombie | crate::mob::Kind::Skeleton | crate::mob::Kind::Wither
        )
    })
}

/// `AreaEffectCloud.serverTick`.
pub fn tick_cloud(t: &mut SpecialTick<'_, '_>) {
    let id = t.id;
    let Some(c) = cloud_mut(t.ents, id) else {
        return;
    };
    c.tick_count += 1;
    if c.duration != -1 && c.tick_count - c.wait_time >= c.duration {
        t.cx.discarded.push(id);
        return;
    }
    let should_wait = c.tick_count < c.wait_time;
    let mut dirty = false;
    if c.waiting != should_wait {
        c.waiting = should_wait;
        dirty = true;
    }
    if should_wait {
        if dirty && let Some(mut e) = t.ents.store.get_mut(id) {
            e.mark_data_dirty();
        }
        return;
    }
    let mut radius = c.radius;
    if c.radius_per_tick != 0.0 {
        radius += c.radius_per_tick;
        if radius < 0.5 {
            t.cx.discarded.push(id);
            return;
        }
        c.radius = radius.clamp(0.0, 32.0);
        dirty = true;
    }
    let dims = c.dims();
    let tick = c.tick_count;
    if dirty && let Some(mut e) = t.ents.store.get_mut(id) {
        e.set_dims(dims);
        e.mark_data_dirty();
    }
    if tick % 5 != 0 {
        return;
    }
    let Some(c) = cloud_mut(t.ents, id) else {
        return;
    };
    c.victims.retain(|&(_, until)| tick < until);
    let Some(amplifier) = c.harm_amplifier else {
        c.victims.clear();
        return;
    };
    let (owner, delay) = (c.owner, c.reapplication_delay);
    let Some(e) = t.ents.store.get(id) else {
        return;
    };
    let (center, area) = (e.pos(), e.aabb());
    let mut inside = Vec::new();
    let ents = &*t.ents;
    ents.store.for_each_in(&area, false, |other| {
        let living = ents.mob(other).is_some_and(|m| !m.is_dead_or_dying())
            || ents
                .player(other)
                .is_some_and(|p| !p.survival.is_dead_or_dying());
        if living {
            inside.push(other);
        }
    });
    for other in inside {
        let Some(op) = t.ents.store.get(other).map(|e| e.pos()) else {
            continue;
        };
        let Some(c) = cloud_mut(t.ents, id) else {
            return;
        };
        if c.victims.iter().any(|&(v, _)| v == other) {
            continue;
        }
        let (xd, zd) = (op.x - center.x, op.z - center.z);
        if xd * xd + zd * zd > f64::from(radius * radius) {
            continue;
        }
        c.victims.push((other, tick + delay));
        // `HealOrHarmMobEffect.applyInstantaneousEffect(cloud, owner, entity, amp, 0.5)`.
        let shift = amplifier.clamp(0, 30);
        if undead(t.ents, other) {
            #[allow(clippy::cast_possible_truncation)]
            let amount = (0.5 * f64::from(4 << shift) + 0.5) as i32;
            if let Some(m) = t.ents.mob_mut(other)
                && m.health > 0.0
            {
                #[allow(clippy::cast_precision_loss)]
                {
                    m.health = (m.health + amount as f32).min(m.max_health());
                }
                if let Some(mut e) = t.ents.store.get_mut(other) {
                    e.mark_data_dirty();
                }
            }
        } else {
            #[allow(clippy::cast_possible_truncation)]
            let amount = (0.5 * f64::from(6 << shift) + 0.5) as i32;
            let cause = owner.filter(|o| t.ents.store.contains(*o)).unwrap_or(id);
            let source = combat::entity_source(t.ents, t.env.types.indirect_magic, cause, id);
            #[allow(clippy::cast_precision_loss)]
            combat::hurt(t.level, t.ents, t.env, t.cx, other, source, amount as f32);
        }
        let Some(c) = cloud_mut(t.ents, id) else {
            return;
        };
        if c.radius_on_use != 0.0 {
            radius += c.radius_on_use;
            if radius < 0.5 {
                t.cx.discarded.push(id);
                return;
            }
            c.radius = radius;
        }
        if c.duration_on_use != 0 && c.duration != -1 {
            c.duration += c.duration_on_use;
            if c.duration <= 0 {
                t.cx.discarded.push(id);
                return;
            }
        }
    }
}

/// The registry id of a particle type.
fn particle_id(name: &str) -> i32 {
    vanillite_data::registry::PARTICLE_TYPE
        .id(name)
        .map_or(0, i32::from)
}

/// The synched data of the specials: crystals (beam target 8, show bottom 9) and clouds
/// (radius 8, waiting 9, particle 10).
pub fn entity_data(out: &mut Vec<u8>, d: &SpecialData, all: bool) {
    match d {
        SpecialData::EndCrystal(c) => {
            if all || c.beam_target.is_some() {
                data::item(out, 8, data::OPTIONAL_BLOCK_POS, |o| match c.beam_target {
                    Some(p) => {
                        o.push(1);
                        o.extend_from_slice(&p.as_long().to_be_bytes());
                    }
                    None => o.push(0),
                });
            }
            data::boolean(out, 9, c.show_bottom, true, all);
        }
        SpecialData::AreaEffectCloud(c) => {
            data::float(out, 8, c.radius, 3.0, all);
            data::boolean(out, 9, c.waiting, false, all);
            if all || c.dragon_breath {
                data::item(out, 10, data::PARTICLE, |o| {
                    if c.dragon_breath {
                        vanillite_protocol::codec::write_varint(
                            o,
                            particle_id("minecraft:dragon_breath"),
                        );
                        o.extend_from_slice(&1.0_f32.to_be_bytes());
                    } else {
                        vanillite_protocol::codec::write_varint(
                            o,
                            particle_id("minecraft:entity_effect"),
                        );
                        o.extend_from_slice(&(-1_i32).to_be_bytes());
                    }
                });
            }
        }
        _ => {}
    }
}

fn uuid_tag(uuid: u128) -> Tag {
    #[allow(clippy::cast_possible_truncation)]
    Tag::IntArray(vec![
        (uuid >> 96) as i32,
        (uuid >> 64) as i32,
        (uuid >> 32) as i32,
        uuid as i32,
    ])
}

fn uuid_of(tag: Option<&Tag>) -> Option<u128> {
    match tag {
        Some(Tag::IntArray(a)) if a.len() == 4 => Some(
            a.iter()
                .fold(0_u128, |acc, &v| (acc << 32) | u128::from(v as u32)),
        ),
        _ => None,
    }
}

fn int(tag: &Compound, key: &str, default: i32) -> i32 {
    tag.get(key)
        .and_then(Tag::as_i64)
        .and_then(|v| i32::try_from(v).ok())
        .unwrap_or(default)
}

fn float(tag: &Compound, key: &str, default: f32) -> f32 {
    match tag.get(key) {
        Some(Tag::Float(f)) if f.is_finite() => *f,
        _ => default,
    }
}

/// The entity NBT of the specials' kind data.
pub fn save(d: &SpecialData, out: &mut Compound) -> Option<()> {
    match d {
        SpecialData::EndCrystal(c) => {
            if c.removed {
                return None;
            }
            if let Some(p) = c.beam_target {
                out.insert("beam_target", Tag::IntArray(vec![p.x, p.y, p.z]));
            }
            out.insert("ShowBottom", Tag::Byte(i8::from(c.show_bottom)));
            out.insert("Invulnerable", Tag::Byte(i8::from(c.invulnerable)));
        }
        SpecialData::DragonFireball(f) => {
            if let Some(u) = f.owner_uuid {
                out.insert("Owner", uuid_tag(u));
            }
            out.insert("leftOwner", Tag::Byte(i8::from(f.left_owner)));
            out.insert("acceleration_power", Tag::Double(f.acceleration_power));
        }
        SpecialData::AreaEffectCloud(c) => {
            out.insert("Age", Tag::Int(c.tick_count));
            out.insert("Duration", Tag::Int(c.duration));
            out.insert("WaitTime", Tag::Int(c.wait_time));
            out.insert("ReapplicationDelay", Tag::Int(c.reapplication_delay));
            out.insert("DurationOnUse", Tag::Int(c.duration_on_use));
            out.insert("RadiusOnUse", Tag::Float(c.radius_on_use));
            out.insert("RadiusPerTick", Tag::Float(c.radius_per_tick));
            out.insert("Radius", Tag::Float(c.radius));
            if c.dragon_breath {
                let mut p = Compound::new();
                p.insert("type", Tag::String("minecraft:dragon_breath".into()));
                p.insert("power", Tag::Float(1.0));
                out.insert("custom_particle", Tag::Compound(p));
            }
            if let Some(u) = c.owner_uuid {
                out.insert("Owner", uuid_tag(u));
            }
            if let Some(amp) = c.harm_amplifier {
                let mut e = Compound::new();
                e.insert("id", Tag::String("minecraft:instant_damage".into()));
                if amp != 0 {
                    e.insert("amplifier", Tag::Byte(i8::try_from(amp).unwrap_or(0)));
                }
                e.insert("duration", Tag::Int(1));
                let mut pc = Compound::new();
                pc.insert("custom_effects", Tag::List(List(vec![Tag::Compound(e)])));
                out.insert("potion_contents", Tag::Compound(pc));
            }
            if c.potion_duration_scale != 1.0 {
                out.insert("potion_duration_scale", Tag::Float(c.potion_duration_scale));
            }
        }
        SpecialData::WitherSkull(_) => {}
    }
    Some(())
}

/// Reads the kind data of a crystal, fireball or cloud.
pub fn load(kind: EntityTypeId, tag: &Compound) -> Option<SpecialData> {
    if kind == entity_ids::END_CRYSTAL {
        let beam_target = match tag.get("beam_target") {
            Some(Tag::IntArray(a)) if a.len() == 3 => Some(BlockPos::new(a[0], a[1], a[2])),
            _ => None,
        };
        let flag = |k: &str, d: bool| tag.get(k).and_then(Tag::as_i64).map_or(d, |v| v != 0);
        Some(SpecialData::EndCrystal(Box::new(Crystal {
            beam_target,
            show_bottom: flag("ShowBottom", true),
            time: 0,
            invulnerable: flag("Invulnerable", false),
            removed: false,
        })))
    } else if kind == entity_ids::DRAGON_FIREBALL {
        let power = match tag.get("acceleration_power") {
            Some(Tag::Double(d)) if d.is_finite() => *d,
            _ => 0.1,
        };
        Some(SpecialData::DragonFireball(Box::new(Fireball {
            owner: None,
            owner_uuid: uuid_of(tag.get("Owner")),
            acceleration_power: power,
            left_owner: tag
                .get("leftOwner")
                .and_then(Tag::as_i64)
                .is_some_and(|v| v != 0),
            tick_count: 0,
        })))
    } else if kind == entity_ids::AREA_EFFECT_CLOUD {
        let dragon_breath = matches!(
            tag.get("custom_particle"),
            Some(Tag::Compound(p)) if matches!(p.get("type"), Some(Tag::String(s)) if s == "minecraft:dragon_breath")
        );
        let harm_amplifier = match tag.get("potion_contents") {
            Some(Tag::Compound(pc)) => match pc.get("custom_effects") {
                Some(Tag::List(l)) => l.0.iter().find_map(|e| match e {
                    Tag::Compound(e)
                        if matches!(e.get("id"), Some(Tag::String(s)) if s == "minecraft:instant_damage") =>
                    {
                        Some(int(e, "amplifier", 0))
                    }
                    _ => None,
                }),
                _ => None,
            },
            _ => None,
        };
        Some(SpecialData::AreaEffectCloud(Box::new(Cloud {
            owner: None,
            owner_uuid: uuid_of(tag.get("Owner")),
            radius: float(tag, "Radius", 3.0).clamp(0.0, 32.0),
            waiting: false,
            duration: int(tag, "Duration", -1),
            wait_time: int(tag, "WaitTime", 20),
            reapplication_delay: int(tag, "ReapplicationDelay", 20),
            duration_on_use: int(tag, "DurationOnUse", 0),
            radius_on_use: float(tag, "RadiusOnUse", 0.0),
            radius_per_tick: float(tag, "RadiusPerTick", 0.0),
            potion_duration_scale: float(tag, "potion_duration_scale", 1.0),
            harm_amplifier,
            dragon_breath,
            tick_count: int(tag, "Age", 0),
            victims: Vec::new(),
        })))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn harm_amounts() {
        // `(int)(0.5 * (6 << amplifier) + 0.5)`.
        let amount = |amp: i32| (0.5 * f64::from(6 << amp) + 0.5) as i32;
        assert_eq!(amount(0), 3);
        assert_eq!(amount(1), 6);
    }

    #[test]
    fn cloud_nbt_round_trip() {
        let mut out = Compound::new();
        let cloud = Cloud::fireball(None);
        save(
            &SpecialData::AreaEffectCloud(Box::new(cloud.clone())),
            &mut out,
        );
        let Some(SpecialData::AreaEffectCloud(back)) = load(entity_ids::AREA_EFFECT_CLOUD, &out)
        else {
            panic!("cloud");
        };
        assert_eq!(back.harm_amplifier, Some(1));
        assert!(back.dragon_breath);
        assert_eq!(back.duration, 600);
        assert_eq!(back.radius_per_tick, cloud.radius_per_tick);
    }

    #[test]
    fn crystal_nbt_round_trip() {
        let mut out = Compound::new();
        let c = Crystal {
            beam_target: Some(BlockPos::new(0, 128, 0)),
            show_bottom: false,
            invulnerable: true,
            ..Crystal::default()
        };
        save(&SpecialData::EndCrystal(Box::new(c.clone())), &mut out);
        let Some(SpecialData::EndCrystal(back)) = load(entity_ids::END_CRYSTAL, &out) else {
            panic!("crystal");
        };
        assert_eq!(*back, c);
    }
}

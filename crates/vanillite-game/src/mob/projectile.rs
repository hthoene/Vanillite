//! Arrows shot by skeletons (`AbstractArrow`) and experience orbs (`ExperienceOrb`).

use vanillite_base::math::ceil;
use vanillite_base::random::{LegacyRandom, RandomSource};
use vanillite_base::{Aabb, BlockPos, Vec3};
use vanillite_data::block::BlockState;
use vanillite_data::entity::ids as entity_ids;
use vanillite_protocol::packets::play::clientbound::SoundSource;
use vanillite_world::entity::{EntityFlags, EntityId, NewEntity, Rotation};

use super::physics::{self, Blocks};
use super::{MobContext, Tick, atan2_deg, combat, random_uuid};
use crate::entities::Entities;
use crate::level::Level;
use crate::survival::play_sound;
use crate::survival::player::Env;
use crate::survival::rules::Difficulty;

/// The kind data of an arrow.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ArrowData {
    pub owner: Option<EntityId>,
    pub base_damage: f64,
    pub in_ground: bool,
    pub life: i32,
    pub shake: i32,
    pub crit: bool,
    /// The arrow left its owner's box (`leftOwner`).
    pub left_owner: bool,
    pub last_state: Option<BlockState>,
    pub tick_count: i32,
    /// `pickup`: 0 disallowed, 1 allowed, 2 creative only.
    pub pickup: u8,
    pub random_seed: i64,
}

/// The kind data of an experience orb.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OrbData {
    pub value: i32,
    pub count: i32,
    pub age: i32,
    pub tick_count: i32,
    pub following: Option<EntityId>,
    pub random_seed: i64,
}

/// `startUsingItem` with the bow: the "using item" living flag is synched.
pub fn start_using_item(t: &mut Tick<'_, '_>) {
    t.mob().using_item = 0;
    t.core_mut().mark_data_dirty();
}

/// `stopUsingItem`.
pub fn stop_using_item(t: &mut Tick<'_, '_>) {
    if t.mob_ref().using_item >= 0 {
        t.mob().using_item = -1;
        t.core_mut().mark_data_dirty();
    }
}

/// `Projectile.getMovementToShoot` and `shoot`: the velocity and rotation of a shot.
fn shoot(random: &mut LegacyRandom, dir: Vec3, power: f32, uncertainty: f32) -> (Vec3, Rotation) {
    let spread = 0.017_227_5 * f64::from(uncertainty);
    let n = dir.normalize();
    let v = Vec3::new(
        n.x + random.triangle(0.0, spread),
        n.y + random.triangle(0.0, spread),
        n.z + random.triangle(0.0, spread),
    ) * f64::from(power);
    let sd = (v.x * v.x + v.z * v.z).sqrt();
    #[allow(clippy::cast_possible_truncation)]
    let rot = Rotation {
        y_rot: atan2_deg(v.x, v.z) as f32,
        x_rot: atan2_deg(v.y, sd) as f32,
        y_head_rot: 0.0,
    };
    (v, rot)
}

/// `AbstractSkeleton.performRangedAttack`.
pub fn skeleton_shoot(t: &mut Tick<'_, '_>, target: EntityId, power: f32) {
    let Some(o) = t.other(target) else {
        return;
    };
    let p = t.pos();
    let start = Vec3::new(p.x, t.eye_y() - 0.1, p.z);
    let difficulty = t.env.difficulty;
    let id = t.id;
    let m = t.mob();
    // `setBaseDamageFromMob`.
    #[allow(clippy::cast_precision_loss)]
    let base_damage = f64::from(power) * 2.0
        + m.random
            .triangle(f64::from(difficulty as i32) * 0.11, 0.574_25);
    let xd = o.pos.x - p.x;
    let target_y = o.aabb.min_y + (o.aabb.max_y - o.aabb.min_y) / 3.0;
    let yd = target_y - start.y;
    let zd = o.pos.z - p.z;
    let dist = (xd * xd + zd * zd).sqrt();
    #[allow(clippy::cast_precision_loss)]
    let uncertainty = (14 - difficulty as i32 * 4) as f32;
    let (velocity, rot) = shoot(
        &mut m.random,
        Vec3::new(xd, yd + dist * f64::from(0.2_f32), zd),
        1.6,
        uncertainty,
    );
    let seed = m.random.next_long();
    let uuid = random_uuid(&mut m.random);
    let crit = false;
    let mut new = NewEntity::new(entity_ids::ARROW, t.level.network_ids().next(), uuid, start);
    new.velocity = velocity;
    new.rot = rot;
    new.flags = EntityFlags::HAS_IMPULSE;
    t.ents.spawn_arrow(
        new,
        ArrowData {
            owner: Some(id),
            base_damage,
            in_ground: false,
            life: 0,
            shake: 0,
            crit,
            left_owner: false,
            last_state: None,
            tick_count: 0,
            pickup: 0,
            random_seed: seed,
        },
    );
    let m = t.mob();
    let pitch = 1.0 / (m.random.next_float() * 0.4 + 0.8);
    super::kinds::play_sound(t, "minecraft:entity.skeleton.shoot", 1.0, pitch);
    let _ = Difficulty::Peaceful;
}

/// `AbstractArrow.tick` (with `Projectile.tick`'s owner check).
pub fn tick_arrow(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    cx: &mut MobContext,
    id: EntityId,
) {
    let Some(mut a) = ents.arrow(id).copied() else {
        return;
    };
    let Some(e) = ents.store.get(id) else {
        return;
    };
    let mut pos = e.pos();
    let mut velocity = e.velocity();
    let mut rot = e.rot();
    let mut random = LegacyRandom::new(a.random_seed);
    a.tick_count += 1;
    let block_pos = BlockPos::containing(pos.x, pos.y, pos.z);
    let state = level.block(block_pos);
    if !state.is_air() {
        for b in state.collision_shape() {
            let shape = Aabb::new(
                f64::from(block_pos.x) + b[0],
                f64::from(block_pos.y) + b[1],
                f64::from(block_pos.z) + b[2],
                f64::from(block_pos.x) + b[3],
                f64::from(block_pos.y) + b[4],
                f64::from(block_pos.z) + b[5],
            );
            if shape.contains(pos.x, pos.y, pos.z) {
                velocity = Vec3::ZERO;
                a.in_ground = true;
                break;
            }
        }
    }
    if a.shake > 0 {
        a.shake -= 1;
    }
    let mut discard = false;
    if a.in_ground {
        let falls = a.last_state != Some(state) && {
            let probe =
                Aabb::new(pos.x, pos.y, pos.z, pos.x, pos.y, pos.z).inflate(0.06, 0.06, 0.06);
            physics::no_collision(level, &probe, &mut cx.boxes)
        };
        if falls {
            a.in_ground = false;
            velocity = velocity.multiply(
                f64::from(random.next_float() * 0.2),
                f64::from(random.next_float() * 0.2),
                f64::from(random.next_float() * 0.2),
            );
            a.life = 0;
        } else {
            a.life += 1;
            if a.life >= 1200 {
                discard = true;
            }
        }
    } else {
        let in_water = super::is_water(state);
        if in_water {
            velocity = velocity * f64::from(0.6_f32);
        }
        #[allow(clippy::cast_possible_truncation)]
        let y_rot = atan2_deg(velocity.x, velocity.z) as f32;
        let hd = (velocity.x * velocity.x + velocity.z * velocity.z).sqrt();
        #[allow(clippy::cast_possible_truncation)]
        let x_rot = atan2_deg(velocity.y, hd) as f32;
        rot.x_rot = lerp_rotation(rot.x_rot, x_rot);
        rot.y_rot = lerp_rotation(rot.y_rot, y_rot);
        // `checkLeftOwner`.
        if !a.left_owner {
            let aabb = e
                .aabb()
                .expand_towards(velocity.x, velocity.y, velocity.z)
                .inflate(1.0, 1.0, 1.0);
            let still_inside = a.owner.is_some_and(|o| {
                ents.store
                    .get(o)
                    .is_some_and(|oe| oe.aabb().intersects(&aabb))
            });
            a.left_owner = !still_inside;
        }
        let to = pos + velocity;
        let block_hit = physics::clip(level as &dyn Blocks, pos, to);
        let end = block_hit.map_or(to, |(_, p)| p);
        // `findHitEntities`: the first living entity whose box (grown by 0.3) the path hits.
        let area = e
            .aabb()
            .expand_towards(velocity.x, velocity.y, velocity.z)
            .inflate(1.0, 1.0, 1.0);
        let mut best: Option<(EntityId, f64, Vec3)> = None;
        ents.store.for_each_in(&area, false, |other| {
            if other == id {
                return;
            }
            if !a.left_owner && a.owner == Some(other) {
                return;
            }
            let alive = ents.mob(other).is_some_and(super::MobData::is_alive)
                || ents.player(other).is_some_and(|p| {
                    !p.survival.is_dead_or_dying()
                        && p.game_mode != vanillite_config::GameMode::Spectator
                });
            if !alive {
                return;
            }
            let Some(oe) = ents.store.get(other) else {
                return;
            };
            let grown = oe.aabb().inflate(0.3, 0.3, 0.3);
            if let Some(t) = segment_box(pos, end, &grown) {
                let hit = pos + (end - pos) * t;
                let d = pos.distance_to_sqr(oe.pos());
                if best.is_none_or(|(_, bd, _)| d < bd) {
                    best = Some((other, d, hit));
                }
            }
        });
        if let Some((target, _, hit)) = best {
            pos = hit;
            // `onHitEntity`.
            let pow = velocity.length_sqr().sqrt();
            #[allow(clippy::cast_possible_truncation)]
            let mut damage = ceil((pow * a.base_damage).clamp(0.0, 2.147_483_647e9));
            if a.crit {
                let inc = random.next_int_bounded(damage / 2 + 2);
                damage = damage.saturating_add(inc);
            }
            let owner = a.owner.filter(|o| ents.store.contains(*o)).unwrap_or(id);
            let mut source = combat::entity_source(ents, env.types.arrow, owner, id);
            source.position = Some(pos);
            #[allow(clippy::cast_precision_loss)]
            let hurt = combat::hurt(level, ents, env, cx, target, source, damage as f32);
            if hurt {
                if let Some(owner) = a.owner
                    && let Some(m) = ents.mob_mut(owner)
                {
                    m.last_hurt_mob = Some(target);
                }
                let pitch = 1.2 / (random.next_float() * 0.2 + 0.9);
                let seed = random.next_long();
                play_sound(
                    env.out,
                    None,
                    pos,
                    "minecraft:entity.arrow.hit",
                    SoundSource::Neutral,
                    1.0,
                    pitch,
                    seed,
                );
                discard = true;
            } else {
                // Deflected (`ProjectileDeflection.REVERSE`).
                velocity = velocity * -0.2;
                if velocity.length_sqr() < 1.0e-7 {
                    discard = true;
                }
            }
        } else if let Some((hit_pos, at)) = block_hit {
            // `onHitBlock`.
            let sign = |v: f64| {
                if v > 0.0 {
                    1.0
                } else if v < 0.0 {
                    -1.0
                } else {
                    0.0
                }
            };
            let offset = Vec3::new(sign(velocity.x), sign(velocity.y), sign(velocity.z))
                * f64::from(0.05_f32);
            pos = at - offset;
            velocity = Vec3::ZERO;
            a.last_state = Some(level.block(hit_pos));
            a.in_ground = true;
            a.shake = 7;
            a.crit = false;
            let pitch = 1.2 / (random.next_float() * 0.2 + 0.9);
            let seed = random.next_long();
            play_sound(
                env.out,
                None,
                pos,
                "minecraft:entity.arrow.hit",
                SoundSource::Neutral,
                1.0,
                pitch,
                seed,
            );
        } else {
            pos = to;
        }
        if !super::is_water(level.block(BlockPos::containing(pos.x, pos.y, pos.z))) {
            velocity = velocity * 0.99;
        }
        if !a.in_ground {
            velocity.y -= 0.05;
        }
    }
    a.random_seed = random.next_long();
    if discard || !ents.store.contains(id) {
        cx.discarded.push(id);
        return;
    }
    if let Some(mut e) = ents.store.get_mut(id) {
        e.set_pos(pos);
        e.set_velocity(velocity);
        e.set_rot(rot);
        e.mark_data_dirty();
    }
    if let Some(d) = ents.arrow_mut(id) {
        *d = a;
    }
}

/// `Projectile.lerpRotation`.
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

/// Where the segment enters `b`, as a fraction of the segment.
fn segment_box(from: Vec3, to: Vec3, b: &Aabb) -> Option<f64> {
    if b.contains(from.x, from.y, from.z) {
        return Some(0.0);
    }
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

/// Adds an experience orb of `value` at `pos`, or merges it into an equal orb nearby
/// (`tryMergeToExisting`).
pub fn add_experience_orb(
    level: &Level,
    ents: &mut Entities,
    cx: &mut MobContext,
    pos: Vec3,
    value: i32,
) {
    let area = Aabb::new(
        pos.x - 0.5,
        pos.y - 0.5,
        pos.z - 0.5,
        pos.x + 0.5,
        pos.y + 0.5,
        pos.z + 0.5,
    );
    let mut random =
        LegacyRandom::new(level.game_time() ^ i64::from(value) ^ pos.x.to_bits() as i64);
    let pick = random.next_int_bounded(40);
    let mut merged = None;
    ents.store.for_each_in(&area, false, |other| {
        if merged.is_some() {
            return;
        }
        let Some(o) = ents.orb(other) else {
            return;
        };
        let Some(e) = ents.store.get(other) else {
            return;
        };
        if (e.network_id().0 - pick) % 40 == 0 && o.value == value {
            merged = Some(other);
        }
    });
    if let Some(other) = merged
        && let Some(o) = ents.orb_mut(other)
    {
        o.count += 1;
        o.age = 0;
        return;
    }
    let mut new = NewEntity::new(
        entity_ids::EXPERIENCE_ORB,
        level.network_ids().next(),
        random_uuid(&mut random),
        pos,
    );
    #[allow(clippy::cast_possible_truncation)]
    {
        new.rot.y_rot = (random.next_double() * 360.0) as f32;
    }
    new.velocity = Vec3::new(
        (random.next_double() * f64::from(0.2_f32) - f64::from(0.1_f32)) * 2.0,
        random.next_double() * 0.2 * 2.0,
        (random.next_double() * f64::from(0.2_f32) - f64::from(0.1_f32)) * 2.0,
    );
    let _ = cx;
    ents.spawn_orb(
        new,
        OrbData {
            value,
            count: 1,
            age: 0,
            tick_count: 0,
            following: None,
            random_seed: random.next_long(),
        },
    );
}

/// `ExperienceOrb.tick`, with the pickup by touching players (`playerTouch`).
pub fn tick_orb(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    cx: &mut MobContext,
    id: EntityId,
) {
    let Some(mut o) = ents.orb(id).copied() else {
        return;
    };
    let Some(e) = ents.store.get(id) else {
        return;
    };
    let mut random = LegacyRandom::new(o.random_seed);
    o.tick_count += 1;
    let mut pos = e.pos();
    let aabb = e.aabb();
    let mut velocity = e.velocity();
    let colliding = !physics::no_collision(level, &aabb, &mut cx.boxes);
    let eye_in_water =
        super::is_water(level.block(BlockPos::containing(pos.x, pos.y + 0.0, pos.z)));
    if eye_in_water {
        velocity = Vec3::new(
            velocity.x * f64::from(0.99_f32),
            (velocity.y + f64::from(5.0e-4_f32)).min(f64::from(0.06_f32)),
            velocity.z * f64::from(0.99_f32),
        );
    } else if !colliding {
        velocity.y -= 0.03;
    }
    if super::is_lava(level.block(BlockPos::containing(pos.x, pos.y, pos.z))) {
        velocity = Vec3::new(
            f64::from((random.next_float() - random.next_float()) * 0.2),
            f64::from(0.2_f32),
            f64::from((random.next_float() - random.next_float()) * 0.2),
        );
    }
    if o.tick_count % 20 == 1 {
        // `scanForMerges`.
        let area = aabb.inflate(0.5, 0.5, 0.5);
        let mut others = Vec::new();
        ents.store.for_each_in(&area, false, |other| {
            if other != id && ents.orb(other).is_some_and(|x| x.value == o.value) {
                others.push(other);
            }
        });
        for other in others {
            if let Some(x) = ents.orb(other).copied() {
                o.count += x.count;
                o.age = o.age.min(x.age);
                cx.discarded.push(other);
                if let Some(x) = ents.orb_mut(other) {
                    x.count = 0;
                }
            }
        }
    }
    // `followNearbyPlayer`.
    let follow_valid = o
        .following
        .and_then(|p| ents.store.get(p))
        .is_some_and(|p| p.pos().distance_to_sqr(pos) <= 64.0);
    if !follow_valid {
        let r = 8.0;
        let area = Aabb::new(
            pos.x - r,
            pos.y - r,
            pos.z - r,
            pos.x + r,
            pos.y + r,
            pos.z + r,
        );
        let mut nearest: Option<(EntityId, f64)> = None;
        ents.store.for_each_in(&area, true, |p| {
            let ok = ents.player(p).is_some_and(|d| {
                !d.survival.is_dead_or_dying()
                    && d.game_mode != vanillite_config::GameMode::Spectator
            });
            if !ok {
                return;
            }
            if let Some(pe) = ents.store.get(p) {
                let d = pe.pos().distance_to_sqr(pos);
                if d <= 64.0 && nearest.is_none_or(|(_, b)| d < b) {
                    nearest = Some((p, d));
                }
            }
        });
        o.following = nearest.map(|(p, _)| p);
    }
    if let Some(pe) = o.following.and_then(|p| ents.store.get(p)) {
        let eye = f64::from(pe.kind().info().eye_height);
        let delta = Vec3::new(
            pe.pos().x - pos.x,
            pe.pos().y + eye / 2.0 - pos.y,
            pe.pos().z - pos.z,
        );
        let power = 1.0 - delta.length_sqr().sqrt() / 8.0;
        velocity = velocity + delta.normalize() * (power * power * 0.1);
    }
    let fall_speed = velocity.y;
    let moved = physics::collide(level, aabb, velocity, 0.0, false, &mut cx.boxes);
    pos = pos + moved;
    let below = moved.y != velocity.y && velocity.y < 0.0;
    if moved.x != velocity.x {
        velocity.x = 0.0;
    }
    if moved.z != velocity.z {
        velocity.z = 0.0;
    }
    if moved.y != velocity.y {
        velocity.y = 0.0;
    }
    let mut friction = 0.98_f32;
    if below {
        let under = level.block(BlockPos::containing(pos.x, pos.y - 0.500_001, pos.z));
        friction *= under.block().info().friction;
    }
    velocity = velocity * f64::from(friction);
    if below && fall_speed < -0.03 {
        velocity.y = -fall_speed * 0.4;
    }
    o.age += 1;
    o.random_seed = random.next_long();
    let mut discard = o.age >= 6000 || o.count <= 0;
    if let Some(mut e) = ents.store.get_mut(id) {
        e.set_pos(pos);
        e.set_velocity(velocity);
        e.set_flags(EntityFlags::ON_GROUND, below);
    }
    // `playerTouch`: players whose grown box touches the orb take one orb's worth.
    if !discard {
        let orb_box = ents.store.get(id).map(|e| e.aabb());
        if let Some(orb_box) = orb_box {
            let area = orb_box.inflate(1.0, 1.0, 1.0);
            let mut touching = Vec::new();
            ents.store.for_each_in(&area, true, |p| touching.push(p));
            for p in touching {
                let Some(pe) = ents.store.get(p) else {
                    continue;
                };
                if !pe.aabb().inflate(1.0, 0.5, 1.0).intersects(&orb_box) {
                    continue;
                }
                let ppos = pe.pos();
                let Some(data) = ents.player_mut(p) else {
                    continue;
                };
                if data.survival.is_dead_or_dying()
                    || data.game_mode == vanillite_config::GameMode::Spectator
                    || data.survival.take_xp_delay != 0
                {
                    continue;
                }
                data.survival.take_xp_delay = 2;
                data.survival.xp.give_points(o.value);
                let pitch = (random.next_float() - random.next_float()) * 0.35 + 0.9;
                let seed = random.next_long();
                play_sound(
                    env.out,
                    None,
                    ppos,
                    "minecraft:entity.experience_orb.pickup",
                    SoundSource::Players,
                    0.1,
                    pitch,
                    seed,
                );
                o.count -= 1;
                if o.count <= 0 {
                    discard = true;
                    break;
                }
            }
        }
    }
    if discard {
        cx.discarded.push(id);
    }
    if let Some(d) = ents.orb_mut(id) {
        *d = o;
    }
}

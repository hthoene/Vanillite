//! Arrows shot by skeletons (`AbstractArrow`).

use vanillite_base::math::ceil;
use vanillite_base::random::{LegacyRandom, RandomSource};
use vanillite_base::{Aabb, BlockPos, Vec3};
use vanillite_data::block::BlockState;
use vanillite_data::entity::ids as entity_ids;
use vanillite_protocol::packets::play::clientbound::SoundSource;
use vanillite_world::entity::{EntityFlags, EntityId, NewEntity, Rotation};

use super::physics::{self, Blocks};
use super::{MobContext, Tick, atan2_deg, combat};
use crate::entities::Entities;
use crate::level::Level;
use crate::survival::play_sound;
use crate::survival::player::Env;
use crate::survival::rules::Difficulty;

/// The kind data of an arrow.
#[derive(Clone, Debug, PartialEq)]
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
    /// The state of its own random ([`LegacyRandom::state`]).
    pub random_state: i64,
    /// The knockback of the weapon's enchantments (Punch).
    pub knockback: f32,
    /// Burning (Flame): sets what it hits on fire for 5 seconds.
    pub fire: bool,
    /// The effect of a tipped arrow (`Arrow.addEffect`; strays, bogged and parched shoot
    /// them), applied to the living entity it hurts.
    pub effect: Option<crate::effect::EffectInstance>,
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
pub fn shoot(
    random: &mut LegacyRandom,
    dir: Vec3,
    power: f32,
    uncertainty: f32,
) -> (Vec3, Rotation) {
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

/// `BowItem.releaseUsing` / `ProjectileWeaponItem.shoot` of one arrow: spawns it at `start`
/// flying along `dir` with `power` and `uncertainty`, with the weapon's enchantments (Power
/// adds damage, Punch knockback, Flame fire). Dispensers and other shooters use it too.
#[allow(clippy::too_many_arguments)]
pub fn shoot_arrow(
    ents: &mut Entities,
    network_ids: &vanillite_world::entity::NetworkIds,
    random: &mut LegacyRandom,
    start: Vec3,
    dir: Vec3,
    power: f32,
    uncertainty: f32,
    owner: Option<EntityId>,
    weapon: Option<&crate::item::ItemStack>,
    crit: bool,
    pickup: u8,
) -> EntityId {
    // The arrow's own random (its constructor), then `shoot` draws the spread from it.
    let mut own = LegacyRandom::new(crate::mob::new_entity_seed());
    let uuid = crate::mob::new_uuid();
    let (velocity, rot) = shoot(&mut own, dir, power, uncertainty);
    let mut data = ArrowData {
        owner,
        base_damage: 2.0,
        in_ground: false,
        life: 0,
        shake: 0,
        crit,
        left_owner: false,
        last_state: None,
        tick_count: 0,
        pickup,
        random_state: own.state(),
        knockback: 0.0,
        fire: false,
        effect: None,
    };
    if let Some(w) = weapon {
        let combat = super::attack::combat_data();
        let (enchants, tags): (
            &crate::enchant::Enchantments,
            &dyn crate::predicate::EntityTags,
        ) = (&combat.enchants, &*combat);
        let arrow = crate::predicate::EntityFacts {
            kind: Some(entity_ids::ARROW),
            ..crate::predicate::EntityFacts::default()
        };
        let types = crate::survival::damage::DamageTypes::built_in();
        let cx = crate::enchant::EnchantContext {
            types: &types,
            tags,
            this: None,
            attacker: owner.and_then(|o| super::attack::facts(ents, o)),
            direct_attacker: Some(arrow),
            attacking_player: None,
            source: None,
        };
        #[allow(clippy::cast_possible_truncation)]
        let dmg = enchants.modify_damage(w, &cx, random, data.base_damage as f32);
        data.base_damage = f64::from(dmg);
        data.knockback = enchants.modify_knockback(w, &cx, random, 0.0);
        data.fire = enchants.projectile_ignite(w).is_some();
    }
    let mut new = NewEntity::new(entity_ids::ARROW, network_ids.next(), uuid, start);
    new.velocity = velocity;
    new.rot = rot;
    new.flags = EntityFlags::HAS_IMPULSE;
    if data.fire {
        new.flags.set(EntityFlags::ON_FIRE, true);
    }
    ents.spawn_arrow(new, data)
}

/// `AbstractSkeleton.performRangedAttack`.
pub fn skeleton_shoot(t: &mut Tick<'_, '_>, target: EntityId, power: f32) {
    let Some(o) = t.other(target) else {
        return;
    };
    let p = t.pos();
    // `getEyeY() - 0.1F`.
    let start = Vec3::new(p.x, t.eye_y() - f64::from(0.1_f32), p.z);
    let difficulty = t.env.difficulty;
    let id = t.id;
    let m = t.mob();
    // The arrow's own random (its constructor); `setBaseDamageFromMob` and `shoot` draw from
    // it, not from the skeleton's.
    let mut own = LegacyRandom::new(super::new_entity_seed());
    #[allow(clippy::cast_precision_loss)]
    let base_damage =
        f64::from(power) * 2.0 + own.triangle(f64::from(difficulty as i32) * 0.11, 0.574_25);
    let xd = o.pos.x - p.x;
    // `target.getY(0.3333333333333333)`.
    let target_y = o.pos.y + f64::from(o.height) * 0.333_333_333_333_333_3;
    let yd = target_y - start.y;
    let zd = o.pos.z - p.z;
    let dist = (xd * xd + zd * zd).sqrt();
    #[allow(clippy::cast_precision_loss)]
    let uncertainty = (14 - difficulty as i32 * 4) as f32;
    let (velocity, rot) = shoot(
        &mut own,
        Vec3::new(xd, yd + dist * f64::from(0.2_f32), zd),
        1.6,
        uncertainty,
    );
    let uuid = crate::mob::new_uuid();
    let crit = false;
    // `getArrow` of the skeleton kinds: tipped arrows and the wither skeleton's fire.
    let (effect, fire) = super::fam::undead::skeleton_arrow(m.kind);
    let mut new = NewEntity::new(entity_ids::ARROW, t.level.network_ids().next(), uuid, start);
    new.velocity = velocity;
    new.rot = rot;
    new.flags = EntityFlags::HAS_IMPULSE;
    if fire {
        new.flags.set(EntityFlags::ON_FIRE, true);
    }
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
            random_state: own.state(),
            knockback: 0.0,
            fire,
            effect,
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
    let Some(mut a) = ents.arrow(id).cloned() else {
        return;
    };
    let Some(e) = ents.store.get(id) else {
        return;
    };
    let mut pos = e.pos();
    let mut velocity = e.velocity();
    let mut rot = e.rot();
    let mut random = LegacyRandom::from_state(a.random_state);
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
        // The step and the rotation use the motion from the start of the tick.
        let movement = velocity;
        let in_water = super::is_water(state);
        if in_water {
            velocity = velocity * f64::from(0.6_f32);
        }
        #[allow(clippy::cast_possible_truncation)]
        let y_rot = atan2_deg(movement.x, movement.z) as f32;
        let hd = (movement.x * movement.x + movement.z * movement.z).sqrt();
        #[allow(clippy::cast_possible_truncation)]
        let x_rot = atan2_deg(movement.y, hd) as f32;
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
        let to = pos + movement;
        let block_hit = physics::clip(level as &dyn Blocks, pos, to);
        let end = block_hit.map_or(to, |(_, p)| p);
        // `findHitEntities`: the hit entity nearest to the arrow (`computeMargin`).
        #[allow(clippy::cast_precision_loss)]
        let margin = ((a.tick_count - 2) as f32 / 20.0).clamp(0.0, 0.3);
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
            // `ProjectileUtil.getManyEntityHitResult`: the exact box, else the box grown by
            // the margin when the way from there to its centre is free.
            let bb = oe.aabb();
            let hit = aabb_clip(&bb, pos, end).or_else(|| {
                if margin <= 0.0 {
                    return None;
                }
                let m = f64::from(margin);
                let outside = aabb_clip(&bb.inflate(m, m, m), pos, end)?;
                let towards = physics::clip(level as &dyn Blocks, outside, bb.center())
                    .map_or(bb.center(), |(_, p)| p);
                aabb_clip(&bb, outside, towards).map(|_| outside)
            });
            if let Some(hit) = hit {
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
                if a.fire {
                    super::attack::ignite(ents, target, 100);
                }
                // `Arrow.doPostHurtEffects`: the tipped arrow's effect.
                if let Some(effect) = a.effect.clone() {
                    crate::effect::living::add_to_entity(level, ents, env, cx, target, effect);
                }
                // `doKnockback`: Punch pushes along the arrow's flight.
                if a.knockback > 0.0 {
                    let resist = ents
                        .mob(target)
                        .map_or(0.0, |m| m.attrs.knockback_resistance);
                    let flat = Vec3::new(velocity.x, 0.0, velocity.z);
                    if flat.length_sqr() > 0.0 {
                        let push = flat.normalize()
                            * (f64::from(a.knockback) * 0.6 * (1.0 - resist).max(0.0));
                        if let Some(mut e) = ents.store.get_mut(target) {
                            let v = e.as_ref().velocity();
                            e.set_velocity(v + Vec3::new(push.x, 0.1, push.z));
                            e.set_flags(EntityFlags::HAS_IMPULSE, true);
                            e.set_flags(EntityFlags::HURT_MARKED, true);
                        }
                    }
                }
                if let Some(owner) = a.owner
                    && let Some(m) = ents.mob_mut(owner)
                {
                    m.last_hurt_mob = Some(target);
                }
                let pitch = 1.2 / (random.next_float() * 0.2 + 0.9);
                let seed = crate::survival::sound_seed();
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
                // Deflected (`deflect(ProjectileDeflection.REVERSE, ..., 0.2)`): back at half
                // the power, turned by 170° to 190°.
                let turn = 170.0 + random.next_float() * 20.0;
                velocity = velocity * -0.1;
                rot.y_rot += turn;
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
            let seed = crate::survival::sound_seed();
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
            // `applyInertia(0.99F)`.
            velocity = velocity * f64::from(0.99_f32);
        }
        if !a.in_ground {
            velocity.y -= 0.05;
        }
    }
    a.random_state = random.state();
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
pub fn lerp_rotation(old: f32, new: f32) -> f32 {
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
pub fn segment_box(from: Vec3, to: Vec3, b: &Aabb) -> Option<f64> {
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

/// `AABB.clip(from, to)`: where the way from `from` to `to` enters the box; nothing when it
/// starts inside.
pub fn aabb_clip(b: &Aabb, from: Vec3, to: Vec3) -> Option<Vec3> {
    let (dx, dy, dz) = (to.x - from.x, to.y - from.y, to.z - from.z);
    let mut scale = 1.0;
    let mut hit = false;
    #[allow(clippy::too_many_arguments)]
    let mut clip_point = |da: f64,
                          db: f64,
                          dc: f64,
                          point: f64,
                          (min_b, max_b): (f64, f64),
                          (min_c, max_c): (f64, f64),
                          (fa, fb, fc): (f64, f64, f64)| {
        let s = (point - fa) / da;
        let pb = fb + s * db;
        let pc = fc + s * dc;
        if 0.0 < s
            && s < scale
            && min_b - 1.0e-7 < pb
            && pb < max_b + 1.0e-7
            && min_c - 1.0e-7 < pc
            && pc < max_c + 1.0e-7
        {
            scale = s;
            hit = true;
        }
    };
    let yz = (from.x, from.y, from.z);
    if dx > 1.0e-7 {
        clip_point(
            dx,
            dy,
            dz,
            b.min_x,
            (b.min_y, b.max_y),
            (b.min_z, b.max_z),
            yz,
        );
    } else if dx < -1.0e-7 {
        clip_point(
            dx,
            dy,
            dz,
            b.max_x,
            (b.min_y, b.max_y),
            (b.min_z, b.max_z),
            yz,
        );
    }
    let zx = (from.y, from.z, from.x);
    if dy > 1.0e-7 {
        clip_point(
            dy,
            dz,
            dx,
            b.min_y,
            (b.min_z, b.max_z),
            (b.min_x, b.max_x),
            zx,
        );
    } else if dy < -1.0e-7 {
        clip_point(
            dy,
            dz,
            dx,
            b.max_y,
            (b.min_z, b.max_z),
            (b.min_x, b.max_x),
            zx,
        );
    }
    let xy = (from.z, from.x, from.y);
    if dz > 1.0e-7 {
        clip_point(
            dz,
            dx,
            dy,
            b.min_z,
            (b.min_x, b.max_x),
            (b.min_y, b.max_y),
            xy,
        );
    } else if dz < -1.0e-7 {
        clip_point(
            dz,
            dx,
            dy,
            b.max_z,
            (b.min_x, b.max_x),
            (b.min_y, b.max_y),
            xy,
        );
    }
    hit.then(|| {
        Vec3::new(
            from.x + scale * dx,
            from.y + scale * dy,
            from.z + scale * dz,
        )
    })
}

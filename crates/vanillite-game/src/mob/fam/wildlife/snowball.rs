//! Snowballs (`Snowball`, a `ThrowableItemProjectile`): they fall with gravity 0.03, slow down
//! by 0.99 in air and 0.8 in water, and break on the first block or entity they hit; blazes
//! take 3 damage, everything else a damage-free hit.

use vanillite_base::random::LegacyRandom;
use vanillite_base::{Aabb, Vec3};
use vanillite_config::GameMode;
use vanillite_world::entity::{EntityFlags, EntityId, Rotation};

use crate::mob::kinds::Kind;
use crate::mob::missile::{MissileKind, MissileTick};
use crate::mob::physics::{self, Blocks};
use crate::mob::projectile::segment_box;
use crate::survival::damage::DamageSource;

/// `Projectile.shoot(x, y, z, power, uncertainty)`.
pub(crate) fn shoot(
    random: &mut LegacyRandom,
    dir: Vec3,
    power: f32,
    uncertainty: f32,
) -> (Vec3, Rotation) {
    crate::mob::projectile::shoot(random, dir, power, uncertainty)
}

/// `canHitEntity`: living, not a spectator, and not the owner until the projectile left it.
fn can_hit(t: &MissileTick<'_, '_>, id: EntityId, owner: Option<EntityId>, left: bool) -> bool {
    if id == t.id || (!left && Some(id) == owner) {
        return false;
    }
    if let Some(p) = t.ents.player(id) {
        return p.game_mode != GameMode::Spectator && !p.survival.is_dead_or_dying();
    }
    t.ents.mob(id).is_some_and(crate::mob::MobData::is_alive) || t.ents.boat(id).is_some()
}

/// `ThrowableProjectile.tick` of a snowball.
pub(super) fn tick(t: &mut MissileTick<'_, '_>) {
    let Some(d) = t.ents.missile(t.id) else {
        return;
    };
    if d.kind != MissileKind::Snowball {
        return;
    }
    let (owner, mut left) = (d.owner, d.left_owner);
    let Some(core) = t.core() else {
        return;
    };
    let (pos, mut v, aabb) = (core.pos(), core.velocity(), core.aabb());
    let no_gravity = core.flags().contains(EntityFlags::NO_GRAVITY);
    // `checkLeftOwner`.
    if !left {
        let area = expand_towards(aabb, v).inflate(1.0, 1.0, 1.0);
        left = owner
            .and_then(|o| t.ents.store.get(o))
            .is_none_or(|o| !o.aabb().intersects(&area));
        if let Some(d) = t.data() {
            d.left_owner = left;
        }
    }
    if pos.y < f64::from(t.level.min_y() - 64) {
        if let Some(d) = t.data() {
            d.removed = true;
        }
        return;
    }
    // `applyGravity` and `applyInertia`.
    if !no_gravity {
        v.y -= 0.03;
    }
    let in_water = crate::mob::is_water(
        t.level
            .block(vanillite_base::BlockPos::containing(pos.x, pos.y, pos.z)),
    );
    let inertia = if in_water { 0.8 } else { f64::from(0.99_f32) };
    v = v * inertia;
    // `getHitResultOnMoveVector`: blocks first, then entities up to that point.
    let mut to = pos + v;
    let block_hit = physics::clip(t.level as &dyn Blocks, pos, to);
    if let Some((_, at)) = block_hit {
        to = at;
    }
    let search = expand_towards(aabb, v).inflate(1.0, 1.0, 1.0);
    let mut found = std::mem::take(&mut t.cx.ids);
    found.clear();
    t.ents
        .store
        .for_each_in(&search, false, |id| found.push(id));
    let mut hit: Option<(EntityId, f64)> = None;
    for &id in &found {
        if !can_hit(t, id, owner, left) {
            continue;
        }
        let Some(e) = t.ents.store.get(id) else {
            continue;
        };
        let b = e.aabb().inflate(0.3, 0.3, 0.3);
        if let Some(f) = segment_box(pos, to, &b)
            && hit.is_none_or(|(_, best)| f < best)
        {
            hit = Some((id, f));
        }
    }
    t.cx.ids = found;
    let new_pos = match hit {
        Some((_, f)) => pos + (to - pos) * f,
        None => to,
    };
    let sd = (v.x * v.x + v.z * v.z).sqrt();
    #[allow(clippy::cast_possible_truncation)]
    let rot = Rotation {
        y_rot: crate::mob::atan2_deg(v.x, v.z) as f32,
        x_rot: crate::mob::atan2_deg(v.y, sd) as f32,
        y_head_rot: 0.0,
    };
    if let Some(mut e) = t.ents.store.get_mut(t.id) {
        e.set_velocity(v);
        e.set_pos(new_pos);
        e.set_rot(rot);
    }
    if hit.is_none() && block_hit.is_none() {
        return;
    }
    if let Some((target, _)) = hit {
        let kind = t.env.types.named("minecraft:thrown");
        let mut source = DamageSource::of(kind);
        source.direct = Some(t.id);
        source.cause = owner.filter(|o| t.ents.store.get(*o).is_some());
        source.position = Some(new_pos);
        if let Some(m) = source.cause.and_then(|c| t.ents.mob(c)) {
            source.cause_is_mob = true;
            source.cause_name = Some(m.kind.translation_key());
        }
        let damage = if t.ents.mob(target).is_some_and(|m| m.kind == Kind::Blaze) {
            3.0
        } else {
            0.0
        };
        crate::mob::combat::hurt(t.level, t.ents, t.env, t.cx, target, source, damage);
    }
    // `onHit`: the break particles (entity event 3) and gone.
    if let Some(e) = t.ents.store.get(t.id) {
        let network_id = e.network_id().0;
        let id = t.id;
        t.env.out.push(crate::survival::Effect::Tracking {
            entity: id,
            with_self: false,
            packet: vanillite_protocol::SharedPacket::of(
                &vanillite_protocol::packets::play::clientbound::EntityEvent {
                    entity_id: network_id,
                    event: 3,
                },
            ),
        });
    }
    if let Some(d) = t.data() {
        d.removed = true;
    }
}

/// `AABB.expandTowards(movement)`.
fn expand_towards(b: Aabb, v: Vec3) -> Aabb {
    Aabb::new(
        if v.x < 0.0 { b.min_x + v.x } else { b.min_x },
        if v.y < 0.0 { b.min_y + v.y } else { b.min_y },
        if v.z < 0.0 { b.min_z + v.z } else { b.min_z },
        if v.x > 0.0 { b.max_x + v.x } else { b.max_x },
        if v.y > 0.0 { b.max_y + v.y } else { b.max_y },
        if v.z > 0.0 { b.max_z + v.z } else { b.max_z },
    )
}

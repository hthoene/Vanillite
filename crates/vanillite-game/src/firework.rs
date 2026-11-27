//! Firework rockets (`FireworkRocketEntity`, `FireworkRocketItem`): launched from a block
//! (`useOn`), from a crossbow (shot at an angle) or held by a gliding player (attached: they
//! push the glider along its look direction). A rocket flies `10 * (flight + 1) + 0..5 + 0..6`
//! ticks and explodes then (entity event 17, the client draws the stars); rockets with stars
//! hurt the living entities within 5 blocks it can see.
//!
//! Kind specific data ([`MissileData`]): `ints[0]` the lifetime, `ints[1]` the network id of
//! the entity it is attached to plus one, `ints[2]` 1 when shot at an angle, `ints[3]` 1 after
//! a horizontal collision; `target` the attached entity.

use vanillite_base::random::{LegacyRandom, RandomSource};
use vanillite_base::{Aabb, Direction, Vec3};
use vanillite_config::GameMode;
use vanillite_nbt::{Compound, Tag};
use vanillite_protocol::codec::Reader;
use vanillite_protocol::packets::play::clientbound::{EntityEvent, SoundSource};
use vanillite_protocol::packets::play::serverbound::Hand;
use vanillite_world::entity::{EntityFlags, EntityId, NetworkIds, Rotation};

use crate::entities::Entities;
use crate::item::ItemStack;
use crate::mob::missile::{self, MissileData, MissileKind, MissileTick};
use crate::mob::physics::{self, Blocks};
use crate::player::encode::DataWriter;
use crate::player::item_use::Used;
use crate::survival::damage::DamageSource;
use crate::survival::play_sound;
use crate::survival::player::Env;

const LIFETIME: usize = 0;
const ATTACHED: usize = 1;
const SHOT_AT_ANGLE: usize = 2;
const HORIZONTAL_COLLISION: usize = 3;

/// The `fireworks` component of a rocket: the flight duration and the number of explosions.
pub fn fireworks(stack: &ItemStack) -> (i32, usize) {
    let id = vanillite_protocol::components::ids::FIREWORKS;
    if let Some(p) = stack.patch() {
        if let Some(v) = p.get(id) {
            let mut r = Reader::new(v);
            let flight = r.varint().unwrap_or(0);
            let count = r.varint().ok().and_then(|n| usize::try_from(n).ok());
            return (flight, count.unwrap_or(0));
        }
        if p.is_removed(id) {
            return (0, 0);
        }
    }
    if crate::item::item_id(stack).name() == "minecraft:firework_rocket" {
        (1, 0)
    } else {
        (0, 0)
    }
}

/// A new rocket at `pos` (`FireworkRocketEntity(level, x, y, z, stack)`): its random draws
/// the start motion and the lifetime. Returns the data, the motion and the random's state.
fn new_rocket(stack: &ItemStack) -> (MissileData, Vec3) {
    let mut own = LegacyRandom::new(crate::mob::new_entity_seed());
    let (flight, _) = fireworks(stack);
    let x = own.triangle(0.0, 0.002_297);
    let z = own.triangle(0.0, 0.002_297);
    let lifetime = 10 * (1 + flight) + own.next_int_bounded(6) + own.next_int_bounded(7);
    let mut d = MissileData::new(MissileKind::FireworkRocket);
    d.item = stack.clone();
    d.ints[LIFETIME] = lifetime;
    d.random_state = own.state();
    (d, Vec3::new(x, 0.05, z))
}

/// Spawns a rocket (`Projectile.spawnProjectile`).
#[allow(clippy::too_many_arguments)]
pub fn spawn_rocket(
    ents: &mut Entities,
    network_ids: &NetworkIds,
    owner: Option<EntityId>,
    pos: Vec3,
    stack: &ItemStack,
    attached: Option<EntityId>,
    shot: Option<Vec3>,
) -> EntityId {
    let (mut d, mut velocity) = new_rocket(stack);
    let uuid = crate::mob::new_uuid();
    d.owner = owner;
    d.owner_uuid = owner.and_then(|o| ents.store.get(o)).map(|e| e.uuid());
    if let Some(a) = attached {
        d.target = Some(a);
        d.ints[ATTACHED] = ents
            .store
            .get(a)
            .map_or(0, |e| e.network_id().0.saturating_add(1));
    }
    let mut rot = Rotation::default();
    if let Some(v) = shot {
        d.ints[SHOT_AT_ANGLE] = 1;
        velocity = v;
        let h = (v.x * v.x + v.z * v.z).sqrt();
        #[allow(clippy::cast_possible_truncation)]
        {
            rot.y_rot = crate::mob::atan2_deg(v.x, v.z) as f32;
            rot.x_rot = crate::mob::atan2_deg(v.y, h) as f32;
        }
    }
    missile::spawn(ents, network_ids, uuid, pos, velocity, rot, d)
}

/// A crossbow's rocket (`CrossbowItem.createProjectile`, `shootProjectile`): the rocket is made
/// first (its random draws the start motion and lifetime), then `shoot` draws the spread from
/// it and sets the motion.
#[allow(clippy::too_many_arguments)]
pub fn crossbow_rocket(
    ents: &mut Entities,
    network_ids: &NetworkIds,
    owner: EntityId,
    pos: Vec3,
    stack: &ItemStack,
    dir: Vec3,
    power: f32,
    uncertainty: f32,
) -> EntityId {
    let (mut d, _) = new_rocket(stack);
    let mut own = LegacyRandom::from_state(d.random_state);
    let (velocity, rot) = crate::mob::projectile::shoot(&mut own, dir, power, uncertainty);
    d.random_state = own.state();
    d.ints[SHOT_AT_ANGLE] = 1;
    d.owner = Some(owner);
    d.owner_uuid = ents.store.get(owner).map(|e| e.uuid());
    let uuid = crate::mob::new_uuid();
    let id = missile::spawn(
        ents,
        network_ids,
        uuid,
        pos,
        velocity,
        Rotation {
            y_head_rot: 0.0,
            ..rot
        },
        d,
    );
    // `shoot`: `needsSync`.
    if let Some(mut e) = ents.store.get_mut(id) {
        e.set_flags(EntityFlags::HAS_IMPULSE, true);
    }
    id
}

/// `FireworkRocketItem.useOn`: a rocket from the clicked point, 0.15 off the face (gliding
/// players use it in the air instead).
pub fn use_on(
    ents: &mut Entities,
    env: &mut Env<'_>,
    network_ids: &NetworkIds,
    player: EntityId,
    stack: &mut ItemStack,
    face: Direction,
    location: Vec3,
) {
    let _ = env;
    if is_fall_flying(ents, player) {
        return;
    }
    let [dx, dy, dz] = face.normal();
    let pos = Vec3::new(
        location.x + f64::from(dx) * 0.15,
        location.y + f64::from(dy) * 0.15,
        location.z + f64::from(dz) * 0.15,
    );
    spawn_rocket(ents, network_ids, Some(player), pos, stack, None, None);
    let creative = ents
        .player(player)
        .is_some_and(|p| p.game_mode == GameMode::Creative);
    // `itemStack.shrink(1)` (also in creative: `useOn` restores the count there).
    if !creative {
        stack.shrink(1);
    }
}

/// `isFallFlying` (shared flag 7).
pub fn is_fall_flying(ents: &Entities, id: EntityId) -> bool {
    ents.store
        .get(id)
        .is_some_and(|e| e.flags().contains(EntityFlags::FALL_FLYING))
}

/// `FireworkRocketItem.use`: a gliding player's rocket, attached to it.
pub fn use_while_gliding(
    ents: &mut Entities,
    env: &mut Env<'_>,
    network_ids: &NetworkIds,
    player: EntityId,
    stack: &mut ItemStack,
    hand: Hand,
) -> Used {
    let _ = hand;
    if !is_fall_flying(ents, player) {
        return Used::Pass;
    }
    // `dropAllLeashConnections`: the player's leashed mobs go free.
    if crate::mob::tame::drop_leashes_of(ents, env, player) {
        let pos = ents.store.get(player).map_or(Vec3::ZERO, |e| e.pos());
        play_sound(
            env.out,
            None,
            pos,
            "minecraft:item.lead.break",
            SoundSource::Neutral,
            1.0,
            1.0,
            crate::survival::sound_seed(),
        );
    }
    let Some(pos) = ents.store.get(player).map(|e| e.pos()) else {
        return Used::Pass;
    };
    spawn_rocket(
        ents,
        network_ids,
        Some(player),
        pos,
        stack,
        Some(player),
        None,
    );
    let creative = ents.player_mut(player).is_some_and(|p| {
        p.stats.award(
            crate::stats::Stat::of(crate::stats::StatType::Used, stack.item),
            1,
        );
        p.game_mode == GameMode::Creative
    });
    if !creative {
        stack.shrink(1);
    }
    Used::Success
}

/// `Entity.calculateViewVector(xRot, yRot)`.
pub fn view_vector(x_rot: f32, y_rot: f32) -> Vec3 {
    let rad = crate::mob::DEG_TO_RAD;
    let (rx, ry) = (x_rot * rad, -y_rot * rad);
    let cos = |a: f32| vanillite_base::math::cos(f64::from(a));
    let sin = |a: f32| vanillite_base::math::sin(f64::from(a));
    let (y_cos, y_sin, x_cos, x_sin) = (cos(ry), sin(ry), cos(rx), sin(rx));
    Vec3::new(
        f64::from(y_sin * x_cos),
        f64::from(-x_sin),
        f64::from(y_cos * x_cos),
    )
}

/// `getHandHoldingItemAngle(FIREWORK_ROCKET)` of a player.
fn hand_angle(ents: &Entities, player: EntityId) -> Vec3 {
    let Some(p) = ents.player(player) else {
        return Vec3::ZERO;
    };
    let Some(e) = ents.store.get(player) else {
        return Vec3::ZERO;
    };
    let is_rocket = |s: &ItemStack| {
        !s.is_empty() && crate::item::item_id(s).name() == "minecraft:firework_rocket"
    };
    let only_off = is_rocket(&p.inventory.offhand) && !is_rocket(p.inventory.selected_item());
    let right = (p.client.main_hand == crate::player::MainHand::Right) != only_off;
    let y = e.rot().y_rot + if right { 80.0 } else { -80.0 };
    view_vector(0.0, y) * 0.5
}

/// `FireworkRocketEntity.tick`.
pub fn tick(t: &mut MissileTick<'_, '_>) {
    let Some(d) = t.ents.missile(t.id).cloned() else {
        return;
    };
    let Some(e) = t.core() else {
        return;
    };
    let (pos, mut v, aabb, rot) = (e.pos(), e.velocity(), e.aabb(), e.rot());
    // `Projectile.tick`: `checkLeftOwner`.
    let left = d.left_owner || {
        let area = aabb.expand_towards(v.x, v.y, v.z).inflate(1.0, 1.0, 1.0);
        let left = d
            .owner
            .and_then(|o| t.ents.store.get(o))
            .is_none_or(|o| !o.aabb().intersects(&area));
        if let Some(m) = t.data() {
            m.left_owner = left;
        }
        left
    };
    let mut new_pos = pos;
    let hit;
    if d.ints[ATTACHED] != 0 {
        if let Some(a) = d.target.filter(|a| t.ents.store.get(*a).is_some()) {
            let hand = if is_fall_flying(t.ents, a) {
                let (look, movement) = {
                    let ae = t.ents.store.get(a);
                    let r = ae.as_ref().map_or(Rotation::default(), |e| e.rot());
                    (
                        view_vector(r.x_rot, r.y_rot),
                        ae.map_or(Vec3::ZERO, |e| e.velocity()),
                    )
                };
                let boosted = movement
                    + Vec3::new(
                        look.x * 0.1 + (look.x * 1.5 - movement.x) * 0.5,
                        look.y * 0.1 + (look.y * 1.5 - movement.y) * 0.5,
                        look.z * 0.1 + (look.z * 1.5 - movement.z) * 0.5,
                    );
                if let Some(mut ae) = t.ents.store.get_mut(a) {
                    ae.set_velocity(boosted);
                }
                hand_angle(t.ents, a)
            } else {
                Vec3::ZERO
            };
            if let Some(ae) = t.ents.store.get(a) {
                new_pos = ae.pos() + hand;
                v = ae.velocity();
            }
            if let Some(mut e) = t.ents.store.get_mut(t.id) {
                e.set_pos(new_pos);
                e.set_velocity(v);
            }
        }
        hit = find_hit(t, &d, new_pos, v, left);
    } else {
        if d.ints[SHOT_AT_ANGLE] == 0 {
            let h = if d.ints[HORIZONTAL_COLLISION] != 0 {
                1.0
            } else {
                1.15
            };
            v = Vec3::new(v.x * h, v.y + 0.04, v.z * h);
        }
        hit = find_hit(t, &d, pos, v, left);
        // `move(SELF, movement)` with block collisions, then the motion stays.
        let mut scratch = Vec::new();
        let moved = physics::collide(t.level as &dyn Blocks, aabb, v, 0.0, false, &mut scratch);
        new_pos = pos + moved;
        let horizontal = (moved.x - v.x).abs() > 1.0e-7 || (moved.z - v.z).abs() > 1.0e-7;
        if let Some(m) = t.data() {
            m.ints[HORIZONTAL_COLLISION] = i32::from(horizontal);
        }
        if let Some(mut e) = t.ents.store.get_mut(t.id) {
            e.set_pos(new_pos);
            e.set_velocity(v);
            e.set_flags(EntityFlags::ON_GROUND, moved.y != v.y && v.y < 0.0);
        }
    }
    let mut exploded = false;
    match hit {
        Hit::Entity => {
            explode(t, &d);
            exploded = true;
        }
        Hit::Block => {
            if fireworks(&d.item).1 > 0 {
                explode(t, &d);
                exploded = true;
            }
        }
        Hit::Miss => {}
    }
    // `updateRotation`.
    let h = (v.x * v.x + v.z * v.z).sqrt();
    #[allow(clippy::cast_possible_truncation)]
    let new_rot = Rotation {
        y_rot: crate::mob::projectile::lerp_rotation(
            rot.y_rot,
            crate::mob::atan2_deg(v.x, v.z) as f32,
        ),
        x_rot: crate::mob::projectile::lerp_rotation(
            rot.x_rot,
            crate::mob::atan2_deg(v.y, h) as f32,
        ),
        y_head_rot: 0.0,
    };
    if let Some(mut e) = t.ents.store.get_mut(t.id) {
        e.set_rot(new_rot);
    }
    // `life == 0` before this tick's increment: the launch sound.
    if d.life == 1 {
        let at = t.core().map_or(new_pos, |e| e.pos());
        play_sound(
            t.env.out,
            None,
            at,
            "minecraft:entity.firework_rocket.launch",
            SoundSource::Ambient,
            3.0,
            1.0,
            crate::survival::sound_seed(),
        );
    }
    if !exploded && d.life > d.ints[LIFETIME] {
        explode(t, &d);
    }
}

/// What a rocket hits on its way.
enum Hit {
    Miss,
    Entity,
    Block,
}

/// `ProjectileUtil.getHitResultOnMoveVector(this, canHitEntity)`: the first block on the way,
/// then the closest entity before it.
fn find_hit(t: &mut MissileTick<'_, '_>, d: &MissileData, pos: Vec3, v: Vec3, left: bool) -> Hit {
    let to = pos + v;
    let block_hit = physics::clip(t.level as &dyn Blocks, pos, to);
    let end = block_hit.map_or(to, |(_, p)| p);
    let Some(aabb) = t.core().map(|e| e.aabb()) else {
        return Hit::Miss;
    };
    let m = missile::margin(d.life);
    let area = aabb.expand_towards(v.x, v.y, v.z).inflate(1.0, 1.0, 1.0);
    let mut candidates = std::mem::take(&mut t.cx.ids);
    candidates.clear();
    let id = t.id;
    t.ents.store.for_each_in(&area, false, |other| {
        if other != id {
            candidates.push(other);
        }
    });
    let mut best: Option<(EntityId, f64)> = None;
    for &other in &candidates {
        if !crate::effect::thrown::can_be_hit(t.ents, other) || (!left && Some(other) == d.owner) {
            continue;
        }
        let Some(oe) = t.ents.store.get(other) else {
            continue;
        };
        let grown = oe.aabb().inflate(m, m, m);
        if let Some(f) = crate::mob::projectile::segment_box(pos, end, &grown) {
            let dist = f * (end - pos).length();
            if best.is_none_or(|(_, b)| dist < b) {
                best = Some((other, dist));
            }
        }
    }
    t.cx.ids = candidates;
    match (best, block_hit) {
        (Some(_), _) => Hit::Entity,
        (None, Some(_)) => Hit::Block,
        (None, None) => Hit::Miss,
    }
}

/// `explode`: entity event 17 (the stars on the clients), the damage, gone.
fn explode(t: &mut MissileTick<'_, '_>, d: &MissileData) {
    if let Some(e) = t.ents.store.get(t.id) {
        let network_id = e.network_id().0;
        let id = t.id;
        t.env.out.push(crate::survival::Effect::Tracking {
            entity: id,
            with_self: true,
            packet: vanillite_protocol::SharedPacket::of(&EntityEvent {
                entity_id: network_id,
                event: 17,
            }),
        });
    }
    deal_explosion_damage(t, d);
    if let Some(m) = t.data() {
        m.removed = true;
    }
}

/// `damageSources().fireworks(rocket, owner)`.
fn fireworks_source(t: &MissileTick<'_, '_>, owner: Option<EntityId>) -> DamageSource {
    let mut source = DamageSource::of(t.env.types.fireworks);
    source.direct = Some(t.id);
    source.cause = owner.filter(|o| t.ents.store.get(*o).is_some());
    source.position = t.core().map(|e| e.pos());
    if let Some(m) = source.cause.and_then(|c| t.ents.mob(c)) {
        source.cause_is_mob = true;
        source.cause_name = Some(m.kind.translation_key());
    }
    source
}

/// `dealExplosionDamage`: rockets with stars hurt the entity they are attached to by
/// `5 + 2 * stars` and the living entities within 5 blocks the rocket sees (their feet or
/// their middle) by that much scaled with the distance.
fn deal_explosion_damage(t: &mut MissileTick<'_, '_>, d: &MissileData) {
    let stars = fireworks(&d.item).1;
    if stars == 0 {
        return;
    }
    #[allow(clippy::cast_precision_loss)]
    let damage = 5.0 + stars as f32 * 2.0;
    let attached = d.target.filter(|_| d.ints[ATTACHED] != 0);
    if let Some(a) = attached {
        let source = fireworks_source(t, d.owner);
        crate::mob::combat::hurt(t.level, t.ents, t.env, t.cx, a, source, damage);
    }
    let Some((pos, aabb)) = t.core().map(|e| (e.pos(), e.aabb())) else {
        return;
    };
    let area: Aabb = aabb.inflate(5.0, 5.0, 5.0);
    let mut targets = Vec::new();
    t.ents
        .store
        .for_each_in(&area, false, |id| targets.push(id));
    for target in targets {
        if Some(target) == attached || !is_living(t.ents, target) {
            continue;
        }
        let Some(te) = t.ents.store.get(target) else {
            continue;
        };
        let (tp, height) = (te.pos(), f64::from(te.dims().height));
        if pos.distance_to_sqr(tp) > 25.0 {
            continue;
        }
        let sees = (0..2).any(|step| {
            let to = Vec3::new(tp.x, tp.y + height * 0.5 * f64::from(step), tp.z);
            physics::clip(t.level as &dyn Blocks, pos, to).is_none()
        });
        if !sees {
            continue;
        }
        #[allow(clippy::cast_possible_truncation)]
        let amount = damage * ((5.0 - pos.distance_to_sqr(tp).sqrt()) / 5.0).sqrt() as f32;
        let source = fireworks_source(t, d.owner);
        crate::mob::combat::hurt(t.level, t.ents, t.env, t.cx, target, source, amount);
    }
}

/// A living entity (`getEntitiesOfClass(LivingEntity.class)`).
fn is_living(ents: &Entities, id: EntityId) -> bool {
    ents.mob(id).is_some() || ents.player(id).is_some()
}

/// The rocket's NBT (`Life`, `LifeTime`, `FireworksItem`, `ShotAtAngle`).
pub fn save(d: &MissileData, out: &mut Compound) {
    out.insert("Life", d.life);
    out.insert("LifeTime", d.ints[LIFETIME]);
    out.insert(
        "FireworksItem",
        Tag::Compound(crate::item_nbt::stack_to_nbt(&d.item)),
    );
    out.insert("ShotAtAngle", d.ints[SHOT_AT_ANGLE] != 0);
}

/// Reads [`save`]'s fields.
pub fn load(d: &mut MissileData, tag: &Compound) {
    let int = |k: &str| match tag.get(k) {
        Some(Tag::Int(v)) => *v,
        _ => 0,
    };
    d.life = int("Life");
    d.ints[LIFETIME] = int("LifeTime");
    if let Some(Tag::Compound(c)) = tag.get("FireworksItem") {
        d.item = crate::item_nbt::stack_from_nbt(c);
    }
    d.ints[SHOT_AT_ANGLE] =
        i32::from(matches!(tag.get("ShotAtAngle"), Some(Tag::Byte(b)) if *b != 0));
}

/// The synched data: the rocket item (8), the attached entity (9), shot at an angle (10).
pub fn encode(w: &mut DataWriter<'_>, d: &MissileData) {
    w.stack(8, &d.item);
    let attached = d.ints[ATTACHED];
    w.raw(
        9,
        crate::player::encode::serializer::OPTIONAL_UNSIGNED_INT,
        attached == 0,
        |o| vanillite_protocol::codec::write_varint(o, attached),
    );
    w.boolean(10, d.ints[SHOT_AT_ANGLE] != 0, false);
}

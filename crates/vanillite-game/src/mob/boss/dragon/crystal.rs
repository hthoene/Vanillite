//! End crystals (`EndCrystal`), dragon fireballs (`DragonFireball` with
//! `AbstractHurtingProjectile`'s flight) and the dragon's breath clouds (`AreaEffectCloud`
//! with [`crate::effect::cloud`]).

use vanillite_base::{Aabb, BlockPos, Vec3};
use vanillite_data::block::ids;
use vanillite_data::entity::{EntityTypeId, ids as entity_ids};
use vanillite_nbt::{Compound, Tag};
use vanillite_world::entity::{EntityId, NewEntity, Rotation};

use super::super::data;
use super::super::special::{SpecialData, SpecialTick};
use super::fight::DragonEvent;
use crate::effect::cloud::CloudData;
use crate::effect::potion::PotionContents;
use crate::effect::{EffectInstance, MobEffect};
use crate::entities::Entities;
use crate::level::Level;
use crate::mob::physics::{self, Blocks};
use crate::mob::{MobContext, WorldAction};
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

/// The cloud of the sitting flame phase: dragon's breath, instant damage, radius 5 for 200
/// ticks.
pub fn flame_cloud(owner: Option<EntityId>) -> CloudData {
    breath_cloud(owner, 5.0, 200, 0.0, 0)
}

/// The cloud a dragon fireball leaves (`DragonFireball.onHit`): radius 3 growing to 7 over
/// 600 ticks, instant damage II.
pub fn fireball_cloud(owner: Option<EntityId>) -> CloudData {
    breath_cloud(owner, 3.0, 600, (7.0 - 3.0) / 600.0, 1)
}

fn breath_cloud(
    owner: Option<EntityId>,
    radius: f32,
    duration: i32,
    radius_per_tick: f32,
    harm: i32,
) -> CloudData {
    CloudData {
        owner,
        radius,
        duration,
        radius_per_tick,
        dragon_breath: true,
        duration_scale: 0.25,
        contents: PotionContents {
            custom_effects: vec![EffectInstance::new(MobEffect::INSTANT_DAMAGE, 1, harm)],
            ..PotionContents::default()
        },
        ..CloudData::default()
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
    let id = ents.spawn_special(new, SpecialData::EndCrystal(Box::new(crystal)));
    ents.spawned(id, vanillite_abi::spawn_reason::DEFAULT, 0);
    id
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
    let id = ents.spawn_special(
        new,
        SpecialData::DragonFireball(Box::new(Fireball {
            owner: Some(owner),
            owner_uuid,
            acceleration_power: 0.1,
            left_owner: false,
            tick_count: 0,
        })),
    );
    ents.launched(id, Some(owner));
    id
}

/// Adds an area effect cloud at `pos`.
pub fn spawn_cloud(level: &Level, ents: &mut Entities, pos: Vec3, cloud: CloudData) -> EntityId {
    crate::effect::cloud::spawn_cloud(ents, level.network_ids(), pos, cloud)
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

/// `EndCrystal.kill`: the fight learns of the destroyed crystal (`onDestroyedBy` with a
/// `generic` source), then it is removed without an explosion (`KILLED`, `ENTITY_DIE`).
/// `false` when `id` is no living crystal.
pub fn kill_crystal(ents: &mut Entities, env: &Env<'_>, cx: &mut MobContext, id: EntityId) -> bool {
    let Some(e) = ents.store.get(id) else {
        return false;
    };
    let (uuid, pos) = (e.uuid(), e.pos());
    let Some(c) = crystal_mut(ents, id) else {
        return false;
    };
    if c.removed {
        return false;
    }
    c.removed = true;
    cx.dragon_events.push(DragonEvent::CrystalDestroyed {
        crystal: id,
        uuid,
        pos: BlockPos::containing(pos.x, pos.y, pos.z),
        source: DamageSource::of(env.types.generic),
    });
    cx.discarded.push(id);
    cx.actions.push(WorldAction::Level(
        crate::mob::boss::warden::LevelAction::GameEvent {
            event: crate::level::game_event::events().entity_die,
            pos,
            context: crate::level::game_event::GameEventContext::of(Some(id)),
        },
    ));
    true
}

/// Where the segment enters `b`, as a fraction (`AABB.clip`).
fn segment_box(from: Vec3, to: Vec3, b: &Aabb) -> Option<f64> {
    crate::mob::projectile::segment_box(from, to, b)
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
    hittable(ents, id)
}

/// Whether `id` is an end crystal a projectile can hit (`canBeHitByProjectile`: alive and
/// pickable).
pub fn hittable(ents: &Entities, id: EntityId) -> bool {
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
    // Bukkit's `ProjectileHitEvent`: a cancelled hit is ignored, the fireball flies on.
    let hit_event = |what| crate::mob::missile::hit_event(t.level, t.ents, id, owner, what);
    let best = best.filter(|&(e, _, _)| hit_event(crate::mob::missile::HitWhat::Entity(e)));
    let block_hit = block_hit.filter(|&(block, at)| {
        best.is_some() || hit_event(crate::mob::missile::HitWhat::Block(block, at))
    });
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
    spawn_cloud(t.level, t.ents, cloud_pos, fireball_cloud(owner));
    t.cx.discarded.push(id);
}

/// The synched data of the specials: crystals (beam target 8, show bottom 9).
pub fn entity_data(out: &mut Vec<u8>, d: &SpecialData, all: bool) {
    if let SpecialData::EndCrystal(c) = d {
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
        SpecialData::WitherSkull(_)
        | SpecialData::ThrownPotion(_)
        | SpecialData::OminousItemSpawner(_) => {}
    }
    Some(())
}

/// Reads the kind data of a crystal or fireball.
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

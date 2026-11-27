//! Spears: the `kinetic_weapon` charge (`KineticWeapon.damageEntities` on every use tick), the
//! `piercing_weapon` jab (`PiercingWeapon.attack`, the player's `STAB` action) and the hit
//! search along the `attack_range` (`ProjectileUtil.getHitEntitiesAlong`), for players and
//! mobs. Both end in `LivingEntity.stabAttack`.
//!
//! The components come from the stack's patch or from the item's default components in the
//! generated item data (the seven spears). A charge remembers the entities it hit with the game
//! time (`recentKineticEnemies`) so each is hit once per contact cool-down.
//!
//! Not ported: the Lunge enchantment (`post_piercing_attack` effects), dragon parts and
//! interaction entities as targets.

use std::borrow::Cow;

use vanillite_base::{Aabb, Vec3};
use vanillite_config::GameMode;
use vanillite_protocol::SharedPacket;
use vanillite_protocol::codec::Reader;
use vanillite_protocol::packets::play::clientbound::{EntityEvent, SoundSource, SwingAnimation};
use vanillite_protocol::packets::play::serverbound::Hand;
use vanillite_world::entity::EntityId;

use crate::entities::Entities;
use crate::item::{self, ItemStack};
use crate::level::Level;
use crate::mob::attack::{
    attack_strength_delay, combat_data, damage_context, extra_knockback, player_attribute,
    post_attack,
};
use crate::mob::{MobContext, combat};
use crate::survival::damage::DamageSource;
use crate::survival::player::Env;
use crate::survival::{Effect, play_sound};

/// A sound event of a component: a registered name or an inline one.
pub type Sound = Cow<'static, str>;

/// `KineticWeapon.Condition`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Condition {
    pub max_duration_ticks: i32,
    pub min_speed: f32,
    pub min_relative_speed: f32,
}

impl Condition {
    /// `Condition.test(ticksUsed, attackerSpeed, relativeSpeed, entityFactor)`.
    pub fn test(
        &self,
        ticks_used: i32,
        attacker_speed: f64,
        relative_speed: f64,
        factor: f64,
    ) -> bool {
        ticks_used <= self.max_duration_ticks
            && attacker_speed >= f64::from(self.min_speed) * factor
            && relative_speed >= f64::from(self.min_relative_speed) * factor
    }
}

/// The `kinetic_weapon` component.
#[derive(Clone, Debug, PartialEq)]
pub struct KineticWeapon {
    pub contact_cooldown_ticks: i32,
    pub delay_ticks: i32,
    pub dismount: Option<Condition>,
    pub knockback: Option<Condition>,
    pub damage: Option<Condition>,
    pub forward_movement: f32,
    pub damage_multiplier: f32,
    pub sound: Option<Sound>,
    pub hit_sound: Option<Sound>,
}

impl KineticWeapon {
    /// `computeDamageUseDuration`.
    pub fn damage_use_duration(&self) -> i32 {
        self.delay_ticks + self.damage.map_or(0, |c| c.max_duration_ticks)
    }
}

/// The `piercing_weapon` component.
#[derive(Clone, Debug, PartialEq)]
pub struct PiercingWeapon {
    pub deals_knockback: bool,
    pub dismounts: bool,
    pub sound: Option<Sound>,
    pub hit_sound: Option<Sound>,
}

/// The `attack_range` component.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AttackRange {
    pub min_reach: f32,
    pub max_reach: f32,
    pub min_creative_reach: f32,
    pub max_creative_reach: f32,
    pub hitbox_margin: f32,
    pub mob_factor: f32,
}

fn component_id(name: &str) -> Option<u16> {
    vanillite_data::registry::DATA_COMPONENT_TYPE.id(name)
}

/// The patched value of a component: `Some(None)` when the patch removes it.
fn patched(stack: &ItemStack, name: &str) -> Option<Option<Box<[u8]>>> {
    let id = component_id(name)?;
    let p = stack.patch()?;
    if let Some(v) = p.get(id) {
        return Some(Some(v.into()));
    }
    p.is_removed(id).then_some(None)
}

fn read_bool(r: &mut Reader<'_>) -> Option<bool> {
    r.u8().ok().map(|b| b != 0)
}

/// `SoundEvent.STREAM_CODEC` (a holder: registry id + 1, or 0 and the inline event).
fn read_sound(r: &mut Reader<'_>) -> Option<Sound> {
    let id = r.varint().ok()?;
    if id == 0 {
        let name = r.string(32767).ok()?.to_owned();
        if read_bool(r)? {
            r.f32().ok()?;
        }
        return Some(Cow::Owned(name));
    }
    let id = u16::try_from(id - 1).ok()?;
    vanillite_data::registry::SOUND_EVENT
        .name(id)
        .map(Cow::Borrowed)
}

fn read_optional_sound(r: &mut Reader<'_>) -> Option<Option<Sound>> {
    if read_bool(r)? {
        read_sound(r).map(Some)
    } else {
        Some(None)
    }
}

fn read_condition(r: &mut Reader<'_>) -> Option<Option<Condition>> {
    if !read_bool(r)? {
        return Some(None);
    }
    Some(Some(Condition {
        max_duration_ticks: r.varint().ok()?,
        min_speed: r.f32().ok()?,
        min_relative_speed: r.f32().ok()?,
    }))
}

fn decode_kinetic(v: &[u8]) -> Option<KineticWeapon> {
    let mut r = Reader::new(v);
    Some(KineticWeapon {
        contact_cooldown_ticks: r.varint().ok()?,
        delay_ticks: r.varint().ok()?,
        dismount: read_condition(&mut r)?,
        knockback: read_condition(&mut r)?,
        damage: read_condition(&mut r)?,
        forward_movement: r.f32().ok()?,
        damage_multiplier: r.f32().ok()?,
        sound: read_optional_sound(&mut r)?,
        hit_sound: read_optional_sound(&mut r)?,
    })
}

fn decode_piercing(v: &[u8]) -> Option<PiercingWeapon> {
    let mut r = Reader::new(v);
    Some(PiercingWeapon {
        deals_knockback: read_bool(&mut r)?,
        dismounts: read_bool(&mut r)?,
        sound: read_optional_sound(&mut r)?,
        hit_sound: read_optional_sound(&mut r)?,
    })
}

fn decode_range(v: &[u8]) -> Option<AttackRange> {
    let mut r = Reader::new(v);
    Some(AttackRange {
        min_reach: r.f32().ok()?,
        max_reach: r.f32().ok()?,
        min_creative_reach: r.f32().ok()?,
        max_creative_reach: r.f32().ok()?,
        hitbox_margin: r.f32().ok()?,
        mob_factor: r.f32().ok()?,
    })
}

fn default_sound(name: &'static str) -> Option<Sound> {
    (!name.is_empty()).then_some(Cow::Borrowed(name))
}

/// The stack's `kinetic_weapon` component.
pub fn kinetic_weapon(stack: &ItemStack) -> Option<KineticWeapon> {
    if stack.is_empty() {
        return None;
    }
    if let Some(v) = patched(stack, "minecraft:kinetic_weapon") {
        return v.and_then(|v| decode_kinetic(&v));
    }
    let k = item::item_id(stack).info().kinetic_weapon?;
    let condition = |c: Option<vanillite_data::item::KineticConditionInfo>| {
        c.map(|c| Condition {
            max_duration_ticks: c.max_duration_ticks,
            min_speed: c.min_speed,
            min_relative_speed: c.min_relative_speed,
        })
    };
    Some(KineticWeapon {
        contact_cooldown_ticks: k.contact_cooldown_ticks,
        delay_ticks: k.delay_ticks,
        dismount: condition(k.dismount_conditions),
        knockback: condition(k.knockback_conditions),
        damage: condition(k.damage_conditions),
        forward_movement: k.forward_movement,
        damage_multiplier: k.damage_multiplier,
        sound: default_sound(k.sound),
        hit_sound: default_sound(k.hit_sound),
    })
}

/// Whether the stack has a `kinetic_weapon` component (it is used over time).
pub fn is_kinetic(stack: &ItemStack) -> bool {
    if stack.is_empty() {
        return false;
    }
    if let Some(v) = patched(stack, "minecraft:kinetic_weapon") {
        return v.is_some();
    }
    item::item_id(stack).info().kinetic_weapon.is_some()
}

/// The stack's `piercing_weapon` component.
pub fn piercing_weapon(stack: &ItemStack) -> Option<PiercingWeapon> {
    if stack.is_empty() {
        return None;
    }
    if let Some(v) = patched(stack, "minecraft:piercing_weapon") {
        return v.and_then(|v| decode_piercing(&v));
    }
    let p = item::item_id(stack).info().piercing_weapon?;
    Some(PiercingWeapon {
        deals_knockback: p.deals_knockback,
        dismounts: p.dismounts,
        sound: default_sound(p.sound),
        hit_sound: default_sound(p.hit_sound),
    })
}

/// `getAttackRangeWith(stack)`: the stack's `attack_range`, else `AttackRange.defaultFor`
/// (the entity interaction range, `interaction_range`).
pub fn attack_range(stack: &ItemStack, interaction_range: f32) -> AttackRange {
    if let Some(v) = patched(stack, "minecraft:attack_range") {
        if let Some(r) = v.and_then(|v| decode_range(&v)) {
            return r;
        }
    } else if !stack.is_empty()
        && let Some(&[min, max, min_creative, max_creative, margin, mob]) =
            item::item_id(stack).info().attack_range
    {
        return AttackRange {
            min_reach: min,
            max_reach: max,
            min_creative_reach: min_creative,
            max_creative_reach: max_creative,
            hitbox_margin: margin,
            mob_factor: mob,
        };
    }
    AttackRange {
        min_reach: 0.0,
        max_reach: interaction_range,
        min_creative_reach: 0.0,
        max_creative_reach: interaction_range,
        hitbox_margin: 0.0,
        mob_factor: 1.0,
    }
}

/// `minimum_attack_charge` of the stack (1 for the spears).
pub fn minimum_attack_charge(stack: &ItemStack) -> f32 {
    if let Some(v) = patched(stack, "minecraft:minimum_attack_charge") {
        return v.and_then(|v| Reader::new(&v).f32().ok()).unwrap_or(0.0);
    }
    if stack.is_empty() {
        return 0.0;
    }
    item::item_id(stack).info().minimum_attack_charge
}

/// The ticks of the stab swing (`attack_animation.duration`).
fn stab_swing_ticks(stack: &ItemStack) -> i32 {
    if stack.is_empty() {
        return 6;
    }
    i32::from(item::item_id(stack).info().attack_animation_ticks)
}

// --- The attacker -----------------------------------------------------------------------------

/// What the hit search and the damage need of the attacking entity.
#[derive(Clone, Copy, Debug)]
pub struct Attacker {
    pub id: EntityId,
    pub is_player: bool,
    pub creative: bool,
    /// `getEyePosition`.
    pub eye: Vec3,
    /// `getHeadLookAngle` (pitch and head yaw).
    pub head_look: Vec3,
    /// `getLookAngle` (pitch and body yaw).
    pub look: Vec3,
    /// `getYRot`, the knockback direction.
    pub y_rot: f32,
    /// `getKnownMovement`.
    pub known_movement: Vec3,
}

impl Attacker {
    /// The attacker as the entities describe it; `None` when it is gone.
    pub fn of(ents: &Entities, id: EntityId) -> Option<Attacker> {
        let e = ents.store.get(id)?;
        let rot = e.rot();
        let pos = e.pos();
        let (eye, is_player, creative, head_yaw) = if let Some(m) = ents.mob(id) {
            (
                pos.y + crate::mob::eye_height(m),
                false,
                false,
                rot.y_head_rot,
            )
        } else {
            let p = ents.player(id)?;
            let info = e.kind().info();
            let eye =
                f64::from(info.eye_height) * f64::from(e.dims().height / info.height.max(0.01));
            (
                pos.y + eye,
                true,
                p.game_mode == GameMode::Creative,
                rot.y_rot,
            )
        };
        Some(Attacker {
            id,
            is_player,
            creative,
            eye: Vec3::new(pos.x, eye, pos.z),
            head_look: crate::mob::view_vector(rot.x_rot, head_yaw),
            look: crate::mob::view_vector(rot.x_rot, rot.y_rot),
            y_rot: rot.y_rot,
            known_movement: known_movement(ents, id),
        })
    }

    fn min_range(&self, r: &AttackRange) -> f32 {
        match (self.is_player, self.creative) {
            (true, true) => r.min_creative_reach,
            (true, false) => r.min_reach,
            _ => r.min_reach * r.mob_factor,
        }
    }

    fn max_range(&self, r: &AttackRange) -> f32 {
        match (self.is_player, self.creative) {
            (true, true) => r.max_creative_reach,
            (true, false) => r.max_reach,
            _ => r.max_reach * r.mob_factor,
        }
    }
}

/// The first passenger of `id` if it is a player (`getControllingPassenger() instanceof
/// Player`).
fn controlling_player(ents: &Entities, id: EntityId) -> Option<EntityId> {
    ents.riding
        .passengers_of(id)
        .first()
        .copied()
        .filter(|&p| ents.player(p).is_some())
}

/// `lastKnownClientMovement`: the player's last movement packet, kept until a client tick ends
/// without one.
fn player_movement(ents: &Entities, id: EntityId) -> Vec3 {
    ents.player(id)
        .map_or(Vec3::ZERO, |p| p.connection.movement.known_movement)
}

/// `getKnownMovement`: a player's own movement, a mob's velocity (or the movement of the
/// player controlling it).
pub(crate) fn known_movement(ents: &Entities, id: EntityId) -> Vec3 {
    if ents.player(id).is_some() {
        return player_movement(ents, id);
    }
    if let Some(p) = controlling_player(ents, id) {
        return player_movement(ents, p);
    }
    ents.store.get(id).map_or(Vec3::ZERO, |e| e.velocity())
}

/// `getKnownSpeed`: a mob's movement between its last two ticks, a player's movement.
pub(crate) fn known_speed(ents: &Entities, id: EntityId) -> Vec3 {
    if ents.player(id).is_some() {
        return player_movement(ents, id);
    }
    if let Some(p) = controlling_player(ents, id) {
        return player_movement(ents, p);
    }
    if let Some(m) = ents.mob(id) {
        return m.known_speed;
    }
    player_movement(ents, id)
}

/// `KineticWeapon.getMotion`: the known speed in blocks per second, of the root vehicle for
/// passengers that are not players.
fn motion(ents: &Entities, id: EntityId) -> Vec3 {
    let mut root = id;
    if ents.player(id).is_none() {
        while let Some(v) = ents.vehicle_of(root) {
            root = v;
        }
    }
    known_speed(ents, root) * 20.0
}

fn root_vehicle(ents: &Entities, id: EntityId) -> EntityId {
    let mut root = id;
    while let Some(v) = ents.vehicle_of(root) {
        root = v;
    }
    root
}

/// `PiercingWeapon.canHitEntity(jabber, target)`: living entities that are alive (not
/// spectators), boats, minecarts, armour stands and end crystals, not passengers of the
/// jabber's vehicle.
fn can_hit(ents: &Entities, jabber: EntityId, target: EntityId) -> bool {
    let Some(e) = ents.store.get(target) else {
        return false;
    };
    let pickable = if let Some(p) = ents.player(target) {
        p.game_mode != GameMode::Spectator && !p.survival.is_dead_or_dying()
    } else if let Some(m) = ents.mob(target) {
        m.is_alive()
    } else {
        use vanillite_data::entity::ids;
        let kind = e.kind();
        crate::vehicle::BoatKind::of(kind).is_some()
            || crate::minecart::MinecartKind::of(kind).is_some()
            || kind == ids::END_CRYSTAL
            || kind == ids::ARMOR_STAND
    };
    pickable && root_vehicle(ents, jabber) != root_vehicle(ents, target)
}

// --- The hit search ---------------------------------------------------------------------------

/// `ProjectileUtil.getHitEntitiesAlong(attacker, range, canHitEntity, COLLIDER)`: the entities
/// the reach touches in the entity order of the sections, each with the hit location, into
/// `out` (cleared). Empty when a block is in the way before the minimum reach.
pub fn hit_entities_along(
    level: &Level,
    ents: &Entities,
    a: &Attacker,
    range: &AttackRange,
    out: &mut Vec<(EntityId, Vec3)>,
) {
    out.clear();
    let look = a.head_look;
    let eye = a.eye;
    let from = eye + look * f64::from(a.min_range(range));
    let along = a.known_movement.dot(look);
    let mut to = eye + look * (f64::from(a.max_range(range)) + along.max(0.0));
    if let Some((_, hit)) = crate::mob::physics::clip(level, eye, to) {
        to = hit;
        if eye.distance_to_sqr(to) < eye.distance_to_sqr(from) {
            return;
        }
    }
    let margin = f64::from(range.hitbox_margin);
    let half = margin / 2.0;
    let d = to - from;
    let area = Aabb::new(
        from.x - half,
        from.y - half,
        from.z - half,
        from.x + half,
        from.y + half,
        from.z + half,
    )
    .expand_towards(d.x, d.y, d.z)
    .inflate(1.0, 1.0, 1.0);
    let mut candidates = Vec::new();
    ents.store.for_each_in(&area, false, |id| {
        if id != a.id {
            candidates.push(id);
        }
    });
    for id in candidates {
        if !can_hit(ents, a.id, id) {
            continue;
        }
        let Some(bb) = ents.store.get(id).map(|e| e.aabb()) else {
            continue;
        };
        if bb.contains(from.x, from.y, from.z) {
            out.push((id, from));
        } else if let Some(hit) = crate::mob::projectile::aabb_clip(&bb, from, to) {
            out.push((id, hit));
        } else if margin > 0.0
            && let Some(outside) =
                crate::mob::projectile::aabb_clip(&bb.inflate(margin, margin, margin), from, to)
        {
            let mut towards = bb.center();
            if let Some((_, hit)) = crate::mob::physics::clip(level, outside, towards) {
                towards = hit;
            }
            if let Some(surface) = crate::mob::projectile::aabb_clip(&bb, outside, towards) {
                out.push((id, surface));
            }
        }
    }
}

// --- Stabbing ---------------------------------------------------------------------------------

/// The `damage_type` of the weapon (`ItemStack.getDamageSource`): spears deal `spear`
/// damage, other items the attacker's own (`mobAttack`, `playerAttack`).
fn weapon_source(
    ents: &Entities,
    env: &Env<'_>,
    attacker: EntityId,
    weapon: &ItemStack,
) -> DamageSource {
    let player = ents.player(attacker).is_some();
    // `getItemDamageSource`: the stack's `damage_type`.
    let own = if weapon.is_empty() {
        ""
    } else {
        item::item_id(weapon).info().damage_type
    };
    let kind = Some(own)
        .filter(|t| !t.is_empty())
        .and_then(|t| env.types.kind(t))
        .unwrap_or(if player {
            env.types.player_attack
        } else {
            env.types.mob_attack
        });
    let mut source = combat::entity_source(ents, kind, attacker, attacker);
    if player {
        source.cause_is_mob = false;
    }
    source
}

/// `getKnockback(target, source)`: the attack knockback through the weapon's enchantments,
/// halved.
fn knockback_amount(
    ents: &Entities,
    env: &mut Env<'_>,
    attacker: EntityId,
    target: EntityId,
    weapon: &ItemStack,
    source: &DamageSource,
) -> f32 {
    #[allow(clippy::cast_possible_truncation)]
    let base = if ents.player(attacker).is_some() {
        player_attribute(ents, attacker, "minecraft:attack_knockback", 0.0, 0.0, 5.0) as f32
    } else {
        ents.mob(attacker)
            .map_or(0.0, |m| m.attrs.attack_knockback as f32)
    };
    let combat = combat_data();
    let (enchants, tags): (
        &crate::enchant::Enchantments,
        &dyn crate::predicate::EntityTags,
    ) = (&combat.enchants, &*combat);
    let ecx = damage_context(ents, env.types, tags, target, source);
    enchants.modify_knockback(weapon, &ecx, env.random, base) / 2.0
}

/// `causeExtraKnockback(target, amount, ...)`: the player's version also stops sprinting and
/// tells a hit player its new motion.
fn cause_extra_knockback(
    ents: &mut Entities,
    env: &mut Env<'_>,
    a: &Attacker,
    target: EntityId,
    amount: f32,
    old_movement: Option<Vec3>,
    by: (f32, bool),
) {
    let by = Some((a.id, by.0, by.1));
    if a.is_player {
        extra_knockback(ents, env, a.id, target, amount, a.y_rot, old_movement, by);
        return;
    }
    let living = ents.mob(target).is_some() || ents.player(target).is_some();
    if amount > 0.0 && living {
        let rad = f64::from(a.y_rot * crate::mob::DEG_TO_RAD);
        let (sin, cos) = (
            f64::from(vanillite_base::math::sin(rad)),
            f64::from(vanillite_base::math::cos(rad)),
        );
        combat::knock_living(ents, env, target, f64::from(amount), sin, -cos, by);
        if let Some(mut e) = ents.store.get_mut(a.id) {
            let v = e.as_ref().velocity();
            e.set_velocity(v.multiply(0.6, 1.0, 0.6));
        }
    }
}

/// `LivingEntity.stabAttack(slot, target, baseDamage, dealsDamage, dealsKnockback,
/// dismounts)`: whether the target was affected.
#[allow(clippy::too_many_arguments)]
pub fn stab_attack(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    cx: &mut MobContext,
    a: &Attacker,
    weapon: &ItemStack,
    target: EntityId,
    base_damage: f32,
    deals_damage: bool,
    deals_knockback: bool,
    dismounts: bool,
) -> bool {
    let source = weapon_source(ents, env, a.id, weapon);
    let combat = combat_data();
    let (enchants, tags): (
        &crate::enchant::Enchantments,
        &dyn crate::predicate::EntityTags,
    ) = (&combat.enchants, &*combat);
    let damage = {
        let ecx = damage_context(ents, env.types, tags, target, &source);
        enchants.modify_damage(weapon, &ecx, env.random, base_damage)
    };
    let old_movement = ents.store.get(target).map(|e| e.velocity());
    let dealt = deals_damage && combat::hurt(level, ents, env, cx, target, source, damage);
    let mut affected = deals_knockback | dealt;
    if deals_knockback {
        cause_extra_knockback(ents, env, a, target, 0.4, old_movement, (damage, false));
        let amount = knockback_amount(ents, env, a.id, target, weapon, &source);
        cause_extra_knockback(ents, env, a, target, amount, old_movement, (damage, true));
    }
    let dismountable = ents.store.get(target).is_some_and(|e| {
        !crate::effect::entity_in_tag(e.kind(), "minecraft:cannot_be_dismounted_by_item_usage")
    });
    if dismounts && ents.vehicle_of(target).is_some() && dismountable {
        affected = true;
        crate::riding::stop_riding(ents, env, target);
    }
    crate::mob::attack::weapon_used_stat(ents, a.id, weapon, target);
    if dealt {
        post_attack(level, ents, env, cx, target, &source, Some(weapon));
    }
    if !affected {
        return false;
    }
    // `setLastHurtMob`.
    if let Some(m) = ents.mob_mut(a.id) {
        m.last_hurt_mob = Some(target);
    } else if let Some(p) = ents.player_mut(a.id) {
        p.survival.last_hurt_mob = Some((target, p.survival.tick_count));
    }
    true
}

/// The stack and the recent kinetic enemies of a user.
fn stabbed_mut(ents: &mut Entities, id: EntityId) -> Option<&mut Vec<(EntityId, i64)>> {
    if ents.mob(id).is_some() {
        return ents.mob_mut(id).map(|m| &mut m.stabbed);
    }
    ents.player_mut(id).map(|p| &mut p.survival.stabbed)
}

/// `wasRecentlyStabbed(target, allowedTime)`.
fn was_recently_stabbed(
    ents: &Entities,
    id: EntityId,
    target: EntityId,
    now: i64,
    allowed: i32,
) -> bool {
    let list = if let Some(m) = ents.mob(id) {
        &m.stabbed
    } else if let Some(p) = ents.player(id) {
        &p.survival.stabbed
    } else {
        return false;
    };
    list.iter()
        .find(|(e, _)| *e == target)
        .is_some_and(|&(_, t)| now - t < i64::from(allowed))
}

/// `rememberStabbedEntity(target)`.
fn remember_stabbed(ents: &mut Entities, id: EntityId, target: EntityId, now: i64) {
    if let Some(list) = stabbed_mut(ents, id) {
        if let Some(slot) = list.iter_mut().find(|(e, _)| *e == target) {
            slot.1 = now;
        } else {
            list.push((target, now));
        }
    }
}

/// `KineticWeapon.damageEntities(stack, ticksRemaining, entity, slot)` with the ticks the
/// weapon has been used (`getUseDuration - ticksRemaining`).
#[allow(clippy::too_many_arguments)]
pub fn damage_entities(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    cx: &mut MobContext,
    user: EntityId,
    weapon: &ItemStack,
    kinetic: &KineticWeapon,
    ticks_used: i32,
) {
    if ticks_used < kinetic.delay_ticks {
        return;
    }
    let ticks_used = ticks_used - kinetic.delay_ticks;
    let Some(a) = Attacker::of(ents, user) else {
        return;
    };
    let attacker_speed = a.look.dot(motion(ents, user));
    let factor = if a.is_player { 1.0 } else { f64::from(0.2_f32) };
    let range = attack_range(weapon, 3.0);
    #[allow(clippy::cast_possible_truncation)]
    let base_damage = if a.is_player {
        1.0
    } else {
        ents.mob(user).map_or(0.0, |m| m.attrs.attack_damage as f32)
    };
    let mut hits = Vec::new();
    hit_entities_along(level, ents, &a, &range, &mut hits);
    let mut affected = false;
    let now = env.game_time;
    for (target, _) in hits {
        if was_recently_stabbed(ents, user, target, now, kinetic.contact_cooldown_ticks) {
            continue;
        }
        remember_stabbed(ents, user, target, now);
        let target_speed = a.look.dot(motion(ents, target));
        let relative = (attacker_speed - target_speed).max(0.0);
        let test = |c: Option<Condition>| {
            c.is_some_and(|c| c.test(ticks_used, attacker_speed, relative, factor))
        };
        let (dismount, knockback, damage) = (
            test(kinetic.dismount),
            test(kinetic.knockback),
            test(kinetic.damage),
        );
        if dismount || knockback || damage {
            #[allow(clippy::cast_possible_truncation)]
            let dealt = base_damage
                + vanillite_base::math::floor(relative * f64::from(kinetic.damage_multiplier))
                    as f32;
            affected |= stab_attack(
                level, ents, env, cx, &a, weapon, target, dealt, damage, knockback, dismount,
            );
        }
    }
    if affected {
        // `broadcastEntityEvent(entity, 2)`: the kinetic hit feedback.
        if let Some(network_id) = ents.store.get(user).map(|e| e.network_id().0) {
            env.out.push(Effect::Tracking {
                entity: user,
                with_self: a.is_player,
                packet: SharedPacket::of(&EntityEvent {
                    entity_id: network_id,
                    event: 2,
                }),
            });
        }
        // `SPEAR_MOBS_TRIGGER` with the living entities stabbed during this use (a killed mob
        // stays in the world until its death animation ends; what is gone was no mob).
        if a.is_player {
            let count = ents.player(user).map_or(0, |p| {
                p.survival
                    .stabbed
                    .iter()
                    .filter(|(e, _)| {
                        ents.store
                            .get(*e)
                            .is_some_and(|e| crate::block::plate::is_living(e.kind()))
                    })
                    .count()
            });
            let count = i32::try_from(count).unwrap_or(i32::MAX);
            if let Some(p) = ents.player_mut(user) {
                p.advancements
                    .trigger(crate::advancements::trigger::Event::SpearMobs(count));
            }
        }
    }
}

/// The sound source of an entity (`getSoundSource`).
fn sound_source(ents: &Entities, id: EntityId) -> SoundSource {
    ents.mob(id)
        .map_or(SoundSource::Players, |m| m.sound_source())
}

/// `makeSound(causer)` of the weapon components: heard by everyone but the causer.
pub fn make_sound(ents: &Entities, env: &mut Env<'_>, causer: EntityId, sound: Option<&Sound>) {
    let (Some(sound), Some(pos)) = (sound, ents.store.get(causer).map(|e| e.pos())) else {
        return;
    };
    let seed = crate::survival::sound_seed();
    let source = sound_source(ents, causer);
    play_sound(env.out, Some(causer), pos, sound, source, 1.0, 1.0, seed);
}

/// `PiercingWeapon.makeHitSound(causer)`: heard by everyone.
fn make_hit_sound(ents: &Entities, env: &mut Env<'_>, causer: EntityId, sound: Option<&Sound>) {
    let (Some(sound), Some(pos)) = (sound, ents.store.get(causer).map(|e| e.pos())) else {
        return;
    };
    let seed = crate::survival::sound_seed();
    let source = sound_source(ents, causer);
    play_sound(env.out, None, pos, sound, source, 1.0, 1.0, seed);
}

// --- Players ----------------------------------------------------------------------------------

/// `handlePlayerAction(STAB)`: a player jabs with the piercing weapon in the main hand unless
/// the attack is not charged enough (`cannotAttackWithItem(item, 5)`).
pub fn player_stab(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    cx: &mut MobContext,
    player: EntityId,
) {
    let Some(p) = ents.player(player) else {
        return;
    };
    if p.game_mode == GameMode::Spectator {
        return;
    }
    let weapon = p.inventory.selected_item().clone();
    #[allow(clippy::cast_precision_loss)]
    let ticker = p.survival.attack_ticker as f32;
    let required = minimum_attack_charge(&weapon);
    let optimistic = (ticker + 5.0) / attack_strength_delay(ents, player);
    if required > 0.0 && optimistic < required {
        return;
    }
    let Some(piercing) = piercing_weapon(&weapon) else {
        return;
    };
    piercing_attack(level, ents, env, cx, player, &weapon, &piercing);
}

/// `PiercingWeapon.attack(attacker, MAINHAND)`.
fn piercing_attack(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    cx: &mut MobContext,
    attacker: EntityId,
    weapon: &ItemStack,
    piercing: &PiercingWeapon,
) {
    let Some(a) = Attacker::of(ents, attacker) else {
        return;
    };
    #[allow(clippy::cast_possible_truncation)]
    let damage = if a.is_player {
        player_attribute(ents, attacker, "minecraft:attack_damage", 1.0, 0.0, 2048.0) as f32
    } else {
        ents.mob(attacker)
            .map_or(0.0, |m| m.attrs.attack_damage as f32)
    };
    let range = attack_range(weapon, 3.0);
    let mut hits = Vec::new();
    hit_entities_along(level, ents, &a, &range, &mut hits);
    let mut hit_something = false;
    for (target, _) in hits {
        hit_something |= stab_attack(
            level,
            ents,
            env,
            cx,
            &a,
            weapon,
            target,
            damage,
            true,
            piercing.deals_knockback,
            piercing.dismounts,
        );
    }
    // `onAttack`: `resetOnlyAttackStrengthTicker`.
    if let Some(p) = ents.player_mut(attacker) {
        p.survival.attack_ticker = 0;
    }
    if hit_something {
        make_hit_sound(ents, env, attacker, piercing.hit_sound.as_ref());
    }
    make_sound(ents, env, attacker, piercing.sound.as_ref());
    // `swingAndResetAttackStrength(MAIN_HAND, stab animation, false)`.
    if let Some(network_id) = ents.store.get(attacker).map(|e| e.network_id().0) {
        env.out.push(Effect::Tracking {
            entity: attacker,
            with_self: false,
            packet: SharedPacket::of(&SwingAnimation {
                entity_id: network_id,
                hand: Hand::MainHand,
                kind: 2,
                duration: stab_swing_ticks(weapon),
            }),
        });
    }
}

/// `onUseTick` of the kinetic weapons players use (after
/// [`crate::player::using::tick`]): every player charging with a spear damages what its
/// reach touches.
pub fn tick_players(level: &Level, ents: &mut Entities, env: &mut Env<'_>, cx: &mut MobContext) {
    let users: Vec<(EntityId, crate::player::using::UsingItem)> = ents
        .players
        .iter()
        .filter_map(|(id, p)| p.survival.using_item.map(|u| (id, u)))
        .collect();
    for (id, u) in users {
        let Some(p) = ents.player(id) else {
            continue;
        };
        let index = match u.hand {
            Hand::MainHand => p.inventory.selected,
            Hand::OffHand => crate::inventory::OFFHAND,
        };
        let weapon = p.inventory.get(index).clone();
        if weapon.item != u.item {
            continue;
        }
        let Some(kinetic) = kinetic_weapon(&weapon) else {
            continue;
        };
        // `onUseTick` sees the ticks used before this one.
        damage_entities(level, ents, env, cx, id, &weapon, &kinetic, u.ticks - 1);
    }
}

// --- Mobs -------------------------------------------------------------------------------------

/// `onUseTick` of a mob's main hand kinetic weapon (`used` ticks used before this one).
pub fn mob_use_tick(t: &mut crate::mob::Tick<'_, '_>, used: i32) {
    let weapon = t
        .mob_ref()
        .equipment
        .get(item::EquipmentSlot::MainHand)
        .clone();
    let Some(kinetic) = kinetic_weapon(&weapon) else {
        return;
    };
    let id = t.id;
    damage_entities(t.level, t.ents, t.env, t.cx, id, &weapon, &kinetic, used);
}

/// `computeDamageUseDuration` of the mob's main hand kinetic weapon (0 without one).
pub fn mob_use_duration(m: &crate::mob::MobData) -> i32 {
    kinetic_weapon(m.equipment.get(item::EquipmentSlot::MainHand))
        .map_or(0, |k| k.damage_use_duration())
}

/// Whether the mob holds a kinetic weapon in the main hand.
pub fn mob_holds_kinetic(m: &crate::mob::MobData) -> bool {
    is_kinetic(m.equipment.get(item::EquipmentSlot::MainHand))
}

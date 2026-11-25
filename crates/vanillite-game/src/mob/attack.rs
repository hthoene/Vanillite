//! `Player.attack`: melee attacks of players on mobs and players with the held item's attack
//! damage and speed, the attack cooldown, critical hits, sweeping, knockback and the combat
//! effects of enchantments (damage, knockback, post attack effects like Fire Aspect), and the
//! weapon's durability.

use vanillite_base::Vec3;
use vanillite_base::random::RandomSource;
use vanillite_protocol::SharedPacket;
use vanillite_protocol::packets::play::clientbound::{Animate, SetEntityMotion, SoundSource};
use vanillite_protocol::types::LpVec3;
use vanillite_world::entity::{EntityFlags, EntityId};

use super::MobContext;
use super::combat::{self, entity_source};
use crate::enchant::{EnchantContext, Enchantments};
use crate::entities::Entities;
use crate::item::{self, EquipmentSlot, ItemStack};
use crate::level::Level;
use crate::predicate::{EntityFacts, EntityTags, SourceFacts};
use crate::survival::damage::DamageSource;
use crate::survival::player::{self, Env};
use crate::survival::{Effect, play_sound};

/// Entity type tags when no data packs are loaded.
struct NoTags;

impl EntityTags for NoTags {
    fn entity_in_tag(&self, _: vanillite_data::entity::EntityTypeId, _: &str) -> bool {
        false
    }
}

static NO_ENCHANTS: Enchantments = Enchantments { list: Vec::new() };

/// The loaded enchantments and entity tags (empty without data packs).
pub fn combat_data() -> (&'static Enchantments, &'static dyn EntityTags) {
    match crate::player::play::game_data() {
        Some(d) => (&d.enchants, d),
        None => (&NO_ENCHANTS, &NoTags),
    }
}

/// The facts predicates test of a living entity.
pub fn facts(ents: &Entities, id: EntityId) -> Option<EntityFacts> {
    let e = ents.store.get(id)?;
    let flags = e.flags();
    let sheep = ents
        .mob(id)
        .filter(|m| m.kind == super::Kind::Sheep)
        .map(|m| (m.sheep_color & 15, m.sheared));
    let (on_fire, baby) = if let Some(m) = ents.mob(id) {
        (m.is_on_fire(), m.baby)
    } else if let Some(p) = ents.player(id) {
        (p.survival.fire_ticks > 0, false)
    } else {
        (flags.contains(EntityFlags::ON_FIRE), false)
    };
    Some(EntityFacts {
        kind: Some(e.kind()),
        on_fire,
        baby,
        sneaking: flags.contains(EntityFlags::CROUCHING),
        sprinting: flags.contains(EntityFlags::SPRINTING),
        swimming: flags.contains(EntityFlags::SWIMMING),
        on_ground: flags.contains(EntityFlags::ON_GROUND),
        smelts_loot: ents.player(id).is_some_and(|p| {
            crate::player::play::game_data().is_some_and(|d| {
                d.enchantment_level(p.inventory.selected_item(), "minecraft:fire_aspect") > 0
            })
        }),
        sheep_color: sheep.map(|s| s.0),
        sheared: sheep.is_some_and(|s| s.1),
    })
}

/// `Enchantment.damageContext(victim, source)`.
pub fn damage_context<'a>(
    ents: &Entities,
    types: &'a crate::survival::damage::DamageTypes,
    tags: &'a dyn EntityTags,
    victim: EntityId,
    source: &DamageSource,
) -> EnchantContext<'a> {
    let attacker = source.cause.and_then(|c| facts(ents, c));
    let direct = source.direct.and_then(|c| facts(ents, c));
    let attacking_player = source
        .cause
        .filter(|&c| ents.player(c).is_some())
        .and_then(|c| facts(ents, c));
    EnchantContext {
        types,
        tags,
        this: facts(ents, victim),
        attacker,
        direct_attacker: direct,
        attacking_player,
        source: Some(SourceFacts {
            kind: source.kind,
            is_direct: source.cause.is_some() && source.cause == source.direct,
            direct,
            cause: attacker,
        }),
    }
}

/// A player's attribute value from the base and the modifiers of its equipment.
pub fn player_attribute(
    ents: &Entities,
    player: EntityId,
    name: &str,
    base: f64,
    min: f64,
    max: f64,
) -> f64 {
    let Some(p) = ents.player(player) else {
        return base;
    };
    let mut mods = Vec::new();
    for slot in crate::player::encode::EQUIPMENT_SLOTS {
        item::attribute_modifiers(p.inventory.equipment(slot), slot, name, &mut mods);
    }
    item::attribute_value(base, &mods, min, max)
}

/// `getCurrentItemAttackStrengthDelay`: ticks until a full-strength attack.
pub fn attack_strength_delay(ents: &Entities, player: EntityId) -> f32 {
    let speed = player_attribute(ents, player, "minecraft:attack_speed", 4.0, 0.0, 1024.0);
    #[allow(clippy::cast_possible_truncation)]
    let delay = (1.0 / speed * 20.0) as f32;
    delay
}

/// `getAttackStrengthScale(a)`.
pub fn attack_strength_scale(ents: &Entities, player: EntityId, a: f32) -> f32 {
    let Some(p) = ents.player(player) else {
        return 1.0;
    };
    #[allow(clippy::cast_precision_loss)]
    let ticker = p.survival.attack_ticker as f32;
    ((ticker + a) / attack_strength_delay(ents, player)).clamp(0.0, 1.0)
}

/// Plays a player sound at the player's position, heard by everyone
/// (`playServerSideSound`).
fn player_sound(env: &mut Env<'_>, pos: Vec3, sound: &str) {
    let seed = crate::survival::sound_seed();
    play_sound(
        env.out,
        None,
        pos,
        sound,
        SoundSource::Players,
        1.0,
        1.0,
        seed,
    );
}

/// Whether `id` is a living entity that is alive.
fn is_alive(ents: &Entities, id: EntityId) -> bool {
    ents.mob(id).is_some_and(super::MobData::is_alive)
        || ents
            .player(id)
            .is_some_and(|p| !p.survival.is_dead_or_dying())
}

/// `Player.attack(entity)`.
pub fn player_attack(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    cx: &mut MobContext,
    attacker: EntityId,
    target: EntityId,
) {
    if attacker == target || !is_alive(ents, target) {
        return;
    }
    let Some(e) = ents.store.get(attacker) else {
        return;
    };
    let ppos = e.pos();
    let flags = e.flags();
    let y_rot = e.rot().y_rot;
    let known_movement = e.pos() - e.old_pos();
    let Some(p) = ents.player(attacker) else {
        return;
    };
    let weapon = p.inventory.selected_item().clone();
    let fall_distance = p.survival.fall_distance;
    let in_water = p.survival.fluids.in_water();
    let (enchants, tags) = combat_data();

    let base = player_attribute(ents, attacker, "minecraft:attack_damage", 1.0, 0.0, 2048.0);
    #[allow(clippy::cast_possible_truncation)]
    let mut base_damage = base as f32;
    let mut source = entity_source(ents, env.types.player_attack, attacker, attacker);
    source.cause_is_mob = false;
    source.disable_shield_seconds = item::item_id(&weapon)
        .info()
        .weapon
        .map_or(0.0, |w| w.disable_blocking_for_seconds);
    let scale = attack_strength_scale(ents, attacker, 0.5);
    let enchanted = {
        let ecx = damage_context(ents, env.types, tags, target, &source);
        enchants.modify_damage(&weapon, &ecx, env.random, base_damage)
    };
    let magic_boost = scale * (enchanted - base_damage);
    base_damage *= 0.2 + scale * scale * 0.8;
    // `onAttack`: `resetOnlyAttackStrengthTicker`.
    if let Some(p) = ents.player_mut(attacker) {
        p.survival.attack_ticker = 0;
    }
    if base_damage <= 0.0 && magic_boost <= 0.0 {
        return;
    }
    let full = scale > 0.9;
    let sprinting = flags.contains(EntityFlags::SPRINTING);
    let on_ground = flags.contains(EntityFlags::ON_GROUND);
    let knockback_attack = sprinting && full;
    if knockback_attack {
        player_sound(env, ppos, "minecraft:entity.player.attack.knockback");
    }
    base_damage += mace_bonus(ents, env, attacker, target, &weapon, &source, fall_distance);
    let passenger = ents.vehicle_of(attacker).is_some();
    let crit = full && fall_distance > 0.0 && !on_ground && !in_water && !passenger && !sprinting;
    if crit {
        base_damage *= 1.5;
    }
    let total = base_damage + magic_boost;
    // `isSweepAttack`: a full strength sword hit on the ground while not moving fast.
    let sweep = full && !crit && !knockback_attack && on_ground && {
        let speed = if sprinting { 0.13 } else { 0.1 };
        let max = speed * 2.5;
        known_movement.x * known_movement.x + known_movement.z * known_movement.z < max * max
            && crate::player::play::game_data()
                .is_some_and(|d| d.item_in_tag(weapon.item, "minecraft:swords"))
    };
    let old_movement = ents.store.get(target).map(|e| e.velocity());
    let hurt = combat::hurt(level, ents, env, cx, target, source, total);
    if !hurt {
        player_sound(env, ppos, "minecraft:entity.player.attack.nodamage");
        return;
    }
    // `getKnockback`: the attack knockback attribute through the enchantments, halved.
    let kb_base = player_attribute(ents, attacker, "minecraft:attack_knockback", 0.0, 0.0, 5.0);
    #[allow(clippy::cast_possible_truncation)]
    let kb = {
        let ecx = damage_context(ents, env.types, tags, target, &source);
        enchants.modify_knockback(&weapon, &ecx, env.random, kb_base as f32) / 2.0
    };
    let kb = kb + if knockback_attack { 0.5 } else { 0.0 };
    extra_knockback(ents, env, attacker, target, kb, y_rot, old_movement);
    if sweep {
        sweep_attack(
            level,
            ents,
            env,
            cx,
            attacker,
            target,
            base_damage,
            &source,
            scale,
        );
    }
    let network_id = ents.store.get(target).map_or(0, |e| e.network_id().0);
    if crit {
        player_sound(env, ppos, "minecraft:entity.player.attack.crit");
        animate(env, target, network_id, 4);
    }
    if !crit && !sweep {
        player_sound(
            env,
            ppos,
            if full {
                "minecraft:entity.player.attack.strong"
            } else {
                "minecraft:entity.player.attack.weak"
            },
        );
    }
    if magic_boost > 0.0 {
        animate(env, target, network_id, 5);
    }
    if let Some(p) = ents.player_mut(attacker) {
        p.survival.last_hurt_mob = Some((target, p.survival.tick_count));
    }
    post_attack(ents, env, target, &source, &weapon);
    hurt_weapon(ents, env, attacker, &weapon);
    if let Some(p) = ents.player_mut(attacker) {
        player::cause_food_exhaustion(p, 0.1);
    }
}

/// `sendToTrackingPlayersAndSelf(new ClientboundAnimatePacket(entity, action))`.
fn animate(env: &mut Env<'_>, target: EntityId, network_id: i32, action: u8) {
    env.out.push(Effect::Tracking {
        entity: target,
        with_self: true,
        packet: SharedPacket::of(&Animate {
            entity_id: network_id,
            action,
        }),
    });
}

/// `MaceItem.getAttackDamageBonus`: smash attacks add damage per fallen block.
fn mace_bonus(
    ents: &Entities,
    env: &mut Env<'_>,
    attacker: EntityId,
    target: EntityId,
    weapon: &ItemStack,
    source: &DamageSource,
    fall_distance: f64,
) -> f32 {
    if item::item_id(weapon).name() != "minecraft:mace" {
        return 0.0;
    }
    let on_ground = ents
        .store
        .get(attacker)
        .is_some_and(|e| e.flags().contains(EntityFlags::ON_GROUND));
    // `canSmashAttack`: falling more than 1.5 blocks, not gliding.
    if fall_distance <= 1.5 || on_ground {
        return 0.0;
    }
    let f = fall_distance;
    let bonus = if f <= 3.0 {
        4.0 * f
    } else if f <= 8.0 {
        12.0 + 2.0 * (f - 3.0)
    } else {
        22.0 + f - 8.0
    };
    let (enchants, tags) = combat_data();
    let ecx = damage_context(ents, env.types, tags, target, source);
    #[allow(clippy::cast_possible_truncation)]
    let per_block = enchants.modify_fall_based_damage(weapon, &ecx, env.random, 0.0) * f as f32;
    #[allow(clippy::cast_possible_truncation)]
    let b = bonus as f32 + per_block;
    b
}

/// `causeExtraKnockback`: knocks the target back in the attacker's facing and slows the
/// attacker; a hit player gets its new motion at once.
fn extra_knockback(
    ents: &mut Entities,
    env: &mut Env<'_>,
    attacker: EntityId,
    target: EntityId,
    amount: f32,
    y_rot: f32,
    old_movement: Option<Vec3>,
) {
    if amount > 0.0 {
        let rad = f64::from(y_rot) * std::f64::consts::PI / 180.0;
        let (sin, cos) = (
            f64::from(vanillite_base::math::sin(rad)),
            f64::from(vanillite_base::math::cos(rad)),
        );
        // Dragon parts are not living entities: no knockback from the attack itself.
        let part = ents
            .mob(target)
            .is_some_and(|m| m.kind == super::Kind::EnderDragon);
        if !part {
            combat::knock_living(ents, env, target, f64::from(amount), sin, -cos);
        }
        if let Some(mut e) = ents.store.get_mut(attacker) {
            let v = e.as_ref().velocity();
            e.set_velocity(v.multiply(0.6, 1.0, 0.6));
            e.set_flags(EntityFlags::SPRINTING, false);
        }
    }
    // A hit player learns its knockback right away (`syncVelocity`).
    if ents.player(target).is_some()
        && let Some(mut e) = ents.store.get_mut(target)
        && e.as_ref().flags().contains(EntityFlags::HURT_MARKED)
    {
        let v = e.as_ref().velocity();
        let id = e.as_ref().network_id().0;
        e.set_flags(EntityFlags::HURT_MARKED, false);
        if let Some(old) = old_movement {
            e.set_velocity(old);
        }
        env.out.push(Effect::To {
            player: target,
            packet: SharedPacket::of(&SetEntityMotion {
                entity_id: id,
                movement: LpVec3(vanillite_protocol::types::Vec3 {
                    x: v.x,
                    y: v.y,
                    z: v.z,
                }),
            }),
        });
    }
}

/// `doSweepAttack`: the living entities around the target take sweeping damage.
#[allow(clippy::too_many_arguments)]
fn sweep_attack(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    cx: &mut MobContext,
    attacker: EntityId,
    target: EntityId,
    base_damage: f32,
    source: &DamageSource,
    scale: f32,
) {
    let Some(ppos) = ents.store.get(attacker).map(|e| e.pos()) else {
        return;
    };
    let y_rot = ents.store.get(attacker).map_or(0.0, |e| e.rot().y_rot);
    player_sound(env, ppos, "minecraft:entity.player.attack.sweep");
    let ratio = player_attribute(
        ents,
        attacker,
        "minecraft:sweeping_damage_ratio",
        0.0,
        0.0,
        1.0,
    );
    #[allow(clippy::cast_possible_truncation)]
    let sweep_damage = 1.0 + ratio as f32 * base_damage;
    let Some(area) = ents
        .store
        .get(target)
        .map(|e| e.aabb().inflate(1.0, 0.25, 1.0))
    else {
        return;
    };
    let mut near = Vec::new();
    ents.store.for_each_in(&area, false, |e| near.push(e));
    let weapon = ents
        .player(attacker)
        .map(|p| p.inventory.selected_item().clone())
        .unwrap_or_default();
    let (enchants, tags) = combat_data();
    for id in near {
        if id == attacker || id == target || !is_alive(ents, id) {
            continue;
        }
        let Some(pos) = ents.store.get(id).map(|e| e.pos()) else {
            continue;
        };
        if pos.distance_to_sqr(ppos) >= 9.0 {
            continue;
        }
        let damage = {
            let ecx = damage_context(ents, env.types, tags, id, source);
            enchants.modify_damage(&weapon, &ecx, env.random, sweep_damage) * scale
        };
        if combat::hurt(level, ents, env, cx, id, *source, damage) {
            let rad = f64::from(y_rot) * std::f64::consts::PI / 180.0;
            combat::knock_living(
                ents,
                env,
                id,
                0.4,
                f64::from(vanillite_base::math::sin(rad)),
                -f64::from(vanillite_base::math::cos(rad)),
            );
            post_attack(ents, env, id, source, &weapon);
        }
    }
}

/// `EnchantmentHelper.doPostAttackEffectsWithItemSource`: the weapon's post attack effects on
/// the victim (Fire Aspect sets it on fire).
pub fn post_attack(
    ents: &mut Entities,
    env: &mut Env<'_>,
    victim: EntityId,
    source: &DamageSource,
    weapon: &ItemStack,
) {
    let (enchants, tags) = combat_data();
    let seconds = {
        let ecx = damage_context(ents, env.types, tags, victim, source);
        enchants.post_attack_ignite(weapon, &ecx, env.random)
    };
    if let Some(s) = seconds {
        #[allow(clippy::cast_possible_truncation)]
        ignite(ents, victim, (s * 20.0) as i32);
    }
}

/// `igniteForTicks`: burns at least `ticks` more ticks.
pub fn ignite(ents: &mut Entities, id: EntityId, ticks: i32) {
    if let Some(m) = ents.mob_mut(id) {
        if m.fire_ticks < ticks {
            m.fire_ticks = ticks;
        }
    } else if let Some(p) = ents.player_mut(id)
        && p.survival.fire_ticks < ticks
    {
        p.survival.fire_ticks = ticks;
    }
}

/// `ItemStack.hurtEnemy` / `postHurtEnemy`: the weapon loses `item_damage_per_attack`
/// durability (with Unbreaking) unless the player is in creative mode.
fn hurt_weapon(ents: &mut Entities, env: &mut Env<'_>, attacker: EntityId, weapon: &ItemStack) {
    let Some(w) = item::item_id(weapon).info().weapon else {
        return;
    };
    let unbreaking = crate::player::play::game_data()
        .map_or(0, |d| d.enchantment_level(weapon, "minecraft:unbreaking"));
    let mut amount: i32 = 0;
    for _ in 0..w.item_damage_per_attack {
        #[allow(clippy::cast_precision_loss)]
        let chance = 1.0 / (unbreaking.max(0) as f32 + 1.0);
        if unbreaking <= 0 || env.random.next_float() < chance {
            amount = amount.saturating_add(1);
        }
    }
    let pos = ents.store.get(attacker).map(|e| e.pos());
    let Some(p) = ents.player_mut(attacker) else {
        return;
    };
    if p.game_mode == vanillite_config::GameMode::Creative || amount == 0 {
        return;
    }
    let Some(tool) = p.inventory.selected_item_mut() else {
        return;
    };
    if !item::is_damageable(tool) {
        return;
    }
    let new_damage = item::damage(tool).saturating_add(amount);
    if new_damage >= item::max_damage(tool) {
        tool.shrink(1);
        if let Some(pos) = pos {
            player_sound(env, pos, "minecraft:entity.item.break");
        }
    } else {
        item::set_damage(tool, new_damage);
    }
}

/// The equipment of a player for armour and protection (`getArmorSlots` plus the hands).
pub fn equipment(ents: &Entities, player: EntityId) -> Vec<(EquipmentSlot, ItemStack)> {
    let Some(p) = ents.player(player) else {
        return Vec::new();
    };
    crate::player::encode::EQUIPMENT_SLOTS
        .iter()
        .map(|&s| (s, p.inventory.equipment(s).clone()))
        .filter(|(_, s)| !s.is_empty())
        .collect()
}

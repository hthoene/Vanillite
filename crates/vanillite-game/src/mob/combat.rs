//! Damage, knockback and death of mobs (`LivingEntity.hurtServer`, `actuallyHurt`,
//! `knockback`, `die`), melee attacks of mobs (`Mob.doHurtTarget`) and of players
//! (`Player.attack`), and experience drops (`ExperienceOrb.award`).

use vanillite_base::Vec3;
use vanillite_base::random::{LegacyRandom, RandomSource};
use vanillite_protocol::SharedPacket;
use vanillite_protocol::packets::play::clientbound::{DamageEvent, EntityEvent, SoundSource};
use vanillite_world::entity::{EntityFlags, EntityId, Pose};

use super::living::event;
use super::{Kind, MobContext, kinds, projectile};
use crate::entities::Entities;
use crate::level::Level;
use crate::survival::damage::{DamageKind, DamageSource, tag};
use crate::survival::player::{self, Env};
use crate::survival::rules::Rule;
use crate::survival::{Effect, play_sound};

/// `LivingEntity.knockback(power, xd, zd)`: the new velocity, `None` when the resistance
/// absorbs it.
pub fn knockback(
    velocity: Vec3,
    on_ground: bool,
    power: f64,
    xd: f64,
    zd: f64,
    resistance: f64,
    random: &mut LegacyRandom,
) -> Option<Vec3> {
    let power = power * (1.0 - resistance);
    if power <= 0.0 {
        return None;
    }
    let (mut xd, mut zd) = (xd, zd);
    while xd * xd + zd * zd < f64::from(1.0e-5_f32) {
        xd = (random.next_double() - random.next_double()) * 0.01;
        zd = (random.next_double() - random.next_double()) * 0.01;
    }
    let d = Vec3::new(xd, 0.0, zd).normalize() * power;
    Some(Vec3::new(
        velocity.x / 2.0 - d.x,
        if on_ground {
            (velocity.y / 2.0 + power).min(0.4)
        } else {
            velocity.y
        },
        velocity.z / 2.0 - d.z,
    ))
}

/// A damage source caused by an entity (`damageSources().mobAttack(mob)` and friends): the
/// source position is the attacker's.
pub fn entity_source(
    ents: &Entities,
    kind: DamageKind,
    cause: EntityId,
    direct: EntityId,
) -> DamageSource {
    let mut source = DamageSource::of(kind);
    source.cause = Some(cause);
    source.direct = Some(direct);
    if let Some(e) = ents.store.get(direct) {
        source.position = Some(e.pos());
    }
    if let Some(m) = ents.mob(cause) {
        source.cause_is_mob = true;
        source.cause_name = Some(m.kind.translation_key());
    }
    source
}

/// `hurtServer` of any living entity: players go through the survival logic, mobs through
/// [`hurt_mob`]. Returns whether the damage was taken.
pub fn hurt(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    cx: &mut MobContext,
    target: EntityId,
    source: DamageSource,
    amount: f32,
) -> bool {
    if ents.mob(target).is_some() {
        return hurt_mob(level, ents, env, cx, target, source, amount);
    }
    if ents.special(target).is_some() {
        return super::boss::special::hurt(ents, env, cx, target, &source);
    }
    if ents.player(target).is_none() {
        return false;
    }
    let Entities { store, players, .. } = ents;
    let Some(mut entity) = store.get_mut(target) else {
        return false;
    };
    let row = entity.as_ref().kind_row();
    let Some(data) = players.get_mut(row) else {
        return false;
    };
    player::hurt(&mut entity, data, env, source, amount)
}

/// `isInvulnerableTo` of a mob.
fn invulnerable_to(ents: &Entities, env: &Env<'_>, target: EntityId, kind: DamageKind) -> bool {
    let types = env.types;
    let Some(e) = ents.store.get(target) else {
        return true;
    };
    let Some(m) = ents.mob(target) else {
        return true;
    };
    m.removed
        || types.is(kind, tag::IS_FIRE) && e.kind().info().fire_immune
        || types.is(kind, tag::IS_FALL) && m.kind.fall_damage_immune()
}

/// `LivingEntity.hurtServer` of a mob.
pub fn hurt_mob(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    cx: &mut MobContext,
    target: EntityId,
    source: DamageSource,
    amount: f32,
) -> bool {
    let kind = source.kind;
    let types = env.types;
    if invulnerable_to(ents, env, target, kind) {
        return false;
    }
    if ents.mob(target).is_some_and(|m| m.is_dead_or_dying()) {
        return false;
    }
    let Some(amount) = super::boss::filter_hurt(level, ents, env, cx, target, &source, amount)
    else {
        return false;
    };
    let Some(m) = ents.mob_mut(target) else {
        return false;
    };
    if m.is_dead_or_dying() {
        return false;
    }
    if m.kind == Kind::Enderman && types.is(kind, tag::IS_PROJECTILE) {
        // `EnderMan.hurtServer`: projectiles make it teleport instead.
        kinds::enderman_dodge(level, ents, env, cx, target);
        return false;
    }
    m.no_action_time = 0;
    let mut damage = amount.max(0.0);
    if !damage.is_finite() {
        damage = f32::MAX;
    }

    let took_full = if m.damage_cooldown > 10 && !types.is(kind, tag::BYPASSES_COOLDOWN) {
        if damage <= m.last_hurt {
            return false;
        }
        let delta = damage - m.last_hurt;
        actually_hurt(ents, env, target, source, delta);
        if let Some(m) = ents.mob_mut(target) {
            m.last_hurt = damage;
        }
        false
    } else {
        m.last_hurt = damage;
        m.damage_cooldown = 20;
        actually_hurt(ents, env, target, source, damage);
        if let Some(m) = ents.mob_mut(target) {
            m.hurt_time = 10;
        }
        true
    };
    // `resolveMobResponsibleForDamage` / `resolvePlayerResponsibleForDamage`.
    if let Some(cause) = source.cause {
        let is_player = ents.player(cause).is_some();
        let living = is_player || ents.mob(cause).is_some();
        if let Some(m) = ents.mob_mut(target) {
            if living {
                m.last_hurt_by_mob = Some(cause);
                m.last_hurt_by_mob_time = m.tick_count;
            }
            if is_player {
                m.last_hurt_by_player = Some(cause);
                m.last_hurt_by_player_time = 100;
            }
        }
    }
    let Some(e) = ents.store.get(target) else {
        return false;
    };
    let network_id = e.network_id().0;
    let pos = e.pos();
    if took_full {
        let cause_id = source
            .cause
            .and_then(|c| ents.store.get(c))
            .map_or(-1, |c| c.network_id().0);
        let direct_id = source
            .direct
            .and_then(|c| ents.store.get(c))
            .map_or(-1, |c| c.network_id().0);
        env.out.push(Effect::Tracking {
            entity: target,
            with_self: false,
            packet: SharedPacket::of(&DamageEvent {
                entity_id: network_id,
                source_type: i32::from(kind.0),
                source_cause_id: cause_id,
                source_direct_id: direct_id,
                source_position: None,
            }),
        });
        if let Some(mut e) = ents.store.get_mut(target)
            && !types.is(kind, tag::NO_IMPACT)
        {
            e.set_flags(EntityFlags::HURT_MARKED, true);
        }
        if !types.is(kind, tag::NO_KNOCKBACK) {
            let (xd, zd) = source
                .position
                .map_or((0.0, 0.0), |p| (p.x - pos.x, p.z - pos.z));
            apply_knockback(ents, target, 0.4, xd, zd);
        }
    }
    let dead = ents
        .mob(target)
        .is_some_and(super::MobData::is_dead_or_dying);
    if dead {
        if took_full {
            let sound = ents
                .mob(target)
                .map(|m| super::boss::guardian::sound_variant(m, m.kind.death_sound()));
            if let Some(sound) = sound {
                mob_sound(ents, env, target, sound);
            }
        }
        die(level, ents, env, cx, target, source);
    } else if took_full {
        let sound = ents.mob_mut(target).map(|m| {
            m.ambient_sound_time = -m.kind.ambient_sound_interval();
            super::boss::guardian::sound_variant(m, m.kind.hurt_sound())
        });
        if let Some(sound) = sound {
            mob_sound(ents, env, target, sound);
        }
    }
    if let Some(m) = ents.mob_mut(target) {
        m.last_damage = Some((kind, env.game_time));
    }
    super::boss::after_hurt(level, ents, env, cx, target, &source, true);
    true
}

/// `knockback(power, xd, zd)` of a mob.
pub fn apply_knockback(ents: &mut Entities, target: EntityId, power: f64, xd: f64, zd: f64) {
    if super::boss::dragon::ignores_knockback(ents, target) {
        return;
    }
    let Some(resistance) = ents.mob(target).map(|m| m.attrs.knockback_resistance) else {
        return;
    };
    let Entities { store, mobs, .. } = ents;
    let Some(mut e) = store.get_mut(target) else {
        return;
    };
    let row = e.as_ref().kind_row();
    let Some(m) = mobs.get_mut(row) else {
        return;
    };
    let on_ground = e.as_ref().flags().contains(EntityFlags::ON_GROUND);
    if let Some(v) = knockback(
        e.as_ref().velocity(),
        on_ground,
        power,
        xd,
        zd,
        resistance,
        &mut m.random,
    ) {
        e.set_velocity(v);
        e.set_flags(EntityFlags::HAS_IMPULSE, true);
    }
}

/// `LivingEntity.actuallyHurt` of a mob: armour, then health.
fn actually_hurt(
    ents: &mut Entities,
    env: &Env<'_>,
    target: EntityId,
    source: DamageSource,
    damage: f32,
) {
    let types = env.types;
    let Some(m) = ents.mob_mut(target) else {
        return;
    };
    let mut damage = damage;
    if !types.is(source.kind, tag::BYPASSES_ARMOR) {
        #[allow(clippy::cast_possible_truncation)]
        let (armor, toughness) = (m.attrs.armor as f32, m.attrs.armor_toughness as f32);
        damage = crate::survival::combat::damage_after_absorb(damage, armor, toughness);
    }
    if damage != 0.0 {
        m.health = (m.health - damage).max(0.0);
        if let Some(mut e) = ents.store.get_mut(target) {
            e.mark_data_dirty();
        }
    }
}

/// `LivingEntity.die` of a mob.
fn die(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    cx: &mut MobContext,
    target: EntityId,
    source: DamageSource,
) {
    let Some(m) = ents.mob_mut(target) else {
        return;
    };
    if m.removed || m.dead {
        return;
    }
    m.dead = true;
    super::boss::on_death(level, ents, env, cx, target);
    let Some(m) = ents.mob_mut(target) else {
        return;
    };
    let killed_by_player = m.last_hurt_by_player_time > 0;
    let baby = m.baby;
    let kind = m.kind;
    let mut reward = kind.base_experience_reward(&mut m.random);
    if baby {
        reward = 0;
    }
    if let Some(cause) = source.cause
        && let Some(p) = ents.player_mut(cause)
    {
        // `awardKillScore`.
        p.survival.score = p.survival.score.saturating_add(1);
    }
    let Some(e) = ents.store.get(target) else {
        return;
    };
    let pos = e.pos();
    let network_id = e.network_id().0;
    // `gameEvent(ENTITY_DIE)`: a sculk catalyst nearby takes the experience
    // (`CatalystListener`, `skipDropExperience`); vibration listeners hear the death.
    let consumed = crate::block::sculk_catalyst::catalyst_near(level, pos);
    if consumed {
        cx.actions.push(super::WorldAction::Level(
            super::boss::warden::LevelAction::SculkDeath {
                pos,
                experience: reward,
            },
        ));
    }
    cx.actions.push(super::WorldAction::Level(
        super::boss::warden::LevelAction::GameEvent {
            event: crate::level::game_event::events().entity_die,
            pos,
            context: crate::level::game_event::GameEventContext::of(Some(target)),
        },
    ));
    // `dropAllDeathLoot`: the loot table, then the experience.
    if !baby && env.rules.bool(Rule::MOB_DROPS) {
        let loot = ents
            .mob_mut(target)
            .map(|m| kinds::death_loot(m, killed_by_player))
            .unwrap_or_default();
        for stack in loot {
            // `spawnAtLocation`: an item entity with the default random motion.
            let r = &mut *env.random;
            let velocity = Vec3::new(
                r.next_double() * 0.2 - 0.1,
                0.2,
                r.next_double() * 0.2 - 0.1,
            );
            let uuid = super::random_uuid(r);
            let y_rot = r.next_float() * 360.0;
            env.out.push(Effect::DropItem {
                uuid,
                pos,
                velocity,
                y_rot,
                pickup_delay: 10,
                stack,
            });
        }
    }
    if !consumed && (killed_by_player && env.rules.bool(Rule::MOB_DROPS)) && reward > 0 {
        award_experience(level, ents, cx, pos, reward);
    }
    env.out.push(Effect::Tracking {
        entity: target,
        with_self: false,
        packet: SharedPacket::of(&EntityEvent {
            entity_id: network_id,
            event: event::DEATH,
        }),
    });
    if let Some(mut e) = ents.store.get_mut(target) {
        e.set_pose(Pose::Dying);
    }
}

/// `ExperienceOrb.getExperienceValue`: the largest orb size not above `max`.
pub fn experience_value(max: i32) -> i32 {
    const SIZES: [i32; 10] = [2477, 1237, 617, 307, 149, 73, 37, 17, 7, 3];
    SIZES.into_iter().find(|&s| max >= s).unwrap_or(1)
}

/// `ExperienceOrb.award`: splits `amount` into orbs, merging into orbs of the same value
/// nearby.
pub fn award_experience(
    level: &Level,
    ents: &mut Entities,
    cx: &mut MobContext,
    pos: Vec3,
    amount: i32,
) {
    let mut amount = amount;
    while amount > 0 {
        let value = experience_value(amount);
        amount -= value;
        projectile::add_experience_orb(level, ents, cx, pos, value);
    }
}

/// Plays a mob's sound at its position with its volume and voice pitch (`makeSound`).
pub fn mob_sound(ents: &mut Entities, env: &mut Env<'_>, id: EntityId, sound: &str) {
    let Some(pos) = ents.store.get(id).map(|e| e.pos()) else {
        return;
    };
    let Some(m) = ents.mob_mut(id) else {
        return;
    };
    if m.silent {
        return;
    }
    let volume = m.kind.sound_volume();
    let a = m.random.next_float();
    let b = m.random.next_float();
    let pitch = if m.baby {
        (a - b) * 0.2 + 1.5
    } else {
        (a - b) * 0.2 + 1.0
    };
    let source = m.kind.sound_source();
    let seed = m.random.next_long();
    play_sound(env.out, None, pos, sound, source, volume, pitch, seed);
}

/// `Mob.doHurtTarget`: a melee hit with the mob's attack damage.
pub fn mob_attack(t: &mut super::Tick<'_, '_>, target: EntityId) -> bool {
    #[allow(clippy::cast_possible_truncation)]
    let damage = t.mob_ref().attrs.attack_damage as f32;
    let source = entity_source(t.ents, t.env.types.mob_attack, t.id, t.id);
    let hurt_ok = hurt(t.level, t.ents, t.env, t.cx, target, source, damage);
    if hurt_ok {
        #[allow(clippy::cast_possible_truncation)]
        let kb = t.mob_ref().attrs.attack_knockback as f32;
        if kb > 0.0 {
            let rad = f64::from(t.core().rot().y_rot) * std::f64::consts::PI / 180.0;
            let (sin, cos) = (
                f64::from(vanillite_base::math::sin(rad)),
                f64::from(vanillite_base::math::cos(rad)),
            );
            knock_living(t.ents, t.env, target, f64::from(kb), sin, -cos);
            let v = t.velocity();
            t.set_velocity(v.multiply(0.6, 1.0, 0.6));
        }
        t.mob().last_hurt_mob = Some(target);
        kinds::after_attack(t, target);
    }
    hurt_ok
}

/// Knocks a player or a mob back (`LivingEntity.knockback`).
fn knock_living(
    ents: &mut Entities,
    env: &mut Env<'_>,
    target: EntityId,
    power: f64,
    xd: f64,
    zd: f64,
) {
    if ents.mob(target).is_some() {
        apply_knockback(ents, target, power, xd, zd);
        return;
    }
    if let Some(mut e) = ents.store.get_mut(target) {
        let on_ground = e.as_ref().flags().contains(EntityFlags::ON_GROUND);
        if let Some(v) = knockback(
            e.as_ref().velocity(),
            on_ground,
            power,
            xd,
            zd,
            0.0,
            env.random,
        ) {
            e.set_velocity(v);
            e.set_flags(EntityFlags::HURT_MARKED, true);
        }
    }
}

/// `Player.attack` of a mob, without items (fist damage).
pub fn player_attack(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    cx: &mut MobContext,
    attacker: EntityId,
    target: EntityId,
) {
    let Some(e) = ents.store.get(attacker) else {
        return;
    };
    let ppos = e.pos();
    let flags = e.flags();
    let y_rot = e.rot().y_rot;
    let Some(p) = ents.player(attacker) else {
        return;
    };
    if !ents.mob(target).is_some_and(super::MobData::is_alive) {
        return;
    }
    // `getAttackStrengthScale(0.5)` with an attack speed of 4 (5 ticks).
    #[allow(clippy::cast_precision_loss)]
    let scale = ((p.survival.attack_ticker as f32 + 0.5) / 5.0).clamp(0.0, 1.0);
    let fall_distance = p.survival.fall_distance;
    let in_water = p.survival.fluids.in_water();
    let mut base = 1.0_f32;
    base *= 0.2 + scale * scale * 0.8;
    if let Some(p) = ents.player_mut(attacker) {
        p.survival.attack_ticker = 0;
    }
    let full = scale > 0.9;
    let sprinting = flags.contains(EntityFlags::SPRINTING);
    let on_ground = flags.contains(EntityFlags::ON_GROUND);
    let player_sound = |env: &mut Env<'_>, sound: &str| {
        let seed = env.random.next_long();
        play_sound(
            env.out,
            None,
            ppos,
            sound,
            SoundSource::Players,
            1.0,
            1.0,
            seed,
        );
    };
    let knockback_attack = sprinting && full;
    if knockback_attack {
        player_sound(env, "minecraft:entity.player.attack.knockback");
    }
    let crit = full && fall_distance > 0.0 && !on_ground && !in_water && !sprinting;
    if crit {
        base *= 1.5;
    }
    let mut source = entity_source(ents, env.types.player_attack, attacker, attacker);
    source.cause_is_mob = false;
    let hurt_ok = hurt_mob(level, ents, env, cx, target, source, base);
    if !hurt_ok {
        player_sound(env, "minecraft:entity.player.attack.nodamage");
        return;
    }
    // Dragon parts are not living entities: no knockback from the attack itself.
    let part = ents
        .mob(target)
        .is_some_and(|m| m.kind == Kind::EnderDragon);
    let kb = if knockback_attack && !part {
        0.5_f32
    } else {
        0.0
    };
    if kb > 0.0 {
        let rad = f64::from(y_rot) * std::f64::consts::PI / 180.0;
        let (sin, cos) = (
            f64::from(vanillite_base::math::sin(rad)),
            f64::from(vanillite_base::math::cos(rad)),
        );
        apply_knockback(ents, target, f64::from(kb), sin, -cos);
        if let Some(mut e) = ents.store.get_mut(attacker) {
            e.set_flags(EntityFlags::SPRINTING, false);
        }
    }
    if crit {
        player_sound(env, "minecraft:entity.player.attack.crit");
    } else {
        player_sound(
            env,
            if full {
                "minecraft:entity.player.attack.strong"
            } else {
                "minecraft:entity.player.attack.weak"
            },
        );
    }
    if let Some(p) = ents.player_mut(attacker) {
        player::cause_food_exhaustion(p, 0.1);
    }
}

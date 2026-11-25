//! Damage, knockback and death of mobs (`LivingEntity.hurtServer`, `actuallyHurt`,
//! `knockback`, `die`), melee attacks of mobs (`Mob.doHurtTarget`) and of players
//! (`Player.attack`), and experience drops (`ExperienceOrb.award`).

use vanillite_base::Vec3;
use vanillite_base::random::{LegacyRandom, RandomSource};
use vanillite_protocol::SharedPacket;
use vanillite_protocol::packets::play::clientbound::{DamageEvent, EntityEvent};
use vanillite_world::entity::{EntityFlags, EntityId, Pose};

use super::living::event;
use super::{Kind, MobContext, kinds};
use crate::advancements::predicate::EntityView;
use crate::advancements::trigger::{Event, TriggerKind};
use crate::advancements::view;
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
    let source = with_projectile_motion(ents, source);
    if ents.mob(target).is_some() {
        return hurt_mob(level, ents, env, cx, target, source, amount);
    }
    if ents.special(target).is_some() {
        return super::boss::special::hurt(ents, env, cx, target, &source);
    }
    if ents.player(target).is_none() {
        return false;
    }
    let armor = super::attack::armor_effectiveness(ents, env.types, target, &source);
    let Entities { store, players, .. } = ents;
    let Some(mut entity) = store.get_mut(target) else {
        return false;
    };
    let row = entity.as_ref().kind_row();
    let Some(data) = players.get_mut(row) else {
        return false;
    };
    let hurt = player::hurt_with(&mut entity, data, env, source, amount, armor.as_deref());
    view::resolve_combat(ents, env.types, target);
    hurt
}

/// The motion of a projectile that hits (arrows, tridents, fireballs, thrown items, …: every
/// `Projectile` but potions and fireworks, which push away from their position), for the
/// knockback direction.
fn with_projectile_motion(ents: &Entities, mut source: DamageSource) -> DamageSource {
    if source.projectile_motion.is_some() {
        return source;
    }
    if let Some(e) = source.direct.and_then(|d| ents.store.get(d))
        && matches!(
            e.kind().name(),
            "minecraft:arrow"
                | "minecraft:spectral_arrow"
                | "minecraft:trident"
                | "minecraft:fireball"
                | "minecraft:small_fireball"
                | "minecraft:dragon_fireball"
                | "minecraft:wither_skull"
                | "minecraft:wind_charge"
                | "minecraft:breeze_wind_charge"
                | "minecraft:snowball"
                | "minecraft:egg"
                | "minecraft:ender_pearl"
                | "minecraft:experience_bottle"
                | "minecraft:llama_spit"
                | "minecraft:shulker_bullet"
                | "minecraft:fishing_bobber"
        )
    {
        source.projectile_motion = Some(e.velocity());
    }
    source
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
        || types.is(kind, tag::IS_FIRE)
            && (e.kind().info().fire_immune
                || m.effects.has(crate::effect::MobEffect::FIRE_RESISTANCE))
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
    let source = with_projectile_motion(ents, source);
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
    if types.is(kind, tag::IS_FIRE) && m.effects.has(crate::effect::MobEffect::FIRE_RESISTANCE) {
        return false;
    }
    let mob_kind = m.kind;
    let mut amount = amount;
    if mob_kind == Kind::Enderman && types.is(kind, tag::IS_PROJECTILE) {
        // `EnderMan.hurtServer`: projectiles make it teleport instead.
        kinds::enderman_dodge(level, ents, env, cx, target);
        return false;
    }
    if mob_kind.family() != super::Family::Classic {
        let mut h = super::fam::Hurt {
            level,
            ents,
            env,
            cx,
            target,
            source,
        };
        match super::fam::hurt_filter(&mut h, mob_kind, amount) {
            Some(a) => amount = a,
            None => return false,
        }
    }
    let Some(m) = ents.mob_mut(target) else {
        return false;
    };
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
            // Against the projectile's motion, else away from `getSourcePosition`: the explicit
            // position, else the direct entity's (a thrown potion pushes away from where it
            // splashed).
            let (xd, zd) = match source.projectile_motion {
                Some(m) => (-m.x, -m.z),
                None => source
                    .position
                    .or_else(|| {
                        source
                            .direct
                            .and_then(|d| ents.store.get(d))
                            .map(|e| e.pos())
                    })
                    .map_or((0.0, 0.0), |p| (p.x - pos.x, p.z - pos.z)),
            };
            apply_knockback(ents, target, f64::from(0.4_f32), xd, zd);
        }
    }
    let dead = ents
        .mob(target)
        .is_some_and(super::MobData::is_dead_or_dying);
    if dead {
        if took_full {
            let sound = ents.mob(target).map(kinds::death_sound);
            if let Some(sound) = sound {
                mob_sound(ents, env, target, sound);
            }
        }
        die(level, ents, env, cx, target, source);
    } else if took_full {
        let sound = ents.mob_mut(target).map(|m| {
            m.ambient_sound_time = -m.kind.ambient_sound_interval();
            kinds::hurt_sound(m)
        });
        if let Some(sound) = sound {
            mob_sound(ents, env, target, sound);
        }
    }
    if let Some(m) = ents.mob_mut(target) {
        m.last_damage = Some((kind, env.game_time));
    }
    // `onMobHurt` of the effects (Infested).
    let view = ents.store.get(target).map(|e| (e.rot(), e.dims().height));
    if let Some((rot, height)) = view
        && let Some(m) = ents.mob_mut(target).filter(|m| !m.effects.is_empty())
    {
        let look = crate::effect::living::view_vector(rot.x_rot, rot.y_rot);
        let fx = std::mem::take(&mut m.effects);
        crate::effect::living::on_hurt(&fx, &mut m.random, env.random, pos, height, look, env.out);
        m.effects = fx;
    }
    // What the kinds' `hurtServer` overrides do after `super.hurtServer` (even when it died).
    if mob_kind.family() == super::Family::Classic {
        if mob_kind == Kind::Enderman {
            kinds::after_hurt(level, ents, env, target, source);
        }
        if mob_kind == Kind::Zombie {
            // `Zombie.hurtServer`: reinforcements on Hard.
            let mut h = super::fam::Hurt {
                level,
                ents,
                env,
                cx,
                target,
                source,
            };
            super::fam::undead::zombies::reinforcements(&mut h);
        }
    } else {
        let mut h = super::fam::Hurt {
            level,
            ents,
            env,
            cx,
            target,
            source,
        };
        super::fam::after_hurt(&mut h, mob_kind, damage);
    }
    // `PLAYER_HURT_ENTITY` of the player that caused it.
    if let Some(player) = source.cause.filter(|&c| ents.player(c).is_some()) {
        let types = env.types;
        view::fire(ents, player, TriggerKind::PLAYER_HURT_ENTITY, |ents| {
            let victim = view::snapshot(ents, target)
                .unwrap_or_else(|| Box::new(EntityView::new(mob_kind.name(), pos)));
            Event::PlayerHurtEntity(
                victim,
                view::damage_event(ents, types, &source, damage, damage, false),
            )
        });
    }
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
    let effectiveness = super::attack::armor_effectiveness(ents, types, target, &source);
    let Some(m) = ents.mob_mut(target) else {
        return;
    };
    let mut damage = damage;
    if !types.is(source.kind, tag::BYPASSES_ARMOR) {
        #[allow(clippy::cast_possible_truncation)]
        let (armor, toughness) = (m.attrs.armor as f32, m.attrs.armor_toughness as f32);
        damage = crate::survival::combat::damage_after_absorb_with(
            damage,
            armor,
            toughness,
            effectiveness.as_deref(),
        );
    }
    // `getDamageAfterMagicAbsorb`: Resistance.
    let mut resisted = 0.0;
    if !types.is(source.kind, tag::BYPASSES_EFFECTS) {
        if !types.is(source.kind, tag::BYPASSES_RESISTANCE) {
            let before = damage;
            damage = m.effects.resistance_damage(damage);
            resisted = before - damage;
        }
        if damage <= 0.0 {
            damage = 0.0;
        }
    }
    damage = super::fam::absorb(m, &source, env, damage);
    // Absorption takes the damage first.
    let remaining = (damage - m.absorption).max(0.0);
    m.absorption -= damage - remaining;
    let absorbed = damage - remaining;
    let damage = remaining;
    if damage != 0.0 {
        m.health = (m.health - damage).max(0.0);
        if let Some(mut e) = ents.store.get_mut(target) {
            e.mark_data_dirty();
        }
    }
    // `DAMAGE_DEALT_RESISTED` and `DAMAGE_DEALT_ABSORBED` of the player that caused it.
    if let Some(p) = source.cause.and_then(|c| ents.player_mut(c)) {
        if let Some(v) = crate::survival::player::damage_stat(resisted) {
            p.stats
                .award(crate::stats::custom::DAMAGE_DEALT_RESISTED, v);
        }
        if let Some(v) = crate::survival::player::damage_stat(absorbed) {
            p.stats
                .award(crate::stats::custom::DAMAGE_DEALT_ABSORBED, v);
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
    let mob_kind = m.kind;
    if mob_kind.family() != super::Family::Classic {
        let mut h = super::fam::Hurt {
            level,
            ents,
            env,
            cx,
            target,
            source,
        };
        super::fam::on_die(&mut h, mob_kind);
    }
    let Some(m) = ents.mob_mut(target) else {
        return;
    };
    let killed_by_player = m.last_hurt_by_player_time > 0;
    let baby = m.baby;
    let mut reward = kinds::experience_reward(m);
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
    if consumed && reward > 0 {
        // `CatalystListener.tryAwardItSpreadsAdvancement`: the player that hurt it last, with
        // the damage source before the killing blow (else a player attack).
        let last = ents.mob(target).and_then(|m| {
            m.last_hurt_by_mob
                .filter(|&p| ents.player(p).is_some())
                .map(|p| (p, m.last_damage_source(env.game_time)))
        });
        if let Some((player, last_kind)) = last {
            let types = env.types;
            let kind = TriggerKind::KILL_MOB_NEAR_SCULK_CATALYST;
            view::fire(ents, player, kind, |ents| {
                let blow = match last_kind {
                    Some(k) => DamageSource::of(k),
                    None => entity_source(ents, types.player_attack, player, player),
                };
                let victim = view::snapshot(ents, target)
                    .unwrap_or_else(|| Box::new(EntityView::new(mob_kind.name(), pos)));
                Event::Killed(
                    kind,
                    victim,
                    Box::new(view::damage_view(ents, types, &blow)),
                )
            });
        }
    }
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
        let mut loot = table_loot(level, ents, env, target, &source)
            .or_else(|| {
                ents.mob_mut(target)
                    .map(|m| kinds::death_loot(m, killed_by_player))
            })
            .unwrap_or_default();
        // `dropCustomDeathLoot` / `dropEquipment`: the equipment, the saddle and the
        // enderman's block.
        if let Some(m) = ents.mob_mut(target) {
            super::equipment::death_drops(m, killed_by_player, &mut loot);
            if m.saddled
                && let Some(saddle) = vanillite_data::registry::ITEM.id("minecraft:saddle")
            {
                m.saddled = false;
                loot.push(crate::item::ItemStack::new(saddle, 1));
            }
            if let Some(block) = m.carried.take()
                && let Some(item) = vanillite_data::registry::ITEM.id(block.block().info().name)
            {
                loot.push(crate::item::ItemStack::new(item, 1));
            }
        }
        for stack in loot {
            // `spawnAtLocation`: an item entity with the default random motion.
            let (uuid, y_rot, velocity) = crate::mob::new_item_entity();
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
        award_experience(env, pos, reward);
    }
    // `getKillCredit().awardKillScore(this, source)`: the player that hurt it last, else the
    // last attacking mob.
    let credit = ents.mob(target).and_then(|m| {
        m.last_hurt_by_player
            .filter(|_| m.last_hurt_by_player_time > 0)
            .or(m.last_hurt_by_mob)
    });
    // `ServerPlayer.awardKillScore`: `PLAYER_KILLED_ENTITY` of the player with the credit.
    if let Some(player) = credit
        .or(source.cause)
        .filter(|&k| k != target && ents.player(k).is_some())
    {
        let types = env.types;
        view::fire(ents, player, TriggerKind::PLAYER_KILLED_ENTITY, |ents| {
            let victim = view::snapshot(ents, target)
                .unwrap_or_else(|| Box::new(EntityView::new(mob_kind.name(), pos)));
            Event::Killed(
                TriggerKind::PLAYER_KILLED_ENTITY,
                victim,
                Box::new(view::damage_view(ents, types, &source)),
            )
        });
    }
    env.out.push(Effect::Died {
        victim: target,
        killer: credit.or(source.cause),
        message: None,
    });
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

/// `ExperienceOrb.award`: the world splits `amount` into orbs from the [`Effect::Experience`].
pub fn award_experience(env: &mut Env<'_>, pos: Vec3, amount: i32) {
    env.out.push(Effect::Experience { pos, amount });
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
    let volume = super::fam::mob_sound_volume(m);
    let a = m.random.next_float();
    let b = m.random.next_float();
    let pitch = if m.baby {
        (a - b) * 0.2 + 1.5
    } else {
        (a - b) * 0.2 + 1.0
    };
    let source = m.kind.sound_source();
    let seed = crate::survival::sound_seed();
    play_sound(env.out, None, pos, sound, source, volume, pitch, seed);
}

/// `Mob.doHurtTarget`: a melee hit with the mob's attack damage.
pub fn mob_attack(t: &mut super::Tick<'_, '_>, target: EntityId) -> bool {
    if t.mob_ref().kind.family() != super::Family::Classic
        && let Some(r) = super::fam::do_hurt_target(t, target)
    {
        return r;
    }
    base_mob_attack(t, target)
}

/// `Mob.doHurtTarget` itself (families call it from their override).
pub fn base_mob_attack(t: &mut super::Tick<'_, '_>, target: EntityId) -> bool {
    #[allow(clippy::cast_possible_truncation)]
    let damage = t.mob_ref().attrs.attack_damage as f32;
    let source = entity_source(t.ents, t.env.types.mob_attack, t.id, t.id);
    let hurt_ok = hurt(t.level, t.ents, t.env, t.cx, target, source, damage);
    if hurt_ok {
        #[allow(clippy::cast_possible_truncation)]
        let kb = t.mob_ref().attrs.attack_knockback as f32;
        if kb > 0.0 {
            let rad = f64::from(t.core().rot().y_rot * super::DEG_TO_RAD);
            let (sin, cos) = (
                f64::from(vanillite_base::math::sin(rad)),
                f64::from(vanillite_base::math::cos(rad)),
            );
            knock_living(t.ents, t.env, target, f64::from(kb), sin, -cos);
            let v = t.velocity();
            t.set_velocity(v.multiply(0.6, 1.0, 0.6));
        }
        // `EnchantmentHelper.doPostAttackEffects`: the target's equipment (Thorns); mobs
        // hold no enchanted weapons.
        super::attack::post_attack(t.level, t.ents, t.env, t.cx, target, &source, None);
        t.mob().last_hurt_mob = Some(target);
        kinds::after_attack(t, target);
    }
    hurt_ok
}

/// Knocks a player or a mob back (`LivingEntity.knockback`).
pub fn knock_living(
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

/// The drops of the mob's `entities/<type>` loot table from the data packs, `None` without
/// that table.
fn table_loot(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    target: EntityId,
    source: &DamageSource,
) -> Option<Vec<crate::item::ItemStack>> {
    let data = crate::player::play::game_data()?;
    let name = ents.store.get(target)?.kind().info().name;
    let (ns, path) = name.split_once(':').unwrap_or(("minecraft", name));
    let table = format!("{ns}:entities/{path}");
    if !data.loot.contains(&table) {
        return None;
    }
    let this = super::attack::facts(ents, target)?;
    let attacker = source.cause.and_then(|c| super::attack::facts(ents, c));
    let direct = source.direct.and_then(|c| super::attack::facts(ents, c));
    let m = ents.mob(target)?;
    let last_player = m
        .last_hurt_by_player
        .filter(|_| m.last_hurt_by_player_time > 0)
        .and_then(|p| super::attack::facts(ents, p));
    let weapon = source
        .cause
        .and_then(|c| ents.player(c))
        .map(|p| p.inventory.selected_item().clone())
        .unwrap_or_default();
    let ench_level = |name: &str| data.enchantment_level(&weapon, name);
    let entity = crate::loot::EntityLootContext {
        this,
        attacker,
        direct_attacker: direct,
        last_damage_player: last_player,
        source: Some(crate::predicate::SourceFacts {
            kind: source.kind,
            is_direct: source.cause.is_some() && source.cause == source.direct,
            direct,
            cause: attacker,
        }),
        types: env.types,
        tags: &*data,
        attacker_enchantment_level: &ench_level,
    };
    let none = |_: &str| 0;
    let in_tag = |item: u16, tag: &str| data.item_in_tag(item, tag);
    let empty = crate::item::ItemStack::default();
    let ctx = crate::loot::BlockLootContext {
        state: vanillite_data::block::BlockState::AIR,
        tool: &empty,
        enchantment_level: &none,
        item_in_tag: &in_tag,
        explosion_radius: None,
        this_present: true,
        entity: Some(&entity),
        block_entity: None,
    };
    let mut out = Vec::new();
    // `dropFromLootTable` without a loot table seed: the table's random sequence, else the
    // level random.
    data.loot
        .roll_table(level, &table, &ctx, env.random, &mut out);
    Some(out)
}

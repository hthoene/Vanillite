//! Snow golems (`SnowGolem`) and iron golems (`IronGolem`).
//!
//! Village behaviour of iron golems (`MoveBackToVillageGoal`, `DefendVillageTargetGoal`,
//! `OfferFlowerGoal`, strolling to villagers and POIs) needs villages (#60): without them the
//! golem strolls like `GolemRandomStrollInVillageGoal` does far from a village.

use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Vec3};
use vanillite_data::block::ids;
use vanillite_protocol::packets::play::clientbound::SoundSource;
use vanillite_world::entity::{EntityFlags, EntityId};

use super::{Goal, st, st_mut};
use crate::mob::animal::{Interaction, hurt_held, sound_at, use_item};
use crate::mob::common::{self, CommonGoal};
use crate::mob::fam::{FamGoal, Hurt, Interact};
use crate::mob::goals::{Goal as MobGoal, MeleeKind};
use crate::mob::kinds::{self, Kind};
use crate::mob::missile::{MissileData, MissileKind};
use crate::mob::{MobData, Tick, WorldAction, combat, living, nav, randompos};
use crate::survival::rules::Rule;

/// `IronGolem` targets: an `Enemy` that is no creeper.
fn enemy_not_creeper(t: &Tick<'_, '_>, id: EntityId) -> bool {
    common::enemy(t, id) && !common::mob_of(t, id, Kind::Creeper)
}

/// `registerGoals` of both golems.
pub(super) fn register_goals(m: &mut MobData) {
    let g = &mut m.goals;
    let tg = &mut m.targets;
    if m.kind == Kind::SnowGolem {
        g.add(1, MobGoal::Common(CommonGoal::ranged(1.25, 20, 20, 10.0)));
        g.add(2, MobGoal::stroll_with(1.0, 1.000_000_1e-5));
        g.add(3, MobGoal::look_at_player(6.0));
        g.add(4, MobGoal::random_look());
        tg.add(
            1,
            MobGoal::Common(CommonGoal::nearest(10, true, common::enemy)),
        );
        return;
    }
    g.add(1, MobGoal::melee(MeleeKind::Plain, 1.0, true));
    g.add(
        2,
        MobGoal::Fam(FamGoal::Wildlife(Goal::MoveTowardsTarget {
            speed: 0.9,
            within: 32.0,
            target: None,
            wanted: Vec3::ZERO,
        })),
    );
    // `GolemRandomStrollInVillageGoal(0.6)` away from villages: `LandRandomPos(10, 7)` every
    // 240 ticks on average.
    g.add(
        4,
        MobGoal::Stroll {
            speed: 0.6,
            interval: 240,
            probability: 0.0,
            wanted: Vec3::ZERO,
        },
    );
    g.add(7, MobGoal::look_at_player(6.0));
    g.add(8, MobGoal::random_look());
    tg.add(2, MobGoal::hurt_by(false));
    tg.add(
        3,
        MobGoal::Common(CommonGoal::nearest_player(true, common::angry_at)),
    );
    tg.add(
        3,
        MobGoal::Common(CommonGoal::nearest(5, false, enemy_not_creeper)),
    );
    tg.add(
        4,
        MobGoal::Common(CommonGoal::ResetUniversalAnger {
            alert_same_type: false,
            last_hurt: 0,
        }),
    );
}

/// `SnowGolem.aiStep`: a snow trail where it walks (with mob griefing).
///
/// `SNOW_GOLEM_MELTS` (hot biomes) is a biome attribute the tick cannot read yet; snow golems
/// do not melt there.
pub(super) fn snow_ai_step(t: &mut Tick<'_, '_>) {
    if !t.env.rules.bool(Rule::MOB_GRIEFING) {
        return;
    }
    let snow = ids::SNOW.default_state();
    let p = t.pos();
    for i in 0..4 {
        let x = vanillite_base::math::floor(p.x + f64::from((i % 2 * 2 - 1) as f32 * 0.25));
        let y = vanillite_base::math::floor(p.y);
        let z = vanillite_base::math::floor(p.z + f64::from((i / 2 % 2 * 2 - 1) as f32 * 0.25));
        let pos = BlockPos::new(x, y, z);
        if t.level.block(pos).is_air() && t.level.can_survive(snow, pos) {
            t.cx.actions.push(WorldAction::SetBlock(pos, snow, 3));
        }
    }
}

/// `SnowGolem.performRangedAttack`: a snowball at the target's chest.
pub(super) fn snow_ranged_attack(t: &mut Tick<'_, '_>, target: EntityId) {
    let Some(o) = t.other(target) else {
        return;
    };
    let p = t.pos();
    let start = Vec3::new(p.x, t.eye_y() - f64::from(0.1_f32), p.z);
    let xd = o.pos.x - p.x;
    let yd = o.eye_y - f64::from(1.1_f32);
    let zd = o.pos.z - p.z;
    let yo = (xd * xd + zd * zd).sqrt() * f64::from(0.2_f32);
    let (velocity, rot) = super::snowball::shoot(
        &mut *t.env.random,
        Vec3::new(xd, yd + yo - start.y, zd),
        1.6,
        12.0,
    );
    let mut data = MissileData::new(MissileKind::Snowball);
    data.item = crate::mob::equipment::stack("minecraft:snowball");
    crate::mob::missile::spawn_from(t, start, velocity, rot, data);
    let r = t.mob().random.next_float();
    kinds::play_sound(
        t,
        "minecraft:entity.snow_golem.shoot",
        1.0,
        0.4 / (r * 0.4 + 0.8),
    );
}

/// `SnowGolem.mobInteract`: shears take the pumpkin off.
pub(super) fn snow_interact(ix: &mut Interact<'_, '_>) -> Interaction {
    if crate::item::item_id(ix.stack).name() != "minecraft:shears" {
        return Interaction::Pass;
    }
    if !ix.ents.mob(ix.target).is_some_and(|m| st(m).pumpkin) {
        return Interaction::Pass;
    }
    sound_at(
        ix.ents,
        ix.env,
        ix.target,
        "minecraft:entity.snow_golem.shear",
        SoundSource::Players,
        1.0,
    );
    let eye = ix.ents.mob(ix.target).map_or(1.7, crate::mob::eye_height);
    if let Some(m) = ix.ents.mob_mut(ix.target) {
        st_mut(m).pumpkin = false;
    }
    if let Some(mut e) = ix.ents.store.get_mut(ix.target) {
        e.mark_data_dirty();
    }
    // `shearing/snow_golem`: the carved pumpkin, dropped at the eyes.
    if let Some(pos) = ix.ents.store.get(ix.target).map(|e| e.pos()) {
        drop_at(
            ix,
            pos + Vec3::new(0.0, eye, 0.0),
            "minecraft:carved_pumpkin",
            1,
        );
    }
    hurt_held(ix.ents, ix.env, ix.player, ix.hand, 1);
    Interaction::Success
}

/// `spawnAtLocation`: an item entity with the default motion.
pub(super) fn drop_at(ix: &mut Interact<'_, '_>, pos: Vec3, item: &str, count: i32) {
    let stack = crate::mob::equipment::stack(item).with_count(count);
    if stack.is_empty() {
        return;
    }
    let r = &mut *ix.env.random;
    let velocity = Vec3::new(
        r.next_double() * 0.2 - 0.1,
        0.2,
        r.next_double() * 0.2 - 0.1,
    );
    let uuid = crate::mob::random_uuid(r);
    let y_rot = r.next_float() * 360.0;
    ix.env.out.push(crate::survival::Effect::DropItem {
        uuid,
        pos,
        velocity,
        y_rot,
        pickup_delay: 10,
        stack,
    });
}

/// `IronGolem.aiStep`: the attack and flower animations and the anger.
pub(super) fn iron_ai_step(t: &mut Tick<'_, '_>) {
    {
        let s = st_mut(t.mob());
        if s.attack_animation > 0 {
            s.attack_animation -= 1;
        }
        if s.offer_flower > 0 {
            s.offer_flower -= 1;
        }
    }
    let range = crate::mob::fam::anger_range(Kind::IronGolem);
    common::update_persistent_anger(t, true, range);
}

/// `IronGolem.doHurtTarget`: half to one and a half times the attack damage, flinging the
/// target up.
pub(super) fn iron_hurt_target(t: &mut Tick<'_, '_>, target: EntityId) -> bool {
    st_mut(t.mob()).attack_animation = 10;
    let id = t.id;
    living::entity_event(t, id, 4);
    #[allow(clippy::cast_possible_truncation)]
    let attack = t.mob_ref().attrs.attack_damage as f32;
    #[allow(clippy::cast_possible_truncation)]
    let whole = attack as i32;
    #[allow(clippy::cast_precision_loss)]
    let damage = if whole > 0 {
        attack / 2.0 + t.mob().random.next_int_bounded(whole) as f32
    } else {
        attack
    };
    let source = combat::entity_source(t.ents, t.env.types.mob_attack, id, id);
    let hurt = combat::hurt(t.level, t.ents, t.env, t.cx, target, source, damage);
    if hurt {
        let resistance = t
            .ents
            .mob(target)
            .map_or(0.0, |m| m.attrs.knockback_resistance);
        if let Some(mut e) = t.ents.store.get_mut(target) {
            let v = fling(e.as_ref().velocity(), resistance);
            e.set_velocity(v);
            e.set_flags(EntityFlags::HURT_MARKED, true);
        }
        t.mob().last_hurt_mob = Some(target);
    }
    if t.exists() {
        kinds::play_sound(t, "minecraft:entity.iron_golem.attack", 1.0, 1.0);
    }
    hurt
}

/// The iron golem's fling: up by 0.4 scaled by what the knockback resistance leaves.
pub(crate) fn fling(v: Vec3, knockback_resistance: f64) -> Vec3 {
    let scale = (1.0 - knockback_resistance).max(0.0);
    Vec3::new(v.x, v.y + f64::from(0.4_f32) * scale, v.z)
}

/// `Crackiness.GOLEM.byFraction`: 0 none, 1 low, 2 medium, 3 high.
pub(crate) fn crackiness(fraction: f32) -> u8 {
    if fraction < 0.25 {
        3
    } else if fraction < 0.5 {
        2
    } else if fraction < 0.75 {
        1
    } else {
        0
    }
}

/// `IronGolem.hurtServer`: a crack sound when the golem looks more damaged.
pub(super) fn iron_after_hurt(h: &mut Hurt<'_, '_>, amount: f32) {
    let Some(m) = h.ents.mob(h.target) else {
        return;
    };
    let max = m.max_health();
    let before = (m.health + amount).min(max);
    if crackiness(before / max) != crackiness(m.health / max) {
        combat::mob_sound(
            h.ents,
            h.env,
            h.target,
            "minecraft:entity.iron_golem.damage",
        );
    }
}

/// `IronGolem.mobInteract`: an iron ingot heals 25.
pub(super) fn iron_interact(ix: &mut Interact<'_, '_>) -> Interaction {
    if crate::item::item_id(ix.stack).name() != "minecraft:iron_ingot" {
        return Interaction::Pass;
    }
    let Some(m) = ix.ents.mob_mut(ix.target) else {
        return Interaction::Pass;
    };
    let before = m.health;
    m.health = (m.health + 25.0).min(m.max_health());
    #[allow(clippy::float_cmp)]
    if m.health == before {
        return Interaction::Pass;
    }
    let pitch = 1.0 + (m.random.next_float() - m.random.next_float()) * 0.2;
    if let Some(mut e) = ix.ents.store.get_mut(ix.target) {
        e.mark_data_dirty();
    }
    sound_at(
        ix.ents,
        ix.env,
        ix.target,
        "minecraft:entity.iron_golem.repair",
        SoundSource::Neutral,
        pitch,
    );
    use_item(ix.ents, ix.player, ix.hand);
    Interaction::Success
}

pub(super) fn can_use(t: &mut Tick<'_, '_>, g: &mut Goal) -> bool {
    let Goal::MoveTowardsTarget {
        within,
        target,
        wanted,
        ..
    } = g
    else {
        return false;
    };
    *target = t.mob_ref().target;
    let Some(o) = target.and_then(|id| t.other(id)) else {
        return false;
    };
    let w = f64::from(*within);
    if o.pos.distance_to_sqr(t.pos()) > w * w {
        return false;
    }
    match randompos::default_pos_towards(t, 16, 7, o.pos, randompos::half_pi()) {
        Some(p) => {
            *wanted = p;
            true
        }
        None => false,
    }
}

pub(super) fn can_continue(t: &mut Tick<'_, '_>, g: &mut Goal) -> bool {
    let Goal::MoveTowardsTarget { within, target, .. } = g else {
        return false;
    };
    let w = f64::from(*within);
    !t.mob_ref().nav.is_done()
        && target
            .and_then(|id| t.other(id))
            .is_some_and(|o| o.pos.distance_to_sqr(t.pos()) < w * w)
}

pub(super) fn start(t: &mut Tick<'_, '_>, g: &mut Goal) {
    if let Goal::MoveTowardsTarget { speed, wanted, .. } = g {
        nav::move_to_pos(t, *wanted, *speed);
    }
}

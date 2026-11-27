//! Creakings (`Creaking`): they freeze while a player looks at them, activate near players,
//! and when bound to a creaking heart they cannot be hurt and die with it. Their brain
//! (`CreakingAi`) is in [`super::ai`].

use vanillite_base::{BlockPos, Vec3};
use vanillite_nbt::{Compound, Tag};
use vanillite_world::entity::EntityId;

use super::State;
use crate::mob::fam::Hurt;
use crate::mob::{MobData, Tick};
use crate::player::encode::DataWriter;
use crate::survival::damage::tag;

/// `ATTACK_ANIMATION_DURATION`.
const ATTACK_ANIMATION_TICKS: i32 = 15;
/// `INVULNERABILITY_ANIMATION_DURATION`.
const INVULNERABILITY_TICKS: i32 = 8;
/// `ACTIVATION_RANGE_SQ`.
const ACTIVATION_RANGE_SQR: f64 = 144.0;
/// `TWITCH_DEATH_DURATION`: a creaking killed with its heart twitches this long.
const TWITCH_DEATH_TICKS: i32 = 45;

/// How the heart of a creaking was destroyed (`removeProtector(damageSource)`): the creaking
/// dies by this source on its next tick (the level cannot run the mob's death itself).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HeartDeath {
    /// `damageSources().playerAttack(player)`: a player broke the heart.
    Player(EntityId),
    /// `ServerExplosion.getDamageSource()`: an explosion broke it.
    Explosion {
        source: Option<EntityId>,
        indirect: Option<EntityId>,
        center: Vec3,
    },
}

/// The creaking part of a mob.
#[derive(Clone, Debug, PartialEq)]
pub struct Creaking {
    /// `CAN_MOVE`.
    pub can_move: bool,
    /// `IS_ACTIVE`.
    pub active: bool,
    /// `IS_TEARING_DOWN`.
    pub tearing_down: bool,
    /// `HOME_POS`: the creaking heart it belongs to.
    pub home: Option<BlockPos>,
    pub attack_animation: i32,
    pub invulnerability_animation: i32,
    pub player_stuck_counter: i32,
    /// The player it is activated for (`ATTACK_TARGET`).
    pub target: Option<EntityId>,
    /// `creakingDeathEffects` the heart asked for.
    pub pending_death: Option<HeartDeath>,
}

impl Default for Creaking {
    fn default() -> Self {
        Creaking {
            can_move: true,
            active: false,
            tearing_down: false,
            home: None,
            attack_animation: 0,
            invulnerability_animation: 0,
            player_stuck_counter: 0,
            target: None,
            pending_death: None,
        }
    }
}

pub fn creaking(m: &MobData) -> Creaking {
    match super::state(m) {
        State::Creaking(c) => c.clone(),
        _ => Creaking::default(),
    }
}

pub fn creaking_mut(m: &mut MobData) -> &mut Creaking {
    let s = super::state_mut(m);
    if !matches!(s, State::Creaking(_)) {
        *s = State::Creaking(Creaking::default());
    }
    match s {
        State::Creaking(c) => c,
        _ => unreachable!("set above"),
    }
}

pub fn init(m: &mut MobData) {
    creaking_mut(m);
    m.nav.can_float = true;
    super::ai::creaking_brain(m);
}

/// `canMove`.
pub fn can_move(m: &MobData) -> bool {
    creaking(m).can_move
}

/// `setTransient(pos)`: bound to a creaking heart (its path costs change, see
/// [`HEART_BOUND_MALUS`]).
pub fn set_transient(m: &mut MobData, pos: BlockPos) {
    creaking_mut(m).home = Some(pos);
}

/// The heart a creaking keeps near (`getHomePos`), for its path finding.
pub fn creaking_home(m: &MobData) -> Option<BlockPos> {
    if m.kind == crate::mob::Kind::Creaking {
        creaking(m).home
    } else {
        None
    }
}

/// `isHeartBound`.
pub fn heart_bound(m: &MobData) -> bool {
    m.kind == crate::mob::Kind::Creaking && creaking(m).home.is_some()
}

/// The path costs `setTransient` sets.
pub const HEART_BOUND_MALUS: &crate::mob::path::MalusTable = &[
    (crate::mob::path::PathType::Damaging, 8.0),
    (crate::mob::path::PathType::PowderSnow, 8.0),
    (crate::mob::path::PathType::Lava, 8.0),
    (crate::mob::path::PathType::Fire, 0.0),
    (crate::mob::path::PathType::FireInNeighbor, 0.0),
];

pub fn ambient_sound(m: &MobData) -> Option<&'static str> {
    (!creaking(m).active).then_some("minecraft:entity.creaking.ambient")
}

pub fn hurt_sound(m: &MobData) -> &'static str {
    if creaking(m).home.is_some() {
        "minecraft:entity.creaking.sway"
    } else {
        "minecraft:entity.hostile.hurt"
    }
}

/// Whether the creaking heart at `pos` protects this creaking (`isProtector`).
fn heart_protects(level: &crate::level::Level, pos: BlockPos, uuid: u128) -> bool {
    crate::block::creaking_heart::is_protector(level, pos, uuid)
}

/// `Creaking.tick` before `super.tick()`: the death its heart asked for
/// (`creakingDeathEffects`), and a heart-bound creaking dies without its heart.
pub fn before_tick(t: &mut Tick<'_, '_>) {
    if let Some(death) = creaking_mut(t.mob()).pending_death.take() {
        death_effects(t, death);
    }
    let uuid = t.core().uuid();
    if let Some(home) = creaking(t.mob_ref()).home
        && !heart_protects(t.level, home, uuid)
        && t.mob_ref().health > 0.0
    {
        t.mob().health = 0.0;
        t.core_mut().mark_data_dirty();
    }
}

/// `creakingDeathEffects(source)`: blamed and dead by the heart's breaker, then it twitches.
fn death_effects(t: &mut Tick<'_, '_>, death: HeartDeath) {
    use crate::mob::combat::entity_source;
    use crate::survival::damage::DamageSource;
    let types = t.env.types;
    let source = match death {
        HeartDeath::Player(p) => entity_source(t.ents, types.player_attack, p, p),
        // `damageSources().explosion(source, indirect)`.
        HeartDeath::Explosion {
            source,
            indirect,
            center,
        } => match (source, indirect) {
            (Some(direct), Some(cause)) => {
                let mut s = entity_source(t.ents, types.player_explosion, cause, direct);
                s.position.get_or_insert(center);
                s
            }
            (direct, _) => {
                let mut s = DamageSource::of(types.explosion);
                s.direct = direct;
                s.position = Some(
                    direct
                        .and_then(|d| t.ents.store.get(d))
                        .map_or(center, |e| e.pos()),
                );
                s
            }
        },
    };
    let id = t.id;
    crate::mob::combat::blame_and_die(t.level, t.ents, t.env, t.cx, id, source);
    super::make_sound(t, "minecraft:entity.creaking.twitch");
}

/// `Creaking.tickDeath`: a heart-bound creaking torn down with its heart twitches 45 ticks,
/// then crumbles. Returns `false` for the common death.
pub fn tick_death(t: &mut Tick<'_, '_>) -> bool {
    let c = creaking(t.mob_ref());
    if c.home.is_none() || !c.tearing_down {
        return false;
    }
    t.mob().death_time += 1;
    if t.mob_ref().death_time > TWITCH_DEATH_TICKS && !t.mob_ref().removed {
        tear_down(t);
    }
    true
}

/// `tearDown`: the death sound and the creaking is discarded (the crumble particles are not
/// sent).
fn tear_down(t: &mut Tick<'_, '_>) {
    let sound = crate::mob::kinds::death_sound(t.mob_ref());
    let id = t.id;
    crate::mob::combat::mob_sound(t.ents, t.env, id, sound);
    t.mob().removed = true;
}

/// `Creaking.aiStep` before `super.aiStep()`: the animations count down and it checks
/// whether it may move.
pub fn ai_step(t: &mut Tick<'_, '_>) {
    let c = creaking_mut(t.mob());
    if c.invulnerability_animation > 0 {
        c.invulnerability_animation -= 1;
    }
    if c.attack_animation > 0 {
        c.attack_animation -= 1;
    }
    let could = creaking(t.mob_ref()).can_move;
    let can = check_can_move(t);
    if can != could {
        if can {
            super::make_sound(t, "minecraft:entity.creaking.unfreeze");
        } else {
            // `stopInPlace`.
            t.mob().nav.stop();
            let m = t.mob();
            m.xxa = 0.0;
            m.zza = 0.0;
            m.speed = 0.0;
            let v = t.velocity();
            t.set_velocity(Vec3::new(0.0, v.y.min(0.0), 0.0));
            super::make_sound(t, "minecraft:entity.creaking.freeze");
        }
        creaking_mut(t.mob()).can_move = can;
        t.core_mut().mark_data_dirty();
    }
}

/// `checkCanMove`: a player that can be attacked and looks at it freezes it; an inactive one
/// activates for a looking player within 12 blocks.
fn check_can_move(t: &mut Tick<'_, '_>) -> bool {
    let active = creaking(t.mob_ref()).active;
    let players = super::ai::nearest_players(t);
    if players.is_empty() {
        if active {
            deactivate(t);
        }
        return true;
    }
    let mut potential = false;
    let pos = t.pos();
    let eye_y = t.eye_y();
    for &p in &players {
        if !crate::mob::goals::can_attack(t, p) {
            continue;
        }
        potential = true;
        let looking = is_looked_at_by(t, p, &[eye_y, pos.y + 0.5, (eye_y + pos.y) / 2.0]);
        if (!active || !wears_disguise(t, p)) && looking {
            if active {
                return false;
            }
            if t.other(p)
                .is_some_and(|o| o.pos.distance_to_sqr(pos) < ACTIVATION_RANGE_SQR)
            {
                activate(t, p);
                return false;
            }
        }
    }
    if !potential && active {
        deactivate(t);
    }
    true
}

/// `PLAYER_NOT_WEARING_DISGUISE_ITEM` negated: a carved pumpkin on the head.
fn wears_disguise(t: &Tick<'_, '_>, player: EntityId) -> bool {
    t.ents.player(player).is_some_and(|p| {
        let head = p.inventory.equipment(crate::item::EquipmentSlot::Head);
        crate::item::item_id(head).name() == "minecraft:carved_pumpkin"
    })
}

/// `LivingEntity.isLookingAtMe(player, 0.5, false, true, heights)`: the player's view is
/// within the tolerance of one of the points and it can see it.
fn is_looked_at_by(t: &Tick<'_, '_>, player: EntityId, heights: &[f64]) -> bool {
    let (Some(e), Some(o)) = (t.ents.store.get(player), t.other(player)) else {
        return false;
    };
    let rot = e.rot();
    let view = crate::mob::view_vector(rot.x_rot, rot.y_rot).normalize();
    let p = t.pos();
    let eye = Vec3::new(o.pos.x, o.eye_y, o.pos.z);
    for &h in heights {
        let to = Vec3::new(p.x - eye.x, h - eye.y, p.z - eye.z);
        let dir = to.normalize();
        let dot = view.x * dir.x + view.y * dir.y + view.z * dir.z;
        // `adjustForDistance` is false: the same cone at every distance.
        if dot > 1.0 - 0.5 {
            let from = eye;
            let target = Vec3::new(p.x, h, p.z);
            if crate::mob::physics::clip(t.level, from, target).is_none() {
                return true;
            }
        }
    }
    false
}

/// `activate(player)`.
fn activate(t: &mut Tick<'_, '_>, player: EntityId) {
    let c = creaking_mut(t.mob());
    c.target = Some(player);
    c.active = true;
    t.mob().target = Some(player);
    super::ai::set_attack_target(t.mob(), Some(player));
    super::make_sound(t, "minecraft:entity.creaking.activate");
    t.core_mut().mark_data_dirty();
}

/// `deactivate`.
fn deactivate(t: &mut Tick<'_, '_>) {
    let c = creaking_mut(t.mob());
    c.target = None;
    c.active = false;
    t.mob().target = None;
    super::ai::set_attack_target(t.mob(), None);
    super::make_sound(t, "minecraft:entity.creaking.deactivate");
    t.core_mut().mark_data_dirty();
}

/// `customServerAiStep`: the brain.
pub fn custom_server_ai_step(t: &mut Tick<'_, '_>) {
    super::ai::tick(t);
}

/// `doHurtTarget`: the attack animation (event 4), then the plain attack.
pub fn do_hurt_target(t: &mut Tick<'_, '_>, target: EntityId) -> bool {
    creaking_mut(t.mob()).attack_animation = ATTACK_ANIMATION_TICKS;
    super::entity_event(t, 4);
    crate::mob::combat::base_mob_attack(t, target)
}

/// `hurtServer`: a heart-bound creaking only flinches (event 66) at living or projectile
/// hits and players' damage; a player's hit makes its heart react (`creakingHurt`). Damage
/// that bypasses invulnerability passes.
pub fn hurt_filter(h: &mut Hurt<'_, '_>, amount: f32) -> Option<f32> {
    let m = h.ents.mob(h.target)?;
    let c = creaking(m);
    let Some(home) = c.home else {
        return Some(amount);
    };
    if h.env.types.is(h.source.kind, tag::BYPASSES_INVULNERABILITY) {
        return Some(amount);
    }
    if c.invulnerability_animation > 0 || m.is_dead_or_dying() {
        return None;
    }
    // `blameSourceForDamage`.
    let player = crate::mob::combat::blame(h.ents, h.target, &h.source);
    let direct_living_or_projectile = h.source.direct.is_some_and(|d| {
        h.ents.mob(d).is_some()
            || h.ents.player(d).is_some()
            || h.ents.arrow(d).is_some()
            || h.ents.missile(d).is_some()
    });
    if !direct_living_or_projectile && player.is_none() {
        return None;
    }
    if let Some(m) = h.ents.mob_mut(h.target) {
        creaking_mut(m).invulnerability_animation = INVULNERABILITY_TICKS;
    }
    crate::mob::animal::entity_event(h.ents, h.env, h.target, 66);
    if let Some(pos) = h.ents.store.get(h.target).map(|e| e.pos()) {
        h.cx.actions.push(crate::mob::WorldAction::Level(
            crate::mob::boss::warden::LevelAction::GameEvent {
                event: crate::level::game_event::events().entity_action,
                pos,
                context: crate::level::game_event::GameEventContext::of(Some(h.target)),
            },
        ));
    }
    let uuid = h.ents.store.get(h.target).map_or(0, |e| e.uuid());
    if heart_protects(h.level, home, uuid) {
        if player.is_some() {
            h.cx.actions
                .push(crate::mob::WorldAction::CreakingHurt(home));
        }
        let s = h.ents.mob(h.target).map(hurt_sound);
        if let Some(s) = s {
            crate::mob::combat::mob_sound(h.ents, h.env, h.target, s);
        }
    }
    None
}

/// The synched data: can move 16, active 17, tearing down 18, home 19.
pub fn encode(w: &mut DataWriter<'_>, m: &MobData) {
    let c = creaking(m);
    w.boolean(16, c.can_move, true);
    w.boolean(17, c.active, false);
    w.boolean(18, c.tearing_down, false);
    w.optional_block_pos(19, c.home);
}

pub fn save(m: &MobData, out: &mut Compound) {
    if let Some(h) = creaking(m).home {
        out.insert("home_pos", vec![h.x, h.y, h.z]);
    }
    super::ai::save_brain(m, out);
}

pub fn load(m: &mut MobData, tag: &Compound) {
    if let Some(Tag::IntArray(v)) = tag.get("home_pos")
        && v.len() == 3
    {
        set_transient(m, BlockPos::new(v[0], v[1], v[2]));
    }
    super::ai::load_brain(m, tag);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mob::Kind;

    #[test]
    fn home_round_trip() {
        let mut m = MobData::new(Kind::Creaking, 1);
        assert!(can_move(&m));
        set_transient(&mut m, BlockPos::new(1, 2, 3));
        let mut out = Compound::new();
        save(&m, &mut out);
        let mut n = MobData::new(Kind::Creaking, 2);
        load(&mut n, &out);
        assert_eq!(creaking(&n).home, Some(BlockPos::new(1, 2, 3)));
    }
}

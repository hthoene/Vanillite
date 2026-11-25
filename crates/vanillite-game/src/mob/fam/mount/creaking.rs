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

/// `setTransient(pos)`: bound to a creaking heart.
pub fn set_transient(m: &mut MobData, pos: BlockPos) {
    creaking_mut(m).home = Some(pos);
}

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

/// Whether the creaking heart at `pos` is there and awake (`isProtector`; the heart's block
/// entity, which knows its creaking, is not there yet).
fn heart_protects(t: &Tick<'_, '_>, pos: BlockPos) -> bool {
    let state = t.level.block(pos);
    state.block().name() == "minecraft:creaking_heart"
}

/// `Creaking.tick` before `super.tick()`: a heart-bound creaking dies without its heart.
pub fn before_tick(t: &mut Tick<'_, '_>) {
    if let Some(home) = creaking(t.mob_ref()).home
        && !heart_protects(t, home)
        && t.mob_ref().health > 0.0
    {
        t.mob().health = 0.0;
        t.core_mut().mark_data_dirty();
    }
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
    let view = view_vector(rot.x_rot, rot.y_rot).normalize();
    let p = t.pos();
    let eye = Vec3::new(o.pos.x, o.eye_y, o.pos.z);
    for &h in heights {
        let to = Vec3::new(p.x - eye.x, h - eye.y, p.z - eye.z);
        let dist = to.length();
        let dir = to.normalize();
        let dot = view.x * dir.x + view.y * dir.y + view.z * dir.z;
        if dot > 1.0 - 0.5 / dist {
            let from = eye;
            let target = Vec3::new(p.x, h, p.z);
            if crate::mob::physics::clip(t.level, from, target).is_none() {
                return true;
            }
        }
    }
    false
}

/// `Entity.calculateViewVector(xRot, yRot)`.
fn view_vector(x_rot: f32, y_rot: f32) -> Vec3 {
    let x = f64::from(x_rot).to_radians();
    let y = f64::from(-y_rot).to_radians();
    let (ycos, ysin) = (y.cos(), y.sin());
    let (xcos, xsin) = (x.cos(), x.sin());
    Vec3::new(ysin * xcos, -xsin, ycos * xcos)
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
/// hits and players' damage, and tells its heart; other damage passes.
pub fn hurt_filter(h: &mut Hurt<'_, '_>, amount: f32) -> Option<f32> {
    let m = h.ents.mob(h.target)?;
    let c = creaking(m);
    let Some(_home) = c.home else {
        return Some(amount);
    };
    if h.env.types.is(h.source.kind, tag::BYPASSES_INVULNERABILITY) {
        return Some(amount);
    }
    if c.invulnerability_animation > 0 || m.is_dead_or_dying() {
        return None;
    }
    let direct_living_or_projectile = h.source.direct.is_some_and(|d| {
        h.ents.mob(d).is_some()
            || h.ents.player(d).is_some()
            || h.ents.arrow(d).is_some()
            || h.ents.missile(d).is_some()
    });
    let player = h.source.cause.filter(|c| h.ents.player(*c).is_some());
    if !direct_living_or_projectile && player.is_none() {
        return None;
    }
    if let Some(m) = h.ents.mob_mut(h.target) {
        creaking_mut(m).invulnerability_animation = INVULNERABILITY_TICKS;
    }
    crate::mob::animal::entity_event(h.ents, h.env, h.target, 66);
    let s = h.ents.mob(h.target).map(hurt_sound);
    if let Some(s) = s {
        crate::mob::combat::mob_sound(h.ents, h.env, h.target, s);
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

    #[test]
    fn looking_vector() {
        let v = view_vector(0.0, 0.0);
        assert!((v.z - 1.0).abs() < 1e-9);
        let v = view_vector(90.0, 0.0);
        assert!((v.y + 1.0).abs() < 1e-9);
    }
}

//! Breezes (`Breeze`): trial chamber monsters that slide, jump far and shoot wind charges;
//! their brain (`BreezeAi` with `Shoot`, `LongJump`, `Slide`, `ShootWhenStuck`) is in
//! [`super::ai`], their wind charges in [`super::wind`].

use vanillite_base::Vec3;
use vanillite_base::random::RandomSource;
use vanillite_world::entity::Pose;

use super::State;
use crate::mob::fam::Hurt;
use crate::mob::{Kind, MobData, Tick};

/// The breeze part of a mob.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Breeze {
    /// `soundTick` of the whirl sound.
    pub sound_tick: i32,
    /// `jumpTrailStartedTick`.
    pub jump_trail: i32,
    /// `setDiscardFriction` during a long jump.
    pub discard_friction: bool,
}

fn breeze_mut(m: &mut MobData) -> &mut Breeze {
    let s = super::state_mut(m);
    if !matches!(s, State::Breeze(_)) {
        *s = State::Breeze(Breeze::default());
    }
    match s {
        State::Breeze(b) => b,
        _ => unreachable!("set above"),
    }
}

pub fn init(m: &mut MobData) {
    breeze_mut(m);
    super::ai::breeze_brain(m);
}

/// `Breeze.tick` before `super.tick()`: the particle and whirl sound timers (the particles
/// themselves are client side, their random draws are not).
pub fn before_tick(t: &mut Tick<'_, '_>) {
    let pose = t.core().pose();
    let passenger = t.ents.vehicle_of(t.id).is_some();
    let m = t.mob();
    match pose {
        Pose::Shooting | Pose::Inhaling | Pose::Standing => {
            breeze_mut(m).jump_trail = 0;
            // `emitGroundParticles(1 + random.nextInt(1))`.
            let _ = m.random.next_int_bounded(1);
            let _ = passenger;
        }
        Pose::LongJumping => {
            let b = breeze_mut(m);
            b.jump_trail += 1;
        }
        _ => {}
    }
    let tick = breeze_mut(m).sound_tick;
    let next = if tick == 0 {
        m.random.next_int_between_inclusive(1, 80)
    } else {
        tick - 1
    };
    breeze_mut(m).sound_tick = next;
    if next == 0 {
        // `playWhirlSound` (a local sound of the client).
        let _ = (m.random.next_float(), m.random.next_float());
    }
}

/// `isInvulnerableTo`: breezes do not hurt each other.
pub fn hurt_filter(h: &mut Hurt<'_, '_>, amount: f32) -> Option<f32> {
    let by_breeze = h
        .source
        .cause
        .and_then(|c| h.ents.mob(c))
        .is_some_and(|m| m.kind == Kind::Breeze);
    (!by_breeze).then_some(amount)
}

/// `customServerAiStep`: the brain.
pub fn custom_server_ai_step(t: &mut Tick<'_, '_>) {
    super::ai::tick(t);
}

/// `getFiringYPosition`.
pub fn firing_y(t: &Tick<'_, '_>) -> f64 {
    let d = t.core().dims();
    t.pos().y + f64::from(d.height) / 2.0 + f64::from(0.3_f32)
}

/// `withinInnerCircleRange(target)`: within 4 horizontally and 10 vertically of the block
/// centre.
pub fn within_inner_circle(t: &Tick<'_, '_>, target: Vec3) -> bool {
    let b = t.block_pos();
    let c = Vec3::new(
        f64::from(b.x) + 0.5,
        f64::from(b.y) + 0.5,
        f64::from(b.z) + 0.5,
    );
    let (dx, dz) = (target.x - c.x, target.z - c.z);
    dx * dx + dz * dz < 16.0 && (target.y - c.y).abs() < 10.0
}

/// `Breeze.canAttack`: players and iron golems only.
pub fn can_attack_kind(t: &Tick<'_, '_>, id: vanillite_world::entity::EntityId) -> bool {
    t.ents.player(id).is_some() || t.ents.mob(id).is_some_and(|m| m.kind == Kind::IronGolem)
}

/// `setDiscardFriction`.
pub fn set_discard_friction(m: &mut MobData, on: bool) {
    breeze_mut(m).discard_friction = on;
}

/// `travelInAir` with `shouldDiscardFriction`: the long jump flies its parabola.
pub fn travel(t: &mut Tick<'_, '_>, input: Vec3) -> bool {
    let discard = matches!(super::state(t.mob_ref()), State::Breeze(b) if b.discard_friction);
    let fluids = t.mob_ref().fluids;
    if !discard || fluids.in_water() || fluids.in_lava() {
        return false;
    }
    super::ghast::move_relative(t, 0.02, input);
    let v = t.velocity();
    crate::mob::living::move_self(t, v);
    if !t.exists() {
        return true;
    }
    let v = t.velocity();
    let gravity = t.mob_ref().attrs.gravity;
    t.set_velocity(Vec3::new(v.x, v.y - gravity, v.z));
    true
}

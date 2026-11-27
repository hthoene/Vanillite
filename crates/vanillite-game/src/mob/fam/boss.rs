//! The boss family: guardians, elder guardians, the wither, the ender dragon and the warden.
//! Their machinery lives in [`crate::mob::boss`]; these hooks connect it to the family
//! dispatch. Some boss hooks are called directly by the mob tick (`filter_hurt`,
//! `tick_death`, `ai_step_override`, `move_control`) and are not repeated here.

use vanillite_base::random::LegacyRandom;
use vanillite_base::{BlockPos, Vec3};
use vanillite_nbt::Compound;

use super::{FamilyHooks, Hurt, SpawnReason};
use crate::level::Level;
use crate::mob::boss;
use crate::mob::kinds::{DefaultSound, Kind};
use crate::mob::path::{MalusTable, PathType};
use crate::mob::{Environment, MobData, Tick};
use crate::player::encode::DataWriter;
use crate::survival::rules::Difficulty;

/// The boss family has no goals of its own in [`super::FamGoal`]: guardians and the wither
/// keep theirs in [`crate::mob::goals::Goal`].
#[derive(Clone, Copy, Debug)]
pub enum Goal {}

#[derive(Debug)]
pub struct Hooks;

fn guardian(kind: Kind) -> bool {
    matches!(kind, Kind::Guardian | Kind::ElderGuardian)
}

impl FamilyHooks for Hooks {
    type Goal = Goal;

    fn finalize_spawn(m: &mut MobData, _ctx: &mut super::SpawnContext<'_>) {
        // `Warden.finalizeSpawn`: every spawn waits 1200 ticks before it may dig back down.
        if m.kind == Kind::Warden
            && let Some(mem) = m.brain.as_deref_mut()
        {
            mem.set_with_expiry(
                crate::mob::brain::MemoryType::DigCooldown,
                crate::mob::brain::MemValue::Unit,
                boss::warden::DIGGING_COOLDOWN,
            );
        }
    }

    fn ambient_sound_interval(kind: Kind) -> i32 {
        if guardian(kind) { 160 } else { 80 }
    }
    fn breathes_underwater(kind: Kind) -> bool {
        kind != Kind::Warden
    }
    fn allowed_in_peaceful(kind: Kind) -> bool {
        kind == Kind::EnderDragon
    }
    fn remove_when_far_away(kind: Kind) -> bool {
        // The wither and the dragon require custom persistence; `Warden.removeWhenFarAway`
        // is never.
        guardian(kind)
    }
    fn malus(kind: Kind) -> &'static MalusTable {
        const GUARDIAN: &MalusTable = &[(PathType::Water, 0.0)];
        const WARDEN: &MalusTable = &[
            (PathType::UnpassableRail, 0.0),
            (PathType::Damaging, 8.0),
            (PathType::PowderSnow, 8.0),
            (PathType::Lava, 8.0),
            (PathType::Fire, 0.0),
            (PathType::FireInNeighbor, 0.0),
        ];
        match kind {
            Kind::Warden => WARDEN,
            Kind::Guardian | Kind::ElderGuardian => GUARDIAN,
            _ => &[],
        }
    }
    fn sound_volume(kind: Kind) -> f32 {
        match kind {
            Kind::EnderDragon => 5.0,
            Kind::Warden => 4.0,
            _ => 1.0,
        }
    }
    fn ambient_sound(m: &mut MobData) -> Option<&'static str> {
        Some(boss::guardian::sound_variant(
            m,
            m.kind.default_sound(DefaultSound::Ambient),
        ))
    }
    fn hurt_sound(m: &MobData) -> &'static str {
        boss::guardian::sound_variant(m, m.kind.default_sound(DefaultSound::Hurt))
    }
    fn death_sound(m: &MobData) -> &'static str {
        if m.kind == Kind::EnderDragon {
            // The dragon has no own death sound (`GENERIC_DEATH`).
            return "minecraft:entity.generic.death";
        }
        boss::guardian::sound_variant(m, m.kind.default_sound(DefaultSound::Death))
    }
    fn experience_reward(m: &mut MobData) -> i32 {
        match m.kind {
            Kind::Guardian | Kind::ElderGuardian => 10,
            Kind::Wither => 50,
            Kind::EnderDragon => 0,
            _ => 5,
        }
    }
    fn walk_target_value(kind: Kind, level: &Level, env: &Environment, pos: BlockPos) -> f64 {
        let cost = f64::from(crate::mob::light_magic_value(level, env, pos) - 0.5);
        if guardian(kind) && crate::mob::is_water(level.block(pos)) {
            // `Guardian.getWalkTargetValue`: water first, lit water preferred.
            #[allow(clippy::cast_possible_truncation)]
            let v = 10.0_f32 + cost as f32;
            return f64::from(v);
        }
        crate::mob::kinds::base_walk_target_value(true, level, env, pos)
    }
    fn check_spawn_rules(
        kind: Kind,
        level: &Level,
        env: &Environment,
        pos: BlockPos,
        random: &mut LegacyRandom,
        reason: SpawnReason,
    ) -> bool {
        if guardian(kind) {
            // Peaceful is excluded before monsters spawn.
            return boss::guardian::check_spawn_rules(level, pos, Difficulty::Normal, random);
        }
        crate::mob::spawn::default_spawn_rules(kind, level, env, pos, random, reason)
    }
    fn register_goals(m: &mut MobData) {
        boss::register_goals(m);
    }
    fn before_tick(t: &mut Tick<'_, '_>) {
        boss::before_tick(t);
    }
    fn after_tick(t: &mut Tick<'_, '_>) {
        boss::after_tick(t);
    }
    fn ai_step_start(t: &mut Tick<'_, '_>) {
        boss::ai_step_pre(t);
    }
    fn ai_step(t: &mut Tick<'_, '_>) {
        boss::ai_step(t);
    }
    fn custom_server_ai_step(t: &mut Tick<'_, '_>) {
        boss::custom_server_ai_step(t);
    }
    fn travel(t: &mut Tick<'_, '_>, input: Vec3) -> bool {
        boss::travel(t, input)
    }
    fn after_hurt(h: &mut Hurt<'_, '_>, _amount: f32) {
        let source = h.source;
        boss::after_hurt(h.level, h.ents, h.env, h.cx, h.target, &source, true);
    }
    fn on_die(h: &mut Hurt<'_, '_>) {
        boss::on_death(h.level, h.ents, h.env, h.cx, h.target);
    }
    fn encode_data(w: &mut DataWriter<'_>, m: &MobData) {
        let (out, all) = w.parts();
        boss::entity_data(out, m, all);
    }
    fn save(m: &MobData, out: &mut Compound) {
        boss::save(m, out);
    }
    fn load(m: &mut MobData, tag: &Compound) {
        boss::load(m, tag);
    }
}

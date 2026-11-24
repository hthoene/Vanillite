//! Bosses and the special mobs with their own machinery: guardians and elder guardians, the
//! wither, the ender dragon and the warden. Each kind keeps its state in [`Ext`] and hooks
//! into the mob tick at the points below; what is shared by all mobs stays in
//! [`super::living`] and [`super::combat`].
//!
//! The non-living entities that belong to them (wither skulls, dragon fireballs, end crystals,
//! area effect clouds) are in [`special`].

pub mod dragon;
pub mod guardian;
pub mod special;
pub mod warden;
pub mod wither;

use vanillite_nbt::Compound;
use vanillite_world::entity::EntityId;

use super::{Kind, MobContext, MobData, Tick};
use crate::entities::Entities;
use crate::level::Level;
use crate::survival::damage::DamageSource;
use crate::survival::player::Env;

/// The kind specific state of a boss (`None` for other mobs).
#[derive(Clone, Debug, Default)]
pub enum Ext {
    #[default]
    None,
    Guardian(Box<guardian::State>),
    Wither(Box<wither::State>),
    Dragon(Box<dragon::State>),
    Warden(Box<warden::State>),
}

/// The fresh state of `kind`.
pub fn new_ext(kind: Kind) -> Ext {
    match kind {
        Kind::Guardian | Kind::ElderGuardian => Ext::Guardian(Box::default()),
        Kind::Wither => Ext::Wither(Box::default()),
        Kind::EnderDragon => Ext::Dragon(Box::default()),
        Kind::Warden => Ext::Warden(Box::default()),
        _ => Ext::None,
    }
}

impl Ext {
    pub fn guardian(&self) -> Option<&guardian::State> {
        match self {
            Ext::Guardian(s) => Some(s),
            _ => None,
        }
    }

    pub fn guardian_mut(&mut self) -> Option<&mut guardian::State> {
        match self {
            Ext::Guardian(s) => Some(s),
            _ => None,
        }
    }

    pub fn wither(&self) -> Option<&wither::State> {
        match self {
            Ext::Wither(s) => Some(s),
            _ => None,
        }
    }

    pub fn wither_mut(&mut self) -> Option<&mut wither::State> {
        match self {
            Ext::Wither(s) => Some(s),
            _ => None,
        }
    }

    pub fn dragon(&self) -> Option<&dragon::State> {
        match self {
            Ext::Dragon(s) => Some(s),
            _ => None,
        }
    }

    pub fn dragon_mut(&mut self) -> Option<&mut dragon::State> {
        match self {
            Ext::Dragon(s) => Some(s),
            _ => None,
        }
    }

    pub fn warden(&self) -> Option<&warden::State> {
        match self {
            Ext::Warden(s) => Some(s),
            _ => None,
        }
    }

    pub fn warden_mut(&mut self) -> Option<&mut warden::State> {
        match self {
            Ext::Warden(s) => Some(s),
            _ => None,
        }
    }
}

/// Whether `kind` is one of this module's kinds.
pub fn is_boss_kind(kind: Kind) -> bool {
    matches!(
        kind,
        Kind::Guardian | Kind::ElderGuardian | Kind::Wither | Kind::EnderDragon | Kind::Warden
    )
}

/// `registerGoals`.
pub fn register_goals(m: &mut MobData) {
    match m.kind {
        Kind::Guardian | Kind::ElderGuardian => guardian::register_goals(m),
        Kind::Wither => wither::register_goals(m),
        Kind::Warden => warden::register_goals(m),
        _ => {}
    }
}

/// Work before `LivingEntity.tick` (the kind's `tick` override before `super.tick()`).
pub fn before_tick(t: &mut Tick<'_, '_>) {
    match t.mob_ref().kind {
        Kind::Guardian | Kind::ElderGuardian => guardian::before_tick(t),
        Kind::Wither => wither::before_tick(t),
        Kind::EnderDragon => dragon::before_tick(t),
        Kind::Warden => warden::before_tick(t),
        _ => {}
    }
}

/// Work after `LivingEntity.tick`.
pub fn after_tick(t: &mut Tick<'_, '_>) {
    match t.mob_ref().kind {
        Kind::Guardian | Kind::ElderGuardian => guardian::after_tick(t),
        Kind::Wither => wither::after_tick(t),
        Kind::EnderDragon => dragon::after_tick(t),
        Kind::Warden => warden::after_tick(t),
        _ => {}
    }
}

/// A kind whose `aiStep` replaces `LivingEntity.aiStep` entirely (the ender dragon): runs it
/// and returns `true`.
pub fn ai_step_override(t: &mut Tick<'_, '_>) -> bool {
    match t.mob_ref().kind {
        Kind::EnderDragon => {
            dragon::ai_step(t);
            true
        }
        Kind::Wither => {
            // `WitherBoss.aiStep` before `super.aiStep()`.
            wither::ai_step_before(t);
            false
        }
        _ => false,
    }
}

/// The kind's additions to `aiStep` (after `LivingEntity.aiStep`).
pub fn ai_step(t: &mut Tick<'_, '_>) {
    match t.mob_ref().kind {
        Kind::Guardian | Kind::ElderGuardian => guardian::ai_step(t),
        Kind::Wither => wither::ai_step(t),
        Kind::Warden => warden::ai_step(t),
        _ => {}
    }
}

/// `customServerAiStep`.
pub fn custom_server_ai_step(t: &mut Tick<'_, '_>) {
    match t.mob_ref().kind {
        Kind::Guardian | Kind::ElderGuardian => guardian::custom_server_ai_step(t),
        Kind::Wither => wither::custom_server_ai_step(t),
        Kind::Warden => warden::custom_server_ai_step(t),
        _ => {}
    }
}

/// A kind's own `MoveControl.tick` (the wither's `FlyingMoveControl`): returns `true` when it
/// replaced the ground move control.
pub fn move_control(t: &mut Tick<'_, '_>) -> bool {
    match t.mob_ref().kind {
        Kind::Wither => {
            wither::move_control(t);
            true
        }
        _ => false,
    }
}

/// A kind's own `travel(input)`: returns `true` when it moved the mob itself.
pub fn travel(t: &mut Tick<'_, '_>, input: vanillite_base::Vec3) -> bool {
    match t.mob_ref().kind {
        Kind::Guardian | Kind::ElderGuardian => guardian::travel(t, input),
        Kind::Wither => wither::travel(t, input),
        _ => false,
    }
}

/// `tickDeath` of a kind with its own death (the dragon's death animation, the wither):
/// returns `true` when it handled it.
pub fn tick_death(t: &mut Tick<'_, '_>) -> bool {
    match t.mob_ref().kind {
        Kind::EnderDragon => dragon::tick_death(t),
        Kind::Warden => warden::tick_death(t),
        Kind::Wither => wither::tick_death(t),
        _ => false,
    }
}

/// The kind's part of `hurtServer` before the common damage handling: the amount to apply,
/// or `None` when the hit is ignored (wither invulnerability and armour, dragon parts, warden
/// anger, guardian thorns are applied here or in [`after_hurt`]).
pub fn filter_hurt(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    cx: &mut MobContext,
    target: EntityId,
    source: &DamageSource,
    amount: f32,
) -> Option<f32> {
    let kind = ents.mob(target)?.kind;
    match kind {
        Kind::Guardian | Kind::ElderGuardian => {
            guardian::filter_hurt(level, ents, env, cx, target, source, amount)
        }
        Kind::Wither => wither::filter_hurt(level, ents, env, cx, target, source, amount),
        Kind::EnderDragon => dragon::filter_hurt(level, ents, env, cx, target, source, amount),
        Kind::Warden => warden::filter_hurt(level, ents, env, cx, target, source, amount),
        _ => Some(amount),
    }
}

/// The kind's part of `hurtServer` after the damage was taken (`took` = it hurt).
pub fn after_hurt(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    cx: &mut MobContext,
    target: EntityId,
    source: &DamageSource,
    took: bool,
) {
    let Some(kind) = ents.mob(target).map(|m| m.kind) else {
        return;
    };
    match kind {
        Kind::Guardian | Kind::ElderGuardian => {
            guardian::after_hurt(level, ents, env, cx, target, source, took);
        }
        Kind::Wither => wither::after_hurt(level, ents, env, cx, target, source, took),
        Kind::Warden => warden::after_hurt(level, ents, env, cx, target, source, took),
        _ => {}
    }
}

/// `die` additions (the wither's nether star, the dragon fight's bookkeeping).
pub fn on_death(
    level: &Level,
    ents: &mut Entities,
    env: &mut Env<'_>,
    cx: &mut MobContext,
    target: EntityId,
) {
    let Some(kind) = ents.mob(target).map(|m| m.kind) else {
        return;
    };
    match kind {
        Kind::Wither => wither::on_death(level, ents, env, cx, target),
        Kind::EnderDragon => dragon::on_death(level, ents, env, cx, target),
        _ => {}
    }
}

/// The loot of `entities/<kind>`.
pub fn death_loot(m: &mut MobData, killed_by_player: bool) -> Vec<crate::item::ItemStack> {
    match m.kind {
        Kind::Guardian | Kind::ElderGuardian => guardian::death_loot(m, killed_by_player),
        Kind::Wither => wither::death_loot(m, killed_by_player),
        Kind::Warden => warden::death_loot(m, killed_by_player),
        _ => Vec::new(),
    }
}

/// The kind's synched entity data after the mob's own (ids from 16 on).
pub fn entity_data(out: &mut Vec<u8>, m: &MobData, all: bool) {
    match &m.ext {
        Ext::Guardian(s) => guardian::entity_data(out, s, all),
        Ext::Wither(s) => wither::entity_data(out, s, all),
        Ext::Dragon(s) => dragon::entity_data(out, s, all),
        Ext::Warden(s) => warden::entity_data(out, s, all),
        Ext::None => {}
    }
}

/// The kind's entity NBT.
pub fn save(m: &MobData, out: &mut Compound) {
    match &m.ext {
        Ext::Guardian(s) => guardian::save(s, out),
        Ext::Wither(s) => wither::save(s, out),
        Ext::Dragon(s) => dragon::save(s, out),
        Ext::Warden(s) => warden::save(s, out),
        Ext::None => {}
    }
}

/// Reads the kind's entity NBT.
pub fn load(m: &mut MobData, tag: &Compound) {
    match &mut m.ext {
        Ext::Guardian(s) => guardian::load(s, tag),
        Ext::Wither(s) => wither::load(s, tag),
        Ext::Dragon(s) => dragon::load(s, tag),
        Ext::Warden(s) => warden::load(s, tag),
        Ext::None => {}
    }
}

/// Entity data helpers for the kinds' synched values (`EntityDataSerializers` ids of 26.3).
pub mod data {
    use vanillite_protocol::codec::write_varint;

    pub const BYTE: i32 = 0;
    pub const INT: i32 = 1;
    pub const FLOAT: i32 = 3;
    pub const BOOLEAN: i32 = 8;
    pub const OPTIONAL_BLOCK_POS: i32 = 11;
    pub const PARTICLE: i32 = 16;
    pub const PARTICLES: i32 = 17;

    /// Appends one data item.
    pub fn item(out: &mut Vec<u8>, id: u8, serializer: i32, value: impl FnOnce(&mut Vec<u8>)) {
        out.push(id);
        write_varint(out, serializer);
        value(out);
    }

    /// An int item, sent when `all` or not `default`.
    pub fn int(out: &mut Vec<u8>, id: u8, v: i32, default: i32, all: bool) {
        if all || v != default {
            item(out, id, INT, |o| write_varint(o, v));
        }
    }

    /// A boolean item, sent when `all` or not `default`.
    pub fn boolean(out: &mut Vec<u8>, id: u8, v: bool, default: bool, all: bool) {
        if all || v != default {
            item(out, id, BOOLEAN, |o| o.push(u8::from(v)));
        }
    }

    /// A float item, sent when `all` or not `default`.
    pub fn float(out: &mut Vec<u8>, id: u8, v: f32, default: f32, all: bool) {
        if all || v != default {
            item(out, id, FLOAT, |o| o.extend_from_slice(&v.to_be_bytes()));
        }
    }
}

//! `Parrot`: taming with seeds, cookies poison it, flying wander to trees, following other
//! mobs, imitating hostile mobs and landing on its owner's shoulder.

use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Vec3};
use vanillite_world::entity::EntityId;

use super::{
    G, Goal as FamGoal, Interact, SpawnContext, consume, dirty, event, fam, in_tag, st_mut,
};
use crate::level::Level;
use crate::mob::animal::Interaction;
use crate::mob::fam::SpawnReason;
use crate::mob::goals::{flag, reduced};
use crate::mob::kinds::Kind;
use crate::mob::nav::{MoveKind, NavKind};
use crate::mob::{MobData, Tick, nav, randompos};

/// The parrot goals.
#[derive(Clone, Copy, Debug)]
pub enum Goal {
    /// `ParrotWanderGoal` (`WaterAvoidingRandomFlyingGoal` preferring trees).
    Wander { speed: f64, wanted: Vec3 },
    /// `FollowMobGoal(mob, 1.0, 3.0, 7.0)`.
    FollowMob {
        following: Option<EntityId>,
        recalc: i32,
    },
    /// `LandOnOwnersShoulderGoal`.
    LandOnShoulder {
        /// `isSittingOnShoulder`.
        sitting: bool,
    },
}

pub fn flags(g: &Goal) -> u8 {
    match g {
        Goal::Wander { .. } => flag::MOVE,
        Goal::FollowMob { .. } => flag::MOVE | flag::LOOK,
        Goal::LandOnShoulder { .. } => 0,
    }
}

/// `registerGoals` and the flying navigation of the parrot.
pub fn register_goals(m: &mut MobData) {
    let g = &mut m.goals;
    g.add(
        0,
        fam(FamGoal::TamePanic {
            speed: 1.25,
            env_only: false,
            pos: Vec3::ZERO,
        }),
    );
    g.add(0, G::Float);
    g.add(1, G::look_at_player(8.0));
    g.add(2, G::SitWhenOrdered);
    g.add(2, G::follow_owner(1.0, 5.0, 1.0));
    g.add(
        2,
        fam(FamGoal::Parrot(Goal::Wander {
            speed: 1.0,
            wanted: Vec3::ZERO,
        })),
    );
    g.add(
        3,
        fam(FamGoal::Parrot(Goal::LandOnShoulder { sitting: false })),
    );
    g.add(
        3,
        fam(FamGoal::Parrot(Goal::FollowMob {
            following: None,
            recalc: 0,
        })),
    );
    m.nav.kind = NavKind::Flying;
    m.nav.can_open_doors = false;
    m.nav.can_float = true;
    m.move_ctl.kind = MoveKind::Flying {
        max_turn: 10,
        hovers: false,
    };
}

/// `finalizeSpawn`: a random variant, never a baby.
pub fn finalize_spawn(m: &mut MobData, _ctx: &mut SpawnContext<'_>) {
    m.variant = m.random.next_int_bounded(5);
}

/// The imitated sound of a mob kind (`MOB_SOUND_MAP`).
pub fn imitated_sound(kind: Kind) -> Option<&'static str> {
    Some(match kind {
        Kind::Blaze => "minecraft:entity.parrot.imitate.blaze",
        Kind::Bogged => "minecraft:entity.parrot.imitate.bogged",
        Kind::Breeze => "minecraft:entity.parrot.imitate.breeze",
        Kind::CamelHusk => "minecraft:entity.parrot.imitate.camel_husk",
        Kind::CaveSpider | Kind::Spider => "minecraft:entity.parrot.imitate.spider",
        Kind::Creaking => "minecraft:entity.parrot.imitate.creaking",
        Kind::Creeper => "minecraft:entity.parrot.imitate.creeper",
        Kind::Drowned => "minecraft:entity.parrot.imitate.drowned",
        Kind::Endermite => "minecraft:entity.parrot.imitate.endermite",
        Kind::Evoker => "minecraft:entity.parrot.imitate.evoker",
        Kind::Ghast => "minecraft:entity.parrot.imitate.ghast",
        Kind::Guardian => "minecraft:entity.parrot.imitate.guardian",
        Kind::Hoglin => "minecraft:entity.parrot.imitate.hoglin",
        Kind::Husk => "minecraft:entity.parrot.imitate.husk",
        Kind::Illusioner => "minecraft:entity.parrot.imitate.illusioner",
        Kind::MagmaCube => "minecraft:entity.parrot.imitate.magma_cube",
        Kind::Parched => "minecraft:entity.parrot.imitate.parched",
        Kind::Phantom => "minecraft:entity.parrot.imitate.phantom",
        Kind::Piglin => "minecraft:entity.parrot.imitate.piglin",
        Kind::PiglinBrute => "minecraft:entity.parrot.imitate.piglin_brute",
        Kind::Pillager => "minecraft:entity.parrot.imitate.pillager",
        Kind::Ravager => "minecraft:entity.parrot.imitate.ravager",
        Kind::Shulker => "minecraft:entity.parrot.imitate.shulker",
        Kind::Silverfish => "minecraft:entity.parrot.imitate.silverfish",
        Kind::Skeleton => "minecraft:entity.parrot.imitate.skeleton",
        Kind::Slime => "minecraft:entity.parrot.imitate.slime",
        Kind::Stray => "minecraft:entity.parrot.imitate.stray",
        Kind::Vex => "minecraft:entity.parrot.imitate.vex",
        Kind::Vindicator => "minecraft:entity.parrot.imitate.vindicator",
        Kind::Witch => "minecraft:entity.parrot.imitate.witch",
        Kind::WitherSkeleton => "minecraft:entity.parrot.imitate.wither_skeleton",
        Kind::Zoglin => "minecraft:entity.parrot.imitate.zoglin",
        Kind::Zombie => "minecraft:entity.parrot.imitate.zombie",
        Kind::ZombieHorse => "minecraft:entity.parrot.imitate.zombie_horse",
        Kind::ZombieNautilus => "minecraft:entity.parrot.imitate.zombie_nautilus",
        Kind::ZombieVillager => "minecraft:entity.parrot.imitate.zombie_villager",
        _ => return None,
    })
}

/// The sounds of `MOB_SOUND_MAP`, one per mob type (`getAmbient` imitates a random type).
pub const IMITATED: [&str; 37] = [
    "minecraft:entity.parrot.imitate.blaze",
    "minecraft:entity.parrot.imitate.bogged",
    "minecraft:entity.parrot.imitate.breeze",
    "minecraft:entity.parrot.imitate.camel_husk",
    "minecraft:entity.parrot.imitate.spider",
    "minecraft:entity.parrot.imitate.spider",
    "minecraft:entity.parrot.imitate.creaking",
    "minecraft:entity.parrot.imitate.creeper",
    "minecraft:entity.parrot.imitate.drowned",
    "minecraft:entity.parrot.imitate.endermite",
    "minecraft:entity.parrot.imitate.evoker",
    "minecraft:entity.parrot.imitate.ghast",
    "minecraft:entity.parrot.imitate.guardian",
    "minecraft:entity.parrot.imitate.hoglin",
    "minecraft:entity.parrot.imitate.husk",
    "minecraft:entity.parrot.imitate.illusioner",
    "minecraft:entity.parrot.imitate.magma_cube",
    "minecraft:entity.parrot.imitate.parched",
    "minecraft:entity.parrot.imitate.phantom",
    "minecraft:entity.parrot.imitate.piglin",
    "minecraft:entity.parrot.imitate.piglin_brute",
    "minecraft:entity.parrot.imitate.pillager",
    "minecraft:entity.parrot.imitate.ravager",
    "minecraft:entity.parrot.imitate.shulker",
    "minecraft:entity.parrot.imitate.silverfish",
    "minecraft:entity.parrot.imitate.skeleton",
    "minecraft:entity.parrot.imitate.slime",
    "minecraft:entity.parrot.imitate.stray",
    "minecraft:entity.parrot.imitate.vex",
    "minecraft:entity.parrot.imitate.vindicator",
    "minecraft:entity.parrot.imitate.witch",
    "minecraft:entity.parrot.imitate.wither_skeleton",
    "minecraft:entity.parrot.imitate.zoglin",
    "minecraft:entity.parrot.imitate.zombie",
    "minecraft:entity.parrot.imitate.zombie_horse",
    "minecraft:entity.parrot.imitate.zombie_nautilus",
    "minecraft:entity.parrot.imitate.zombie_villager",
];

/// `getAmbientSound` (the 1-in-1000 imitation is done in `ai_step`).
pub fn ambient_sound(_m: &MobData) -> &'static str {
    "minecraft:entity.parrot.ambient"
}

/// `Parrot.aiStep`: imitating nearby hostiles, the slow fall while flapping.
/// `Parrot.aiStep` before `super.aiStep()`: now and then it imitates a mob nearby (with the
/// level's random).
pub fn ai_step_start(t: &mut Tick<'_, '_>) {
    if t.env.random.next_int_bounded(400) == 0 {
        imitate_nearby(t);
    }
}

pub fn ai_step(t: &mut Tick<'_, '_>) {
    let v = t.velocity();
    if !t.on_ground() && v.y < 0.0 {
        t.set_velocity(v.multiply(1.0, 0.6, 1.0));
    }
    let delta = if t.on_ground() { -0.3 } else { 1.2 };
    let flap = &mut st_mut(t.mob()).flap;
    *flap = (*flap + delta).clamp(0.0, 1.0);
}

/// `imitateNearbyMobs`: half the time a sound of a random imitable mob within 20 blocks.
fn imitate_nearby(t: &mut Tick<'_, '_>) {
    if !t.mob_ref().is_alive() || t.mob_ref().silent || t.env.random.next_int_bounded(2) != 0 {
        return;
    }
    let area = t.aabb().inflate(20.0, 20.0, 20.0);
    let mut ids = std::mem::take(&mut t.cx.ids);
    ids.clear();
    let ents = &*t.ents;
    ents.store.for_each_in(&area, false, |id| {
        if ents
            .mob(id)
            .is_some_and(|m| imitated_sound(m.kind).is_some())
        {
            ids.push(id);
        }
    });
    if !ids.is_empty() {
        let n = i32::try_from(ids.len()).unwrap_or(i32::MAX);
        let i = usize::try_from(t.env.random.next_int_bounded(n)).unwrap_or(0);
        let chosen = ids[i];
        let sound = t
            .ents
            .mob(chosen)
            .filter(|m| !m.silent)
            .and_then(|m| imitated_sound(m.kind));
        if let Some(s) = sound {
            let pitch = {
                let r = &mut *t.env.random;
                (r.next_float() - r.next_float()) * 0.2 + 1.0
            };
            crate::mob::kinds::play_sound(t, s, 0.7, pitch);
        }
    }
    t.cx.ids = ids;
}

/// Parrots ignore `doHurtTarget` overrides.
pub fn do_hurt_target(_t: &mut Tick<'_, '_>, _target: EntityId) -> Option<bool> {
    None
}

/// `Parrot.mobInteract`: seeds tame one in ten, cookies kill it, the owner sits it down.
pub fn interact(ix: &mut Interact<'_, '_>) -> Interaction {
    let Some(m) = ix.ents.mob(ix.target) else {
        return Interaction::Pass;
    };
    let tame = m.tame;
    if !tame && in_tag(ix.stack, "minecraft:parrot_food") {
        consume(ix);
        let pitch = ix.ents.mob_mut(ix.target).map_or(1.0, |m| {
            1.0 + (m.random.next_float() - m.random.next_float()) * 0.2
        });
        super::sound(ix, "minecraft:entity.parrot.eat", 1.0, pitch);
        let owner = super::player_uuid(ix);
        let tamed = ix
            .ents
            .mob_mut(ix.target)
            .is_some_and(|m| m.random.next_int_bounded(10) == 0);
        if tamed {
            if let (Some(m), Some(o)) = (ix.ents.mob_mut(ix.target), owner) {
                super::tame_by(m, o);
            }
            event(ix, 7);
        } else {
            event(ix, 6);
        }
        dirty(ix);
        return Interaction::Success;
    }
    if in_tag(ix.stack, "minecraft:parrot_poisonous_food") {
        consume(ix);
        if let Some(m) = ix.ents.mob_mut(ix.target) {
            crate::effect::living::add_to_mob(
                m,
                crate::effect::EffectInstance::new(crate::effect::MobEffect::POISON, 900, 0),
            );
        }
        let kind = ix.env.types.player_attack;
        let source = crate::mob::combat::entity_source(ix.ents, kind, ix.player, ix.player);
        crate::mob::combat::hurt(
            ix.level,
            ix.ents,
            ix.env,
            ix.cx,
            ix.target,
            source,
            f32::MAX,
        );
        return Interaction::Success;
    }
    let flying = ix.ents.store.get(ix.target).is_some_and(|e| {
        !e.flags()
            .contains(vanillite_world::entity::EntityFlags::ON_GROUND)
    });
    if !flying && tame && super::owned_by(ix) {
        if let Some(m) = ix.ents.mob_mut(ix.target) {
            m.order_to_sit = !m.order_to_sit;
        }
        return Interaction::Success;
    }
    super::animal_interact(ix)
}

/// `PathfinderMob.getWalkTargetValue` (parrots have no preference).
pub fn walk_target_value(_level: &Level, _pos: BlockPos) -> f64 {
    0.0
}

/// `checkParrotSpawnRules`: `#parrots_spawnable_on` below and bright enough.
pub fn spawn_rules(level: &Level, pos: BlockPos, reason: SpawnReason) -> bool {
    let below = level.block(BlockPos::new(pos.x, pos.y - 1, pos.z));
    let name = below.block().name();
    let ok = below.block() == vanillite_data::block::ids::GRASS_BLOCK
        || below.is_air()
        || name.ends_with("_leaves")
        || name.ends_with("_log")
        || name.ends_with("_wood");
    ok && super::bright_enough(level, pos, reason)
}

// --- Goals --------------------------------------------------------------------------------

/// `ParrotWanderGoal.getTreePos`: an empty spot on leaves or logs within 3 and 6 blocks.
fn tree_pos(t: &Tick<'_, '_>) -> Option<Vec3> {
    let p = t.pos();
    let me = t.block_pos();
    let (x0, y0, z0) = (
        vanillite_base::math::floor(p.x - 3.0),
        vanillite_base::math::floor(p.y - 6.0),
        vanillite_base::math::floor(p.z - 3.0),
    );
    let (x1, y1, z1) = (
        vanillite_base::math::floor(p.x + 3.0),
        vanillite_base::math::floor(p.y + 6.0),
        vanillite_base::math::floor(p.z + 3.0),
    );
    // `BlockPos.betweenClosed` order: x fastest, then y, then z.
    for z in z0..=z1 {
        for y in y0..=y1 {
            for x in x0..=x1 {
                let pos = BlockPos::new(x, y, z);
                if pos == me {
                    continue;
                }
                let below = t.level.block(BlockPos::new(x, y - 1, z));
                let name = below.block().name();
                let sit =
                    name.ends_with("_leaves") || name.ends_with("_log") || name.ends_with("_wood");
                if sit
                    && t.level.block(pos).is_air()
                    && t.level.block(BlockPos::new(x, y + 1, z)).is_air()
                {
                    return Some(randompos::bottom_center(pos));
                }
            }
        }
    }
    None
}

pub fn can_use(t: &mut Tick<'_, '_>, g: &mut Goal) -> bool {
    match g {
        Goal::Wander { wanted, .. } => {
            if t.mob_ref().no_action_time >= 100 {
                return false;
            }
            if t.mob().random.next_int_bounded(reduced(120)) != 0 {
                return false;
            }
            let mut pos = None;
            if t.mob_ref().fluids.in_water() {
                pos = randompos::land_pos(t, 15, 15);
            }
            if t.mob().random.next_float() >= 0.001 {
                pos = tree_pos(t);
            }
            // The hover and air positions of `WaterAvoidingRandomFlyingGoal`.
            let pos = pos.or_else(|| randompos::default_pos(t, 8, 7));
            match pos {
                Some(p) => {
                    *wanted = p;
                    true
                }
                None => false,
            }
        }
        Goal::FollowMob { following, .. } => {
            let area = t.aabb().inflate(7.0, 7.0, 7.0);
            let me = t.id;
            let kind = t.mob_ref().kind;
            let mut found = None;
            let ents = &*t.ents;
            ents.store.for_each_in(&area, false, |id| {
                if found.is_some() || id == me {
                    return;
                }
                let visible = ents.store.get(id).is_some_and(|e| {
                    !e.flags()
                        .contains(vanillite_world::entity::EntityFlags::INVISIBLE)
                });
                if ents.mob(id).is_some_and(|m| m.kind != kind) && visible {
                    found = Some(id);
                }
            });
            *following = found;
            found.is_some()
        }
        Goal::LandOnShoulder { .. } => can_land_on_shoulder(t),
    }
}

/// `LandOnOwnersShoulderGoal.canUse`: the owner is a player here that is no spectator and not
/// in water, the parrot is not ordered to sit and rode no shoulder for 100 ticks
/// (`canSitOnShoulder`; `rideCooldownCounter` counts like `tickCount`).
fn can_land_on_shoulder(t: &Tick<'_, '_>) -> bool {
    let m = t.mob_ref();
    let Some(owner) = crate::mob::tame::owner_of(t.ents, m) else {
        return false;
    };
    let Some(p) = t.ents.player(owner) else {
        return false;
    };
    let can_be_sat_on =
        p.game_mode != vanillite_config::GameMode::Spectator && !p.survival.fluids.in_water();
    !m.order_to_sit && can_be_sat_on && m.tick_count > 100
}

/// `ShoulderRidingEntity.setEntityOnShoulder` with `ServerPlayer.setEntityOnShoulder`: the
/// parrot's NBT goes onto a free shoulder of a player on the ground, out of vehicles and
/// water, and the parrot leaves the world.
fn set_on_shoulder(t: &mut Tick<'_, '_>, owner: EntityId) -> bool {
    let on_ground = t.ents.store.get(owner).is_some_and(|e| {
        e.flags()
            .contains(vanillite_world::entity::EntityFlags::ON_GROUND)
    });
    let riding = t.ents.vehicle_of(owner).is_some();
    let Some(core) = t.ents.store.get(t.id) else {
        return false;
    };
    let tag = crate::mob::nbt::save_mob(&core, t.mob_ref());
    let game_time = t.game_time();
    let Some(p) = t.ents.player_mut(owner) else {
        return false;
    };
    if riding || !on_ground || p.survival.fluids.in_water() {
        return false;
    }
    if !p.survival.shoulders.put(tag, game_time) {
        return false;
    }
    if let Some(mut e) = t.ents.store.get_mut(owner) {
        e.mark_data_dirty();
    }
    t.mob().removed = true;
    true
}

pub fn can_continue(t: &mut Tick<'_, '_>, g: &mut Goal) -> bool {
    match g {
        Goal::Wander { .. } => !t.mob_ref().nav.is_done() && t.ents.vehicle_of(t.id).is_none(),
        Goal::FollowMob { following, .. } => {
            let Some(o) = following.and_then(|id| t.other(id)) else {
                return false;
            };
            !t.mob_ref().nav.is_done() && o.pos.distance_to_sqr(t.pos()) > 9.0
        }
        Goal::LandOnShoulder { .. } => can_land_on_shoulder(t),
    }
}

pub fn start(t: &mut Tick<'_, '_>, g: &mut Goal) {
    match g {
        Goal::Wander { speed, wanted } => {
            nav::move_to_pos(t, *wanted, *speed);
        }
        Goal::FollowMob { recalc, .. } => *recalc = 0,
        Goal::LandOnShoulder { sitting } => *sitting = false,
    }
}

pub fn stop(t: &mut Tick<'_, '_>, g: &mut Goal) {
    match g {
        Goal::Wander { .. } => t.mob().nav.stop(),
        Goal::FollowMob { following, .. } => {
            *following = None;
            t.mob().nav.stop();
        }
        Goal::LandOnShoulder { .. } => {}
    }
}

pub fn tick(t: &mut Tick<'_, '_>, g: &mut Goal) {
    if let Goal::LandOnShoulder { sitting } = g {
        if *sitting || t.mob_ref().in_sitting_pose || t.ents.leashes.contains_key(&t.id) {
            return;
        }
        let owner = crate::mob::tame::owner_of(t.ents, t.mob_ref());
        if let Some(owner) = owner
            && t.ents.player(owner).is_some()
            && t.ents
                .store
                .get(owner)
                .is_some_and(|e| e.aabb().intersects(&t.aabb()))
        {
            *sitting = set_on_shoulder(t, owner);
        }
        return;
    }
    if let Goal::FollowMob { following, recalc } = g {
        if t.ents.leashes.contains_key(&t.id) {
            return;
        }
        let Some(o) = following.and_then(|id| t.other(id)) else {
            return;
        };
        t.mob()
            .look
            .set_look_at(Vec3::new(o.pos.x, o.eye_y, o.pos.z), 10.0, 40.0);
        *recalc -= 1;
        if *recalc > 0 {
            return;
        }
        *recalc = reduced(10);
        let p = t.pos();
        let d = p.distance_to_sqr(o.pos);
        if d > 9.0 {
            nav::move_to_entity(t, o.pos, 1, 1.0);
        } else {
            t.mob().nav.stop();
            if d <= 3.0 {
                let back = Vec3::new(p.x - (o.pos.x - p.x), p.y, p.z - (o.pos.z - p.z));
                nav::move_to_pos(t, back, 1.0);
            }
        }
    }
}

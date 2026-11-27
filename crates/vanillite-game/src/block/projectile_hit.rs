//! `BlockBehaviour.onProjectileHit`: what blocks do when a projectile hits them (target blocks,
//! bells, TNT, campfires, candles, chorus flowers, amethyst, decorated pots, pointed dripstone,
//! big dripleaves). `Projectile.onHitBlock` queues the hit as a world action; it runs after the
//! entities' tick.

use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Direction, Vec3};
use vanillite_data::block::{BlockId, BlockState, ids};
use vanillite_world::entity::{EntityFlags, EntityId};
use vanillite_worldgen::behaviour::{Class, StateExt, class_of_block};

use super::{BlockBehaviour, block_tags, sound_id};
use crate::level::{Level, SOUND_BLOCKS, update};

/// A projectile hit on a block.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProjectileHit {
    /// The projectile.
    pub projectile: EntityId,
    /// The hit location.
    pub at: Vec3,
    /// The face hit (`BlockHitResult.getDirection`).
    pub face: Direction,
    /// The projectile's speed when it hit (`getDeltaMovement().length()`).
    pub speed: f64,
}

/// `onProjectileHit(level, state, hitResult, projectile)`.
pub type ProjectileHitFn = fn(&mut Level, BlockState, BlockPos, &ProjectileHit);

/// Fills the `onProjectileHit` behaviours.
pub(super) fn fill(block: BlockId, b: &mut BlockBehaviour) {
    let f: Option<ProjectileHitFn> = if block == ids::TARGET {
        Some(target)
    } else if block == ids::BELL {
        Some(bell)
    } else if block == ids::TNT {
        Some(tnt)
    } else if block == ids::CAMPFIRE || block == ids::SOUL_CAMPFIRE {
        Some(campfire)
    } else if block == ids::CHORUS_FLOWER {
        Some(chorus_flower)
    } else if block == ids::DECORATED_POT {
        Some(decorated_pot)
    } else if block == ids::BIG_DRIPLEAF {
        Some(super::dripleaf::projectile_hit)
    } else if is_amethyst(block) {
        Some(amethyst)
    } else if class_of_block(block) == Class::Speleothem {
        Some(speleothem)
    } else if is_candle(block) {
        Some(candle)
    } else {
        None
    };
    if f.is_some() {
        b.projectile_hit = f;
    }
}

/// `AmethystBlock` and its subclasses (`BuddingAmethystBlock`, `AmethystClusterBlock`).
fn is_amethyst(block: BlockId) -> bool {
    block == ids::AMETHYST_BLOCK
        || block == ids::BUDDING_AMETHYST
        || block == ids::AMETHYST_CLUSTER
        || block == ids::LARGE_AMETHYST_BUD
        || block == ids::MEDIUM_AMETHYST_BUD
        || block == ids::SMALL_AMETHYST_BUD
}

/// `AbstractCandleBlock`: candles and candle cakes.
fn is_candle(block: BlockId) -> bool {
    let name = block.name().trim_start_matches("minecraft:");
    name == "candle" || name.ends_with("_candle") || name.ends_with("candle_cake")
}

/// `BlockState.onProjectileHit` of the block at `pos` that `projectile` hit at `at` with
/// `speed` (the world action of `Projectile.onHitBlock`).
pub fn projectile_hit_block(
    level: &mut Level,
    pos: BlockPos,
    at: Vec3,
    projectile: EntityId,
    speed: f64,
) {
    let state = level.block(pos);
    let Some(f) = level.behaviours().get(state.block()).projectile_hit else {
        return;
    };
    if !level.entities.store.contains(projectile) {
        return;
    }
    let hit = ProjectileHit {
        projectile,
        at,
        face: hit_face(state, pos, at),
        speed,
    };
    f(level, state, pos, &hit);
}

/// The face of the block's collision shape the hit location lies on (the clip's direction).
pub fn hit_face(state: BlockState, pos: BlockPos, at: Vec3) -> Direction {
    let l = at - Vec3::new(f64::from(pos.x), f64::from(pos.y), f64::from(pos.z));
    let mut best = (f64::MAX, Direction::Up);
    let mut boxes = state.collision_shape().to_vec();
    if boxes.is_empty() {
        boxes.push([0.0, 0.0, 0.0, 1.0, 1.0, 1.0]);
    }
    const E: f64 = 1.0e-6;
    for b in boxes {
        let in_x = l.x >= b[0] - E && l.x <= b[3] + E;
        let in_y = l.y >= b[1] - E && l.y <= b[4] + E;
        let in_z = l.z >= b[2] - E && l.z <= b[5] + E;
        let faces = [
            ((l.x - b[0]).abs(), in_y && in_z, Direction::West),
            ((l.x - b[3]).abs(), in_y && in_z, Direction::East),
            ((l.y - b[1]).abs(), in_x && in_z, Direction::Down),
            ((l.y - b[4]).abs(), in_x && in_z, Direction::Up),
            ((l.z - b[2]).abs(), in_x && in_y, Direction::North),
            ((l.z - b[5]).abs(), in_x && in_y, Direction::South),
        ];
        for (d, inside, face) in faces {
            if inside && d < best.0 {
                best = (d, face);
            }
        }
    }
    best.1
}

/// `projectile.getOwner()`.
pub fn owner(level: &Level, projectile: EntityId) -> Option<EntityId> {
    let ents = &level.entities;
    ents.arrow(projectile)
        .and_then(|a| a.owner)
        .or_else(|| ents.missile(projectile).and_then(|m| m.owner))
        .or_else(|| match ents.special(projectile) {
            Some(crate::mob::boss::special::SpecialData::ThrownPotion(p)) => p.owner,
            _ => None,
        })
        .filter(|&o| ents.store.contains(o))
}

/// `projectile.isOnFire()`.
fn on_fire(level: &Level, projectile: EntityId) -> bool {
    let ents = &level.entities;
    ents.arrow(projectile).is_some_and(|a| a.fire)
        || ents
            .store
            .get(projectile)
            .is_some_and(|e| e.flags().contains(EntityFlags::ON_FIRE))
}

fn is_player(level: &Level, id: EntityId) -> bool {
    level.entities.player(id).is_some()
}

/// `Projectile.mayInteract(level, pos)`: a player owner may where they may build (the world
/// border), other owners only with `mob_griefing`, no owner always.
pub fn may_interact(level: &Level, projectile: EntityId, pos: BlockPos) -> bool {
    match owner(level, projectile) {
        Some(o) if is_player(level, o) => level
            .border
            .is_within_bounds(f64::from(pos.x), f64::from(pos.z)),
        Some(_) => level.settings.mob_griefing,
        None => true,
    }
}

/// `Projectile.mayBreak(level, pos)`: impact projectiles with `projectiles_can_break_blocks`;
/// not for an adventure mode owner (the projectile's `can_break` is not kept).
pub fn may_break(level: &Level, projectile: EntityId, _pos: BlockPos) -> bool {
    let Some(kind) = level.entities.store.get(projectile).map(|e| e.kind()) else {
        return false;
    };
    let adventure = owner(level, projectile)
        .and_then(|o| level.entities.player(o))
        .is_some_and(|p| p.game_mode == vanillite_config::GameMode::Adventure);
    crate::effect::entity_in_tag(kind, "minecraft:impact_projectiles")
        && level.settings.projectiles_can_break_blocks
        && !adventure
}

/// `TargetBlock.onProjectileHit`.
fn target(level: &mut Level, _: BlockState, pos: BlockPos, hit: &ProjectileHit) {
    let Some(kind) = level.entities.store.get(hit.projectile).map(|e| e.kind()) else {
        return;
    };
    // `projectile instanceof AbstractArrow`: arrows, spectral arrows and tridents.
    let arrow =
        crate::mob::projectile::is_arrow_kind(kind) || kind == vanillite_data::entity::ids::TRIDENT;
    let owner = owner(level, hit.projectile);
    super::sensor::target_projectile_hit(
        level,
        pos,
        hit.face,
        hit.at,
        arrow,
        hit.projectile,
        owner,
    );
}

/// `BellBlock.onProjectileHit`: `onHit` with the player owner, from the correct side only.
fn bell(level: &mut Level, state: BlockState, pos: BlockPos, hit: &ProjectileHit) {
    let player = owner(level, hit.projectile).filter(|&o| is_player(level, o));
    let click_y = hit.at.y - f64::from(pos.y);
    super::bell::on_hit(level, state, pos, hit.face, click_y, player);
}

/// `TntBlock.onProjectileHit`: a burning projectile primes the TNT.
fn tnt(level: &mut Level, _: BlockState, pos: BlockPos, hit: &ProjectileHit) {
    if !on_fire(level, hit.projectile) || !may_interact(level, hit.projectile, pos) {
        return;
    }
    // `owner instanceof LivingEntity`.
    let source = owner(level, hit.projectile).filter(|&o| {
        level
            .entities
            .store
            .get(o)
            .is_some_and(|e| crate::block::plate::is_living(e.kind()))
    });
    if super::tnt::prime(level, pos, source) {
        level.remove_block(pos, false);
    }
}

/// `CampfireBlock.onProjectileHit`: a burning projectile lights an unlit, dry campfire.
fn campfire(level: &mut Level, state: BlockState, pos: BlockPos, hit: &ProjectileHit) {
    if on_fire(level, hit.projectile)
        && may_interact(level, hit.projectile, pos)
        && !state.flag("lit")
        && !state.flag("waterlogged")
    {
        level.set_block(pos, state.set_flag("lit", true), update::ALL_IMMEDIATE);
    }
}

/// `AbstractCandleBlock.onProjectileHit`: a burning projectile lights candles (`canBeLit`).
fn candle(level: &mut Level, state: BlockState, pos: BlockPos, hit: &ProjectileHit) {
    let can_be_lit = !state.flag("lit")
        && (block_tags().candle_cakes.contains(state) || !state.flag("waterlogged"));
    if on_fire(level, hit.projectile) && can_be_lit {
        level.set_block(pos, state.set_flag("lit", true), update::ALL_IMMEDIATE);
    }
}

/// `ChorusFlowerBlock.onProjectileHit`.
fn chorus_flower(level: &mut Level, _: BlockState, pos: BlockPos, hit: &ProjectileHit) {
    if may_interact(level, hit.projectile, pos) && may_break(level, hit.projectile, pos) {
        level.destroy_block(pos, true, Some(hit.projectile));
    }
}

/// `DecoratedPotBlock.onProjectileHit`: the pot cracks and breaks.
fn decorated_pot(level: &mut Level, state: BlockState, pos: BlockPos, hit: &ProjectileHit) {
    if may_interact(level, hit.projectile, pos) && may_break(level, hit.projectile, pos) {
        level.set_block(
            pos,
            state.set_flag("cracked", true),
            update::SKIP_BLOCK_ENTITY_SIDEEFFECTS | update::INVISIBLE,
        );
        level.destroy_block(pos, true, Some(hit.projectile));
    }
}

/// `AmethystBlock.onProjectileHit`: the chime.
fn amethyst(level: &mut Level, _: BlockState, pos: BlockPos, _: &ProjectileHit) {
    let pitch = 0.5 + level.random().next_float() * 1.2;
    level.play_block_sound(
        None,
        pos,
        sound_id("block.amethyst_block.chime"),
        SOUND_BLOCKS,
        1.0,
        pitch,
    );
}

/// `SpeleothemBlock.onProjectileHit`: a fast trident breaks pointed dripstone.
fn speleothem(level: &mut Level, _: BlockState, pos: BlockPos, hit: &ProjectileHit) {
    let trident = level
        .entities
        .store
        .get(hit.projectile)
        .is_some_and(|e| e.kind() == vanillite_data::entity::ids::TRIDENT);
    if may_interact(level, hit.projectile, pos)
        && may_break(level, hit.projectile, pos)
        && trident
        && hit.speed > 0.6
    {
        level.destroy_block(pos, true, None);
    }
}

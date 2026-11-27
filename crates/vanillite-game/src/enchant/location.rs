//! The location-based enchantment effects of players (`minecraft:location_changed` and the
//! Soul Speed `minecraft:tick` sound): Frost Walker's `replace_disk` and Soul Speed's
//! attribute modifiers and durability. Vanilla runs them from `LivingEntity.onChangedBlock`
//! (a new block position, a landing, new equipment) and `EnchantmentHelper.tickEffects`.

use vanillite_base::random::RandomSource;
use vanillite_base::{Aabb, BlockPos, Vec3};
use vanillite_data::block::{BlockState, ids as blocks};
use vanillite_protocol::packets::play::clientbound::SoundSource;
use vanillite_world::entity::{EntityFlags, EntityMut};

use crate::effect::Operation;
use crate::effect::attribute::Attribute;
use crate::item::EquipmentSlot;
use crate::level::Level;
use crate::player::PlayerData;
use crate::survival::player::Env;

/// The id of Soul Speed's modifiers in the feet slot (`idForSlot`).
const SOUL_SPEED_ID: &str = "minecraft:enchantment.soul_speed/feet";

/// The Frost Walker and Soul Speed levels of the boots.
fn feet_levels(player: &PlayerData) -> (i32, i32) {
    let boots = player.inventory.equipment(EquipmentSlot::Feet);
    if boots.is_empty() {
        return (0, 0);
    }
    crate::player::play::game_data().map_or((0, 0), |d| {
        (
            d.enchantment_level(boots, "minecraft:frost_walker"),
            d.enchantment_level(boots, "minecraft:soul_speed"),
        )
    })
}

/// `getBlockPosBelowThatAffectsMyMovement` is in `#soul_speed_blocks`.
fn on_soul_block(env: &Env<'_>, pos: Vec3) -> bool {
    let below = BlockPos::new(
        vanillite_base::math::floor(pos.x),
        vanillite_base::math::floor(pos.y - 0.500_001),
        vanillite_base::math::floor(pos.z),
    );
    env.blocks
        .block_state(below)
        .is_some_and(|s| s.block() == blocks::SOUL_SAND || s.block() == blocks::SOUL_SOIL)
}

/// The tracked state of a player's location-based enchantments.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LocationState {
    /// `lastPos` of `LivingEntity.baseTick`.
    pub last_pos: Option<BlockPos>,
    /// The boots' Frost Walker and Soul Speed levels at the last tick (new equipment runs the
    /// effects at once, `collectEquipmentChanges`).
    pub last_levels: (i32, i32),
    /// Soul Speed's attribute modifiers are active (`activeLocationDependentEnchantments`).
    pub soul_speed_active: bool,
}

/// The `baseTick` part of a living player: `EnchantmentHelper.tickEffects` (Soul Speed's
/// 35 % sound roll every tick), then `onChangedBlock` when the block position or the boots
/// changed. Returns whether the equipment attributes must be synced again.
pub fn base_tick(entity: &EntityMut<'_>, player: &mut PlayerData, env: &mut Env<'_>) -> bool {
    let levels = feet_levels(player);
    let pos = entity.as_ref().pos();
    if levels.1 > 0 {
        tick_soul_speed(entity, player, env, pos);
    }
    let block = BlockPos::new(
        vanillite_base::math::floor(pos.x),
        vanillite_base::math::floor(pos.y),
        vanillite_base::math::floor(pos.z),
    );
    let state = &mut player.survival.location;
    let moved = state.last_pos != Some(block);
    let equipped = state.last_levels != levels;
    state.last_pos = Some(block);
    state.last_levels = levels;
    if moved || equipped {
        return on_changed_block(entity, player, env);
    }
    false
}

/// `Enchantment.tick` of Soul Speed: on soul blocks, on the ground and moving, every fifth
/// tick a `soul` particle (its position and velocity from the player's random); the sound's
/// `random_chance` 0.35 is drawn from the level random every tick, and under the same
/// conditions it plays `particle.soul_escape` (volume 0.6, pitch uniform 0.6..1 from the
/// player's random).
fn tick_soul_speed(entity: &EntityMut<'_>, player: &mut PlayerData, env: &mut Env<'_>, pos: Vec3) {
    let flags = entity.as_ref().flags();
    let movement = player.connection.movement.known_movement;
    let active = !player.survival.flying
        && flags.contains(EntityFlags::ON_GROUND)
        && (movement.x * movement.x + movement.z * movement.z).sqrt() >= 9.999_999_747_378_752e-6
        && player.survival.tick_count % 5 == 0
        && on_soul_block(env, pos);
    if active {
        let width = f64::from(entity.as_ref().dims().width);
        let r = &mut player.survival.random;
        let x = pos.x + (r.next_double() - 0.5) * width;
        let z = pos.z + (r.next_double() - 0.5) * width;
        let particle = crate::level::particles::packet(
            crate::level::particles::simple("minecraft:soul"),
            Vec3::new(x, pos.y + 0.1, z),
            0,
            Vec3::new(movement.x * -0.2, 0.1, movement.z * -0.2),
            1.0,
        );
        player
            .survival
            .world_actions
            .push(crate::mob::WorldAction::Particles(Box::new(particle)));
    }
    if env.random.next_float() >= 0.35 || !active {
        return;
    }
    let pitch = 0.6 + player.survival.random.next_float() * 0.4;
    crate::survival::play_sound(
        env.out,
        None,
        pos,
        "minecraft:particle.soul_escape",
        SoundSource::Players,
        0.6,
        pitch,
        crate::survival::sound_seed(),
    );
}

/// `LivingEntity.onChangedBlock` → `EnchantmentHelper.runLocationChangedEffects` of the boots:
/// Frost Walker freezes the water around the player; Soul Speed (de)activates its modifiers
/// and may cost a point of durability. Returns whether the Soul Speed modifiers changed.
pub fn on_changed_block(
    entity: &EntityMut<'_>,
    player: &mut PlayerData,
    env: &mut Env<'_>,
) -> bool {
    let (frost, soul) = feet_levels(player);
    let flags = entity.as_ref().flags();
    let on_ground = flags.contains(EntityFlags::ON_GROUND);
    let passenger = flags.contains(EntityFlags::PASSENGER);
    let pos = entity.as_ref().pos();
    if frost > 0 && on_ground && !passenger {
        // `clamped(linear(3, 1), 0, 16)`.
        #[allow(clippy::cast_possible_truncation, clippy::as_conversions)]
        let radius = (3.0 + (frost - 1) as f32).clamp(0.0, 16.0) as i32;
        player
            .survival
            .world_actions
            .push(crate::mob::WorldAction::FrostWalker {
                pos,
                radius,
                source: entity.as_ref().id(),
            });
    }
    let mut changed = false;
    if soul > 0 {
        let was_active = player.survival.location.soul_speed_active;
        let on_soul = on_soul_block(env, pos);
        let flying = player.survival.flying;
        let matches = !passenger
            && !flying
            && if was_active {
                on_soul || !on_ground
            } else {
                on_soul
            };
        if matches {
            if !was_active {
                player.survival.location.soul_speed_active = true;
                changed = true;
            }
        } else if was_active {
            player.survival.location.soul_speed_active = false;
            changed = true;
        }
        // `change_item_damage` 1 with `random_chance` 0.04 (level random), on the ground on
        // a soul block.
        if env.random.next_float() < 0.04 && on_ground && on_soul {
            crate::elytra::hurt_slot(entity, player, env, EquipmentSlot::Feet);
        }
    } else if player.survival.location.soul_speed_active {
        // `stopLocationBasedEffects` of boots taken off.
        player.survival.location.soul_speed_active = false;
        changed = true;
    }
    changed
}

/// The modifiers of the active location-based enchantments (Soul Speed: movement speed
/// `linear(0.0405, 0.0105)` and movement efficiency 1).
pub fn location_modifiers(player: &PlayerData, out: &mut crate::effect::attribute::Modifiers) {
    if !player.survival.location.soul_speed_active {
        return;
    }
    let soul = feet_levels(player).1.max(1);
    #[allow(clippy::cast_precision_loss, clippy::as_conversions)]
    let speed = 0.0405 + 0.0105 * (soul - 1) as f32;
    out.set(
        Attribute::MOVEMENT_SPEED,
        SOUL_SPEED_ID,
        f64::from(speed),
        Operation::AddValue,
    );
    if let Some(id) = crate::item::attribute_id("minecraft:movement_efficiency")
        .and_then(|i| u8::try_from(i).ok())
    {
        out.set(Attribute(id), SOUL_SPEED_ID, 1.0, Operation::AddValue);
    }
}

/// Frost Walker's `replace_disk` (`ReplaceDisk.apply`): in the disk of `radius` around `pos`
/// one block below the feet, every water source with air above and no entity in the way
/// becomes `frosted_ice[age=0]` (`setBlockAndUpdate`; its `onPlace` schedules the melting)
/// with a `block_place` game event of the player.
pub fn frost_walker(
    level: &mut Level,
    pos: Vec3,
    radius: i32,
    source: vanillite_world::entity::EntityId,
) {
    let center = BlockPos::new(
        vanillite_base::math::floor(pos.x),
        vanillite_base::math::floor(pos.y) - 1,
        vanillite_base::math::floor(pos.z),
    );
    let water = blocks::WATER.default_state();
    let ice = blocks::FROSTED_ICE.default_state();
    let limit = f64::from(radius * radius);
    // `BlockPos.betweenClosed`: x fastest, then y (one layer), then z.
    for z in center.z - radius..=center.z + radius {
        for x in center.x - radius..=center.x + radius {
            let at = BlockPos::new(x, center.y, z);
            let dx = f64::from(x) + 0.5 - pos.x;
            let dz = f64::from(z) + 0.5 - pos.z;
            if dx * dx + dz * dz >= limit {
                continue;
            }
            if !level.block(BlockPos::new(x, center.y + 1, z)).is_air()
                || level.block(at) != water
                || !unobstructed(level, ice, at)
            {
                continue;
            }
            if level.set_block_and_update(at, ice) {
                let context = crate::level::game_event::GameEventContext {
                    source: Some(source),
                    affected: None,
                    owner: None,
                };
                level.game_event(
                    crate::level::game_event::events().block_place,
                    Vec3::new(
                        f64::from(x) + 0.5,
                        f64::from(center.y) + 0.5,
                        f64::from(z) + 0.5,
                    ),
                    context,
                );
            }
        }
    }
}

/// `Level.isUnobstructed(state, pos, CollisionContext.empty())`.
fn unobstructed(level: &Level, state: BlockState, pos: BlockPos) -> bool {
    let (dx, dy, dz) = (f64::from(pos.x), f64::from(pos.y), f64::from(pos.z));
    state.collision_shape().iter().all(|b| {
        let shape = Aabb::new(
            b[0] + dx,
            b[1] + dy,
            b[2] + dz,
            b[3] + dx,
            b[4] + dy,
            b[5] + dz,
        );
        let mut blocked = false;
        level.entities.store.for_each_in(&shape, false, |id| {
            blocked |= crate::block::dispenser::blocks_building(level, id);
        });
        !blocked
    })
}

/// Frost Walker's `damage_immunity` (`EnchantmentHelper.isImmuneToDamage`): boots with Frost
/// Walker ignore `#burn_from_stepping` damage (magma blocks, campfires, hot sulfur cubes)
/// that does not bypass invulnerability.
pub fn frost_walker_immune(
    types: &crate::survival::damage::DamageTypes,
    kind: crate::survival::damage::DamageKind,
    boots: &crate::item::ItemStack,
) -> bool {
    use crate::survival::damage::tag;
    if boots.is_empty() || types.is(kind, tag::BYPASSES_INVULNERABILITY) {
        return false;
    }
    let stepping = kind == types.hot_floor
        || kind == types.campfire
        || types.kind("minecraft:sulfur_cube_hot") == Some(kind);
    stepping
        && crate::player::play::game_data()
            .is_some_and(|d| d.enchantment_level(boots, "minecraft:frost_walker") > 0)
}

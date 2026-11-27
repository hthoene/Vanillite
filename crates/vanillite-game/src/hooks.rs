//! Hooks the server installs into the game so plugins can observe and change game events
//! (block breaking and placing, damage, thrown projectiles, totems, fishing). Each method has a no-op default; the server checks
//! for listeners before it does any work, so an unused hook costs one virtual call.

use vanillite_base::BlockPos;
use vanillite_data::block::BlockState;

/// Game events the server forwards to plugins.
pub trait GameHooks: Sync {
    /// An entity is about to take `amount` damage of `damage_type` (e.g. `minecraft:fall`).
    /// Returns the damage to apply, or `None` to cancel it.
    fn entity_damage(
        &self,
        _entity: u128,
        _entity_type: &str,
        _damage_type: &str,
        amount: f32,
    ) -> Option<f32> {
        Some(amount)
    }

    /// A player is about to break the block `state` at `pos`. `false` cancels it.
    fn block_break(&self, _player: u128, _pos: BlockPos, _state: BlockState) -> bool {
        true
    }

    /// A player is about to place `state` at `pos`. `false` cancels it.
    fn block_place(&self, _player: u128, _pos: BlockPos, _state: BlockState) -> bool {
        true
    }

    /// A player is about to throw or shoot a projectile of `projectile_type` (e.g.
    /// `minecraft:snowball`; Paper's `PlayerLaunchProjectileEvent`). `false` cancels it: the
    /// item stays.
    fn projectile_launch(&self, _shooter: u128, _projectile_type: &str) -> bool {
        true
    }

    /// A totem of undying (`death_protection`) is about to save `entity` from death (Paper's
    /// `EntityResurrectEvent`). `false` lets it die.
    fn entity_resurrect(&self, _entity: u128, _entity_type: &str) -> bool {
        true
    }

    /// A fishing rod action of `player` (Paper's `PlayerFishEvent`): `state` is `fishing`
    /// (the cast), `caught_entity`, `caught_fish`, `in_ground` or `reel_in`. `false` cancels
    /// it: no cast, or the bobber stays without pulling or loot.
    fn player_fish(&self, _player: u128, _state: &str) -> bool {
        true
    }
}

/// No hooks (tests, worlds without plugins).
#[derive(Debug, Default, Clone, Copy)]
pub struct NoHooks;

impl GameHooks for NoHooks {}

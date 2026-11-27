//! Hooks the server installs into the game so plugins can observe and change game events
//! (block breaking and placing, damage, deaths, item use, thrown projectiles, totems, fishing,
//! sheep dyeing, entity transformation, item drops of mobs). Each method has a no-op default; the server checks
//! for listeners before it does any work, so an unused hook costs one virtual call.

use vanillite_base::BlockPos;
use vanillite_data::block::BlockState;
use vanillite_protocol::text::Component;

/// What a player's death does that hooks may change.
#[derive(Clone, Debug, PartialEq)]
pub struct Death {
    /// The death message; `None` shows none.
    pub message: Option<Component>,
    /// The player keeps its inventory and experience.
    pub keep_inventory: bool,
}

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

    /// `player` is about to dye the sheep `sheep` with the dye colour `color` (e.g. `lime`;
    /// Paper's `SheepDyeWoolEvent`). `false` cancels it: the dye stays.
    fn sheep_dye_wool(&self, _sheep: u128, _color: &str, _player: u128) -> bool {
        true
    }

    /// `entity` of `from_type` is about to turn into `to_type` (e.g. `minecraft:pig` into
    /// `minecraft:zombified_piglin`) for `reason` (`lightning`; Paper's
    /// `EntityTransformEvent`). `false` cancels it: the entity stays what it is.
    fn entity_transform(
        &self,
        _entity: u128,
        _from_type: &str,
        _to_type: &str,
        _reason: &str,
    ) -> bool {
        true
    }

    /// `entity` of `entity_type` is about to drop `item` on its own (a chicken's egg; Paper's
    /// `EntityDropItemEvent`). `false` cancels it: nothing drops.
    fn entity_drop_item(&self, _entity: u128, _entity_type: &str, _item: &str) -> bool {
        true
    }

    /// A player is about to fall asleep in the bed `state` whose head is at `pos`. `false`
    /// cancels it (the respawn point a bed sets is already set).
    fn bed_enter(&self, _player: u128, _pos: BlockPos, _state: BlockState) -> bool {
        true
    }

    /// A player left the bed `state` at `pos` (woke up, was woken or left it).
    fn bed_leave(&self, _player: u128, _pos: BlockPos, _state: BlockState) {}

    /// A bed or respawn anchor (`state` at `pos`) is about to become the player's respawn
    /// point. `false` cancels it.
    fn spawn_change(&self, _player: u128, _pos: BlockPos, _state: BlockState) -> bool {
        true
    }

    /// A player died of `damage_type`, caused by `killer` when known; hooks may change its
    /// death message and whether it keeps its inventory.
    fn player_death(
        &self,
        _player: u128,
        _damage_type: &str,
        _killer: Option<u128>,
        _death: &mut Death,
    ) {
    }

    /// A mob of `entity_type` died of `damage_type`, caused by `killer` when known.
    fn entity_death(
        &self,
        _entity: u128,
        _entity_type: &str,
        _damage_type: &str,
        _killer: Option<u128>,
    ) {
    }

    /// A player uses `item` in `hand` (0 main, 1 off) on a block (position, clicked face as
    /// `Direction` index, state) or in the air. `false` cancels it.
    fn player_interact(
        &self,
        _player: u128,
        _hand: u32,
        _item: &str,
        _block: Option<(BlockPos, u32, BlockState)>,
    ) -> bool {
        true
    }
}

/// No hooks (tests, worlds without plugins).
#[derive(Debug, Default, Clone, Copy)]
pub struct NoHooks;

impl GameHooks for NoHooks {}

//! What breaking blocks drops (`Block.dropResources`, `Entity.spawnAtLocation`).
//!
//! Loot tables and item entities are not ported yet: these are the seams the block and fluid
//! code calls, and they drop nothing for now.

use vanillite_base::{BlockPos, Vec3};
use vanillite_data::block::{BlockId, BlockState};
use vanillite_world::entity::EntityId;

use crate::level::Level;

/// `Block.dropResources(state, level, pos, blockEntity, breaker, tool)`: the block's loot at
/// `pos`, if the `doTileDrops` game rule allows.
pub fn drop_resources(level: &mut Level, pos: BlockPos, state: BlockState, breaker: Option<EntityId>) {
    if !level.settings.block_drops {
        return;
    }
    let _ = (pos, state, breaker);
}

/// `spawnAtLocation(level, block)`: the item of `block` as an item entity at `pos`.
pub fn spawn_block_item(level: &mut Level, pos: Vec3, block: BlockId) {
    let _ = (level, pos, block);
}

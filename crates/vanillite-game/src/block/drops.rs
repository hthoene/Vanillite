//! What breaking blocks drops (`Block.dropResources`, `Entity.spawnAtLocation`) and how item
//! entities enter the level (`new ItemEntity` + `addFreshEntity`).

use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Vec3};
use vanillite_data::block::{BlockId, BlockState};
use vanillite_world::entity::EntityId;

use crate::item::ItemStack;
use crate::item_entity::ItemData;
use crate::level::Level;
use crate::loot::BlockLootContext;

/// `Block.dropResources(state, level, pos, blockEntity, breaker, tool)` without a tool: the
/// block's loot at `pos`, if the `doTileDrops` game rule allows. Player breaks evaluate the
/// loot with their tool in [`crate::player::play`].
pub fn drop_resources(
    level: &mut Level,
    pos: BlockPos,
    state: BlockState,
    breaker: Option<EntityId>,
) {
    let _ = breaker;
    if !level.settings.block_drops {
        return;
    }
    let Some(data) = crate::player::play::game_data() else {
        return;
    };
    let mut stacks = Vec::new();
    let tool = ItemStack::EMPTY;
    let item_in_tag = |item: u16, tag: &str| data.item_in_tag(item, tag);
    let loot = BlockLootContext {
        state,
        tool: &tool,
        enchantment_level: &|_| 0,
        item_in_tag: &item_in_tag,
        explosion_radius: None,
        entity: None,
    };
    data.loot.block_drops(&loot, level.random(), &mut stacks);
    for stack in stacks {
        pop_resource(level, pos, stack);
    }
}

/// `Block.popResource(level, pos, stack)`: the stack as an item entity around the block's
/// centre.
pub fn pop_resource(level: &mut Level, pos: BlockPos, stack: ItemStack) {
    if stack.is_empty() {
        return;
    }
    let half_height = 0.125;
    let x = f64::from(pos.x) + 0.5 + level.random().next_double() * 0.5 - 0.25;
    let y = f64::from(pos.y) + 0.5 + level.random().next_double() * 0.5 - 0.25 - half_height;
    let z = f64::from(pos.z) + 0.5 + level.random().next_double() * 0.5 - 0.25;
    let velocity = default_velocity(level);
    spawn_item(level, Vec3::new(x, y, z), stack, velocity, 10, None);
}

/// `spawnAtLocation(level, block)`: the item of `block` as an item entity at `pos`.
pub fn spawn_block_item(level: &mut Level, pos: Vec3, block: BlockId) {
    let item = block.info().item;
    if item == u16::MAX || item == 0 {
        return;
    }
    let velocity = default_velocity(level);
    spawn_item(level, pos, ItemStack::new(item, 1), velocity, 10, None);
}

/// The velocity of `new ItemEntity(level, x, y, z, stack)`.
fn default_velocity(level: &mut Level) -> Vec3 {
    let x = level.random().next_double() * 0.2 - 0.1;
    let z = level.random().next_double() * 0.2 - 0.1;
    Vec3::new(x, 0.2, z)
}

/// Adds an item entity with `pickup_delay` ticks before it can be picked up and the UUID of
/// its thrower.
pub fn spawn_item(
    level: &mut Level,
    pos: Vec3,
    stack: ItemStack,
    velocity: Vec3,
    pickup_delay: i16,
    thrower: Option<u128>,
) -> Option<EntityId> {
    if stack.is_empty() {
        return None;
    }
    let uuid = level.new_uuid();
    let y_rot = level.random().next_float() * 360.0;
    let network_id = level.network_ids().next();
    Some(level.entities.spawn_item(
        network_id,
        uuid,
        pos,
        velocity,
        y_rot,
        ItemData::new(stack, pickup_delay, thrower),
    ))
}

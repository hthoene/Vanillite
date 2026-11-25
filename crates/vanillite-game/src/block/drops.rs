//! What breaking blocks drops (`Block.dropResources`, `Entity.spawnAtLocation`) and how item
//! entities enter the level (`new ItemEntity` + `addFreshEntity`).

use vanillite_base::random::{LegacyRandom, RandomSource};
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
    let Some(data) = crate::player::play::game_data() else {
        return;
    };
    let mut stacks = Vec::new();
    let tool = ItemStack::EMPTY;
    let entity = level.block_entity(pos).cloned();
    let components = |id: u16| {
        entity
            .as_ref()
            .and_then(|e| crate::level::block_entity::collect_component(e, id))
    };
    let item_in_tag = |item: u16, tag: &str| data.item_in_tag(item, tag);
    let loot = BlockLootContext {
        state,
        tool: &tool,
        enchantment_level: &|_| 0,
        item_in_tag: &item_in_tag,
        explosion_radius: None,
        this_present: breaker.is_some(),
        entity: None,
        block_entity: Some(&components),
    };
    // `LootContext.Builder.create`: the table's random sequence, else the level random.
    match data.loot.block_sequence(state) {
        Some(key) => {
            let sequences = std::sync::Arc::clone(level.random_sequences());
            let mut sequences = sequences
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            data.loot
                .block_drops(&loot, sequences.get(key), &mut stacks);
        }
        None => data.loot.block_drops(&loot, level.random(), &mut stacks),
    }
    for stack in stacks {
        pop_resource(level, pos, stack);
    }
}

/// `BlockState.getDrops` of a block an explosion destroys (`onExplosionHit`): its loot without
/// a tool, with the explosion radius for `explosion_decay` when the blocks decay. The loot
/// is rolled whether or not blocks drop (`popResource` checks that).
pub fn explosion_loot(
    level: &mut Level,
    pos: BlockPos,
    state: BlockState,
    radius: Option<f32>,
    source: Option<EntityId>,
    out: &mut Vec<ItemStack>,
) {
    let Some(data) = crate::player::play::game_data() else {
        return;
    };
    let tool = ItemStack::EMPTY;
    let entity = level.block_entity(pos).cloned();
    let components = |id: u16| {
        entity
            .as_ref()
            .and_then(|e| super::container::component(e, id))
    };
    let item_in_tag = |item: u16, tag: &str| data.item_in_tag(item, tag);
    let loot = BlockLootContext {
        state,
        tool: &tool,
        enchantment_level: &|_| 0,
        item_in_tag: &item_in_tag,
        explosion_radius: radius,
        this_present: source.is_some(),
        entity: None,
        block_entity: Some(&components),
    };
    let mut random = std::mem::replace(level.random(), LegacyRandom::new(0));
    data.loot.roll_block(level, &loot, &mut random, out);
    *level.random() = random;
}

/// `Block.popResource(level, pos, stack)`: the stack as an item entity around the block's
/// centre.
pub fn pop_resource(level: &mut Level, pos: BlockPos, stack: ItemStack) {
    let half_height = 0.125;
    let x = f64::from(pos.x) + 0.5 + level.random().next_double() * 0.5 - 0.25;
    let y = f64::from(pos.y) + 0.5 + level.random().next_double() * 0.5 - 0.25 - half_height;
    let z = f64::from(pos.z) + 0.5 + level.random().next_double() * 0.5 - 0.25;
    // The position is drawn either way; empty stacks and the `block_drops` rule only stop the
    // item entity.
    if stack.is_empty() || !level.settings.block_drops {
        return;
    }
    spawn_item_moving(level, Vec3::new(x, y, z), stack, None, 10, None);
}

/// `spawnAtLocation(level, block)`: the item of `block` as an item entity at `pos`.
pub fn spawn_block_item(level: &mut Level, pos: Vec3, block: BlockId) {
    let item = block.info().item;
    if item == u16::MAX || item == 0 {
        return;
    }
    spawn_item_moving(level, pos, ItemStack::new(item, 1), None, 10, None);
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
    spawn_item_moving(level, pos, stack, Some(velocity), pickup_delay, thrower)
}

/// [`spawn_item`] with `new ItemEntity(level, x, y, z, stack)`'s random velocity if `velocity`
/// is `None`. The item draws its yaw, then the velocity, from its own random.
pub fn spawn_item_moving(
    level: &mut Level,
    pos: Vec3,
    stack: ItemStack,
    velocity: Option<Vec3>,
    pickup_delay: i16,
    thrower: Option<u128>,
) -> Option<EntityId> {
    if stack.is_empty() {
        return None;
    }
    // The `ItemEntity` constructor draws `bobOffs`, the yaw and the motion from the item's own
    // random (seeded from the entity seed stream).
    let (uuid, y_rot, motion) = crate::mob::new_item_entity();
    let velocity = velocity.unwrap_or(motion);
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

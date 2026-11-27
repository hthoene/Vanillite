//! Entity–block contact that changes the world: `stepOn` of the blocks entities stand on and
//! `FarmlandBlock.fallOn` trampling.

use vanillite_base::{Aabb, BlockPos};
use vanillite_data::block::{BlockId, ids};
use vanillite_world::entity::EntityId;
use vanillite_worldgen::behaviour::StateExt;

use crate::level::Level;
use crate::level::game_event::{GameEventContext, events};

/// Whether `stepOn` of the block changes the world (the mob tick queues a
/// [`crate::mob::WorldAction::StepOn`] for it): sculk sensors and shriekers, redstone ore.
pub fn has_step_on(block: BlockId) -> bool {
    super::sculk::has_step_on(block) || is_redstone_ore(block)
}

fn is_redstone_ore(block: BlockId) -> bool {
    block == ids::REDSTONE_ORE || block == ids::DEEPSLATE_REDSTONE_ORE
}

/// `LilyPadBlock.entityInside`: a boat breaks the pad.
pub(super) fn lily_pad_entity_inside(
    level: &mut Level,
    _state: vanillite_data::block::BlockState,
    pos: BlockPos,
    entity: EntityId,
) {
    if level.entities.boat(entity).is_some() {
        level.destroy_block(pos, true, Some(entity));
    }
}

/// `Block.stepOn` of the block at `pos` for an entity standing on it (`applyEffectsFromBlocks`;
/// the entity is not stepping carefully).
pub fn step_on(level: &mut Level, pos: BlockPos, entity: EntityId) {
    let state = level.block(pos);
    if is_redstone_ore(state.block()) {
        // `RedStoneOreBlock.stepOn`.
        super::weather::redstone_ore_interact(level, state, pos);
    } else {
        super::sculk::step_on_block(level, pos, entity);
    }
}

/// `FarmlandBlock.fallOn`'s trample test after the chance roll: living entities whose box is
/// larger than 0.512 cubic blocks (`getBbWidth² * getBbHeight`).
pub fn can_trample(width: f32, height: f32) -> bool {
    width * width * height > 0.512
}

/// Bukkit's `PlayerInteractEvent` `PHYSICAL`: whether a plugin cancelled the step of
/// `entity` on the block at `pos` (pressure plates, tripwire, farmland). Only players fire it.
pub fn physical_cancelled(level: &Level, pos: BlockPos, entity: EntityId) -> bool {
    let Some(hooks) = level.hooks_for(vanillite_abi::event::PLAYER_INTERACT) else {
        return false;
    };
    if level.entities.player(entity).is_none() {
        return false;
    }
    let Some(uuid) = level.entities.store.get(entity).map(|e| e.uuid()) else {
        return false;
    };
    let up = vanillite_base::Direction::ALL
        .iter()
        .position(|&d| d == vanillite_base::Direction::Up)
        .and_then(|i| u32::try_from(i).ok())
        .unwrap_or(1);
    !hooks.player_interact(
        uuid,
        vanillite_abi::interact::PHYSICAL,
        vanillite_abi::hand::MAIN,
        "",
        Some((pos, up, level.block(pos))),
    )
}

/// `FarmlandBlock.turnToBaseBlock(source, state, level, pos)`: dirt, entities on it pushed up
/// by the 1/16 block the farmland lacked, and a `block_change` game event. A player's trample
/// fires [`physical_cancelled`] first.
pub fn trample_farmland(level: &mut Level, pos: BlockPos, source: Option<EntityId>) {
    if !level.block(pos).is(ids::FARMLAND) {
        return;
    }
    if source.is_some_and(|s| physical_cancelled(level, pos, s)) {
        return;
    }
    let dirt = ids::DIRT.default_state();
    push_entities_up(level, pos, 15.0 / 16.0);
    level.set_block_and_update(pos, dirt);
    level.game_event_block(
        events().block_change,
        pos,
        GameEventContext {
            source,
            affected: Some(dirt),
        },
    );
}

/// `Block.pushEntitiesUp` from a block whose top was at `top` (fraction of the block) to a full
/// block: entities in the gap move up onto the new top. Players are left to their client.
pub fn push_entities_up(level: &mut Level, pos: BlockPos, top: f64) {
    let (x, y, z) = (f64::from(pos.x), f64::from(pos.y), f64::from(pos.z));
    push_entities_up_from(level, Aabb::new(x, y + top, z, x + 1.0, y + 1.0, z + 1.0));
}

/// `Block.pushEntitiesUp` for the box `gap` the new state adds: the entities in it move up
/// onto its top. Players are left to their client.
pub fn push_entities_up_from(level: &mut Level, gap: Aabb) {
    let mut found = Vec::new();
    level
        .entities
        .store
        .for_each_in(&gap, false, |id| found.push(id));
    for id in found {
        let Some(mut e) = level.entities.store.get_mut(id) else {
            continue;
        };
        if e.as_ref().kind() == vanillite_data::entity::ids::PLAYER {
            continue;
        }
        // `Shapes.collide(Y, box.move(0, 1, 0), [gap], -1)`: back down onto the new top.
        let min_y = e.as_ref().aabb().min_y + 1.0;
        let offset = if min_y >= gap.max_y - 1.0e-7 {
            (gap.max_y - min_y).max(-1.0)
        } else {
            -1.0
        };
        let p = e.as_ref().pos();
        e.set_pos(vanillite_base::Vec3::new(p.x, p.y + 1.0 + offset, p.z));
    }
}

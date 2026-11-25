//! Cakes and candles (`CakeBlock`, `CandleCakeBlock`, `CandleBlock`, `AbstractCandleBlock`):
//! eating slices, putting a candle on a whole cake and putting candles out by hand. Lighting
//! is the flint and steel's ([`crate::player::play`]).

use vanillite_base::BlockPos;
use vanillite_data::block::{BlockId, BlockKind, BlockState, ids};
use vanillite_world::entity::EntityId;
use vanillite_worldgen::behaviour::StateExt;

use super::{BlockBehaviour, Interaction, ItemInteraction, PlayerEffect, UseContext, sound_id};
use crate::item::{self, ItemStack};
use crate::level::game_event::{GameEventContext, events};
use crate::level::{Level, SOUND_BLOCKS};
use crate::stats::{Stat, StatType};

/// Fills the cake and candle behaviours.
pub(super) fn fill(block: BlockId, b: &mut BlockBehaviour) {
    let kind = block.info().kind;
    if block == ids::CAKE {
        b.use_item_on = Some(cake_use_item_on);
        b.use_without_item = Some(cake_use_without_item);
        b.analog_output = Some(|_, state, _, _| output_signal(state.int("bites").unwrap_or(0)));
    } else if kind == BlockKind::CandleCakeBlock {
        b.use_item_on = Some(candle_cake_use_item_on);
        b.use_without_item = Some(candle_cake_use_without_item);
        b.analog_output = Some(|_, _, _, _| output_signal(0));
    } else if kind == BlockKind::CandleBlock {
        b.use_item_on = Some(candle_use_item_on);
    }
}

/// `CakeBlock.getOutputSignal(bites)`.
fn output_signal(bites: i32) -> i32 {
    (7 - bites) * 2
}

/// `CandleCakeBlock.byCandle`: `minecraft:red_candle` → `minecraft:red_candle_cake`.
fn candle_cake_of(candle: BlockId) -> Option<BlockId> {
    BlockId::by_name(&format!("{}_cake", candle.name()))
}

/// `CakeBlock.useItemOn`: a candle goes on a whole cake.
fn cake_use_item_on(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    ctx: &UseContext,
    stack: &mut ItemStack,
) -> ItemInteraction {
    let id = item::item_id(stack);
    let is_candle = crate::player::play::game_data()
        .is_some_and(|d| d.item_in_tag(stack.item, "minecraft:candles"));
    let cake = id
        .block()
        .filter(|b| b.info().kind == BlockKind::CandleBlock)
        .and_then(candle_cake_of);
    match cake {
        Some(cake) if is_candle && state.int("bites") == Some(0) => {
            stack.shrink(1);
            level.play_block_sound(
                None,
                pos,
                sound_id("block.cake.add_candle"),
                SOUND_BLOCKS,
                1.0,
                1.0,
            );
            level.set_block_and_update(pos, cake.default_state());
            level.game_event_at(Some(ctx.player), events().block_change, pos);
            level.award_stat(ctx.player, Stat::of(StatType::Used, id.0), 1);
            ItemInteraction::Done(Interaction::Success)
        }
        _ => ItemInteraction::TryWithEmptyHand,
    }
}

/// `CakeBlock.useWithoutItem`.
fn cake_use_without_item(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    ctx: &UseContext,
) -> Interaction {
    eat(level, pos, state, ctx)
}

/// `CakeBlock.eat`: a slice feeds the player; the last one removes the cake.
fn eat(level: &mut Level, pos: BlockPos, state: BlockState, ctx: &UseContext) -> Interaction {
    if !ctx.can_eat {
        return Interaction::Pass;
    }
    level.award_stat(ctx.player, Stat::custom_named("minecraft:eat_cake_slice"), 1);
    // `FoodData.eat(2, 0.1F)`: saturation `nutrition * modifier * 2`.
    level.player_effect(ctx.player, PlayerEffect::Eat(2, 2.0 * 0.1 * 2.0));
    let bites = state.int("bites").unwrap_or(0);
    level.game_event_at(Some(ctx.player), events().eat, pos);
    if bites < 6 {
        level.set_block_and_update(pos, state.set_int("bites", bites + 1));
    } else {
        level.remove_block(pos, false);
        level.game_event_at(Some(ctx.player), events().block_destroy, pos);
    }
    Interaction::Success
}

/// `CandleCakeBlock.useItemOn`: an empty hand on the candle puts it out.
fn candle_cake_use_item_on(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    ctx: &UseContext,
    stack: &mut ItemStack,
) -> ItemInteraction {
    let id = item::item_id(stack);
    if id == vanillite_data::item::ids::FLINT_AND_STEEL
        || id == vanillite_data::item::ids::FIRE_CHARGE
    {
        return ItemInteraction::Pass;
    }
    // `candleHit`: above half the block.
    if ctx.hit.y - f64::from(pos.y) > 0.5 && stack.is_empty() && state.flag("lit") {
        extinguish(level, Some(ctx.player), state, pos);
        return ItemInteraction::Done(Interaction::Success);
    }
    ItemInteraction::TryWithEmptyHand
}

/// `CandleCakeBlock.useWithoutItem`: eating a slice drops the candle.
fn candle_cake_use_without_item(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    ctx: &UseContext,
) -> Interaction {
    let result = eat(level, pos, ids::CAKE.default_state(), ctx);
    if result == Interaction::Success {
        super::drops::drop_resources(level, pos, state, None);
    }
    result
}

/// `CandleBlock.useItemOn`: an empty hand puts lit candles out.
fn candle_use_item_on(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    ctx: &UseContext,
    stack: &mut ItemStack,
) -> ItemInteraction {
    if stack.is_empty() && ctx.may_build && state.flag("lit") {
        extinguish(level, Some(ctx.player), state, pos);
        return ItemInteraction::Done(Interaction::Success);
    }
    ItemInteraction::TryWithEmptyHand
}

/// `AbstractCandleBlock.extinguish` (the smoke particles are client side).
pub fn extinguish(level: &mut Level, player: Option<EntityId>, state: BlockState, pos: BlockPos) {
    level.set_block(
        pos,
        state.set_flag("lit", false),
        crate::level::update::ALL_IMMEDIATE,
    );
    level.play_block_sound(
        None,
        pos,
        sound_id("block.candle.extinguish"),
        SOUND_BLOCKS,
        1.0,
        1.0,
    );
    level.game_event_block(events().block_change, pos, GameEventContext::of(player));
}

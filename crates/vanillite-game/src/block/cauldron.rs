//! Cauldron interactions (`CauldronInteractions`, `AbstractCauldronBlock.useItemOn`,
//! `LayeredCauldronBlock.entityInside`): buckets and bottles fill and empty cauldrons, water
//! washes dye off leather, banners and shulker boxes. Rain, snow and dripstone filling are in
//! [`super::weather`] and [`super::dripstone`].

use vanillite_base::BlockPos;
use vanillite_data::block::{BlockId, BlockState, ids};
use vanillite_data::item::ids as items;
use vanillite_protocol::components::ids as components;
use vanillite_world::entity::EntityId;
use vanillite_worldgen::behaviour::{PosExt, StateExt};

use super::{BlockBehaviour, Interaction, ItemInteraction, PlayerEffect, UseContext, sound_id};
use crate::item::{self, ItemStack};
use crate::level::game_event::{GameEventContext, events};
use crate::level::{Level, SOUND_BLOCKS};
use crate::stats::{Stat, StatType};

/// Fills the cauldron behaviours.
pub(super) fn fill(block: BlockId, b: &mut BlockBehaviour) {
    if block == ids::CAULDRON
        || block == ids::WATER_CAULDRON
        || block == ids::LAVA_CAULDRON
        || block == ids::POWDER_SNOW_CAULDRON
    {
        b.use_item_on = Some(use_item_on);
    }
    if block == ids::WATER_CAULDRON || block == ids::POWDER_SNOW_CAULDRON {
        b.entity_inside = Some(entity_inside);
    }
}

/// `AbstractCauldronBlock.useItemOn`: the interaction of the cauldron's dispatcher for the
/// held item (`CauldronInteraction.Dispatcher.get`: tags first, then items).
fn use_item_on(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    ctx: &UseContext,
    stack: &mut ItemStack,
) -> ItemInteraction {
    let id = item::item_id(stack);
    let block = state.block();
    let water = block == ids::WATER_CAULDRON;
    // `addDefaultInteractions`: every cauldron takes the three filled buckets.
    if id == items::WATER_BUCKET {
        let full = ids::WATER_CAULDRON.default_state().set_int("level", 3);
        return empty_bucket(level, pos, ctx, stack, full, "item.bucket.empty");
    }
    if id == items::LAVA_BUCKET {
        if is_under_water(level, pos) {
            return ItemInteraction::Done(Interaction::Consume);
        }
        let lava = ids::LAVA_CAULDRON.default_state();
        return empty_bucket(level, pos, ctx, stack, lava, "item.bucket.empty_lava");
    }
    if id == items::POWDER_SNOW_BUCKET {
        if is_under_water(level, pos) {
            return ItemInteraction::Done(Interaction::Consume);
        }
        let full = ids::POWDER_SNOW_CAULDRON
            .default_state()
            .set_int("level", 3);
        return empty_bucket(
            level,
            pos,
            ctx,
            stack,
            full,
            "item.bucket.empty_powder_snow",
        );
    }
    let full = state.int("level") == Some(3);
    if block == ids::CAULDRON {
        if id == items::POTION && is_water_potion(stack) {
            bottle_used(
                level,
                ctx,
                stack,
                ItemStack::new(items::GLASS_BOTTLE.0, 1),
                id,
            );
            level.set_block_and_update(pos, ids::WATER_CAULDRON.default_state());
            play(level, pos, "item.bottle.empty");
            level.game_event_at(None, events().fluid_place, pos);
            return ItemInteraction::Done(Interaction::Success);
        }
    } else if water {
        let dye_removable = crate::player::play::game_data()
            .is_some_and(|d| d.item_in_tag(stack.item, "minecraft:cauldron_can_remove_dye"));
        if dye_removable {
            return dyed_item(level, state, pos, ctx, stack);
        }
        if id == items::BUCKET {
            let filled = ItemStack::new(items::WATER_BUCKET.0, 1);
            return fill_bucket(level, pos, ctx, stack, full, filled, "item.bucket.fill");
        }
        if id == items::GLASS_BOTTLE {
            bottle_used(level, ctx, stack, water_potion(), id);
            lower_fill_level(level, state, pos);
            play(level, pos, "item.bottle.fill");
            level.game_event_at(None, events().fluid_pickup, pos);
            return ItemInteraction::Done(Interaction::Success);
        }
        if id == items::POTION {
            if full || !is_water_potion(stack) {
                return ItemInteraction::TryWithEmptyHand;
            }
            bottle_used(
                level,
                ctx,
                stack,
                ItemStack::new(items::GLASS_BOTTLE.0, 1),
                id,
            );
            let level_now = state.int("level").unwrap_or(1);
            level.set_block_and_update(pos, state.set_int("level", level_now % 3 + 1));
            play(level, pos, "item.bottle.empty");
            level.game_event_at(None, events().fluid_place, pos);
            return ItemInteraction::Done(Interaction::Success);
        }
        let name = id.name();
        if name.ends_with("_banner") && !name.ends_with("_wall_banner") {
            return banner(level, state, pos, ctx, stack);
        }
        if name.ends_with("_shulker_box") {
            return shulker_box(level, state, pos, ctx, stack);
        }
    } else if id == items::BUCKET {
        let (filled, sound, can) = if block == ids::LAVA_CAULDRON {
            (items::LAVA_BUCKET, "item.bucket.fill_lava", true)
        } else {
            (
                items::POWDER_SNOW_BUCKET,
                "item.bucket.fill_powder_snow",
                full,
            )
        };
        let filled = ItemStack::new(filled.0, 1);
        return fill_bucket(level, pos, ctx, stack, can, filled, sound);
    }
    ItemInteraction::TryWithEmptyHand
}

fn play(level: &mut Level, pos: BlockPos, sound: &str) {
    level.play_block_sound(None, pos, sound_id(sound), SOUND_BLOCKS, 1.0, 1.0);
}

/// `isUnderWater`: water above the cauldron.
fn is_under_water(level: &Level, pos: BlockPos) -> bool {
    super::fluid::is_water(level.fluid(pos.above()))
}

/// `potion.is(Potions.WATER)`.
fn is_water_potion(stack: &ItemStack) -> bool {
    crate::recipe::potion_of(stack).is_some()
        && crate::recipe::potion_of(stack) == vanillite_data::registry::POTION.id("minecraft:water")
}

/// `PotionContents.createItemStack(Items.POTION, Potions.WATER)`.
fn water_potion() -> ItemStack {
    let mut stack = ItemStack::new(items::POTION.0, 1);
    if let Some(water) = vanillite_data::registry::POTION.id("minecraft:water") {
        stack.patch_mut().set(
            components::POTION_CONTENTS,
            crate::recipe::potion_contents(water),
        );
    }
    stack
}

/// `ItemUtils.createFilledResult(stack, player, new, limitCreativeStackSize)`, written back
/// into the held stack.
pub fn filled_result(
    level: &mut Level,
    ctx: &UseContext,
    stack: &mut ItemStack,
    new: ItemStack,
    limit_creative: bool,
) {
    if limit_creative && ctx.infinite {
        level.player_effect(ctx.player, PlayerEffect::GiveIfMissing(new));
        return;
    }
    // `consume(1, player)`: creative players keep their stack.
    if !ctx.infinite {
        stack.shrink(1);
    }
    if stack.is_empty() {
        *stack = new;
    } else {
        level.give_to_player(ctx.player, new);
    }
}

/// The stats and the filled result of a bottle interaction.
fn bottle_used(
    level: &mut Level,
    ctx: &UseContext,
    stack: &mut ItemStack,
    new: ItemStack,
    used: vanillite_data::item::ItemId,
) {
    filled_result(level, ctx, stack, new, true);
    level.award_stat(ctx.player, Stat::custom_named("minecraft:use_cauldron"), 1);
    level.award_stat(ctx.player, Stat::of(StatType::Used, used.0), 1);
}

/// `CauldronInteractions.fillBucket`: an empty bucket takes the cauldron's contents.
fn fill_bucket(
    level: &mut Level,
    pos: BlockPos,
    ctx: &UseContext,
    stack: &mut ItemStack,
    can_fill: bool,
    filled: ItemStack,
    sound: &str,
) -> ItemInteraction {
    if !can_fill {
        return ItemInteraction::TryWithEmptyHand;
    }
    let used = item::item_id(stack);
    filled_result(level, ctx, stack, filled, true);
    level.award_stat(ctx.player, Stat::custom_named("minecraft:use_cauldron"), 1);
    level.award_stat(ctx.player, Stat::of(StatType::Used, used.0), 1);
    level.set_block_and_update(pos, ids::CAULDRON.default_state());
    play(level, pos, sound);
    level.game_event_at(None, events().fluid_pickup, pos);
    ItemInteraction::Done(Interaction::Success)
}

/// `CauldronInteractions.emptyBucket`: a filled bucket fills the cauldron.
fn empty_bucket(
    level: &mut Level,
    pos: BlockPos,
    ctx: &UseContext,
    stack: &mut ItemStack,
    new_state: BlockState,
    sound: &str,
) -> ItemInteraction {
    let used = item::item_id(stack);
    filled_result(level, ctx, stack, ItemStack::new(items::BUCKET.0, 1), true);
    level.award_stat(ctx.player, Stat::custom_named("minecraft:fill_cauldron"), 1);
    level.award_stat(ctx.player, Stat::of(StatType::Used, used.0), 1);
    level.set_block_and_update(pos, new_state);
    play(level, pos, sound);
    level.game_event_at(None, events().fluid_place, pos);
    ItemInteraction::Done(Interaction::Success)
}

/// `CauldronInteractions.dyedItemIteration`: water washes the dye off.
fn dyed_item(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    ctx: &UseContext,
    stack: &mut ItemStack,
) -> ItemInteraction {
    if stack.component(components::DYED_COLOR).is_none() {
        return ItemInteraction::TryWithEmptyHand;
    }
    stack.patch_mut().unset(components::DYED_COLOR);
    if ctx.infinite {
        // The stack itself changes, also in creative.
        level.player_effect(ctx.player, PlayerEffect::SetHand(stack.clone()));
    }
    level.award_stat(ctx.player, Stat::custom_named("minecraft:clean_armor"), 1);
    lower_fill_level(level, state, pos);
    ItemInteraction::Done(Interaction::Success)
}

/// `CauldronInteractions.bannerInteraction`: water washes the last pattern off a banner.
fn banner(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    ctx: &UseContext,
    stack: &mut ItemStack,
) -> ItemInteraction {
    let Some(rest) = stack
        .component(components::BANNER_PATTERNS)
        .and_then(remove_last_layer)
    else {
        return ItemInteraction::TryWithEmptyHand;
    };
    let mut cleaned = stack.with_count(1);
    match rest {
        // The banner's default: no layers.
        None => cleaned.patch_mut().unset(components::BANNER_PATTERNS),
        Some(bytes) => cleaned.patch_mut().set(components::BANNER_PATTERNS, bytes),
    }
    filled_result(level, ctx, stack, cleaned, false);
    level.award_stat(ctx.player, Stat::custom_named("minecraft:clean_banner"), 1);
    lower_fill_level(level, state, pos);
    ItemInteraction::Done(Interaction::Success)
}

/// `BannerPatternLayers.removeLast` of the wire value: `None` without layers, `Some(None)`
/// when no layer is left, else the remaining layers.
fn remove_last_layer(value: &[u8]) -> Option<Option<Box<[u8]>>> {
    use vanillite_protocol::codec::Reader;
    let mut r = Reader::new(value);
    let count = usize::try_from(r.varint().ok()?).ok()?;
    if count == 0 {
        return None;
    }
    if count == 1 {
        return Some(None);
    }
    let mut out = Vec::new();
    vanillite_protocol::codec::write_varint(&mut out, i32::try_from(count - 1).ok()?);
    for _ in 0..count - 1 {
        let start = value.len() - r.remaining();
        // `Holder<BannerPattern>`: a registry id + 1, or 0 and the pattern inline.
        if r.varint().ok()? == 0 {
            r.string(32767).ok()?;
            r.string(32767).ok()?;
        }
        r.varint().ok()?;
        let end = value.len() - r.remaining();
        out.extend_from_slice(value.get(start..end)?);
    }
    Some(Some(out.into_boxed_slice()))
}

/// `CauldronInteractions.shulkerBoxInteraction`: water washes the colour off a shulker box.
fn shulker_box(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    ctx: &UseContext,
    stack: &mut ItemStack,
) -> ItemInteraction {
    let mut cleaned = stack.with_count(1);
    cleaned.item = items::SHULKER_BOX.0;
    filled_result(level, ctx, stack, cleaned, false);
    level.award_stat(
        ctx.player,
        Stat::custom_named("minecraft:clean_shulker_box"),
        1,
    );
    lower_fill_level(level, state, pos);
    ItemInteraction::Done(Interaction::Success)
}

/// `LayeredCauldronBlock.lowerFillLevel`.
pub fn lower_fill_level(level: &mut Level, state: BlockState, pos: BlockPos) {
    let new_level = state.int("level").unwrap_or(1) - 1;
    let new_state = if new_level == 0 {
        ids::CAULDRON.default_state()
    } else {
        state.set_int("level", new_level)
    };
    level.set_block_and_update(pos, new_state);
    level.game_event_block(
        events().block_change,
        pos,
        GameEventContext {
            source: None,
            affected: Some(new_state),
            owner: None,
        },
    );
}

/// `LayeredCauldronBlock.entityInside` for players: a burning player takes a level of water
/// before the fire goes out ([`burning_inside`]).
fn entity_inside(level: &mut Level, _: BlockState, pos: BlockPos, entity: EntityId) {
    if level
        .entities
        .player(entity)
        .is_some_and(|p| p.survival.fire_ticks > 0)
    {
        burning_inside(level, pos);
    }
}

/// `LayeredCauldronBlock.handleEntityOnFireInside`: a burning entity takes a level of water (a
/// powder snow cauldron melts into a water cauldron one level lower). The extinguishing itself
/// is the entity's ([`crate::survival::blocks::inside_blocks`]).
pub fn burning_inside(level: &mut Level, pos: BlockPos) {
    let state = level.block(pos);
    let state = if state.is(ids::POWDER_SNOW_CAULDRON) {
        ids::WATER_CAULDRON
            .default_state()
            .set_int("level", state.int("level").unwrap_or(1))
    } else if state.is(ids::WATER_CAULDRON) {
        state
    } else {
        return;
    };
    lower_fill_level(level, state, pos);
}

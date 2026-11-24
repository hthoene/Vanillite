//! Growing plants and soils: the random ticks of crops, stems, sugar cane, cactus, bamboo,
//! growing plant heads (kelp, vines), cocoa, nether wart, sweet berries, saplings, farmland
//! and budding amethyst (`CropBlock`, `StemBlock`, `SugarCaneBlock`, …).

use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Direction};
use vanillite_data::block::{BlockId, BlockState, ids};
use vanillite_worldgen::behaviour::{Class, HORIZONTAL, PosExt, StateExt, class_of};

use super::{block_tags, fluid};
use crate::level::{Level, update};

/// `setBlock` flags of growth that does not update neighbours' shapes: `UPDATE_INVISIBLE |
/// UPDATE_SKIP_BLOCK_ENTITY_SIDEEFFECTS` (260).
const QUIET: u32 = update::INVISIBLE | update::SKIP_BLOCK_ENTITY_SIDEEFFECTS;

/// `pos.below(n)`.
fn below_n(pos: BlockPos, n: i32) -> BlockPos {
    pos.offset(0, -n, 0)
}

/// The highest age of a crop (`getMaxAge`).
pub(super) fn crop_max_age(block: BlockId) -> i32 {
    if block == ids::BEETROOTS {
        3
    } else if block == ids::TORCHFLOWER_CROP {
        2
    } else {
        7
    }
}

/// `CropBlock.getStateForAge`: the torchflower crop becomes a torchflower.
pub(super) fn crop_state_for_age(block: BlockId, age: i32) -> BlockState {
    if block == ids::TORCHFLOWER_CROP && age == 2 {
        ids::TORCHFLOWER.default_state()
    } else {
        block.default_state().set_int("age", age)
    }
}

/// `CropBlock.getGrowthSpeed(type, level, pos)`.
fn growth_speed(level: &Level, block: BlockId, pos: BlockPos) -> f32 {
    let mut speed = 1.0_f32;
    let below = pos.below();
    for dx in -1..=1 {
        for dz in -1..=1 {
            let state = level.block(below.offset(dx, 0, dz));
            let mut block_speed = 0.0_f32;
            if block_tags().grows_crops.contains(state) {
                block_speed = 1.0;
                if state.int("moisture").unwrap_or(0) > 0 {
                    block_speed = 3.0;
                }
            }
            if dx != 0 || dz != 0 {
                block_speed /= 4.0;
            }
            speed += block_speed;
        }
    }
    let is = |p: BlockPos| level.block(p).is(block);
    let (north, south) = (pos.offset(0, 0, -1), pos.offset(0, 0, 1));
    let (west, east) = (pos.offset(-1, 0, 0), pos.offset(1, 0, 0));
    let horizontal = is(west) || is(east);
    let vertical = is(north) || is(south);
    let diagonal = || {
        is(west.offset(0, 0, -1))
            || is(east.offset(0, 0, -1))
            || is(east.offset(0, 0, 1))
            || is(west.offset(0, 0, 1))
    };
    if horizontal && vertical || diagonal() {
        speed /= 2.0;
    }
    speed
}

/// `random.nextInt((int)(25.0F / growthSpeed) + 1) == 0`.
fn grows(level: &mut Level, speed: f32) -> bool {
    let bound = (25.0_f32 / speed) as i32 + 1;
    level.random().next_int_bounded(bound) == 0
}

/// `CropBlock.randomTick` (with the extra one-in-three gate of beetroots and torchflowers).
pub(super) fn crop_random_tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    let block = state.block();
    if (block == ids::BEETROOTS || block == ids::TORCHFLOWER_CROP)
        && level.random().next_int_bounded(3) == 0
    {
        return;
    }
    if level.raw_brightness(pos) < 9 {
        return;
    }
    let age = state.int("age").unwrap_or(0);
    if age < crop_max_age(block) {
        let speed = growth_speed(level, block, pos);
        if grows(level, speed) {
            level.set_block(pos, crop_state_for_age(block, age + 1), update::CLIENTS);
        }
    }
}

/// `StemBlock.randomTick`: grows, then places its fruit next to it.
pub(super) fn stem_random_tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    if level.raw_brightness(pos) < 9 {
        return;
    }
    let block = state.block();
    let speed = growth_speed(level, block, pos);
    if !grows(level, speed) {
        return;
    }
    let age = state.int("age").unwrap_or(0);
    if age < 7 {
        level.set_block(pos, state.set_int("age", age + 1), update::CLIENTS);
        return;
    }
    let direction = HORIZONTAL[level.random().next_int_bounded(4) as usize];
    let target = pos.relative(direction);
    let below = level.block(target.below());
    let (fruit, attached, support) = if block == ids::PUMPKIN_STEM {
        (
            ids::PUMPKIN,
            ids::ATTACHED_PUMPKIN_STEM,
            &block_tags().supports_pumpkin_stem_fruit,
        )
    } else {
        (
            ids::MELON,
            ids::ATTACHED_MELON_STEM,
            &block_tags().supports_melon_stem_fruit,
        )
    };
    if level.block(target).is_air() && support.contains(below) {
        level.set_block_and_update(target, fruit.default_state());
        level.set_block_and_update(
            pos,
            attached.default_state().set_direction("facing", direction),
        );
    }
}

/// `SugarCaneBlock.randomTick`: up to three blocks tall.
pub(super) fn sugar_cane_random_tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    if !level.block(pos.above()).is_air() {
        return;
    }
    let mut height = 1;
    while level.block(below_n(pos, height)).is(state.block()) {
        height += 1;
    }
    if height < 3 {
        let age = state.int("age").unwrap_or(0);
        if age == 15 {
            level.set_block_and_update(pos.above(), state.block().default_state());
            level.set_block(pos, state.set_int("age", 0), QUIET);
        } else {
            level.set_block(pos, state.set_int("age", age + 1), QUIET);
        }
    }
}

/// `CactusBlock.randomTick`: up to three blocks tall, sometimes a flower on top.
pub(super) fn cactus_random_tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    let above = pos.above();
    if !level.block(above).is_air() {
        return;
    }
    let mut height = 1;
    let age = state.int("age").unwrap_or(0);
    while level.block(below_n(pos, height)).is(state.block()) {
        height += 1;
        if height == 3 && age == 15 {
            return;
        }
    }
    if age == 8 && level.can_survive(state.block().default_state(), above) {
        let chance = if height >= 3 { 0.25 } else { 0.1 };
        if level.random().next_double() <= chance {
            level.set_block_and_update(above, ids::CACTUS_FLOWER.default_state());
        }
    } else if age == 15 && height < 3 {
        level.set_block_and_update(above, state.block().default_state());
        let reset = state.set_int("age", 0);
        level.set_block(pos, reset, QUIET);
        level.neighbor_changed(above, state.block());
    }
    if age < 15 {
        level.set_block(pos, state.set_int("age", age + 1), QUIET);
    }
}

/// `BambooSaplingBlock.randomTick`: grows the first stalk.
pub(super) fn bamboo_sapling_random_tick(level: &mut Level, _state: BlockState, pos: BlockPos) {
    if level.random().next_int_bounded(3) == 0
        && level.block(pos.above()).is_air()
        && level.raw_brightness(pos.above()) >= 9
    {
        level.set_block_and_update(
            pos.above(),
            ids::BAMBOO.default_state().set("leaves", "small"),
        );
    }
}

/// `BambooStalkBlock.randomTick`: grows up to 16 blocks.
pub(super) fn bamboo_random_tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    if state.int("stage") != Some(0) {
        return;
    }
    if level.random().next_int_bounded(3) != 0
        || !level.block(pos.above()).is_air()
        || level.raw_brightness(pos.above()) < 9
    {
        return;
    }
    let mut below = 0;
    while below < 16 && level.block(below_n(pos, below + 1)).is(ids::BAMBOO) {
        below += 1;
    }
    let height = below + 1;
    if height >= 16 {
        return;
    }
    // `growBamboo`.
    let below_state = level.block(pos.below());
    let two_below_pos = below_n(pos, 2);
    let two_below = level.block(two_below_pos);
    let mut leaves = "none";
    if height >= 1 {
        if !below_state.is(ids::BAMBOO) || below_state.value("leaves") == Some("none") {
            leaves = "small";
        } else {
            leaves = "large";
            if two_below.is(ids::BAMBOO) {
                level.set_block_and_update(pos.below(), below_state.set("leaves", "small"));
                level.set_block_and_update(two_below_pos, two_below.set("leaves", "none"));
            }
        }
    }
    let age = if state.int("age") != Some(1) && !two_below.is(ids::BAMBOO) {
        0
    } else {
        1
    };
    let stage = if (height < 11 || level.random().next_float() >= 0.25) && height != 15 {
        0
    } else {
        1
    };
    level.set_block_and_update(
        pos.above(),
        ids::BAMBOO
            .default_state()
            .set_int("age", age)
            .set("leaves", leaves)
            .set_int("stage", stage),
    );
}

/// `GrowingPlantHeadBlock.randomTick` of kelp, weeping, twisting and cave vines.
pub(super) fn growing_plant_head_random_tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    let (direction, chance) = match class_of(state) {
        Class::Kelp => (Direction::Up, 0.14),
        Class::TwistingVines => (Direction::Up, 0.1),
        _ => (Direction::Down, 0.1),
    };
    let age = state.int("age").unwrap_or(25);
    if age >= 25 || level.random().next_double() >= chance {
        return;
    }
    let target = pos.relative(direction);
    let target_state = level.block(target);
    let can_grow = match class_of(state) {
        Class::Kelp => target_state.is(ids::WATER),
        _ => target_state.is_air(),
    };
    if !can_grow {
        return;
    }
    // `getGrowIntoState`: the age cycles; cave vines may get berries.
    let mut grown = state.set_int("age", (age + 1) % 26);
    if class_of(state) == Class::CaveVines {
        let berries = level.random().next_float() < 0.11;
        grown = grown.set_flag("berries", berries);
    }
    level.set_block_and_update(target, grown);
}

/// `CocoaBlock.randomTick`.
pub(super) fn cocoa_random_tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    if level.random().next_int_bounded(5) == 0 {
        let age = state.int("age").unwrap_or(2);
        if age < 2 {
            level.set_block(pos, state.set_int("age", age + 1), update::CLIENTS);
        }
    }
}

/// `NetherWartBlock.randomTick`.
pub(super) fn nether_wart_random_tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    let age = state.int("age").unwrap_or(3);
    if age < 3 && level.random().next_int_bounded(10) == 0 {
        level.set_block(pos, state.set_int("age", age + 1), update::CLIENTS);
    }
}

/// `SweetBerryBushBlock.randomTick`.
pub(super) fn sweet_berry_random_tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    let age = state.int("age").unwrap_or(3);
    if age < 3 && level.random().next_int_bounded(5) == 0 && level.raw_brightness(pos.above()) >= 9
    {
        level.set_block(pos, state.set_int("age", age + 1), update::CLIENTS);
    }
}

/// Whether a block is a sapling (`SaplingBlock`).
pub(super) fn is_sapling(block: BlockId) -> bool {
    block != ids::MANGROVE_PROPAGULE
        && block_tags().saplings.contains(block.default_state())
        && block.default_state().int("stage").is_some()
}

/// `SaplingBlock.randomTick`: the sapling advances to stage 1. Growing into a tree needs the
/// tree features on the live world, which are not connected yet; a sapling at stage 1 stays.
pub(super) fn sapling_random_tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    if level.max_local_raw_brightness(pos.above()) >= 9
        && level.random().next_int_bounded(7) == 0
        && state.int("stage") == Some(0)
    {
        level.set_block(pos, state.set_int("stage", 1), QUIET);
    }
}

/// `FarmlandBlock.tick`: turns to dirt if it cannot stay.
pub(super) fn farmland_tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    if !level.can_survive(state, pos) {
        level.set_block_and_update(pos, ids::DIRT.default_state());
    }
}

/// `FarmlandBlock.randomTick`: moisture from water within 4 blocks, dries out to dirt.
pub(super) fn farmland_random_tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    let moisture = state.int("moisture").unwrap_or(0);
    // `isRainingAt(pos.above())`: there is no weather yet.
    if !near_water(level, pos) {
        if moisture > 0 {
            level.set_block(
                pos,
                state.set_int("moisture", moisture - 1),
                update::CLIENTS,
            );
        } else if !block_tags()
            .maintains_farmland
            .contains(level.block(pos.above()))
        {
            level.set_block_and_update(pos, ids::DIRT.default_state());
        }
    } else if moisture < 7 {
        level.set_block(pos, state.set_int("moisture", 7), update::CLIENTS);
    }
}

/// `FarmlandBlock.isNearWater`: water from 4 blocks around up to one block above.
fn near_water(level: &Level, pos: BlockPos) -> bool {
    for y in 0..=1 {
        for z in -4..=4 {
            for x in -4..=4 {
                if fluid::is_water(level.fluid(pos.offset(x, y, z))) {
                    return true;
                }
            }
        }
    }
    false
}

/// `DirtPathBlock.tick`: becomes dirt.
pub(super) fn path_tick(level: &mut Level, _state: BlockState, pos: BlockPos) {
    level.set_block_and_update(pos, ids::DIRT.default_state());
}

/// `BuddingAmethystBlock.randomTick`: buds grow on its faces.
pub(super) fn budding_amethyst_random_tick(level: &mut Level, _state: BlockState, pos: BlockPos) {
    if level.random().next_int_bounded(5) != 0 {
        return;
    }
    let direction = Direction::ALL[level.random().next_int_bounded(6) as usize];
    let target = pos.relative(direction);
    let current = level.block(target);
    let facing_here = current.direction("facing") == Some(direction);
    let next =
        if current.is_air() || current.is(ids::WATER) && current.fluid_state().info().amount == 8 {
            Some(ids::SMALL_AMETHYST_BUD)
        } else if current.is(ids::SMALL_AMETHYST_BUD) && facing_here {
            Some(ids::MEDIUM_AMETHYST_BUD)
        } else if current.is(ids::MEDIUM_AMETHYST_BUD) && facing_here {
            Some(ids::LARGE_AMETHYST_BUD)
        } else if current.is(ids::LARGE_AMETHYST_BUD) && facing_here {
            Some(ids::AMETHYST_CLUSTER)
        } else {
            None
        };
    if let Some(next) = next {
        let waterlogged = current.fluid_state().info().fluid == fluid::WATER;
        let state = next
            .default_state()
            .set_direction("facing", direction)
            .set_flag("waterlogged", waterlogged);
        level.set_block_and_update(target, state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crop_ages() {
        assert_eq!(crop_max_age(ids::WHEAT), 7);
        assert_eq!(
            crop_state_for_age(ids::TORCHFLOWER_CROP, 2),
            ids::TORCHFLOWER.default_state()
        );
        assert_eq!(crop_state_for_age(ids::WHEAT, 3).int("age"), Some(3));
    }
}

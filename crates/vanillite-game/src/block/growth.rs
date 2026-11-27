//! Growing plants and soils: the random ticks of crops, stems, sugar cane, cactus, bamboo,
//! growing plant heads (kelp, vines), cocoa, nether wart, sweet berries, saplings, farmland
//! and budding amethyst (`CropBlock`, `StemBlock`, `SugarCaneBlock`, …).

use vanillite_abi::event::{BLOCK_FADE, BLOCK_GROW, BLOCK_SPREAD};
use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Direction};
use vanillite_data::block::{BlockId, BlockState, ids};
use vanillite_worldgen::behaviour::{
    Class, HORIZONTAL, PosExt, StateExt, can_attach_multiface, class_of, clockwise,
    counter_clockwise, is_horizontal,
};

use super::hook_events as he;
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
            let grown = crop_state_for_age(block, age + 1);
            he::set_block(level, BLOCK_GROW, pos, grown, update::CLIENTS);
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
        he::set_block(
            level,
            BLOCK_GROW,
            pos,
            state.set_int("age", age + 1),
            update::CLIENTS,
        );
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
    if level.block(target).is_air()
        && support.contains(below)
        && he::allows(level, BLOCK_GROW, target, fruit.default_state())
    {
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
            let grown = state.block().default_state();
            if he::allows(level, BLOCK_GROW, pos.above(), grown) {
                level.set_block_and_update(pos.above(), grown);
                level.set_block(pos, state.set_int("age", 0), QUIET);
            }
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
            let flower = ids::CACTUS_FLOWER.default_state();
            he::set_block_and_update(level, BLOCK_GROW, above, flower);
        }
    } else if age == 15 && height < 3 {
        let grown = state.block().default_state();
        if !he::allows(level, BLOCK_GROW, above, grown) {
            return;
        }
        level.set_block_and_update(above, grown);
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
        he::set_block_and_update(
            level,
            BLOCK_SPREAD,
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
    he::set_block_and_update(
        level,
        BLOCK_SPREAD,
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
    he::set_block_and_update(level, BLOCK_SPREAD, target, grown);
}

/// `CocoaBlock.randomTick`.
pub(super) fn cocoa_random_tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    if level.random().next_int_bounded(5) == 0 {
        let age = state.int("age").unwrap_or(2);
        if age < 2 {
            let grown = state.set_int("age", age + 1);
            he::set_block(level, BLOCK_GROW, pos, grown, update::CLIENTS);
        }
    }
}

/// `NetherWartBlock.randomTick`.
pub(super) fn nether_wart_random_tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    let age = state.int("age").unwrap_or(3);
    if age < 3 && level.random().next_int_bounded(10) == 0 {
        let grown = state.set_int("age", age + 1);
        he::set_block(level, BLOCK_GROW, pos, grown, update::CLIENTS);
    }
}

/// `SweetBerryBushBlock.randomTick`.
pub(super) fn sweet_berry_random_tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    let age = state.int("age").unwrap_or(3);
    if age < 3 && level.random().next_int_bounded(5) == 0 && level.raw_brightness(pos.above()) >= 9
    {
        let grown = state.set_int("age", age + 1);
        he::set_block(level, BLOCK_GROW, pos, grown, update::CLIENTS);
    }
}

/// Whether a block is a sapling (`SaplingBlock`).
pub(super) fn is_sapling(block: BlockId) -> bool {
    block != ids::MANGROVE_PROPAGULE
        && block_tags().saplings.contains(block.default_state())
        && block.default_state().int("stage").is_some()
}

/// `SaplingBlock.randomTick` and `advanceTree`: the sapling advances to stage 1, then grows
/// into a tree.
pub(super) fn sapling_random_tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    if level.max_local_raw_brightness(pos.above()) >= 9 && level.random().next_int_bounded(7) == 0 {
        if state.int("stage") == Some(0) {
            he::set_block(level, BLOCK_GROW, pos, state.set_int("stage", 1), QUIET);
        } else {
            super::tree::grow_tree(level, pos, state);
        }
    }
}

/// `MangrovePropaguleBlock.randomTick`: a standing propagule grows like a sapling (without the
/// light check); a hanging one ages up to 4.
pub(super) fn propagule_random_tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    if !state.flag("hanging") {
        if level.random().next_int_bounded(7) == 0 {
            if state.int("stage") == Some(0) {
                he::set_block(level, BLOCK_GROW, pos, state.set_int("stage", 1), QUIET);
            } else {
                super::tree::grow_tree(level, pos, state);
            }
        }
    } else if let Some(age) = state.int("age")
        && age != 4
    {
        let grown = state.set_int("age", age + 1);
        he::set_block(level, BLOCK_GROW, pos, grown, update::CLIENTS);
    }
}

/// `FarmlandBlock.tick`: turns to dirt if it cannot stay.
pub(super) fn farmland_tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    if !level.can_survive(state, pos) {
        he::set_block_and_update(level, BLOCK_FADE, pos, ids::DIRT.default_state());
    }
}

/// `FarmlandBlock.randomTick`: moisture from water within 4 blocks, dries out to dirt.
pub(super) fn farmland_random_tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    let moisture = state.int("moisture").unwrap_or(0);
    if !near_water(level, pos) && !level.is_raining_at(pos.above()) {
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
            he::set_block_and_update(level, BLOCK_FADE, pos, ids::DIRT.default_state());
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
        he::set_block_and_update(level, BLOCK_SPREAD, target, state);
    }
}

/// `LevelEvent.SOUND_CHORUS_GROW`.
const CHORUS_GROW: i32 = 1033;
/// `LevelEvent.SOUND_CHORUS_DEATH`.
const CHORUS_DEATH: i32 = 1034;

/// `ChorusPlantBlock.getStateWithConnections` of a chorus plant at `pos`.
fn chorus_plant_state(level: &Level, pos: BlockPos) -> BlockState {
    let connects = |p: BlockPos| {
        let s = level.block(p);
        s.is(ids::CHORUS_PLANT) || s.is(ids::CHORUS_FLOWER)
    };
    let below = level.block(pos.below());
    let down = below.is(ids::CHORUS_PLANT)
        || below.is(ids::CHORUS_FLOWER)
        || block_tags().supports_chorus_plant.contains(below);
    ids::CHORUS_PLANT
        .default_state()
        .set_flag("down", down)
        .set_flag("up", connects(pos.above()))
        .set_flag("north", connects(pos.relative(Direction::North)))
        .set_flag("east", connects(pos.relative(Direction::East)))
        .set_flag("south", connects(pos.relative(Direction::South)))
        .set_flag("west", connects(pos.relative(Direction::West)))
}

/// `ChorusFlowerBlock.allNeighborsEmpty`.
fn chorus_neighbours_empty(level: &Level, pos: BlockPos, ignore: Option<Direction>) -> bool {
    HORIZONTAL
        .into_iter()
        .all(|d| Some(d) == ignore || level.block(pos.relative(d)).is_air())
}

/// `ChorusFlowerBlock.placeGrownFlower`.
fn place_grown_flower(level: &mut Level, pos: BlockPos, age: i32) {
    let flower = ids::CHORUS_FLOWER.default_state().set_int("age", age);
    level.set_block(pos, flower, update::CLIENTS);
    level.level_event(None, CHORUS_GROW, pos, 0);
}

/// Whether plugins let a chorus flower of `age` spread to `pos` (`BLOCK_SPREAD`).
fn flower_may_spread(level: &Level, pos: BlockPos, age: i32) -> bool {
    let flower = ids::CHORUS_FLOWER.default_state().set_int("age", age);
    he::allows(level, BLOCK_SPREAD, pos, flower)
}

/// `ChorusFlowerBlock.placeDeadFlower`; a plugin may cancel it (`BLOCK_GROW`).
fn place_dead_flower(level: &mut Level, pos: BlockPos) {
    let flower = ids::CHORUS_FLOWER.default_state().set_int("age", 5);
    if !he::allows(level, BLOCK_GROW, pos, flower) {
        return;
    }
    level.set_block(pos, flower, update::CLIENTS);
    level.level_event(None, CHORUS_DEATH, pos, 0);
}

/// `ChorusFlowerBlock.randomTick`: grows up, branches out or dies.
pub(super) fn chorus_flower_random_tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    let above = pos.above();
    if !level.block(above).is_air() || above.y > level.max_y() {
        return;
    }
    let age = state.int("age").unwrap_or(5);
    if age >= 5 {
        return;
    }
    let mut grow_upwards = false;
    let mut pillar_on_support = false;
    let below = level.block(pos.below());
    if block_tags().supports_chorus_flower.contains(below) {
        grow_upwards = true;
    } else if below.is(ids::CHORUS_PLANT) {
        let mut height = 1;
        for _ in 0..4 {
            let test = level.block(below_n(pos, height + 1));
            if !test.is(ids::CHORUS_PLANT) {
                if block_tags().supports_chorus_flower.contains(test) {
                    pillar_on_support = true;
                }
                break;
            }
            height += 1;
        }
        if height < 2
            || height
                <= level
                    .random()
                    .next_int_bounded(if pillar_on_support { 5 } else { 4 })
        {
            grow_upwards = true;
        }
    } else if below.is_air() {
        grow_upwards = true;
    }
    if grow_upwards
        && chorus_neighbours_empty(level, above, None)
        && level.block(below_n(pos, -2)).is_air()
    {
        if flower_may_spread(level, above, age) {
            let plant = chorus_plant_state(level, pos);
            level.set_block(pos, plant, update::CLIENTS);
            place_grown_flower(level, above, age);
        }
    } else if age < 4 {
        let mut attempts = level.random().next_int_bounded(4);
        if pillar_on_support {
            attempts += 1;
        }
        let mut branched = false;
        for _ in 0..attempts {
            let direction = HORIZONTAL[level.random().next_int_bounded(4) as usize];
            let target = pos.relative(direction);
            if level.block(target).is_air()
                && level.block(target.below()).is_air()
                && chorus_neighbours_empty(level, target, Some(direction.opposite()))
                && flower_may_spread(level, target, age + 1)
            {
                place_grown_flower(level, target, age + 1);
                branched = true;
            }
        }
        if branched {
            let plant = chorus_plant_state(level, pos);
            level.set_block(pos, plant, update::CLIENTS);
        } else {
            place_dead_flower(level, pos);
        }
    } else {
        place_dead_flower(level, pos);
    }
}

/// The vine face property of a horizontal direction.
fn vine_face(direction: Direction) -> &'static str {
    match direction {
        Direction::North => "north",
        Direction::South => "south",
        Direction::West => "west",
        Direction::East => "east",
        Direction::Up => "up",
        Direction::Down => "down",
    }
}

/// `VineBlock.isAcceptableNeighbour`.
fn vine_acceptable(level: &Level, pos: BlockPos, direction: Direction) -> bool {
    can_attach_multiface(level.block(pos), direction)
}

/// `VineBlock.canSupportAtFace`.
fn vine_can_support_at_face(level: &Level, pos: BlockPos, direction: Direction) -> bool {
    if direction == Direction::Down {
        return false;
    }
    if vine_acceptable(level, pos.relative(direction), direction) {
        return true;
    }
    if direction == Direction::Up {
        return false;
    }
    let above = level.block(pos.above());
    above.is(ids::VINE) && above.flag(vine_face(direction))
}

/// `VineBlock.hasHorizontalConnection`.
fn vine_has_horizontal(state: BlockState) -> bool {
    HORIZONTAL.into_iter().any(|d| state.flag(vine_face(d)))
}

/// `VineBlock.canSpread`: at most four vines within 4 blocks around and 1 above or below.
fn vine_can_spread(level: &Level, pos: BlockPos) -> bool {
    let mut count = 0;
    for y in -1..=1 {
        for z in -4..=4 {
            for x in -4..=4 {
                if level.block(pos.offset(x, y, z)).is(ids::VINE) {
                    count += 1;
                    if count > 4 {
                        return false;
                    }
                }
            }
        }
    }
    true
}

/// `VineBlock.copyRandomFaces`.
fn vine_copy_random_faces(level: &mut Level, from: BlockState, mut to: BlockState) -> BlockState {
    for direction in HORIZONTAL {
        if level.random().next_boolean() {
            let face = vine_face(direction);
            if from.flag(face) {
                to = to.set_flag(face, true);
            }
        }
    }
    to
}

/// `VineBlock.randomTick`: vines spread sideways, up and down.
pub(super) fn vine_random_tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    if !level.settings.spread_vines || level.random().next_int_bounded(4) != 0 {
        return;
    }
    let test = Direction::ALL[level.random().next_int_bounded(6) as usize];
    let above_pos = pos.above();
    let vine = ids::VINE.default_state();
    if is_horizontal(test) && !state.flag(vine_face(test)) {
        if !vine_can_spread(level, pos) {
            return;
        }
        let test_pos = pos.relative(test);
        let edge = level.block(test_pos);
        if edge.is_air() {
            let cw = clockwise(test);
            let ccw = counter_clockwise(test);
            let cw_face = state.flag(vine_face(cw));
            let ccw_face = state.flag(vine_face(ccw));
            let cw_pos = test_pos.relative(cw);
            let ccw_pos = test_pos.relative(ccw);
            if cw_face && vine_acceptable(level, cw_pos, cw) {
                he::set_block(
                    level,
                    BLOCK_SPREAD,
                    test_pos,
                    vine.set_flag(vine_face(cw), true),
                    update::CLIENTS,
                );
            } else if ccw_face && vine_acceptable(level, ccw_pos, ccw) {
                he::set_block(
                    level,
                    BLOCK_SPREAD,
                    test_pos,
                    vine.set_flag(vine_face(ccw), true),
                    update::CLIENTS,
                );
            } else {
                let opposite = test.opposite();
                if cw_face
                    && level.block(cw_pos).is_air()
                    && vine_acceptable(level, pos.relative(cw), opposite)
                {
                    let grown = vine.set_flag(vine_face(opposite), true);
                    he::set_block(level, BLOCK_SPREAD, cw_pos, grown, update::CLIENTS);
                } else if ccw_face
                    && level.block(ccw_pos).is_air()
                    && vine_acceptable(level, pos.relative(ccw), opposite)
                {
                    let grown = vine.set_flag(vine_face(opposite), true);
                    he::set_block(level, BLOCK_SPREAD, ccw_pos, grown, update::CLIENTS);
                } else if f64::from(level.random().next_float()) < 0.05
                    && vine_acceptable(level, test_pos.above(), Direction::Up)
                {
                    let grown = vine.set_flag("up", true);
                    he::set_block(level, BLOCK_SPREAD, test_pos, grown, update::CLIENTS);
                }
            }
        } else if vine_acceptable(level, test_pos, test) {
            let grown = state.set_flag(vine_face(test), true);
            he::set_block(level, BLOCK_GROW, pos, grown, update::CLIENTS);
        }
        return;
    }
    if test == Direction::Up && pos.y < level.max_y() {
        if vine_can_support_at_face(level, pos, test) {
            let grown = state.set_flag("up", true);
            he::set_block(level, BLOCK_GROW, pos, grown, update::CLIENTS);
            return;
        }
        if level.block(above_pos).is_air() {
            if !vine_can_spread(level, pos) {
                return;
            }
            let mut above_state = state;
            for direction in HORIZONTAL {
                if level.random().next_boolean()
                    || !vine_acceptable(level, above_pos.relative(direction), direction)
                {
                    above_state = above_state.set_flag(vine_face(direction), false);
                }
            }
            if vine_has_horizontal(above_state) {
                he::set_block(level, BLOCK_SPREAD, above_pos, above_state, update::CLIENTS);
            }
            return;
        }
    }
    if pos.y > level.min_y() {
        let below_pos = pos.below();
        let below = level.block(below_pos);
        if below.is_air() || below.is(ids::VINE) {
            let before = if below.is_air() { vine } else { below };
            let after = vine_copy_random_faces(level, state, before);
            if before != after && vine_has_horizontal(after) {
                he::set_block(level, BLOCK_SPREAD, below_pos, after, update::CLIENTS);
            }
        }
    }
}

/// `PitcherCropBlock.randomTick`: the lower half grows like a crop, from age 3 two blocks tall.
pub(super) fn pitcher_crop_random_tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    let speed = growth_speed(level, state.block(), pos);
    if !grows(level, speed) {
        return;
    }
    // `grow(level, state, pos, 1)`.
    let age = state.int("age").unwrap_or(4);
    let new_age = (age + 1).min(4);
    let double = new_age >= 3;
    let can_grow = age < 4
        && level.raw_brightness(pos) >= 8
        && level.in_height(pos.y + 1)
        && (!double || {
            let above = level.block(pos.above());
            above.is_air() || above.is(ids::PITCHER_CROP)
        });
    if can_grow {
        let lower = state.set_int("age", new_age);
        if !he::allows(level, BLOCK_GROW, pos, lower) {
            return;
        }
        level.set_block(pos, lower, update::CLIENTS);
        if double {
            level.set_block_and_update(pos.above(), lower.set("half", "upper"));
        }
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

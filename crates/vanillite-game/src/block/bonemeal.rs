//! Bone meal (`BoneMealItem.growCrop`, `BoneMealItem.growWaterPlant`) and the bone meal
//! behaviour of every `BonemealableBlock`: `isValidBonemealTarget`, `isBonemealSuccess` and
//! `performBonemeal`, dispatched by the block's Vanilla class.
//!
//! Blocks that grow by placing a configured world generation feature at run time (trees,
//! huge mushrooms, huge fungi, moss and nylium vegetation, the flowers of grass blocks) place
//! Vanilla's feature in the live world ([`super::tree`]); without the features (no data
//! packs) they are no target.

use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Direction};
use vanillite_data::block::{BlockId, BlockKind, BlockState, flags, ids};
use vanillite_worldgen::behaviour::{HORIZONTAL, PosExt, StateExt};

use super::{block_tags, growth};
use crate::item::ItemStack;
use crate::level::{Level, update};

/// `BoneMealItem.growCrop`: bone meal on the block at `pos`. Takes one bone meal and returns
/// `true` if the block is a valid target (whether or not it grew).
pub fn grow_crop(level: &mut Level, pos: BlockPos, stack: &mut ItemStack) -> bool {
    let state = level.block(pos);
    if !is_valid_target(level, pos, state) {
        return false;
    }
    if is_success(level, state) {
        perform(level, pos, state, 0);
    }
    stack.shrink(1);
    true
}

/// [`grow_crop`] by a player or (`player` 0) a dispenser, which plugins may cancel
/// (`BLOCK_FERTILIZE`; Bukkit's `BlockFertilizeEvent`): a cancelled use takes no bone meal and
/// returns `false`. Trees and huge plants it grows name the player (`STRUCTURE_GROW`).
pub fn fertilize(level: &mut Level, pos: BlockPos, stack: &mut ItemStack, player: u128) -> bool {
    let state = level.block(pos);
    if !is_valid_target(level, pos, state) {
        return false;
    }
    if let Some(hooks) = level.hooks_for(vanillite_abi::event::BLOCK_FERTILIZE) {
        let mut ev = crate::hooks::Event {
            player,
            pos,
            state: Some(state),
            ..crate::hooks::Event::new(vanillite_abi::event::BLOCK_FERTILIZE)
        };
        hooks.event(&mut ev);
        if ev.cancelled {
            return false;
        }
    }
    if is_success(level, state) {
        perform(level, pos, state, player);
    }
    stack.shrink(1);
    true
}

/// `BoneMealItem.growWaterPlant` without a clicked face: bone meal in a water source grows
/// seagrass (and corals in warm oceans) around it. Takes one bone meal and returns `true` if
/// `pos` is a full water block.
pub fn grow_water_plant(level: &mut Level, pos: BlockPos, stack: &mut ItemStack) -> bool {
    grow_water_plant_from(level, pos, None, stack)
}

/// `BoneMealItem.growWaterPlant` with the clicked face of a player's use: the first attempt
/// against a horizontal face in a warm ocean grows a wall coral on it.
pub fn grow_water_plant_from(
    level: &mut Level,
    pos: BlockPos,
    clicked_face: Option<Direction>,
    stack: &mut ItemStack,
) -> bool {
    if !is_full_water(level, pos) {
        return false;
    }
    'attempts: for j in 0..128 {
        let mut test = pos;
        let mut grow = ids::SEAGRASS.default_state();
        for _ in 0..j / 16 {
            let dx = level.random().next_int_bounded(3) - 1;
            let dy =
                (level.random().next_int_bounded(3) - 1) * level.random().next_int_bounded(3) / 2;
            let dz = level.random().next_int_bounded(3) - 1;
            test = test.offset(dx, dy, dz);
            if full_collision(level.block(test)) {
                continue 'attempts;
            }
        }
        if produces_corals(level, test) {
            let horizontal = clicked_face
                .filter(|f| !matches!(f, Direction::Up | Direction::Down))
                .filter(|_| j == 0);
            if let Some(face) = horizontal {
                let options = WALL_CORALS;
                let index = level.random().next_int_bounded(options.len() as i32) as usize;
                if let Some(&block) = options.get(index) {
                    grow = block.default_state().set_direction("facing", face);
                }
            } else if level.random().next_int_bounded(4) == 0 {
                let options = underwater_bonemeals();
                let index = level.random().next_int_bounded(options.len() as i32) as usize;
                if let Some(&block) = options.get(index) {
                    grow = block.default_state();
                }
            }
        }
        if is_wall_coral(grow.block()) {
            let mut d = 0;
            while !level.can_survive(grow, test) && d < 4 {
                let direction = HORIZONTAL[level.random().next_int_bounded(4) as usize];
                grow = grow.set_direction("facing", direction);
                d += 1;
            }
        }
        if level.can_survive(grow, test) {
            let current = level.block(test);
            if current.is(ids::WATER) && is_full_water(level, test) {
                level.set_block_and_update(test, grow);
            } else if current.is(ids::SEAGRASS)
                && is_valid_target(level, test, current)
                && level.random().next_int_bounded(10) == 0
            {
                perform(level, test, current, 0);
            }
        }
    }
    stack.shrink(1);
    true
}

/// `level.getBlockState(pos).is(Blocks.WATER) && level.getFluidState(pos).isFull()`.
fn is_full_water(level: &Level, pos: BlockPos) -> bool {
    let state = level.block(pos);
    state.is(ids::WATER) && state.fluid_state().info().amount == 8
}

/// `isCollisionShapeFullBlock`.
fn full_collision(state: BlockState) -> bool {
    let shape = state.collision_shape();
    shape.len() == 1
        && shape
            .first()
            .is_some_and(|b| *b == [0.0, 0.0, 0.0, 1.0, 1.0, 1.0])
}

/// `#minecraft:produces_corals_from_bonemeal`: the warm ocean.
fn produces_corals(level: &Level, pos: BlockPos) -> bool {
    let biome = level.biome(pos);
    level
        .chunks
        .info()
        .biomes
        .get(usize::from(biome))
        .is_some_and(|name| name == "minecraft:warm_ocean")
}

/// `#minecraft:underwater_bonemeals` in tag order.
fn underwater_bonemeals() -> [BlockId; 16] {
    [
        ids::SEAGRASS,
        ids::TUBE_CORAL,
        ids::BRAIN_CORAL,
        ids::BUBBLE_CORAL,
        ids::FIRE_CORAL,
        ids::HORN_CORAL,
        ids::TUBE_CORAL_FAN,
        ids::BRAIN_CORAL_FAN,
        ids::BUBBLE_CORAL_FAN,
        ids::FIRE_CORAL_FAN,
        ids::HORN_CORAL_FAN,
        ids::TUBE_CORAL_WALL_FAN,
        ids::BRAIN_CORAL_WALL_FAN,
        ids::BUBBLE_CORAL_WALL_FAN,
        ids::FIRE_CORAL_WALL_FAN,
        ids::HORN_CORAL_WALL_FAN,
    ]
}

/// `#minecraft:wall_corals` in tag order.
const WALL_CORALS: [BlockId; 5] = [
    ids::TUBE_CORAL_WALL_FAN,
    ids::BRAIN_CORAL_WALL_FAN,
    ids::BUBBLE_CORAL_WALL_FAN,
    ids::FIRE_CORAL_WALL_FAN,
    ids::HORN_CORAL_WALL_FAN,
];

/// `#minecraft:wall_corals`.
fn is_wall_coral(block: BlockId) -> bool {
    WALL_CORALS.contains(&block)
}

/// `Mth.nextInt(random, min, max)`.
fn next_int_between(level: &mut Level, min: i32, max: i32) -> i32 {
    if min >= max {
        min
    } else {
        level.random().next_int_bounded(max - min + 1) + min
    }
}

fn kind(state: BlockState) -> BlockKind {
    state.block().info().kind
}

/// `isInsideBuildHeight(pos)`.
fn in_height(level: &Level, pos: BlockPos) -> bool {
    level.in_height(pos.y)
}

/// `isEmptyBlock(pos)`.
fn is_empty(level: &Level, pos: BlockPos) -> bool {
    level.block(pos).is_air()
}

/// `isValidBonemealTarget(level, pos, state, INTERACTION)`.
pub fn is_valid_target(level: &mut Level, pos: BlockPos, state: BlockState) -> bool {
    use BlockKind as K;
    let age = || state.int("age").unwrap_or(0);
    match kind(state) {
        K::CropBlock
        | K::CarrotBlock
        | K::PotatoBlock
        | K::BeetrootBlock
        | K::TorchflowerCropBlock => age() < growth::crop_max_age(state.block()),
        K::StemBlock => age() != 7,
        K::CocoaBlock => age() < 2,
        K::SweetBerryBushBlock => age() < 3,
        K::ShelfMushroomBlock => age() < 1,
        // `SaplingBlock.isValidBonemealTarget`: `canGrow` draws from the level random.
        K::SaplingBlock => {
            super::tree::can_grow(level, pos, state)
                && in_height(
                    level,
                    pos.offset(0, super::tree::minimum_height(state.block()), 0),
                )
        }
        K::MangrovePropaguleBlock => !state.flag("hanging") || age() != 4,
        K::AzaleaBlock => {
            in_height(
                level,
                pos.offset(0, super::tree::minimum_height(state.block()) + 2, 0),
            ) && level.fluid(pos.above()).is_empty()
        }
        K::PitcherCropBlock => {
            pitcher_lower(level, pos, state).is_some_and(|(lower_pos, lower)| {
                pitcher_can_grow(level, lower_pos, lower, lower.int("age").unwrap_or(0) + 1)
            })
        }
        K::BambooStalkBlock => {
            let above = bamboo_height_above(level, pos);
            let below = bamboo_height_below(level, pos);
            let growth_pos = pos.offset(0, above + 1, 0);
            above + below + 1 < 16
                && level.block(pos.offset(0, above, 0)).int("stage") != Some(1)
                && in_height(level, growth_pos)
                && is_empty(level, growth_pos)
        }
        K::BambooSaplingBlock => level.block(pos.above()).is_air() && in_height(level, pos.above()),
        K::KelpBlock | K::WeepingVinesBlock | K::TwistingVinesBlock => {
            let growth_pos = pos.relative(head_direction(state.block()));
            can_grow_into(state.block(), level.block(growth_pos)) && in_height(level, growth_pos)
        }
        K::KelpPlantBlock | K::WeepingVinesPlantBlock | K::TwistingVinesPlantBlock => {
            let Some((head, head_pos)) = growing_plant_head(level, pos, state.block()) else {
                return false;
            };
            let growth_pos = head_pos.relative(head_direction(head));
            can_grow_into(head, level.block(growth_pos)) && in_height(level, growth_pos)
        }
        K::CaveVinesBlock | K::CaveVinesPlantBlock => !state.flag("berries"),
        K::TallGrassBlock => {
            let grown = tall_grass_grown(state.block()).default_state();
            level.can_survive(grown, pos)
                && is_empty(level, pos.above())
                && in_height(level, pos.above())
        }
        K::SeagrassBlock => level.block(pos.above()).is(ids::WATER),
        K::TallFlowerBlock | K::FlowerBedBlock | K::SmallDripleafBlock | K::ShortDryGrassBlock => {
            true
        }
        K::BigDripleafBlock => dripleaf_can_grow_into(level, pos.above()),
        K::BigDripleafStemBlock => {
            top_connected(level, pos, state.block(), Direction::Up, ids::BIG_DRIPLEAF)
                .is_some_and(|head| dripleaf_can_grow_into(level, head.above()))
        }
        K::HangingMossBlock => {
            let grow_pos = hanging_moss_tip(level, pos, state.block()).below();
            level.block(grow_pos).is_air() && in_height(level, grow_pos)
        }
        K::BushBlock | K::FireflyBushBlock => spreadable_neighbour(level, pos, state).is_some(),
        K::TallDryGrassBlock => {
            spreadable_neighbour(level, pos, ids::SHORT_DRY_GRASS.default_state()).is_some()
        }
        K::RootedDirtBlock => level.block(pos.below()).is_air() && in_height(level, pos.below()),
        K::MangroveLeavesBlock => level.block(pos.below()).is_air(),
        K::SeaPickleBlock => {
            // `!isDead(state)`: a sea pickle is alive while waterlogged.
            state.flag("waterlogged")
                && block_tags().coral_blocks.contains(level.block(pos.below()))
        }
        K::MossyCarpetBlock => state.flag("bottom") && mossy_topper(level, pos, state).is_some(),
        K::NetherrackBlock => {
            level
                .block(pos.above())
                .has(flags::PROPAGATES_SKYLIGHT_DOWN)
                && nylium_around(level, pos) != (false, false)
        }
        K::GrassBlock => level.block(pos.above()).is_air() && in_height(level, pos.above()),
        K::NetherFungusBlock => {
            let required = if state.is(ids::CRIMSON_FUNGUS) {
                ids::CRIMSON_NYLIUM
            } else {
                ids::WARPED_NYLIUM
            };
            level.block(pos.below()).is(required)
                && in_height(level, pos.above())
                && level.tree_features().is_some()
        }
        K::MushroomBlock => mushroom_feature(state.block()).is_some_and(|(_, foliage)| {
            level.tree_features().is_some() && in_height(level, pos.offset(0, 4 + foliage, 0))
        }),
        K::BonemealableFeaturePlacerBlock => {
            level.block(pos.above()).is_air() && level.tree_features().is_some()
        }
        K::NyliumBlock => {
            level.block(pos.above()).is_air()
                && in_height(level, pos.above())
                && level.tree_features().is_some()
        }
        K::GlowLichenBlock => Direction::ALL
            .into_iter()
            .any(|face| lichen_can_spread_in_any_direction(level, state, pos, face.opposite())),
        _ => false,
    }
}

/// The huge mushroom feature of a mushroom and its foliage radius.
fn mushroom_feature(block: BlockId) -> Option<(&'static str, i32)> {
    if block == ids::BROWN_MUSHROOM {
        Some(("minecraft:huge_brown_mushroom", 3))
    } else if block == ids::RED_MUSHROOM {
        Some(("minecraft:huge_red_mushroom", 2))
    } else {
        None
    }
}

// --- Glow lichen (`MultifaceSpreader` with `DefaultSpreaderConfig`) ---------------------

fn axis_of(d: Direction) -> u8 {
    match d {
        Direction::Down | Direction::Up => 1,
        Direction::North | Direction::South => 2,
        Direction::West | Direction::East => 0,
    }
}

/// `MultifaceBlock.isValidStateForPlacement` of glow lichen.
fn lichen_valid_for_placement(
    level: &Level,
    old: BlockState,
    pos: BlockPos,
    face: Direction,
) -> bool {
    if old.is(ids::GLOW_LICHEN) && super::sculk_catalyst::has_face(old, face) {
        return false;
    }
    super::sculk_catalyst::can_attach_to(level, pos, face)
}

/// `DefaultSpreaderConfig.canSpreadInto`.
fn lichen_can_spread_into(level: &Level, (pos, face): (BlockPos, Direction)) -> bool {
    let existing = level.block(pos);
    let replaceable = existing.is_air()
        || existing.is(ids::GLOW_LICHEN)
        || existing.is(ids::WATER) && existing.fluid_state().info().source;
    replaceable && lichen_valid_for_placement(level, existing, pos, face)
}

/// `getSpreadFromFaceTowardDirection` with the default spread order.
fn lichen_spread_pos(
    level: &Level,
    state: BlockState,
    pos: BlockPos,
    starting: Direction,
    spread: Direction,
) -> Option<(BlockPos, Direction)> {
    use super::sculk_catalyst::has_face;
    if axis_of(spread) == axis_of(starting) {
        return None;
    }
    if !(has_face(state, starting) && !has_face(state, spread)) {
        return None;
    }
    [
        (pos, spread),
        (pos.relative(spread), starting),
        (pos.relative(spread).relative(starting), spread.opposite()),
    ]
    .into_iter()
    .find(|&target| lichen_can_spread_into(level, target))
}

/// `canSpreadInAnyDirection(state, level, pos, startingFace)`.
fn lichen_can_spread_in_any_direction(
    level: &Level,
    state: BlockState,
    pos: BlockPos,
    starting: Direction,
) -> bool {
    Direction::ALL
        .into_iter()
        .any(|spread| lichen_spread_pos(level, state, pos, starting, spread).is_some())
}

/// `Direction.allShuffled(random)` (`Util.shuffle`).
fn shuffled_directions(level: &mut Level) -> [Direction; 6] {
    let mut d = Direction::ALL;
    for i in (2..=d.len()).rev() {
        #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
        let j = level.random().next_int_bounded(i as i32) as usize;
        d.swap(i - 1, j);
    }
    d
}

/// `spreadToFace` with `placeBlock` (`getStateForPlacement`, `setBlock(pos, state, 2)`).
fn lichen_spread_to_face(level: &mut Level, (pos, face): (BlockPos, Direction)) -> bool {
    let old = level.block(pos);
    if !lichen_valid_for_placement(level, old, pos, face) {
        return false;
    }
    let base = if old.is(ids::GLOW_LICHEN) {
        old
    } else if old.fluid_state().info().source && super::sculk_catalyst::is_water(old) {
        ids::GLOW_LICHEN
            .default_state()
            .set_flag("waterlogged", true)
    } else {
        ids::GLOW_LICHEN.default_state()
    };
    let state = base.set_flag(super::sculk_catalyst::direction_name(face), true);
    level.set_block(pos, state, update::CLIENTS)
}

/// `spreadFromRandomFaceTowardRandomDirection`: the faces the lichen has in random order,
/// each toward the directions in random order, until one spreads.
fn perform_lichen(level: &mut Level, pos: BlockPos, state: BlockState) {
    for face in shuffled_directions(level) {
        if !super::sculk_catalyst::has_face(state, face) {
            continue;
        }
        for spread in shuffled_directions(level) {
            if let Some(target) = lichen_spread_pos(level, state, pos, face, spread)
                && lichen_spread_to_face(level, target)
            {
                return;
            }
        }
    }
}

/// `isBonemealSuccess(level, random, pos, state, INTERACTION)`.
fn is_success(level: &mut Level, state: BlockState) -> bool {
    match kind(state) {
        BlockKind::SaplingBlock | BlockKind::AzaleaBlock => level.random().next_float() < 0.45,
        BlockKind::MangrovePropaguleBlock if !state.flag("hanging") => {
            level.random().next_float() < 0.45
        }
        BlockKind::NetherFungusBlock | BlockKind::MushroomBlock => {
            level.random().next_float() < 0.4
        }
        _ => true,
    }
}

/// `performBonemeal(level, random, pos, state, INTERACTION)`; `player` (0 for none) used the
/// bone meal.
fn perform(level: &mut Level, pos: BlockPos, state: BlockState, player: u128) {
    use BlockKind as K;
    let block = state.block();
    let age = state.int("age").unwrap_or(0);
    match kind(state) {
        K::CropBlock | K::CarrotBlock | K::PotatoBlock | K::BeetrootBlock => {
            let mut increase = next_int_between(level, 2, 5);
            if block == ids::BEETROOTS {
                increase /= 3;
            }
            let age = (age + increase).min(growth::crop_max_age(block));
            level.set_block(pos, growth::crop_state_for_age(block, age), update::CLIENTS);
        }
        K::TorchflowerCropBlock => {
            let age = (age + 1).min(growth::crop_max_age(block));
            level.set_block(pos, growth::crop_state_for_age(block, age), update::CLIENTS);
        }
        K::StemBlock => {
            let age = (age + next_int_between(level, 2, 5)).min(7);
            let grown = state.set_int("age", age);
            level.set_block(pos, grown, update::CLIENTS);
            if age == 7 {
                growth::stem_random_tick(level, grown, pos);
            }
        }
        K::CocoaBlock | K::ShelfMushroomBlock => {
            level.set_block(pos, state.set_int("age", age + 1), update::CLIENTS);
        }
        K::SweetBerryBushBlock => {
            level.set_block(pos, state.set_int("age", (age + 1).min(3)), update::CLIENTS);
        }
        K::SaplingBlock => advance_tree(level, pos, state, player),
        K::MangrovePropaguleBlock => {
            if state.flag("hanging") {
                if age != 4 {
                    level.set_block(pos, state.set_int("age", age + 1), update::CLIENTS);
                }
            } else {
                advance_tree(level, pos, state, player);
            }
        }
        K::AzaleaBlock => {
            super::tree::grow_tree_by(level, pos, state, player);
        }
        K::PitcherCropBlock => {
            if let Some((lower_pos, lower)) = pitcher_lower(level, pos, state) {
                let new_age = (lower.int("age").unwrap_or(0) + 1).min(4);
                if pitcher_can_grow(level, lower_pos, lower, new_age) {
                    let grown = lower.set_int("age", new_age);
                    level.set_block(lower_pos, grown, update::CLIENTS);
                    if pitcher_is_double(new_age) {
                        level.set_block_and_update(lower_pos.above(), grown.set("half", "upper"));
                    }
                }
            }
        }
        K::BambooStalkBlock => perform_bamboo(level, pos),
        K::BambooSaplingBlock => {
            level.set_block_and_update(
                pos.above(),
                ids::BAMBOO.default_state().set("leaves", "small"),
            );
        }
        K::KelpBlock | K::WeepingVinesBlock | K::TwistingVinesBlock => {
            grow_plant_head(level, pos, state);
        }
        K::KelpPlantBlock | K::WeepingVinesPlantBlock | K::TwistingVinesPlantBlock => {
            if let Some((_, head_pos)) = growing_plant_head(level, pos, block) {
                let head = level.block(head_pos);
                grow_plant_head(level, head_pos, head);
            }
        }
        K::CaveVinesBlock | K::CaveVinesPlantBlock => {
            level.set_block(pos, state.set_flag("berries", true), update::CLIENTS);
        }
        K::TallGrassBlock => {
            place_double_plant(
                level,
                tall_grass_grown(block).default_state(),
                pos,
                update::CLIENTS,
            );
        }
        K::SeagrassBlock => {
            let lower = ids::TALL_SEAGRASS.default_state();
            level.set_block(pos, lower, update::CLIENTS);
            level.set_block(pos.above(), lower.set("half", "upper"), update::CLIENTS);
        }
        K::TallFlowerBlock => {
            super::drops::pop_resource(level, pos, item_of(block));
        }
        K::FlowerBedBlock => {
            let amount = state.int("flower_amount").unwrap_or(4);
            if amount < 4 {
                level.set_block(
                    pos,
                    state.set_int("flower_amount", amount + 1),
                    update::CLIENTS,
                );
            } else {
                super::drops::pop_resource(level, pos, item_of(block));
            }
        }
        K::BigDripleafBlock => {
            let above = pos.above();
            if dripleaf_can_grow_into(level, above) {
                let facing = state.direction("facing").unwrap_or(Direction::North);
                let water = is_water_source_state(state);
                place_dripleaf(level, ids::BIG_DRIPLEAF_STEM, pos, water, facing);
                let water = is_water_source(level, above);
                place_dripleaf(level, ids::BIG_DRIPLEAF, above, water, facing);
            }
        }
        K::BigDripleafStemBlock => {
            if let Some(head) = top_connected(level, pos, block, Direction::Up, ids::BIG_DRIPLEAF) {
                let facing = state.direction("facing").unwrap_or(Direction::North);
                let water = is_water_source(level, head);
                place_dripleaf(level, ids::BIG_DRIPLEAF_STEM, head, water, facing);
                let water = is_water_source(level, head.above());
                place_dripleaf(level, ids::BIG_DRIPLEAF, head.above(), water, facing);
            }
        }
        K::SmallDripleafBlock => {
            let (lower_pos, lower) = if state.value("half") == Some("upper") {
                (pos.below(), level.block(pos.below()))
            } else {
                (pos, state)
            };
            if lower.value("half") == Some("lower") || lower_pos == pos {
                let above = lower_pos.above();
                let fluid = level.fluid(above).block_state();
                level.set_block(above, fluid, update::CLIENTS | update::KNOWN_SHAPE);
                let facing = lower.direction("facing").unwrap_or(Direction::North);
                big_dripleaf_with_random_height(level, lower_pos, facing);
            }
        }
        K::HangingMossBlock => {
            let tip = hanging_moss_tip(level, pos, block).below();
            if level.block(tip).is_air() {
                level.set_block_and_update(tip, state.set_flag("tip", true));
            }
        }
        K::BushBlock | K::FireflyBushBlock => {
            if let Some(target) = find_spreadable_neighbour(level, pos, state) {
                level.set_block_and_update(target, block.default_state());
            }
        }
        K::ShortDryGrassBlock => {
            level.set_block_and_update(pos, ids::TALL_DRY_GRASS.default_state());
        }
        K::TallDryGrassBlock => {
            let short = ids::SHORT_DRY_GRASS.default_state();
            if let Some(target) = find_spreadable_neighbour(level, pos, short) {
                level.set_block_and_update(target, short);
            }
        }
        K::RootedDirtBlock => {
            level.set_block_and_update(pos.below(), ids::HANGING_ROOTS.default_state());
        }
        K::MangroveLeavesBlock => {
            let propagule = ids::MANGROVE_PROPAGULE
                .default_state()
                .set_flag("hanging", true)
                .set_int("age", 0);
            level.set_block(pos.below(), propagule, update::CLIENTS);
        }
        K::SeaPickleBlock => perform_sea_pickle(level, pos, state),
        K::MossyCarpetBlock => {
            if let Some(topper) = mossy_topper(level, pos, state) {
                level.set_block_and_update(pos.above(), topper);
            }
        }
        K::NetherrackBlock => {
            let nylium = match nylium_around(level, pos) {
                (true, true) => {
                    if level.random().next_boolean() {
                        Some(ids::WARPED_NYLIUM)
                    } else {
                        Some(ids::CRIMSON_NYLIUM)
                    }
                }
                (true, false) => Some(ids::WARPED_NYLIUM),
                (false, true) => Some(ids::CRIMSON_NYLIUM),
                (false, false) => None,
            };
            if let Some(nylium) = nylium {
                level.set_block_and_update(pos, nylium.default_state());
            }
        }
        K::GrassBlock => perform_grass(level, pos),
        K::NetherFungusBlock => {
            let name = if block == ids::CRIMSON_FUNGUS {
                "minecraft:crimson_fungus_planted"
            } else {
                "minecraft:warped_fungus_planted"
            };
            if super::tree::structure_may_grow(level, pos, state, player) {
                super::tree::place_named(level, name, pos);
            }
        }
        K::MushroomBlock => {
            // `growMushroom`: the mushroom goes, and comes back if the feature fails.
            if let Some((name, _)) = mushroom_feature(block)
                && level.tree_features().is_some()
                && super::tree::structure_may_grow(level, pos, state, player)
            {
                level.remove_block(pos, false);
                if super::tree::place_named(level, name, pos) != Some(true) {
                    level.set_block_and_update(pos, state);
                }
            }
        }
        K::BonemealableFeaturePlacerBlock => {
            let name = if block == ids::PALE_MOSS_BLOCK {
                "minecraft:pale_moss_patch_bonemeal"
            } else {
                "minecraft:moss_patch_bonemeal"
            };
            super::tree::place_named(level, name, pos.above());
        }
        K::NyliumBlock => {
            if in_height(level, pos.above()) {
                super::tree::place_named(level, "minecraft:nylium_bonemeal", pos.above());
            }
        }
        K::GlowLichenBlock => perform_lichen(level, pos, state),
        _ => {}
    }
}

/// `setBlock` flags of `advanceTree`: `UPDATE_INVISIBLE | UPDATE_SKIP_BLOCK_ENTITY_SIDEEFFECTS`.
const QUIET: u32 = update::INVISIBLE | update::SKIP_BLOCK_ENTITY_SIDEEFFECTS;

/// `SaplingBlock.advanceTree`: stage 0 becomes stage 1, then the tree grows.
fn advance_tree(level: &mut Level, pos: BlockPos, state: BlockState, player: u128) {
    if state.int("stage") == Some(0) {
        level.set_block(pos, state.set_int("stage", 1), QUIET);
    } else {
        super::tree::grow_tree_by(level, pos, state, player);
    }
}

/// `new ItemStack(block)`.
fn item_of(block: BlockId) -> ItemStack {
    let item = block.info().item;
    if item == u16::MAX {
        ItemStack::EMPTY
    } else {
        ItemStack::new(item, 1)
    }
}

/// `state.getFluidState().isSourceOfType(Fluids.WATER)`.
fn is_water_source_state(state: BlockState) -> bool {
    let fluid = state.fluid_state();
    fluid.info().source && super::fluid::is_water(fluid)
}

/// `fluidState.isSourceOfType(Fluids.WATER)`.
fn is_water_source(level: &Level, pos: BlockPos) -> bool {
    is_water_source_state(level.block(pos))
}

// --- Pitcher crop ----------------------------------------------------------------------------

/// `PitcherCropBlock.isDouble(age)`.
fn pitcher_is_double(age: i32) -> bool {
    age >= 3
}

/// `PitcherCropBlock.getLowerHalf`.
fn pitcher_lower(
    level: &Level,
    pos: BlockPos,
    state: BlockState,
) -> Option<(BlockPos, BlockState)> {
    if state.value("half") != Some("upper") {
        return Some((pos, state));
    }
    let below = pos.below();
    let lower = level.block(below);
    (lower.is(ids::PITCHER_CROP) && lower.value("half") == Some("lower")).then_some((below, lower))
}

/// `PitcherCropBlock.canGrow`.
fn pitcher_can_grow(level: &Level, lower_pos: BlockPos, lower: BlockState, new_age: i32) -> bool {
    let above = lower_pos.above();
    let into = level.block(above);
    lower.int("age").unwrap_or(0) < 4
        && (level.raw_brightness(lower_pos) >= 8 || can_see_sky(level, lower_pos))
        && in_height(level, above)
        && (!pitcher_is_double(new_age) || into.is_air() || into.is(ids::PITCHER_CROP))
}

/// `canSeeSky(pos)`, read as full sky light at `pos`.
fn can_see_sky(level: &Level, pos: BlockPos) -> bool {
    level.light(pos).0 >= 15
}

// --- Bamboo ----------------------------------------------------------------------------------

fn bamboo_height_above(level: &Level, pos: BlockPos) -> i32 {
    let mut height = 0;
    while height < 16 && level.block(pos.offset(0, height + 1, 0)).is(ids::BAMBOO) {
        height += 1;
    }
    height
}

fn bamboo_height_below(level: &Level, pos: BlockPos) -> i32 {
    let mut height = 0;
    while height < 16 && level.block(pos.offset(0, -height - 1, 0)).is(ids::BAMBOO) {
        height += 1;
    }
    height
}

/// `BambooStalkBlock.performBonemeal`.
fn perform_bamboo(level: &mut Level, pos: BlockPos) {
    let above = bamboo_height_above(level, pos);
    let below = bamboo_height_below(level, pos);
    let count = 1 + level.random().next_int_bounded(2);
    for grown in 0..count {
        let total = above + below + 1 + grown;
        let top = pos.offset(0, above + grown, 0);
        let top_state = level.block(top);
        let growth_pos = top.above();
        if total >= 16
            || top_state.int("stage") == Some(1)
            || !is_empty(level, growth_pos)
            || !in_height(level, growth_pos)
        {
            return;
        }
        grow_bamboo(level, top_state, top, total);
    }
}

/// `BambooStalkBlock.growBamboo`.
fn grow_bamboo(level: &mut Level, state: BlockState, pos: BlockPos, height: i32) {
    let below = level.block(pos.below());
    let two_below_pos = pos.offset(0, -2, 0);
    let two_below = level.block(two_below_pos);
    let mut leaves = "none";
    if height >= 1 {
        if !below.is(ids::BAMBOO) || below.value("leaves") == Some("none") {
            leaves = "small";
        } else {
            leaves = "large";
            if two_below.is(ids::BAMBOO) {
                level.set_block_and_update(pos.below(), below.set("leaves", "small"));
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
    let grown = ids::BAMBOO
        .default_state()
        .set_int("age", age)
        .set("leaves", leaves)
        .set_int("stage", stage);
    level.set_block_and_update(pos.above(), grown);
}

// --- Growing plants (kelp, weeping and twisting vines) ---------------------------------------

/// The growth direction of a growing plant head or body.
fn head_direction(block: BlockId) -> Direction {
    if block == ids::WEEPING_VINES || block == ids::WEEPING_VINES_PLANT {
        Direction::Down
    } else {
        Direction::Up
    }
}

/// The head block of a growing plant body.
fn head_of(body: BlockId) -> BlockId {
    if body == ids::KELP_PLANT {
        ids::KELP
    } else if body == ids::WEEPING_VINES_PLANT {
        ids::WEEPING_VINES
    } else {
        ids::TWISTING_VINES
    }
}

/// `canGrowInto` of a growing plant head.
fn can_grow_into(head: BlockId, state: BlockState) -> bool {
    if head == ids::KELP {
        state.is(ids::WATER)
    } else {
        state.is_air()
    }
}

/// `BlockUtil.getTopConnectedBlock`.
fn top_connected(
    level: &Level,
    pos: BlockPos,
    body: BlockId,
    direction: Direction,
    head: BlockId,
) -> Option<BlockPos> {
    let mut forward = pos;
    loop {
        forward = forward.relative(direction);
        let state = level.block(forward);
        if !state.is(body) {
            return state.is(head).then_some(forward);
        }
    }
}

/// The head of the growing plant body at `pos`.
fn growing_plant_head(level: &Level, pos: BlockPos, body: BlockId) -> Option<(BlockId, BlockPos)> {
    let head = head_of(body);
    top_connected(level, pos, body, head_direction(body), head).map(|p| (head, p))
}

/// `GrowingPlantHeadBlock.performBonemeal`.
fn grow_plant_head(level: &mut Level, pos: BlockPos, state: BlockState) {
    let head = state.block();
    let direction = head_direction(head);
    let mut forward = pos.relative(direction);
    let mut next_age = (state.int("age").unwrap_or(0) + 1).min(25);
    let blocks = if head == ids::KELP {
        1
    } else {
        // `NetherVines.getBlocksToGrowWhenBonemealed`.
        let mut probability = 1.0;
        let mut count = 0;
        while level.random().next_double() < probability {
            probability *= 0.826;
            count += 1;
        }
        count
    };
    let mut i = 0;
    while i < blocks && can_grow_into(head, level.block(forward)) && in_height(level, forward) {
        level.set_block_and_update(forward, state.set_int("age", next_age));
        forward = forward.relative(direction);
        next_age = (next_age + 1).min(25);
        i += 1;
    }
}

// --- Double plants and dripleaves ------------------------------------------------------------

/// `TallGrassBlock.getGrownBlock`.
fn tall_grass_grown(block: BlockId) -> BlockId {
    if block == ids::FERN {
        ids::LARGE_FERN
    } else {
        ids::TALL_GRASS
    }
}

/// `DoublePlantBlock.placeAt`.
fn place_double_plant(level: &mut Level, state: BlockState, lower: BlockPos, flags: u32) {
    let upper = lower.above();
    let lower_state = copy_waterlogged(level, lower, state.set("half", "lower"));
    level.set_block(lower, lower_state, flags);
    let upper_state = copy_waterlogged(level, upper, state.set("half", "upper"));
    level.set_block(upper, upper_state, flags);
}

/// `DoublePlantBlock.copyWaterloggedFrom`.
fn copy_waterlogged(level: &Level, pos: BlockPos, state: BlockState) -> BlockState {
    if state.value("waterlogged").is_some() {
        state.set_flag("waterlogged", super::fluid::is_water(level.fluid(pos)))
    } else {
        state
    }
}

/// `BigDripleafBlock.canGrowInto` / `canPlaceAt`.
fn dripleaf_can_grow_into(level: &Level, pos: BlockPos) -> bool {
    let state = level.block(pos);
    in_height(level, pos)
        && (state.is_air() || state.is(ids::WATER) || state.is(ids::SMALL_DRIPLEAF))
}

/// `BigDripleafBlock.place` / `BigDripleafStemBlock.place`.
fn place_dripleaf(
    level: &mut Level,
    block: BlockId,
    pos: BlockPos,
    water: bool,
    facing: Direction,
) {
    let state = block
        .default_state()
        .set_flag("waterlogged", water)
        .set_direction("facing", facing);
    level.set_block_and_update(pos, state);
}

/// `BigDripleafBlock.placeWithRandomHeight`.
fn big_dripleaf_with_random_height(level: &mut Level, bottom: BlockPos, facing: Direction) {
    let desired = next_int_between(level, 2, 5);
    let mut height = 0;
    let mut pos = bottom;
    while height < desired && dripleaf_can_grow_into(level, pos) {
        height += 1;
        pos = pos.above();
    }
    let leaf_y = bottom.y + height - 1;
    let mut pos = bottom;
    while pos.y < leaf_y {
        let water = is_water_source(level, pos);
        place_dripleaf(level, ids::BIG_DRIPLEAF_STEM, pos, water, facing);
        pos = pos.above();
    }
    let water = is_water_source(level, pos);
    place_dripleaf(level, ids::BIG_DRIPLEAF, pos, water, facing);
}

/// `HangingMossBlock.getTip`.
fn hanging_moss_tip(level: &Level, pos: BlockPos, block: BlockId) -> BlockPos {
    let mut forward = pos;
    loop {
        forward = forward.below();
        if !level.block(forward).is(block) {
            return forward.above();
        }
    }
}

// --- Spreading -------------------------------------------------------------------------------

/// `BonemealableBlock.hasSpreadableNeighbourPos` (fixed order).
fn spreadable_neighbour(level: &Level, pos: BlockPos, place: BlockState) -> Option<BlockPos> {
    HORIZONTAL
        .into_iter()
        .map(|d| pos.relative(d))
        .find(|&neighbour| is_empty(level, neighbour) && level.can_survive(place, neighbour))
}

/// `BonemealableBlock.findSpreadableNeighbourPos`: the horizontal directions shuffled with the
/// level random (`Util.shuffle`).
fn find_spreadable_neighbour(
    level: &mut Level,
    pos: BlockPos,
    place: BlockState,
) -> Option<BlockPos> {
    let mut directions = HORIZONTAL;
    for i in (2..=directions.len()).rev() {
        let swap = level.random().next_int_bounded(i as i32) as usize;
        directions.swap(i - 1, swap);
    }
    directions
        .into_iter()
        .map(|d| pos.relative(d))
        .find(|&neighbour| is_empty(level, neighbour) && level.can_survive(place, neighbour))
}

/// `SeaPickleBlock.performBonemeal`.
fn perform_sea_pickle(level: &mut Level, pos: BlockPos, state: BlockState) {
    let x_start = pos.x - 2;
    let mut z_span = 1;
    let mut z_offset = 0;
    for (count, x) in (0..5).enumerate() {
        for z in 0..z_span {
            let end_y = 2 + pos.y - 1;
            for y in end_y - 2..end_y {
                let position = BlockPos::new(x_start + x, y, pos.z - z_offset + z);
                if position != pos
                    && level.random().next_int_bounded(6) == 0
                    && level.block(position).is(ids::WATER)
                    && block_tags()
                        .coral_blocks
                        .contains(level.block(position.below()))
                {
                    let pickles = level.random().next_int_bounded(4) + 1;
                    level.set_block_and_update(
                        position,
                        ids::SEA_PICKLE.default_state().set_int("pickles", pickles),
                    );
                }
            }
        }
        if count < 2 {
            z_span += 2;
            z_offset += 1;
        } else {
            z_span -= 2;
            z_offset -= 1;
        }
    }
    level.set_block(pos, state.set_int("pickles", 4), update::CLIENTS);
}

/// `MossyCarpetBlock.createTopperWithSideChance(level, pos, () -> true)`.
fn mossy_topper(level: &Level, pos: BlockPos, state: BlockState) -> Option<BlockState> {
    super::place::mossy_carpet_topper(state, pos, level, &mut || true)
}

/// Warped and crimson nylium in the 3×3×3 box around `pos` (`NetherrackBlock`).
fn nylium_around(level: &Level, pos: BlockPos) -> (bool, bool) {
    let (mut warped, mut crimson) = (false, false);
    for y in -1..=1 {
        for z in -1..=1 {
            for x in -1..=1 {
                let state = level.block(pos.offset(x, y, z));
                if !block_tags().nylium.contains(state) {
                    continue;
                }
                if state.is(ids::WARPED_NYLIUM) {
                    warped = true;
                } else if state.is(ids::CRIMSON_NYLIUM) {
                    crimson = true;
                }
                if warped && crimson {
                    return (true, true);
                }
            }
        }
    }
    (warped, crimson)
}

/// `GrassBlock.performBonemeal`: short grass (`grass_bonemeal`) and one in eight times a flower
/// feature of the biome (`getBoneMealFeatures`) around the block.
fn perform_grass(level: &mut Level, pos: BlockPos) {
    let above = pos.above();
    let grass = ids::SHORT_GRASS.default_state();
    'attempts: for attempt in 0..128 {
        let mut test = above;
        for _ in 0..attempt / 16 {
            let dx = level.random().next_int_bounded(3) - 1;
            let dy =
                (level.random().next_int_bounded(3) - 1) * level.random().next_int_bounded(3) / 2;
            let dz = level.random().next_int_bounded(3) - 1;
            test = test.offset(dx, dy, dz);
            // `stopBonemealSpread`.
            if !level.block(test.below()).is(ids::GRASS_BLOCK) || full_collision(level.block(test))
            {
                continue 'attempts;
            }
        }
        let current = level.block(test);
        if current.is(ids::SHORT_GRASS)
            && level.random().next_float() < 0.1
            && is_valid_target(level, test, current)
        {
            perform(level, test, current, 0);
        }
        if current.is_air() && in_height(level, test) {
            if level.random().next_float() < 0.125 {
                if !super::tree::biome_flowers_empty(level, test) {
                    super::tree::place_biome_flower(level, test);
                }
            } else if level.tree_features().is_some() {
                super::tree::place_grass_bonemeal(level, test);
            } else if level.block(test).is_air() && level.can_survive(grass, test) {
                // `grass_bonemeal` without the data packs: `simple_block` of short grass.
                level.set_block(test, grass, update::CLIENTS);
            }
        }
    }
}

impl super::place::BlockReader for Level {
    fn state(&self, pos: BlockPos) -> BlockState {
        self.block(pos)
    }

    fn min_y(&self) -> i32 {
        Level::min_y(self)
    }

    fn max_y(&self) -> i32 {
        Level::max_y(self)
    }
}

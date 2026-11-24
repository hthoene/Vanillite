//! Plants, crops, corals and eggs.
//!
//! Light levels are not readable through [`BlockReader`]: the light conditions of
//! `MushroomBlock.canSurvive` (raw brightness below 13), `CropBlock.canSurvive` and
//! `PitcherCropBlock.canSurvive` (raw brightness at least 8) are taken as met.

use vanillite_base::{BlockPos, Direction};
use vanillite_data::block::{BlockId, BlockKind, BlockState, flags, ids};

use super::tags;
use super::util::{self, DirExt, StateExt, Support, above, below};
use super::{BlockReader, PlaceContext};

const UPPER: &str = "upper";
const LOWER: &str = "lower";

/// `getStateForPlacement` of the kinds of this group (no survival check).
pub(super) fn state_for_placement(
    block: BlockId,
    ctx: &PlaceContext,
    level: &dyn BlockReader,
) -> Option<BlockState> {
    use BlockKind as K;
    let pos = ctx.pos;
    let default = block.default_state();
    match block.info().kind {
        K::MangrovePropaguleBlock => Some(
            default
                .set_bool("waterlogged", util::is_water_source(level, pos))
                .set_int("age", 4),
        ),
        K::DoublePlantBlock | K::TallFlowerBlock => double_plant_placement(block, ctx, level),
        K::TallSeagrassBlock => {
            let state = double_plant_placement(block, ctx, level)?;
            full_water(level.state(above(pos))).then_some(state)
        }
        K::SmallDripleafBlock => {
            let state = double_plant_placement(block, ctx, level)?;
            // `DoublePlantBlock.copyWaterloggedFrom`: `isWaterAt` (any water).
            Some(
                state
                    .set_dir("facing", ctx.horizontal_direction().opposite())
                    .set_bool("waterlogged", util::is_water(level, pos)),
            )
        }
        // `PitcherCropBlock.getStateForPlacement` skips the space check of `DoublePlantBlock`.
        K::PitcherCropBlock | K::BigDripleafStemBlock => Some(default),
        K::BigDripleafBlock => {
            let below_state = level.state(below(pos));
            let facing =
                if below_state.is(ids::BIG_DRIPLEAF) || below_state.is(ids::BIG_DRIPLEAF_STEM) {
                    below_state.get_dir("facing").unwrap_or(Direction::North)
                } else {
                    ctx.horizontal_direction().opposite()
                };
            Some(
                default
                    .set_bool("waterlogged", util::is_water_source(level, pos))
                    .set_dir("facing", facing),
            )
        }
        K::BambooStalkBlock => bamboo_placement(block, ctx, level),
        K::SeagrassBlock => full_water(level.state(pos)).then_some(default),
        K::CoralPlantBlock | K::BaseCoralPlantBlock | K::CoralFanBlock | K::BaseCoralFanBlock => {
            Some(coral_placement(default, ctx, level))
        }
        K::CoralWallFanBlock | K::BaseCoralWallFanBlock => {
            let mut state = coral_placement(default, ctx, level);
            for direction in ctx.nearest_looking_directions() {
                if direction.is_horizontal() {
                    state = state.set_dir("facing", direction.opposite());
                    if can_survive(state, pos, level) {
                        return Some(state);
                    }
                }
            }
            None
        }
        K::FlowerBedBlock => Some(segment_placement(block, ctx, level, "flower_amount")),
        K::LeafLitterBlock => Some(segment_placement(block, ctx, level, "segment_amount")),
        K::TurtleEggBlock => {
            let existing = level.state(pos);
            Some(if existing.is(block) {
                existing.set_int("eggs", (existing.get_int("eggs") + 1).min(4))
            } else {
                default
            })
        }
        K::DriedGhastBlock => Some(
            default
                .set_bool("waterlogged", util::is_water_source(level, pos))
                .set_dir("facing", ctx.horizontal_direction().opposite()),
        ),
        K::PotentSulfurBlock => Some(potent_sulfur_state(default, pos, level)),
        _ => Some(util::default_placement(block, ctx, level)),
    }
}

/// `canSurvive` of the kinds of this group.
pub(super) fn can_survive(state: BlockState, pos: BlockPos, level: &dyn BlockReader) -> bool {
    use BlockKind as K;
    match state.kind() {
        K::BushBlock
        | K::FlowerBlock
        | K::WitherRoseBlock
        | K::EyeblossomBlock
        | K::TallGrassBlock
        | K::FireflyBushBlock
        | K::DryVegetationBlock
        | K::ShortDryGrassBlock
        | K::TallDryGrassBlock
        | K::SaplingBlock
        | K::AzaleaBlock
        | K::NetherFungusBlock
        | K::NetherRootsBlock
        | K::NetherSproutsBlock
        | K::StemBlock
        | K::AttachedStemBlock
        | K::NetherWartBlock
        | K::SweetBerryBushBlock
        | K::SeagrassBlock
        | K::CactusFlowerBlock
        | K::LilyPadBlock
        | K::FlowerBedBlock => vegetation_survives(state, pos, level),
        // `CropBlock.canSurvive`: sufficient light (taken as met) and `VegetationBlock`'s rule.
        K::CropBlock
        | K::CarrotBlock
        | K::PotatoBlock
        | K::BeetrootBlock
        | K::TorchflowerCropBlock => vegetation_survives(state, pos, level),
        K::MangrovePropaguleBlock => {
            if state.get_bool("hanging") {
                tags::SUPPORTS_HANGING_MANGROVE_PROPAGULE.has(level.state(above(pos)))
            } else {
                vegetation_survives(state, pos, level)
            }
        }
        K::MushroomBlock => {
            let below_pos = below(pos);
            let below_state = level.state(below_pos);
            // The raw brightness below 13 is taken as met.
            tags::OVERRIDES_MUSHROOM_LIGHT_REQUIREMENT.has(below_state)
                || may_place_on(state.block(), below_state, below_pos, level)
        }
        // `PitcherCropBlock.canSurvive`: the light check of the lower half is taken as met.
        K::DoublePlantBlock | K::TallFlowerBlock | K::PitcherCropBlock => {
            double_plant_survives(state, pos, level)
        }
        K::TallSeagrassBlock => {
            if state.get("half") == Some(UPPER) {
                double_plant_survives(state, pos, level)
            } else {
                vegetation_survives(state, pos, level) && full_water(level.state(pos))
            }
        }
        K::SmallDripleafBlock => {
            if state.get("half") == Some(UPPER) {
                double_plant_survives(state, pos, level)
            } else {
                let below_pos = below(pos);
                may_place_on(state.block(), level.state(below_pos), below_pos, level)
            }
        }
        K::BigDripleafBlock => {
            let below_state = level.state(below(pos));
            below_state.is(state.block())
                || below_state.is(ids::BIG_DRIPLEAF_STEM)
                || tags::SUPPORTS_BIG_DRIPLEAF.has(below_state)
        }
        K::BigDripleafStemBlock => {
            let below_state = level.state(below(pos));
            let above_state = level.state(above(pos));
            (below_state.is(state.block()) || tags::SUPPORTS_BIG_DRIPLEAF.has(below_state))
                && (above_state.is(state.block()) || above_state.is(ids::BIG_DRIPLEAF))
        }
        K::SugarCaneBlock => sugar_cane_survives(state, pos, level),
        K::CactusBlock => cactus_survives(state, pos, level),
        K::BambooStalkBlock | K::BambooSaplingBlock => {
            tags::SUPPORTS_BAMBOO.has(level.state(below(pos)))
        }
        K::CoralPlantBlock | K::BaseCoralPlantBlock | K::CoralFanBlock | K::BaseCoralFanBlock => {
            util::sturdy(level, below(pos), Direction::Up, Support::Full)
        }
        K::CoralWallFanBlock | K::BaseCoralWallFanBlock => {
            let Some(facing) = state.get_dir("facing") else {
                return false;
            };
            util::sturdy(
                level,
                pos.relative(facing.opposite()),
                facing,
                Support::Full,
            )
        }
        K::FrogspawnBlock => {
            let below_pos = below(pos);
            let below_state = level.state(below_pos);
            (below_state.fluid_state().info().fluid == util::fluid::WATER
                || tags::SUPPORTS_FROGSPAWN.has(below_state))
                && level.state(pos).fluid_state().is_empty()
        }
        K::LeafLitterBlock => util::sturdy(level, below(pos), Direction::Up, Support::Full),
        K::ChorusFlowerBlock => chorus_flower_survives(pos, level),
        _ => true,
    }
}

/// `canBeReplaced(state, context)` of the kinds of this group.
pub(super) fn can_be_replaced(state: BlockState, ctx: &PlaceContext) -> bool {
    use BlockKind as K;
    match state.kind() {
        // `SegmentableBlock.canBeReplaced`.
        K::FlowerBedBlock => {
            segment_can_be_replaced(state, ctx, "flower_amount")
                || util::default_can_be_replaced(state, ctx)
        }
        K::LeafLitterBlock => {
            segment_can_be_replaced(state, ctx, "segment_amount")
                || util::default_can_be_replaced(state, ctx)
        }
        K::TurtleEggBlock => {
            segment_can_be_replaced(state, ctx, "eggs") || util::default_can_be_replaced(state, ctx)
        }
        K::PitcherCropBlock => false,
        _ => util::default_can_be_replaced(state, ctx),
    }
}

/// Blocks placed with `state` (`setPlacedBy` and item specifics).
pub(super) fn placed_extra(
    state: BlockState,
    pos: BlockPos,
    _ctx: &PlaceContext,
    level: &dyn BlockReader,
    out: &mut Vec<(BlockPos, BlockState)>,
) {
    use BlockKind as K;
    match state.kind() {
        // `DoublePlantBlock.setPlacedBy`; tall grass, ferns, pitcher plants and tall flowers have no
        // `waterlogged`, so `copyWaterloggedFrom` changes nothing.
        K::DoublePlantBlock | K::TallFlowerBlock | K::TallSeagrassBlock => {
            let upper = state.block().default_state().set("half", UPPER);
            out.push((above(pos), upper));
        }
        // `SmallDripleafBlock.setPlacedBy` with `copyWaterloggedFrom(level, above)`.
        K::SmallDripleafBlock => {
            let mut upper = state.block().default_state().set("half", UPPER);
            if let Some(facing) = state.get_dir("facing") {
                upper = upper.set_dir("facing", facing);
            }
            upper = upper.set_bool("waterlogged", util::is_water(level, above(pos)));
            out.push((above(pos), upper));
        }
        // `PitcherCropBlock.setPlacedBy` does nothing.
        _ => {}
    }
}

/// Blocks changed when a player breaks `state` (`playerWillDestroy`).
pub(super) fn destroy_extra(
    state: BlockState,
    pos: BlockPos,
    level: &dyn BlockReader,
    creative: bool,
    out: &mut Vec<(BlockPos, BlockState)>,
) {
    use BlockKind as K;
    match state.kind() {
        // `DoublePlantBlock.playerWillDestroy` → `preventDropFromBottomPart` (creative only).
        K::DoublePlantBlock
        | K::TallFlowerBlock
        | K::TallSeagrassBlock
        | K::SmallDripleafBlock
        | K::PitcherCropBlock => {
            if !creative || state.get("half") != Some(UPPER) {
                return;
            }
            let bottom_pos = below(pos);
            let bottom = level.state(bottom_pos);
            if bottom.is(state.block()) && bottom.get("half") == Some(LOWER) {
                let replacement = if bottom.fluid_state().info().fluid == util::fluid::WATER {
                    super::water()
                } else {
                    BlockState::AIR
                };
                out.push((bottom_pos, replacement));
            }
        }
        _ => {}
    }
}

/// `fluidState.is(FluidTags.WATER) && fluidState.isFull()`.
fn full_water(state: BlockState) -> bool {
    util::fluid_is_water(state) && util::fluid_amount(state) == 8
}

/// `VegetationBlock.mayPlaceOn` and its overrides: whether `support` at `support_pos` carries
/// `block` above it.
fn may_place_on(
    block: BlockId,
    support: BlockState,
    support_pos: BlockPos,
    level: &dyn BlockReader,
) -> bool {
    use BlockKind as K;
    match block.info().kind {
        K::WitherRoseBlock => tags::SUPPORTS_WITHER_ROSE.has(support),
        K::DryVegetationBlock | K::ShortDryGrassBlock | K::TallDryGrassBlock => {
            tags::SUPPORTS_DRY_VEGETATION.has(support)
        }
        K::AzaleaBlock => tags::SUPPORTS_AZALEA.has(support),
        K::MangrovePropaguleBlock => tags::SUPPORTS_MANGROVE_PROPAGULE.has(support),
        K::MushroomBlock => support.has(flags::SOLID_RENDER),
        K::NetherFungusBlock => {
            if block == ids::WARPED_FUNGUS {
                tags::SUPPORTS_WARPED_FUNGUS.has(support)
            } else {
                tags::SUPPORTS_CRIMSON_FUNGUS.has(support)
            }
        }
        K::NetherRootsBlock => {
            if block == ids::WARPED_ROOTS {
                tags::SUPPORTS_WARPED_ROOTS.has(support)
            } else {
                tags::SUPPORTS_CRIMSON_ROOTS.has(support)
            }
        }
        K::NetherSproutsBlock => tags::SUPPORTS_NETHER_SPROUTS.has(support),
        K::CropBlock
        | K::CarrotBlock
        | K::PotatoBlock
        | K::BeetrootBlock
        | K::TorchflowerCropBlock
        | K::PitcherCropBlock => tags::SUPPORTS_CROPS.has(support),
        K::StemBlock | K::AttachedStemBlock => {
            if block == ids::PUMPKIN_STEM || block == ids::ATTACHED_PUMPKIN_STEM {
                tags::SUPPORTS_PUMPKIN_STEM.has(support)
            } else {
                tags::SUPPORTS_MELON_STEM.has(support)
            }
        }
        K::NetherWartBlock => tags::SUPPORTS_NETHER_WART.has(support),
        K::SeagrassBlock | K::TallSeagrassBlock => {
            support.sturdy(Direction::Up, Support::Full)
                && !tags::CANNOT_SUPPORT_SEAGRASS.has(support)
        }
        K::SmallDripleafBlock => {
            tags::SUPPORTS_SMALL_DRIPLEAF.has(support)
                || (util::is_water_source(level, above(support_pos))
                    && tags::SUPPORTS_VEGETATION.has(support))
        }
        K::CactusFlowerBlock => {
            tags::SUPPORT_OVERRIDE_CACTUS_FLOWER.has(support)
                || support.sturdy(Direction::Up, Support::Center)
        }
        K::LilyPadBlock => {
            (support.fluid_state().info().fluid == util::fluid::WATER
                || tags::SUPPORTS_LILY_PAD.has(support))
                && level.state(above(support_pos)).fluid_state().is_empty()
        }
        _ => tags::SUPPORTS_VEGETATION.has(support),
    }
}

/// `VegetationBlock.canSurvive`: `mayPlaceOn` of the block below.
fn vegetation_survives(state: BlockState, pos: BlockPos, level: &dyn BlockReader) -> bool {
    let below_pos = below(pos);
    may_place_on(state.block(), level.state(below_pos), below_pos, level)
}

/// `DoublePlantBlock.canSurvive`: the lower half follows `VegetationBlock`, the upper half needs
/// the lower half below.
fn double_plant_survives(state: BlockState, pos: BlockPos, level: &dyn BlockReader) -> bool {
    if state.get("half") != Some(UPPER) {
        vegetation_survives(state, pos, level)
    } else {
        let below_state = level.state(below(pos));
        below_state.is(state.block()) && below_state.get("half") == Some(LOWER)
    }
}

/// `DoublePlantBlock.getStateForPlacement`: room for the upper half.
fn double_plant_placement(
    block: BlockId,
    ctx: &PlaceContext,
    level: &dyn BlockReader,
) -> Option<BlockState> {
    let pos = ctx.pos;
    // `Level.getMaxY()` is the highest block y, one below the exclusive top.
    let room = pos.y < level.max_y() - 1 && super::can_be_replaced(level.state(above(pos)), ctx);
    room.then(|| block.default_state())
}

/// `BambooStalkBlock.getStateForPlacement` (may place a bamboo sapling instead).
fn bamboo_placement(
    block: BlockId,
    ctx: &PlaceContext,
    level: &dyn BlockReader,
) -> Option<BlockState> {
    let pos = ctx.pos;
    if !level.state(pos).fluid_state().is_empty() {
        return None;
    }
    let below_state = level.state(below(pos));
    if !tags::SUPPORTS_BAMBOO.has(below_state) {
        return None;
    }
    let default = block.default_state();
    if below_state.is(ids::BAMBOO_SAPLING) {
        Some(default.set_int("age", 0))
    } else if below_state.is(ids::BAMBOO) {
        let age = i32::from(below_state.get_int("age") > 0);
        Some(default.set_int("age", age))
    } else {
        let above_state = level.state(above(pos));
        Some(if above_state.is(ids::BAMBOO) {
            default.set_int("age", above_state.get_int("age"))
        } else {
            ids::BAMBOO_SAPLING.default_state()
        })
    }
}

/// `BaseCoralPlantTypeBlock.getStateForPlacement`: waterlogged in full water.
fn coral_placement(default: BlockState, ctx: &PlaceContext, level: &dyn BlockReader) -> BlockState {
    default.set_bool("waterlogged", full_water(level.state(ctx.pos)))
}

/// `SegmentableBlock.getStateForPlacement` (also `TurtleEggBlock`'s shape of it).
fn segment_placement(
    block: BlockId,
    ctx: &PlaceContext,
    level: &dyn BlockReader,
    amount: &str,
) -> BlockState {
    let existing = level.state(ctx.pos);
    if existing.is(block) {
        existing.set_int(amount, (existing.get_int(amount) + 1).min(4))
    } else {
        block
            .default_state()
            .set_dir("facing", ctx.horizontal_direction().opposite())
    }
}

/// `SegmentableBlock.canBeReplaced(state, context, segment)` (also `TurtleEggBlock`'s): one more
/// segment fits.
fn segment_can_be_replaced(state: BlockState, ctx: &PlaceContext, amount: &str) -> bool {
    !ctx.sneaking && util::holds_item_of(ctx, state.block()) && state.get_int(amount) < 4
}

/// `SugarCaneBlock.canSurvive`.
fn sugar_cane_survives(state: BlockState, pos: BlockPos, level: &dyn BlockReader) -> bool {
    let below_pos = below(pos);
    let below_state = level.state(below_pos);
    if below_state.is(state.block()) {
        return true;
    }
    if tags::SUPPORTS_SUGAR_CANE.has(below_state) {
        for direction in util::HORIZONTAL {
            let neighbour = level.state(below_pos.relative(direction));
            if util::fluid_is_water(neighbour)
                || tags::SUPPORTS_SUGAR_CANE_ADJACENTLY.has(neighbour)
            {
                return true;
            }
        }
    }
    false
}

/// `CactusBlock.canSurvive`.
fn cactus_survives(state: BlockState, pos: BlockPos, level: &dyn BlockReader) -> bool {
    for direction in util::HORIZONTAL {
        let neighbour = level.state(pos.relative(direction));
        if neighbour.is_solid() || util::fluid_is_lava(neighbour) {
            return false;
        }
    }
    let below_state = level.state(below(pos));
    (below_state.is(state.block()) || tags::SUPPORTS_CACTUS.has(below_state))
        && !level.state(above(pos)).has(flags::LIQUID)
}

/// `ChorusFlowerBlock.canSurvive`.
fn chorus_flower_survives(pos: BlockPos, level: &dyn BlockReader) -> bool {
    let plant = ids::CHORUS_PLANT;
    let below_state = level.state(below(pos));
    if below_state.is(plant) || tags::SUPPORTS_CHORUS_FLOWER.has(below_state) {
        return true;
    }
    if !below_state.is_air() {
        return false;
    }
    let mut one_neighbour = false;
    for direction in util::HORIZONTAL {
        let neighbour = level.state(pos.relative(direction));
        if neighbour.is(plant) {
            if one_neighbour {
                return false;
            }
            one_neighbour = true;
        } else if !neighbour.is_air() {
            return false;
        }
    }
    one_neighbour
}

/// `PotentSulfurBlock.validBlockState` (the geyser countdown reset of the block entity is the
/// caller's).
fn potent_sulfur_state(state: BlockState, pos: BlockPos, level: &dyn BlockReader) -> BlockState {
    const PROP: &str = "potent_sulfur_state";
    if !util::is_water_source(level, above(pos)) {
        return state.set(PROP, "dry");
    }
    let below_state = level.state(below(pos));
    let fluid = below_state.fluid_state();
    let source_if_fluid = fluid.is_empty() || fluid.info().source;
    if tags::CAUSES_CONTINUOUS_GEYSER_ERUPTIONS.has(below_state) && source_if_fluid {
        state.set(PROP, "continuous")
    } else if tags::CAUSES_PERIODIC_GEYSER_ERUPTIONS.has(below_state) && source_if_fluid {
        if state.get(PROP) == Some("erupting") {
            state
        } else {
            state.set(PROP, "dormant")
        }
    } else {
        state.set(PROP, "wet")
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::*;
    use super::super::util::StateExt;
    use super::super::{can_survive, placed_extra, state_for_placement};
    use vanillite_base::{BlockPos, Direction};

    #[test]
    fn flowers_need_vegetation_support() {
        let mut level = TestLevel::new();
        let pos = BlockPos::new(0, 65, 0);
        let poppy = block("poppy").default_state();
        level.set_block(pos.offset(0, -1, 0), "stone");
        assert!(!can_survive(poppy, pos, &level));
        level.set_block(pos.offset(0, -1, 0), "grass_block");
        assert!(can_survive(poppy, pos, &level));
        let c = ctx(
            &level,
            "poppy",
            pos.offset(0, -1, 0),
            Direction::Up,
            0.0,
            60.0,
        );
        assert_state(state_for_placement(&c, &level), "poppy", &[]);
        level.set_block(pos.offset(0, -1, 0), "stone");
        let c = ctx(
            &level,
            "dandelion",
            pos.offset(0, -1, 0),
            Direction::Up,
            0.0,
            60.0,
        );
        assert_eq!(state_for_placement(&c, &level), None);
    }

    #[test]
    fn tall_flowers_place_their_upper_half() {
        let mut level = TestLevel::new();
        let ground = BlockPos::new(3, 64, -2);
        level.set_block(ground, "grass_block");
        let c = ctx(&level, "sunflower", ground, Direction::Up, 90.0, 60.0);
        let lower = state_for_placement(&c, &level);
        assert_state(lower, "sunflower", &[("half", "lower")]);
        let mut out = Vec::new();
        placed_extra(lower.unwrap(), c.pos, &c, &level, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, c.pos.offset(0, 1, 0));
        assert_state(Some(out[0].1), "sunflower", &[("half", "upper")]);
        // No room above: a solid block blocks the upper half.
        level.set_block(c.pos.offset(0, 1, 0), "stone");
        let c = ctx(&level, "sunflower", ground, Direction::Up, 90.0, 60.0);
        assert_eq!(state_for_placement(&c, &level), None);
    }

    #[test]
    fn creative_break_of_upper_half_removes_the_lower_half() {
        let mut level = TestLevel::new();
        let lower_pos = BlockPos::new(0, 65, 0);
        let lower = block("lilac").default_state().set("half", "lower");
        let upper = lower.set("half", "upper");
        level
            .set(lower_pos, lower)
            .set(lower_pos.offset(0, 1, 0), upper);
        let mut out = Vec::new();
        super::super::destroy_extra(upper, lower_pos.offset(0, 1, 0), &level, true, &mut out);
        assert_eq!(
            out,
            vec![(lower_pos, vanillite_data::block::BlockState::AIR)]
        );
        out.clear();
        super::super::destroy_extra(upper, lower_pos.offset(0, 1, 0), &level, false, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn flower_beds_stack_and_face_the_player() {
        let mut level = TestLevel::new();
        let ground = BlockPos::new(0, 64, 0);
        level.set_block(ground, "grass_block");
        // Facing south (yaw 0): the bed faces north.
        let c = ctx(&level, "pink_petals", ground, Direction::Up, 0.0, 60.0);
        let first = state_for_placement(&c, &level);
        assert_state(
            first,
            "pink_petals",
            &[("facing", "north"), ("flower_amount", "1")],
        );
        level.set(c.pos, first.unwrap());
        let c = ctx(&level, "pink_petals", c.pos, Direction::Up, 0.0, 60.0);
        assert!(c.replace_clicked);
        assert_state(
            state_for_placement(&c, &level),
            "pink_petals",
            &[("facing", "north"), ("flower_amount", "2")],
        );
    }

    #[test]
    fn sugar_cane_needs_water_next_to_its_ground() {
        let mut level = TestLevel::new();
        let ground = BlockPos::new(0, 64, 0);
        level.set_block(ground, "sand");
        let cane = block("sugar_cane").default_state();
        assert!(!can_survive(cane, ground.offset(0, 1, 0), &level));
        level.set_block(ground.offset(1, 0, 0), "water");
        assert!(can_survive(cane, ground.offset(0, 1, 0), &level));
    }

    #[test]
    fn coral_wall_fans_attach_to_the_clicked_wall() {
        let mut level = TestLevel::new();
        let wall = BlockPos::new(0, 64, 0);
        level.set_block(wall, "stone");
        // Clicking the east face of the wall: the fan hangs on it facing east.
        let c = ctx(&level, "tube_coral_fan", wall, Direction::East, 90.0, 0.0);
        assert_state(
            state_for_placement(&c, &level),
            "tube_coral_wall_fan",
            &[("facing", "east"), ("waterlogged", "false")],
        );
    }

    #[test]
    fn bamboo_on_grass_becomes_a_sapling() {
        let mut level = TestLevel::new();
        let ground = BlockPos::new(0, 64, 0);
        level.set_block(ground, "grass_block");
        let c = ctx(&level, "bamboo", ground, Direction::Up, 0.0, 60.0);
        assert_state(state_for_placement(&c, &level), "bamboo_sapling", &[]);
    }
}

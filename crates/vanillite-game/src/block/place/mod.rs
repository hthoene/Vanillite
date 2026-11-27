//! Vanilla-exact block placement: `BlockPlaceContext`, `BlockItem.getPlacementState` (with the
//! wall variants of `StandingAndWallBlockItem` and `HangingSignItem`), every block's
//! `getStateForPlacement`, `canSurvive` and `canBeReplaced`, and the extra blocks that
//! `setPlacedBy` places and `playerWillDestroy` removes.
//!
//! Dispatch is a `match` over the block's [`BlockKind`] (its Java class): every kind belongs to
//! one of the rule groups below, kinds without rules of their own use the defaults of
//! `Block`/`BlockBehaviour`.
//!
//! What the caller does around these functions (as `BlockItem.place` does):
//! 1. [`PlaceContext::new`] from the hit result.
//! 2. [`update_placement_context`] (scaffolding climbs/extends; other items keep the context).
//! 3. [`state_for_placement`]; `None` = the item fails. Entity collision
//!    (`Level.isUnobstructed`) and the game-master permission of command, structure, jigsaw and
//!    test blocks are the caller's.
//! 4. Set the state with flags 11, then [`placed_extra`] (flags 3) and, for pale moss carpets,
//!    [`mossy_carpet_topper`].
//!
//! Known gaps, all from facts a [`BlockReader`] does not have:
//! - Light: crops, mushrooms and the pitcher crop treat their light requirement as met.
//! - Block entities: redstone signals ([`signal`]) count a powered comparator as 15 and trapped
//!   chests, jukeboxes and test blocks as 0; item components (lectern book, jukebox record, test
//!   block mode, `block_state`) are not applied.
//! - Randomness: growing plant heads (kelp, vines) are placed with age 0 instead of
//!   `nextInt(25)`; the mossy carpet topper takes the random from the caller.
//! - Tags are Vanilla's defaults ([`tags`]); data-pack changes to them are not seen.
//! - The world border (beds) and neighbour shape updates after placement are the caller's.

mod attached;
mod building;
pub use building::copper_chest_from_block;
mod functional;
mod plants;
mod signal;
pub mod tags;
pub mod util;

#[cfg(test)]
mod tests;

use std::sync::OnceLock;

use vanillite_base::{BlockPos, Direction, Vec3};
use vanillite_data::block::{BLOCKS, BlockId, BlockKind, BlockState, ids};
use vanillite_data::item::ItemId;

use util::{DirExt, StateExt};

/// Read access to the blocks of a level.
pub trait BlockReader {
    /// The state at `pos`; air when unloaded or outside the world.
    fn state(&self, pos: BlockPos) -> BlockState;
    /// Lowest block y and the exclusive top of the build height.
    fn min_y(&self) -> i32;
    fn max_y(&self) -> i32;
}

/// `BlockPlaceContext`: a block item used on a block.
#[derive(Clone, Copy, Debug)]
pub struct PlaceContext {
    /// Where the block goes (`BlockPlaceContext.getClickedPos`: the clicked block if it is
    /// replaceable (`replace_clicked`), else clicked + face).
    pub pos: BlockPos,
    pub clicked_pos: BlockPos,
    pub face: Direction,
    /// Absolute click location (`BlockHitResult.getLocation`).
    pub hit: Vec3,
    pub inside: bool,
    pub replace_clicked: bool,
    /// Player yaw in degrees.
    pub y_rot: f32,
    /// Player pitch in degrees.
    pub x_rot: f32,
    /// Whether the player is sneaking (`isSecondaryUseActive`).
    pub sneaking: bool,
    pub item: ItemId,
}

impl PlaceContext {
    /// The `BlockPlaceContext` constructor: computes `pos` and `replace_clicked` from the clicked
    /// block's state.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        clicked_pos: BlockPos,
        face: Direction,
        hit: Vec3,
        inside: bool,
        y_rot: f32,
        x_rot: f32,
        sneaking: bool,
        item: ItemId,
        level: &dyn BlockReader,
    ) -> PlaceContext {
        let mut ctx = PlaceContext {
            pos: clicked_pos,
            clicked_pos,
            face,
            hit,
            inside,
            replace_clicked: true,
            y_rot,
            x_rot,
            sneaking,
            item,
        };
        // `canBeReplaced` runs while `replaceClicked` is still true, so it sees the clicked
        // position as the placement position.
        ctx.replace_clicked = can_be_replaced(level.state(clicked_pos), &ctx);
        if !ctx.replace_clicked {
            ctx.pos = clicked_pos.relative(face);
        }
        ctx
    }

    /// `BlockPlaceContext.at(context, pos, direction)`: a context aimed at the `direction` face
    /// of `pos`.
    pub fn at(&self, pos: BlockPos, direction: Direction, level: &dyn BlockReader) -> PlaceContext {
        let hit = Vec3::new(
            f64::from(pos.x) + 0.5 + f64::from(direction.step_x()) * 0.5,
            f64::from(pos.y) + 0.5 + f64::from(direction.step_y()) * 0.5,
            f64::from(pos.z) + 0.5 + f64::from(direction.step_z()) * 0.5,
        );
        PlaceContext::new(
            pos,
            direction,
            hit,
            false,
            self.y_rot,
            self.x_rot,
            self.sneaking,
            self.item,
            level,
        )
    }

    /// `BlockPlaceContext.canPlace()`.
    pub fn can_place(&self, level: &dyn BlockReader) -> bool {
        self.replace_clicked || can_be_replaced(level.state(self.pos), self)
    }

    /// `getClickedFace()`.
    #[inline]
    pub fn clicked_face(&self) -> Direction {
        self.face
    }

    /// `getClickLocation()`.
    #[inline]
    pub fn click_location(&self) -> Vec3 {
        self.hit
    }

    /// `isSecondaryUseActive()`.
    #[inline]
    pub fn is_secondary_use_active(&self) -> bool {
        self.sneaking
    }

    /// `getRotation()`: the player's yaw.
    #[inline]
    pub fn rotation(&self) -> f32 {
        self.y_rot
    }

    /// `getHorizontalDirection()`: `Direction.fromYRot(player.getYRot())`.
    #[inline]
    pub fn horizontal_direction(&self) -> Direction {
        util::from_y_rot(f64::from(self.y_rot))
    }

    /// `getNearestLookingDirection()`.
    #[inline]
    pub fn nearest_looking_direction(&self) -> Direction {
        self.ordered_by_nearest()[0]
    }

    /// `getNearestLookingVerticalDirection()`.
    #[inline]
    pub fn nearest_looking_vertical_direction(&self) -> Direction {
        if self.x_rot < 0.0 {
            Direction::Up
        } else {
            Direction::Down
        }
    }

    /// `Direction.orderedByNearest(player)`.
    pub fn ordered_by_nearest(&self) -> [Direction; 6] {
        let to_rad = (std::f64::consts::PI / 180.0) as f32;
        let pitch = self.x_rot * to_rad;
        let yaw = -self.y_rot * to_rad;
        let pitch_sin = vanillite_base::math::sin(f64::from(pitch));
        let pitch_cos = vanillite_base::math::cos(f64::from(pitch));
        let yaw_sin = vanillite_base::math::sin(f64::from(yaw));
        let yaw_cos = vanillite_base::math::cos(f64::from(yaw));
        let x_pos = yaw_sin > 0.0;
        let y_pos = pitch_sin < 0.0;
        let z_pos = yaw_cos > 0.0;
        let x_yaw = if x_pos { yaw_sin } else { -yaw_sin };
        let y_mag = if y_pos { -pitch_sin } else { pitch_sin };
        let z_yaw = if z_pos { yaw_cos } else { -yaw_cos };
        let x_mag = x_yaw * pitch_cos;
        let z_mag = z_yaw * pitch_cos;
        let axis_x = if x_pos {
            Direction::East
        } else {
            Direction::West
        };
        let axis_y = if y_pos {
            Direction::Up
        } else {
            Direction::Down
        };
        let axis_z = if z_pos {
            Direction::South
        } else {
            Direction::North
        };
        let make = |a: Direction, b: Direction, c: Direction| {
            [a, b, c, c.opposite(), b.opposite(), a.opposite()]
        };
        if x_yaw > z_yaw {
            if y_mag > x_mag {
                make(axis_y, axis_x, axis_z)
            } else if z_mag > y_mag {
                make(axis_x, axis_z, axis_y)
            } else {
                make(axis_x, axis_y, axis_z)
            }
        } else if y_mag > z_mag {
            make(axis_y, axis_z, axis_x)
        } else if x_mag > y_mag {
            make(axis_z, axis_x, axis_y)
        } else {
            make(axis_z, axis_y, axis_x)
        }
    }

    /// `getNearestLookingDirections()`: [`Self::ordered_by_nearest`], with the opposite of the
    /// clicked face moved to the front unless the clicked block is replaced.
    pub fn nearest_looking_directions(&self) -> [Direction; 6] {
        let mut directions = self.ordered_by_nearest();
        if self.replace_clicked {
            return directions;
        }
        let target = self.face.opposite();
        let index = directions
            .iter()
            .position(|d| *d == target)
            .unwrap_or(directions.len());
        if index > 0 {
            let end = index.min(directions.len() - 1);
            directions.copy_within(0..end, 1);
            directions[0] = target;
        }
        directions
    }
}

/// The rule group a block kind belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Group {
    /// No rules of its own: `Block`'s defaults.
    Default,
    /// Building blocks and containers (`building.rs`).
    Building,
    /// Attached and hanging blocks: torches, signs, banners, heads, buttons, vines, … (`attached.rs`).
    Attached,
    /// Plants, crops, corals and eggs (`plants.rs`).
    Plants,
    /// Redstone, workstations, technical and fluid-like blocks (`functional.rs`).
    Functional,
}

/// The rule group of `kind`.
fn group(kind: BlockKind) -> Group {
    use BlockKind as K;
    match kind {
        K::AirBlock
        | K::AmethystBlock
        | K::Block
        | K::BonemealableFeaturePlacerBlock
        | K::BuddingAmethystBlock
        | K::CartographyTableBlock
        | K::ColoredFallingBlock
        | K::CraftingTableBlock
        | K::CryingObsidianBlock
        | K::DragonEggBlock
        | K::DropExperienceBlock
        | K::HalfTransparentBlock
        | K::HoneyBlock
        | K::IceBlock
        | K::InfestedBlock
        | K::MagmaBlock
        | K::MudBlock
        | K::NetherrackBlock
        | K::NyliumBlock
        | K::PoweredBlock
        | K::PumpkinBlock
        | K::RootedDirtBlock
        | K::SandBlock
        | K::SculkBlock
        | K::SlimeBlock
        | K::SmithingTableBlock
        | K::SoulSandBlock
        | K::SpongeBlock
        | K::WetSpongeBlock
        | K::StainedGlassBlock
        | K::TintedGlassBlock
        | K::TransparentBlock
        | K::StructureVoidBlock
        | K::WeatheringCopperFullBlock
        | K::WebBlock
        | K::PowderSnowBlock
        | K::BeaconBlock
        | K::EnchantingTableBlock
        | K::SpawnerBlock
        | K::RedStoneOreBlock => Group::Default,

        K::StairBlock
        | K::WeatheringCopperStairBlock
        | K::SlabBlock
        | K::WeatheringCopperSlabBlock
        | K::FenceBlock
        | K::FenceGateBlock
        | K::WallBlock
        | K::IronBarsBlock
        | K::WeatheringCopperBarsBlock
        | K::StainedGlassPaneBlock
        | K::DoorBlock
        | K::WeatheringCopperDoorBlock
        | K::TrapDoorBlock
        | K::WeatheringCopperTrapDoorBlock
        | K::RotatedPillarBlock
        | K::InfestedRotatedPillarBlock
        | K::HayBlock
        | K::ChainBlock
        | K::WeatheringCopperChainBlock
        | K::CreakingHeartBlock
        | K::GlazedTerracottaBlock
        | K::LadderBlock
        | K::CarpetBlock
        | K::WoolCarpetBlock
        | K::MossyCarpetBlock
        | K::ScaffoldingBlock
        | K::SnowLayerBlock
        | K::BedBlock
        | K::StrawBedBlock
        | K::ChestBlock
        | K::TrappedChestBlock
        | K::CopperChestBlock
        | K::WeatheringCopperChestBlock
        | K::EnderChestBlock
        | K::ShelfBlock
        | K::WaterloggedTransparentBlock
        | K::WeatheringCopperGrateBlock
        | K::BarrierBlock
        | K::LightBlock
        | K::HeavyCoreBlock
        | K::ConduitBlock
        | K::DecoratedPotBlock
        | K::CopperGolemStatueBlock
        | K::WeatheringCopperGolemStatueBlock
        | K::LanternBlock
        | K::WeatheringLanternBlock
        | K::HugeMushroomBlock
        | K::LeavesBlock
        | K::MangroveLeavesBlock
        | K::TintedParticleLeavesBlock
        | K::UntintedParticleLeavesBlock
        | K::ChorusPlantBlock
        | K::AnvilBlock
        | K::MangroveRootsBlock
        | K::CakeBlock
        | K::FlowerPotBlock
        | K::GrassBlock
        | K::MyceliumBlock
        | K::SnowyBlock
        | K::FarmlandBlock
        | K::PathBlock
        | K::ConcretePowderBlock => Group::Building,

        K::TorchBlock
        | K::WallTorchBlock
        | K::RedstoneTorchBlock
        | K::RedstoneWallTorchBlock
        | K::StandingSignBlock
        | K::WallSignBlock
        | K::CeilingHangingSignBlock
        | K::WallHangingSignBlock
        | K::BannerBlock
        | K::WallBannerBlock
        | K::SkullBlock
        | K::WallSkullBlock
        | K::WitherSkullBlock
        | K::WitherWallSkullBlock
        | K::PlayerHeadBlock
        | K::PlayerWallHeadBlock
        | K::PiglinWallSkullBlock
        | K::ButtonBlock
        | K::LeverBlock
        | K::GrindstoneBlock
        | K::BellBlock
        | K::EndRodBlock
        | K::LightningRodBlock
        | K::WeatheringLightningRodBlock
        | K::AmethystClusterBlock
        | K::CocoaBlock
        | K::TripWireHookBlock
        | K::TripWireBlock
        | K::ShelfMushroomBlock
        | K::PointedDripstoneBlock
        | K::SulfurSpikeBlock
        | K::MultifaceBlock
        | K::GlowLichenBlock
        | K::SculkVeinBlock
        | K::VineBlock
        | K::HangingRootsBlock
        | K::SporeBlossomBlock
        | K::HangingMossBlock
        | K::CaveVinesBlock
        | K::CaveVinesPlantBlock
        | K::WeepingVinesBlock
        | K::WeepingVinesPlantBlock
        | K::TwistingVinesBlock
        | K::TwistingVinesPlantBlock
        | K::KelpBlock
        | K::KelpPlantBlock
        | K::CandleBlock
        | K::CandleCakeBlock
        | K::SeaPickleBlock => Group::Attached,

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
        | K::MangrovePropaguleBlock
        | K::MushroomBlock
        | K::NetherFungusBlock
        | K::NetherRootsBlock
        | K::NetherSproutsBlock
        | K::CropBlock
        | K::CarrotBlock
        | K::PotatoBlock
        | K::BeetrootBlock
        | K::TorchflowerCropBlock
        | K::PitcherCropBlock
        | K::StemBlock
        | K::AttachedStemBlock
        | K::NetherWartBlock
        | K::SweetBerryBushBlock
        | K::DoublePlantBlock
        | K::TallFlowerBlock
        | K::TallSeagrassBlock
        | K::SmallDripleafBlock
        | K::BigDripleafBlock
        | K::BigDripleafStemBlock
        | K::SugarCaneBlock
        | K::CactusBlock
        | K::CactusFlowerBlock
        | K::BambooStalkBlock
        | K::BambooSaplingBlock
        | K::SeagrassBlock
        | K::CoralPlantBlock
        | K::BaseCoralPlantBlock
        | K::CoralFanBlock
        | K::BaseCoralFanBlock
        | K::CoralWallFanBlock
        | K::BaseCoralWallFanBlock
        | K::CoralBlock
        | K::LilyPadBlock
        | K::FrogspawnBlock
        | K::FlowerBedBlock
        | K::LeafLitterBlock
        | K::TurtleEggBlock
        | K::SnifferEggBlock
        | K::ChorusFlowerBlock
        | K::DriedGhastBlock
        | K::PotentSulfurBlock => Group::Plants,

        K::RedstoneWireBlock
        | K::RepeaterBlock
        | K::ComparatorBlock
        | K::ObserverBlock
        | K::PistonBaseBlock
        | K::PistonHeadBlock
        | K::MovingPistonBlock
        | K::DispenserBlock
        | K::DropperBlock
        | K::CrafterBlock
        | K::HopperBlock
        | K::RailBlock
        | K::PoweredRailBlock
        | K::DetectorRailBlock
        | K::PressurePlateBlock
        | K::WeightedPressurePlateBlock
        | K::DaylightDetectorBlock
        | K::TargetBlock
        | K::LecternBlock
        | K::NoteBlock
        | K::RedstoneLampBlock
        | K::CopperBulbBlock
        | K::WeatheringCopperBulbBlock
        | K::TntBlock
        | K::CommandBlock
        | K::StructureBlock
        | K::JigsawBlock
        | K::TestBlock
        | K::TestInstanceBlock
        | K::SculkSensorBlock
        | K::CalibratedSculkSensorBlock
        | K::SculkShriekerBlock
        | K::SculkCatalystBlock
        | K::FurnaceBlock
        | K::BlastFurnaceBlock
        | K::SmokerBlock
        | K::BarrelBlock
        | K::BeehiveBlock
        | K::CampfireBlock
        | K::ChiseledBookShelfBlock
        | K::LoomBlock
        | K::StonecutterBlock
        | K::CarvedPumpkinBlock
        | K::EndPortalFrameBlock
        | K::ShulkerBoxBlock
        | K::VaultBlock
        | K::TrialSpawnerBlock
        | K::BrewingStandBlock
        | K::ComposterBlock
        | K::JukeboxBlock
        | K::RespawnAnchorBlock
        | K::CauldronBlock
        | K::LavaCauldronBlock
        | K::LayeredCauldronBlock
        | K::BrushableBlock
        | K::FireBlock
        | K::SoulFireBlock
        | K::BubbleColumnBlock
        | K::LiquidBlock
        | K::NetherPortalBlock
        | K::EndPortalBlock
        | K::EndGatewayBlock
        | K::FrostedIceBlock => Group::Functional,
    }
}

/// `BlockState.canBeReplaced(BlockPlaceContext)` for `existing` at `ctx.pos`.
pub fn can_be_replaced(existing: BlockState, ctx: &PlaceContext) -> bool {
    match group(existing.kind()) {
        Group::Default => util::default_can_be_replaced(existing, ctx),
        Group::Building => building::can_be_replaced(existing, ctx),
        Group::Attached => attached::can_be_replaced(existing, ctx),
        Group::Plants => plants::can_be_replaced(existing, ctx),
        Group::Functional => functional::can_be_replaced(existing, ctx),
    }
}

/// `Block.getStateForPlacement(context)` of `block` (no survival check).
fn block_state_for_placement(
    block: BlockId,
    ctx: &PlaceContext,
    level: &dyn BlockReader,
) -> Option<BlockState> {
    match group(block.info().kind) {
        Group::Default => Some(util::default_placement(block, ctx, level)),
        Group::Building => building::state_for_placement(block, ctx, level),
        Group::Attached => attached::state_for_placement(block, ctx, level),
        Group::Plants => plants::state_for_placement(block, ctx, level),
        Group::Functional => functional::state_for_placement(block, ctx, level),
    }
}

/// `BlockState.canSurvive(level, pos)`.
/// `Direction.fromYRot(yRot)`: the horizontal direction a player with this yaw faces.
pub fn horizontal_direction_of(y_rot: f32) -> Direction {
    util::from_y_rot(f64::from(y_rot))
}

/// `BaseFireBlock.getState(level, pos)`: soul fire on soul fire bases, else fire facing its
/// flammable neighbours.
pub fn fire_state(level: &dyn BlockReader, pos: BlockPos) -> BlockState {
    functional::base_fire_state(level, pos)
}

pub fn can_survive(state: BlockState, pos: BlockPos, level: &dyn BlockReader) -> bool {
    match group(state.kind()) {
        Group::Default => true,
        Group::Building => building::can_survive(state, pos, level),
        Group::Attached => attached::can_survive(state, pos, level),
        Group::Plants => plants::can_survive(state, pos, level),
        Group::Functional => functional::can_survive(state, pos, level),
    }
}

/// The wall block that a `StandingAndWallBlockItem` places next to its standing block, by block
/// id (`u16::MAX` = none).
fn wall_variants() -> &'static [u16] {
    static TABLE: OnceLock<Vec<u16>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = vec![u16::MAX; BLOCKS.len()];
        for (id, info) in BLOCKS.iter().enumerate() {
            use BlockKind as K;
            let standing = matches!(
                info.kind,
                K::TorchBlock
                    | K::RedstoneTorchBlock
                    | K::CoralFanBlock
                    | K::BaseCoralFanBlock
                    | K::StandingSignBlock
                    | K::CeilingHangingSignBlock
                    | K::BannerBlock
                    | K::SkullBlock
                    | K::WitherSkullBlock
                    | K::PlayerHeadBlock
            );
            if !standing {
                continue;
            }
            let name = info.name.trim_start_matches("minecraft:");
            let wall = if let Some(prefix) = name.strip_suffix("_hanging_sign") {
                format!("{prefix}_wall_hanging_sign")
            } else if name == "torch" {
                "wall_torch".to_owned()
            } else if let Some((prefix, last)) = name.rsplit_once('_') {
                format!("{prefix}_wall_{last}")
            } else {
                continue;
            };
            if let Some(wall) = BlockId::by_name(&format!("minecraft:{wall}")) {
                table[id] = wall.0;
            }
        }
        table
    })
}

/// The wall variant of a standing block placed by a `StandingAndWallBlockItem`.
pub fn wall_variant(block: BlockId) -> Option<BlockId> {
    let wall = *wall_variants().get(usize::from(block.0))?;
    (wall != u16::MAX).then_some(BlockId(wall))
}

/// `BlockItem.updatePlacementContext`: scaffolding placed on scaffolding moves up or sideways
/// (`ScaffoldingBlockItem`); `None` = the item fails. Other items keep the context.
pub fn update_placement_context(
    ctx: &PlaceContext,
    level: &dyn BlockReader,
) -> Option<PlaceContext> {
    let Some(block) = ctx.item.block() else {
        return Some(*ctx);
    };
    if block.info().kind != BlockKind::ScaffoldingBlock {
        return Some(*ctx);
    }
    let pos = ctx.pos;
    let replaced = level.state(pos);
    if !replaced.is(block) {
        return (building::scaffolding_distance(level, pos) != 7).then_some(*ctx);
    }
    let direction = if ctx.sneaking {
        if ctx.inside {
            ctx.face.opposite()
        } else {
            ctx.face
        }
    } else if ctx.face == Direction::Up {
        ctx.horizontal_direction()
    } else {
        Direction::Up
    };
    let mut horizontal = 0;
    let mut placement = pos.relative(direction);
    while horizontal < 7 {
        if !util::in_build_height(level, placement) {
            break;
        }
        let state = level.state(placement);
        if !state.is(block) {
            if can_be_replaced(state, ctx) {
                return Some(ctx.at(placement, direction, level));
            }
            break;
        }
        placement = placement.relative(direction);
        if direction.is_horizontal() {
            horizontal += 1;
        }
    }
    None
}

/// `BlockItem.getPlacementState` for the held item, after `BlockPlaceContext.canPlace`: the
/// block's state for placement (with wall variants for standing-and-wall items) if it can
/// survive there. `None` = the item cannot place here. Entity collision is not checked.
pub fn state_for_placement(ctx: &PlaceContext, level: &dyn BlockReader) -> Option<BlockState> {
    let block = ctx.item.block()?;
    if !ctx.can_place(level) {
        return None;
    }
    if let Some(wall) = wall_variant(block) {
        // `StandingAndWallBlockItem.getPlacementState`.
        let hanging = block.info().kind == BlockKind::CeilingHangingSignBlock;
        let attachment = if hanging {
            Direction::Up
        } else {
            Direction::Down
        };
        let wall_state = block_state_for_placement(wall, ctx, level);
        for direction in ctx.nearest_looking_directions() {
            if direction == attachment.opposite() {
                continue;
            }
            let possible = if direction == attachment {
                block_state_for_placement(block, ctx, level)
            } else {
                wall_state
            };
            let Some(possible) = possible else {
                continue;
            };
            // `HangingSignItem.canPlace`.
            if possible.kind() == BlockKind::WallHangingSignBlock
                && !attached::wall_hanging_sign_can_place(possible, ctx.pos, level)
            {
                continue;
            }
            if can_survive(possible, ctx.pos, level) {
                return Some(possible);
            }
        }
        return None;
    }
    let state = block_state_for_placement(block, ctx, level)?;
    // `ScaffoldingBlockItem.mustSurvive()` is false.
    let must_survive = block.info().kind != BlockKind::ScaffoldingBlock;
    (!must_survive || can_survive(state, ctx.pos, level)).then_some(state)
}

/// Blocks that `setPlacedBy` (and the item) place or change after `state` was set at `pos`: the
/// upper half of doors, double plants and small dripleaves, the head of beds, the hooks and
/// strings of a tripwire that a new hook completes, … `level` already holds `state` at `pos`.
/// Each `(pos, state)` is set by the caller in order with Vanilla's flags (3).
///
/// The pale moss carpet's hanging topper depends on the level random: see
/// [`mossy_carpet_topper`].
pub fn placed_extra(
    state: BlockState,
    pos: BlockPos,
    ctx: &PlaceContext,
    level: &dyn BlockReader,
    out: &mut Vec<(BlockPos, BlockState)>,
) {
    match group(state.kind()) {
        Group::Default => {}
        Group::Building => building::placed_extra(state, pos, ctx, level, out),
        Group::Attached => attached::placed_extra(state, pos, ctx, level, out),
        Group::Plants => plants::placed_extra(state, pos, ctx, level, out),
        Group::Functional => functional::placed_extra(state, pos, ctx, level, out),
    }
}

/// `BaseFireBlock.getState(level, pos)`: soul fire on soul fire bases, else fire with its
/// faces.
pub fn base_fire_state(level: &dyn BlockReader, pos: BlockPos) -> BlockState {
    functional::base_fire_state(level, pos)
}

/// `MossyCarpetBlock.setPlacedBy`: the carpet to set (flags 3) above a pale moss carpet `state`
/// just placed at `pos`, if any. `next_boolean` is the level random's `nextBoolean()`, called
/// once per side that could hang down, in north, east, south, west order.
pub fn mossy_carpet_topper(
    state: BlockState,
    pos: BlockPos,
    level: &dyn BlockReader,
    next_boolean: &mut dyn FnMut() -> bool,
) -> Option<BlockState> {
    if state.kind() != BlockKind::MossyCarpetBlock {
        return None;
    }
    building::mossy_carpet_topper(state, pos, level, next_boolean)
}

/// The parts of `playerWillDestroy` that change other blocks when a player breaks `state` at
/// `pos`: the other half of beds, doors and double plants (`DoublePlantBlock
/// .preventDropFromBottomPart` in creative), piston bases behind heads, … `out` gets the
/// `(pos, new state)` to set (flags 35, as Vanilla, unless noted).
pub fn destroy_extra(
    state: BlockState,
    pos: BlockPos,
    level: &dyn BlockReader,
    creative: bool,
    out: &mut Vec<(BlockPos, BlockState)>,
) {
    match group(state.kind()) {
        Group::Default => {}
        Group::Building => building::destroy_extra(state, pos, level, creative, out),
        Group::Attached => attached::destroy_extra(state, pos, level, creative, out),
        Group::Plants => plants::destroy_extra(state, pos, level, creative, out),
        Group::Functional => functional::destroy_extra(state, pos, level, creative, out),
    }
}

/// Air, for removals.
pub const AIR: BlockState = BlockState::AIR;

/// The water source block state (for halves that leave water behind).
pub fn water() -> BlockState {
    ids::WATER.default_state()
}

//! The Vanilla block class of every block, as far as its generation behaviour goes.
//!
//! Derived from `Blocks.java` (26.3): each block maps to the most derived class in its class
//! hierarchy that overrides `canSurvive`, `mayPlaceOn`, `updateShape`, `rotate`, `mirror` or a
//! helper they call (`canAttachTo`, `getBodyBlock`, …). Blocks whose classes override none of
//! them behave like `Block` ([`Class::Plain`]).

use std::sync::OnceLock;

use vanillite_data::block::{BLOCK_COUNT, BlockId, BlockState};

/// A Vanilla block class with generation relevant behaviour (variants are named after the class
/// without the `Block` suffix).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Class {
    /// `Block`: no behaviour of its own.
    Plain,
    /// `AbstractBedBlock`.
    AbstractBed,
    /// `AbstractFurnaceBlock`.
    AbstractFurnace,
    /// `AmethystClusterBlock`.
    AmethystCluster,
    /// `AnvilBlock`.
    Anvil,
    /// `AttachedStemBlock`.
    AttachedStem,
    /// `AzaleaBlock`.
    Azalea,
    /// `BambooSaplingBlock`.
    BambooSapling,
    /// `BambooStalkBlock`.
    BambooStalk,
    /// `BannerBlock`.
    Banner,
    /// `BarrelBlock`.
    Barrel,
    /// `BarrierBlock`.
    Barrier,
    /// `BaseCoralPlantTypeBlock`.
    BaseCoralPlantType,
    /// `BaseCoralWallFanBlock`.
    BaseCoralWallFan,
    /// `BasePressurePlateBlock`.
    BasePressurePlate,
    /// `BaseTorchBlock`.
    BaseTorch,
    /// `BeehiveBlock`.
    Beehive,
    /// `BellBlock`.
    Bell,
    /// `BigDripleafBlock`.
    BigDripleaf,
    /// `BigDripleafStemBlock`.
    BigDripleafStem,
    /// `BrushableBlock`.
    Brushable,
    /// `BubbleColumnBlock`.
    BubbleColumn,
    /// `CactusBlock`.
    Cactus,
    /// `CactusFlowerBlock`.
    CactusFlower,
    /// `CakeBlock`.
    Cake,
    /// `CalibratedSculkSensorBlock`.
    CalibratedSculkSensor,
    /// `CampfireBlock`.
    Campfire,
    /// `CandleBlock`.
    Candle,
    /// `CandleCakeBlock`.
    CandleCake,
    /// `CarpetBlock`.
    Carpet,
    /// `CaveVinesBlock`.
    CaveVines,
    /// `CaveVinesPlantBlock`.
    CaveVinesPlant,
    /// `CeilingHangingSignBlock`.
    CeilingHangingSign,
    /// `ChainBlock`.
    Chain,
    /// `ChestBlock`.
    Chest,
    /// `ChiseledBookShelfBlock`.
    ChiseledBookShelf,
    /// `ChorusFlowerBlock`.
    ChorusFlower,
    /// `ChorusPlantBlock`.
    ChorusPlant,
    /// `CocoaBlock`.
    Cocoa,
    /// `CommandBlock`.
    Command,
    /// `ComparatorBlock`.
    Comparator,
    /// `ConcretePowderBlock`.
    ConcretePowder,
    /// `ConduitBlock`.
    Conduit,
    /// `CopperChestBlock`.
    CopperChest,
    /// `CopperGolemStatueBlock`.
    CopperGolemStatue,
    /// `CoralBlock`.
    Coral,
    /// `CoralFanBlock`.
    CoralFan,
    /// `CoralPlantBlock`.
    CoralPlant,
    /// `CoralWallFanBlock`.
    CoralWallFan,
    /// `CrafterBlock`.
    Crafter,
    /// `CreakingHeartBlock`.
    CreakingHeart,
    /// `CropBlock`.
    Crop,
    /// `DecoratedPotBlock`.
    DecoratedPot,
    /// `DetectorRailBlock`.
    DetectorRail,
    /// `DispenserBlock`.
    Dispenser,
    /// `DoorBlock`.
    Door,
    /// `DoublePlantBlock`.
    DoublePlant,
    /// `DriedGhastBlock`.
    DriedGhast,
    /// `DryVegetationBlock`.
    DryVegetation,
    /// `EndPortalFrameBlock`.
    EndPortalFrame,
    /// `EnderChestBlock`.
    EnderChest,
    /// `FaceAttachedHorizontalDirectionalBlock`.
    FaceAttachedHorizontalDirectional,
    /// `FallingBlock`.
    Falling,
    /// `FarmlandBlock`.
    Farmland,
    /// `FenceBlock`.
    Fence,
    /// `FenceGateBlock`.
    FenceGate,
    /// `FireBlock`.
    Fire,
    /// `FlowerBedBlock`.
    FlowerBed,
    /// `FlowerPotBlock`.
    FlowerPot,
    /// `FrogspawnBlock`.
    Frogspawn,
    /// `GrindstoneBlock`.
    Grindstone,
    /// `HangingMossBlock`.
    HangingMoss,
    /// `HangingRootsBlock`.
    HangingRoots,
    /// `HeavyCoreBlock`.
    HeavyCore,
    /// `HopperBlock`.
    Hopper,
    /// `HorizontalDirectionalBlock`.
    HorizontalDirectional,
    /// `HugeMushroomBlock`.
    HugeMushroom,
    /// `InfestedRotatedPillarBlock`.
    InfestedRotatedPillar,
    /// `IronBarsBlock`.
    IronBars,
    /// `JigsawBlock`.
    Jigsaw,
    /// `KelpBlock`.
    Kelp,
    /// `KelpPlantBlock`.
    KelpPlant,
    /// `LadderBlock`.
    Ladder,
    /// `LanternBlock`.
    Lantern,
    /// `LeafLitterBlock`.
    LeafLitter,
    /// `LeavesBlock`.
    Leaves,
    /// `LecternBlock`.
    Lectern,
    /// `LightBlock`.
    Light,
    /// `LightningRodBlock`.
    LightningRod,
    /// `LilyPadBlock`.
    LilyPad,
    /// `LiquidBlock`.
    Liquid,
    /// `MangrovePropaguleBlock`.
    MangrovePropagule,
    /// `MangroveRootsBlock`.
    MangroveRoots,
    /// `MossyCarpetBlock`.
    MossyCarpet,
    /// `MovingPistonBlock`.
    MovingPiston,
    /// `MultifaceBlock`.
    Multiface,
    /// `MushroomBlock`.
    Mushroom,
    /// `NetherFungusBlock`.
    NetherFungus,
    /// `NetherPortalBlock`.
    NetherPortal,
    /// `NetherRootsBlock`.
    NetherRoots,
    /// `NetherSproutsBlock`.
    NetherSprouts,
    /// `NetherWartBlock`.
    NetherWart,
    /// `NoteBlock`.
    NoteBlock,
    /// `ObserverBlock`.
    Observer,
    /// `PathBlock`.
    Path,
    /// `PistonBaseBlock`.
    PistonBase,
    /// `PistonHeadBlock`.
    PistonHead,
    /// `PitcherCropBlock`.
    PitcherCrop,
    /// `PotentSulfurBlock`.
    PotentSulfur,
    /// `PoweredRailBlock`.
    PoweredRail,
    /// `RailBlock`.
    Rail,
    /// `RedstoneWallTorchBlock`.
    RedstoneWallTorch,
    /// `RedstoneWireBlock`.
    RedstoneWire,
    /// `RepeaterBlock`.
    Repeater,
    /// `RodBlock`.
    Rod,
    /// `RotatedPillarBlock`.
    RotatedPillar,
    /// `ScaffoldingBlock`.
    Scaffolding,
    /// `SculkSensorBlock`.
    SculkSensor,
    /// `SculkShriekerBlock`.
    SculkShrieker,
    /// `SeaPickleBlock`.
    SeaPickle,
    /// `SeagrassBlock`.
    Seagrass,
    /// `ShelfBlock`.
    Shelf,
    /// `ShelfMushroomBlock`.
    ShelfMushroom,
    /// `ShulkerBoxBlock`.
    ShulkerBox,
    /// `SkullBlock`.
    Skull,
    /// `SlabBlock`.
    Slab,
    /// `SmallDripleafBlock`.
    SmallDripleaf,
    /// `SnowLayerBlock`.
    SnowLayer,
    /// `SnowyBlock`.
    Snowy,
    /// `SoulFireBlock`.
    SoulFire,
    /// `SpeleothemBlock`.
    Speleothem,
    /// `SporeBlossomBlock`.
    SporeBlossom,
    /// `StairBlock`.
    Stair,
    /// `StandingSignBlock`.
    StandingSign,
    /// `StemBlock`.
    Stem,
    /// `StonecutterBlock`.
    Stonecutter,
    /// `SugarCaneBlock`.
    SugarCane,
    /// `TallSeagrassBlock`.
    TallSeagrass,
    /// `TrapDoorBlock`.
    TrapDoor,
    /// `TripWireBlock`.
    TripWire,
    /// `TripWireHookBlock`.
    TripWireHook,
    /// `TwistingVinesBlock`.
    TwistingVines,
    /// `TwistingVinesPlantBlock`.
    TwistingVinesPlant,
    /// `VaultBlock`.
    Vault,
    /// `VegetationBlock`.
    Vegetation,
    /// `VineBlock`.
    Vine,
    /// `WallBannerBlock`.
    WallBanner,
    /// `WallBlock`.
    Wall,
    /// `WallHangingSignBlock`.
    WallHangingSign,
    /// `WallSignBlock`.
    WallSign,
    /// `WallSkullBlock`.
    WallSkull,
    /// `WallTorchBlock`.
    WallTorch,
    /// `WaterloggedTransparentBlock`.
    WaterloggedTransparent,
    /// `WeepingVinesBlock`.
    WeepingVines,
    /// `WeepingVinesPlantBlock`.
    WeepingVinesPlant,
    /// `WitherRoseBlock`.
    WitherRose,
}

/// The blocks of each class, by registry name without the `minecraft:` prefix.
const CLASSES: &[(Class, &[&str])] = &[
    (
        Class::AbstractBed,
        &[
            "white_bed",
            "orange_bed",
            "magenta_bed",
            "light_blue_bed",
            "yellow_bed",
            "lime_bed",
            "pink_bed",
            "gray_bed",
            "light_gray_bed",
            "cyan_bed",
            "purple_bed",
            "blue_bed",
            "brown_bed",
            "green_bed",
            "red_bed",
            "black_bed",
            "straw_bed",
        ],
    ),
    (
        Class::AbstractFurnace,
        &["furnace", "smoker", "blast_furnace"],
    ),
    (
        Class::AmethystCluster,
        &[
            "amethyst_cluster",
            "large_amethyst_bud",
            "medium_amethyst_bud",
            "small_amethyst_bud",
        ],
    ),
    (Class::Anvil, &["anvil", "chipped_anvil", "damaged_anvil"]),
    (
        Class::AttachedStem,
        &["attached_pumpkin_stem", "attached_melon_stem"],
    ),
    (Class::Azalea, &["azalea", "flowering_azalea"]),
    (Class::BambooSapling, &["bamboo_sapling"]),
    (Class::BambooStalk, &["bamboo"]),
    (
        Class::Banner,
        &[
            "white_banner",
            "orange_banner",
            "magenta_banner",
            "light_blue_banner",
            "yellow_banner",
            "lime_banner",
            "pink_banner",
            "gray_banner",
            "light_gray_banner",
            "cyan_banner",
            "purple_banner",
            "blue_banner",
            "brown_banner",
            "green_banner",
            "red_banner",
            "black_banner",
        ],
    ),
    (Class::Barrel, &["barrel"]),
    (Class::Barrier, &["barrier"]),
    (
        Class::BaseCoralPlantType,
        &[
            "dead_tube_coral",
            "dead_brain_coral",
            "dead_bubble_coral",
            "dead_fire_coral",
            "dead_horn_coral",
            "dead_tube_coral_fan",
            "dead_brain_coral_fan",
            "dead_bubble_coral_fan",
            "dead_fire_coral_fan",
            "dead_horn_coral_fan",
        ],
    ),
    (
        Class::BaseCoralWallFan,
        &[
            "dead_tube_coral_wall_fan",
            "dead_brain_coral_wall_fan",
            "dead_bubble_coral_wall_fan",
            "dead_fire_coral_wall_fan",
            "dead_horn_coral_wall_fan",
        ],
    ),
    (
        Class::BasePressurePlate,
        &[
            "stone_pressure_plate",
            "oak_pressure_plate",
            "spruce_pressure_plate",
            "birch_pressure_plate",
            "jungle_pressure_plate",
            "acacia_pressure_plate",
            "cherry_pressure_plate",
            "dark_oak_pressure_plate",
            "pale_oak_pressure_plate",
            "mangrove_pressure_plate",
            "poplar_pressure_plate",
            "bamboo_pressure_plate",
            "light_weighted_pressure_plate",
            "heavy_weighted_pressure_plate",
            "crimson_pressure_plate",
            "warped_pressure_plate",
            "polished_blackstone_pressure_plate",
        ],
    ),
    (
        Class::BaseTorch,
        &["torch", "redstone_torch", "soul_torch", "copper_torch"],
    ),
    (Class::Beehive, &["bee_nest", "beehive"]),
    (Class::Bell, &["bell"]),
    (Class::BigDripleaf, &["big_dripleaf"]),
    (Class::BigDripleafStem, &["big_dripleaf_stem"]),
    (Class::Brushable, &["suspicious_sand", "suspicious_gravel"]),
    (Class::BubbleColumn, &["bubble_column"]),
    (Class::Cactus, &["cactus"]),
    (Class::CactusFlower, &["cactus_flower"]),
    (Class::Cake, &["cake"]),
    (Class::CalibratedSculkSensor, &["calibrated_sculk_sensor"]),
    (Class::Campfire, &["campfire", "soul_campfire"]),
    (
        Class::Candle,
        &[
            "candle",
            "white_candle",
            "orange_candle",
            "magenta_candle",
            "light_blue_candle",
            "yellow_candle",
            "lime_candle",
            "pink_candle",
            "gray_candle",
            "light_gray_candle",
            "cyan_candle",
            "purple_candle",
            "blue_candle",
            "brown_candle",
            "green_candle",
            "red_candle",
            "black_candle",
        ],
    ),
    (
        Class::CandleCake,
        &[
            "candle_cake",
            "white_candle_cake",
            "orange_candle_cake",
            "magenta_candle_cake",
            "light_blue_candle_cake",
            "yellow_candle_cake",
            "lime_candle_cake",
            "pink_candle_cake",
            "gray_candle_cake",
            "light_gray_candle_cake",
            "cyan_candle_cake",
            "purple_candle_cake",
            "blue_candle_cake",
            "brown_candle_cake",
            "green_candle_cake",
            "red_candle_cake",
            "black_candle_cake",
        ],
    ),
    (
        Class::Carpet,
        &[
            "white_carpet",
            "orange_carpet",
            "magenta_carpet",
            "light_blue_carpet",
            "yellow_carpet",
            "lime_carpet",
            "pink_carpet",
            "gray_carpet",
            "light_gray_carpet",
            "cyan_carpet",
            "purple_carpet",
            "blue_carpet",
            "brown_carpet",
            "green_carpet",
            "red_carpet",
            "black_carpet",
            "moss_carpet",
        ],
    ),
    (Class::CaveVines, &["cave_vines"]),
    (Class::CaveVinesPlant, &["cave_vines_plant"]),
    (
        Class::CeilingHangingSign,
        &[
            "oak_hanging_sign",
            "spruce_hanging_sign",
            "birch_hanging_sign",
            "acacia_hanging_sign",
            "cherry_hanging_sign",
            "jungle_hanging_sign",
            "dark_oak_hanging_sign",
            "pale_oak_hanging_sign",
            "crimson_hanging_sign",
            "warped_hanging_sign",
            "mangrove_hanging_sign",
            "poplar_hanging_sign",
            "bamboo_hanging_sign",
        ],
    ),
    (
        Class::Chain,
        &[
            "iron_chain",
            "copper_chain",
            "exposed_copper_chain",
            "weathered_copper_chain",
            "oxidized_copper_chain",
            "waxed_copper_chain",
            "waxed_exposed_copper_chain",
            "waxed_weathered_copper_chain",
            "waxed_oxidized_copper_chain",
        ],
    ),
    (Class::Chest, &["chest", "trapped_chest"]),
    (Class::ChiseledBookShelf, &["chiseled_bookshelf"]),
    (Class::ChorusFlower, &["chorus_flower"]),
    (Class::ChorusPlant, &["chorus_plant"]),
    (Class::Cocoa, &["cocoa"]),
    (
        Class::Command,
        &[
            "command_block",
            "repeating_command_block",
            "chain_command_block",
        ],
    ),
    (Class::Comparator, &["comparator"]),
    (
        Class::ConcretePowder,
        &[
            "white_concrete_powder",
            "orange_concrete_powder",
            "magenta_concrete_powder",
            "light_blue_concrete_powder",
            "yellow_concrete_powder",
            "lime_concrete_powder",
            "pink_concrete_powder",
            "gray_concrete_powder",
            "light_gray_concrete_powder",
            "cyan_concrete_powder",
            "purple_concrete_powder",
            "blue_concrete_powder",
            "brown_concrete_powder",
            "green_concrete_powder",
            "red_concrete_powder",
            "black_concrete_powder",
        ],
    ),
    (Class::Conduit, &["conduit"]),
    (
        Class::CopperChest,
        &[
            "copper_chest",
            "exposed_copper_chest",
            "weathered_copper_chest",
            "oxidized_copper_chest",
            "waxed_copper_chest",
            "waxed_exposed_copper_chest",
            "waxed_weathered_copper_chest",
            "waxed_oxidized_copper_chest",
        ],
    ),
    (
        Class::CopperGolemStatue,
        &[
            "copper_golem_statue",
            "exposed_copper_golem_statue",
            "weathered_copper_golem_statue",
            "oxidized_copper_golem_statue",
            "waxed_copper_golem_statue",
            "waxed_exposed_copper_golem_statue",
            "waxed_weathered_copper_golem_statue",
            "waxed_oxidized_copper_golem_statue",
        ],
    ),
    (
        Class::Coral,
        &[
            "tube_coral_block",
            "brain_coral_block",
            "bubble_coral_block",
            "fire_coral_block",
            "horn_coral_block",
        ],
    ),
    (
        Class::CoralFan,
        &[
            "tube_coral_fan",
            "brain_coral_fan",
            "bubble_coral_fan",
            "fire_coral_fan",
            "horn_coral_fan",
        ],
    ),
    (
        Class::CoralPlant,
        &[
            "tube_coral",
            "brain_coral",
            "bubble_coral",
            "fire_coral",
            "horn_coral",
        ],
    ),
    (
        Class::CoralWallFan,
        &[
            "tube_coral_wall_fan",
            "brain_coral_wall_fan",
            "bubble_coral_wall_fan",
            "fire_coral_wall_fan",
            "horn_coral_wall_fan",
        ],
    ),
    (Class::Crafter, &["crafter"]),
    (Class::CreakingHeart, &["creaking_heart"]),
    (
        Class::Crop,
        &[
            "wheat",
            "carrots",
            "potatoes",
            "torchflower_crop",
            "beetroots",
        ],
    ),
    (Class::DecoratedPot, &["decorated_pot"]),
    (Class::DetectorRail, &["detector_rail"]),
    (Class::Dispenser, &["dispenser", "dropper"]),
    (
        Class::Door,
        &[
            "oak_door",
            "iron_door",
            "spruce_door",
            "birch_door",
            "jungle_door",
            "acacia_door",
            "cherry_door",
            "dark_oak_door",
            "pale_oak_door",
            "mangrove_door",
            "poplar_door",
            "bamboo_door",
            "crimson_door",
            "warped_door",
            "copper_door",
            "exposed_copper_door",
            "weathered_copper_door",
            "oxidized_copper_door",
            "waxed_copper_door",
            "waxed_exposed_copper_door",
            "waxed_weathered_copper_door",
            "waxed_oxidized_copper_door",
        ],
    ),
    (
        Class::DoublePlant,
        &[
            "sunflower",
            "lilac",
            "rose_bush",
            "peony",
            "tall_grass",
            "large_fern",
            "pitcher_plant",
        ],
    ),
    (Class::DriedGhast, &["dried_ghast"]),
    (
        Class::DryVegetation,
        &["dead_bush", "short_dry_grass", "tall_dry_grass"],
    ),
    (Class::EndPortalFrame, &["end_portal_frame"]),
    (Class::EnderChest, &["ender_chest"]),
    (
        Class::FaceAttachedHorizontalDirectional,
        &[
            "lever",
            "stone_button",
            "oak_button",
            "spruce_button",
            "birch_button",
            "jungle_button",
            "acacia_button",
            "cherry_button",
            "dark_oak_button",
            "pale_oak_button",
            "mangrove_button",
            "poplar_button",
            "bamboo_button",
            "crimson_button",
            "warped_button",
            "polished_blackstone_button",
        ],
    ),
    (
        Class::Falling,
        &["sand", "red_sand", "gravel", "dragon_egg"],
    ),
    (Class::Farmland, &["farmland"]),
    (
        Class::Fence,
        &[
            "oak_fence",
            "nether_brick_fence",
            "spruce_fence",
            "birch_fence",
            "jungle_fence",
            "acacia_fence",
            "cherry_fence",
            "dark_oak_fence",
            "pale_oak_fence",
            "mangrove_fence",
            "poplar_fence",
            "bamboo_fence",
            "crimson_fence",
            "warped_fence",
        ],
    ),
    (
        Class::FenceGate,
        &[
            "oak_fence_gate",
            "spruce_fence_gate",
            "birch_fence_gate",
            "jungle_fence_gate",
            "acacia_fence_gate",
            "cherry_fence_gate",
            "dark_oak_fence_gate",
            "pale_oak_fence_gate",
            "mangrove_fence_gate",
            "poplar_fence_gate",
            "bamboo_fence_gate",
            "crimson_fence_gate",
            "warped_fence_gate",
        ],
    ),
    (Class::Fire, &["fire"]),
    (Class::FlowerBed, &["pink_petals", "wildflowers"]),
    (
        Class::FlowerPot,
        &[
            "flower_pot",
            "potted_torchflower",
            "potted_oak_sapling",
            "potted_spruce_sapling",
            "potted_birch_sapling",
            "potted_jungle_sapling",
            "potted_acacia_sapling",
            "potted_cherry_sapling",
            "potted_dark_oak_sapling",
            "potted_pale_oak_sapling",
            "potted_poplar_sapling",
            "potted_mangrove_propagule",
            "potted_fern",
            "potted_dandelion",
            "potted_golden_dandelion",
            "potted_poppy",
            "potted_blue_orchid",
            "potted_allium",
            "potted_azure_bluet",
            "potted_red_tulip",
            "potted_orange_tulip",
            "potted_white_tulip",
            "potted_pink_tulip",
            "potted_oxeye_daisy",
            "potted_cornflower",
            "potted_lily_of_the_valley",
            "potted_wither_rose",
            "potted_red_mushroom",
            "potted_brown_mushroom",
            "potted_dead_bush",
            "potted_cactus",
            "potted_bamboo",
            "potted_crimson_fungus",
            "potted_warped_fungus",
            "potted_crimson_roots",
            "potted_warped_roots",
            "potted_azalea_bush",
            "potted_flowering_azalea_bush",
            "potted_open_eyeblossom",
            "potted_closed_eyeblossom",
        ],
    ),
    (Class::Frogspawn, &["frogspawn"]),
    (Class::Grindstone, &["grindstone"]),
    (Class::HangingMoss, &["pale_hanging_moss"]),
    (Class::HangingRoots, &["hanging_roots"]),
    (Class::HeavyCore, &["heavy_core"]),
    (Class::Hopper, &["hopper"]),
    (
        Class::HorizontalDirectional,
        &[
            "carved_pumpkin",
            "jack_o_lantern",
            "white_glazed_terracotta",
            "orange_glazed_terracotta",
            "magenta_glazed_terracotta",
            "light_blue_glazed_terracotta",
            "yellow_glazed_terracotta",
            "lime_glazed_terracotta",
            "pink_glazed_terracotta",
            "gray_glazed_terracotta",
            "light_gray_glazed_terracotta",
            "cyan_glazed_terracotta",
            "purple_glazed_terracotta",
            "blue_glazed_terracotta",
            "brown_glazed_terracotta",
            "green_glazed_terracotta",
            "red_glazed_terracotta",
            "black_glazed_terracotta",
            "loom",
        ],
    ),
    (
        Class::HugeMushroom,
        &[
            "brown_mushroom_block",
            "red_mushroom_block",
            "mushroom_stem",
        ],
    ),
    (Class::InfestedRotatedPillar, &["infested_deepslate"]),
    (
        Class::IronBars,
        &[
            "iron_bars",
            "copper_bars",
            "exposed_copper_bars",
            "weathered_copper_bars",
            "oxidized_copper_bars",
            "waxed_copper_bars",
            "waxed_exposed_copper_bars",
            "waxed_weathered_copper_bars",
            "waxed_oxidized_copper_bars",
            "glass_pane",
            "white_stained_glass_pane",
            "orange_stained_glass_pane",
            "magenta_stained_glass_pane",
            "light_blue_stained_glass_pane",
            "yellow_stained_glass_pane",
            "lime_stained_glass_pane",
            "pink_stained_glass_pane",
            "gray_stained_glass_pane",
            "light_gray_stained_glass_pane",
            "cyan_stained_glass_pane",
            "purple_stained_glass_pane",
            "blue_stained_glass_pane",
            "brown_stained_glass_pane",
            "green_stained_glass_pane",
            "red_stained_glass_pane",
            "black_stained_glass_pane",
        ],
    ),
    (Class::Jigsaw, &["jigsaw"]),
    (Class::Kelp, &["kelp"]),
    (Class::KelpPlant, &["kelp_plant"]),
    (Class::Ladder, &["ladder"]),
    (
        Class::Lantern,
        &[
            "lantern",
            "soul_lantern",
            "copper_lantern",
            "exposed_copper_lantern",
            "weathered_copper_lantern",
            "oxidized_copper_lantern",
            "waxed_copper_lantern",
            "waxed_exposed_copper_lantern",
            "waxed_weathered_copper_lantern",
            "waxed_oxidized_copper_lantern",
        ],
    ),
    (Class::LeafLitter, &["leaf_litter"]),
    (
        Class::Leaves,
        &[
            "oak_leaves",
            "spruce_leaves",
            "birch_leaves",
            "jungle_leaves",
            "acacia_leaves",
            "cherry_leaves",
            "dark_oak_leaves",
            "pale_oak_leaves",
            "mangrove_leaves",
            "red_poplar_leaves",
            "orange_poplar_leaves",
            "yellow_poplar_leaves",
            "azalea_leaves",
            "flowering_azalea_leaves",
        ],
    ),
    (Class::Lectern, &["lectern"]),
    (Class::Light, &["light"]),
    (
        Class::LightningRod,
        &[
            "lightning_rod",
            "exposed_lightning_rod",
            "weathered_lightning_rod",
            "oxidized_lightning_rod",
            "waxed_lightning_rod",
            "waxed_exposed_lightning_rod",
            "waxed_weathered_lightning_rod",
            "waxed_oxidized_lightning_rod",
        ],
    ),
    (Class::LilyPad, &["lily_pad"]),
    (Class::Liquid, &["water", "lava"]),
    (Class::MangrovePropagule, &["mangrove_propagule"]),
    (Class::MangroveRoots, &["mangrove_roots"]),
    (Class::MossyCarpet, &["pale_moss_carpet"]),
    (Class::MovingPiston, &["moving_piston"]),
    (
        Class::Multiface,
        &["glow_lichen", "resin_clump", "sculk_vein"],
    ),
    (Class::Mushroom, &["brown_mushroom", "red_mushroom"]),
    (Class::NetherFungus, &["warped_fungus", "crimson_fungus"]),
    (Class::NetherPortal, &["nether_portal"]),
    (Class::NetherRoots, &["warped_roots", "crimson_roots"]),
    (Class::NetherSprouts, &["nether_sprouts"]),
    (Class::NetherWart, &["nether_wart"]),
    (Class::NoteBlock, &["note_block"]),
    (Class::Observer, &["observer"]),
    (Class::Path, &["dirt_path"]),
    (Class::PistonBase, &["sticky_piston", "piston"]),
    (Class::PistonHead, &["piston_head"]),
    (Class::PitcherCrop, &["pitcher_crop"]),
    (Class::PotentSulfur, &["potent_sulfur"]),
    (Class::PoweredRail, &["powered_rail", "activator_rail"]),
    (Class::Rail, &["rail"]),
    (Class::RedstoneWallTorch, &["redstone_wall_torch"]),
    (Class::RedstoneWire, &["redstone_wire"]),
    (Class::Repeater, &["repeater"]),
    (Class::Rod, &["end_rod"]),
    (
        Class::RotatedPillar,
        &[
            "pale_oak_wood",
            "oak_log",
            "spruce_log",
            "birch_log",
            "jungle_log",
            "acacia_log",
            "cherry_log",
            "dark_oak_log",
            "pale_oak_log",
            "mangrove_log",
            "poplar_log",
            "muddy_mangrove_roots",
            "bamboo_block",
            "stripped_spruce_log",
            "stripped_birch_log",
            "stripped_jungle_log",
            "stripped_acacia_log",
            "stripped_cherry_log",
            "stripped_dark_oak_log",
            "stripped_pale_oak_log",
            "stripped_oak_log",
            "stripped_mangrove_log",
            "stripped_poplar_log",
            "stripped_bamboo_block",
            "oak_wood",
            "spruce_wood",
            "birch_wood",
            "jungle_wood",
            "acacia_wood",
            "cherry_wood",
            "dark_oak_wood",
            "mangrove_wood",
            "poplar_wood",
            "stripped_oak_wood",
            "stripped_spruce_wood",
            "stripped_birch_wood",
            "stripped_jungle_wood",
            "stripped_acacia_wood",
            "stripped_cherry_wood",
            "stripped_dark_oak_wood",
            "stripped_pale_oak_wood",
            "stripped_mangrove_wood",
            "stripped_poplar_wood",
            "basalt",
            "polished_basalt",
            "quartz_pillar",
            "hay_block",
            "purpur_pillar",
            "bone_block",
            "warped_stem",
            "stripped_warped_stem",
            "warped_hyphae",
            "stripped_warped_hyphae",
            "crimson_stem",
            "stripped_crimson_stem",
            "crimson_hyphae",
            "stripped_crimson_hyphae",
            "deepslate",
            "ochre_froglight",
            "verdant_froglight",
            "pearlescent_froglight",
        ],
    ),
    (Class::Scaffolding, &["scaffolding"]),
    (Class::SculkSensor, &["sculk_sensor"]),
    (Class::SculkShrieker, &["sculk_shrieker"]),
    (Class::SeaPickle, &["sea_pickle"]),
    (Class::Seagrass, &["seagrass"]),
    (
        Class::Shelf,
        &[
            "acacia_shelf",
            "bamboo_shelf",
            "birch_shelf",
            "cherry_shelf",
            "crimson_shelf",
            "dark_oak_shelf",
            "jungle_shelf",
            "mangrove_shelf",
            "oak_shelf",
            "pale_oak_shelf",
            "poplar_shelf",
            "spruce_shelf",
            "warped_shelf",
        ],
    ),
    (Class::ShelfMushroom, &["shelf_mushroom"]),
    (
        Class::ShulkerBox,
        &[
            "shulker_box",
            "white_shulker_box",
            "orange_shulker_box",
            "magenta_shulker_box",
            "light_blue_shulker_box",
            "yellow_shulker_box",
            "lime_shulker_box",
            "pink_shulker_box",
            "gray_shulker_box",
            "light_gray_shulker_box",
            "cyan_shulker_box",
            "purple_shulker_box",
            "blue_shulker_box",
            "brown_shulker_box",
            "green_shulker_box",
            "red_shulker_box",
            "black_shulker_box",
        ],
    ),
    (
        Class::Skull,
        &[
            "skeleton_skull",
            "wither_skeleton_skull",
            "zombie_head",
            "player_head",
            "creeper_head",
            "dragon_head",
            "piglin_head",
        ],
    ),
    (
        Class::Slab,
        &[
            "white_wool_slab",
            "orange_wool_slab",
            "magenta_wool_slab",
            "light_blue_wool_slab",
            "yellow_wool_slab",
            "lime_wool_slab",
            "pink_wool_slab",
            "gray_wool_slab",
            "light_gray_wool_slab",
            "cyan_wool_slab",
            "purple_wool_slab",
            "blue_wool_slab",
            "brown_wool_slab",
            "green_wool_slab",
            "red_wool_slab",
            "black_wool_slab",
            "resin_brick_slab",
            "prismarine_slab",
            "prismarine_brick_slab",
            "dark_prismarine_slab",
            "oak_slab",
            "spruce_slab",
            "birch_slab",
            "jungle_slab",
            "acacia_slab",
            "cherry_slab",
            "dark_oak_slab",
            "pale_oak_slab",
            "mangrove_slab",
            "poplar_slab",
            "bamboo_slab",
            "bamboo_mosaic_slab",
            "stone_slab",
            "sandstone_slab",
            "cut_sandstone_slab",
            "petrified_oak_slab",
            "cobblestone_slab",
            "brick_slab",
            "stone_brick_slab",
            "mud_brick_slab",
            "nether_brick_slab",
            "quartz_slab",
            "red_sandstone_slab",
            "cut_red_sandstone_slab",
            "smooth_stone_slab",
            "purpur_slab",
            "white_concrete_slab",
            "orange_concrete_slab",
            "magenta_concrete_slab",
            "light_blue_concrete_slab",
            "yellow_concrete_slab",
            "lime_concrete_slab",
            "pink_concrete_slab",
            "gray_concrete_slab",
            "light_gray_concrete_slab",
            "cyan_concrete_slab",
            "purple_concrete_slab",
            "blue_concrete_slab",
            "brown_concrete_slab",
            "green_concrete_slab",
            "red_concrete_slab",
            "black_concrete_slab",
            "polished_granite_slab",
            "smooth_red_sandstone_slab",
            "mossy_stone_brick_slab",
            "polished_diorite_slab",
            "mossy_cobblestone_slab",
            "end_stone_brick_slab",
            "smooth_sandstone_slab",
            "smooth_quartz_slab",
            "granite_slab",
            "andesite_slab",
            "red_nether_brick_slab",
            "polished_andesite_slab",
            "diorite_slab",
            "crimson_slab",
            "warped_slab",
            "blackstone_slab",
            "polished_blackstone_brick_slab",
            "polished_blackstone_slab",
            "tuff_slab",
            "polished_tuff_slab",
            "tuff_brick_slab",
            "sulfur_slab",
            "polished_sulfur_slab",
            "sulfur_brick_slab",
            "cinnabar_slab",
            "polished_cinnabar_slab",
            "cinnabar_brick_slab",
            "cut_copper_slab",
            "exposed_cut_copper_slab",
            "weathered_cut_copper_slab",
            "oxidized_cut_copper_slab",
            "waxed_cut_copper_slab",
            "waxed_exposed_cut_copper_slab",
            "waxed_weathered_cut_copper_slab",
            "waxed_oxidized_cut_copper_slab",
            "cobbled_deepslate_slab",
            "polished_deepslate_slab",
            "deepslate_tile_slab",
            "deepslate_brick_slab",
        ],
    ),
    (Class::SmallDripleaf, &["small_dripleaf"]),
    (Class::SnowLayer, &["snow"]),
    (Class::Snowy, &["grass_block", "podzol", "mycelium"]),
    (Class::SoulFire, &["soul_fire"]),
    (Class::Speleothem, &["pointed_dripstone", "sulfur_spike"]),
    (Class::SporeBlossom, &["spore_blossom"]),
    (
        Class::Stair,
        &[
            "white_wool_stairs",
            "orange_wool_stairs",
            "magenta_wool_stairs",
            "light_blue_wool_stairs",
            "yellow_wool_stairs",
            "lime_wool_stairs",
            "pink_wool_stairs",
            "gray_wool_stairs",
            "light_gray_wool_stairs",
            "cyan_wool_stairs",
            "purple_wool_stairs",
            "blue_wool_stairs",
            "brown_wool_stairs",
            "green_wool_stairs",
            "red_wool_stairs",
            "black_wool_stairs",
            "oak_stairs",
            "cobblestone_stairs",
            "brick_stairs",
            "stone_brick_stairs",
            "mud_brick_stairs",
            "resin_brick_stairs",
            "nether_brick_stairs",
            "sandstone_stairs",
            "spruce_stairs",
            "birch_stairs",
            "jungle_stairs",
            "quartz_stairs",
            "acacia_stairs",
            "cherry_stairs",
            "dark_oak_stairs",
            "pale_oak_stairs",
            "mangrove_stairs",
            "poplar_stairs",
            "bamboo_stairs",
            "bamboo_mosaic_stairs",
            "prismarine_stairs",
            "prismarine_brick_stairs",
            "dark_prismarine_stairs",
            "red_sandstone_stairs",
            "purpur_stairs",
            "white_concrete_stairs",
            "orange_concrete_stairs",
            "magenta_concrete_stairs",
            "light_blue_concrete_stairs",
            "yellow_concrete_stairs",
            "lime_concrete_stairs",
            "pink_concrete_stairs",
            "gray_concrete_stairs",
            "light_gray_concrete_stairs",
            "cyan_concrete_stairs",
            "purple_concrete_stairs",
            "blue_concrete_stairs",
            "brown_concrete_stairs",
            "green_concrete_stairs",
            "red_concrete_stairs",
            "black_concrete_stairs",
            "polished_granite_stairs",
            "smooth_red_sandstone_stairs",
            "mossy_stone_brick_stairs",
            "polished_diorite_stairs",
            "mossy_cobblestone_stairs",
            "end_stone_brick_stairs",
            "stone_stairs",
            "smooth_sandstone_stairs",
            "smooth_quartz_stairs",
            "granite_stairs",
            "andesite_stairs",
            "red_nether_brick_stairs",
            "polished_andesite_stairs",
            "diorite_stairs",
            "crimson_stairs",
            "warped_stairs",
            "blackstone_stairs",
            "polished_blackstone_brick_stairs",
            "polished_blackstone_stairs",
            "tuff_stairs",
            "polished_tuff_stairs",
            "tuff_brick_stairs",
            "sulfur_stairs",
            "polished_sulfur_stairs",
            "sulfur_brick_stairs",
            "cinnabar_stairs",
            "polished_cinnabar_stairs",
            "cinnabar_brick_stairs",
            "cut_copper_stairs",
            "exposed_cut_copper_stairs",
            "weathered_cut_copper_stairs",
            "oxidized_cut_copper_stairs",
            "waxed_cut_copper_stairs",
            "waxed_exposed_cut_copper_stairs",
            "waxed_weathered_cut_copper_stairs",
            "waxed_oxidized_cut_copper_stairs",
            "cobbled_deepslate_stairs",
            "polished_deepslate_stairs",
            "deepslate_tile_stairs",
            "deepslate_brick_stairs",
        ],
    ),
    (
        Class::StandingSign,
        &[
            "oak_sign",
            "spruce_sign",
            "birch_sign",
            "acacia_sign",
            "cherry_sign",
            "jungle_sign",
            "dark_oak_sign",
            "pale_oak_sign",
            "mangrove_sign",
            "poplar_sign",
            "bamboo_sign",
            "crimson_sign",
            "warped_sign",
        ],
    ),
    (Class::Stem, &["pumpkin_stem", "melon_stem"]),
    (Class::Stonecutter, &["stonecutter"]),
    (Class::SugarCane, &["sugar_cane"]),
    (Class::TallSeagrass, &["tall_seagrass"]),
    (
        Class::TrapDoor,
        &[
            "oak_trapdoor",
            "spruce_trapdoor",
            "birch_trapdoor",
            "jungle_trapdoor",
            "acacia_trapdoor",
            "cherry_trapdoor",
            "dark_oak_trapdoor",
            "pale_oak_trapdoor",
            "mangrove_trapdoor",
            "poplar_trapdoor",
            "bamboo_trapdoor",
            "iron_trapdoor",
            "crimson_trapdoor",
            "warped_trapdoor",
            "copper_trapdoor",
            "exposed_copper_trapdoor",
            "weathered_copper_trapdoor",
            "oxidized_copper_trapdoor",
            "waxed_copper_trapdoor",
            "waxed_exposed_copper_trapdoor",
            "waxed_weathered_copper_trapdoor",
            "waxed_oxidized_copper_trapdoor",
        ],
    ),
    (Class::TripWire, &["tripwire"]),
    (Class::TripWireHook, &["tripwire_hook"]),
    (Class::TwistingVines, &["twisting_vines"]),
    (Class::TwistingVinesPlant, &["twisting_vines_plant"]),
    (Class::Vault, &["vault"]),
    (
        Class::Vegetation,
        &[
            "oak_sapling",
            "spruce_sapling",
            "birch_sapling",
            "jungle_sapling",
            "acacia_sapling",
            "cherry_sapling",
            "dark_oak_sapling",
            "pale_oak_sapling",
            "poplar_sapling",
            "short_grass",
            "fern",
            "bush",
            "red_shrub",
            "dandelion",
            "golden_dandelion",
            "torchflower",
            "poppy",
            "blue_orchid",
            "allium",
            "azure_bluet",
            "red_tulip",
            "orange_tulip",
            "white_tulip",
            "pink_tulip",
            "oxeye_daisy",
            "cornflower",
            "lily_of_the_valley",
            "sweet_berry_bush",
            "open_eyeblossom",
            "closed_eyeblossom",
            "firefly_bush",
        ],
    ),
    (Class::Vine, &["vine"]),
    (
        Class::WallBanner,
        &[
            "white_wall_banner",
            "orange_wall_banner",
            "magenta_wall_banner",
            "light_blue_wall_banner",
            "yellow_wall_banner",
            "lime_wall_banner",
            "pink_wall_banner",
            "gray_wall_banner",
            "light_gray_wall_banner",
            "cyan_wall_banner",
            "purple_wall_banner",
            "blue_wall_banner",
            "brown_wall_banner",
            "green_wall_banner",
            "red_wall_banner",
            "black_wall_banner",
        ],
    ),
    (
        Class::Wall,
        &[
            "resin_brick_wall",
            "cobblestone_wall",
            "mossy_cobblestone_wall",
            "brick_wall",
            "prismarine_wall",
            "red_sandstone_wall",
            "mossy_stone_brick_wall",
            "granite_wall",
            "stone_brick_wall",
            "mud_brick_wall",
            "nether_brick_wall",
            "andesite_wall",
            "red_nether_brick_wall",
            "sandstone_wall",
            "end_stone_brick_wall",
            "diorite_wall",
            "blackstone_wall",
            "polished_blackstone_brick_wall",
            "polished_blackstone_wall",
            "tuff_wall",
            "polished_tuff_wall",
            "tuff_brick_wall",
            "sulfur_wall",
            "polished_sulfur_wall",
            "sulfur_brick_wall",
            "cinnabar_wall",
            "polished_cinnabar_wall",
            "cinnabar_brick_wall",
            "cobbled_deepslate_wall",
            "polished_deepslate_wall",
            "deepslate_tile_wall",
            "deepslate_brick_wall",
        ],
    ),
    (
        Class::WallHangingSign,
        &[
            "oak_wall_hanging_sign",
            "spruce_wall_hanging_sign",
            "birch_wall_hanging_sign",
            "acacia_wall_hanging_sign",
            "cherry_wall_hanging_sign",
            "jungle_wall_hanging_sign",
            "dark_oak_wall_hanging_sign",
            "pale_oak_wall_hanging_sign",
            "mangrove_wall_hanging_sign",
            "poplar_wall_hanging_sign",
            "crimson_wall_hanging_sign",
            "warped_wall_hanging_sign",
            "bamboo_wall_hanging_sign",
        ],
    ),
    (
        Class::WallSign,
        &[
            "oak_wall_sign",
            "spruce_wall_sign",
            "birch_wall_sign",
            "acacia_wall_sign",
            "cherry_wall_sign",
            "jungle_wall_sign",
            "dark_oak_wall_sign",
            "pale_oak_wall_sign",
            "mangrove_wall_sign",
            "poplar_wall_sign",
            "bamboo_wall_sign",
            "crimson_wall_sign",
            "warped_wall_sign",
        ],
    ),
    (
        Class::WallSkull,
        &[
            "skeleton_wall_skull",
            "wither_skeleton_wall_skull",
            "zombie_wall_head",
            "player_wall_head",
            "creeper_wall_head",
            "dragon_wall_head",
            "piglin_wall_head",
        ],
    ),
    (
        Class::WallTorch,
        &["wall_torch", "soul_wall_torch", "copper_wall_torch"],
    ),
    (
        Class::WaterloggedTransparent,
        &[
            "copper_grate",
            "exposed_copper_grate",
            "weathered_copper_grate",
            "oxidized_copper_grate",
            "waxed_copper_grate",
            "waxed_exposed_copper_grate",
            "waxed_weathered_copper_grate",
            "waxed_oxidized_copper_grate",
        ],
    ),
    (Class::WeepingVines, &["weeping_vines"]),
    (Class::WeepingVinesPlant, &["weeping_vines_plant"]),
    (Class::WitherRose, &["wither_rose"]),
];

/// The class of every block id, built on first use.
fn table() -> &'static [Class] {
    static TABLE: OnceLock<Vec<Class>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = vec![Class::Plain; BLOCK_COUNT];
        let mut name = String::new();
        for &(class, blocks) in CLASSES {
            for block in blocks {
                name.clear();
                name.push_str("minecraft:");
                name.push_str(block);
                if let Some(id) = BlockId::by_name(&name) {
                    table[usize::from(id.0)] = class;
                }
            }
        }
        table
    })
}

/// The class of a block.
#[inline]
pub(crate) fn class_of_block(block: BlockId) -> Class {
    table()
        .get(usize::from(block.0))
        .copied()
        .unwrap_or(Class::Plain)
}

/// The class of a state's block.
#[inline]
pub(crate) fn class_of(state: BlockState) -> Class {
    class_of_block(state.block())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listed_block_exists() {
        for &(_, blocks) in CLASSES {
            for block in blocks {
                assert!(
                    BlockId::by_name(&format!("minecraft:{block}")).is_some(),
                    "unknown block {block}"
                );
            }
        }
    }
}

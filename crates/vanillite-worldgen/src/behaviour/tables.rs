//! Lookup tables of block behaviour: the block and fluid tags it reads (`BlockTags`,
//! `FluidTags`), resolved once from the loaded data packs, and `FireBlock`'s flammable blocks.

use std::sync::OnceLock;

use vanillite_data::block::{BlockId, BlockState};
use vanillite_data::fluid::FluidState;
use vanillite_datapack::LoadedPacks;

use crate::blocks::BlockSet;

macro_rules! block_tags {
    ($($field:ident),* $(,)?) => {
        /// The block tags behaviour reads, each named after its tag (`minecraft:<field>`).
        #[derive(Debug, Default)]
        pub struct BlockTags {
            $(pub $field: BlockSet,)*
        }

        impl BlockTags {
            fn load(packs: &LoadedPacks) -> Self {
                Self {
                    $($field: BlockSet::from_tag(packs, concat!("minecraft:", stringify!($field))),)*
                }
            }
        }
    };
}

block_tags!(
    blocks_lava_fire_spread,
    campfires,
    candle_cakes,
    candles,
    cannot_support_kelp,
    cannot_support_seagrass,
    cannot_support_snow_layer,
    causes_continuous_geyser_eruptions,
    causes_periodic_geyser_eruptions,
    copper_chests,
    dampens_vibrations,
    enables_bubble_column_drag_down,
    enables_bubble_column_push_up,
    fences,
    fire,
    grows_crops,
    infiniburn_end,
    infiniburn_nether,
    infiniburn_overworld,
    maintains_farmland,
    nether_portal_frame,
    occludes_vibration_signals,
    overrides_mushroom_light_requirement,
    prevents_nearby_leaf_decay,
    saplings,
    shulker_boxes,
    snow,
    soul_fire_base_blocks,
    speleothems,
    support_override_cactus_flower,
    support_override_snow_layer,
    supports_azalea,
    supports_bamboo,
    supports_big_dripleaf,
    supports_cactus,
    supports_chorus_flower,
    supports_chorus_plant,
    supports_cocoa,
    supports_crimson_fungus,
    supports_crimson_roots,
    supports_crops,
    supports_dry_vegetation,
    supports_frogspawn,
    supports_hanging_mangrove_propagule,
    supports_lily_pad,
    supports_mangrove_propagule,
    supports_melon_stem,
    supports_melon_stem_fruit,
    supports_nether_sprouts,
    supports_nether_wart,
    supports_pumpkin_stem,
    supports_pumpkin_stem_fruit,
    supports_small_dripleaf,
    supports_sugar_cane,
    supports_sugar_cane_adjacently,
    supports_vegetation,
    supports_warped_fungus,
    supports_warped_roots,
    supports_wither_rose,
    suppresses_bounce,
    unstable_bottom_center,
    vibration_resonators,
    wall_post_override,
    walls,
    washed_away_by_fluids,
    wooden_fences,
);

/// A set of fluids (a resolved fluid tag), one bit per `minecraft:fluid` id.
#[derive(Clone, Copy, Debug, Default)]
pub struct FluidSet(u64);

impl FluidSet {
    fn from_tag(packs: &LoadedPacks, tag: &str) -> Self {
        let registry = vanillite_data::registry::Registry::by_name("minecraft:fluid");
        let mut bits = 0;
        if let (Some(registry), Some(entries)) = (
            registry,
            packs.tags("minecraft:fluid").and_then(|t| t.get(tag)),
        ) {
            for entry in entries {
                if let Some(id) = registry.id(entry).filter(|&id| id < 64) {
                    bits |= 1 << id;
                }
            }
        }
        Self(bits)
    }

    /// `fluidState.is(tag)`.
    #[inline]
    pub fn contains(self, fluid: FluidState) -> bool {
        let id = fluid.info().fluid;
        id < 64 && self.0 & (1 << id) != 0
    }
}

/// The fluid tags behaviour reads.
#[derive(Debug, Default)]
pub struct FluidTags {
    pub water: FluidSet,
    pub lava: FluidSet,
    pub bubble_column_can_occupy: FluidSet,
    pub supports_frogspawn: FluidSet,
    pub supports_lily_pad: FluidSet,
    pub supports_sugar_cane_adjacently: FluidSet,
}

impl FluidTags {
    fn load(packs: &LoadedPacks) -> Self {
        let tag = |name: &str| FluidSet::from_tag(packs, name);
        Self {
            water: tag("minecraft:water"),
            lava: tag("minecraft:lava"),
            bubble_column_can_occupy: tag("minecraft:bubble_column_can_occupy"),
            supports_frogspawn: tag("minecraft:supports_frogspawn"),
            supports_lily_pad: tag("minecraft:supports_lily_pad"),
            supports_sugar_cane_adjacently: tag("minecraft:supports_sugar_cane_adjacently"),
        }
    }
}

/// A set of game events (a resolved `minecraft:game_event` tag), one bit per registry id.
#[derive(Clone, Debug, Default)]
pub struct GameEventSet(Vec<u64>);

impl GameEventSet {
    fn from_tag(packs: &LoadedPacks, tag: &str) -> Self {
        let registry = &vanillite_data::registry::GAME_EVENT;
        let mut bits = vec![0u64; registry.len().div_ceil(64)];
        if let Some(entries) = packs.tags("minecraft:game_event").and_then(|t| t.get(tag)) {
            for entry in entries {
                if let Some(id) = registry.id(entry) {
                    bits[usize::from(id) / 64] |= 1 << (id % 64);
                }
            }
        }
        Self(bits)
    }

    /// `event.is(tag)` for a `minecraft:game_event` id.
    #[inline]
    pub fn contains(&self, event: u16) -> bool {
        self.0
            .get(usize::from(event) / 64)
            .is_some_and(|word| word & (1 << (event % 64)) != 0)
    }
}

/// The game event tags behaviour reads.
#[derive(Debug, Default)]
pub struct GameEventTags {
    pub vibrations: GameEventSet,
    pub ignore_vibrations_sneaking: GameEventSet,
}

impl GameEventTags {
    fn load(packs: &LoadedPacks) -> Self {
        Self {
            vibrations: GameEventSet::from_tag(packs, "minecraft:vibrations"),
            ignore_vibrations_sneaking: GameEventSet::from_tag(
                packs,
                "minecraft:ignore_vibrations_sneaking",
            ),
        }
    }
}

/// Everything behaviour resolves from the data packs.
#[derive(Debug, Default)]
pub struct Tags {
    pub blocks: BlockTags,
    pub fluids: FluidTags,
    pub game_events: GameEventTags,
}

static TAGS: OnceLock<Tags> = OnceLock::new();

/// Resolves the block and fluid tags block behaviour reads from `packs`. Called by
/// [`crate::feature::FeatureSet::load`]; the first call wins (all dimensions of a server share
/// their data packs).
pub fn init(packs: &LoadedPacks) {
    TAGS.get_or_init(|| Tags {
        blocks: BlockTags::load(packs),
        fluids: FluidTags::load(packs),
        game_events: GameEventTags::load(packs),
    });
}

/// The resolved tags; empty tags (as Vanilla treats unknown tags) before [`init`].
pub fn tags() -> &'static Tags {
    static EMPTY: OnceLock<Tags> = OnceLock::new();
    TAGS.get()
        .unwrap_or_else(|| EMPTY.get_or_init(Tags::default))
}

/// The blocks `FireBlock.bootStrap` gives ignite odds (all of them positive), by name.
const FLAMMABLE: &[&str] = &[
    "oak_planks",
    "spruce_planks",
    "birch_planks",
    "jungle_planks",
    "acacia_planks",
    "cherry_planks",
    "dark_oak_planks",
    "pale_oak_planks",
    "poplar_planks",
    "mangrove_planks",
    "bamboo_planks",
    "bamboo_mosaic",
    "oak_slab",
    "spruce_slab",
    "birch_slab",
    "jungle_slab",
    "acacia_slab",
    "cherry_slab",
    "dark_oak_slab",
    "pale_oak_slab",
    "poplar_slab",
    "mangrove_slab",
    "bamboo_slab",
    "bamboo_mosaic_slab",
    "oak_fence_gate",
    "spruce_fence_gate",
    "birch_fence_gate",
    "jungle_fence_gate",
    "acacia_fence_gate",
    "cherry_fence_gate",
    "dark_oak_fence_gate",
    "pale_oak_fence_gate",
    "poplar_fence_gate",
    "mangrove_fence_gate",
    "bamboo_fence_gate",
    "oak_fence",
    "spruce_fence",
    "birch_fence",
    "jungle_fence",
    "acacia_fence",
    "cherry_fence",
    "dark_oak_fence",
    "pale_oak_fence",
    "poplar_fence",
    "mangrove_fence",
    "bamboo_fence",
    "oak_stairs",
    "birch_stairs",
    "spruce_stairs",
    "jungle_stairs",
    "acacia_stairs",
    "cherry_stairs",
    "dark_oak_stairs",
    "pale_oak_stairs",
    "poplar_stairs",
    "mangrove_stairs",
    "bamboo_stairs",
    "bamboo_mosaic_stairs",
    "oak_log",
    "spruce_log",
    "birch_log",
    "jungle_log",
    "acacia_log",
    "cherry_log",
    "pale_oak_log",
    "poplar_log",
    "dark_oak_log",
    "mangrove_log",
    "bamboo_block",
    "stripped_oak_log",
    "stripped_spruce_log",
    "stripped_birch_log",
    "stripped_jungle_log",
    "stripped_acacia_log",
    "stripped_cherry_log",
    "stripped_dark_oak_log",
    "stripped_pale_oak_log",
    "stripped_poplar_log",
    "stripped_mangrove_log",
    "stripped_bamboo_block",
    "stripped_oak_wood",
    "stripped_spruce_wood",
    "stripped_birch_wood",
    "stripped_jungle_wood",
    "stripped_acacia_wood",
    "stripped_cherry_wood",
    "stripped_dark_oak_wood",
    "stripped_pale_oak_wood",
    "stripped_poplar_wood",
    "stripped_mangrove_wood",
    "oak_wood",
    "spruce_wood",
    "birch_wood",
    "jungle_wood",
    "acacia_wood",
    "cherry_wood",
    "pale_oak_wood",
    "poplar_wood",
    "dark_oak_wood",
    "mangrove_wood",
    "mangrove_roots",
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
    "bookshelf",
    "tnt",
    "short_grass",
    "fern",
    "dead_bush",
    "short_dry_grass",
    "tall_dry_grass",
    "sunflower",
    "lilac",
    "rose_bush",
    "peony",
    "red_shrub",
    "tall_grass",
    "large_fern",
    "dandelion",
    "golden_dandelion",
    "poppy",
    "open_eyeblossom",
    "closed_eyeblossom",
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
    "torchflower",
    "pitcher_plant",
    "wither_rose",
    "pink_petals",
    "wildflowers",
    "leaf_litter",
    "cactus_flower",
    "vine",
    "coal_block",
    "hay_block",
    "target",
    "pale_moss_block",
    "pale_moss_carpet",
    "pale_hanging_moss",
    "dried_kelp_block",
    "bamboo",
    "scaffolding",
    "lectern",
    "composter",
    "sweet_berry_bush",
    "beehive",
    "bee_nest",
    "azalea_leaves",
    "flowering_azalea_leaves",
    "cave_vines",
    "cave_vines_plant",
    "spore_blossom",
    "azalea",
    "flowering_azalea",
    "big_dripleaf",
    "big_dripleaf_stem",
    "small_dripleaf",
    "hanging_roots",
    "glow_lichen",
    "firefly_bush",
    "bush",
    "acacia_shelf",
    "bamboo_shelf",
    "birch_shelf",
    "cherry_shelf",
    "dark_oak_shelf",
    "jungle_shelf",
    "mangrove_shelf",
    "poplar_shelf",
    "oak_shelf",
    "pale_oak_shelf",
    "spruce_shelf",
    "straw_bed",
    "white_wool",
    "orange_wool",
    "magenta_wool",
    "light_blue_wool",
    "yellow_wool",
    "lime_wool",
    "pink_wool",
    "gray_wool",
    "light_gray_wool",
    "cyan_wool",
    "purple_wool",
    "blue_wool",
    "brown_wool",
    "green_wool",
    "red_wool",
    "black_wool",
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
];

/// `FireBlock.canBurn(state)`: whether fire can spread to the state (`igniteOdds > 0`, never
/// for waterlogged states).
pub fn can_burn(state: BlockState) -> bool {
    static SET: OnceLock<BlockSet> = OnceLock::new();
    let set = SET.get_or_init(|| {
        let mut set = BlockSet::default();
        for name in FLAMMABLE {
            if let Some(block) = BlockId::by_name(&format!("minecraft:{name}")) {
                set.insert(block);
            }
        }
        set
    });
    set.contains(state) && state.value("waterlogged") != Some("true")
}

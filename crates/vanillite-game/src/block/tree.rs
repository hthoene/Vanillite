//! Saplings growing into trees (`TreeGrower.growTree`).
//!
//! The tree is Vanilla's configured feature from `vanillite-worldgen`, placed in the live
//! world like Vanilla places it into the `ServerLevel`: every block write goes to the level
//! with its neighbour and shape updates and scheduled ticks, in the feature's order. The
//! feature reads from a region of copies of the 3 × 3 chunks around the sapling, which follow
//! every block the level changes (`WorldGenRegion` in live mode). The level random is the
//! feature's random.

use std::sync::Arc;

use vanillite_base::BlockPos;
use vanillite_base::hash::FxHashMap;
use vanillite_base::random::{DecorationRandom, LegacyRandom, RandomSource};
use vanillite_data::block::{BlockId, BlockState, ids};
use vanillite_datapack::LoadedPacks;
use vanillite_world::chunks::Chunks;
use vanillite_worldgen::anchor::WorldGenerationContext;
use vanillite_worldgen::behaviour::StateExt;
use vanillite_worldgen::biome::BiomeRegistry;
use vanillite_worldgen::feature::region::LiveLevel;
use vanillite_worldgen::feature::{
    FeatureContext, FeatureEnv, FeatureId, FeatureParser, PlacedFeatureId, WorldGenRegion,
};
use vanillite_worldgen::proto::ProtoChunk;
use vanillite_worldgen::random_state::RandomState;

use super::block_tags;
use crate::level::Level;

/// `ServerLevel.setBlock` flags of `removeSapling` (818) and `resetSaplings` (260).
const REMOVE_FLAGS: u32 = 818;
const RESET_FLAGS: u32 = 260;

/// A weighted list of configured features (`WeightedList<ResourceKey<ConfiguredFeature>>`).
type Weighted = &'static [(&'static str, i32)];

/// `TreeGrower`: the trees a sapling grows into.
#[derive(Debug)]
struct Grower {
    trees: Weighted,
    mega_trees: Weighted,
    flower_trees: Weighted,
}

/// The growers of Vanilla's saplings (`TreeGrower.OAK` …).
fn grower(block: BlockId) -> Option<&'static Grower> {
    static OAK: Grower = Grower {
        trees: &[("minecraft:oak", 9), ("minecraft:fancy_oak", 1)],
        mega_trees: &[],
        flower_trees: &[
            ("minecraft:oak_bees_005", 9),
            ("minecraft:fancy_oak_bees_005", 1),
        ],
    };
    static SPRUCE: Grower = Grower {
        trees: &[("minecraft:spruce", 1)],
        mega_trees: &[("minecraft:mega_spruce", 1), ("minecraft:mega_pine", 1)],
        flower_trees: &[],
    };
    static BIRCH: Grower = Grower {
        trees: &[("minecraft:birch", 1)],
        mega_trees: &[],
        flower_trees: &[("minecraft:birch_bees_005", 1)],
    };
    static JUNGLE: Grower = Grower {
        trees: &[("minecraft:jungle_tree_no_vine", 1)],
        mega_trees: &[("minecraft:mega_jungle_tree", 1)],
        flower_trees: &[],
    };
    static ACACIA: Grower = Grower {
        trees: &[("minecraft:acacia", 1)],
        mega_trees: &[],
        flower_trees: &[],
    };
    static CHERRY: Grower = Grower {
        trees: &[("minecraft:cherry", 1)],
        mega_trees: &[],
        flower_trees: &[("minecraft:cherry_bees_005", 1)],
    };
    static DARK_OAK: Grower = Grower {
        trees: &[],
        mega_trees: &[("minecraft:dark_oak", 1)],
        flower_trees: &[],
    };
    static PALE_OAK: Grower = Grower {
        trees: &[],
        mega_trees: &[("minecraft:pale_oak_bonemeal", 1)],
        flower_trees: &[],
    };
    static MANGROVE: Grower = Grower {
        trees: &[("minecraft:mangrove", 15), ("minecraft:tall_mangrove", 85)],
        mega_trees: &[],
        flower_trees: &[],
    };
    static AZALEA: Grower = Grower {
        trees: &[("minecraft:azalea_tree", 1)],
        mega_trees: &[],
        flower_trees: &[],
    };
    Some(match block {
        b if b == ids::OAK_SAPLING => &OAK,
        b if b == ids::SPRUCE_SAPLING => &SPRUCE,
        b if b == ids::BIRCH_SAPLING => &BIRCH,
        b if b == ids::JUNGLE_SAPLING => &JUNGLE,
        b if b == ids::ACACIA_SAPLING => &ACACIA,
        b if b == ids::CHERRY_SAPLING => &CHERRY,
        b if b == ids::DARK_OAK_SAPLING => &DARK_OAK,
        b if b == ids::PALE_OAK_SAPLING => &PALE_OAK,
        b if b == ids::MANGROVE_PROPAGULE => &MANGROVE,
        b if b == ids::AZALEA || b == ids::FLOWERING_AZALEA => &AZALEA,
        _ => return None,
    })
}

/// The tree features of a world, parsed once.
#[derive(Debug)]
pub struct TreeFeatures {
    env: FeatureEnv,
    random_state: RandomState,
    ids: FxHashMap<&'static str, FeatureId>,
    sea_level: i32,
    /// `VegetationPlacements.GRASS_BONEMEAL`.
    grass_bonemeal: Option<PlacedFeatureId>,
    /// `getBoneMealFeatures()` of each biome by protocol id: the features of its placed
    /// features in `#can_spawn_from_bone_meal` (the flowers grass grows).
    biome_flowers: Vec<Vec<FeatureId>>,
}

/// The configured features bone meal places besides trees.
pub const BONEMEAL_FEATURES: [&str; 7] = [
    "minecraft:crimson_fungus_planted",
    "minecraft:warped_fungus_planted",
    "minecraft:huge_brown_mushroom",
    "minecraft:huge_red_mushroom",
    "minecraft:moss_patch_bonemeal",
    "minecraft:pale_moss_patch_bonemeal",
    "minecraft:nylium_bonemeal",
];

impl TreeFeatures {
    /// Parses every tree a sapling grows into from the data packs.
    pub fn load(
        packs: &LoadedPacks,
        biomes: &BiomeRegistry,
        seed: i64,
        min_y: i32,
        height: i32,
        sea_level: i32,
    ) -> Result<Self, vanillite_worldgen::Error> {
        let random_state = RandomState::new(packs, seed, false)?;
        let mut parser = FeatureParser::new(packs, biomes, &random_state);
        let mut ids = FxHashMap::default();
        let all = [
            ids::OAK_SAPLING,
            ids::SPRUCE_SAPLING,
            ids::BIRCH_SAPLING,
            ids::JUNGLE_SAPLING,
            ids::ACACIA_SAPLING,
            ids::CHERRY_SAPLING,
            ids::DARK_OAK_SAPLING,
            ids::PALE_OAK_SAPLING,
            ids::MANGROVE_PROPAGULE,
            ids::AZALEA,
        ];
        for block in all {
            let Some(g) = grower(block) else {
                continue;
            };
            for &(name, _) in g.trees.iter().chain(g.mega_trees).chain(g.flower_trees) {
                if !ids.contains_key(name) {
                    let id = parser.feature(&serde_json::Value::String(name.to_owned()))?;
                    ids.insert(name, id);
                }
            }
        }
        for name in BONEMEAL_FEATURES {
            match parser.feature(&serde_json::Value::String(name.to_owned())) {
                Ok(id) => {
                    ids.insert(name, id);
                }
                Err(e) => log::warn!("bone meal feature {name}: {e}"),
            }
        }
        let grass_bonemeal = parser
            .placed(&serde_json::Value::String(
                "minecraft:grass_bonemeal".to_owned(),
            ))
            .ok();
        let biome_flowers = biome_flower_features(packs, biomes, &mut parser);
        let env = FeatureEnv {
            registry: parser.finish(),
            context: WorldGenerationContext::new(min_y, height, sea_level),
            biome_feature_sets: Vec::new(),
        };
        Ok(Self {
            env,
            random_state,
            ids,
            sea_level,
            grass_bonemeal,
            biome_flowers,
        })
    }
}

/// `BiomeGenerationSettings.getBoneMealFeatures` of every biome: in step order, the feature of
/// each placed feature (and the features inside it) that is in `#can_spawn_from_bone_meal`.
fn biome_flower_features(
    packs: &LoadedPacks,
    biomes: &BiomeRegistry,
    parser: &mut FeatureParser<'_>,
) -> Vec<Vec<FeatureId>> {
    let tag: Vec<String> = packs
        .tags("minecraft:worldgen/feature")
        .and_then(|t| t.get("minecraft:can_spawn_from_bone_meal"))
        .cloned()
        .unwrap_or_default();
    let placed_registry = packs.registry("minecraft:worldgen/placed_feature");
    let mut out = Vec::with_capacity(biomes.all().len());
    for biome in biomes.all() {
        let mut list = Vec::new();
        for step in &biome.features {
            let names: Vec<String> = match step {
                serde_json::Value::Array(a) => a
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect(),
                serde_json::Value::String(s) => match s.strip_prefix('#') {
                    Some(t) => packs
                        .tags("minecraft:worldgen/placed_feature")
                        .and_then(|tags| tags.get(t))
                        .cloned()
                        .unwrap_or_default(),
                    None => vec![s.clone()],
                },
                _ => Vec::new(),
            };
            for name in names {
                let Some(entry) = placed_registry.and_then(|r| r.get(&qualify(&name))) else {
                    continue;
                };
                let Ok(json) = serde_json::from_str::<serde_json::Value>(entry.json.get()) else {
                    continue;
                };
                if let Some(feature) = json.get("feature").and_then(serde_json::Value::as_str) {
                    let feature = qualify(feature);
                    if tag.contains(&feature)
                        && let Ok(id) = parser.feature(&serde_json::Value::String(feature))
                    {
                        list.push(id);
                    }
                }
            }
        }
        out.push(list);
    }
    out
}

/// A resource location with the default namespace.
fn qualify(name: &str) -> String {
    if name.contains(':') {
        name.to_owned()
    } else {
        format!("minecraft:{name}")
    }
}

/// `Holder<Feature>.value().place(level, generator, level.random, origin)` of a bone meal
/// feature ([`BONEMEAL_FEATURES`]); `None` without it.
pub fn place_named(level: &mut Level, name: &str, origin: BlockPos) -> Option<bool> {
    let trees = level.tree_features().cloned()?;
    let id = *trees.ids.get(name)?;
    Some(place(level, &trees, id, origin))
}

/// `GRASS_BONEMEAL.place(level, generator, level.random, origin)`.
pub fn place_grass_bonemeal(level: &mut Level, origin: BlockPos) -> bool {
    let Some(trees) = level.tree_features().cloned() else {
        return false;
    };
    let Some(id) = trees.grass_bonemeal else {
        return false;
    };
    place_with(level, &trees, origin, |ctx| ctx.place_placed(id, origin))
}

/// `getBoneMealFeatures()` of the biome at `pos` is empty.
pub fn biome_flowers_empty(level: &Level, pos: BlockPos) -> bool {
    let biome = usize::from(level.biome(pos));
    level
        .tree_features()
        .and_then(|t| t.biome_flowers.get(biome))
        .is_none_or(Vec::is_empty)
}

/// `Util.getRandom(getBoneMealFeatures(), random).place(level, generator, random, pos)` of the
/// biome at `pos`.
pub fn place_biome_flower(level: &mut Level, pos: BlockPos) {
    let Some(trees) = level.tree_features().cloned() else {
        return;
    };
    let biome = usize::from(level.biome(pos));
    let Some(list) = trees.biome_flowers.get(biome).filter(|l| !l.is_empty()) else {
        return;
    };
    let len = i32::try_from(list.len()).unwrap_or(i32::MAX);
    let index = usize::try_from(level.random().next_int_bounded(len)).unwrap_or(0);
    let id = list[index];
    place(level, &trees, id, pos);
}

/// `WeightedList.getRandom(random)`.
fn pick(list: Weighted, random: &mut LegacyRandom) -> Option<&'static str> {
    let total: i32 = list.iter().map(|e| e.1).sum();
    if total == 0 {
        return None;
    }
    let mut selection = random.next_int_bounded(total);
    for &(name, weight) in list {
        selection -= weight;
        if selection < 0 {
            return Some(name);
        }
    }
    None
}

/// `TreeGrower.growTree(level, generator, pos, state, level.random)`.
pub fn grow_tree(level: &mut Level, pos: BlockPos, state: BlockState) -> bool {
    grow_tree_by(level, pos, state, 0)
}

/// Whether plugins let the sapling, mushroom or fungus `state` at `pos` grow into its tree or
/// huge plant (`STRUCTURE_GROW`, Bukkit's `StructureGrowEvent`); `player` (0 for none) used
/// bone meal on it. `true` when no plugin listens.
pub fn structure_may_grow(level: &Level, pos: BlockPos, state: BlockState, player: u128) -> bool {
    let Some(hooks) = level.hooks_for(vanillite_abi::event::STRUCTURE_GROW) else {
        return true;
    };
    let mut ev = crate::hooks::Event {
        pos,
        key: super::hook_events::state_text(state),
        state: Some(state),
        player,
        ..crate::hooks::Event::new(vanillite_abi::event::STRUCTURE_GROW)
    };
    hooks.event(&mut ev);
    !ev.cancelled
}

/// [`grow_tree`] by `player`'s bone meal (0 for a random tick); plugins may cancel it
/// ([`structure_may_grow`]) after the tree is chosen: the sapling stays.
pub fn grow_tree_by(level: &mut Level, pos: BlockPos, state: BlockState, player: u128) -> bool {
    let Some(grower) = grower(state.block()) else {
        return false;
    };
    let Some(trees) = level.tree_features().cloned() else {
        return false;
    };
    let mega = pick(grower.mega_trees, level.random());
    if let Some(name) = mega
        && let Some(&id) = trees.ids.get(name)
        && let Some((dx, dz)) = two_by_two(level, state, pos)
    {
        if !structure_may_grow(level, pos, state, player) {
            return false;
        }
        let saplings: Vec<(BlockState, BlockPos)> = [(0, 0), (1, 0), (0, 1), (1, 1)]
            .iter()
            .map(|&(x, z)| {
                let p = pos.offset(dx + x, 0, dz + z);
                (level.block(p), p)
            })
            .collect();
        for &(_, p) in &saplings {
            remove_sapling(level, p);
        }
        if place(level, &trees, id, pos.offset(dx, 0, dz)) {
            return true;
        }
        for &(s, p) in &saplings {
            level.set_block(p, s, RESET_FLAGS);
        }
        return false;
    }
    let flowers = has_flowers(level, pos);
    let list = if flowers && !grower.flower_trees.is_empty() {
        grower.flower_trees
    } else {
        grower.trees
    };
    let Some(name) = pick(list, level.random()) else {
        return false;
    };
    let Some(&id) = trees.ids.get(name) else {
        return false;
    };
    if !structure_may_grow(level, pos, state, player) {
        return false;
    }
    remove_sapling(level, pos);
    if place(level, &trees, id, pos) {
        true
    } else {
        level.set_block(pos, state, RESET_FLAGS);
        false
    }
}

/// `TreeGrower.canGrow`: draws the tree and the mega tree like `growTree` would; a grower with
/// only mega trees needs a 2 × 2 of saplings.
pub fn can_grow(level: &mut Level, pos: BlockPos, state: BlockState) -> bool {
    let Some(grower) = grower(state.block()) else {
        return false;
    };
    let flowers = has_flowers(level, pos);
    let list = if flowers && !grower.flower_trees.is_empty() {
        grower.flower_trees
    } else {
        grower.trees
    };
    let feature = pick(list, level.random());
    let mega = pick(grower.mega_trees, level.random());
    if feature.is_none() && mega.is_some() {
        two_by_two(level, state, pos).is_some()
    } else {
        true
    }
}

/// `TreeGrower.getMinimumHeight`: the trunk base height of the grower's shortest tree (0 for
/// the growers without one).
pub fn minimum_height(block: BlockId) -> i32 {
    match block {
        b if b == ids::OAK_SAPLING || b == ids::JUNGLE_SAPLING => 4,
        b if b == ids::SPRUCE_SAPLING || b == ids::BIRCH_SAPLING || b == ids::ACACIA_SAPLING => 5,
        b if b == ids::CHERRY_SAPLING => 7,
        b if b == ids::MANGROVE_PROPAGULE => 2,
        b if b == ids::AZALEA || b == ids::FLOWERING_AZALEA => 4,
        _ => 0,
    }
}

/// `findTwoByTwoSaplingPos`: the offset of a 2 × 2 of this sapling containing `pos`.
fn two_by_two(level: &Level, state: BlockState, pos: BlockPos) -> Option<(i32, i32)> {
    for dx in [0, -1] {
        for dz in [0, -1] {
            let all = [(0, 0), (1, 0), (0, 1), (1, 1)]
                .iter()
                .all(|&(x, z)| level.block(pos.offset(dx + x, 0, dz + z)).is(state.block()));
            if all {
                return Some((dx, dz));
            }
        }
    }
    None
}

/// `removeSapling`: the sapling's fluid (water or air) replaces it.
fn remove_sapling(level: &mut Level, pos: BlockPos) {
    let fluid = level.block(pos).fluid_state();
    let empty = if fluid.is_empty() {
        BlockState::AIR
    } else {
        ids::WATER.default_state()
    };
    level.set_block(pos, empty, REMOVE_FLAGS);
}

/// `hasFlowers`: a flower within 2 blocks horizontally and 1 vertically.
fn has_flowers(level: &Level, pos: BlockPos) -> bool {
    let flowers = &block_tags().flowers;
    (-1..=1).any(|y| {
        (-2..=2).any(|z| (-2..=2).any(|x| flowers.contains(level.block(pos.offset(x, y, z)))))
    })
}

/// `ConfiguredFeature.place(level, generator, level.random, origin)` in the live level.
fn place(level: &mut Level, trees: &TreeFeatures, id: FeatureId, origin: BlockPos) -> bool {
    place_with(level, trees, origin, |ctx| ctx.place_feature(id, origin))
}

/// Runs `f` on a feature context of the live level around `origin` with the level random.
fn place_with(
    level: &mut Level,
    trees: &TreeFeatures,
    origin: BlockPos,
    f: impl FnOnce(&mut FeatureContext<'_, '_>) -> bool,
) -> bool {
    let center = origin.chunk();
    let mut copies = Vec::with_capacity(9);
    for dz in -1..=1 {
        for dx in -1..=1 {
            let pos = vanillite_base::ChunkPos::new(center.x + dx, center.z + dz);
            let Some(chunk) = level.chunks.chunk(pos) else {
                return false;
            };
            let sections = chunk
                .chunk
                .sections()
                .iter()
                .map(|s| (**s).clone())
                .collect();
            copies.push(ProtoChunk::from_sections(
                pos,
                chunk.chunk.min_y(),
                sections,
                true,
            ));
        }
    }
    let random = std::mem::replace(level.random(), LegacyRandom::new(0));
    let mut random = DecorationRandom::legacy(random);
    let placed = {
        let live = LiveWorld {
            mark: level.chunks.block_change_count(),
            level: &mut *level,
        };
        let mut region = WorldGenRegion::new(
            copies.iter_mut().collect(),
            1,
            1,
            &trees.random_state,
            trees.sea_level,
        );
        region.set_live(Box::new(live));
        let mut ctx = FeatureContext {
            level: &mut region,
            env: &trees.env,
            random: &mut random,
        };
        f(&mut ctx)
    };
    *level.random() = random.into_legacy().unwrap_or_else(|| LegacyRandom::new(0));
    placed
}

/// The level as the live world of a feature.
struct LiveWorld<'l> {
    level: &'l mut Level,
    /// Block changes of the level already mirrored.
    mark: usize,
}

impl LiveLevel for LiveWorld<'_> {
    fn set_block(&mut self, pos: BlockPos, state: BlockState, flags: i32) -> bool {
        self.level.set_block(pos, state, flags as u32)
    }

    fn schedule_block_tick(&mut self, pos: BlockPos, block: BlockId, delay: i32) {
        self.level.schedule_block_tick(pos, block, delay);
    }

    fn schedule_fluid_tick(&mut self, pos: BlockPos, fluid: u16, delay: i32) {
        self.level.schedule_fluid_tick(pos, fluid, delay);
    }

    fn take_changes(&mut self, out: &mut Vec<(BlockPos, BlockState)>) {
        let chunks: &Chunks = &self.level.chunks;
        let changes = chunks.block_changes_from(self.mark);
        out.extend(changes.iter().map(|&p| (p, self.level.block(p))));
        self.mark = chunks.block_change_count();
    }
}

/// Keeps [`TreeFeatures`] shareable between the worlds of a server.
pub type SharedTreeFeatures = Arc<TreeFeatures>;

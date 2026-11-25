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
    FeatureContext, FeatureEnv, FeatureId, FeatureParser, WorldGenRegion,
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
}

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
        })
    }
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
    remove_sapling(level, pos);
    if place(level, &trees, id, pos) {
        true
    } else {
        level.set_block(pos, state, RESET_FLAGS);
        false
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
        ctx.place_feature(id, origin)
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

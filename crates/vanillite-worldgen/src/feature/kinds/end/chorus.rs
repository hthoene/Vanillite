//! `chorus_plant` with `ChorusFlowerBlock.generatePlant` and
//! `ChorusPlantBlock.getStateWithConnections`.

use std::sync::LazyLock;

use serde_json::Value;
use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Direction};
use vanillite_data::block::{BlockState, ids};

use crate::Error;
use crate::blocks::BlockSet;
use crate::feature::kinds::vegetation::util::{HORIZONTAL, with, with_bool};
use crate::feature::region::WorldGenRegion;
use crate::feature::{FeatureContext, FeatureParser, FeatureType};

/// `ChorusFlowerBlock.generatePlant(level, origin, random, 8)`'s spread limit.
const MAX_HORIZONTAL_SPREAD: i32 = 8;

/// The chorus plant states by connection mask (bit = `Direction` ordinal).
static PLANT_STATES: LazyLock<[BlockState; 64]> = LazyLock::new(|| {
    const PROPERTY: [&str; 6] = ["down", "up", "north", "south", "west", "east"];
    std::array::from_fn(|mask| {
        let mut state = ids::CHORUS_PLANT.default_state();
        for (bit, property) in PROPERTY.iter().enumerate() {
            state = with_bool(state, property, mask & (1 << bit) != 0);
        }
        state
    })
});

/// `chorus_flower` with `age=5`, the dead flower that ends every branch.
static DEAD_FLOWER: LazyLock<BlockState> =
    LazyLock::new(|| with(ids::CHORUS_FLOWER.default_state(), "age", "5"));

/// `chorus_plant`: a chorus tree on a block that supports chorus plants.
#[derive(Debug)]
pub struct ChorusPlantFeature {
    /// `#minecraft:supports_chorus_plant`.
    supports: BlockSet,
}

impl FeatureType for ChorusPlantFeature {
    fn parse(_json: &Value, parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        Ok(Self {
            supports: parser.block_tag("minecraft:supports_chorus_plant"),
        })
    }

    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        if ctx.level.is_empty_block(origin)
            && self
                .supports
                .contains(ctx.level.block(origin.offset(0, -1, 0)))
        {
            self.generate_plant(ctx.level, origin, ctx.random);
            true
        } else {
            false
        }
    }
}

impl ChorusPlantFeature {
    /// `ChorusPlantBlock.getStateWithConnections(level, pos, CHORUS_PLANT.defaultBlockState())`.
    fn plant_state(&self, level: &WorldGenRegion<'_>, pos: BlockPos) -> BlockState {
        let connects = |state: BlockState| {
            let block = state.block();
            block == ids::CHORUS_PLANT || block == ids::CHORUS_FLOWER
        };
        let mut mask = 0;
        for direction in Direction::ALL {
            let neighbour = level.block(pos.relative(direction));
            let connected = connects(neighbour)
                || (direction == Direction::Down && self.supports.contains(neighbour));
            if connected {
                mask |= 1 << direction as usize;
            }
        }
        PLANT_STATES[mask]
    }

    /// `ChorusFlowerBlock.generatePlant`.
    fn generate_plant<R: RandomSource>(
        &self,
        level: &mut WorldGenRegion<'_>,
        target: BlockPos,
        random: &mut R,
    ) {
        let state = self.plant_state(level, target);
        level.set_block(target, state, 2);
        self.grow_tree_recursive(level, target, random, target, 0);
    }

    /// `ChorusFlowerBlock.growTreeRecursive`.
    fn grow_tree_recursive<R: RandomSource>(
        &self,
        level: &mut WorldGenRegion<'_>,
        current: BlockPos,
        random: &mut R,
        start: BlockPos,
        depth: i32,
    ) {
        let mut height = random.next_int_bounded(4) + 1;
        if depth == 0 {
            height += 1;
        }
        for i in 0..height {
            let target = current.offset(0, i + 1, 0);
            if !all_neighbors_empty(level, target, None) {
                return;
            }
            let state = self.plant_state(level, target);
            level.set_block(target, state, 2);
            let below = target.offset(0, -1, 0);
            let state = self.plant_state(level, below);
            level.set_block(below, state, 2);
        }
        let mut placed_stem = false;
        if depth < 4 {
            let mut stems = random.next_int_bounded(4);
            if depth == 0 {
                stems += 1;
            }
            for _ in 0..stems {
                let direction = HORIZONTAL[random.next_int_bounded(4) as usize];
                let target = current.offset(0, height, 0).relative(direction);
                if (target.x - start.x).abs() < MAX_HORIZONTAL_SPREAD
                    && (target.z - start.z).abs() < MAX_HORIZONTAL_SPREAD
                    && level.is_empty_block(target)
                    && level.is_empty_block(target.offset(0, -1, 0))
                    && all_neighbors_empty(level, target, Some(direction.opposite()))
                {
                    placed_stem = true;
                    let state = self.plant_state(level, target);
                    level.set_block(target, state, 2);
                    let back = target.relative(direction.opposite());
                    let state = self.plant_state(level, back);
                    level.set_block(back, state, 2);
                    self.grow_tree_recursive(level, target, random, start, depth + 1);
                }
            }
        }
        if !placed_stem {
            level.set_block(current.offset(0, height, 0), *DEAD_FLOWER, 2);
        }
    }
}

/// `ChorusFlowerBlock.allNeighborsEmpty`: the horizontal neighbours except `ignore` are air.
fn all_neighbors_empty(
    level: &WorldGenRegion<'_>,
    pos: BlockPos,
    ignore: Option<Direction>,
) -> bool {
    HORIZONTAL
        .iter()
        .all(|&d| Some(d) == ignore || level.is_empty_block(pos.relative(d)))
}

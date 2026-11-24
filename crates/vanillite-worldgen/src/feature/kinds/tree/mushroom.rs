//! Huge mushrooms (`AbstractHugeMushroomFeature`, `HugeBrownMushroomFeature`,
//! `HugeRedMushroomFeature`).

use serde_json::Value;
use vanillite_base::BlockPos;
use vanillite_base::random::RandomSource;
use vanillite_data::block::BlockState;

use super::{has_property, try_set_bool};
use crate::Error;
use crate::blocks::BlockSet;
use crate::feature::region::WorldGenRegion;
use crate::feature::{
    BlockPredicate, BlockStateProvider, FeatureContext, FeatureParser, FeatureType, field, int_or,
};

/// The configuration shared by both huge mushrooms (`AbstractHugeMushroomFeature`).
#[derive(Debug)]
struct HugeMushroom {
    cap_provider: BlockStateProvider,
    stem_provider: BlockStateProvider,
    foliage_radius: i32,
    can_place_on: BlockPredicate,
    /// `#minecraft:leaves`.
    leaves: BlockSet,
    /// `#minecraft:replaceable_by_mushrooms`.
    replaceable_by_mushrooms: BlockSet,
}

/// What differs between the two mushrooms.
trait MushroomShape {
    fn config(&self) -> &HugeMushroom;

    /// `getTreeRadiusForHeight(trunkHeight, treeHeight, leafRadius, yo)`.
    fn tree_radius_for_height(
        &self,
        trunk_height: i32,
        tree_height: i32,
        leaf_radius: i32,
        yo: i32,
    ) -> i32;

    /// `makeCap(level, random, origin, treeHeight, blockPos)`.
    fn make_cap(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos, tree_height: i32);

    /// `AbstractHugeMushroomFeature.place`.
    fn place_mushroom(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        // `getTreeHeight`.
        let mut tree_height = ctx.random.next_int_bounded(3) + 4;
        if ctx.random.next_int_bounded(12) == 0 {
            tree_height *= 2;
        }
        if !self.is_valid_position(ctx.level, origin, tree_height) {
            return false;
        }
        self.make_cap(ctx, origin, tree_height);
        // `placeTrunk`: the stem state is sampled at the origin for every block.
        let config = self.config();
        for dy in 0..tree_height {
            let state = config
                .stem_provider
                .get_state(ctx.level, ctx.random, origin);
            config.place_mushroom_block(ctx.level, origin.offset(0, dy, 0), state);
        }
        true
    }

    /// `isValidPosition`.
    fn is_valid_position(
        &self,
        level: &WorldGenRegion<'_>,
        origin: BlockPos,
        tree_height: i32,
    ) -> bool {
        let config = self.config();
        let y = origin.y;
        if y < level.min_y() + 1 || y + tree_height + 1 > level.max_y() {
            return false;
        }
        if !config.can_place_on.test(level, origin.offset(0, -1, 0)) {
            return false;
        }
        for dy in 0..=tree_height {
            let radius = self.tree_radius_for_height(-1, -1, config.foliage_radius, dy);
            for dx in -radius..=radius {
                for dz in -radius..=radius {
                    let state = level.block(origin.offset(dx, dy, dz));
                    if !state.is_air() && !config.leaves.contains(state) {
                        return false;
                    }
                }
            }
        }
        true
    }
}

impl HugeMushroom {
    fn parse(json: &Value, parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        Ok(Self {
            cap_provider: parser.state_provider(field(json, "cap_provider")?)?,
            stem_provider: parser.state_provider(field(json, "stem_provider")?)?,
            foliage_radius: int_or(json, "foliage_radius", 2),
            can_place_on: parser.block_predicate(field(json, "can_place_on")?)?,
            leaves: parser.block_tag("minecraft:leaves"),
            replaceable_by_mushrooms: parser.block_tag("minecraft:replaceable_by_mushrooms"),
        })
    }

    /// `placeMushroomBlock`: replaces air and blocks replaceable by mushrooms
    /// (`setBlockAndUpdate`).
    fn place_mushroom_block(
        &self,
        level: &mut WorldGenRegion<'_>,
        pos: BlockPos,
        state: BlockState,
    ) {
        let current = level.block(pos);
        if current.is_air() || self.replaceable_by_mushrooms.contains(current) {
            level.set_block_and_update(pos, state);
        }
    }
}

/// `minecraft:huge_brown_mushroom` (`HugeBrownMushroomFeature`).
#[derive(Debug)]
pub struct HugeBrownMushroomFeature(HugeMushroom);

impl FeatureType for HugeBrownMushroomFeature {
    fn parse(json: &Value, parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        HugeMushroom::parse(json, parser).map(Self)
    }

    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        self.place_mushroom(ctx, origin)
    }
}

impl MushroomShape for HugeBrownMushroomFeature {
    fn config(&self) -> &HugeMushroom {
        &self.0
    }

    fn tree_radius_for_height(&self, _: i32, _: i32, leaf_radius: i32, yo: i32) -> i32 {
        if yo <= 3 { 0 } else { leaf_radius }
    }

    fn make_cap(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos, tree_height: i32) {
        let config = &self.0;
        let r = config.foliage_radius;
        for dx in -r..=r {
            for dz in -r..=r {
                let (min_x, max_x, min_z, max_z) = (dx == -r, dx == r, dz == -r, dz == r);
                let x_edge = min_x || max_x;
                let z_edge = min_z || max_z;
                if x_edge && z_edge {
                    continue;
                }
                let pos = origin.offset(dx, tree_height, dz);
                let west = min_x || z_edge && dx == 1 - r;
                let east = max_x || z_edge && dx == r - 1;
                let north = min_z || x_edge && dz == 1 - r;
                let south = max_z || x_edge && dz == r - 1;
                let mut state = config.cap_provider.get_state(ctx.level, ctx.random, origin);
                if ["west", "east", "north", "south"]
                    .iter()
                    .all(|p| has_property(state, p))
                {
                    state = try_set_bool(state, "west", west);
                    state = try_set_bool(state, "east", east);
                    state = try_set_bool(state, "north", north);
                    state = try_set_bool(state, "south", south);
                }
                config.place_mushroom_block(ctx.level, pos, state);
            }
        }
    }
}

/// `minecraft:huge_red_mushroom` (`HugeRedMushroomFeature`).
#[derive(Debug)]
pub struct HugeRedMushroomFeature(HugeMushroom);

impl FeatureType for HugeRedMushroomFeature {
    fn parse(json: &Value, parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        HugeMushroom::parse(json, parser).map(Self)
    }

    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        self.place_mushroom(ctx, origin)
    }
}

impl MushroomShape for HugeRedMushroomFeature {
    fn config(&self) -> &HugeMushroom {
        &self.0
    }

    fn tree_radius_for_height(&self, _: i32, tree_height: i32, leaf_radius: i32, yo: i32) -> i32 {
        if yo < tree_height && yo >= tree_height - 3 || yo == tree_height {
            leaf_radius
        } else {
            0
        }
    }

    fn make_cap(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos, tree_height: i32) {
        let config = &self.0;
        for dy in tree_height - 3..=tree_height {
            let radius = if dy < tree_height {
                config.foliage_radius
            } else {
                config.foliage_radius - 1
            };
            let center = config.foliage_radius - 2;
            for dx in -radius..=radius {
                for dz in -radius..=radius {
                    let x_edge = dx == -radius || dx == radius;
                    let z_edge = dz == -radius || dz == radius;
                    if dy < tree_height && x_edge == z_edge {
                        continue;
                    }
                    let pos = origin.offset(dx, dy, dz);
                    let mut state = config.cap_provider.get_state(ctx.level, ctx.random, origin);
                    if ["west", "east", "north", "south", "up"]
                        .iter()
                        .all(|p| has_property(state, p))
                    {
                        state = try_set_bool(state, "up", dy >= tree_height - 1);
                        state = try_set_bool(state, "west", dx < -center);
                        state = try_set_bool(state, "east", dx > center);
                        state = try_set_bool(state, "north", dz < -center);
                        state = try_set_bool(state, "south", dz > center);
                    }
                    config.place_mushroom_block(ctx.level, pos, state);
                }
            }
        }
    }
}

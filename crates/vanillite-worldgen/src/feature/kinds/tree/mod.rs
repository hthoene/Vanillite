//! Trees and tree-like features: `tree` with its trunk, foliage and root placers and decorators,
//! fallen trees, huge mushrooms and fungi, coral.
//!
//! [`TreeFeature`] ports Vanilla's `TreeFeature`: the trunk placer ([`trunk`]) places the logs
//! and returns foliage attachments, the foliage placer ([`foliage`]) grows leaves around them, an
//! optional root placer ([`root`]) places roots below, then the decorators ([`decorator`]) run and
//! the leaves get their `distance` ([`leaves`]). Every position a tree writes is recorded in a
//! [`JavaHashSet`], because Vanilla's `HashSet` iteration order decides what the decorators and
//! the distance update do.

pub mod coral;
pub mod decorator;
pub mod fallen;
pub mod foliage;
pub mod fungus;
pub mod java_set;
pub mod leaves;
pub mod mushroom;
pub mod root;
pub mod size;
pub mod trunk;

use serde_json::Value;
use vanillite_base::random::{DecorationRandom, RandomSource};
use vanillite_base::{BlockPos, Direction};
use vanillite_data::block::{BlockState, ids};
use vanillite_data::fluid::FluidState;

use crate::Error;
use crate::blocks::BlockSet;
use crate::feature::region::{WorldGenRegion, flags};
use crate::feature::{
    BlockStateProvider, FeatureContext, FeatureParser, FeatureType, bool_or, field,
};

use decorator::{DecoratorContext, TreeDecorator};
use foliage::FoliagePlacer;
pub use java_set::JavaHashSet;
use root::RootPlacer;
use size::FeatureSize;
use trunk::TrunkPlacer;

/// `TreeFeature.BLOCK_UPDATE_FLAGS` (`UPDATE_ALL | UPDATE_KNOWN_SHAPE`): how trees write blocks.
pub const TREE_UPDATE_FLAGS: i32 = flags::ALL | flags::KNOWN_SHAPE;

/// The block tags tree placement tests, resolved once per feature.
#[derive(Clone, Debug)]
pub struct TreeBlocks {
    /// `#minecraft:leaves`.
    pub leaves: BlockSet,
    /// `#minecraft:logs`.
    pub logs: BlockSet,
    /// `#minecraft:replaceable_by_trees`.
    pub replaceable_by_trees: BlockSet,
    /// `#minecraft:prevents_nearby_leaf_decay`.
    pub prevents_nearby_leaf_decay: BlockSet,
}

impl TreeBlocks {
    /// Resolves the tags from the data packs.
    pub fn new(parser: &FeatureParser<'_>) -> Self {
        Self {
            leaves: parser.block_tag("minecraft:leaves"),
            logs: parser.block_tag("minecraft:logs"),
            replaceable_by_trees: parser.block_tag("minecraft:replaceable_by_trees"),
            prevents_nearby_leaf_decay: parser.block_tag("minecraft:prevents_nearby_leaf_decay"),
        }
    }

    /// `TreeFeature.validTreePos`: air or replaceable by trees.
    #[inline]
    pub fn valid_tree_pos(&self, level: &WorldGenRegion<'_>, pos: BlockPos) -> bool {
        let state = level.block(pos);
        state.is_air() || self.replaceable_by_trees.contains(state)
    }

    /// `TreeFeature.isAirOrLeaves`.
    #[inline]
    pub fn is_air_or_leaves(&self, level: &WorldGenRegion<'_>, pos: BlockPos) -> bool {
        let state = level.block(pos);
        state.is_air() || self.leaves.contains(state)
    }
}

/// `TreeFeature.isVine`.
#[inline]
pub fn is_vine(level: &WorldGenRegion<'_>, pos: BlockPos) -> bool {
    level.block(pos).block() == ids::VINE
}

/// `minecraft:tree` (`TreeFeature`); the configuration is boxed to keep [`Feature`] small.
///
/// [`Feature`]: crate::feature::Feature
#[derive(Debug)]
pub struct TreeFeature(Box<Tree>);

/// The configuration of a [`TreeFeature`].
#[derive(Debug)]
pub struct Tree {
    trunk_provider: BlockStateProvider,
    trunk_placer: TrunkPlacer,
    foliage_provider: BlockStateProvider,
    foliage_placer: FoliagePlacer,
    root_placer: Option<RootPlacer>,
    minimum_size: FeatureSize,
    decorators: Vec<TreeDecorator>,
    ignore_vines: bool,
    below_trunk_provider: BlockStateProvider,
    blocks: TreeBlocks,
}

impl FeatureType for TreeFeature {
    fn parse(json: &Value, parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        let trunk_provider = parser.state_provider(field(json, "trunk_provider")?)?;
        let trunk_placer = TrunkPlacer::parse(field(json, "trunk_placer")?, parser)?;
        let foliage_provider = parser.state_provider(field(json, "foliage_provider")?)?;
        let foliage_placer = FoliagePlacer::parse(field(json, "foliage_placer")?)?;
        let root_placer = json
            .get("root_placer")
            .map(|j| RootPlacer::parse(j, parser))
            .transpose()?;
        let minimum_size = FeatureSize::parse(field(json, "minimum_size")?)?;
        let decorators = decorator::parse_list(field(json, "decorators")?, parser)?;
        let below_trunk_provider = parser.state_provider(field(json, "below_trunk_provider")?)?;
        Ok(Self(Box::new(Tree {
            trunk_provider,
            trunk_placer,
            foliage_provider,
            foliage_placer,
            root_placer,
            minimum_size,
            decorators,
            ignore_vines: bool_or(json, "ignore_vines", false),
            below_trunk_provider,
            blocks: TreeBlocks::new(parser),
        })))
    }

    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        self.0.place(ctx, origin)
    }
}

impl Tree {
    /// `TreeFeature.place`.
    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        let mut writer = TreeWriter {
            ctx: &mut *ctx,
            tree: self,
            roots: JavaHashSet::new(),
            trunks: JavaHashSet::new(),
            foliage: JavaHashSet::new(),
        };
        let result = self.do_place(&mut writer, origin);
        let TreeWriter {
            roots,
            trunks,
            foliage,
            ..
        } = writer;
        if !result || (trunks.is_empty() && foliage.is_empty()) {
            return false;
        }
        let mut decorations = JavaHashSet::new();
        if !self.decorators.is_empty() {
            let mut context = DecoratorContext::new(
                ctx,
                &self.blocks,
                trunks.to_sorted_by_y(),
                foliage.to_sorted_by_y(),
                roots.to_sorted_by_y(),
                Some(&mut decorations),
            );
            for decorator in &self.decorators {
                decorator.place(&mut context);
            }
        }
        let Some(bounds) = leaves::BoundingBox::encapsulating(
            roots
                .iter()
                .chain(trunks.iter())
                .chain(foliage.iter())
                .chain(decorations.iter()),
        ) else {
            return false;
        };
        let shape = leaves::update_leaves(
            ctx.level,
            &self.blocks,
            &bounds,
            &trunks,
            &decorations,
            &roots,
        );
        leaves::update_shape_at_edge(ctx.level, flags::ALL, &shape, bounds.min());
        true
    }

    /// `TreeFeature.doPlace`.
    fn do_place(&self, w: &mut TreeWriter<'_, '_, '_>, origin: BlockPos) -> bool {
        let tree_height = self.trunk_placer.tree_height(w.ctx.random);
        let foliage_height = self
            .foliage_placer
            .foliage_height(w.ctx.random, tree_height);
        let trunk_height = tree_height - foliage_height;
        let leaf_radius = self
            .foliage_placer
            .foliage_radius(w.ctx.random, trunk_height);
        let trunk_origin = match &self.root_placer {
            Some(root_placer) => root_placer.trunk_origin(origin, w.ctx.random),
            None => origin,
        };
        let min_y = origin.y.min(trunk_origin.y);
        let max_y = origin.y.max(trunk_origin.y) + tree_height + 1;
        let level = &*w.ctx.level;
        if min_y < level.min_y() + 1 || max_y > level.max_y() + 1 {
            return false;
        }
        let clipped_height = self.max_free_tree_height(level, tree_height, trunk_origin);
        let fits = clipped_height >= tree_height
            || self
                .minimum_size
                .min_clipped_height()
                .is_some_and(|min| clipped_height >= min);
        if !fits {
            return false;
        }
        if let Some(root_placer) = &self.root_placer
            && !root_placer.place_roots(w, origin, trunk_origin)
        {
            return false;
        }
        let attachments = self
            .trunk_placer
            .place_trunk(w, clipped_height, trunk_origin);
        for attachment in &attachments {
            self.foliage_placer.create_foliage(
                w,
                clipped_height,
                attachment,
                foliage_height,
                leaf_radius,
            );
        }
        true
    }

    /// `TreeFeature.getMaxFreeTreeHeight`.
    fn max_free_tree_height(
        &self,
        level: &WorldGenRegion<'_>,
        max_tree_height: i32,
        tree_pos: BlockPos,
    ) -> i32 {
        for y in 0..=max_tree_height + 1 {
            let r = self.minimum_size.size_at_height(max_tree_height, y);
            for x in -r..=r {
                for z in -r..=r {
                    let pos = tree_pos.offset(x, y, z);
                    if !self.trunk_placer.is_free(level, &self.blocks, pos)
                        || !self.ignore_vines && is_vine(level, pos)
                    {
                        return y - 2;
                    }
                }
            }
        }
        max_tree_height
    }
}

/// What trunk, foliage and root placers write through: the feature context, the tree's
/// configuration and the positions written so far (the root, trunk and foliage setters of
/// `TreeFeature.place`).
#[derive(Debug)]
pub struct TreeWriter<'w, 'r, 'a> {
    pub ctx: &'w mut FeatureContext<'r, 'a>,
    pub tree: &'w Tree,
    roots: JavaHashSet,
    trunks: JavaHashSet,
    foliage: JavaHashSet,
}

impl TreeWriter<'_, '_, '_> {
    /// The region.
    #[inline]
    pub fn level(&self) -> &WorldGenRegion<'_> {
        self.ctx.level
    }

    /// The decoration random.
    #[inline]
    pub fn random(&mut self) -> &mut DecorationRandom {
        self.ctx.random
    }

    /// The tree's block tags.
    #[inline]
    pub fn blocks(&self) -> &TreeBlocks {
        &self.tree.blocks
    }

    /// `TreeFeature.validTreePos`.
    #[inline]
    pub fn valid_tree_pos(&self, pos: BlockPos) -> bool {
        self.tree.blocks.valid_tree_pos(self.ctx.level, pos)
    }

    /// The root setter.
    pub fn set_root(&mut self, pos: BlockPos, state: BlockState) {
        self.roots.insert(pos);
        self.ctx.level.set_block(pos, state, TREE_UPDATE_FLAGS);
    }

    /// The trunk setter.
    pub fn set_trunk(&mut self, pos: BlockPos, state: BlockState) {
        self.trunks.insert(pos);
        self.ctx.level.set_block(pos, state, TREE_UPDATE_FLAGS);
    }

    /// `FoliageSetter.set`.
    pub fn set_foliage(&mut self, pos: BlockPos, state: BlockState) {
        self.foliage.insert(pos);
        self.ctx.level.set_block(pos, state, TREE_UPDATE_FLAGS);
    }

    /// `FoliageSetter.isSet`.
    pub fn is_foliage_set(&self, pos: BlockPos) -> bool {
        self.foliage.contains(pos)
    }

    /// `trunkProvider.getState(level, random, pos)`.
    pub fn trunk_state(&mut self, pos: BlockPos) -> BlockState {
        self.tree
            .trunk_provider
            .get_state(self.ctx.level, self.ctx.random, pos)
    }

    /// `foliageProvider.getState(level, random, pos)`.
    pub fn foliage_state(&mut self, pos: BlockPos) -> BlockState {
        self.tree
            .foliage_provider
            .get_state(self.ctx.level, self.ctx.random, pos)
    }

    /// `TrunkPlacer.placeBelowTrunkBlock`.
    pub fn place_below_trunk_block(&mut self, pos: BlockPos) {
        if let Some(state) =
            self.tree
                .below_trunk_provider
                .get_optional_state(self.ctx.level, self.ctx.random, pos)
        {
            self.set_trunk(pos, state);
        }
    }
}

// --- Small Vanilla helpers shared by the tree-like features. ---

/// `Direction.Plane.HORIZONTAL` in Vanilla's order.
pub const HORIZONTAL: [Direction; 4] = [
    Direction::North,
    Direction::East,
    Direction::South,
    Direction::West,
];

/// `Direction.Plane.HORIZONTAL.getRandomDirection(random)`.
#[inline]
pub fn random_horizontal<R: RandomSource>(random: &mut R) -> Direction {
    HORIZONTAL[random.next_int_bounded(4) as usize]
}

/// `Direction.getClockWise()` (horizontal directions; vertical ones are returned unchanged,
/// Vanilla throws).
pub const fn clockwise(direction: Direction) -> Direction {
    match direction {
        Direction::North => Direction::East,
        Direction::East => Direction::South,
        Direction::South => Direction::West,
        Direction::West => Direction::North,
        other => other,
    }
}

/// `Direction.getCounterClockWise()` (horizontal directions).
pub const fn counter_clockwise(direction: Direction) -> Direction {
    match direction {
        Direction::North => Direction::West,
        Direction::West => Direction::South,
        Direction::South => Direction::East,
        Direction::East => Direction::North,
        other => other,
    }
}

/// `pos.relative(direction, steps)`.
#[inline]
pub const fn relative(pos: BlockPos, direction: Direction, steps: i32) -> BlockPos {
    let [x, y, z] = direction.normal();
    pos.offset(x * steps, y * steps, z * steps)
}

/// `Vec3i.distManhattan`.
#[inline]
pub const fn dist_manhattan(a: BlockPos, b: BlockPos) -> i32 {
    (b.x - a.x).abs() + (b.y - a.y).abs() + (b.z - a.z).abs()
}

/// `Util.shuffle(list, random)`.
pub fn shuffle<T, R: RandomSource>(list: &mut [T], random: &mut R) {
    for i in (2..=list.len()).rev() {
        let swap_to = random.next_int_bounded(i as i32) as usize;
        list.swap(i - 1, swap_to);
    }
}

/// `Direction.CODEC`: a direction by its serialized name.
pub fn parse_direction(json: &Value) -> Result<Direction, Error> {
    Ok(match json.as_str() {
        Some("down") => Direction::Down,
        Some("up") => Direction::Up,
        Some("north") => Direction::North,
        Some("south") => Direction::South,
        Some("west") => Direction::West,
        Some("east") => Direction::East,
        _ => return Err(Error::new(format!("invalid direction {json}"))),
    })
}

/// The serialized name of a direction (`Direction.getSerializedName`), also the name of the
/// matching boolean block state property (`VineBlock`, `HugeMushroomBlock`).
pub const fn direction_name(direction: Direction) -> &'static str {
    match direction {
        Direction::Down => "down",
        Direction::Up => "up",
        Direction::North => "north",
        Direction::South => "south",
        Direction::West => "west",
        Direction::East => "east",
    }
}

/// `Direction.Axis`, by its block state value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    X,
    Y,
    Z,
}

impl Axis {
    /// The axis of a direction (`Direction.getAxis`).
    pub const fn of(direction: Direction) -> Self {
        match direction {
            Direction::Down | Direction::Up => Self::Y,
            Direction::North | Direction::South => Self::Z,
            Direction::West | Direction::East => Self::X,
        }
    }

    /// The value of the `axis` property.
    pub const fn name(self) -> &'static str {
        match self {
            Self::X => "x",
            Self::Y => "y",
            Self::Z => "z",
        }
    }
}

/// `state.trySetValue(property, value)`: the state with the property set, or unchanged if the
/// block has no such property (or value).
#[inline]
pub fn try_set(state: BlockState, property: &str, value: &str) -> BlockState {
    state.with(property, value).unwrap_or(state)
}

/// [`try_set`] for a boolean property.
#[inline]
pub fn try_set_bool(state: BlockState, property: &str, value: bool) -> BlockState {
    try_set(state, property, if value { "true" } else { "false" })
}

/// [`try_set`] for an integer property.
#[inline]
pub fn try_set_int(state: BlockState, property: &str, value: i32) -> BlockState {
    const DIGITS: [&str; 26] = [
        "0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12", "13", "14", "15", "16",
        "17", "18", "19", "20", "21", "22", "23", "24", "25",
    ];
    match usize::try_from(value).ok().and_then(|v| DIGITS.get(v)) {
        Some(digits) => try_set(state, property, digits),
        None => state,
    }
}

/// `state.trySetValue(RotatedPillarBlock.AXIS, axis)`.
#[inline]
pub fn with_axis(state: BlockState, axis: Axis) -> BlockState {
    try_set(state, "axis", axis.name())
}

/// Whether the state has a property.
#[inline]
pub fn has_property(state: BlockState, property: &str) -> bool {
    state.value(property).is_some()
}

/// Registry ids of `minecraft:fluid` (`Fluids` registration order).
const FLUID_FLOWING_WATER: u16 = 1;
const FLUID_WATER: u16 = 2;

/// `fluid.is(FluidTags.WATER)`.
#[inline]
pub fn is_water(fluid: FluidState) -> bool {
    matches!(fluid.info().fluid, FLUID_WATER | FLUID_FLOWING_WATER)
}

/// `fluid.isSourceOfType(Fluids.WATER)`.
#[inline]
pub fn is_water_source(fluid: FluidState) -> bool {
    let info = fluid.info();
    info.source && info.fluid == FLUID_WATER
}

/// `Feature.markAboveForPostProcessing`: marks up to two non-air blocks above `pos`.
pub fn mark_above_for_post_processing(level: &mut WorldGenRegion<'_>, pos: BlockPos) {
    let mut pos = pos;
    for _ in 0..2 {
        pos = pos.offset(0, 1, 0);
        if level.block(pos).is_air() {
            return;
        }
        level.mark_post_processing(pos);
    }
}

#[cfg(test)]
mod tests;

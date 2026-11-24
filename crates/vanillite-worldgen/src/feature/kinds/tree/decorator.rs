//! Tree decorators (`treedecorators/`): vines, cocoa, beehives, moss, mushrooms and ground
//! cover placed after a tree's logs and leaves.

use rustc_hash::FxHashSet;
use serde_json::Value;
use vanillite_base::random::{DecorationRandom, RandomSource};
use vanillite_base::{BlockPos, Direction};
use vanillite_chunk::HeightmapKind;
use vanillite_data::block::{BlockState, ids};
use vanillite_nbt::{Compound, List, Tag};

use super::{
    HORIZONTAL, JavaHashSet, TREE_UPDATE_FLAGS, TreeBlocks, clockwise, direction_name,
    parse_direction, random_horizontal, relative, shuffle, try_set, try_set_bool, try_set_int,
};
use crate::Error;
use crate::behaviour;
use crate::blocks::qualify;
use crate::feature::{
    BlockStateProvider, FeatureContext, FeatureId, FeatureParser, field, float_field, int_or,
};

/// `TreeDecorator.Context`: the region and random of the placing feature, the tree's logs,
/// leaves and roots sorted by Y, and the decoration setter.
#[derive(Debug)]
pub struct DecoratorContext<'d, 'r, 'a> {
    pub ctx: &'d mut FeatureContext<'r, 'a>,
    pub blocks: &'d TreeBlocks,
    pub logs: Vec<BlockPos>,
    pub leaves: Vec<BlockPos>,
    pub roots: Vec<BlockPos>,
    /// `TreeFeature`'s decoration set; `None` for fallen trees, whose setter records nothing.
    decorations: Option<&'d mut JavaHashSet>,
}

impl<'d, 'r, 'a> DecoratorContext<'d, 'r, 'a> {
    /// A context over the given (already sorted) positions.
    pub fn new(
        ctx: &'d mut FeatureContext<'r, 'a>,
        blocks: &'d TreeBlocks,
        logs: Vec<BlockPos>,
        leaves: Vec<BlockPos>,
        roots: Vec<BlockPos>,
        decorations: Option<&'d mut JavaHashSet>,
    ) -> Self {
        Self {
            ctx,
            blocks,
            logs,
            leaves,
            roots,
            decorations,
        }
    }

    #[inline]
    fn random(&mut self) -> &mut DecorationRandom {
        self.ctx.random
    }

    #[inline]
    fn block(&self, pos: BlockPos) -> BlockState {
        self.ctx.level.block(pos)
    }

    /// `setBlock(pos, state)` through the decoration setter (`setBlock(pos, state, 19)`).
    pub fn set_block(&mut self, pos: BlockPos, state: BlockState) {
        if let Some(decorations) = self.decorations.as_deref_mut() {
            decorations.insert(pos);
        }
        self.ctx.level.set_block(pos, state, TREE_UPDATE_FLAGS);
    }

    /// `placeVine(pos, direction)`.
    fn place_vine(&mut self, pos: BlockPos, side: Direction) {
        let vine = try_set_bool(ids::VINE.default_state(), direction_name(side), true);
        self.set_block(pos, vine);
    }

    /// `isAir(pos)`.
    #[inline]
    fn is_air(&self, pos: BlockPos) -> bool {
        self.block(pos).is_air()
    }

    /// `isReplaceable(pos)`.
    fn is_replaceable(&self, pos: BlockPos) -> bool {
        behaviour::can_be_replaced(self.block(pos))
    }

    /// `isWaterOrWaterNearby(pos)`.
    fn is_water_or_water_nearby(&self, pos: BlockPos) -> bool {
        let water = |p: BlockPos| self.block(p).block() == ids::WATER;
        water(pos)
            || water(pos.offset(1, 0, 0))
            || water(pos.offset(-1, 0, 0))
            || water(pos.offset(0, 0, -1))
            || water(pos.offset(0, 0, 1))
    }

    /// `TreeFeature.getLowestTrunkOrRootOfTree(context)`.
    fn lowest_trunk_or_root(&self) -> Vec<BlockPos> {
        if self.roots.is_empty() {
            self.logs.clone()
        } else if !self.logs.is_empty() && self.roots[0].y == self.logs[0].y {
            let mut positions = self.logs.clone();
            positions.extend_from_slice(&self.roots);
            positions
        } else {
            self.roots.clone()
        }
    }
}

/// A tree decorator (`TreeDecorator`).
#[derive(Clone, Debug)]
pub enum TreeDecorator {
    /// `TrunkVineDecorator`.
    TrunkVine,
    /// `LeaveVineDecorator`.
    LeaveVine { probability: f32 },
    /// `PaleMossDecorator`; `feature` is `minecraft:pale_moss_patch` if registered.
    PaleMoss {
        leaves_probability: f32,
        trunk_probability: f32,
        ground_probability: f32,
        feature: Option<FeatureId>,
    },
    /// `CreakingHeartDecorator`.
    CreakingHeart { probability: f32 },
    /// `CocoaDecorator`.
    Cocoa { probability: f32 },
    /// `ShelfMushroomDecorator`.
    ShelfMushroom { probability: f32 },
    /// `BeehiveDecorator`.
    Beehive { probability: f32 },
    /// `AlterGroundDecorator`.
    AlterGround { provider: BlockStateProvider },
    /// `AttachedToLeavesDecorator`.
    AttachedToLeaves {
        probability: f32,
        exclusion_radius_xz: i32,
        exclusion_radius_y: i32,
        block_provider: BlockStateProvider,
        required_empty_blocks: i32,
        directions: Vec<Direction>,
    },
    /// `PlaceOnGroundDecorator`.
    PlaceOnGround {
        tries: i32,
        radius: i32,
        height: i32,
        block_state_provider: BlockStateProvider,
    },
    /// `AttachedToLogsDecorator`.
    AttachedToLogs {
        probability: f32,
        block_provider: BlockStateProvider,
        directions: Vec<Direction>,
    },
}

fn probability(json: &Value, name: &str) -> Result<f32, Error> {
    let value = float_field(json, name)?;
    if (0.0..=1.0).contains(&value) {
        Ok(value)
    } else {
        Err(Error::new(format!("{name} {value} outside [0, 1]")))
    }
}

fn int_in(
    json: &Value,
    name: &str,
    default: Option<i32>,
    min: i32,
    max: i32,
) -> Result<i32, Error> {
    let value = match default {
        Some(default) => int_or(json, name, default),
        None => crate::feature::int_field(json, name)?,
    };
    if (min..=max).contains(&value) {
        Ok(value)
    } else {
        Err(Error::new(format!("{name} {value} outside [{min}, {max}]")))
    }
}

fn directions(json: &Value) -> Result<Vec<Direction>, Error> {
    let list = field(json, "directions")?
        .as_array()
        .ok_or_else(|| Error::new("directions is not a list"))?;
    if list.is_empty() {
        return Err(Error::new("directions must not be empty"));
    }
    list.iter().map(parse_direction).collect()
}

/// Parses a list of decorators (`TreeDecorator.CODEC.listOf()`).
pub fn parse_list(
    json: &Value,
    parser: &mut FeatureParser<'_>,
) -> Result<Vec<TreeDecorator>, Error> {
    json.as_array()
        .ok_or_else(|| Error::new("decorators is not a list"))?
        .iter()
        .map(|d| TreeDecorator::parse(d, parser))
        .collect()
}

impl TreeDecorator {
    /// Parses `TreeDecorator.CODEC`.
    pub fn parse(json: &Value, parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        let kind = json
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::new("tree decorator without type"))?;
        Ok(match kind.strip_prefix("minecraft:").unwrap_or(kind) {
            "trunk_vine" => Self::TrunkVine,
            "leave_vine" => Self::LeaveVine {
                probability: probability(json, "probability")?,
            },
            "pale_moss" => {
                let name = qualify("pale_moss_patch");
                let registered = parser
                    .packs()
                    .registry("minecraft:worldgen/feature")
                    .and_then(|r| r.get(&name))
                    .is_some();
                Self::PaleMoss {
                    leaves_probability: probability(json, "leaves_probability")?,
                    trunk_probability: probability(json, "trunk_probability")?,
                    ground_probability: probability(json, "ground_probability")?,
                    feature: if registered {
                        Some(parser.feature(&Value::String(name))?)
                    } else {
                        None
                    },
                }
            }
            "creaking_heart" => Self::CreakingHeart {
                probability: probability(json, "probability")?,
            },
            "cocoa" => Self::Cocoa {
                probability: probability(json, "probability")?,
            },
            "shelf_mushroom" => Self::ShelfMushroom {
                probability: probability(json, "probability")?,
            },
            "beehive" => Self::Beehive {
                probability: probability(json, "probability")?,
            },
            "alter_ground" => Self::AlterGround {
                provider: parser.state_provider(field(json, "provider")?)?,
            },
            "attached_to_leaves" => Self::AttachedToLeaves {
                probability: probability(json, "probability")?,
                exclusion_radius_xz: int_in(json, "exclusion_radius_xz", None, 0, 16)?,
                exclusion_radius_y: int_in(json, "exclusion_radius_y", None, 0, 16)?,
                block_provider: parser.state_provider(field(json, "block_provider")?)?,
                required_empty_blocks: int_in(json, "required_empty_blocks", None, 1, 16)?,
                directions: directions(json)?,
            },
            "place_on_ground" => Self::PlaceOnGround {
                tries: int_in(json, "tries", Some(128), 1, i32::MAX)?,
                radius: int_in(json, "radius", Some(2), 0, i32::MAX)?,
                height: int_in(json, "height", Some(1), 0, i32::MAX)?,
                block_state_provider: parser
                    .state_provider(field(json, "block_state_provider")?)?,
            },
            "attached_to_logs" => Self::AttachedToLogs {
                probability: probability(json, "probability")?,
                block_provider: parser.state_provider(field(json, "block_provider")?)?,
                directions: directions(json)?,
            },
            other => return Err(Error::new(format!("unknown tree decorator type {other}"))),
        })
    }

    /// `place(context)`.
    pub fn place(&self, c: &mut DecoratorContext<'_, '_, '_>) {
        match self {
            Self::TrunkVine => trunk_vine(c),
            Self::LeaveVine { probability } => leave_vine(c, *probability),
            Self::PaleMoss {
                leaves_probability,
                trunk_probability,
                ground_probability,
                feature,
            } => pale_moss(
                c,
                *leaves_probability,
                *trunk_probability,
                *ground_probability,
                *feature,
            ),
            Self::CreakingHeart { probability } => creaking_heart(c, *probability),
            Self::Cocoa { probability } => cocoa(c, *probability),
            Self::ShelfMushroom { probability } => shelf_mushroom(c, *probability),
            Self::Beehive { probability } => beehive(c, *probability),
            Self::AlterGround { provider } => alter_ground(c, provider),
            Self::AttachedToLeaves {
                probability,
                exclusion_radius_xz,
                exclusion_radius_y,
                block_provider,
                required_empty_blocks,
                directions,
            } => attached_to_leaves(
                c,
                *probability,
                *exclusion_radius_xz,
                *exclusion_radius_y,
                block_provider,
                *required_empty_blocks,
                directions,
            ),
            Self::PlaceOnGround {
                tries,
                radius,
                height,
                block_state_provider,
            } => place_on_ground(c, *tries, *radius, *height, block_state_provider),
            Self::AttachedToLogs {
                probability,
                block_provider,
                directions,
            } => attached_to_logs(c, *probability, block_provider, directions),
        }
    }
}

/// `TrunkVineDecorator.place`.
fn trunk_vine(c: &mut DecoratorContext<'_, '_, '_>) {
    for i in 0..c.logs.len() {
        let pos = c.logs[i];
        // (vine position offset, the vine's side facing the log)
        for (dx, dz, side) in [
            (-1, 0, Direction::East),
            (1, 0, Direction::West),
            (0, -1, Direction::South),
            (0, 1, Direction::North),
        ] {
            if c.random().next_int_bounded(3) > 0 {
                let vine_pos = pos.offset(dx, 0, dz);
                if c.is_air(vine_pos) {
                    c.place_vine(vine_pos, side);
                }
            }
        }
    }
}

/// `LeaveVineDecorator.place`.
fn leave_vine(c: &mut DecoratorContext<'_, '_, '_>, probability: f32) {
    for i in 0..c.leaves.len() {
        let pos = c.leaves[i];
        for (dx, dz, side) in [
            (-1, 0, Direction::East),
            (1, 0, Direction::West),
            (0, -1, Direction::South),
            (0, 1, Direction::North),
        ] {
            if c.random().next_float() < probability {
                let vine_pos = pos.offset(dx, 0, dz);
                if c.is_air(vine_pos) {
                    add_hanging_vine(c, vine_pos, side);
                }
            }
        }
    }
}

/// `LeaveVineDecorator.addHangingVine`.
fn add_hanging_vine(c: &mut DecoratorContext<'_, '_, '_>, pos: BlockPos, side: Direction) {
    c.place_vine(pos, side);
    let mut max_dir = 4;
    let mut pos = pos.offset(0, -1, 0);
    while c.is_air(pos) && max_dir > 0 {
        c.place_vine(pos, side);
        pos = pos.offset(0, -1, 0);
        max_dir -= 1;
    }
}

/// `PaleMossDecorator.place`.
fn pale_moss(
    c: &mut DecoratorContext<'_, '_, '_>,
    leaves_probability: f32,
    trunk_probability: f32,
    ground_probability: f32,
    feature: Option<FeatureId>,
) {
    let mut logs = c.logs.clone();
    shuffle(&mut logs, c.random());
    // `Collections.min(logs, comparingInt(getY))`: the first lowest in shuffled order.
    let Some(origin) = logs
        .iter()
        .copied()
        .reduce(|min, p| if p.y < min.y { p } else { min })
    else {
        return;
    };
    if c.random().next_float() < ground_probability
        && let Some(feature) = feature
    {
        c.ctx.place_feature(feature, origin.offset(0, 1, 0));
    }
    for i in 0..c.logs.len() {
        if c.random().next_float() < trunk_probability {
            let down = c.logs[i].offset(0, -1, 0);
            if c.is_air(down) {
                add_moss_hanger(c, down);
            }
        }
    }
    for i in 0..c.leaves.len() {
        if c.random().next_float() < leaves_probability {
            let down = c.leaves[i].offset(0, -1, 0);
            if c.is_air(down) {
                add_moss_hanger(c, down);
            }
        }
    }
}

/// `PaleMossDecorator.addMossHanger`.
fn add_moss_hanger(c: &mut DecoratorContext<'_, '_, '_>, pos: BlockPos) {
    let moss = ids::PALE_HANGING_MOSS.default_state();
    let mut pos = pos;
    while c.is_air(pos.offset(0, -1, 0)) && f64::from(c.random().next_float()) >= 0.5 {
        c.set_block(pos, try_set_bool(moss, "tip", false));
        pos = pos.offset(0, -1, 0);
    }
    c.set_block(pos, try_set_bool(moss, "tip", true));
}

/// `CreakingHeartDecorator.place`.
fn creaking_heart(c: &mut DecoratorContext<'_, '_, '_>, probability: f32) {
    if c.logs.is_empty() || c.random().next_float() >= probability {
        return;
    }
    let mut placements = c.logs.clone();
    shuffle(&mut placements, c.random());
    let logs = &c.blocks.logs;
    let target = placements.iter().copied().find(|&pos| {
        Direction::ALL
            .iter()
            .all(|&dir| logs.contains(c.ctx.level.block(pos.relative(dir))))
    });
    if let Some(target) = target {
        let heart = try_set_bool(
            try_set(
                ids::CREAKING_HEART.default_state(),
                "creaking_heart_state",
                "dormant",
            ),
            "natural",
            true,
        );
        c.set_block(target, heart);
    }
}

/// `CocoaDecorator.place`.
fn cocoa(c: &mut DecoratorContext<'_, '_, '_>, probability: f32) {
    if c.random().next_float() >= probability || c.logs.is_empty() {
        return;
    }
    let tree_y = c.logs[0].y;
    for i in 0..c.logs.len() {
        let pos = c.logs[i];
        if pos.y - tree_y > 2 {
            continue;
        }
        for direction in HORIZONTAL {
            if c.random().next_float() <= 0.25 {
                let [ox, _, oz] = direction.opposite().normal();
                let cocoa_pos = pos.offset(ox, 0, oz);
                if c.is_air(cocoa_pos) {
                    let age = c.random().next_int_bounded(3);
                    let state = try_set(
                        try_set_int(ids::COCOA.default_state(), "age", age),
                        "facing",
                        direction_name(direction),
                    );
                    c.set_block(cocoa_pos, state);
                }
            }
        }
    }
}

/// `ShelfMushroomDecorator.place`.
fn shelf_mushroom(c: &mut DecoratorContext<'_, '_, '_>, probability: f32) {
    if c.random().next_float() >= probability || c.logs.is_empty() {
        return;
    }
    let first = c.logs[0];
    let last = c.logs[c.logs.len() - 1];
    if first.y == last.y {
        // `placeOnFallenLog`.
        let directions = if first.x != last.x {
            [Direction::North, Direction::South]
        } else {
            [Direction::East, Direction::West]
        };
        for i in 0..c.logs.len() {
            let log = c.logs[i];
            for facing in directions {
                if c.random().next_float() <= 0.25 {
                    let pos = shelf_mushroom_pos(log, facing);
                    if shelf_replaceable(c, pos)
                        && !has_adjacent_shelf_mushroom(c, pos)
                        && !has_adjacent_shelf_mushroom(c, log)
                    {
                        place_shelf_mushroom(c, pos, facing);
                    }
                }
            }
        }
    } else {
        // `placeOnStandingTree`.
        let first_direction = random_horizontal(c.random());
        let directions = [first_direction, clockwise(first_direction)];
        let base_y = first.y;
        for i in 0..c.logs.len() {
            let log = c.logs[i];
            let dy = log.y - base_y;
            if !(1..=4).contains(&dy) {
                continue;
            }
            for facing in directions {
                if c.random().next_float() <= 0.25 {
                    let pos = shelf_mushroom_pos(log, facing);
                    if shelf_replaceable(c, pos)
                        && c.block(pos.offset(0, -1, 0)).block() != ids::SHELF_MUSHROOM
                    {
                        place_shelf_mushroom(c, pos, facing);
                        break;
                    }
                }
            }
        }
    }
}

/// `ShelfMushroomDecorator.mushroomPosFor`.
fn shelf_mushroom_pos(log: BlockPos, facing: Direction) -> BlockPos {
    let [x, _, z] = facing.normal();
    log.offset(x, 0, z)
}

/// `ShelfMushroomDecorator.isBlockReplaceableWithShelfMushroom`.
fn shelf_replaceable(c: &DecoratorContext<'_, '_, '_>, pos: BlockPos) -> bool {
    c.is_replaceable(pos) && !c.is_water_or_water_nearby(pos)
}

/// `ShelfMushroomDecorator.hasHorizontallyAdjacentShelfMushroom`.
fn has_adjacent_shelf_mushroom(c: &DecoratorContext<'_, '_, '_>, pos: BlockPos) -> bool {
    HORIZONTAL
        .iter()
        .any(|&dir| c.block(pos.relative(dir)).block() == ids::SHELF_MUSHROOM)
}

/// `ShelfMushroomDecorator.placeMushroom`.
fn place_shelf_mushroom(c: &mut DecoratorContext<'_, '_, '_>, pos: BlockPos, facing: Direction) {
    let age = c.random().next_int_bounded(2);
    let state = try_set(
        try_set_int(ids::SHELF_MUSHROOM.default_state(), "age", age),
        "facing",
        direction_name(facing),
    );
    c.set_block(pos, state);
}

/// `BeehiveDecorator.SPAWN_DIRECTIONS`: horizontal directions except north.
const BEEHIVE_SPAWN_DIRECTIONS: [Direction; 3] =
    [Direction::East, Direction::South, Direction::West];

/// `BeehiveDecorator.place`.
///
/// The bees are stored as `BeehiveBlockEntity.Occupant.create(ticks)` saves them:
/// `{entity_data: {id: "minecraft:bee"}, ticks_in_hive, min_ticks_in_hive: 600}` in the block
/// entity's `bees` list.
fn beehive(c: &mut DecoratorContext<'_, '_, '_>, probability: f32) {
    if c.logs.is_empty() || c.random().next_float() >= probability {
        return;
    }
    let first_log = c.logs[0];
    let hive_y = if c.leaves.is_empty() {
        let last_log = c.logs[c.logs.len() - 1];
        (first_log.y + 1 + c.random().next_int_bounded(3)).min(last_log.y)
    } else {
        (c.leaves[0].y - 1).max(first_log.y + 1)
    };
    let mut placements: Vec<BlockPos> = c
        .logs
        .iter()
        .filter(|p| p.y == hive_y)
        .flat_map(|&p| BEEHIVE_SPAWN_DIRECTIONS.map(|d| p.relative(d)))
        .collect();
    if placements.is_empty() {
        return;
    }
    shuffle(&mut placements, c.random());
    let Some(hive_pos) = placements
        .iter()
        .copied()
        .find(|&p| c.is_air(p) && c.is_air(p.relative(Direction::South)))
    else {
        return;
    };
    let nest = try_set(ids::BEE_NEST.default_state(), "facing", "south");
    c.set_block(hive_pos, nest);
    // `getBlockEntity(hivePos, BEEHIVE).ifPresent(...)`.
    let (level, random) = (&mut *c.ctx.level, &mut *c.ctx.random);
    let Some(entity) = level.block_entity_mut(hive_pos) else {
        return;
    };
    if !matches!(entity.get("id"), Some(Tag::String(id)) if id == "minecraft:beehive") {
        return;
    }
    let mut bees = match entity.remove("bees") {
        Some(Tag::List(list)) => list,
        _ => List::new(),
    };
    let num_bees = 2 + random.next_int_bounded(2);
    for _ in 0..num_bees {
        let ticks_in_hive = random.next_int_bounded(599);
        let mut entity_data = Compound::new();
        entity_data.insert("id", Tag::String("minecraft:bee".into()));
        let mut bee = Compound::new();
        bee.insert("entity_data", Tag::Compound(entity_data));
        bee.insert("ticks_in_hive", Tag::Int(ticks_in_hive));
        bee.insert("min_ticks_in_hive", Tag::Int(600));
        bees.0.push(Tag::Compound(bee));
    }
    entity.insert("bees", Tag::List(bees));
}

/// `AlterGroundDecorator.place`.
fn alter_ground(c: &mut DecoratorContext<'_, '_, '_>, provider: &BlockStateProvider) {
    let positions = c.lowest_trunk_or_root();
    let Some(first) = positions.first() else {
        return;
    };
    let min_y = first.y;
    for &pos in positions.iter().filter(|p| p.y == min_y) {
        alter_ground_circle(c, provider, pos.offset(-1, 0, -1));
        alter_ground_circle(c, provider, pos.offset(2, 0, -1));
        alter_ground_circle(c, provider, pos.offset(-1, 0, 2));
        alter_ground_circle(c, provider, pos.offset(2, 0, 2));
        for _ in 0..5 {
            let placement = c.random().next_int_bounded(64);
            let xx = placement % 8;
            let zz = placement / 8;
            if xx == 0 || xx == 7 || zz == 0 || zz == 7 {
                alter_ground_circle(c, provider, pos.offset(-3 + xx, 0, -3 + zz));
            }
        }
    }
}

/// `AlterGroundDecorator.placeCircle`.
fn alter_ground_circle(
    c: &mut DecoratorContext<'_, '_, '_>,
    provider: &BlockStateProvider,
    pos: BlockPos,
) {
    for xx in -2..=2_i32 {
        for zz in -2..=2_i32 {
            if xx.abs() != 2 || zz.abs() != 2 {
                alter_ground_block(c, provider, pos.offset(xx, 0, zz));
            }
        }
    }
}

/// `AlterGroundDecorator.placeBlockAt`.
fn alter_ground_block(
    c: &mut DecoratorContext<'_, '_, '_>,
    provider: &BlockStateProvider,
    pos: BlockPos,
) {
    for dy in (-3..=2).rev() {
        let cursor = pos.offset(0, dy, 0);
        if let Some(state) = provider.get_optional_state(c.ctx.level, c.ctx.random, cursor) {
            c.set_block(cursor, state);
            break;
        }
        if !c.is_air(cursor) && dy < 0 {
            break;
        }
    }
}

/// `AttachedToLeavesDecorator.place`.
#[allow(clippy::too_many_arguments)]
fn attached_to_leaves(
    c: &mut DecoratorContext<'_, '_, '_>,
    probability: f32,
    exclusion_radius_xz: i32,
    exclusion_radius_y: i32,
    block_provider: &BlockStateProvider,
    required_empty_blocks: i32,
    directions: &[Direction],
) {
    let mut blacklist: FxHashSet<BlockPos> = FxHashSet::default();
    let mut leaves = c.leaves.clone();
    shuffle(&mut leaves, c.random());
    for leaf in leaves {
        let direction = directions[c.random().next_int_bounded(directions.len() as i32) as usize];
        let placement = leaf.relative(direction);
        if !blacklist.contains(&placement)
            && c.random().next_float() < probability
            && (1..=required_empty_blocks).all(|i| c.is_air(relative(leaf, direction, i)))
        {
            for x in -exclusion_radius_xz..=exclusion_radius_xz {
                for y in -exclusion_radius_y..=exclusion_radius_y {
                    for z in -exclusion_radius_xz..=exclusion_radius_xz {
                        blacklist.insert(placement.offset(x, y, z));
                    }
                }
            }
            let state = block_provider.get_state(c.ctx.level, c.ctx.random, placement);
            c.set_block(placement, state);
        }
    }
}

/// `PlaceOnGroundDecorator.place`.
fn place_on_ground(
    c: &mut DecoratorContext<'_, '_, '_>,
    tries: i32,
    radius: i32,
    height: i32,
    provider: &BlockStateProvider,
) {
    let positions = c.lowest_trunk_or_root();
    let Some(&origin) = positions.first() else {
        return;
    };
    let min_y = origin.y;
    let (mut min_x, mut max_x, mut min_z, mut max_z) = (origin.x, origin.x, origin.z, origin.z);
    for p in positions.iter().filter(|p| p.y == min_y) {
        min_x = min_x.min(p.x);
        max_x = max_x.max(p.x);
        min_z = min_z.min(p.z);
        max_z = max_z.max(p.z);
    }
    // `new BoundingBox(minX, minY, minZ, maxX, minY, maxZ).inflatedBy(radius, height, radius)`.
    let (bx0, by0, bz0) = (min_x - radius, min_y - height, min_z - radius);
    let (bx1, by1, bz1) = (max_x + radius, min_y + height, max_z + radius);
    for _ in 0..tries {
        let random = c.random();
        let x = random.next_int_between_inclusive(bx0, bx1);
        let y = random.next_int_between_inclusive(by0, by1);
        let z = random.next_int_between_inclusive(bz0, bz1);
        let pos = BlockPos::new(x, y, z);
        // `attemptToPlaceBlockAbove`.
        let above = pos.offset(0, 1, 0);
        let above_state = c.block(above);
        if (above_state.is_air() || above_state.block() == ids::VINE)
            && behaviour::is_solid_render(c.block(pos))
            && c.ctx
                .level
                .heightmap_pos(HeightmapKind::MotionBlockingNoLeaves, pos)
                .y
                <= above.y
        {
            let state = provider.get_state(c.ctx.level, c.ctx.random, above);
            c.set_block(above, state);
        }
    }
}

/// `AttachedToLogsDecorator.place`.
fn attached_to_logs(
    c: &mut DecoratorContext<'_, '_, '_>,
    probability: f32,
    block_provider: &BlockStateProvider,
    directions: &[Direction],
) {
    let mut logs = c.logs.clone();
    shuffle(&mut logs, c.random());
    for log in logs {
        let direction = directions[c.random().next_int_bounded(directions.len() as i32) as usize];
        let placement = log.relative(direction);
        if c.random().next_float() <= probability && c.is_air(placement) {
            let state = block_provider.get_state(c.ctx.level, c.ctx.random, placement);
            c.set_block(placement, state);
        }
    }
}

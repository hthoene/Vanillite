//! `JigsawStructure`: structures assembled from template pools by `JigsawPlacement` (villages,
//! pillager outposts, bastion remnants, ancient cities, trail ruins, trial chambers, abandoned
//! camps).
//!
//! - [`pools`]: `StructureTemplatePool` and the pool aliases;
//! - [`element`]: the pool elements and their jigsaw blocks;
//! - [`placement`]: `JigsawPlacement.addPieces` and its `Placer`;
//! - the pieces are [`crate::structure::pool::PoolElementStructurePiece`]s.

pub mod element;
pub mod placement;
pub mod pools;

use serde_json::Value;
use vanillite_base::BlockPos;
use vanillite_chunk::HeightmapKind;

use self::pools::{PoolAliasBinding, PoolAliasLookup, PoolId, PoolsBuilder, Symbol, TemplatePools};
use crate::Error;
use crate::blocks::qualify;
use crate::carver::providers::HeightProvider;
use crate::feature::field;
use crate::structure::{
    GenerationContext, GenerationStub, LevelHeight, StructureParser, StructureType,
    TerrainAdjustment,
};
use crate::template::processor::heightmap_by_name;
use crate::template::{BoundingBox, LiquidSettings};

/// `JigsawStructure.MAX_TOTAL_STRUCTURE_RANGE`.
const MAX_TOTAL_STRUCTURE_RANGE: i32 = 128;
/// `JigsawStructure.DEFAULT_LIQUID_SETTINGS`.
pub const DEFAULT_LIQUID_SETTINGS: LiquidSettings = LiquidSettings::ApplyWaterlogging;
/// `DimensionType.Y_SIZE`: the default (and largest) vertical distance from the centre.
const Y_SIZE: i32 = (1 << 12) - 32;

/// `JigsawStructure.MaxDistance`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MaxDistance {
    pub horizontal: i32,
    pub vertical: i32,
}

impl MaxDistance {
    /// Parses `MaxDistance.CODEC`: `{horizontal, vertical?}` or a number for both.
    fn parse(json: &Value) -> Result<Self, Error> {
        let int = |value: &Value, min: i32, max: i32, what: &str| -> Result<i32, Error> {
            value
                .as_i64()
                .and_then(|v| i32::try_from(v).ok())
                .filter(|v| (min..=max).contains(v))
                .ok_or_else(|| Error::new(format!("{what} {value} outside [{min}, {max}]")))
        };
        if json.is_object() {
            let horizontal = int(field(json, "horizontal")?, 1, 128, "horizontal distance")?;
            let vertical = match json.get("vertical") {
                Some(v) => int(v, 1, Y_SIZE, "vertical distance")?,
                None => Y_SIZE,
            };
            return Ok(Self {
                horizontal,
                vertical,
            });
        }
        let value = int(json, 1, 128, "max distance")?;
        Ok(Self {
            horizontal: value,
            vertical: value,
        })
    }
}

/// `DimensionPadding`: blocks the centre piece keeps from the bottom and top of the level.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DimensionPadding {
    pub bottom: i32,
    pub top: i32,
}

impl DimensionPadding {
    /// Parses `DimensionPadding.CODEC`: a non-negative number or `{bottom?, top?}` (lenient:
    /// invalid values fall back to 0).
    fn parse(json: &Value) -> Result<Self, Error> {
        let non_negative = |value: &Value| {
            value
                .as_i64()
                .and_then(|v| i32::try_from(v).ok())
                .filter(|v| *v >= 0)
        };
        if json.is_object() {
            let lenient = |name: &str| json.get(name).and_then(non_negative).unwrap_or(0);
            return Ok(Self {
                bottom: lenient("bottom"),
                top: lenient("top"),
            });
        }
        let value = non_negative(json)
            .ok_or_else(|| Error::new(format!("invalid dimension padding {json}")))?;
        Ok(Self {
            bottom: value,
            top: value,
        })
    }

    /// `JigsawPlacement.isStartTooCloseToWorldHeightLimits(heightAccessor, padding, box)`.
    fn is_start_too_close(&self, height: LevelHeight, bb: &BoundingBox) -> bool {
        if *self == Self::default() {
            return false;
        }
        bb.min_y() < height.min_y + self.bottom || bb.max_y() > height.max_y() - self.top
    }
}

/// `JigsawStructure`.
#[derive(Debug)]
pub struct JigsawStructure {
    /// The pools the structure can reach.
    pub pools: TemplatePools,
    /// `startPool`.
    pub start_pool: PoolId,
    /// `startJigsawName`.
    start_jigsaw_name: Option<Symbol>,
    /// `maxDepth` (`size`, 0 to 20).
    pub max_depth: i32,
    /// `startHeight`.
    pub start_height: HeightProvider,
    /// `useExpansionHack`.
    pub use_expansion_hack: bool,
    /// `projectStartToHeightmap`.
    pub project_start_to_heightmap: Option<HeightmapKind>,
    /// `maxDistanceFromCenter`.
    pub max_distance_from_center: MaxDistance,
    /// `poolAliases`.
    pub pool_aliases: Vec<PoolAliasBinding>,
    /// `dimensionPadding`.
    pub dimension_padding: DimensionPadding,
    /// `liquidSettings`.
    pub liquid_settings: LiquidSettings,
}

impl StructureType for JigsawStructure {
    fn parse(json: &Value, parser: &mut StructureParser<'_>) -> Result<Self, Error> {
        let mut pools = PoolsBuilder::new();
        let start_pool = pools.holder(field(json, "start_pool")?, parser)?;
        let start_jigsaw_name = match json.get("start_jigsaw_name") {
            None => None,
            Some(name) => Some(
                pools
                    .symbol(&qualify(name.as_str().ok_or_else(|| {
                        Error::new("start_jigsaw_name is not a string")
                    })?)),
            ),
        };
        let max_depth = field(json, "size")?
            .as_i64()
            .filter(|size| (0..=20).contains(size))
            .ok_or_else(|| Error::new("size outside [0, 20]"))? as i32;
        let start_height = HeightProvider::from_json(field(json, "start_height")?)?;
        let use_expansion_hack = field(json, "use_expansion_hack")?
            .as_bool()
            .ok_or_else(|| Error::new("use_expansion_hack is not a boolean"))?;
        let project_start_to_heightmap = match json.get("project_start_to_heightmap") {
            None => None,
            Some(name) => Some(heightmap_by_name(name.as_str().ok_or_else(|| {
                Error::new("project_start_to_heightmap is not a string")
            })?)?),
        };
        let max_distance_from_center =
            MaxDistance::parse(field(json, "max_distance_from_center")?)?;
        let pool_aliases = match json.get("pool_aliases") {
            None => Vec::new(),
            Some(list) => list
                .as_array()
                .ok_or_else(|| Error::new("pool_aliases is not a list"))?
                .iter()
                .map(|binding| PoolAliasBinding::parse(binding, &mut pools))
                .collect::<Result<_, _>>()?,
        };
        let dimension_padding = match json.get("dimension_padding") {
            None => DimensionPadding::default(),
            Some(padding) => DimensionPadding::parse(padding)?,
        };
        let liquid_settings = match json.get("liquid_settings") {
            None => DEFAULT_LIQUID_SETTINGS,
            Some(value) => value
                .as_str()
                .and_then(LiquidSettings::from_name)
                .ok_or_else(|| Error::new(format!("invalid liquid settings {value}")))?,
        };
        // `verifyRange`.
        let adapted = match json.get("terrain_adaptation").and_then(Value::as_str) {
            Some(name) => TerrainAdjustment::from_name(name)? != TerrainAdjustment::None,
            None => false,
        };
        let edge_needed = if adapted { 12 } else { 0 };
        if max_distance_from_center.horizontal + edge_needed > MAX_TOTAL_STRUCTURE_RANGE {
            return Err(Error::new(
                "Horizontal structure size including terrain adaptation must not exceed 128",
            ));
        }
        Ok(Self {
            pools: pools.finish(parser)?,
            start_pool,
            start_jigsaw_name,
            max_depth,
            start_height,
            use_expansion_hack,
            project_start_to_heightmap,
            max_distance_from_center,
            pool_aliases,
            dimension_padding,
            liquid_settings,
        })
    }

    fn find_generation_point<'s>(
        &'s self,
        ctx: &mut GenerationContext<'_>,
    ) -> Option<GenerationStub<'s>> {
        let chunk_pos = ctx.chunk_pos;
        let context = ctx.generator.generation_context(ctx.height);
        let height = self.start_height.sample(&mut ctx.random.random, &context);
        let start = BlockPos::new(chunk_pos.min_block_x(), height, chunk_pos.min_block_z());
        let aliases = PoolAliasLookup::create(&self.pool_aliases, start, ctx.seed);
        placement::add_pieces(self, ctx, start, aliases)
    }
}

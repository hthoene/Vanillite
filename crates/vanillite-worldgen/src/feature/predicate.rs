//! Block predicates (`net.minecraft.world.level.levelgen.blockpredicates`): tests on a position
//! of the region.
//!
//! Every type of `BlockPredicateType` is ported. Offsets (`offset`, `Vec3i.offsetCodec(16)`) are
//! part of the predicates that have one and are applied before the state is read.

use serde_json::Value;
use vanillite_base::{BlockPos, Direction};
use vanillite_chunk::BiomeId;
use vanillite_data::block::BlockState;
use vanillite_data::registry::FLUID;

use super::FeatureParser;
use super::region::WorldGenRegion;
use crate::Error;
use crate::anchor::{VerticalAnchor, codec_int};
use crate::behaviour::{self, SupportType};
use crate::blocks::{BlockSet, parse_block_state, qualify};

/// A block predicate (`BlockPredicate`).
#[derive(Clone, Debug)]
pub enum BlockPredicate {
    /// `true` (`TrueBlockPredicate`).
    True,
    /// `matching_blocks` (`MatchingBlocksPredicate`): the state's block is in the set.
    MatchingBlocks { offset: BlockPos, blocks: BlockSet },
    /// `matching_block_tag` (`MatchingBlockTagPredicate`): the state's block is in the tag.
    MatchingBlockTag { offset: BlockPos, tag: BlockSet },
    /// `matching_fluids` (`MatchingFluidsPredicate`): the state's fluid type is in the set.
    MatchingFluids { offset: BlockPos, fluids: FluidSet },
    /// `matching_biomes` (`MatchingBiomesPredicate`): the biome at the position is in the set.
    MatchingBiomes { biomes: Vec<BiomeId> },
    /// `has_sturdy_face` (`HasSturdyFacePredicate`): `isFaceSturdy(direction, FULL)`.
    HasSturdyFace {
        offset: BlockPos,
        direction: Direction,
    },
    /// `solid` (`SolidPredicate`): the legacy `isSolid` flag.
    Solid { offset: BlockPos },
    /// `replaceable` (`ReplaceablePredicate`): `canBeReplaced`.
    Replaceable { offset: BlockPos },
    /// `would_survive` (`WouldSurvivePredicate`): `state.canSurvive(level, pos)`.
    WouldSurvive { offset: BlockPos, state: BlockState },
    /// `inside_world_bounds` (`InsideWorldBoundsPredicate`): inside the build height.
    InsideWorldBounds { offset: BlockPos },
    /// `any_of` (`AnyOfPredicate`).
    AnyOf(Vec<BlockPredicate>),
    /// `all_of` (`AllOfPredicate`).
    AllOf(Vec<BlockPredicate>),
    /// `not` (`NotPredicate`).
    Not(Box<BlockPredicate>),
    /// `unobstructed` (`UnobstructedPredicate`): no entity collides with the block's cube.
    ///
    /// A `WorldGenRegion` has no entities (`getEntities` is always empty), so this is always
    /// true during generation; the offset is parsed but, as in Vanilla, never used.
    Unobstructed { offset: BlockPos },
    /// `height_range` (`HeightRangePredicate`): the Y lies within the resolved anchors.
    HeightRange {
        min_inclusive: VerticalAnchor,
        max_inclusive: VerticalAnchor,
    },
    /// `volume_match` (`VolumeMatchPredicate`): `predicate` holds at every offset of the box
    /// `min..=max`.
    VolumeMatch {
        min: BlockPos,
        max: BlockPos,
        predicate: Box<BlockPredicate>,
    },
}

/// A set of fluid types (`HolderSet<Fluid>`), one bit per `minecraft:fluid` registry id.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FluidSet(u64);

impl FluidSet {
    /// Whether the fluid type with registry id `fluid` is in the set.
    #[inline]
    pub fn contains(self, fluid: u16) -> bool {
        fluid < 64 && self.0 & (1 << fluid) != 0
    }

    /// Parses a `HolderSet<Fluid>`: an id, a list of ids or a `#tag`.
    pub fn parse(json: &Value, parser: &FeatureParser<'_>) -> Result<Self, Error> {
        let mut bits = 0u64;
        for name in parser.resolve_set(json, "minecraft:fluid")? {
            let id = FLUID
                .id(&name)
                .filter(|&id| id < 64)
                .ok_or_else(|| Error::new(format!("unknown fluid {name}")))?;
            bits |= 1 << id;
        }
        Ok(Self(bits))
    }
}

impl BlockPredicate {
    /// `BlockPredicate.alwaysTrue()`.
    pub const fn always_true() -> Self {
        Self::True
    }

    /// Parses `BlockPredicate.CODEC` (dispatch on `type`).
    pub fn parse(json: &Value, parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        let kind = json
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::new(format!("block predicate without type: {json}")))?;
        let offset = || optional_offset(json, "offset");
        let predicate = match kind.strip_prefix("minecraft:").unwrap_or(kind) {
            "true" => Self::True,
            "matching_blocks" => Self::MatchingBlocks {
                offset: offset()?,
                blocks: parser.block_set(field(json, "blocks")?)?,
            },
            "matching_block_tag" => {
                let tag = field(json, "tag")?
                    .as_str()
                    .ok_or_else(|| Error::new("matching_block_tag: tag is not a string"))?;
                if tag.starts_with('#') {
                    return Err(Error::new(format!(
                        "matching_block_tag: tag {tag} must not start with #"
                    )));
                }
                Self::MatchingBlockTag {
                    offset: offset()?,
                    tag: parser.block_tag(tag),
                }
            }
            "matching_fluids" => Self::MatchingFluids {
                offset: offset()?,
                fluids: FluidSet::parse(field(json, "fluids")?, parser)?,
            },
            "matching_biomes" => Self::MatchingBiomes {
                biomes: parser.biome_set(field(json, "biomes")?)?,
            },
            "has_sturdy_face" => Self::HasSturdyFace {
                offset: offset()?,
                direction: parse_direction(field(json, "direction")?)?,
            },
            "solid" => Self::Solid { offset: offset()? },
            "replaceable" => Self::Replaceable { offset: offset()? },
            "would_survive" => Self::WouldSurvive {
                offset: offset()?,
                state: parse_block_state(field(json, "state")?)?,
            },
            "inside_world_bounds" => Self::InsideWorldBounds { offset: offset()? },
            "any_of" => Self::AnyOf(parse_list(json, parser)?),
            "all_of" => Self::AllOf(parse_list(json, parser)?),
            "not" => Self::Not(Box::new(parser.block_predicate(field(json, "predicate")?)?)),
            "unobstructed" => Self::Unobstructed {
                offset: match json.get("offset") {
                    None => BlockPos::ZERO,
                    Some(value) => parse_vec3i(value, None)?,
                },
            },
            "height_range" => Self::HeightRange {
                min_inclusive: VerticalAnchor::parse(field(json, "min_inclusive")?)?,
                max_inclusive: VerticalAnchor::parse(field(json, "max_inclusive")?)?,
            },
            "volume_match" => {
                let min = parse_vec3i(field(json, "min")?, Some(16))?;
                let max = parse_vec3i(field(json, "max")?, Some(16))?;
                if min.x > max.x || min.y > max.y || min.z > max.z {
                    return Err(Error::new(
                        "volume_match: min bound cannot be larger than max bound",
                    ));
                }
                Self::VolumeMatch {
                    min,
                    max,
                    predicate: Box::new(parser.block_predicate(field(json, "match")?)?),
                }
            }
            other => return Err(Error::new(format!("unknown block predicate type {other}"))),
        };
        Ok(predicate)
    }

    /// `test(level, pos)`.
    pub fn test(&self, level: &WorldGenRegion<'_>, pos: BlockPos) -> bool {
        match self {
            Self::True | Self::Unobstructed { .. } => true,
            Self::MatchingBlocks { offset, blocks } => {
                blocks.contains(state_at(level, pos, *offset))
            }
            Self::MatchingBlockTag { offset, tag } => tag.contains(state_at(level, pos, *offset)),
            Self::MatchingFluids { offset, fluids } => {
                fluids.contains(state_at(level, pos, *offset).fluid_state().info().fluid)
            }
            Self::MatchingBiomes { biomes } => biomes.contains(&level.biome(pos)),
            Self::HasSturdyFace { offset, direction } => behaviour::is_face_sturdy(
                state_at(level, pos, *offset),
                *direction,
                SupportType::Full,
            ),
            Self::Solid { offset } => behaviour::is_solid(state_at(level, pos, *offset)),
            Self::Replaceable { offset } => {
                behaviour::can_be_replaced(state_at(level, pos, *offset))
            }
            Self::WouldSurvive { offset, state } => {
                behaviour::can_survive(level, *state, offset_pos(pos, *offset))
            }
            Self::InsideWorldBounds { offset } => {
                !level.is_outside_build_height(offset_pos(pos, *offset).y)
            }
            Self::AnyOf(predicates) => predicates.iter().any(|p| p.test(level, pos)),
            Self::AllOf(predicates) => predicates.iter().all(|p| p.test(level, pos)),
            Self::Not(predicate) => !predicate.test(level, pos),
            Self::HeightRange {
                min_inclusive,
                max_inclusive,
            } => {
                let context = level.generation_context();
                let min = min_inclusive.resolve_y(&context);
                let max = max_inclusive.resolve_y(&context);
                pos.y >= min && pos.y <= max
            }
            Self::VolumeMatch {
                min,
                max,
                predicate,
            } => {
                for ox in min.x..=max.x {
                    for oz in min.z..=max.z {
                        for oy in min.y..=max.y {
                            if !predicate.test(level, pos.offset(ox, oy, oz)) {
                                return false;
                            }
                        }
                    }
                }
                true
            }
        }
    }
}

#[inline]
fn offset_pos(pos: BlockPos, offset: BlockPos) -> BlockPos {
    pos.offset(offset.x, offset.y, offset.z)
}

/// `level.getBlockState(origin.offset(offset))`.
#[inline]
fn state_at(level: &WorldGenRegion<'_>, pos: BlockPos, offset: BlockPos) -> BlockState {
    level.block(offset_pos(pos, offset))
}

fn field<'v>(json: &'v Value, name: &str) -> Result<&'v Value, Error> {
    json.get(name)
        .ok_or_else(|| Error::new(format!("block predicate: missing field {name} in {json}")))
}

/// `CombiningPredicate.codec`: the `predicates` list.
fn parse_list(json: &Value, parser: &mut FeatureParser<'_>) -> Result<Vec<BlockPredicate>, Error> {
    field(json, "predicates")?
        .as_array()
        .ok_or_else(|| Error::new("block predicate: predicates is not a list"))?
        .iter()
        .map(|p| parser.block_predicate(p))
        .collect()
}

/// `Vec3i.offsetCodec(16).optionalFieldOf(name, Vec3i.ZERO)`.
fn optional_offset(json: &Value, name: &str) -> Result<BlockPos, Error> {
    match json.get(name) {
        None => Ok(BlockPos::ZERO),
        Some(value) => parse_vec3i(value, Some(16)),
    }
}

/// `Vec3i.CODEC` (`[x, y, z]`), or `Vec3i.offsetCodec(max)` when `max_offset` is given (every
/// component within `-max..=max`).
pub fn parse_vec3i(json: &Value, max_offset: Option<i32>) -> Result<BlockPos, Error> {
    let items = json
        .as_array()
        .filter(|items| items.len() == 3)
        .ok_or_else(|| Error::new(format!("vec3i: expected [x, y, z], got {json}")))?;
    let mut components = [0; 3];
    for (component, item) in components.iter_mut().zip(items) {
        let value =
            codec_int(item).ok_or_else(|| Error::new(format!("vec3i: {item} is not an int")))?;
        if let Some(max) = max_offset
            && !(-max..=max).contains(&value)
        {
            return Err(Error::new(format!(
                "vec3i: position out of range, expected at most {max}: {json}"
            )));
        }
        *component = value;
    }
    Ok(BlockPos::new(components[0], components[1], components[2]))
}

/// `Direction.CODEC`: a direction by its serialized name (`down`, `up`, `north`, …).
pub fn parse_direction(json: &Value) -> Result<Direction, Error> {
    let name = json
        .as_str()
        .ok_or_else(|| Error::new(format!("direction: {json} is not a string")))?;
    Ok(match name {
        "down" => Direction::Down,
        "up" => Direction::Up,
        "north" => Direction::North,
        "south" => Direction::South,
        "west" => Direction::West,
        "east" => Direction::East,
        other => return Err(Error::new(format!("unknown direction {other}"))),
    })
}

/// The blocks of `HolderSet<Block>` in Vanilla's order (direct lists keep their order and
/// duplicates; tags are in tag order), for the types that pick one of them by index.
pub fn ordered_blocks(json: &Value, parser: &FeatureParser<'_>) -> Result<Vec<BlockState>, Error> {
    parser
        .resolve_set(json, "minecraft:block")?
        .iter()
        .map(|name| {
            vanillite_data::block::BlockId::by_name(&qualify(name))
                .map(|block| block.default_state())
                .ok_or_else(|| Error::new(format!("unknown block {name}")))
        })
        .collect()
}

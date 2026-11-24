//! Structure placement (`world/level/levelgen/structure/placement`): which chunks may hold a
//! start of a structure set.

use std::sync::OnceLock;

use serde_json::Value;
use vanillite_base::ChunkPos;
use vanillite_base::random::{LegacyRandom, RandomSource};

use super::state::StructureState;
use super::{BiomeFilter, StructureParser, StructureSetId};
use crate::Error;
use crate::feature::{field, float_or, int_field};
use crate::random::WorldgenRandom;

/// `RandomSpreadType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RandomSpreadType {
    Linear,
    Triangular,
}

impl RandomSpreadType {
    /// `evaluate(random, limit)`.
    pub fn evaluate(self, random: &mut LegacyRandom, limit: i32) -> i32 {
        match self {
            Self::Linear => random.next_int_bounded(limit),
            Self::Triangular => {
                (random.next_int_bounded(limit) + random.next_int_bounded(limit)) / 2
            }
        }
    }
}

/// `AbstractSpreadingStructurePlacement.FrequencyReductionMethod`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrequencyReductionMethod {
    /// `default`: `probabilityReducer`.
    Default,
    /// `legacy_type_1`: `legacyPillagerOutpostReducer`.
    LegacyType1,
    /// `legacy_type_2`: `legacyArbitrarySaltProbabilityReducer`.
    LegacyType2,
    /// `legacy_type_3`: `legacyProbabilityReducerWithDouble`.
    LegacyType3,
}

/// `AbstractSpreadingStructurePlacement.HIGHLY_ARBITRARY_RANDOM_SALT`.
const HIGHLY_ARBITRARY_RANDOM_SALT: i32 = 10_387_320;

impl FrequencyReductionMethod {
    fn from_name(name: &str) -> Result<Self, Error> {
        Ok(match name {
            "default" => Self::Default,
            "legacy_type_1" => Self::LegacyType1,
            "legacy_type_2" => Self::LegacyType2,
            "legacy_type_3" => Self::LegacyType3,
            other => {
                return Err(Error::new(format!(
                    "unknown frequency reduction method {other}"
                )));
            }
        })
    }

    /// `shouldGenerate(seed, salt, sourceX, sourceZ, probability)`.
    pub fn should_generate(self, seed: i64, salt: i32, x: i32, z: i32, probability: f32) -> bool {
        match self {
            Self::Default => {
                let mut random = WorldgenRandom::new(0);
                random.set_large_feature_with_salt(seed, x, z, salt);
                random.random.next_float() < probability
            }
            Self::LegacyType1 => {
                let (cx, cz) = (x >> 4, z >> 4);
                let mut random = WorldgenRandom::new(0);
                random.random.set_seed(i64::from(cx ^ (cz << 4)) ^ seed);
                random.random.next_int();
                random.random.next_int_bounded((1.0 / probability) as i32) == 0
            }
            Self::LegacyType2 => {
                let mut random = WorldgenRandom::new(0);
                random.set_large_feature_with_salt(seed, x, z, HIGHLY_ARBITRARY_RANDOM_SALT);
                random.random.next_float() < probability
            }
            Self::LegacyType3 => {
                let mut random = WorldgenRandom::new(0);
                random.set_large_feature_seed(seed, x, z);
                random.random.next_double() < f64::from(probability)
            }
        }
    }
}

/// `AbstractSpreadingStructurePlacement.ExclusionZone`: no start within `chunk_count` chunks of
/// a start chunk of another set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExclusionZone {
    pub other_set: StructureSetId,
    pub chunk_count: i32,
}

/// What every spreading placement shares (`AbstractSpreadingStructurePlacement`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spreading {
    /// `locate_offset` (only used by `/locate`).
    pub locate_offset: [i32; 3],
    pub frequency_reduction_method: FrequencyReductionMethod,
    pub frequency: f32,
    pub salt: i32,
    pub exclusion_zone: Option<ExclusionZone>,
}

/// `ConcentricRingsStructurePlacement`'s own fields, with the ring positions
/// `ChunkGeneratorStructureState` computes for it once.
#[derive(Clone, Debug)]
pub struct ConcentricRings {
    pub distance: i32,
    pub spread: i32,
    pub count: i32,
    pub preferred_biomes: BiomeFilter,
    /// Whether the set has a structure that can generate in the dimension (only then
    /// `generatePositions` computes ring positions; otherwise there are none).
    pub(crate) placeable: bool,
    /// `ringPositions.get(placement)`, computed on first use.
    pub(crate) positions: OnceLock<Vec<ChunkPos>>,
}

/// A `StructurePlacement`.
#[derive(Clone, Debug)]
pub enum StructurePlacement {
    /// `minecraft:random_spread`.
    RandomSpread {
        common: Spreading,
        spacing: i32,
        separation: i32,
        spread_type: RandomSpreadType,
    },
    /// `minecraft:concentric_rings`.
    ConcentricRings {
        common: Spreading,
        rings: ConcentricRings,
    },
    /// `minecraft:dimension_origin` (`DimensionOriginStructurePlacement`).
    DimensionOrigin,
}

impl StructurePlacement {
    /// Parses `StructurePlacement.CODEC`; `other_set` references resolve through `parser`.
    pub(crate) fn parse(json: &Value, parser: &mut StructureParser<'_>) -> Result<Self, Error> {
        let kind = json
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::new("structure placement without type"))?;
        let kind = kind.strip_prefix("minecraft:").unwrap_or(kind);
        if kind == "dimension_origin" {
            return Ok(Self::DimensionOrigin);
        }
        let common = Spreading::parse(json, parser)?;
        match kind {
            "random_spread" => {
                let spacing = int_field(json, "spacing")?;
                let separation = int_field(json, "separation")?;
                if spacing <= separation {
                    return Err(Error::new("Spacing has to be larger than separation"));
                }
                let spread_type = match json.get("spread_type").and_then(Value::as_str) {
                    None | Some("linear") => RandomSpreadType::Linear,
                    Some("triangular") => RandomSpreadType::Triangular,
                    Some(other) => return Err(Error::new(format!("unknown spread type {other}"))),
                };
                Ok(Self::RandomSpread {
                    common,
                    spacing,
                    separation,
                    spread_type,
                })
            }
            "concentric_rings" => Ok(Self::ConcentricRings {
                common,
                rings: ConcentricRings {
                    distance: int_field(json, "distance")?,
                    spread: int_field(json, "spread")?,
                    count: int_field(json, "count")?,
                    preferred_biomes: parser.biome_filter(field(json, "preferred_biomes")?)?,
                    placeable: false,
                    positions: OnceLock::new(),
                },
            }),
            other => Err(Error::new(format!("unknown structure placement {other}"))),
        }
    }

    /// The spreading fields, if this is a spreading placement.
    pub fn spreading(&self) -> Option<&Spreading> {
        match self {
            Self::RandomSpread { common, .. } | Self::ConcentricRings { common, .. } => {
                Some(common)
            }
            Self::DimensionOrigin => None,
        }
    }

    /// `RandomSpreadStructurePlacement.getPotentialStructureChunk(seed, sourceX, sourceZ)`;
    /// `None` for other placements.
    pub fn potential_structure_chunk(&self, seed: i64, x: i32, z: i32) -> Option<ChunkPos> {
        let Self::RandomSpread {
            common,
            spacing,
            separation,
            spread_type,
        } = self
        else {
            return None;
        };
        let grid_x = x.div_euclid(*spacing);
        let grid_z = z.div_euclid(*spacing);
        let mut random = WorldgenRandom::new(0);
        random.set_large_feature_with_salt(seed, grid_x, grid_z, common.salt);
        let limit = spacing - separation;
        let spread_x = spread_type.evaluate(&mut random.random, limit);
        let spread_z = spread_type.evaluate(&mut random.random, limit);
        Some(ChunkPos::new(
            grid_x * spacing + spread_x,
            grid_z * spacing + spread_z,
        ))
    }

    /// `isStructureChunk(state, sourceX, sourceZ)`.
    pub fn is_structure_chunk(&self, state: &StructureState<'_>, x: i32, z: i32) -> bool {
        let common = match self {
            Self::DimensionOrigin => {
                let origin = state.dimension_origin();
                return origin.x == x && origin.z == z;
            }
            Self::RandomSpread { common, .. } => {
                let chunk = self.potential_structure_chunk(state.level_seed(), x, z);
                if chunk != Some(ChunkPos::new(x, z)) {
                    return false;
                }
                common
            }
            Self::ConcentricRings { common, rings } => {
                let Some(positions) = state.ring_positions_for(rings) else {
                    return false;
                };
                if !positions.contains(&ChunkPos::new(x, z)) {
                    return false;
                }
                common
            }
        };
        // `applyAdditionalChunkRestrictions`.
        if common.frequency < 1.0
            && !common.frequency_reduction_method.should_generate(
                state.level_seed(),
                common.salt,
                x,
                z,
                common.frequency,
            )
        {
            return false;
        }
        // `applyInteractionsWithOtherStructures`.
        match common.exclusion_zone {
            Some(zone) => {
                !state.has_structure_chunk_in_range(zone.other_set, x, z, zone.chunk_count)
            }
            None => true,
        }
    }
}

impl Spreading {
    fn parse(json: &Value, parser: &mut StructureParser<'_>) -> Result<Self, Error> {
        let locate_offset = match json.get("locate_offset").and_then(Value::as_array) {
            Some(offset) if offset.len() == 3 => {
                let mut out = [0; 3];
                for (o, v) in out.iter_mut().zip(offset) {
                    *o = v
                        .as_i64()
                        .map(|v| v as i32)
                        .ok_or_else(|| Error::new("locate_offset is not a vector"))?;
                }
                out
            }
            Some(_) => return Err(Error::new("locate_offset is not a vector")),
            None => [0; 3],
        };
        let frequency_reduction_method = match json
            .get("frequency_reduction_method")
            .and_then(Value::as_str)
        {
            Some(name) => FrequencyReductionMethod::from_name(name)?,
            None => FrequencyReductionMethod::Default,
        };
        let frequency = float_or(json, "frequency", 1.0);
        if !(0.0..=1.0).contains(&frequency) {
            return Err(Error::new(format!("frequency {frequency} outside [0, 1]")));
        }
        let salt = int_field(json, "salt")?;
        let exclusion_zone = match json.get("exclusion_zone") {
            Some(zone) => Some(ExclusionZone {
                other_set: parser.structure_set(field(zone, "other_set")?)?,
                chunk_count: int_field(zone, "chunk_count")?,
            }),
            None => None,
        };
        Ok(Self {
            locate_offset,
            frequency_reduction_method,
            frequency,
            salt,
            exclusion_zone,
        })
    }
}

/// Java `Math.round(double)` as an `int` cast (`(int) Math.round(value)`).
pub(crate) fn java_round(value: f64) -> i32 {
    if value.is_nan() {
        return 0;
    }
    let floor = value.floor();
    let rounded = if value - floor >= 0.5 {
        floor + 1.0
    } else {
        floor
    };
    rounded as i64 as i32
}

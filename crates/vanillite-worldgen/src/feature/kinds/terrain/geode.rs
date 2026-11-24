//! `GeodeFeature` with `GeodeBlockSettings`, `GeodeLayerSettings` and `GeodeCrackSettings`.

use serde_json::Value;
use vanillite_base::random::{LegacyRandom, RandomSource};
use vanillite_base::{BlockPos, Direction};
use vanillite_data::block::{BlockState, ids};

use super::{direction_name, is, is_source, safe_set_block, schedule_fluid_tick};
use crate::Error;
use crate::blocks::BlockSet;
use crate::blocks::parse_block_state;
use crate::carver::providers::IntProvider;
use crate::feature::{
    BlockStateProvider, FeatureContext, FeatureParser, FeatureType, bool_or, field, int_field,
    int_or,
};
use crate::noise::{Noise, NormalNoise, Normalization};
use crate::random::AnyRandom;

/// `GeodeBlockSettings`.
#[derive(Debug)]
struct BlockSettings {
    filling: BlockStateProvider,
    inner_layer: BlockStateProvider,
    alternate_inner_layer: BlockStateProvider,
    middle_layer: BlockStateProvider,
    outer_layer: BlockStateProvider,
    inner_placements: Vec<BlockState>,
    cannot_replace: BlockSet,
    invalid_blocks: BlockSet,
}

/// `GeodeLayerSettings`.
#[derive(Debug)]
struct LayerSettings {
    filling: f64,
    inner_layer: f64,
    middle_layer: f64,
    outer_layer: f64,
}

/// `GeodeCrackSettings`.
#[derive(Debug)]
struct CrackSettings {
    generate_crack_chance: f64,
    base_crack_size: f64,
    crack_point_offset: i32,
}

/// `geode`: layered shells around a few random points, with an optional crack and crystals on
/// the inner layer.
#[derive(Debug)]
pub struct GeodeFeature {
    blocks: BlockSettings,
    layers: LayerSettings,
    crack: CrackSettings,
    use_potential_placements_chance: f64,
    use_alternate_layer0_chance: f64,
    placements_require_layer0_alternate: bool,
    outer_wall_distance: IntProvider,
    distribution_points: IntProvider,
    point_offset: IntProvider,
    min_gen_offset: i32,
    max_gen_offset: i32,
    noise_multiplier: f64,
    invalid_blocks_threshold: i32,
    /// `NOISE_PARAMETERS.create(new WorldgenRandom(new LegacyRandomSource(level.getSeed())))`:
    /// the same for every placement of a world, so it is created once.
    noise: Noise,
}

/// An optional `double` field with a default and Vanilla's range check.
fn double_in(json: &Value, name: &str, default: f64, min: f64, max: f64) -> Result<f64, Error> {
    let value = match json.get(name) {
        None => default,
        Some(v) => v
            .as_f64()
            .ok_or_else(|| Error::new(format!("{name} is not a number")))?,
    };
    if !(min..=max).contains(&value) {
        return Err(Error::new(format!(
            "{name} {value} outside of [{min}, {max}]"
        )));
    }
    Ok(value)
}

/// An optional int provider field with a default and `IntProviders.codec(min, max)` range.
fn int_provider_or(
    json: &Value,
    name: &str,
    default: (i32, i32),
    min: i32,
    max: i32,
) -> Result<IntProvider, Error> {
    match json.get(name) {
        Some(v) => IntProvider::from_json_in_range(v, min, max),
        None => Ok(IntProvider::Uniform {
            min_inclusive: default.0,
            max_inclusive: default.1,
        }),
    }
}

/// `NormalNoise.createParity(-4, 1.0)` created from `seed` as the feature does.
fn geode_noise(seed: i64) -> Noise {
    // `computeParityBaseAmplitude(-4, [1.0])`: one octave of amplitude 1.
    const PERLIN_STANDARD_DEVIATION: f64 = 0.270_224_783_124_521_1;
    const TARGET_DEVIATION: f64 = 0.333_333_333_333_333_3;
    let deviation = PERLIN_STANDARD_DEVIATION * 1.0;
    let input_deviation = (0.0 + deviation * deviation).sqrt();
    let new_factor = 1.0 * TARGET_DEVIATION / (input_deviation * 2.0_f64.sqrt());
    // `computeParityNormalizationFactor(1.0, 1, [1.0])`: octave span 0.
    let octave_span = 0;
    let expected_deviation = 0.1 * (1.0 + 1.0 / f64::from(octave_span + 1));
    let old_factor = 1.0 * 0.5 * TARGET_DEVIATION / expected_deviation;
    let base_amplitude = old_factor / new_factor;
    let parameters = NormalNoise::new(base_amplitude, -4, 1, Normalization::Enabled, Vec::new());
    parameters.create(&mut AnyRandom::Legacy(LegacyRandom::new(seed)))
}

impl FeatureType for GeodeFeature {
    fn parse(json: &Value, parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        let blocks_json = field(json, "blocks")?;
        let inner_placements = field(blocks_json, "inner_placements")?
            .as_array()
            .ok_or_else(|| Error::new("inner_placements is not a list"))?
            .iter()
            .map(parse_block_state)
            .collect::<Result<Vec<_>, _>>()?;
        if inner_placements.is_empty() {
            return Err(Error::new("inner_placements must not be empty"));
        }
        let blocks = BlockSettings {
            filling: parser.state_provider(field(blocks_json, "filling_provider")?)?,
            inner_layer: parser.state_provider(field(blocks_json, "inner_layer_provider")?)?,
            alternate_inner_layer: parser
                .state_provider(field(blocks_json, "alternate_inner_layer_provider")?)?,
            middle_layer: parser.state_provider(field(blocks_json, "middle_layer_provider")?)?,
            outer_layer: parser.state_provider(field(blocks_json, "outer_layer_provider")?)?,
            inner_placements,
            cannot_replace: parser.block_set(field(blocks_json, "cannot_replace")?)?,
            invalid_blocks: parser.block_set(field(blocks_json, "invalid_blocks")?)?,
        };
        let layers_json = field(json, "layers")?;
        let layers = LayerSettings {
            filling: double_in(layers_json, "filling", 1.7, 0.01, 50.0)?,
            inner_layer: double_in(layers_json, "inner_layer", 2.2, 0.01, 50.0)?,
            middle_layer: double_in(layers_json, "middle_layer", 3.2, 0.01, 50.0)?,
            outer_layer: double_in(layers_json, "outer_layer", 4.2, 0.01, 50.0)?,
        };
        let crack_json = field(json, "crack")?;
        let crack_point_offset = int_or(crack_json, "crack_point_offset", 2);
        if !(0..=10).contains(&crack_point_offset) {
            return Err(Error::new("crack_point_offset outside of [0, 10]"));
        }
        let crack = CrackSettings {
            generate_crack_chance: double_in(crack_json, "generate_crack_chance", 1.0, 0.0, 1.0)?,
            base_crack_size: double_in(crack_json, "base_crack_size", 2.0, 0.0, 5.0)?,
            crack_point_offset,
        };
        Ok(Self {
            blocks,
            layers,
            crack,
            use_potential_placements_chance: double_in(
                json,
                "use_potential_placements_chance",
                0.35,
                0.0,
                1.0,
            )?,
            use_alternate_layer0_chance: double_in(
                json,
                "use_alternate_layer0_chance",
                0.0,
                0.0,
                1.0,
            )?,
            placements_require_layer0_alternate: bool_or(
                json,
                "placements_require_layer0_alternate",
                true,
            ),
            outer_wall_distance: int_provider_or(json, "outer_wall_distance", (4, 5), 1, 20)?,
            distribution_points: int_provider_or(json, "distribution_points", (3, 4), 1, 20)?,
            point_offset: int_provider_or(json, "point_offset", (1, 2), 0, 10)?,
            min_gen_offset: int_or(json, "min_gen_offset", -16),
            max_gen_offset: int_or(json, "max_gen_offset", 16),
            noise_multiplier: double_in(json, "noise_multiplier", 0.05, 0.0, 1.0)?,
            invalid_blocks_threshold: int_field(json, "invalid_blocks_threshold")?,
            noise: geode_noise(parser.random_state().seed()),
        })
    }

    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        let level = &mut *ctx.level;
        let random = &mut *ctx.random;
        let num_points = self.distribution_points.sample(random);
        let crack_size_adjustment =
            f64::from(num_points) / f64::from(self.outer_wall_distance.max_inclusive());
        let inner_air = 1.0 / self.layers.filling.sqrt();
        let innermost_block_layer = 1.0 / (self.layers.inner_layer + crack_size_adjustment).sqrt();
        let inner_crust = 1.0 / (self.layers.middle_layer + crack_size_adjustment).sqrt();
        let outer_crust = 1.0 / (self.layers.outer_layer + crack_size_adjustment).sqrt();
        let crack_size = 1.0
            / (self.crack.base_crack_size
                + random.next_double() / 2.0
                + if num_points > 3 {
                    crack_size_adjustment
                } else {
                    0.0
                })
            .sqrt();
        let should_generate_crack =
            f64::from(random.next_float()) < self.crack.generate_crack_chance;

        let mut points: Vec<(BlockPos, i32)> = Vec::with_capacity(num_points.max(0) as usize);
        let mut invalid_points = 0;
        for _ in 0..num_points {
            let x = self.outer_wall_distance.sample(random);
            let y = self.outer_wall_distance.sample(random);
            let z = self.outer_wall_distance.sample(random);
            let pos = origin.offset(x, y, z);
            let state = level.block(pos);
            if state.is_air() || self.blocks.invalid_blocks.contains(state) {
                invalid_points += 1;
                if invalid_points > self.invalid_blocks_threshold {
                    return false;
                }
            }
            points.push((pos, self.point_offset.sample(random)));
        }

        let mut crack_points: Vec<BlockPos> = Vec::new();
        if should_generate_crack {
            let offset_index = random.next_int_bounded(4);
            let crack_offset = num_points * 2 + 1;
            let (dx, dz) = match offset_index {
                0 => (crack_offset, 0),
                1 => (0, crack_offset),
                2 => (crack_offset, crack_offset),
                _ => (0, 0),
            };
            for dy in [7, 5, 1] {
                crack_points.push(origin.offset(dx, dy, dz));
            }
        }

        let mut potential_crystals: Vec<BlockPos> = Vec::new();
        let cannot_replace = &self.blocks.cannot_replace;
        let can_replace = |s: BlockState| !cannot_replace.contains(s);
        let (min, max) = (self.min_gen_offset, self.max_gen_offset);
        for z in origin.z + min..=origin.z + max {
            for y in origin.y + min..=origin.y + max {
                for x in origin.x + min..=origin.x + max {
                    let pos = BlockPos::new(x, y, z);
                    let noise_offset =
                        f64::from(self.noise.get(f64::from(x), f64::from(y), f64::from(z)))
                            * self.noise_multiplier;
                    let mut dist_sum_shell = 0.0;
                    for &(point, offset) in &points {
                        dist_sum_shell +=
                            inv_sqrt(pos.dist_sqr(point) + f64::from(offset)) + noise_offset;
                    }
                    if dist_sum_shell < outer_crust {
                        continue;
                    }
                    if dist_sum_shell >= inner_air {
                        let state = self.blocks.filling.get_state(level, random, pos);
                        safe_set_block(level, pos, state, can_replace);
                        continue;
                    }
                    let mut dist_sum_crack = 0.0;
                    for &point in &crack_points {
                        dist_sum_crack += inv_sqrt(
                            pos.dist_sqr(point) + f64::from(self.crack.crack_point_offset),
                        ) + noise_offset;
                    }
                    if should_generate_crack && dist_sum_crack >= crack_size {
                        safe_set_block(level, pos, BlockState::AIR, can_replace);
                        for direction in Direction::ALL {
                            let adjacent = pos.relative(direction);
                            let fluid = level.fluid(adjacent);
                            if !fluid.is_empty() {
                                schedule_fluid_tick(level, adjacent, fluid);
                            }
                        }
                    } else if dist_sum_shell >= innermost_block_layer {
                        let use_alternate =
                            f64::from(random.next_float()) < self.use_alternate_layer0_chance;
                        let provider = if use_alternate {
                            &self.blocks.alternate_inner_layer
                        } else {
                            &self.blocks.inner_layer
                        };
                        let state = provider.get_state(level, random, pos);
                        safe_set_block(level, pos, state, can_replace);
                        if (!self.placements_require_layer0_alternate || use_alternate)
                            && f64::from(random.next_float()) < self.use_potential_placements_chance
                        {
                            potential_crystals.push(pos);
                        }
                    } else if dist_sum_shell >= inner_crust {
                        let state = self.blocks.middle_layer.get_state(level, random, pos);
                        safe_set_block(level, pos, state, can_replace);
                    } else if dist_sum_shell >= outer_crust {
                        let state = self.blocks.outer_layer.get_state(level, random, pos);
                        safe_set_block(level, pos, state, can_replace);
                    }
                }
            }
        }

        let placements = &self.blocks.inner_placements;
        for crystal in potential_crystals {
            let mut state = placements[random.next_int_bounded(placements.len() as i32) as usize];
            for direction in Direction::ALL {
                if let Some(facing) = state.with("facing", direction_name(direction)) {
                    state = facing;
                }
                let place_pos = crystal.relative(direction);
                let place_state = level.block(place_pos);
                let waterlogged = if is_source(place_state.fluid_state()) {
                    "true"
                } else {
                    "false"
                };
                if let Some(s) = state.with("waterlogged", waterlogged) {
                    state = s;
                }
                if can_cluster_grow_at_state(place_state) {
                    safe_set_block(level, place_pos, state, can_replace);
                    break;
                }
            }
        }
        true
    }
}

/// `Mth.invSqrt(double)` (JOML `Math.invsqrt`: `1.0 / Math.sqrt(x)`).
#[inline]
fn inv_sqrt(x: f64) -> f64 {
    1.0 / x.sqrt()
}

/// `BuddingAmethystBlock.canClusterGrowAtState`.
fn can_cluster_grow_at_state(state: BlockState) -> bool {
    state.is_air() || is(state, ids::WATER) && state.fluid_state().info().amount == 8
}

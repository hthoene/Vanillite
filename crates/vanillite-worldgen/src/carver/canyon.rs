//! `CanyonWorldCarver`: one long, tall ravine per start chunk.

use serde_json::Value;
use vanillite_base::ChunkPos;
use vanillite_base::math::{cos, sin};
use vanillite_base::random::{LegacyRandom, RandomSource};

use super::providers::{FloatProvider, HeightProvider, WorldGenerationContext, random_between};
use super::{Cuts, RANGE, Skip, block_x, block_z, probability};
use crate::Error;

/// `CanyonWorldCarver.Shape`.
#[derive(Clone, Debug, PartialEq)]
pub struct CanyonShape {
    pub distance_factor: FloatProvider,
    pub thickness: FloatProvider,
    pub width_smoothness: i32,
    pub horizontal_radius_factor: FloatProvider,
    pub vertical_radius_default_factor: f32,
    pub vertical_radius_center_factor: f32,
    pub y_scale: FloatProvider,
}

/// `CanyonWorldCarver` with its configuration (`canyon` carver type).
#[derive(Clone, Debug, PartialEq)]
pub struct CanyonCarver {
    pub probability: f32,
    pub y: HeightProvider,
    pub vertical_rotation: FloatProvider,
    pub shape: CanyonShape,
}

fn get<'v>(json: &'v Value, name: &str) -> Result<&'v Value, Error> {
    json.get(name)
        .ok_or_else(|| Error::new(format!("canyon carver: missing field {name}")))
}

fn float(json: &Value, name: &str) -> Result<f32, Error> {
    get(json, name)?
        .as_f64()
        .map(|v| v as f32)
        .ok_or_else(|| Error::new(format!("canyon carver: {name} is not a number")))
}

impl CanyonCarver {
    /// Parses `CanyonWorldCarver.MAP_CODEC`.
    pub fn from_json(json: &Value) -> Result<Self, Error> {
        let shape = get(json, "shape")?;
        let width_smoothness = get(shape, "width_smoothness")?
            .as_i64()
            .and_then(|v| i32::try_from(v).ok())
            .filter(|&v| v > 0)
            .ok_or_else(|| Error::new("canyon carver: width_smoothness must be a positive int"))?;
        Ok(Self {
            probability: probability(json)?,
            y: HeightProvider::from_json(get(json, "y")?)?,
            vertical_rotation: FloatProvider::from_json(get(json, "vertical_rotation")?)?,
            shape: CanyonShape {
                distance_factor: FloatProvider::from_json(get(shape, "distance_factor")?)?,
                thickness: FloatProvider::from_json(get(shape, "thickness")?)?,
                width_smoothness,
                horizontal_radius_factor: FloatProvider::from_json(get(
                    shape,
                    "horizontal_radius_factor",
                )?)?,
                vertical_radius_default_factor: float(shape, "vertical_radius_default_factor")?,
                vertical_radius_center_factor: float(shape, "vertical_radius_center_factor")?,
                y_scale: FloatProvider::from_json(get(shape, "y_scale")?)?,
            },
        })
    }

    /// `carve(context, random, chunkPos, sourceChunkPos, output)`: the ellipsoids go to `cuts`.
    pub(super) fn carve<R: RandomSource>(
        &self,
        context: &WorldGenerationContext,
        random: &mut R,
        source: ChunkPos,
        cuts: &mut impl Cuts,
    ) {
        let max_distance = (RANGE * 2 - 1) * 16;
        let x = f64::from(block_x(source, random.next_int_bounded(16)));
        let y = self.y.sample(random, context);
        let z = f64::from(block_z(source, random.next_int_bounded(16)));
        let horizontal_rotation = random.next_float() * std::f64::consts::TAU as f32;
        let vertical_rotation = self.vertical_rotation.sample(random);
        let y_scale = f64::from(self.shape.y_scale.sample(random));
        let thickness = self.shape.thickness.sample(random);
        let distance = (max_distance as f32 * self.shape.distance_factor.sample(random)) as i32;
        self.do_carve(
            context,
            random.next_long(),
            x,
            f64::from(y),
            z,
            thickness,
            horizontal_rotation,
            vertical_rotation,
            0,
            distance,
            y_scale,
            cuts,
        );
    }

    /// `doCarve(…)`.
    #[allow(clippy::too_many_arguments)]
    fn do_carve(
        &self,
        context: &WorldGenerationContext,
        tunnel_seed: i64,
        mut x: f64,
        mut y: f64,
        mut z: f64,
        thickness: f32,
        mut horizontal_rotation: f32,
        mut vertical_rotation: f32,
        step: i32,
        distance: i32,
        y_scale: f64,
        cuts: &mut impl Cuts,
    ) {
        let start = cuts.mark();
        // `RandomSource.createThreadLocalInstance(seed)`: the same LCG as the legacy source.
        let mut random = LegacyRandom::new(tunnel_seed);
        let width_factor_per_height = self.init_width_factors(context, &mut random);
        let skip = Skip::Canyon {
            width_factor_per_height: &width_factor_per_height,
            min_gen_y: context.min_gen_y(),
        };
        let mut y_rota = 0.0_f32;
        let mut x_rota = 0.0_f32;
        for current_step in step..distance {
            let mut horizontal_radius = 1.5
                + f64::from(
                    sin(f64::from(
                        current_step as f32 * std::f64::consts::PI as f32 / distance as f32,
                    )) * thickness,
                );
            let mut vertical_radius = horizontal_radius * y_scale;
            horizontal_radius *= f64::from(self.shape.horizontal_radius_factor.sample(&mut random));
            vertical_radius = self.update_vertical_radius(
                &mut random,
                vertical_radius,
                distance as f32,
                current_step as f32,
            );
            let xc = cos(f64::from(vertical_rotation));
            let xs = sin(f64::from(vertical_rotation));
            x += f64::from(cos(f64::from(horizontal_rotation)) * xc);
            y += f64::from(xs);
            z += f64::from(sin(f64::from(horizontal_rotation)) * xc);
            vertical_rotation *= 0.7;
            vertical_rotation += x_rota * 0.05;
            horizontal_rotation += y_rota * 0.05;
            x_rota *= 0.8;
            y_rota *= 0.5;
            x_rota += (random.next_float() - random.next_float()) * random.next_float() * 2.0;
            y_rota += (random.next_float() - random.next_float()) * random.next_float() * 4.0;
            if random.next_int_bounded(4) != 0
                && !cuts.step(
                    (current_step, distance, thickness),
                    x,
                    y,
                    z,
                    horizontal_radius,
                    vertical_radius,
                    skip,
                )
            {
                return;
            }
        }
        let end = cuts.mark();
        cuts.close(start, end);
    }

    /// `initWidthFactors(context, random)`: squared width factor per Y above the minimum.
    fn init_width_factors(
        &self,
        context: &WorldGenerationContext,
        random: &mut LegacyRandom,
    ) -> Vec<f32> {
        let depth = context.gen_depth().max(0) as usize;
        let mut factors = Vec::with_capacity(depth);
        let mut width_factor = 1.0_f32;
        for y_index in 0..depth {
            if y_index == 0 || random.next_int_bounded(self.shape.width_smoothness) == 0 {
                width_factor = 1.0 + random.next_float() * random.next_float();
            }
            factors.push(width_factor * width_factor);
        }
        factors
    }

    /// `updateVerticalRadius(random, verticalRadius, distance, currentStep)`.
    fn update_vertical_radius(
        &self,
        random: &mut LegacyRandom,
        vertical_radius: f64,
        distance: f32,
        current_step: f32,
    ) -> f64 {
        let vertical_multiplier = 1.0 - (0.5 - current_step / distance).abs() * 2.0;
        let factor = self.shape.vertical_radius_default_factor
            + self.shape.vertical_radius_center_factor * vertical_multiplier;
        f64::from(factor) * vertical_radius * f64::from(random_between(random, 0.75, 1.0))
    }
}

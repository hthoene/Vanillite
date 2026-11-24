//! `CaveWorldCarver`: branching tunnels with occasional rooms.

use serde_json::Value;
use vanillite_base::ChunkPos;
use vanillite_base::math::{cos, sin};
use vanillite_base::random::{LegacyRandom, RandomSource};

use super::providers::{FloatProvider, HeightProvider, IntProvider, WorldGenerationContext};
use super::{CarvingMask, RANGE, block_x, block_z, can_reach, carve_ellipsoid, probability};
use crate::Error;

/// `CaveWorldCarver` with its configuration (`cave` carver type).
#[derive(Clone, Debug, PartialEq)]
pub struct CaveCarver {
    pub probability: f32,
    pub y: HeightProvider,
    pub count: IntProvider,
    pub thickness: FloatProvider,
    pub weird_thickness_bias: bool,
    pub room_vertical_radius_multiplier: FloatProvider,
    pub horizontal_radius_multiplier: FloatProvider,
    pub vertical_radius_multiplier: FloatProvider,
    pub start_vertical_radius_multiplier: FloatProvider,
    pub floor_level: FloatProvider,
}

impl CaveCarver {
    /// Parses `CaveWorldCarver.MAP_CODEC`.
    pub fn from_json(json: &Value) -> Result<Self, Error> {
        let get = |name: &str| {
            json.get(name)
                .ok_or_else(|| Error::new(format!("cave carver: missing field {name}")))
        };
        let float = |name: &str| FloatProvider::from_json(get(name)?);
        Ok(Self {
            probability: probability(json)?,
            y: HeightProvider::from_json(get("y")?)?,
            count: IntProvider::from_json_in_range(get("count")?, 0, i32::MAX)?,
            thickness: FloatProvider::from_json_in_range(get("thickness")?, 0.0, f32::INFINITY)?,
            weird_thickness_bias: match json.get("weird_thickness_bias") {
                None => false,
                Some(v) => v
                    .as_bool()
                    .ok_or_else(|| Error::new("cave carver: weird_thickness_bias"))?,
            },
            room_vertical_radius_multiplier: float("room_vertical_radius_multiplier")?,
            horizontal_radius_multiplier: float("horizontal_radius_multiplier")?,
            vertical_radius_multiplier: float("vertical_radius_multiplier")?,
            start_vertical_radius_multiplier: match json.get("start_vertical_radius_multiplier") {
                None => FloatProvider::Constant(1.0),
                Some(v) => FloatProvider::from_json(v)?,
            },
            floor_level: FloatProvider::from_json_in_range(get("floor_level")?, -1.0, 1.0)?,
        })
    }

    /// `carve(context, random, chunkPos, sourceChunkPos, output)`.
    pub(super) fn carve<R: RandomSource>(
        &self,
        context: &WorldGenerationContext,
        random: &mut R,
        chunk: ChunkPos,
        source: ChunkPos,
        output: &mut CarvingMask,
    ) {
        let max_distance = (RANGE * 2 - 1) << 4;
        let cave_count = self.count.sample(random);
        for _ in 0..cave_count {
            let x = f64::from(block_x(source, random.next_int_bounded(16)));
            let y = f64::from(self.y.sample(random, context));
            let z = f64::from(block_z(source, random.next_int_bounded(16)));
            let horizontal_radius_multiplier =
                f64::from(self.horizontal_radius_multiplier.sample(random));
            let vertical_radius_multiplier =
                f64::from(self.vertical_radius_multiplier.sample(random));
            let start_vertical_radius_multiplier =
                f64::from(self.start_vertical_radius_multiplier.sample(random));
            let floor_level = f64::from(self.floor_level.sample(random));
            let tunnel = Tunnel {
                chunk,
                horizontal_radius_multiplier,
                vertical_radius_multiplier,
                floor_level,
            };
            let mut tunnels = 1;
            if random.next_int_bounded(4) == 0 {
                let y_scale = f64::from(self.room_vertical_radius_multiplier.sample(random));
                let thickness = 1.0 + random.next_float() * 6.0;
                tunnel.create_room(x, y, z, thickness, y_scale, output);
                tunnels += random.next_int_bounded(4);
            }
            for _ in 0..tunnels {
                let horizontal_rotation = random.next_float() * std::f64::consts::TAU as f32;
                let vertical_rotation = (random.next_float() - 0.5) / 4.0;
                let thickness = self.thickness(random);
                let distance = max_distance - random.next_int_bounded(max_distance / 4);
                tunnel.create_tunnel(
                    random.next_long(),
                    x,
                    y,
                    z,
                    thickness,
                    horizontal_rotation,
                    vertical_rotation,
                    0,
                    distance,
                    start_vertical_radius_multiplier,
                    output,
                );
            }
        }
    }

    /// `getThickness(random)`.
    fn thickness<R: RandomSource>(&self, random: &mut R) -> f32 {
        let mut thickness = self.thickness.sample(random);
        if self.weird_thickness_bias && random.next_int_bounded(10) == 0 {
            thickness *= random.next_float() * random.next_float() * 3.0 + 1.0;
        }
        thickness
    }
}

/// The per-cave constants of `createRoom`/`createTunnel`.
struct Tunnel {
    chunk: ChunkPos,
    horizontal_radius_multiplier: f64,
    vertical_radius_multiplier: f64,
    floor_level: f64,
}

impl Tunnel {
    /// `CaveWorldCarver.shouldSkip(xd, yd, zd, floorLevel)`.
    fn should_skip(&self, xd: f64, yd: f64, zd: f64) -> bool {
        yd <= self.floor_level || xd * xd + yd * yd + zd * zd >= 1.0
    }

    /// `createRoom(…)`.
    fn create_room(
        &self,
        x: f64,
        y: f64,
        z: f64,
        thickness: f32,
        y_scale: f64,
        output: &mut CarvingMask,
    ) {
        let horizontal_radius =
            1.5 + f64::from(sin(f64::from(std::f64::consts::FRAC_PI_2 as f32)) * thickness);
        let vertical_radius = horizontal_radius * y_scale;
        carve_ellipsoid(
            self.chunk,
            x + 1.0,
            y,
            z,
            horizontal_radius,
            vertical_radius,
            output,
            |xd, yd, zd, _| self.should_skip(xd, yd, zd),
        );
    }

    /// `createTunnel(…)`.
    #[allow(clippy::too_many_arguments)]
    fn create_tunnel(
        &self,
        tunnel_seed: i64,
        mut x: f64,
        mut y: f64,
        mut z: f64,
        thickness: f32,
        mut horizontal_rotation: f32,
        mut vertical_rotation: f32,
        step: i32,
        dist: i32,
        y_scale: f64,
        output: &mut CarvingMask,
    ) {
        // `RandomSource.createThreadLocalInstance(seed)`: a `SingleThreadedRandomSource`, the
        // same LCG as the legacy source.
        let mut random = LegacyRandom::new(tunnel_seed);
        let split_point = random.next_int_bounded(dist / 2) + dist / 4;
        let steep = random.next_int_bounded(6) == 0;
        let mut y_rota = 0.0_f32;
        let mut x_rota = 0.0_f32;
        for current_step in step..dist {
            let horizontal_radius = 1.5
                + f64::from(
                    sin(f64::from(
                        std::f64::consts::PI as f32 * current_step as f32 / dist as f32,
                    )) * thickness,
                );
            let vertical_radius = horizontal_radius * y_scale;
            let cos_x = cos(f64::from(vertical_rotation));
            x += f64::from(cos(f64::from(horizontal_rotation)) * cos_x);
            y += f64::from(sin(f64::from(vertical_rotation)));
            z += f64::from(sin(f64::from(horizontal_rotation)) * cos_x);
            vertical_rotation *= if steep { 0.92 } else { 0.7 };
            vertical_rotation += x_rota * 0.1;
            horizontal_rotation += y_rota * 0.1;
            x_rota *= 0.9;
            y_rota *= 0.75;
            x_rota += (random.next_float() - random.next_float()) * random.next_float() * 2.0;
            y_rota += (random.next_float() - random.next_float()) * random.next_float() * 4.0;
            if current_step == split_point && thickness > 1.0 {
                for side in [-1.0_f32, 1.0] {
                    let branch_seed = random.next_long();
                    let branch_thickness = random.next_float() * 0.5 + 0.5;
                    self.create_tunnel(
                        branch_seed,
                        x,
                        y,
                        z,
                        branch_thickness,
                        horizontal_rotation + side * std::f64::consts::FRAC_PI_2 as f32,
                        vertical_rotation / 3.0,
                        current_step,
                        dist,
                        1.0,
                        output,
                    );
                }
                return;
            }
            if random.next_int_bounded(4) != 0 {
                if !can_reach(self.chunk, x, z, current_step, dist, thickness) {
                    return;
                }
                carve_ellipsoid(
                    self.chunk,
                    x,
                    y,
                    z,
                    horizontal_radius * self.horizontal_radius_multiplier,
                    vertical_radius * self.vertical_radius_multiplier,
                    output,
                    |xd, yd, zd, _| self.should_skip(xd, yd, zd),
                );
            }
        }
    }
}

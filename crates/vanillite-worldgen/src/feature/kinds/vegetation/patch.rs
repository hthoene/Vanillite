//! `vegetation_patch` and `waterlogged_vegetation_patch`: a patch of ground blocks on a floor or
//! ceiling with vegetation on top.

use serde_json::Value;
use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Direction};
use vanillite_data::block::{BlockState, ids};

use super::util::with_bool;
use crate::Error;
use crate::behaviour::{SupportType, is_face_sturdy};
use crate::blocks::BlockSet;
use crate::carver::providers::IntProvider;
use crate::feature::kinds::tree::java_set::JavaHashSet;
use crate::feature::{
    BlockStateProvider, FeatureContext, FeatureParser, FeatureType, PlacedFeatureId, field,
    float_field, int_field,
};

/// The configuration both patch features share (`VegetationPatchFeature.makeCodec`).
#[derive(Debug)]
struct PatchConfig {
    replaceable: BlockSet,
    ground_state: BlockStateProvider,
    vegetation_feature: PlacedFeatureId,
    /// `CaveSurface.getDirection()`: down for `floor`, up for `ceiling`.
    surface: Direction,
    depth: IntProvider,
    extra_bottom_block_chance: f32,
    vertical_range: i32,
    vegetation_chance: f32,
    xz_radius: IntProvider,
    extra_edge_column_chance: f32,
}

/// A required float field within `0..=1` (`Codec.floatRange(0, 1)`).
fn chance_field(json: &Value, name: &str) -> Result<f32, Error> {
    super::unit_float(float_field(json, name)?, name)
}

impl PatchConfig {
    fn parse(json: &Value, parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        let surface = match field(json, "surface")?.as_str() {
            Some("floor") => Direction::Down,
            Some("ceiling") => Direction::Up,
            other => return Err(Error::new(format!("invalid cave surface {other:?}"))),
        };
        let vertical_range = int_field(json, "vertical_range")?;
        if !(1..=256).contains(&vertical_range) {
            return Err(Error::new(format!(
                "vertical_range out of range: {vertical_range}"
            )));
        }
        Ok(Self {
            replaceable: parser.block_set(field(json, "replaceable")?)?,
            ground_state: parser.state_provider(field(json, "ground_state")?)?,
            vegetation_feature: parser.placed(field(json, "vegetation_feature")?)?,
            surface,
            depth: IntProvider::from_json_in_range(field(json, "depth")?, 1, 128)?,
            extra_bottom_block_chance: chance_field(json, "extra_bottom_block_chance")?,
            vertical_range,
            vegetation_chance: chance_field(json, "vegetation_chance")?,
            xz_radius: IntProvider::from_json(field(json, "xz_radius")?)?,
            extra_edge_column_chance: chance_field(json, "extra_edge_column_chance")?,
        })
    }

    /// `VegetationPatchFeature.placeGroundPatch`: the ground positions placed, as Vanilla's
    /// `HashSet`.
    fn place_ground_patch(
        &self,
        ctx: &mut FeatureContext<'_, '_>,
        origin: BlockPos,
        x_radius: i32,
        z_radius: i32,
    ) -> JavaHashSet {
        let inwards = self.surface;
        let outwards = inwards.opposite();
        let mut surface = JavaHashSet::new();
        for dx in -x_radius..=x_radius {
            let is_x_edge = dx == -x_radius || dx == x_radius;
            for dz in -z_radius..=z_radius {
                let is_z_edge = dz == -z_radius || dz == z_radius;
                let is_edge = is_x_edge || is_z_edge;
                let is_corner = is_x_edge && is_z_edge;
                let is_edge_but_not_corner = is_edge && !is_corner;
                if is_corner
                    || (is_edge_but_not_corner
                        && (self.extra_edge_column_chance == 0.0
                            || ctx.random.next_float() > self.extra_edge_column_chance))
                {
                    continue;
                }
                let mut pos = origin.offset(dx, 0, dz);
                let mut offset = 0;
                while ctx.level.block(pos).is_air() && offset < self.vertical_range {
                    pos = pos.relative(inwards);
                    offset += 1;
                }
                let mut offset = 0;
                while !ctx.level.block(pos).is_air() && offset < self.vertical_range {
                    pos = pos.relative(outwards);
                    offset += 1;
                }
                let below = pos.relative(self.surface);
                let below_state = ctx.level.block(below);
                if ctx.level.is_empty_block(pos)
                    && is_face_sturdy(below_state, self.surface.opposite(), SupportType::Full)
                {
                    let extra = self.extra_bottom_block_chance > 0.0
                        && ctx.random.next_float() < self.extra_bottom_block_chance;
                    let depth = self.depth.sample(ctx.random) + i32::from(extra);
                    if self.place_ground(ctx, below, depth) {
                        surface.insert(below);
                    }
                }
            }
        }
        surface
    }

    /// `VegetationPatchFeature.placeGround`.
    fn place_ground(
        &self,
        ctx: &mut FeatureContext<'_, '_>,
        mut pos: BlockPos,
        depth: i32,
    ) -> bool {
        for i in 0..depth {
            let state = self.ground_state.get_state(ctx.level, ctx.random, pos);
            let below = ctx.level.block(pos);
            if state.block() != below.block() {
                if !self.replaceable.contains(below) {
                    return i != 0;
                }
                ctx.level.set_block(pos, state, 2);
                pos = pos.relative(self.surface);
            }
        }
        true
    }

    /// `VegetationPatchFeature.placeVegetation`.
    fn place_vegetation(&self, ctx: &mut FeatureContext<'_, '_>, pos: BlockPos) -> bool {
        ctx.place_placed(
            self.vegetation_feature,
            pos.relative(self.surface.opposite()),
        )
    }

    /// `VegetationPatchFeature.distributeVegetation` over `surface` in Java's order;
    /// `place` is the (possibly overridden) `placeVegetation`.
    fn distribute_vegetation(
        &self,
        ctx: &mut FeatureContext<'_, '_>,
        surface: &JavaHashSet,
        mut place: impl FnMut(&mut FeatureContext<'_, '_>, BlockPos),
    ) {
        for pos in surface.iter() {
            if self.vegetation_chance > 0.0 && ctx.random.next_float() < self.vegetation_chance {
                place(ctx, pos);
            }
        }
    }

    /// The radii both features sample first (`xzRadius.sample + 1`, x then z).
    fn sample_radii(&self, ctx: &mut FeatureContext<'_, '_>) -> (i32, i32) {
        let x = self.xz_radius.sample(ctx.random).wrapping_add(1);
        let z = self.xz_radius.sample(ctx.random).wrapping_add(1);
        (x, z)
    }
}

/// `vegetation_patch`.
#[derive(Debug)]
pub struct VegetationPatchFeature {
    config: PatchConfig,
}

impl FeatureType for VegetationPatchFeature {
    fn parse(json: &Value, parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        Ok(Self {
            config: PatchConfig::parse(json, parser)?,
        })
    }

    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        let config = &self.config;
        let (x_radius, z_radius) = config.sample_radii(ctx);
        let surface = config.place_ground_patch(ctx, origin, x_radius, z_radius);
        config.distribute_vegetation(ctx, &surface, |ctx, pos| {
            config.place_vegetation(ctx, pos);
        });
        !surface.is_empty()
    }
}

/// `waterlogged_vegetation_patch`: the enclosed ground positions become water and the
/// vegetation on them is waterlogged.
#[derive(Debug)]
pub struct WaterloggedVegetationPatchFeature {
    config: PatchConfig,
}

impl FeatureType for WaterloggedVegetationPatchFeature {
    fn parse(json: &Value, parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        Ok(Self {
            config: PatchConfig::parse(json, parser)?,
        })
    }

    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        let config = &self.config;
        let (x_radius, z_radius) = config.sample_radii(ctx);
        let surface = self.place_ground_patch(ctx, origin, x_radius, z_radius);
        config.distribute_vegetation(ctx, &surface, |ctx, pos| {
            self.place_vegetation(ctx, pos);
        });
        !surface.is_empty()
    }
}

impl WaterloggedVegetationPatchFeature {
    /// `WaterloggedVegetationPatchFeature.placeGroundPatch`: the ground positions that are not
    /// exposed, now water.
    fn place_ground_patch(
        &self,
        ctx: &mut FeatureContext<'_, '_>,
        origin: BlockPos,
        x_radius: i32,
        z_radius: i32,
    ) -> JavaHashSet {
        let surface = self
            .config
            .place_ground_patch(ctx, origin, x_radius, z_radius);
        let mut water_surface = JavaHashSet::new();
        for pos in surface.iter() {
            if !is_exposed(ctx, pos) {
                water_surface.insert(pos);
            }
        }
        let water = ids::WATER.default_state();
        for pos in water_surface.iter() {
            ctx.level.set_block(pos, water, 2);
        }
        water_surface
    }

    /// `WaterloggedVegetationPatchFeature.placeVegetation`.
    fn place_vegetation(&self, ctx: &mut FeatureContext<'_, '_>, pos: BlockPos) -> bool {
        if !self.config.place_vegetation(ctx, pos.offset(0, -1, 0)) {
            return false;
        }
        let placed: BlockState = ctx.level.block(pos);
        if placed.value("waterlogged") == Some("false") {
            ctx.level
                .set_block(pos, with_bool(placed, "waterlogged", true), 2);
        }
        true
    }
}

/// `WaterloggedVegetationPatchFeature.isExposed`: a side (or the bottom) is not sturdy.
fn is_exposed(ctx: &FeatureContext<'_, '_>, pos: BlockPos) -> bool {
    [
        Direction::North,
        Direction::East,
        Direction::South,
        Direction::West,
        Direction::Down,
    ]
    .into_iter()
    .any(|direction| {
        let state = ctx.level.block(pos.relative(direction));
        !is_face_sturdy(state, direction.opposite(), SupportType::Full)
    })
}

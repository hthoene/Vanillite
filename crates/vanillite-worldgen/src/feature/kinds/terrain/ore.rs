//! `OreFeature` and `ScatteredOreFeature` with their shared `AbstractOreFeature` configuration.

use serde_json::Value;
use vanillite_base::math::{ceil_f, floor, lerp, sin};
use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Direction};
use vanillite_chunk::HeightmapKind;
use vanillite_data::block::BlockState;

use super::rule_test::BlockReplacement;
use crate::Error;
use crate::feature::region::flags;
use crate::feature::{FeatureContext, FeatureParser, FeatureType, float_field, int_field};
use crate::mth::round_f;

/// The configuration of `AbstractOreFeature`: replacement targets, vein size and the chance to
/// discard a block that touches air.
#[derive(Debug)]
struct OreConfig {
    targets: Vec<BlockReplacement>,
    size: i32,
    discard_chance_on_air_exposure: f32,
}

impl OreConfig {
    /// `AbstractOreFeature.makeCodec`.
    fn parse(json: &Value, parser: &FeatureParser<'_>) -> Result<Self, Error> {
        let size = int_field(json, "size")?;
        if !(0..=64).contains(&size) {
            return Err(Error::new(format!("ore size {size} outside of [0, 64]")));
        }
        let discard = float_field(json, "discard_chance_on_air_exposure")?;
        if !(0.0..=1.0).contains(&discard) {
            return Err(Error::new(format!(
                "discard_chance_on_air_exposure {discard} outside of [0, 1]"
            )));
        }
        Ok(Self {
            targets: BlockReplacement::parse_list(json, parser)?,
            size,
            discard_chance_on_air_exposure: discard,
        })
    }

    /// `canPlaceOre(state, blockGetter, random, targetState, orePos)`.
    fn can_place_ore<R: RandomSource>(
        &self,
        state: BlockState,
        block_getter: impl Fn(BlockPos) -> BlockState,
        random: &mut R,
        target: &BlockReplacement,
        pos: BlockPos,
    ) -> bool {
        if !target.target.test(state, pos, random) {
            return false;
        }
        should_skip_air_check(random, self.discard_chance_on_air_exposure)
            || !is_adjacent_to_air(block_getter, pos)
    }
}

/// `AbstractOreFeature.isAdjacentToAir`.
fn is_adjacent_to_air(block_getter: impl Fn(BlockPos) -> BlockState, pos: BlockPos) -> bool {
    Direction::ALL
        .iter()
        .any(|&d| block_getter(pos.relative(d)).is_air())
}

/// `AbstractOreFeature.shouldSkipAirCheck`.
fn should_skip_air_check<R: RandomSource>(random: &mut R, discard_chance: f32) -> bool {
    if discard_chance <= 0.0 {
        true
    } else if discard_chance >= 1.0 {
        false
    } else {
        random.next_float() >= discard_chance
    }
}

/// `ore`: a blob of ore along a random line through the origin, written straight into the
/// chunk sections (`BulkSectionAccess`: no heightmap updates, no block entities, no
/// post-processing marks).
#[derive(Debug)]
pub struct OreFeature {
    config: OreConfig,
}

impl FeatureType for OreFeature {
    fn parse(json: &Value, parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        Ok(Self {
            config: OreConfig::parse(json, parser)?,
        })
    }

    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        let size = self.config.size;
        let dir = ctx.random.next_float() * std::f32::consts::PI;
        let spread_xy = size as f32 / 8.0;
        let max_radius = ceil_f((size as f32 / 16.0 * 2.0 + 1.0) / 2.0);
        let (sin_dir, cos_dir) = (f64::from(dir).sin(), f64::from(dir).cos());
        let spread = f64::from(spread_xy);
        let x0 = f64::from(origin.x) + sin_dir * spread;
        let x1 = f64::from(origin.x) - sin_dir * spread;
        let z0 = f64::from(origin.z) + cos_dir * spread;
        let z1 = f64::from(origin.z) - cos_dir * spread;
        let y0 = f64::from(
            origin
                .y
                .wrapping_add(ctx.random.next_int_bounded(3))
                .wrapping_sub(2),
        );
        let y1 = f64::from(
            origin
                .y
                .wrapping_add(ctx.random.next_int_bounded(3))
                .wrapping_sub(2),
        );
        let spread_ceil = ceil_f(spread_xy);
        let start = BlockPos::new(
            origin.x - spread_ceil - max_radius,
            origin.y - 2 - max_radius,
            origin.z - spread_ceil - max_radius,
        );
        let size_xz = 2 * (spread_ceil + max_radius);
        let size_y = 2 * (2 + max_radius);
        for x in start.x..=start.x + size_xz {
            for z in start.z..=start.z + size_xz {
                if start.y <= ctx.level.height_at(HeightmapKind::OceanFloorWg, x, z) {
                    let line = [x0, x1, y0, y1, z0, z1];
                    return self.do_place(ctx, line, start, size_xz, size_y);
                }
            }
        }
        false
    }
}

impl OreFeature {
    /// `doPlace`: `line` is `[x0, x1, y0, y1, z0, z1]`.
    fn do_place(
        &self,
        ctx: &mut FeatureContext<'_, '_>,
        line: [f64; 6],
        start: BlockPos,
        size_xz: i32,
        size_y: i32,
    ) -> bool {
        let [x0, x1, y0, y1, z0, z1] = line;
        let size = self.config.size as usize;
        let mut placed = 0;
        let mut tested = BitSet::with_bits((size_xz * size_y * size_xz).max(0) as usize);
        // `data`: x, y, z, radius per point.
        let mut data = [[0.0f64; 4]; 64];
        for (i, point) in data.iter_mut().enumerate().take(size) {
            let step = i as f32 / size as f32;
            let xx = lerp(f64::from(step), x0, x1);
            let yy = lerp(f64::from(step), y0, y1);
            let zz = lerp(f64::from(step), z0, z1);
            let ss = ctx.random.next_double() * size as f64 / 16.0;
            let r = (f64::from(sin(f64::from(std::f32::consts::PI * step)) + 1.0) * ss + 1.0) / 2.0;
            *point = [xx, yy, zz, r];
        }

        for i1 in 0..size.saturating_sub(1) {
            if data[i1][3] <= 0.0 {
                continue;
            }
            for i2 in i1 + 1..size {
                if data[i2][3] <= 0.0 {
                    continue;
                }
                let dx = data[i1][0] - data[i2][0];
                let dy = data[i1][1] - data[i2][1];
                let dz = data[i1][2] - data[i2][2];
                let dr = data[i1][3] - data[i2][3];
                if dr * dr > dx * dx + dy * dy + dz * dz {
                    if dr > 0.0 {
                        data[i2][3] = -1.0;
                    } else {
                        data[i1][3] = -1.0;
                    }
                }
            }
        }

        for &[xx, yy, zz, r] in &data[..size] {
            if r < 0.0 {
                continue;
            }
            let x_min = floor(xx - r).max(start.x);
            let y_min = floor(yy - r).max(start.y);
            let z_min = floor(zz - r).max(start.z);
            let x_max = floor(xx + r).max(x_min);
            let y_max = floor(yy + r).max(y_min);
            let z_max = floor(zz + r).max(z_min);
            for x in x_min..=x_max {
                let xd = (f64::from(x) + 0.5 - xx) / r;
                if xd * xd >= 1.0 {
                    continue;
                }
                for y in y_min..=y_max {
                    let yd = (f64::from(y) + 0.5 - yy) / r;
                    if xd * xd + yd * yd >= 1.0 {
                        continue;
                    }
                    for z in z_min..=z_max {
                        let zd = (f64::from(z) + 0.5 - zz) / r;
                        if xd * xd + yd * yd + zd * zd >= 1.0
                            || ctx.level.is_outside_build_height(y)
                        {
                            continue;
                        }
                        let index = (x - start.x)
                            + (y - start.y) * size_xz
                            + (z - start.z) * size_xz * size_y;
                        if !tested.insert(index as usize) {
                            continue;
                        }
                        let pos = BlockPos::new(x, y, z);
                        if !ctx.level.ensure_can_write(pos) {
                            continue;
                        }
                        if self.place_in_section(ctx, pos) {
                            placed += 1;
                        }
                    }
                }
            }
        }
        placed > 0
    }

    /// The body of the innermost loop: tests the targets against the section's block and writes
    /// the first matching replacement into the section.
    fn place_in_section(&self, ctx: &mut FeatureContext<'_, '_>, pos: BlockPos) -> bool {
        let level = &*ctx.level;
        let state = level.chunk(pos).block(pos.x, pos.y, pos.z);
        // `BulkSectionAccess.getBlockState`: air outside the build height.
        let block_getter = |p: BlockPos| level.chunk(p).block(p.x, p.y, p.z);
        let Some(target) = self.config.targets.iter().find(|target| {
            self.config
                .can_place_ore(state, block_getter, ctx.random, target, pos)
        }) else {
            return false;
        };
        let replacement = target.state;
        ctx.level
            .chunk_mut(pos)
            .set_block_in_section(pos.x, pos.y, pos.z, replacement);
        true
    }
}

/// A growable bit set (`java.util.BitSet`): Vanilla's indices can exceed the initial size.
struct BitSet(Vec<u64>);

impl BitSet {
    fn with_bits(bits: usize) -> Self {
        Self(vec![0; bits.div_ceil(64)])
    }

    /// Sets a bit; whether it was clear before.
    fn insert(&mut self, index: usize) -> bool {
        let word = index / 64;
        if word >= self.0.len() {
            self.0.resize(word + 1, 0);
        }
        let mask = 1u64 << (index % 64);
        let was_clear = self.0[word] & mask == 0;
        self.0[word] |= mask;
        was_clear
    }
}

/// `scattered_ore`: single ore blocks scattered around the origin, set through the level.
#[derive(Debug)]
pub struct ScatteredOreFeature {
    config: OreConfig,
}

impl FeatureType for ScatteredOreFeature {
    fn parse(json: &Value, parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        Ok(Self {
            config: OreConfig::parse(json, parser)?,
        })
    }

    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        let tries = ctx.random.next_int_bounded(self.config.size + 1);
        for i in 0..tries {
            let max_dist = i.min(7);
            let xd = axis_offset(ctx.random, max_dist);
            let yd = axis_offset(ctx.random, max_dist);
            let zd = axis_offset(ctx.random, max_dist);
            let pos = origin.offset(xd, yd, zd);
            let state = ctx.level.block(pos);
            let level = &*ctx.level;
            let target = self.config.targets.iter().find(|target| {
                self.config
                    .can_place_ore(state, |p| level.block(p), ctx.random, target, pos)
            });
            if let Some(target) = target {
                let replacement = target.state;
                ctx.level.set_block(pos, replacement, flags::CLIENTS);
            }
        }
        true
    }
}

/// `getRandomPlacementInOneAxisRelativeToOrigin`.
fn axis_offset<R: RandomSource>(random: &mut R, max_distance: i32) -> i32 {
    round_f((random.next_float() - random.next_float()) * max_distance as f32)
}

//! `OreFeature` and `ScatteredOreFeature` with their shared `AbstractOreFeature` configuration.

use serde_json::Value;
use vanillite_base::math::{ceil_f, floor, lerp, sin};
use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Direction};
use vanillite_chunk::HeightmapKind;
use vanillite_data::block::{BLOCK_COUNT, BlockId, BlockState};

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
    /// Per block id, when the block alone decides the test of every target (at most eight):
    /// bit `i` is set where the test of `targets[i]` passes.
    by_block: Option<Box<[u8]>>,
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
        let targets = BlockReplacement::parse_list(json, parser)?;
        let by_block = (targets.len() <= 8)
            .then(|| {
                (0..BLOCK_COUNT)
                    .map(|block| {
                        let block = BlockId(block as u16);
                        targets.iter().enumerate().try_fold(0, |mask, (i, target)| {
                            Some(mask | (u8::from(target.target.test_block(block)?) << i))
                        })
                    })
                    .collect()
            })
            .flatten();
        Ok(Self {
            targets,
            size,
            discard_chance_on_air_exposure: discard,
            by_block,
        })
    }

    /// The first target that takes `state` at `pos`: `canPlaceOre` of the targets in order.
    fn target_for<R: RandomSource>(
        &self,
        state: BlockState,
        block_getter: impl Fn(BlockPos) -> BlockState + Copy,
        random: &mut R,
        pos: BlockPos,
    ) -> Option<&BlockReplacement> {
        let Some(by_block) = &self.by_block else {
            return self
                .targets
                .iter()
                .find(|target| self.can_place_ore(state, block_getter, random, target, pos));
        };
        let mut passing = by_block
            .get(usize::from(state.block().0))
            .copied()
            .unwrap_or(0);
        while passing != 0 {
            let target = passing.trailing_zeros() as usize;
            passing &= passing - 1;
            // `canPlaceOre` after its test.
            if should_skip_air_check(random, self.discard_chance_on_air_exposure)
                || !is_adjacent_to_air(block_getter, pos)
            {
                return Some(&self.targets[target]);
            }
        }
        None
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

        let FeatureContext { level, random, .. } = ctx;
        // The chunk of the last block as `(x, z, slot if writable)`: a blob touches few chunks.
        let mut chunk = (i32::MIN, i32::MIN, None);
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
            for x in (0..x_max - x_min + 1).map(|offset| x_min + offset) {
                let xd = (f64::from(x) + 0.5 - xx) / r;
                let xd_sqr = xd * xd;
                if xd_sqr >= 1.0 {
                    continue;
                }
                for y in (0..y_max - y_min + 1).map(|offset| y_min + offset) {
                    let yd = (f64::from(y) + 0.5 - yy) / r;
                    let xyd_sqr = xd_sqr + yd * yd;
                    if xyd_sqr >= 1.0 || level.is_outside_build_height(y) {
                        continue;
                    }
                    for z in (0..z_max - z_min + 1).map(|offset| z_min + offset) {
                        let zd = (f64::from(z) + 0.5 - zz) / r;
                        if xyd_sqr + zd * zd >= 1.0 {
                            continue;
                        }
                        let index = (x - start.x)
                            + (y - start.y) * size_xz
                            + (z - start.z) * size_xz * size_y;
                        if !tested.insert(index as usize) {
                            continue;
                        }
                        if (chunk.0, chunk.1) != (x >> 4, z >> 4) {
                            // `ensureCanWrite(pos)`.
                            chunk = (x >> 4, z >> 4, level.writable_chunk(x >> 4, z >> 4));
                        }
                        let Some(slot) = chunk.2 else {
                            continue;
                        };
                        // The targets against the section's block; the first matching
                        // replacement goes straight into the section.
                        let pos = BlockPos::new(x, y, z);
                        let state = level.chunk_at(slot).block(x, y, z);
                        let shared = &**level;
                        // `BulkSectionAccess.getBlockState`: air outside the build height.
                        let block_getter = |p: BlockPos| shared.chunk(p).block(p.x, p.y, p.z);
                        let target = self.config.target_for(state, block_getter, *random, pos);
                        if let Some(replacement) = target.map(|target| target.state) {
                            level
                                .chunk_at_mut(slot)
                                .set_block_in_section(x, y, z, replacement);
                            placed += 1;
                        }
                    }
                }
            }
        }
        placed > 0
    }
}

/// A growable bit set (`java.util.BitSet`): Vanilla's indices can exceed the initial size.
struct BitSet(Vec<u64>);

impl BitSet {
    fn with_bits(bits: usize) -> Self {
        Self(vec![0; bits.div_ceil(64)])
    }

    /// Sets a bit; whether it was clear before.
    #[inline]
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
            let target = self
                .config
                .target_for(state, |p| level.block(p), ctx.random, pos);
            if let Some(replacement) = target.map(|target| target.state) {
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

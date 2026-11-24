//! `Beardifier`: the density the terrain stage adds around structures with terrain adaptation
//! (`beardifier` in the noise router).

use std::sync::OnceLock;

use vanillite_base::{BlockPos, ChunkPos};

use super::pool::{JigsawJunction, Projection};
use super::start::ReferencedStarts;
use super::{StructureRegistry, TerrainAdjustment};
use crate::density::ContextSampler;
use crate::template::BoundingBox;
use crate::volume::DensityVolume;

/// `BEARD_KERNEL_RADIUS`.
const KERNEL_RADIUS: i32 = 12;
/// `BEARD_KERNEL_SIZE`.
const KERNEL_SIZE: i32 = 24;

/// `BEARD_KERNEL`: `exp(-(dx² + (dy + 0.5)² + dz²) / 16)` as floats, z outer, then x, then y.
fn kernel() -> &'static [f32] {
    static KERNEL: OnceLock<Vec<f32>> = OnceLock::new();
    KERNEL.get_or_init(|| {
        let mut kernel = vec![0.0_f32; (KERNEL_SIZE * KERNEL_SIZE * KERNEL_SIZE) as usize];
        for zi in 0..KERNEL_SIZE {
            for xi in 0..KERNEL_SIZE {
                for yi in 0..KERNEL_SIZE {
                    let (dx, dy, dz) = (
                        f64::from(xi - KERNEL_RADIUS),
                        f64::from(yi - KERNEL_RADIUS) + 0.5,
                        f64::from(zi - KERNEL_RADIUS),
                    );
                    let distance_sqr = dx * dx + dy * dy + dz * dz;
                    kernel[(zi * KERNEL_SIZE * KERNEL_SIZE + xi * KERNEL_SIZE + yi) as usize] =
                        std::f64::consts::E.powf(-distance_sqr / 16.0) as f32;
                }
            }
        }
        kernel
    })
}

/// `Beardifier.Rigid`.
#[derive(Clone, Copy, Debug)]
struct Rigid {
    bx: BoundingBox,
    adjustment: TerrainAdjustment,
    ground_level_delta: i32,
}

/// `Beardifier` of one chunk. Built only when some piece is close to the chunk; Vanilla's
/// `EMPTY` beardifier is the absent one (the router's fallback 0).
#[derive(Clone, Debug)]
pub struct Beardifier {
    rigids: Vec<Rigid>,
    junctions: Vec<JigsawJunction>,
    affected_box: BoundingBox,
}

impl Beardifier {
    /// `forStructuresInChunk(structureManager, chunkPos)` over the starts `chunk_pos`
    /// references; `None` for Vanilla's `EMPTY`.
    pub fn for_structures_in_chunk(
        registry: &StructureRegistry,
        starts: &ReferencedStarts,
        chunk_pos: ChunkPos,
    ) -> Option<Self> {
        let (chunk_x, chunk_z) = (chunk_pos.min_block_x(), chunk_pos.min_block_z());
        let mut rigids = Vec::new();
        let mut junctions = Vec::new();
        let mut any: Option<BoundingBox> = None;
        let include = |any: &mut Option<BoundingBox>, bx: BoundingBox| match any {
            Some(b) => b.encapsulate(&bx),
            None => *any = Some(bx),
        };
        for (structure, start) in starts.all() {
            let adjustment = registry.structure(*structure).terrain_adaptation();
            if adjustment == TerrainAdjustment::None {
                continue;
            }
            for piece in start.pieces().iter() {
                if !piece.is_close_to_chunk(chunk_pos, 12) {
                    continue;
                }
                let bx = *piece.bounding_box();
                let Some(pool) = piece.pool_element() else {
                    rigids.push(Rigid {
                        bx,
                        adjustment,
                        ground_level_delta: 0,
                    });
                    include(&mut any, bx);
                    continue;
                };
                if pool.projection == Projection::Rigid {
                    rigids.push(Rigid {
                        bx,
                        adjustment,
                        ground_level_delta: pool.ground_level_delta,
                    });
                    include(&mut any, bx);
                }
                for junction in pool.junctions {
                    let (x, z) = (junction.source_x, junction.source_z);
                    if x > chunk_x - 12
                        && z > chunk_z - 12
                        && x < chunk_x + 15 + 12
                        && z < chunk_z + 15 + 12
                    {
                        junctions.push(*junction);
                        include(
                            &mut any,
                            BoundingBox::of_block(BlockPos::new(x, junction.source_ground_y, z)),
                        );
                    }
                }
            }
        }
        let affected_box = any?.inflated_by(24, 24, 24);
        Some(Self {
            rigids,
            junctions,
            affected_box,
        })
    }

    /// The number of rigid pieces (`pieces`).
    pub fn rigid_count(&self) -> usize {
        self.rigids.len()
    }

    /// The number of jigsaw junctions (`junctions`).
    pub fn junction_count(&self) -> usize {
        self.junctions.len()
    }

    /// `affectedBox`: where the beardifier contributes.
    pub fn affected_box(&self) -> BoundingBox {
        self.affected_box
    }

    /// `sampleValueUnchecked(blockX, blockY, blockZ)`.
    fn sample_unchecked(&self, x: i32, y: i32, z: i32) -> f32 {
        let mut value = 0.0_f32;
        for rigid in &self.rigids {
            let bx = &rigid.bx;
            let dx = 0.max((bx.min_x() - x).max(x - bx.max_x()));
            let dz = 0.max((bx.min_z() - z).max(z - bx.max_z()));
            let ground_y = bx.min_y() + rigid.ground_level_delta;
            let dy_to_ground = y - ground_y;
            let dy = match rigid.adjustment {
                TerrainAdjustment::None => 0,
                TerrainAdjustment::Bury | TerrainAdjustment::BeardThin => dy_to_ground,
                TerrainAdjustment::BeardBox => 0.max((ground_y - y).max(y - bx.max_y())),
                TerrainAdjustment::Encapsulate => 0.max((bx.min_y() - y).max(y - bx.max_y())),
            };
            value += match rigid.adjustment {
                TerrainAdjustment::None => 0.0,
                TerrainAdjustment::Bury => bury_contribution(dx as f32, dy as f32 / 2.0, dz as f32),
                TerrainAdjustment::BeardThin | TerrainAdjustment::BeardBox => {
                    beard_contribution(dx, dy, dz, dy_to_ground) * 0.8
                }
                TerrainAdjustment::Encapsulate => {
                    bury_contribution(dx as f32 / 2.0, dy as f32 / 2.0, dz as f32 / 2.0) * 0.8
                }
            };
        }
        for junction in &self.junctions {
            let dx = x - junction.source_x;
            let dy = y - junction.source_ground_y;
            let dz = z - junction.source_z;
            value += beard_contribution(dx, dy, dz, dy) * 0.4;
        }
        value
    }
}

impl ContextSampler for Beardifier {
    /// `sampleValue(context, blockX, blockY, blockZ)`.
    fn sample_value(&self, x: i32, y: i32, z: i32) -> f32 {
        if self.affected_box.is_inside(BlockPos::new(x, y, z)) {
            self.sample_unchecked(x, y, z)
        } else {
            0.0
        }
    }

    /// `sampleVolume(context, outputBuffer, volume)`.
    fn sample_volume(&self, out: &mut [f32], v: &DensityVolume) {
        out.fill(0.0);
        let bx = &self.affected_box;
        let intersects = bx.intersects(&BoundingBox::new(
            v.min_x,
            v.min_y,
            v.min_z,
            v.max_x(),
            v.max_y(),
            v.max_z(),
        ));
        if !intersects {
            return;
        }
        let min_x = 0.max(bx.min_x() - v.min_x).div_euclid(v.step_x);
        let min_y = 0.max(bx.min_y() - v.min_y).div_euclid(v.step_y);
        let min_z = 0.max(bx.min_z() - v.min_z).div_euclid(v.step_z);
        let max_x = (v.size_x - 1).min((bx.max_x() - v.min_x).div_euclid(v.step_x));
        let max_y = (v.size_y - 1).min((bx.max_y() - v.min_y).div_euclid(v.step_y));
        let max_z = (v.size_z - 1).min((bx.max_z() - v.min_z).div_euclid(v.step_z));
        for z in min_z..=max_z {
            let block_z = v.block_z(z);
            for x in min_x..=max_x {
                let block_x = v.block_x(x);
                for y in min_y..=max_y {
                    out[v.index(x, y, z)] = self.sample_unchecked(block_x, v.block_y(y), block_z);
                }
            }
        }
    }
}

/// `getBuryContribution(dx, dy, dz)`.
fn bury_contribution(dx: f32, dy: f32, dz: f32) -> f32 {
    let distance_sq = dx * dx + dy * dy + dz * dz;
    if distance_sq >= 36.0 {
        0.0
    } else {
        1.0 - crate::mth::sqrt_f(distance_sq) / 6.0
    }
}

/// `getBeardContribution(dx, dy, dz, yToGround)`.
fn beard_contribution(dx: i32, dy: i32, dz: i32, y_to_ground: i32) -> f32 {
    let xi = dx + KERNEL_RADIUS;
    let yi = dy + KERNEL_RADIUS;
    let zi = dz + KERNEL_RADIUS;
    let in_range = |i: i32| (0..KERNEL_SIZE).contains(&i);
    if !(in_range(xi) && in_range(yi) && in_range(zi)) {
        return 0.0;
    }
    let dy_with_offset = y_to_ground as f32 + 0.5;
    let distance_sqr =
        (dx as f32) * (dx as f32) + dy_with_offset * dy_with_offset + (dz as f32) * (dz as f32);
    let value = -dy_with_offset * fast_inv_sqrt(f64::from(distance_sqr / 2.0)) as f32 / 2.0;
    value * kernel()[(zi * KERNEL_SIZE * KERNEL_SIZE + xi * KERNEL_SIZE + yi) as usize]
}

/// `Mth.fastInvSqrt(double)`.
fn fast_inv_sqrt(x: f64) -> f64 {
    let half = 0.5 * x;
    let i = 6_910_469_410_427_058_090_i64 - ((x.to_bits() as i64) >> 1);
    let y = f64::from_bits(i as u64);
    y * (1.5 - half * y * y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fast_inv_sqrt_is_close() {
        for x in [0.5, 1.0, 2.0, 10.0, 123.25] {
            let exact = 1.0 / f64::sqrt(x);
            assert!((fast_inv_sqrt(x) - exact).abs() / exact < 0.01, "{x}");
        }
        // Bit for bit as `Mth.fastInvSqrt` in Java.
        let bits = [4.0, 0.125, 37.5].map(|x| fast_inv_sqrt(x).to_bits());
        assert_eq!(
            bits,
            [
                0x3fdf_f223_eb08_e347,
                0x4006_9f2a_ee57_a7ac,
                0x3fc4_df58_5eb9_dc78
            ]
        );
    }

    #[test]
    fn kernel_peaks_at_the_centre() {
        let k = kernel();
        let centre = (12 * 24 * 24 + 12 * 24 + 12) as usize;
        assert_eq!(k[centre], (std::f64::consts::E.powf(-0.25 / 16.0)) as f32);
        assert!(k.iter().all(|&v| v <= k[centre]));
        // dx 1, dy 11 (+0.5), dz 3, as Java computes it.
        let index = (15 * 24 * 24 + 13 * 24 + 23) as usize;
        assert_eq!(k[index].to_bits(), 0x3910_5c79);
    }
}

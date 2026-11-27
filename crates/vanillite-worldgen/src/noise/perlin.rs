//! `PerlinNoise` and its smeared variant `SmearedPerlinNoise` (one octave each).
//!
//! `get` and the batched `add_to_volume` round differently (`addToVolume` sums the XZ part of each
//! corner's dot product first), exactly as in Vanilla, so both are ported separately.

use vanillite_base::math::floor;
use vanillite_base::random::RandomSource;

use super::gradient::{GradientNoise, dot_xz, smoothstep, wrap};
use super::lanes::F32x4;
use super::stack::Octave;
use crate::interval::Interval;
use crate::mth::lerp_f;
use crate::volume::DensityVolume;

/// `SmearedPerlinNoise.SHIFT_UP_EPSILON`, a `float` widened to `double` where it is used.
const SHIFT_UP_EPSILON: f32 = 1.0e-7;

/// One octave of improved Perlin noise (`PerlinNoise`).
#[derive(Clone, Debug)]
pub(super) struct PerlinNoise {
    base: GradientNoise,
}

/// The XZ parts of the eight corner dot products and the corner gradients' Y components of the
/// lattice cells of the lanes of [`add_columns`], corner by corner, cached while the cells' Y
/// does not change (`addToVolume`).
#[derive(Clone, Copy, Default)]
struct Cells {
    xz: [[f32; LANES]; 8],
    gy: [[f32; LANES]; 8],
}

impl Cells {
    /// Sets lane `lane` to the corner values of the cell at `floor_y` above `column`, in
    /// `lerp3` order.
    #[inline(always)]
    fn set(&mut self, lane: usize, noise: &GradientNoise, column: &Column, floor_y: i32) {
        let (x0, x1, floor_z) = (column.x0, column.x1, column.z.floor);
        let (rx, rz) = (column.x.relative, column.z.relative);
        // (Each pair holds the permutations of an index and of the next one.)
        let xy0 = noise.pair(x0.wrapping_add(floor_y));
        let xy1 = noise.pair(x1.wrapping_add(floor_y));
        let rx1 = rx - 1.0;
        let rz1 = rz - 1.0;
        // The corners with `y`, `y + 1` at `x` and at `x + 1`, each at `z` and `z + 1`.
        let corners = [
            (xy0.first(), rx),
            (xy1.first(), rx1),
            (xy0.second(), rx),
            (xy1.second(), rx1),
        ];
        for (i, (xy, cx)) in corners.into_iter().enumerate() {
            let xyz = noise.pair(xy.wrapping_add(floor_z));
            let (g, g1) = (xyz.first_grad(), xyz.second_grad());
            self.xz[i][lane] = dot_xz(g, cx, rz);
            self.gy[i][lane] = g[1];
            self.xz[i + 4][lane] = dot_xz(g1, cx, rz1);
            self.gy[i + 4][lane] = g1[1];
        }
    }
}

/// A wrapped, offset coordinate split into its lattice cell, the `float` position inside the
/// cell and its smoothstep.
#[derive(Clone, Copy)]
struct Lattice {
    floor: i32,
    relative: f32,
    alpha: f32,
}

impl Lattice {
    #[inline(always)]
    fn new(coordinate: f64, offset: f64) -> Self {
        let c = wrap(coordinate) + offset;
        let floor = floor(c);
        let relative = (c - f64::from(floor)) as f32;
        Self {
            floor,
            relative,
            alpha: smoothstep(relative),
        }
    }
}

/// The X and Z values of one (Z, X) column in `addToVolume`, with the X permutations.
#[derive(Clone, Copy)]
struct Column {
    x: Lattice,
    z: Lattice,
    x0: i32,
    x1: i32,
}

/// The Y values of one row of a volume in `addToVolume`: the lattice cell, the position inside
/// it the corner gradients are multiplied with, and the smoothstep. They are the same in every
/// column, so each row computes them once per call.
#[derive(Clone, Copy, Default)]
struct Row {
    known: bool,
    floor: i32,
    relative: f32,
    alpha: f32,
}

/// The rows of a volume, computed when a column first needs them.
struct Rows {
    short: [Row; Self::SHORT],
    long: Vec<Row>,
}

impl Rows {
    /// Volumes up to this height keep their rows on the stack.
    const SHORT: usize = 64;

    #[inline(always)]
    fn new(size_y: usize) -> Self {
        Self {
            short: [Row::default(); Self::SHORT],
            long: if size_y > Self::SHORT {
                vec![Row::default(); size_y]
            } else {
                Vec::new()
            },
        }
    }

    /// Row `index`, from `compute` at its first use.
    #[inline(always)]
    fn get(&mut self, index: usize, compute: impl FnOnce() -> (i32, f32, f32)) -> Row {
        let row = if self.long.is_empty() {
            &mut self.short[index]
        } else {
            &mut self.long[index]
        };
        if !row.known {
            let (floor, relative, alpha) = compute();
            *row = Row {
                known: true,
                floor,
                relative,
                alpha,
            };
        }
        *row
    }
}

/// The points of a volume `add_to_volume` computes: all, or those of a mask.
trait Points: Copy {
    /// Whether any point of the column starting at `start` (`len` points) is needed.
    fn any(self, start: usize, len: usize) -> bool;
    /// The points of the column starting at `start` (`len` points), indexed from 0.
    fn column(self, start: usize, len: usize) -> Self;
    /// Whether point `index` is needed.
    fn has(self, index: usize) -> bool;
}

/// Every point.
#[derive(Clone, Copy)]
struct All;

impl Points for All {
    #[inline(always)]
    fn any(self, _: usize, _: usize) -> bool {
        true
    }

    #[inline(always)]
    fn column(self, _: usize, _: usize) -> Self {
        self
    }

    #[inline(always)]
    fn has(self, _: usize) -> bool {
        true
    }
}

impl Points for &[bool] {
    #[inline(always)]
    fn any(self, start: usize, len: usize) -> bool {
        self[start..start + len].contains(&true)
    }

    #[inline(always)]
    fn column(self, start: usize, len: usize) -> Self {
        &self[start..start + len]
    }

    #[inline(always)]
    fn has(self, index: usize) -> bool {
        self[index]
    }
}

/// The columns [`add_columns`] samples together, one per lane of its vector steps.
const LANES: usize = 4;

/// `addToVolume` at `points`, the outer loops and the cell interpolation of both Perlin noise
/// kinds: `row_of(index_y)` gives a row's lattice cell, relative Y and alpha. Runs of
/// [`LANES`] consecutive (Z, X) columns go through the rows together, each lane doing exactly
/// the arithmetic of one column. A column's cell is computed when its Y cell changes at a
/// needed point of any lane, so the values do not depend on which points are skipped.
#[inline(always)]
fn add_columns<P: Points>(
    noise: &GradientNoise,
    buffer: &mut [f32],
    volume: &DensityVolume,
    xz_scale: f64,
    amplitude: f32,
    points: P,
    mut row_of: impl FnMut(i32) -> (i32, f32, f32),
) {
    let size_y = volume.size_y as usize;
    let size_x = volume.size_x.max(0) as usize;
    let columns = size_x * volume.size_z.max(0) as usize;
    if size_y == 0 || columns == 0 {
        return;
    }
    let mut rows = Rows::new(size_y);
    let amplitude = F32x4::splat(amplitude);
    for (group_index, group) in buffer[..columns * size_y]
        .chunks_mut(LANES * size_y)
        .enumerate()
    {
        let start = group_index * LANES;
        let count = group.len() / size_y;
        let mut lanes: [Option<Column>; LANES] = [None; LANES];
        let mut lane_points = [points; LANES];
        for (l, (lane, lane_points)) in lanes.iter_mut().zip(&mut lane_points).enumerate() {
            let column = start + l;
            if l >= count || !points.any(column * size_y, size_y) {
                continue;
            }
            let (index_x, index_z) = ((column % size_x) as i32, (column / size_x) as i32);
            let x = Lattice::new(
                f64::from(volume.block_x(index_x)) * xz_scale,
                noise.offset_x,
            );
            let z = Lattice::new(
                f64::from(volume.block_z(index_z)) * xz_scale,
                noise.offset_z,
            );
            let xs = noise.pair(x.floor);
            *lane = Some(Column {
                x0: xs.first(),
                x1: xs.second(),
                x,
                z,
            });
            *lane_points = points.column(column * size_y, size_y);
        }
        if lanes.iter().all(Option::is_none) {
            continue;
        }
        let (mut alpha_x, mut alpha_z) = ([0.0f32; LANES], [0.0f32; LANES]);
        for (l, lane) in lanes.iter().enumerate() {
            if let Some(column) = lane {
                alpha_x[l] = column.x.alpha;
                alpha_z[l] = column.z.alpha;
            }
        }
        let (alpha_x, alpha_z) = (F32x4::new(alpha_x), F32x4::new(alpha_z));
        let mut cells = Cells::default();
        let (mut xz, mut gy) = ([F32x4::splat(0.0); 8], [F32x4::splat(0.0); 8]);
        let mut last_floor_y = i32::MIN;
        for index_y in 0..size_y {
            let mut needed = [false; LANES];
            for ((need, lane), lane_points) in needed.iter_mut().zip(&lanes).zip(lane_points) {
                *need = lane.is_some() && lane_points.has(index_y);
            }
            if !needed.contains(&true) {
                continue;
            }
            let row = rows.get(index_y, || row_of(index_y as i32));
            if last_floor_y != row.floor {
                for (l, lane) in lanes.iter().enumerate() {
                    if let Some(column) = lane {
                        cells.set(l, noise, column, row.floor);
                    }
                }
                for corner in 0..8 {
                    xz[corner] = F32x4::new(cells.xz[corner]);
                    gy[corner] = F32x4::new(cells.gy[corner]);
                }
                last_floor_y = row.floor;
            }
            // `lerp3` of the corners `xz + gy * ry` (`ry - 1` at `y + 1`), as `CellColumn` of
            // one column would compute it.
            let (ry, ry1) = (F32x4::splat(row.relative), F32x4::splat(row.relative - 1.0));
            let corner = |i: usize, ry: F32x4| xz[i].add(gy[i].mul(ry));
            let alpha_y = F32x4::splat(row.alpha);
            let lerp2 = |x00: F32x4, x10: F32x4, x01: F32x4, x11: F32x4| {
                F32x4::lerp(
                    alpha_y,
                    F32x4::lerp(alpha_x, x00, x10),
                    F32x4::lerp(alpha_x, x01, x11),
                )
            };
            let value = F32x4::lerp(
                alpha_z,
                lerp2(corner(0, ry), corner(1, ry), corner(2, ry1), corner(3, ry1)),
                lerp2(corner(4, ry), corner(5, ry), corner(6, ry1), corner(7, ry1)),
            );
            let values = amplitude.mul(value).to_array();
            for (l, (need, value)) in needed.into_iter().zip(values).enumerate() {
                if need {
                    group[l * size_y + index_y] += value;
                }
            }
        }
    }
}

impl PerlinNoise {
    /// `new PerlinNoise(random)`.
    pub fn new(random: &mut impl RandomSource) -> Self {
        Self {
            base: GradientNoise::new(random, 256.0),
        }
    }

    /// `sampleAndLerp(x, y, z, relativeX, relativeY, relativeZ, originalRelativeY)`.
    #[allow(clippy::too_many_arguments)]
    #[inline(always)]
    fn sample_and_lerp(
        &self,
        x: i32,
        y: i32,
        z: i32,
        rx: f32,
        ry: f32,
        rz: f32,
        original_ry: f32,
    ) -> f32 {
        let n = &self.base;
        // (Each pair holds the permutations of an index and of the next one.)
        let xs = n.pair(x);
        let xy0 = n.pair(xs.first().wrapping_add(y));
        let xy1 = n.pair(xs.second().wrapping_add(y));
        let c00 = n.pair(xy0.first().wrapping_add(z));
        let c10 = n.pair(xy1.first().wrapping_add(z));
        let c01 = n.pair(xy0.second().wrapping_add(z));
        let c11 = n.pair(xy1.second().wrapping_add(z));
        // The corners' dot products in lanes, `x` and `x + 1` apart: (y, z), (y + 1, z),
        // (y, z + 1), (y + 1, z + 1); each lane computes `dot` of one corner.
        let (rx1, ry1, rz1) = (rx - 1.0, ry - 1.0, rz - 1.0);
        let (ys, zs) = (
            F32x4::new([ry, ry1, ry, ry1]),
            F32x4::new([rz, rz, rz1, rz1]),
        );
        let dots = |g: [&[f32; 3]; 4], xs: F32x4| {
            let component = |i: usize| F32x4::new([g[0][i], g[1][i], g[2][i], g[3][i]]);
            component(0)
                .mul(xs)
                .add(component(1).mul(ys))
                .add(component(2).mul(zs))
        };
        let low = dots(
            [
                c00.first_grad(),
                c01.first_grad(),
                c00.second_grad(),
                c01.second_grad(),
            ],
            F32x4::splat(rx),
        );
        let high = dots(
            [
                c10.first_grad(),
                c11.first_grad(),
                c10.second_grad(),
                c11.second_grad(),
            ],
            F32x4::splat(rx1),
        );
        let [x_alpha, y_alpha, z_alpha, _] = F32x4::new([rx, original_ry, rz, 0.0])
            .smoothstep()
            .to_array();
        // `lerp3`: along X in the lanes, then along Y and Z.
        let [x00, x10, x01, x11] = F32x4::lerp(F32x4::splat(x_alpha), low, high).to_array();
        lerp_f(
            z_alpha,
            lerp_f(y_alpha, x00, x10),
            lerp_f(y_alpha, x01, x11),
        )
    }
}

impl Octave for PerlinNoise {
    fn range(&self) -> Interval {
        Interval::of_symmetric(2.0)
    }

    fn get(&self, x: f64, y: f64, z: f64) -> f32 {
        let x = wrap(x) + self.base.offset_x;
        let y = wrap(y) + self.base.offset_y;
        let z = wrap(z) + self.base.offset_z;
        let floor_x = floor(x);
        let floor_y = floor(y);
        let floor_z = floor(z);
        let rx = (x - f64::from(floor_x)) as f32;
        let ry = (y - f64::from(floor_y)) as f32;
        let rz = (z - f64::from(floor_z)) as f32;
        self.sample_and_lerp(floor_x, floor_y, floor_z, rx, ry, rz, ry)
    }

    fn get2(&self, x: f64, y: f64) -> f32 {
        self.get(wrap(x), 0.0, wrap(y))
    }

    fn add_to_volume(
        &self,
        buffer: &mut [f32],
        volume: &DensityVolume,
        xz_scale: f64,
        y_scale: f64,
        amplitude: f32,
    ) {
        self.add_points(buffer, volume, xz_scale, y_scale, amplitude, All);
    }

    fn add_to_volume_masked(
        &self,
        buffer: &mut [f32],
        volume: &DensityVolume,
        xz_scale: f64,
        y_scale: f64,
        amplitude: f32,
        mask: &[bool],
    ) {
        self.add_points(buffer, volume, xz_scale, y_scale, amplitude, mask);
    }
}

impl PerlinNoise {
    /// `addToVolume` at `points`: the cell column is computed when a needed point enters a new
    /// cell, so the values do not depend on which points are skipped.
    #[inline(always)]
    fn add_points(
        &self,
        buffer: &mut [f32],
        volume: &DensityVolume,
        xz_scale: f64,
        y_scale: f64,
        amplitude: f32,
        points: impl Points,
    ) {
        let n = &self.base;
        add_columns(n, buffer, volume, xz_scale, amplitude, points, |index_y| {
            let y = wrap(f64::from(volume.block_y(index_y)) * y_scale) + n.offset_y;
            let floor_y = floor(y);
            let ry = (y - f64::from(floor_y)) as f32;
            (floor_y, ry, smoothstep(ry))
        });
    }
}

/// One octave of Perlin noise whose Y is snapped to a grid (`SmearedPerlinNoise`), used by the
/// legacy blended terrain noise.
#[derive(Clone, Debug)]
pub(super) struct SmearedPerlinNoise {
    perlin: PerlinNoise,
    fudge_y_scale: f64,
}

impl SmearedPerlinNoise {
    /// `new SmearedPerlinNoise(random, fudgeYScale)`.
    pub fn new(random: &mut impl RandomSource, fudge_y_scale: f64) -> Self {
        Self {
            perlin: PerlinNoise::new(random),
            fudge_y_scale,
        }
    }

    /// `SmearedPerlinNoise.range(fudgeYScale)`.
    pub fn range_of(fudge_y_scale: f64) -> Interval {
        Interval::of_symmetric((fudge_y_scale.abs() + 2.0) as f32)
    }

    /// `computeFudgeY(originalY, relativeY)`.
    #[inline(always)]
    fn compute_fudge_y(&self, original_y: f64, relative_y: f64) -> f64 {
        let fudge_limit = if original_y >= 0.0 && original_y < relative_y {
            original_y
        } else {
            relative_y
        };
        f64::from(floor(
            fudge_limit / self.fudge_y_scale + f64::from(SHIFT_UP_EPSILON),
        )) * self.fudge_y_scale
    }
}

impl Octave for SmearedPerlinNoise {
    fn range(&self) -> Interval {
        Self::range_of(self.fudge_y_scale)
    }

    fn get(&self, original_x: f64, original_y: f64, original_z: f64) -> f32 {
        let n = &self.perlin.base;
        let x = wrap(original_x) + n.offset_x;
        let y = wrap(original_y) + n.offset_y;
        let z = wrap(original_z) + n.offset_z;
        let floor_x = floor(x);
        let floor_y = floor(y);
        let floor_z = floor(z);
        let rx = (x - f64::from(floor_x)) as f32;
        let ry = y - f64::from(floor_y);
        let rz = (z - f64::from(floor_z)) as f32;
        let fudged_ry = (ry - self.compute_fudge_y(original_y, ry)) as f32;
        self.perlin
            .sample_and_lerp(floor_x, floor_y, floor_z, rx, fudged_ry, rz, ry as f32)
    }

    /// `PerlinNoise.get(x, y)`, which calls the smeared `get(x, y, z)`.
    fn get2(&self, x: f64, y: f64) -> f32 {
        self.get(wrap(x), 0.0, wrap(y))
    }

    fn add_to_volume(
        &self,
        buffer: &mut [f32],
        volume: &DensityVolume,
        xz_scale: f64,
        y_scale: f64,
        amplitude: f32,
    ) {
        self.add_points(buffer, volume, xz_scale, y_scale, amplitude, All);
    }

    fn add_to_volume_masked(
        &self,
        buffer: &mut [f32],
        volume: &DensityVolume,
        xz_scale: f64,
        y_scale: f64,
        amplitude: f32,
        mask: &[bool],
    ) {
        self.add_points(buffer, volume, xz_scale, y_scale, amplitude, mask);
    }
}

impl SmearedPerlinNoise {
    /// `addToVolume` at `points` (see [`PerlinNoise::add_points`]).
    #[inline(always)]
    fn add_points(
        &self,
        buffer: &mut [f32],
        volume: &DensityVolume,
        xz_scale: f64,
        y_scale: f64,
        amplitude: f32,
        points: impl Points,
    ) {
        let n = &self.perlin.base;
        add_columns(n, buffer, volume, xz_scale, amplitude, points, |index_y| {
            let original_y = f64::from(volume.block_y(index_y)) * y_scale;
            let y = wrap(original_y) + n.offset_y;
            let floor_y = floor(y);
            let ry = y - f64::from(floor_y);
            let fudged_ry = (ry - self.compute_fudge_y(original_y, ry)) as f32;
            (floor_y, fudged_ry, smoothstep(ry as f32))
        });
    }
}

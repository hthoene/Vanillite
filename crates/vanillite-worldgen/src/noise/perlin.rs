//! `PerlinNoise` and its smeared variant `SmearedPerlinNoise` (one octave each).
//!
//! `get` and the batched `add_to_volume` round differently (`addToVolume` sums the XZ part of each
//! corner's dot product first), exactly as in Vanilla, so both are ported separately.

use vanillite_base::math::floor;
use vanillite_base::random::RandomSource;

use super::gradient::{GradientNoise, dot_xz, grad_dot, smoothstep, wrap};
use super::stack::Octave;
use crate::interval::Interval;
use crate::mth::lerp3_f;
use crate::volume::DensityVolume;

/// `SmearedPerlinNoise.SHIFT_UP_EPSILON`, a `float` widened to `double` where it is used.
const SHIFT_UP_EPSILON: f32 = 1.0e-7;

/// One octave of improved Perlin noise (`PerlinNoise`).
#[derive(Clone, Debug)]
pub(super) struct PerlinNoise {
    base: GradientNoise,
}

/// The XZ parts of the eight corner dot products and the corner gradients' Y components of one
/// lattice cell, cached per column while the cell's Y does not change (`addToVolume`).
#[derive(Clone, Copy, Default)]
struct CellColumn {
    xz: [f32; 8],
    gy: [f32; 8],
}

impl CellColumn {
    /// The corner values of the cell at `floor_y` above `column`, in `lerp3` order.
    #[inline(always)]
    fn new(noise: &GradientNoise, column: &Column, floor_y: i32) -> Self {
        let (x0, x1, floor_z) = (column.x0, column.x1, column.z.floor);
        let (rx, rz) = (column.x.relative, column.z.relative);
        let xy00 = noise.permute(x0.wrapping_add(floor_y));
        let xy01 = noise.permute(x0.wrapping_add(floor_y).wrapping_add(1));
        let xy10 = noise.permute(x1.wrapping_add(floor_y));
        let xy11 = noise.permute(x1.wrapping_add(floor_y).wrapping_add(1));
        let rx1 = rx - 1.0;
        let rz1 = rz - 1.0;
        let corners = [
            (xy00, 0, rx, rz),
            (xy10, 0, rx1, rz),
            (xy01, 0, rx, rz),
            (xy11, 0, rx1, rz),
            (xy00, 1, rx, rz1),
            (xy10, 1, rx1, rz1),
            (xy01, 1, rx, rz1),
            (xy11, 1, rx1, rz1),
        ];
        let mut column = Self::default();
        for (i, (xy, dz, cx, cz)) in corners.into_iter().enumerate() {
            let g = noise.permute_to_grad(xy.wrapping_add(floor_z).wrapping_add(dz));
            column.xz[i] = dot_xz(g, cx, cz);
            column.gy[i] = g[1];
        }
        column
    }

    /// Trilinear interpolation of the corner values at relative Y `ry`.
    #[inline(always)]
    fn lerp(&self, alpha_x: f32, alpha_y: f32, alpha_z: f32, ry: f32) -> f32 {
        let (xz, gy) = (&self.xz, &self.gy);
        lerp3_f(
            alpha_x,
            alpha_y,
            alpha_z,
            xz[0] + gy[0] * ry,
            xz[1] + gy[1] * ry,
            xz[2] + gy[2] * (ry - 1.0),
            xz[3] + gy[3] * (ry - 1.0),
            xz[4] + gy[4] * ry,
            xz[5] + gy[5] * ry,
            xz[6] + gy[6] * (ry - 1.0),
            xz[7] + gy[7] * (ry - 1.0),
        )
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
    fn has(self, index: usize) -> bool {
        self[index]
    }
}

/// Runs `body` for every (Z, X) column of `volume` with a needed point, with the column's
/// slice of `buffer` and the index of its first point (the outer loops of `addToVolume`).
#[inline(always)]
fn for_each_column(
    noise: &GradientNoise,
    buffer: &mut [f32],
    volume: &DensityVolume,
    xz_scale: f64,
    points: impl Points,
    mut body: impl FnMut(&mut [f32], &Column, usize),
) {
    let size_y = volume.size_y as usize;
    let mut columns = buffer[..volume.len()].chunks_exact_mut(size_y);
    let mut start = 0;
    for index_z in 0..volume.size_z {
        let z = Lattice::new(
            f64::from(volume.block_z(index_z)) * xz_scale,
            noise.offset_z,
        );
        for index_x in 0..volume.size_x {
            let out = columns.next().expect("buffer covers the volume");
            let first = start;
            start += size_y;
            if !points.any(first, size_y) {
                continue;
            }
            let x = Lattice::new(
                f64::from(volume.block_x(index_x)) * xz_scale,
                noise.offset_x,
            );
            let column = Column {
                x0: noise.permute(x.floor),
                x1: noise.permute(x.floor.wrapping_add(1)),
                x,
                z,
            };
            body(out, &column, first);
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
        let x0 = n.permute(x);
        let x1 = n.permute(x.wrapping_add(1));
        let xy00 = n.permute(x0.wrapping_add(y));
        let xy01 = n.permute(x0.wrapping_add(y).wrapping_add(1));
        let xy10 = n.permute(x1.wrapping_add(y));
        let xy11 = n.permute(x1.wrapping_add(y).wrapping_add(1));
        let z1 = |xy: i32| xy.wrapping_add(z).wrapping_add(1);
        let d000 = grad_dot(n.permute(xy00.wrapping_add(z)), rx, ry, rz);
        let d100 = grad_dot(n.permute(xy10.wrapping_add(z)), rx - 1.0, ry, rz);
        let d010 = grad_dot(n.permute(xy01.wrapping_add(z)), rx, ry - 1.0, rz);
        let d110 = grad_dot(n.permute(xy11.wrapping_add(z)), rx - 1.0, ry - 1.0, rz);
        let d001 = grad_dot(n.permute(z1(xy00)), rx, ry, rz - 1.0);
        let d101 = grad_dot(n.permute(z1(xy10)), rx - 1.0, ry, rz - 1.0);
        let d011 = grad_dot(n.permute(z1(xy01)), rx, ry - 1.0, rz - 1.0);
        let d111 = grad_dot(n.permute(z1(xy11)), rx - 1.0, ry - 1.0, rz - 1.0);
        let x_alpha = smoothstep(rx);
        let y_alpha = smoothstep(original_ry);
        let z_alpha = smoothstep(rz);
        lerp3_f(
            x_alpha, y_alpha, z_alpha, d000, d100, d010, d110, d001, d101, d011, d111,
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
        let mut rows = Rows::new(volume.size_y as usize);
        for_each_column(n, buffer, volume, xz_scale, points, |out, c, first| {
            let mut cell = CellColumn::default();
            let mut last_floor_y = i32::MIN;
            for (index_y, value) in (0..volume.size_y).zip(out.iter_mut()) {
                if !points.has(first + index_y as usize) {
                    continue;
                }
                let row = rows.get(index_y as usize, || {
                    let y = wrap(f64::from(volume.block_y(index_y)) * y_scale) + n.offset_y;
                    let floor_y = floor(y);
                    let ry = (y - f64::from(floor_y)) as f32;
                    (floor_y, ry, smoothstep(ry))
                });
                if last_floor_y != row.floor {
                    cell = CellColumn::new(n, c, row.floor);
                    last_floor_y = row.floor;
                }
                *value += amplitude * cell.lerp(c.x.alpha, row.alpha, c.z.alpha, row.relative);
            }
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
        let mut rows = Rows::new(volume.size_y as usize);
        for_each_column(n, buffer, volume, xz_scale, points, |out, c, first| {
            let mut cell = CellColumn::default();
            let mut last_floor_y = i32::MIN;
            for (index_y, value) in (0..volume.size_y).zip(out.iter_mut()) {
                if !points.has(first + index_y as usize) {
                    continue;
                }
                let row = rows.get(index_y as usize, || {
                    let original_y = f64::from(volume.block_y(index_y)) * y_scale;
                    let y = wrap(original_y) + n.offset_y;
                    let floor_y = floor(y);
                    let ry = y - f64::from(floor_y);
                    let fudged_ry = (ry - self.compute_fudge_y(original_y, ry)) as f32;
                    (floor_y, fudged_ry, smoothstep(ry as f32))
                });
                if last_floor_y != row.floor {
                    cell = CellColumn::new(n, c, row.floor);
                    last_floor_y = row.floor;
                }
                *value += amplitude * cell.lerp(c.x.alpha, row.alpha, c.z.alpha, row.relative);
            }
        });
    }
}

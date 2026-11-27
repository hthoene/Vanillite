//! `PerlinNoise` and its smeared variant `SmearedPerlinNoise` (one octave each).
//!
//! `get` and the batched `add_to_volume` round differently (`addToVolume` sums the XZ part of each
//! corner's dot product first), exactly as in Vanilla, so both are ported separately.

use vanillite_base::math::floor;
use vanillite_base::random::RandomSource;

#[cfg(target_arch = "x86_64")]
use super::gradient::GRADIENT;
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
/// consecutive (Z, X) columns go through the rows together, each lane doing exactly the
/// arithmetic of one column: eight with AVX2, else four ([`LANES`]). A column's cell is
/// computed when its Y cell changes at a needed point of any lane, so the values do not depend
/// on which points are skipped, nor on how many columns go together.
#[inline(always)]
fn add_columns<P: Points>(
    noise: &GradientNoise,
    buffer: &mut [f32],
    volume: &DensityVolume,
    xz_scale: f64,
    amplitude: f32,
    points: P,
    row_of: impl FnMut(i32) -> (i32, f32, f32),
) {
    #[cfg(target_arch = "x86_64")]
    if super::lanes::has_avx2() {
        // SAFETY: the CPU supports AVX2.
        unsafe {
            avx2::add_columns(noise, buffer, volume, xz_scale, amplitude, points, row_of);
        }
        return;
    }
    add_columns_4(noise, buffer, volume, xz_scale, amplitude, points, row_of);
}

/// The column of `volume` at `column` (`x + z * size_x`), if any of its points is needed.
#[inline(always)]
fn column_at<P: Points>(
    noise: &GradientNoise,
    volume: &DensityVolume,
    xz_scale: f64,
    points: P,
    column: usize,
) -> Option<(Column, P)> {
    let size_y = volume.size_y as usize;
    let size_x = volume.size_x.max(0) as usize;
    if !points.any(column * size_y, size_y) {
        return None;
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
    Some((
        Column {
            x0: xs.first(),
            x1: xs.second(),
            x,
            z,
        },
        points.column(column * size_y, size_y),
    ))
}

/// [`add_columns`] four columns at a time (SSE2).
#[inline(always)]
fn add_columns_4<P: Points>(
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
            if l >= count {
                continue;
            }
            if let Some((column, column_points)) =
                column_at(noise, volume, xz_scale, points, start + l)
            {
                *lane = Some(column);
                *lane_points = column_points;
            }
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

/// [`add_columns`] eight columns at a time, with the cell corners of all lanes computed in
/// vectors too (AVX2: gathers from the permutation pairs, gradient components by permutes).
#[cfg(target_arch = "x86_64")]
mod avx2 {
    use std::arch::x86_64::{
        __m256, __m256i, _mm256_add_epi32, _mm256_add_ps, _mm256_and_si256, _mm256_blendv_ps,
        _mm256_castsi256_ps, _mm256_i32gather_epi32, _mm256_loadu_ps, _mm256_loadu_si256,
        _mm256_mul_ps, _mm256_permutevar8x32_ps, _mm256_set1_epi32, _mm256_set1_ps,
        _mm256_setzero_ps, _mm256_slli_epi32, _mm256_srli_epi32, _mm256_storeu_ps, _mm256_sub_ps,
    };

    use super::{Column, DensityVolume, GRADIENT, GradientNoise, Points, Rows, column_at};

    const LANES: usize = 8;

    /// One component of the gradients of 16 indices, split in two halves of eight.
    #[derive(Clone, Copy)]
    struct Component(__m256, __m256);

    impl Component {
        #[inline]
        #[target_feature(enable = "avx2")]
        fn of(component: usize) -> Self {
            let half = |from: usize| -> __m256 {
                let values: [f32; 8] = std::array::from_fn(|i| GRADIENT[from + i][component]);
                // SAFETY: `values` holds eight `f32`s.
                unsafe { _mm256_loadu_ps(values.as_ptr()) }
            };
            Self(half(0), half(8))
        }

        /// The component of the gradients at `index` (`0..16`) per lane.
        #[inline]
        #[target_feature(enable = "avx2")]
        fn at(self, index: __m256i) -> __m256 {
            let low = _mm256_permutevar8x32_ps(self.0, index);
            let high = _mm256_permutevar8x32_ps(self.1, index);
            // Bit 3 of the index, moved to the sign bit, picks the half.
            _mm256_blendv_ps(
                low,
                high,
                _mm256_castsi256_ps(_mm256_slli_epi32::<28>(index)),
            )
        }
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    fn lerp(alpha: __m256, p0: __m256, p1: __m256) -> __m256 {
        _mm256_add_ps(p0, _mm256_mul_ps(alpha, _mm256_sub_ps(p1, p0)))
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    fn load_i32(values: &[i32; LANES]) -> __m256i {
        // SAFETY: `values` holds eight `i32`s.
        unsafe { _mm256_loadu_si256(values.as_ptr().cast()) }
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    fn load_f32(values: &[f32; LANES]) -> __m256 {
        // SAFETY: `values` holds eight `f32`s.
        unsafe { _mm256_loadu_ps(values.as_ptr()) }
    }

    /// See [`super::add_columns`].
    #[target_feature(enable = "avx2")]
    pub(super) fn add_columns<P: Points>(
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
        let amplitude = _mm256_set1_ps(amplitude);
        let (grad_x, grad_y, grad_z) = (Component::of(0), Component::of(1), Component::of(2));
        let pairs = noise.pairs.as_ptr().cast::<i32>();
        let low_byte = _mm256_set1_epi32(0xFF);
        let low_nibble = _mm256_set1_epi32(15);
        // `pairs[(index & 255)]` per lane.
        let pair = |index: __m256i| -> __m256i {
            // SAFETY: every index is masked to `0..256`, inside `pairs`.
            unsafe { _mm256_i32gather_epi32::<4>(pairs, _mm256_and_si256(index, low_byte)) }
        };
        for (group_index, group) in buffer[..columns * size_y]
            .chunks_mut(LANES * size_y)
            .enumerate()
        {
            let start = group_index * LANES;
            let count = group.len() / size_y;
            let mut lanes: [Option<Column>; LANES] = [None; LANES];
            let mut lane_points = [points; LANES];
            for (l, (lane, lane_points)) in lanes.iter_mut().zip(&mut lane_points).enumerate() {
                if l >= count {
                    continue;
                }
                if let Some((column, column_points)) =
                    column_at(noise, volume, xz_scale, points, start + l)
                {
                    *lane = Some(column);
                    *lane_points = column_points;
                }
            }
            if lanes.iter().all(Option::is_none) {
                continue;
            }
            // Lanes without a column compute on zeros; their values are not stored.
            let (mut x0, mut x1, mut floor_z) = ([0; LANES], [0; LANES], [0; LANES]);
            let (mut rx, mut rz, mut alpha_x, mut alpha_z) =
                ([0.0; LANES], [0.0; LANES], [0.0; LANES], [0.0; LANES]);
            for (l, lane) in lanes.iter().enumerate() {
                if let Some(column) = lane {
                    x0[l] = column.x0;
                    x1[l] = column.x1;
                    floor_z[l] = column.z.floor;
                    rx[l] = column.x.relative;
                    rz[l] = column.z.relative;
                    alpha_x[l] = column.x.alpha;
                    alpha_z[l] = column.z.alpha;
                }
            }
            let (x0, x1, floor_z) = (load_i32(&x0), load_i32(&x1), load_i32(&floor_z));
            let (rx, rz) = (load_f32(&rx), load_f32(&rz));
            let one = _mm256_set1_ps(1.0);
            let (rx1, rz1) = (_mm256_sub_ps(rx, one), _mm256_sub_ps(rz, one));
            let (alpha_x, alpha_z) = (load_f32(&alpha_x), load_f32(&alpha_z));
            let (mut xz, mut gy) = ([_mm256_setzero_ps(); 8], [_mm256_setzero_ps(); 8]);
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
                    // `Cells::set` of every lane.
                    let floor_y = _mm256_set1_epi32(row.floor);
                    let xy0 = pair(_mm256_add_epi32(x0, floor_y));
                    let xy1 = pair(_mm256_add_epi32(x1, floor_y));
                    let first = |p: __m256i| _mm256_and_si256(p, low_byte);
                    let second = |p: __m256i| _mm256_and_si256(_mm256_srli_epi32::<8>(p), low_byte);
                    let corners = [
                        (first(xy0), rx),
                        (first(xy1), rx1),
                        (second(xy0), rx),
                        (second(xy1), rx1),
                    ];
                    for (i, (xy, cx)) in corners.into_iter().enumerate() {
                        let xyz = pair(_mm256_add_epi32(xy, floor_z));
                        let g = _mm256_and_si256(_mm256_srli_epi32::<16>(xyz), low_nibble);
                        let g1 = _mm256_and_si256(_mm256_srli_epi32::<24>(xyz), low_nibble);
                        // `dot_xz`: `g[0] * x + g[2] * z`.
                        xz[i] = _mm256_add_ps(
                            _mm256_mul_ps(grad_x.at(g), cx),
                            _mm256_mul_ps(grad_z.at(g), rz),
                        );
                        gy[i] = grad_y.at(g);
                        xz[i + 4] = _mm256_add_ps(
                            _mm256_mul_ps(grad_x.at(g1), cx),
                            _mm256_mul_ps(grad_z.at(g1), rz1),
                        );
                        gy[i + 4] = grad_y.at(g1);
                    }
                    last_floor_y = row.floor;
                }
                let ry = _mm256_set1_ps(row.relative);
                let ry1 = _mm256_set1_ps(row.relative - 1.0);
                let corner = |i: usize, ry: __m256| _mm256_add_ps(xz[i], _mm256_mul_ps(gy[i], ry));
                let alpha_y = _mm256_set1_ps(row.alpha);
                let lerp2 = |x00: __m256, x10: __m256, x01: __m256, x11: __m256| {
                    lerp(alpha_y, lerp(alpha_x, x00, x10), lerp(alpha_x, x01, x11))
                };
                let value = lerp(
                    alpha_z,
                    lerp2(corner(0, ry), corner(1, ry), corner(2, ry1), corner(3, ry1)),
                    lerp2(corner(4, ry), corner(5, ry), corner(6, ry1), corner(7, ry1)),
                );
                let mut values = [0.0f32; LANES];
                // SAFETY: `values` holds eight `f32`s.
                unsafe { _mm256_storeu_ps(values.as_mut_ptr(), _mm256_mul_ps(amplitude, value)) };
                for (l, (need, value)) in needed.into_iter().zip(values).enumerate() {
                    if need {
                        group[l * size_y + index_y] += value;
                    }
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

#[cfg(all(test, target_arch = "x86_64"))]
mod tests {
    use vanillite_base::random::LegacyRandom;

    use super::*;

    /// The eight-lane AVX2 path gives the bits of the four-lane path, with and without masks.
    #[test]
    fn avx2_columns_match_four_lanes() {
        if !super::super::lanes::has_avx2() {
            return;
        }
        let mut random = LegacyRandom::new(12345);
        let noise = PerlinNoise::new(&mut random);
        let smeared = SmearedPerlinNoise::new(&mut random, 0.37);
        let volumes = [
            DensityVolume::with_step(5, 49, 5, -16, -64, 32, 4, 8, 4),
            DensityVolume::new(16, 23, 16, 1003, -17, -2050),
            DensityVolume::new(3, 70, 7, -33_554_430, 5, 16_777_300),
        ];
        for (v, volume) in volumes.iter().enumerate() {
            let mask: Vec<bool> = (0..volume.len()).map(|i| (i * 7 + v) % 5 < 2).collect();
            for (scale, y_scale) in [(1.0, 1.0), (0.25, 0.125), (684.412 * 0.01, 3.7)] {
                let run = |avx2: bool, masked: bool, smeared_noise: bool| -> Vec<u32> {
                    let mut buffer = vec![0.5f32; volume.len()];
                    let row = |index_y: i32| {
                        let y = wrap(f64::from(volume.block_y(index_y)) * y_scale)
                            + noise.base.offset_y;
                        let floor_y = floor(y);
                        let ry = (y - f64::from(floor_y)) as f32;
                        (floor_y, ry, smoothstep(ry))
                    };
                    let n = if smeared_noise {
                        &smeared.perlin.base
                    } else {
                        &noise.base
                    };
                    match (avx2, masked) {
                        // SAFETY: AVX2 is available (checked above).
                        (true, true) => unsafe {
                            avx2::add_columns(n, &mut buffer, volume, scale, 0.75, &mask[..], row);
                        },
                        // SAFETY: as above.
                        (true, false) => unsafe {
                            avx2::add_columns(n, &mut buffer, volume, scale, 0.75, All, row);
                        },
                        (false, true) => {
                            add_columns_4(n, &mut buffer, volume, scale, 0.75, &mask[..], row);
                        }
                        (false, false) => {
                            add_columns_4(n, &mut buffer, volume, scale, 0.75, All, row);
                        }
                    }
                    buffer.iter().map(|value| value.to_bits()).collect()
                };
                for masked in [false, true] {
                    for smeared_noise in [false, true] {
                        assert_eq!(
                            run(true, masked, smeared_noise),
                            run(false, masked, smeared_noise),
                            "volume {v}, scale {scale}, masked {masked}"
                        );
                    }
                }
            }
        }
    }
}

//! `SimplexNoise`: 2D and 3D simplex noise in `double`, rounded to `float` at the end.

use vanillite_base::math::floor;
use vanillite_base::random::RandomSource;

use super::gradient::{GRADIENT, GradientNoise};
use super::stack::Octave;
use crate::interval::Interval;
use crate::volume::DensityVolume;

/// One simplex noise (`SimplexNoise`).
#[derive(Clone, Debug)]
pub(super) struct SimplexNoise {
    base: GradientNoise,
}

/// `SimplexNoise.F2 = 0.5 * (SQRT_3 - 1.0)`.
#[inline(always)]
fn f2() -> f64 {
    0.5 * (3.0_f64.sqrt() - 1.0)
}

/// `SimplexNoise.G2 = (3.0 - SQRT_3) / 6.0`.
#[inline(always)]
fn g2() -> f64 {
    (3.0 - 3.0_f64.sqrt()) / 6.0
}

/// `SimplexNoise.G3` (`0.16666666666666666`).
const G3: f64 = 1.0 / 6.0;
/// `SimplexNoise.F3` (`0.3333333333333333`).
const F3: f64 = 1.0 / 3.0;

impl SimplexNoise {
    /// `new SimplexNoise(random, discardNoiseOffset)`.
    pub fn new(random: &mut impl RandomSource, discard_noise_offset: bool) -> Self {
        let scale = if discard_noise_offset { 0.0 } else { 256.0 };
        Self {
            base: GradientNoise::new(random, scale),
        }
    }

    /// `getCornerNoise3D(index, x, y, z, base)`.
    #[inline(always)]
    fn corner_noise(index: i32, x: f64, y: f64, z: f64, base: f64) -> f64 {
        let mut t0 = base - x * x - y * y - z * z;
        if t0 < 0.0 {
            0.0
        } else {
            t0 *= t0;
            let g = &GRADIENT[index as usize];
            t0 * t0 * (f64::from(g[0]) * x + f64::from(g[1]) * y + f64::from(g[2]) * z)
        }
    }
}

impl Octave for SimplexNoise {
    fn range(&self) -> Interval {
        Interval::of_symmetric(2.0)
    }

    fn get2(&self, x: f64, y: f64) -> f32 {
        let n = &self.base;
        let (f2, g2) = (f2(), g2());
        let xin = x + n.offset_x;
        let yin = y + n.offset_y;
        let s = (xin + yin) * f2;
        let i = floor(xin + s);
        let j = floor(yin + s);
        let t = f64::from(i.wrapping_add(j)) * g2;
        let x0 = xin - (f64::from(i) - t);
        let y0 = yin - (f64::from(j) - t);
        let (i1, j1) = if x0 > y0 { (1, 0) } else { (0, 1) };
        let x1 = x0 - f64::from(i1) + g2;
        let y1 = y0 - f64::from(j1) + g2;
        let x2 = x0 - 1.0 + 2.0 * g2;
        let y2 = y0 - 1.0 + 2.0 * g2;
        let ii = i & 0xFF;
        let jj = j & 0xFF;
        let gi0 = n.permute(ii + n.permute(jj)) % 12;
        let gi1 = n.permute(ii + i1 + n.permute(jj + j1)) % 12;
        let gi2 = n.permute(ii + 1 + n.permute(jj + 1)) % 12;
        let n0 = Self::corner_noise(gi0, x0, y0, 0.0, 0.5);
        let n1 = Self::corner_noise(gi1, x1, y1, 0.0, 0.5);
        let n2 = Self::corner_noise(gi2, x2, y2, 0.0, 0.5);
        (70.0 * (n0 + n1 + n2)) as f32
    }

    fn get(&self, x: f64, y: f64, z: f64) -> f32 {
        let n = &self.base;
        let xin = x + n.offset_x;
        let yin = y + n.offset_y;
        let zin = z + n.offset_z;
        let s = (xin + yin + zin) * F3;
        let i = floor(xin + s);
        let j = floor(yin + s);
        let k = floor(zin + s);
        let t = f64::from(i.wrapping_add(j).wrapping_add(k)) * G3;
        let x0 = xin - (f64::from(i) - t);
        let y0 = yin - (f64::from(j) - t);
        let z0 = zin - (f64::from(k) - t);
        let ((i1, j1, k1), (i2, j2, k2)) = if x0 >= y0 {
            if y0 >= z0 {
                ((1, 0, 0), (1, 1, 0))
            } else if x0 >= z0 {
                ((1, 0, 0), (1, 0, 1))
            } else {
                ((0, 0, 1), (1, 0, 1))
            }
        } else if y0 < z0 {
            ((0, 0, 1), (0, 1, 1))
        } else if x0 < z0 {
            ((0, 1, 0), (0, 1, 1))
        } else {
            ((0, 1, 0), (1, 1, 0))
        };
        let x1 = x0 - f64::from(i1) + G3;
        let y1 = y0 - f64::from(j1) + G3;
        let z1 = z0 - f64::from(k1) + G3;
        let x2 = x0 - f64::from(i2) + F3;
        let y2 = y0 - f64::from(j2) + F3;
        let z2 = z0 - f64::from(k2) + F3;
        let x3 = x0 - 1.0 + 0.5;
        let y3 = y0 - 1.0 + 0.5;
        let z3 = z0 - 1.0 + 0.5;
        let ii = i & 0xFF;
        let jj = j & 0xFF;
        let kk = k & 0xFF;
        let p =
            |a: i32, b: i32, c: i32| n.permute(ii + a + n.permute(jj + b + n.permute(kk + c))) % 12;
        let gi0 = p(0, 0, 0);
        let gi1 = p(i1, j1, k1);
        let gi2 = p(i2, j2, k2);
        let gi3 = p(1, 1, 1);
        let n0 = Self::corner_noise(gi0, x0, y0, z0, 0.6);
        let n1 = Self::corner_noise(gi1, x1, y1, z1, 0.6);
        let n2 = Self::corner_noise(gi2, x2, y2, z2, 0.6);
        let n3 = Self::corner_noise(gi3, x3, y3, z3, 0.6);
        (32.0 * (n0 + n1 + n2 + n3)) as f32
    }

    /// `Noise.addToVolume`: the default point-by-point loop.
    fn add_to_volume(
        &self,
        buffer: &mut [f32],
        volume: &DensityVolume,
        xz_scale: f64,
        y_scale: f64,
        amplitude: f32,
    ) {
        let size_y = volume.size_y as usize;
        let mut columns = buffer[..volume.len()].chunks_exact_mut(size_y);
        for index_z in 0..volume.size_z {
            let z = f64::from(volume.block_z(index_z)) * xz_scale;
            for index_x in 0..volume.size_x {
                let x = f64::from(volume.block_x(index_x)) * xz_scale;
                let out = columns.next().expect("buffer covers the volume");
                for (index_y, value) in (0..volume.size_y).zip(out.iter_mut()) {
                    let y = f64::from(volume.block_y(index_y)) * y_scale;
                    *value += amplitude * self.get(x, y, z);
                }
            }
        }
    }

    fn add_to_volume_masked(
        &self,
        buffer: &mut [f32],
        volume: &DensityVolume,
        xz_scale: f64,
        y_scale: f64,
        amplitude: f32,
        _mask: &[bool],
    ) {
        self.add_to_volume(buffer, volume, xz_scale, y_scale, amplitude);
    }
}

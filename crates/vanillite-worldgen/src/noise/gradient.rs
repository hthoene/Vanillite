//! `GradientNoise`: the permutation table, coordinate offsets and gradient vectors shared by
//! Perlin and simplex noise.

use vanillite_base::random::RandomSource;

/// `GradientNoise.GRADIENT` as `float`s (Java converts the `int` components with `i2f`/`i2d`,
/// which is exact).
pub(super) const GRADIENT: [[f32; 3]; 16] = [
    [1.0, 1.0, 0.0],
    [-1.0, 1.0, 0.0],
    [1.0, -1.0, 0.0],
    [-1.0, -1.0, 0.0],
    [1.0, 0.0, 1.0],
    [-1.0, 0.0, 1.0],
    [1.0, 0.0, -1.0],
    [-1.0, 0.0, -1.0],
    [0.0, 1.0, 1.0],
    [0.0, -1.0, 1.0],
    [0.0, 1.0, -1.0],
    [0.0, -1.0, -1.0],
    [1.0, 1.0, 0.0],
    [0.0, -1.0, 1.0],
    [-1.0, 1.0, 0.0],
    [0.0, -1.0, -1.0],
];

/// `ROUND_OFF`: coordinates are wrapped into one period of this size.
const ROUND_OFF: f64 = 33_554_432.0;
/// `HALF_ROUND_OFF = Math.nextDown(1.6777216E7)`.
const HALF_ROUND_OFF: f64 = 16_777_216.0_f64.next_down();

/// Permutation table and offsets of one gradient noise octave (`GradientNoise`).
#[derive(Clone, Debug)]
pub(super) struct GradientNoise {
    pub perms: [u8; 256],
    pub offset_x: f64,
    pub offset_y: f64,
    pub offset_z: f64,
}

impl GradientNoise {
    /// `GradientNoise(random, noiseOffsetScale)`.
    pub fn new(random: &mut impl RandomSource, noise_offset_scale: f64) -> Self {
        let offset_x = random.next_double() * noise_offset_scale;
        let offset_y = random.next_double() * noise_offset_scale;
        let offset_z = random.next_double() * noise_offset_scale;
        let mut perms = [0_u8; 256];
        for (i, p) in perms.iter_mut().enumerate() {
            *p = i as u8;
        }
        for i in 0..256 {
            let offset = random.next_int_bounded(256 - i as i32) as usize;
            perms.swap(i, offset + i);
        }
        Self {
            perms,
            offset_x,
            offset_y,
            offset_z,
        }
    }

    /// `permute(x)`.
    #[inline(always)]
    pub fn permute(&self, x: i32) -> i32 {
        i32::from(self.perms[(x & 0xFF) as usize])
    }

    /// `permuteToGrad(x)`.
    #[inline(always)]
    pub fn permute_to_grad(&self, x: i32) -> &'static [f32; 3] {
        &GRADIENT[(self.permute(x) & 15) as usize]
    }
}

/// `GradientNoise.gradDot(hash, x, y, z)`: `Gradient.dot(float, float, float)`.
#[inline(always)]
pub(super) fn grad_dot(hash: i32, x: f32, y: f32, z: f32) -> f32 {
    let g = &GRADIENT[(hash & 15) as usize];
    g[0] * x + g[1] * y + g[2] * z
}

/// `Gradient.dotXz(x, z)`.
#[inline(always)]
pub(super) fn dot_xz(g: &[f32; 3], x: f32, z: f32) -> f32 {
    g[0] * x + g[2] * z
}

/// `GradientNoise.wrap(x)`: keeps coordinates in a range where `double` has enough precision.
#[inline(always)]
pub(super) fn wrap(x: f64) -> f64 {
    if (-HALF_ROUND_OFF..HALF_ROUND_OFF).contains(&x) {
        x
    } else {
        x - (x / ROUND_OFF + 0.5).floor() * ROUND_OFF
    }
}

/// `Mth.smoothstep(float)`.
#[inline(always)]
pub(super) fn smoothstep(x: f32) -> f32 {
    x * x * x * (x * (x * 6.0 - 15.0) + 10.0)
}

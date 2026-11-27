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
    /// Per index `i`: `perms[i]`, `perms[i + 1]` and the gradient indices of both (`& 15`), one
    /// byte each from the lowest. Perlin noise always reads the permutations of `i` and
    /// `i + 1` together, so one entry answers both (see [`Pair`]).
    pub pairs: [u32; 256],
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
        let mut pairs = [0_u32; 256];
        for (i, pair) in pairs.iter_mut().enumerate() {
            let (a, b) = (u32::from(perms[i]), u32::from(perms[(i + 1) & 0xFF]));
            *pair = a | (b << 8) | ((a & 15) << 16) | ((b & 15) << 24);
        }
        Self {
            perms,
            pairs,
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

    /// `permute(x)` and `permute(x + 1)` with their gradients.
    #[inline(always)]
    pub fn pair(&self, x: i32) -> Pair {
        Pair(self.pairs[(x & 0xFF) as usize])
    }
}

/// The permutations of an index and of the next one, with their gradients
/// ([`GradientNoise::pair`]).
#[derive(Clone, Copy)]
pub(super) struct Pair(u32);

impl Pair {
    /// `permute(x)`.
    #[inline(always)]
    pub fn first(self) -> i32 {
        (self.0 & 0xFF) as i32
    }

    /// `permute(x + 1)`.
    #[inline(always)]
    pub fn second(self) -> i32 {
        ((self.0 >> 8) & 0xFF) as i32
    }

    /// `permuteToGrad(x)`.
    #[inline(always)]
    pub fn first_grad(self) -> &'static [f32; 3] {
        &GRADIENT[((self.0 >> 16) & 15) as usize]
    }

    /// `permuteToGrad(x + 1)`.
    #[inline(always)]
    pub fn second_grad(self) -> &'static [f32; 3] {
        &GRADIENT[((self.0 >> 24) & 15) as usize]
    }
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

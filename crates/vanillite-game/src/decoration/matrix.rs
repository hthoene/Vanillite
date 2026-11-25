//! `MatrixUtil.svdDecompose`: a display transformation given as a 4×4 matrix splits into
//! translation, left rotation, scale and right rotation like Vanilla's (Jacobi eigenvalues of
//! `AᵀA` in five sweeps, then a QR decomposition by Givens rotations). Matrices are JOML's
//! column-major `m[column][row]`.

use super::display::Transformation;

type M3 = [[f32; 3]; 3];
/// `x, y, z, w`.
type Q = [f32; 4];

/// `this * right`.
fn mul(a: &M3, b: &M3) -> M3 {
    let mut out = [[0.0; 3]; 3];
    for (c, col) in out.iter_mut().enumerate() {
        for (r, v) in col.iter_mut().enumerate() {
            *v = a[0][r] * b[c][0] + (a[1][r] * b[c][1] + a[2][r] * b[c][2]);
        }
    }
    out
}

fn transpose(a: &M3) -> M3 {
    let mut out = [[0.0; 3]; 3];
    for (c, col) in out.iter_mut().enumerate() {
        for (r, v) in col.iter_mut().enumerate() {
            *v = a[r][c];
        }
    }
    out
}

/// `Quaternionf.mul`.
fn qmul(a: Q, b: Q) -> Q {
    let [x, y, z, w] = a;
    [
        w * b[0] + (x * b[3] + (y * b[2] + -z * b[1])),
        w * b[1] + (-x * b[2] + (y * b[3] + z * b[0])),
        w * b[2] + (x * b[1] + (-y * b[0] + z * b[3])),
        w * b[3] + (-x * b[0] + (-y * b[1] + -z * b[2])),
    ]
}

fn qnormalize(q: Q) -> Q {
    let inv = 1.0 / (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
    [q[0] * inv, q[1] * inv, q[2] * inv, q[3] * inv]
}

/// `Matrix3f.rotate(quaternion)`: `this * R(q)`.
fn rotate(a: &M3, q: Q) -> M3 {
    let [x, y, z, w] = q;
    let (w2, x2, y2, z2) = (w * w, x * x, y * y, z * z);
    let (zw, xy, xz, yw, yz, xw) = (z * w, x * y, x * z, y * w, y * z, x * w);
    let (dzw, dxy, dxz, dyw, dyz, dxw) = (zw + zw, xy + xy, xz + xz, yw + yw, yz + yz, xw + xw);
    let r = [
        [w2 + x2 - z2 - y2, dxy + dzw, dxz - dyw],
        [dxy - dzw, y2 - z2 + w2 - x2, dyz + dxw],
        [dyw + dxz, dyz - dxw, z2 - y2 - x2 + w2],
    ];
    mul(a, &r)
}

/// `GivensParameters`: the sine and cosine of half the angle.
#[derive(Clone, Copy)]
struct Givens {
    sin_half: f32,
    cos_half: f32,
}

impl Givens {
    fn from_unnormalized(sin_half: f32, cos_half: f32) -> Givens {
        let w = 1.0 / (sin_half * sin_half + cos_half * cos_half).sqrt();
        Givens {
            sin_half: w * sin_half,
            cos_half: w * cos_half,
        }
    }

    fn pi_4() -> Givens {
        let half = std::f32::consts::FRAC_PI_4 / 2.0;
        let sin = half.sin();
        Givens {
            sin_half: sin,
            cos_half: (1.0 - sin * sin).sqrt(),
        }
    }

    fn inverse(self) -> Givens {
        Givens {
            sin_half: -self.sin_half,
            cos_half: self.cos_half,
        }
    }

    fn cos(self) -> f32 {
        self.cos_half * self.cos_half - self.sin_half * self.sin_half
    }

    fn sin(self) -> f32 {
        2.0 * self.sin_half * self.cos_half
    }

    fn q_x(self) -> Q {
        [self.sin_half, 0.0, 0.0, self.cos_half]
    }

    fn q_y(self) -> Q {
        [0.0, self.sin_half, 0.0, self.cos_half]
    }

    fn q_z(self) -> Q {
        [0.0, 0.0, self.sin_half, self.cos_half]
    }

    fn m_x(self) -> M3 {
        let (c, s) = (self.cos(), self.sin());
        [[1.0, 0.0, 0.0], [0.0, c, s], [0.0, -s, c]]
    }

    fn m_y(self) -> M3 {
        let (c, s) = (self.cos(), self.sin());
        [[c, 0.0, -s], [0.0, 1.0, 0.0], [s, 0.0, c]]
    }

    fn m_z(self) -> M3 {
        let (c, s) = (self.cos(), self.sin());
        [[c, s, 0.0], [-s, c, 0.0], [0.0, 0.0, 1.0]]
    }
}

fn approx_givens(a11: f32, a12: f32, a22: f32) -> Givens {
    let g = 3.0 + 2.0 * 2.0_f32.sqrt();
    let ch = 2.0 * (a11 - a22);
    if g * a12 * a12 < ch * ch {
        Givens::from_unnormalized(a12, ch)
    } else {
        Givens::pi_4()
    }
}

fn qr_givens(a1: f32, a2: f32) -> Givens {
    #[allow(clippy::cast_possible_truncation)]
    let p = f64::from(a1).hypot(f64::from(a2)) as f32;
    let mut sh = if p > 1.0e-6 { a2 } else { 0.0 };
    let mut ch = a1.abs() + p.max(1.0e-6);
    if a1 < 0.0 {
        std::mem::swap(&mut sh, &mut ch);
    }
    Givens::from_unnormalized(sh, ch)
}

/// `similarityTransform`: `qᵀ a q`.
fn similarity(a: &M3, q: &M3) -> M3 {
    mul(&transpose(q), &mul(a, q))
}

fn step_jacobi(m: &mut M3, out: &mut Q) {
    if m[0][1] * m[0][1] + m[1][0] * m[1][0] > 1.0e-6 {
        let p = approx_givens(m[0][0], 0.5 * (m[0][1] + m[1][0]), m[1][1]);
        *out = qmul(*out, p.q_z());
        *m = similarity(m, &p.m_z());
    }
    if m[0][2] * m[0][2] + m[2][0] * m[2][0] > 1.0e-6 {
        let p = approx_givens(m[0][0], 0.5 * (m[0][2] + m[2][0]), m[2][2]).inverse();
        *out = qmul(*out, p.q_y());
        *m = similarity(m, &p.m_y());
    }
    if m[1][2] * m[1][2] + m[2][1] * m[2][1] > 1.0e-6 {
        let p = approx_givens(m[1][1], 0.5 * (m[1][2] + m[2][1]), m[2][2]);
        *out = qmul(*out, p.q_x());
        *m = similarity(m, &p.m_x());
    }
}

/// A row-major 4×4 matrix (`ExtraCodecs.MATRIX4F`) as a transformation.
pub fn decompose(rows: [f32; 16]) -> Transformation {
    let at = |r: usize, c: usize| rows[r * 4 + c];
    let factor = 1.0 / at(3, 3);
    let mut input: M3 = [[0.0; 3]; 3];
    for (c, col) in input.iter_mut().enumerate() {
        for (r, v) in col.iter_mut().enumerate() {
            *v = at(r, c) * factor;
        }
    }
    // `eigenvalueJacobi(AᵀA, 5, v)`.
    let mut scratch = mul(&transpose(&input), &input);
    let mut v: Q = [0.0, 0.0, 0.0, 1.0];
    for _ in 0..5 {
        step_jacobi(&mut scratch, &mut v);
    }
    v = qnormalize(v);
    let zero0 = f64::from(scratch[0][0]) < 1.0e-6;
    let zero1 = f64::from(scratch[1][1]) < 1.0e-6;
    let u012s = rotate(&input, v);
    let mut u: Q = [0.0, 0.0, 0.0, 1.0];
    let p = if zero0 {
        qr_givens(u012s[1][1], -u012s[1][0])
    } else {
        qr_givens(u012s[0][0], u012s[0][1])
    };
    u = qmul(u, p.q_z());
    let u12s = mul(&transpose(&p.m_z()), &u012s);
    let p = if zero0 {
        qr_givens(u12s[2][2], -u12s[2][0])
    } else {
        qr_givens(u12s[0][0], u12s[0][2])
    }
    .inverse();
    u = qmul(u, p.q_y());
    let u2s = mul(&transpose(&p.m_y()), &u12s);
    let p = if zero1 {
        qr_givens(u2s[2][2], -u2s[2][1])
    } else {
        qr_givens(u2s[1][1], u2s[1][2])
    };
    u = qmul(u, p.q_x());
    let sm = mul(&transpose(&p.m_x()), &u2s);
    Transformation {
        translation: [at(0, 3) * factor, at(1, 3) * factor, at(2, 3) * factor],
        left_rotation: u,
        scale: [sm[0][0], sm[1][1], sm[2][2]],
        right_rotation: [-v[0], -v[1], -v[2], v[3]],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scaled_translation_splits_into_its_parts() {
        let t = decompose([
            2.0, 0.0, 0.0, 1.0, //
            0.0, 3.0, 0.0, 2.0, //
            0.0, 0.0, 4.0, 3.0, //
            0.0, 0.0, 0.0, 1.0,
        ]);
        assert_eq!(t.translation, [1.0, 2.0, 3.0]);
        let mut s = t.scale;
        s.iter_mut().for_each(|v| *v = v.abs());
        s.sort_by(f32::total_cmp);
        for (a, b) in s.iter().zip([2.0, 3.0, 4.0]) {
            assert!((a - b).abs() < 1.0e-4, "{s:?}");
        }
    }
}

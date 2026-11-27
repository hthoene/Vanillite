//! `BiomeManager`: the block-level biome lookup that picks one of the eight surrounding noise
//! biomes (quarts) with seeded jitter ("biome zoom"), plus `obfuscateSeed` and
//! `LinearCongruentialGenerator`.

use vanillite_chunk::BiomeId;

use super::source::BiomeResolver;

/// The zoomed biome lookup of a world (`BiomeManager` without its source: the noise biome lookup
/// is passed per call, as Vanilla's `withDifferentSource` swaps it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BiomeManager {
    biome_zoom_seed: i64,
}

impl BiomeManager {
    /// A manager with the obfuscated seed (`BiomeManager(source, obfuscateSeed(seed))`).
    pub fn new(biome_zoom_seed: i64) -> Self {
        Self { biome_zoom_seed }
    }

    /// A manager for a world seed: `new(obfuscate_seed(seed))`.
    pub fn for_world_seed(seed: i64) -> Self {
        Self::new(obfuscate_seed(seed))
    }

    /// The zoom seed (`biomeZoomSeed`).
    pub fn biome_zoom_seed(&self) -> i64 {
        self.biome_zoom_seed
    }

    /// `getBiome(x, y, z)`: the noise biome of the quart [`Self::zoom`] picks.
    #[inline]
    pub fn get_biome(&self, x: i32, y: i32, z: i32, source: &mut impl BiomeResolver) -> BiomeId {
        let (qx, qy, qz) = self.zoom(x, y, z);
        source.noise_biome(qx, qy, qz)
    }

    /// The quart coordinates whose noise biome `getBiome(x, y, z)` returns.
    pub fn zoom(&self, x: i32, y: i32, z: i32) -> (i32, i32, i32) {
        let abs_x = x.wrapping_sub(2);
        let abs_y = y.wrapping_sub(2);
        let abs_z = z.wrapping_sub(2);
        let parent = [abs_x >> 2, abs_y >> 2, abs_z >> 2];
        let fract = [
            f64::from(abs_x & 3) / 4.0,
            f64::from(abs_y & 3) / 4.0,
            f64::from(abs_z & 3) / 4.0,
        ];
        let seed = self.biome_zoom_seed;
        // The corners in Vanilla's order (x, then y, then z bit). A corner whose fiddled
        // distance cannot get below the best so far is not hashed: it could never win.
        let corner = |axis: usize, odd: bool| {
            if odd {
                (parent[axis].wrapping_add(1), fract[axis] - 1.0)
            } else {
                (parent[axis], fract[axis])
            }
        };
        let mut min_i = 0;
        let mut min_fiddled_distance = f64::INFINITY;
        for i in 0..8 {
            let (corner_x, distance_x) = corner(0, i & 4 != 0);
            let (corner_y, distance_y) = corner(1, i & 2 != 0);
            let (corner_z, distance_z) = corner(2, i & 1 != 0);
            let bound = min_square(distance_x) + min_square(distance_y) + min_square(distance_z);
            if bound > min_fiddled_distance {
                continue;
            }
            let next = fiddled_distance(
                seed, corner_x, corner_y, corner_z, distance_x, distance_y, distance_z,
            );
            if min_fiddled_distance > next {
                min_i = i;
                min_fiddled_distance = next;
            }
        }
        (
            corner(0, min_i & 4 != 0).0,
            corner(1, min_i & 2 != 0).0,
            corner(2, min_i & 1 != 0).0,
        )
    }
}

/// A lower bound of `square(distance + fiddle)` over every value of [`fiddle`] (in
/// [-0.45, 0.45]), slightly lowered so rounding never puts it above the exact value.
#[inline]
fn min_square(distance: f64) -> f64 {
    let gap = distance.abs() - 0.45;
    if gap <= 0.0 {
        0.0
    } else {
        gap * gap * 0.999_999
    }
}

fn fiddled_distance(
    seed: i64,
    x: i32,
    y: i32,
    z: i32,
    distance_x: f64,
    distance_y: f64,
    distance_z: f64,
) -> f64 {
    let (x, y, z) = (i64::from(x), i64::from(y), i64::from(z));
    let mut rval = lcg_next(seed, x);
    rval = lcg_next(rval, y);
    rval = lcg_next(rval, z);
    rval = lcg_next(rval, x);
    rval = lcg_next(rval, y);
    rval = lcg_next(rval, z);
    let fiddle_x = fiddle(rval);
    rval = lcg_next(rval, seed);
    let fiddle_y = fiddle(rval);
    rval = lcg_next(rval, seed);
    let fiddle_z = fiddle(rval);
    square(distance_z + fiddle_z) + square(distance_y + fiddle_y) + square(distance_x + fiddle_x)
}

#[inline]
fn square(d: f64) -> f64 {
    d * d
}

/// `getFiddle(rval)`.
#[inline]
fn fiddle(rval: i64) -> f64 {
    let uniform = (rval >> 24).rem_euclid(1024) as f64 / 1024.0;
    (uniform - 0.5) * 0.9
}

/// `LinearCongruentialGenerator.next(rval, c)`.
#[inline]
pub fn lcg_next(rval: i64, c: i64) -> i64 {
    let rval = rval.wrapping_mul(
        rval.wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407),
    );
    rval.wrapping_add(c)
}

/// `BiomeManager.obfuscateSeed(seed)`: the first 8 bytes (little endian) of the SHA-256 of the
/// seed's 8 little-endian bytes (Guava's `Hashing.sha256().hashLong(seed).asLong()`).
pub fn obfuscate_seed(seed: i64) -> i64 {
    let digest = sha256(&seed.to_le_bytes());
    i64::from_le_bytes(digest[..8].try_into().expect("8 bytes"))
}

/// SHA-256 (FIPS 180-4) of a short message.
fn sha256(message: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut padded = message.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&((message.len() as u64) * 8).to_be_bytes());
    for block in padded.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for (i, word) in block.as_chunks::<4>().0.iter().enumerate() {
            w[i] = u32::from_be_bytes(*word);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (state, v) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *state = state.wrapping_add(v);
        }
    }
    let mut out = [0u8; 32];
    for (chunk, v) in out.as_chunks_mut::<4>().0.iter_mut().zip(h) {
        *chunk = v.to_be_bytes();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_known_vectors() {
        let hex = |d: [u8; 32]| d.iter().map(|b| format!("{b:02x}")).collect::<String>();
        assert_eq!(
            hex(sha256(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex(sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}

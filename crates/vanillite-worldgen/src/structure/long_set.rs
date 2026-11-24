//! A set of `long`s that iterates in the order of fastutil's `LongOpenHashSet`.
//!
//! Vanilla keeps the structure references of a chunk in `LongOpenHashSet`s and iterates them
//! when it places structures and builds the `Beardifier`; the order decides which start is placed
//! first where two starts of one structure overlap a chunk, so the port reproduces the set's
//! open-addressing layout (default capacity, load factor 0.75, linear probing, `HashCommon.mix`)
//! and its iteration from the last slot down.

/// fastutil's `LongOpenHashSet` with Vanilla's default construction (`new LongOpenHashSet()`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LongOpenHashSet {
    /// Slots; `0` is free (the key `0` lives in `contains_zero`).
    keys: Vec<i64>,
    contains_zero: bool,
    size: usize,
    max_fill: usize,
}

/// `Hash.DEFAULT_LOAD_FACTOR`.
const LOAD_FACTOR: f32 = 0.75;
/// `Hash.DEFAULT_INITIAL_SIZE`.
const INITIAL_SIZE: usize = 16;

/// `HashCommon.mix(long)`.
#[inline]
fn mix(x: i64) -> i64 {
    let h = x.wrapping_mul(0x9E37_79B9_7F4A_7C15_u64 as i64);
    let h = h ^ ((h as u64) >> 32) as i64;
    h ^ ((h as u64) >> 16) as i64
}

/// `HashCommon.arraySize(expected, f)`.
fn array_size(expected: usize, f: f32) -> usize {
    let needed = (expected as f64 / f64::from(f)).ceil() as usize;
    needed.next_power_of_two().max(2)
}

/// `HashCommon.maxFill(n, f)`.
fn max_fill(n: usize, f: f32) -> usize {
    ((n as f64 * f64::from(f)).ceil() as usize).min(n - 1)
}

impl Default for LongOpenHashSet {
    fn default() -> Self {
        let n = array_size(INITIAL_SIZE, LOAD_FACTOR);
        Self {
            keys: vec![0; n],
            contains_zero: false,
            size: 0,
            max_fill: max_fill(n, LOAD_FACTOR),
        }
    }
}

impl LongOpenHashSet {
    /// An empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// `new LongOpenHashSet(long[] a)`: sized for the array, then filled in its order (how
    /// Vanilla reads a chunk's saved references).
    pub fn from_array(keys: &[i64]) -> Self {
        let n = array_size(keys.len(), LOAD_FACTOR);
        let mut set = Self {
            keys: vec![0; n],
            contains_zero: false,
            size: 0,
            max_fill: max_fill(n, LOAD_FACTOR),
        };
        for &key in keys {
            set.insert(key);
        }
        set
    }

    /// Number of elements.
    pub fn len(&self) -> usize {
        self.size
    }

    /// Whether the set is empty.
    pub fn is_empty(&self) -> bool {
        self.size == 0
    }

    fn mask(&self) -> usize {
        self.keys.len() - 1
    }

    /// `contains(k)`.
    pub fn contains(&self, key: i64) -> bool {
        if key == 0 {
            return self.contains_zero;
        }
        let mask = self.mask();
        let mut pos = mix(key) as i32 as usize & mask;
        loop {
            let current = self.keys[pos];
            if current == 0 {
                return false;
            }
            if current == key {
                return true;
            }
            pos = (pos + 1) & mask;
        }
    }

    /// `add(k)`: whether the key was new.
    pub fn insert(&mut self, key: i64) -> bool {
        if key == 0 {
            if self.contains_zero {
                return false;
            }
            self.contains_zero = true;
        } else {
            let mask = self.mask();
            let mut pos = mix(key) as i32 as usize & mask;
            loop {
                let current = self.keys[pos];
                if current == 0 {
                    break;
                }
                if current == key {
                    return false;
                }
                pos = (pos + 1) & mask;
            }
            self.keys[pos] = key;
        }
        let before = self.size;
        self.size += 1;
        if before >= self.max_fill {
            self.rehash(array_size(self.size + 1, LOAD_FACTOR));
        }
        true
    }

    /// `rehash(newN)`: re-inserts the keys from the last slot down.
    fn rehash(&mut self, new_n: usize) {
        let mask = new_n - 1;
        let mut new_keys = vec![0_i64; new_n];
        for &key in self.keys.iter().rev().filter(|&&k| k != 0) {
            let mut pos = mix(key) as i32 as usize & mask;
            while new_keys[pos] != 0 {
                pos = (pos + 1) & mask;
            }
            new_keys[pos] = key;
        }
        self.keys = new_keys;
        self.max_fill = max_fill(new_n, LOAD_FACTOR);
    }

    /// The keys in iteration order: `0` first if present, then the slots from last to first.
    pub fn iter(&self) -> impl Iterator<Item = i64> + '_ {
        self.contains_zero
            .then_some(0)
            .into_iter()
            .chain(self.keys.iter().rev().copied().filter(|&k| k != 0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_every_key_once() {
        let mut set = LongOpenHashSet::new();
        for i in -40..40_i64 {
            assert!(set.insert(i * 0x1_0000_0001));
            assert!(!set.insert(i * 0x1_0000_0001));
        }
        assert_eq!(set.len(), 80);
        let mut keys: Vec<i64> = set.iter().collect();
        assert_eq!(keys.len(), 80);
        assert_eq!(keys[0], 0);
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), 80);
        assert!(set.contains(3 * 0x1_0000_0001));
        assert!(!set.contains(77));
    }

    /// Iteration order checked against fastutil 8.5.18 (`LongOpenHashSet` filled with
    /// `ChunkPos.pack(x, z)` for `x` in -2..=2, `z` in -2..=2, x outer).
    #[test]
    fn iterates_like_fastutil() {
        let mut set = LongOpenHashSet::new();
        for x in -2..=2_i32 {
            for z in -2..=2_i32 {
                set.insert(i64::from(x as u32) | (i64::from(z as u32) << 32));
            }
        }
        let order: Vec<(i32, i32)> = set.iter().map(|k| (k as i32, (k >> 32) as i32)).collect();
        assert_eq!(order, FASTUTIL_ORDER);
    }

    /// A larger set (287 keys, two rehashes): size and a Java style hash of the order.
    #[test]
    fn rehashes_like_fastutil() {
        let mut set = LongOpenHashSet::new();
        for x in -20..=20_i32 {
            for z in -3..=3_i32 {
                set.insert(i64::from(x as u32) | (i64::from(z as u32) << 32));
            }
        }
        let hash = set
            .iter()
            .fold(0_i64, |h, k| h.wrapping_mul(31).wrapping_add(k));
        assert_eq!((set.len(), hash), (287, -6_995_584_464_196_863_242));
    }

    const FASTUTIL_ORDER: [(i32, i32); 25] = [
        (0, 0),
        (0, 2),
        (0, -2),
        (-1, 2),
        (-1, 1),
        (1, -2),
        (1, 2),
        (2, -2),
        (-2, 1),
        (-2, -1),
        (2, 0),
        (2, 2),
        (0, 1),
        (0, -1),
        (1, -1),
        (-1, 0),
        (-1, -1),
        (-1, -2),
        (1, 0),
        (1, 1),
        (2, -1),
        (-2, 0),
        (-2, -2),
        (-2, 2),
        (2, 1),
    ];
}

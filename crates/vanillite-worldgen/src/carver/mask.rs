//! `CarvingMask` (`net.minecraft.world.level.chunk.CarvingMask`): the blocks of one chunk that
//! carvers cut out, one bit per block between a minimum and a maximum Y.

/// The carved blocks of one chunk (`CarvingMask`, Vanilla's only `CarverOutput`).
///
/// Bits are stored column by column like Vanilla's `BitSet`
/// (`index = y - min_y + (z + (x << 4)) * height`), so [`CarvingMask::visit`] reports the same
/// column segments in the same order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CarvingMask {
    min_y: i32,
    max_y: i32,
    height: i32,
    words: Box<[u64]>,
}

impl CarvingMask {
    /// An empty mask for `min_y..=max_y`.
    pub fn new(min_y: i32, max_y: i32) -> Self {
        let height = max_y - min_y + 1;
        assert!(height > 0, "carving mask height must be positive");
        let bits = 256 * height as usize;
        Self {
            min_y,
            max_y,
            height,
            words: vec![0; bits.div_ceil(64)].into_boxed_slice(),
        }
    }

    #[inline]
    fn index(&self, x: i32, y: i32, z: i32) -> usize {
        (y - self.min_y + (z + (x << 4)) * self.height) as usize
    }

    /// `minY()`: the lowest Y carvers may cut.
    pub const fn min_y(&self) -> i32 {
        self.min_y
    }

    /// `maxY()`: the highest Y carvers may cut.
    pub const fn max_y(&self) -> i32 {
        self.max_y
    }

    /// `carve(x, y, z)`: marks a block (`x`, `z` chunk-relative, `y` absolute within range).
    #[inline]
    pub fn carve(&mut self, x: i32, y: i32, z: i32) {
        let index = self.index(x, y, z);
        self.words[index / 64] |= 1 << (index % 64);
    }

    /// Whether a block is carved (`x`, `z` chunk-relative); `false` outside the Y range.
    pub fn get(&self, x: i32, y: i32, z: i32) -> bool {
        if !(0..16).contains(&x) || !(0..16).contains(&z) || y < self.min_y || y > self.max_y {
            return false;
        }
        let index = self.index(x, y, z);
        self.words[index / 64] & (1 << (index % 64)) != 0
    }

    /// `isEmpty()`.
    pub fn is_empty(&self) -> bool {
        self.words.iter().all(|&w| w == 0)
    }

    /// Number of carved blocks.
    pub fn count(&self) -> usize {
        self.words.iter().map(|w| w.count_ones() as usize).sum()
    }

    /// Indices of the set bits in ascending order (Vanilla's `BitSet` order).
    pub fn set_bits(&self) -> impl Iterator<Item = usize> + '_ {
        self.words.iter().enumerate().flat_map(|(i, &word)| {
            let mut w = word;
            std::iter::from_fn(move || {
                if w == 0 {
                    return None;
                }
                let bit = w.trailing_zeros() as usize;
                w &= w - 1;
                Some(i * 64 + bit)
            })
        })
    }

    /// `visit(visitor)`: calls `visitor(x, z, bottom_y, top_y)` for every run of carved blocks
    /// within a column, in ascending bit order.
    pub fn visit(&self, mut visitor: impl FnMut(i32, i32, i32, i32)) {
        let total = self.words.len() * 64;
        let mut start = self.next_set_bit(0);
        while let Some(start_index) = start {
            let end_index = self.next_clear_bit(start_index).min(total) - 1;
            self.visit_segment(&mut visitor, start_index, end_index);
            start = self.next_set_bit(end_index + 1);
        }
    }

    fn visit_segment(
        &self,
        visitor: &mut impl FnMut(i32, i32, i32, i32),
        start: usize,
        end: usize,
    ) {
        let height = self.height as usize;
        let start_column = start / height;
        let end_column = end / height;
        for column in start_column..=end_column {
            let column_x = (column >> 4 & 15) as i32;
            let column_z = (column & 15) as i32;
            let base = column * height;
            let bottom = start.saturating_sub(base) as i32 + self.min_y;
            let top = (end - base).min(height - 1) as i32 + self.min_y;
            visitor(column_x, column_z, bottom, top);
        }
    }

    fn next_set_bit(&self, from: usize) -> Option<usize> {
        let mut word_index = from / 64;
        if word_index >= self.words.len() {
            return None;
        }
        let mut word = self.words[word_index] & (u64::MAX << (from % 64));
        loop {
            if word != 0 {
                return Some(word_index * 64 + word.trailing_zeros() as usize);
            }
            word_index += 1;
            if word_index == self.words.len() {
                return None;
            }
            word = self.words[word_index];
        }
    }

    fn next_clear_bit(&self, from: usize) -> usize {
        let mut word_index = from / 64;
        if word_index >= self.words.len() {
            return from;
        }
        let mut word = !self.words[word_index] & (u64::MAX << (from % 64));
        loop {
            if word != 0 {
                return word_index * 64 + word.trailing_zeros() as usize;
            }
            word_index += 1;
            if word_index == self.words.len() {
                return self.words.len() * 64;
            }
            word = !self.words[word_index];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visit_reports_column_runs() {
        let mut mask = CarvingMask::new(-63, 312);
        for y in 10..=20 {
            mask.carve(3, y, 5);
        }
        mask.carve(3, 312, 5);
        mask.carve(3, -63, 6);
        mask.carve(15, 312, 15);
        let mut runs = Vec::new();
        mask.visit(|x, z, bottom, top| runs.push((x, z, bottom, top)));
        // The run at the top of column (3, 5) continues into the bottom of column (3, 6): one
        // Vanilla segment spanning two columns.
        assert_eq!(
            runs,
            vec![
                (3, 5, 10, 20),
                (3, 5, 312, 312),
                (3, 6, -63, -63),
                (15, 15, 312, 312)
            ]
        );
        assert_eq!(mask.count(), 14);
        assert!(mask.get(3, 15, 5) && !mask.get(3, 21, 5));
    }
}

//! Small Vanilla helpers shared by the vegetation and End features: `Util.shuffle`,
//! `Direction` parsing and planes, `Vec3i.distManhattan`, block state setters and the
//! iteration order of `java.util.HashSet<BlockPos>`.

use rustc_hash::FxHashSet;
use serde_json::Value;
use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Direction};
use vanillite_data::block::BlockState;
use vanillite_data::fluid::FluidState;

use crate::Error;

/// `Direction.Plane.HORIZONTAL` in Vanilla's order.
pub(crate) const HORIZONTAL: [Direction; 4] = [
    Direction::North,
    Direction::East,
    Direction::South,
    Direction::West,
];

/// Registry id of `minecraft:flowing_water` in `minecraft:fluid`.
const FLOWING_WATER: u16 = 1;
/// Registry id of `minecraft:water` in `minecraft:fluid`.
const WATER: u16 = 2;

/// `Util.shuffle(list, random)`: Vanilla's Fisher–Yates variant (swap from the end).
pub(crate) fn shuffle<T, R: RandomSource>(list: &mut [T], random: &mut R) {
    for i in (2..=list.len()).rev() {
        let swap_to = random.next_int_bounded(i as i32) as usize;
        list.swap(i - 1, swap_to);
    }
}

/// `Direction.getAxis()` as an index (x = 0, y = 1, z = 2).
pub(crate) const fn axis(direction: Direction) -> u8 {
    match direction {
        Direction::West | Direction::East => 0,
        Direction::Down | Direction::Up => 1,
        Direction::North | Direction::South => 2,
    }
}

/// `Direction.CODEC`: a direction by its serialized name.
pub(crate) fn parse_direction(json: &Value) -> Result<Direction, Error> {
    Ok(match json.as_str() {
        Some("down") => Direction::Down,
        Some("up") => Direction::Up,
        Some("north") => Direction::North,
        Some("south") => Direction::South,
        Some("west") => Direction::West,
        Some("east") => Direction::East,
        _ => return Err(Error::new(format!("invalid direction {json}"))),
    })
}

/// `BlockPos.CODEC`: `[x, y, z]`.
pub(crate) fn parse_block_pos(json: &Value) -> Result<BlockPos, Error> {
    let coordinate = |v: &Value| {
        v.as_i64()
            .and_then(|v| i32::try_from(v).ok())
            .ok_or_else(|| Error::new(format!("invalid block position {json}")))
    };
    match json.as_array().map(Vec::as_slice) {
        Some([x, y, z]) => Ok(BlockPos::new(
            coordinate(x)?,
            coordinate(y)?,
            coordinate(z)?,
        )),
        _ => Err(Error::new(format!("invalid block position {json}"))),
    }
}

/// `Vec3i.distManhattan`: the sum is taken in `float`, as Vanilla does.
pub(crate) fn dist_manhattan(a: BlockPos, b: BlockPos) -> i32 {
    let xd = (b.x.wrapping_sub(a.x) as f32).abs();
    let yd = (b.y.wrapping_sub(a.y) as f32).abs();
    let zd = (b.z.wrapping_sub(a.z) as f32).abs();
    (xd + yd + zd) as i32
}

/// `state.setValue(property, value)` for a property the block is known to have (fixed Vanilla
/// block data, so a miss is a programming error).
pub(crate) fn with(state: BlockState, property: &str, value: &str) -> BlockState {
    state.with(property, value).unwrap_or_else(|| {
        panic!(
            "{} has no property {property}={value}",
            state.block().name()
        )
    })
}

/// `state.setValue(property, flag)` for a boolean property (see [`with`]).
pub(crate) fn with_bool(state: BlockState, property: &str, flag: bool) -> BlockState {
    with(state, property, if flag { "true" } else { "false" })
}

/// `state.getValue(property)` of a boolean property; `false` if the state has none
/// (`getValueOrElse(property, false)`).
pub(crate) fn bool_value(state: BlockState, property: &str) -> bool {
    state.value(property) == Some("true")
}

/// `fluidState.is(FluidTags.WATER)`.
pub(crate) fn is_water(fluid: FluidState) -> bool {
    matches!(fluid.info().fluid, FLOWING_WATER | WATER)
}

/// `fluidState.is(Fluids.WATER)`: the (source type) water fluid, not flowing water.
pub(crate) fn is_water_fluid(fluid: FluidState) -> bool {
    fluid.info().fluid == WATER
}

/// `fluidState.isSourceOfType(Fluids.WATER)`.
pub(crate) fn is_water_source(fluid: FluidState) -> bool {
    is_water_fluid(fluid) && fluid.info().source
}

/// A `java.util.HashSet<BlockPos>` that only grows: positions are kept in insertion order and
/// iterated in the order Java's `HashMap` buckets yield them.
///
/// Java iterates bucket by bucket (`(capacity - 1) & (h ^ h >>> 16)` with `h` =
/// `Vec3i.hashCode`), each bucket in insertion order (resizing splits buckets but keeps their
/// relative order). The capacity is simulated insert by insert, including the resize that
/// `treeifyBin` does instead of treeifying while the table is smaller than 64. Tree bins of
/// larger tables (nine or more positions in one bucket) are not modelled: their order depends on
/// identity hash codes in Vanilla itself.
#[derive(Debug, Default)]
pub(crate) struct JavaPosSet {
    entries: Vec<BlockPos>,
    seen: FxHashSet<BlockPos>,
    capacity: usize,
    bucket_sizes: Vec<u32>,
}

impl JavaPosSet {
    /// `Vec3i.hashCode()` spread like `HashMap.hash`.
    fn hash(pos: BlockPos) -> u32 {
        let h = pos
            .y
            .wrapping_add(pos.z.wrapping_mul(31))
            .wrapping_mul(31)
            .wrapping_add(pos.x) as u32;
        h ^ (h >> 16)
    }

    fn bucket(&self, pos: BlockPos) -> usize {
        Self::hash(pos) as usize & (self.capacity - 1)
    }

    fn resize(&mut self, capacity: usize) {
        self.capacity = capacity;
        self.bucket_sizes.clear();
        self.bucket_sizes.resize(capacity, 0);
        for i in 0..self.entries.len() {
            let bucket = self.bucket(self.entries[i]);
            self.bucket_sizes[bucket] += 1;
        }
    }

    /// `add(pos)`: whether the position was new.
    pub(crate) fn insert(&mut self, pos: BlockPos) -> bool {
        if !self.seen.insert(pos) {
            return false;
        }
        if self.capacity == 0 {
            self.resize(16);
        }
        let bucket = self.bucket(pos);
        let bin_count = self.bucket_sizes[bucket];
        self.entries.push(pos);
        self.bucket_sizes[bucket] += 1;
        // `putVal`: a ninth node in one bin calls `treeifyBin`, which resizes small tables.
        if bin_count >= 8 && self.capacity < 64 {
            self.resize(self.capacity * 2);
        }
        if self.entries.len() > self.capacity / 4 * 3 {
            self.resize(self.capacity * 2);
        }
        true
    }

    /// `isEmpty()`.
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Writes the positions to `out` (cleared first) in Java's iteration order.
    pub(crate) fn iteration_order(&self, out: &mut Vec<BlockPos>) {
        out.clear();
        out.extend_from_slice(&self.entries);
        if self.capacity > 0 {
            out.sort_by_key(|&pos| self.bucket(pos));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn java_hash_set_orders_by_bucket_then_insertion() {
        let mut set = JavaPosSet::default();
        // Hashes 17 and 1 share bucket 1 of a 16 bucket table; 0 is bucket 0.
        for pos in [
            BlockPos::new(17, 0, 0),
            BlockPos::new(1, 0, 0),
            BlockPos::new(0, 0, 0),
        ] {
            assert!(set.insert(pos));
        }
        assert!(!set.insert(BlockPos::new(1, 0, 0)));
        let mut order = Vec::new();
        set.iteration_order(&mut order);
        assert_eq!(
            order,
            [
                BlockPos::new(0, 0, 0),
                BlockPos::new(17, 0, 0),
                BlockPos::new(1, 0, 0)
            ]
        );
    }

    #[test]
    fn java_hash_set_grows_after_three_quarters() {
        let mut set = JavaPosSet::default();
        for x in 0..12 {
            set.insert(BlockPos::new(x, 0, 0));
        }
        assert_eq!(set.capacity, 16);
        set.insert(BlockPos::new(12, 0, 0));
        assert_eq!(set.capacity, 32);
    }

    #[test]
    fn java_hash_set_resizes_instead_of_treeifying_small_tables() {
        let mut set = JavaPosSet::default();
        // Hashes x·16 all fall into bucket 0 of 16: the ninth one resizes to 32 buckets, where
        // the even multiples stay in bucket 0 and the odd ones move to bucket 16.
        for x in 0..9 {
            set.insert(BlockPos::new(x * 16, 0, 0));
        }
        assert_eq!(set.capacity, 32);
        let mut order = Vec::new();
        set.iteration_order(&mut order);
        let xs: Vec<i32> = order.iter().map(|p| p.x / 16).collect();
        assert_eq!(xs, [0, 2, 4, 6, 8, 1, 3, 5, 7]);
    }

    #[test]
    fn shuffle_matches_vanilla_swaps() {
        use vanillite_base::random::LegacyRandom;
        let mut list = [0, 1, 2, 3];
        let mut random = LegacyRandom::new(0);
        let mut expected = [0, 1, 2, 3];
        let mut check = LegacyRandom::new(0);
        for i in (2..=4).rev() {
            let j = check.next_int_bounded(i) as usize;
            expected.swap(i as usize - 1, j);
        }
        shuffle(&mut list, &mut random);
        assert_eq!(list, expected);
    }
}

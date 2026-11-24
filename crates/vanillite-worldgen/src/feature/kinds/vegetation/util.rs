//! Small Vanilla helpers shared by the vegetation and End features: `Util.shuffle`,
//! `Direction` parsing and planes, `Vec3i.distManhattan`, block state setters and the
//! iteration order of `java.util.HashSet<BlockPos>`.

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

#[cfg(test)]
mod tests {
    use super::*;

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

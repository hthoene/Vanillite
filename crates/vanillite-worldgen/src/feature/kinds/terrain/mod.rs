//! Underground and terrain shaping features: ores, disks, lakes, springs, geodes, dripstone, ice,
//! sculk and monster rooms.
//!
//! One file per feature or group of related features; the helpers shared between them (block
//! and fluid state parsing, `Feature` default methods, direction utilities, face shapes) live
//! here.

mod blob;
mod column;
mod fluid;
mod geode;
mod ice;
mod monster_room;
mod ore;
mod root_system;
mod rule_test;
mod sculk;
mod simple;
mod speleothem;

pub use blob::{BlockBlobFeature, DiskFeature, ReplaceBlobsFeature};
pub use fluid::{LakeFeature, SpringFeature, UnderwaterMagmaFeature};
pub use geode::GeodeFeature;
pub use ice::{BlueIceFeature, IcebergFeature, SnowAndFreezeFeature, SpikeFeature};
pub use monster_room::MonsterRoomFeature;
pub use ore::{OreFeature, ScatteredOreFeature};
pub use root_system::RootSystemFeature;
pub use rule_test::{BlockReplacement, RuleTest};
pub use sculk::SculkPatchFeature;
pub use simple::{FillLayerFeature, OverlayFeature, ReplaceBlockFeature};
pub use speleothem::{LargeDripstoneFeature, SpeleothemClusterFeature, SpeleothemFeature};

use serde_json::Value;
use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Direction};
use vanillite_data::block::{BlockId, BlockState, Shape, ids};
use vanillite_data::fluid::{FLUID_STATES, FluidState};
use vanillite_data::registry::Registry;

use crate::Error;
use crate::biome::BiomeData;
use crate::blocks::{parse_block_state, qualify};
use crate::feature::FeatureParser;
use crate::feature::region::{WorldGenRegion, flags};

/// `Direction.Plane.HORIZONTAL` in Vanilla's iteration order.
pub(crate) const HORIZONTAL: [Direction; 4] = [
    Direction::North,
    Direction::East,
    Direction::South,
    Direction::West,
];

/// A required block state field.
pub(crate) fn state_field(json: &Value, name: &str) -> Result<BlockState, Error> {
    parse_block_state(crate::feature::field(json, name)?)
}

/// Parses a fluid state as `FluidState.CODEC` reads it: `{"id": "minecraft:lava", "properties":
/// {"falling": "true"}}` (`Name`/`Properties` accepted as well). Flowing fluids need `level`.
pub(crate) fn parse_fluid_state(json: &Value) -> Result<FluidState, Error> {
    let (name, properties) = match json {
        Value::String(name) => (name.as_str(), None),
        Value::Object(map) => (
            map.get("id")
                .or_else(|| map.get("Name"))
                .and_then(Value::as_str)
                .ok_or_else(|| Error::new(format!("fluid state without id: {json}")))?,
            map.get("properties")
                .or_else(|| map.get("Properties"))
                .and_then(Value::as_object),
        ),
        _ => return Err(Error::new(format!("invalid fluid state {json}"))),
    };
    let name = qualify(name);
    let fluid = Registry::by_name("minecraft:fluid")
        .and_then(|r| r.id(&name))
        .ok_or_else(|| Error::new(format!("unknown fluid {name}")))?;
    if fluid == 0 {
        return Ok(FluidState::EMPTY);
    }
    let property = |key: &str| properties.and_then(|p| p.get(key)).and_then(Value::as_str);
    let falling = match property("falling") {
        None | Some("false") => false,
        Some("true") => true,
        Some(other) => return Err(Error::new(format!("{name}: invalid falling={other}"))),
    };
    let source = matches!(fluid, FLUID_WATER | FLUID_LAVA);
    let amount = if source {
        8
    } else {
        property("level")
            .and_then(|l| l.parse::<u8>().ok())
            .filter(|l| (1..=8).contains(l))
            .ok_or_else(|| Error::new(format!("{name}: missing or invalid level")))?
    };
    FLUID_STATES
        .iter()
        .position(|f| {
            f.fluid == fluid && f.source == source && f.falling == falling && f.amount == amount
        })
        .and_then(|i| u8::try_from(i).ok())
        .map(FluidState)
        .ok_or_else(|| Error::new(format!("no fluid state for {json}")))
}

/// Registry ids of `minecraft:fluid`.
const FLUID_FLOWING_WATER: u16 = 1;
const FLUID_WATER: u16 = 2;
const FLUID_FLOWING_LAVA: u16 = 3;
const FLUID_LAVA: u16 = 4;

/// `fluidState.is(FluidTags.WATER)`.
#[inline]
pub(crate) fn is_water(fluid: FluidState) -> bool {
    matches!(fluid.info().fluid, FLUID_FLOWING_WATER | FLUID_WATER)
}

/// `fluidState.is(FluidTags.LAVA)`.
#[inline]
pub(crate) fn is_lava(fluid: FluidState) -> bool {
    matches!(fluid.info().fluid, FLUID_FLOWING_LAVA | FLUID_LAVA)
}

/// `fluidState.is(Fluids.WATER)`: still (source) water only.
#[inline]
pub(crate) fn is_still_water(fluid: FluidState) -> bool {
    fluid.info().fluid == FLUID_WATER
}

/// `fluidState.isSource()`.
#[inline]
pub(crate) fn is_source(fluid: FluidState) -> bool {
    fluid.info().source
}

/// The fluid tick type of a non-empty fluid state (see [`crate::proto::FluidTickKind`]).
#[inline]
pub(crate) fn fluid_tick_kind(fluid: FluidState) -> Option<u16> {
    (!fluid.is_empty()).then_some(fluid.info().fluid)
}

/// `level.scheduleTick(pos, fluidState.getType(), 0)` for a non-empty fluid state.
pub(crate) fn schedule_fluid_tick(
    level: &mut WorldGenRegion<'_>,
    pos: BlockPos,
    fluid: FluidState,
) {
    if let Some(kind) = fluid_tick_kind(fluid) {
        level.schedule_fluid_tick(pos, kind);
    }
}

/// `state.is(block)`.
#[inline]
pub(crate) fn is(state: BlockState, block: BlockId) -> bool {
    state.block() == block
}

/// `Feature.safeSetBlock`: sets the block with `UPDATE_CLIENTS` if the current one passes
/// `can_replace`.
pub(crate) fn safe_set_block(
    level: &mut WorldGenRegion<'_>,
    pos: BlockPos,
    state: BlockState,
    can_replace: impl FnOnce(BlockState) -> bool,
) {
    if can_replace(level.block(pos)) {
        level.set_block(pos, state, flags::CLIENTS);
    }
}

/// `Feature.markAboveForPostProcessing`: marks up to two non-air blocks above `pos`.
pub(crate) fn mark_above_for_post_processing(level: &mut WorldGenRegion<'_>, pos: BlockPos) {
    let mut pos = pos;
    for _ in 0..2 {
        pos = pos.relative(Direction::Up);
        if level.block(pos).is_air() {
            return;
        }
        level.mark_post_processing(pos);
    }
}

/// `Direction.getRandom(random)`.
#[inline]
pub(crate) fn random_direction<R: RandomSource>(random: &mut R) -> Direction {
    Direction::ALL[random.next_int_bounded(6) as usize]
}

/// `Util.shuffle(list, random)`: Vanilla's Fisher–Yates order.
pub(crate) fn shuffle<T, R: RandomSource>(items: &mut [T], random: &mut R) {
    for i in (2..=items.len()).rev() {
        let swap_to = random.next_int_bounded(i as i32) as usize;
        items.swap(i - 1, swap_to);
    }
}

/// `Direction.allShuffled(random)`.
pub(crate) fn shuffled_directions<R: RandomSource>(random: &mut R) -> [Direction; 6] {
    let mut directions = Direction::ALL;
    shuffle(&mut directions, random);
    directions
}

/// The serialized name of a direction (`Direction.getSerializedName`).
#[inline]
pub(crate) const fn direction_name(direction: Direction) -> &'static str {
    match direction {
        Direction::Down => "down",
        Direction::Up => "up",
        Direction::North => "north",
        Direction::South => "south",
        Direction::West => "west",
        Direction::East => "east",
    }
}

/// Axis index of a direction (x = 0, y = 1, z = 2).
#[inline]
pub(crate) const fn axis(direction: Direction) -> usize {
    match direction {
        Direction::West | Direction::East => 0,
        Direction::Down | Direction::Up => 1,
        Direction::North | Direction::South => 2,
    }
}

/// Whether a direction points along its axis' positive side.
#[inline]
pub(crate) const fn is_positive(direction: Direction) -> bool {
    matches!(
        direction,
        Direction::Up | Direction::South | Direction::East
    )
}

/// `Block.isFaceFull(shape, direction)`: whether the shape's face slice on `direction`
/// (`VoxelShape.getFaceShape`) is exactly the full square.
///
/// The face slice holds the boxes that contain the coordinate `0.9999999` (positive side) or
/// `1.0E-7` (negative side) along the axis; the slice is full when the union of their
/// projections equals the unit square (nothing missing inside, nothing sticking out).
pub(crate) fn is_face_full(shape: Shape, direction: Direction) -> bool {
    const FULL: [f64; 6] = [0.0, 0.0, 0.0, 1.0, 1.0, 1.0];
    if shape.is_empty() {
        return false;
    }
    if shape.len() == 1 && shape[0] == FULL {
        return true;
    }
    let a = axis(direction);
    let (u, v) = match a {
        0 => (1, 2),
        1 => (0, 2),
        _ => (0, 1),
    };
    let at = if is_positive(direction) {
        0.9999999
    } else {
        1.0E-7
    };
    let selected = |b: &[f64; 6]| b[a] < at && b[a + 3] > at;
    // Coordinate compression on the two face axes; shapes have few boxes.
    let mut us = [0.0f64; 66];
    let mut vs = [0.0f64; 66];
    let (mut nu, mut nv) = (2, 2);
    us[1] = 1.0;
    vs[1] = 1.0;
    let mut heap_u = Vec::new();
    let mut heap_v = Vec::new();
    for b in shape.iter().filter(|b| selected(b)) {
        if nu + 2 <= us.len() {
            us[nu] = b[u];
            us[nu + 1] = b[u + 3];
            vs[nv] = b[v];
            vs[nv + 1] = b[v + 3];
            nu += 2;
            nv += 2;
        } else {
            heap_u.extend([b[u], b[u + 3]]);
            heap_v.extend([b[v], b[v + 3]]);
        }
    }
    if !heap_u.is_empty() {
        heap_u.extend_from_slice(&us[..nu]);
        heap_v.extend_from_slice(&vs[..nv]);
        return face_cells_full(&mut heap_u, &mut heap_v, shape, selected, u, v);
    }
    face_cells_full(&mut us[..nu], &mut vs[..nv], shape, selected, u, v)
}

fn face_cells_full(
    us: &mut [f64],
    vs: &mut [f64],
    shape: Shape,
    selected: impl Fn(&[f64; 6]) -> bool,
    u: usize,
    v: usize,
) -> bool {
    us.sort_unstable_by(f64::total_cmp);
    vs.sort_unstable_by(f64::total_cmp);
    for pair_u in us.windows(2) {
        if pair_u[0] == pair_u[1] {
            continue;
        }
        let mu = (pair_u[0] + pair_u[1]) * 0.5;
        for pair_v in vs.windows(2) {
            if pair_v[0] == pair_v[1] {
                continue;
            }
            let mv = (pair_v[0] + pair_v[1]) * 0.5;
            let inside = mu > 0.0 && mu < 1.0 && mv > 0.0 && mv < 1.0;
            let covered = shape
                .iter()
                .any(|b| selected(b) && b[u] < mu && mu < b[u + 3] && b[v] < mv && mv < b[v + 3]);
            if inside != covered {
                return false;
            }
        }
    }
    true
}

/// The climate settings of every biome, copied from the biome registry at parse time for the
/// features that check `Biome.shouldFreeze`/`shouldSnow`.
#[derive(Debug)]
pub(crate) struct Climates(Vec<BiomeData>);

impl Climates {
    /// Copies the climate of every biome (without the biome JSON).
    pub(crate) fn new(parser: &FeatureParser<'_>) -> Self {
        Self(
            parser
                .biomes()
                .all()
                .iter()
                .map(|b| BiomeData {
                    name: String::new(),
                    id: b.id,
                    temperature: b.temperature,
                    downfall: b.downfall,
                    has_precipitation: b.has_precipitation,
                    frozen_temperature_modifier: b.frozen_temperature_modifier,
                    carvers: Vec::new(),
                    json: Value::Null,
                })
                .collect(),
        )
    }

    /// The biome at `pos` of the region (`level.getBiome(pos).value()`).
    fn at(&self, level: &WorldGenRegion<'_>, pos: BlockPos) -> &BiomeData {
        &self.0[usize::from(level.biome(pos))]
    }

    /// `level.getBiome(biomePos).value().shouldFreeze(level, pos, false)`.
    ///
    /// Block light is always below 10 during generation: light is initialized after the
    /// features stage, so `getBrightness(BLOCK, pos)` reads 0.
    pub(crate) fn should_freeze(
        &self,
        level: &WorldGenRegion<'_>,
        biome_pos: BlockPos,
        pos: BlockPos,
    ) -> bool {
        let biome = self.at(level, biome_pos);
        if biome.warm_enough_to_rain(pos.x, pos.y, pos.z, level.sea_level()) {
            return false;
        }
        if level.is_outside_build_height(pos.y) {
            return false;
        }
        // `fluidState.is(Fluids.WATER) && blockState.getBlock() instanceof LiquidBlock`
        let state = level.block(pos);
        is(state, ids::WATER) && is_still_water(state.fluid_state())
    }

    /// `level.getBiome(pos).value().shouldSnow(level, pos)` (block light is 0, see
    /// [`Self::should_freeze`]).
    pub(crate) fn should_snow(&self, level: &WorldGenRegion<'_>, pos: BlockPos) -> bool {
        let biome = self.at(level, pos);
        if !biome.has_precipitation
            || biome.warm_enough_to_rain(pos.x, pos.y, pos.z, level.sea_level())
        {
            return false;
        }
        if level.is_outside_build_height(pos.y) {
            return false;
        }
        let state = level.block(pos);
        (state.is_air() || is(state, ids::SNOW))
            && crate::behaviour::can_survive(level, ids::SNOW.default_state(), pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn face_fullness() {
        let full: Shape = &[[0.0, 0.0, 0.0, 1.0, 1.0, 1.0]];
        let bottom_slab: Shape = &[[0.0, 0.0, 0.0, 1.0, 0.5, 1.0]];
        let halves: Shape = &[
            [0.0, 0.0, 0.0, 0.5, 1.0, 1.0],
            [0.5, 0.0, 0.0, 1.0, 1.0, 1.0],
        ];
        let tall: Shape = &[[0.0, 0.0, 0.0, 1.0, 1.5, 1.0]];
        for direction in Direction::ALL {
            assert!(is_face_full(full, direction));
            assert!(is_face_full(halves, direction));
            assert!(!is_face_full(&[], direction));
        }
        assert!(is_face_full(bottom_slab, Direction::Down));
        assert!(!is_face_full(bottom_slab, Direction::Up));
        assert!(!is_face_full(bottom_slab, Direction::North));
        assert!(!is_face_full(tall, Direction::North));
        assert!(is_face_full(tall, Direction::Down));
    }

    #[test]
    fn fluid_states() {
        let lava = parse_fluid_state(&serde_json::json!({
            "id": "minecraft:lava",
            "properties": {"falling": "true"}
        }))
        .expect("lava");
        assert!(is_lava(lava) && is_source(lava) && lava.info().falling);
        assert_eq!(lava.block_state(), ids::LAVA.default_state());
        let water =
            parse_fluid_state(&serde_json::json!({"Name": "minecraft:water"})).expect("water");
        assert!(is_still_water(water) && !water.info().falling);
    }

    #[test]
    fn block_states() {
        let state = parse_block_state(&serde_json::json!({
            "id": "minecraft:lava",
            "properties": {"level": "0"}
        }))
        .expect("lava");
        assert_eq!(state, ids::LAVA.default_state());
        assert_eq!(
            parse_block_state(&serde_json::json!("stone")).expect("stone"),
            ids::STONE.default_state()
        );
    }
}

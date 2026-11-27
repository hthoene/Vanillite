//! Blocks that react to light, cold and time: ice and snow melting, water freezing in cold
//! biomes (`ServerLevel.tickPrecipitation`), lit redstone ore going dark and copper weathering
//! (`ChangeOverTimeBlock`).

use std::sync::OnceLock;

use vanillite_abi::event::{BLOCK_FADE, BLOCK_FORM};
use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Direction};
use vanillite_chunk::HeightmapKind;
use vanillite_data::block::{BLOCK_COUNT, BlockId, BlockState, ids};
use vanillite_worldgen::behaviour::{PosExt, StateExt, with_properties_of};
use vanillite_worldgen::biome::{BiomeManager, BiomeRegistry};

use super::hook_events as he;
use super::{drops, fluid, is_liquid};
use crate::level::{Level, update};

/// The biomes of a world as far as the simulation needs them: temperatures for freezing.
#[derive(Debug)]
pub struct Climate {
    /// The world's biomes by protocol id (the ids its chunks store).
    pub biomes: BiomeRegistry,
    /// `BiomeManager` of the world seed.
    pub manager: BiomeManager,
    /// The dimension's sea level.
    pub sea_level: i32,
}

/// `Biome.Precipitation`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Precipitation {
    None,
    Rain,
    Snow,
}

/// `Biome.getPrecipitationAt(pos, seaLevel)` of the biome at `biome_pos`.
pub fn biome_precipitation(level: &Level, biome_pos: BlockPos, pos: BlockPos) -> Precipitation {
    let Some(climate) = level.climate() else {
        return Precipitation::Rain;
    };
    let data = climate.biomes.get(level.biome(biome_pos));
    if !data.has_precipitation {
        Precipitation::None
    } else if data.warm_enough_to_rain(pos.x, pos.y, pos.z, climate.sea_level) {
        Precipitation::Rain
    } else {
        Precipitation::Snow
    }
}

/// `ServerLevel.tickPrecipitation(pos)`: water at the surface of cold biomes freezes; while it
/// rains, snow layers grow in cold biomes and blocks below the surface get the precipitation
/// (`handlePrecipitation`: cauldrons fill).
pub fn tick_precipitation(level: &mut Level, pos: BlockPos) {
    let top = BlockPos::new(
        pos.x,
        level.heightmap_y(HeightmapKind::MotionBlocking, pos),
        pos.z,
    );
    let below = top.below();
    if should_freeze(level, top, below) {
        he::set_block_and_update(level, BLOCK_FORM, below, ids::ICE.default_state());
    }
    if !level.is_raining() {
        return;
    }
    let max_height = level.settings.max_snow_accumulation_height;
    if max_height > 0 && should_snow(level, top) {
        let state = level.block(top);
        if state.is(ids::SNOW) {
            let layers: i32 = state
                .value("layers")
                .and_then(|v| v.parse().ok())
                .unwrap_or(1);
            if layers < max_height.min(8) {
                // `Block.pushEntitiesUp` moves entities in the way; scenario worlds have none.
                let grown = state
                    .with("layers", &(layers + 1).to_string())
                    .unwrap_or(state);
                he::set_block_and_update(level, BLOCK_FORM, top, grown);
            }
        } else {
            he::set_block_and_update(level, BLOCK_FORM, top, ids::SNOW.default_state());
        }
    }
    let precipitation = biome_precipitation(level, top, below);
    if precipitation != Precipitation::None {
        let state = level.block(below);
        handle_precipitation(level, state, below, precipitation);
    }
}

/// `Biome.shouldSnow(level, pos)`.
fn should_snow(level: &Level, pos: BlockPos) -> bool {
    if biome_precipitation(level, pos, pos) != Precipitation::Snow {
        return false;
    }
    if !level.in_height(pos.y) || level.light(pos).1 >= 10 {
        return false;
    }
    let state = level.block(pos);
    (state.is_air() || state.is(ids::SNOW)) && level.can_survive(ids::SNOW.default_state(), pos)
}

/// `CauldronBlock.shouldHandlePrecipitation`.
fn cauldron_fills(level: &mut Level, precipitation: Precipitation) -> bool {
    match precipitation {
        Precipitation::Rain => level.random().next_float() < 0.05,
        Precipitation::Snow => level.random().next_float() < 0.1,
        Precipitation::None => false,
    }
}

/// `Block.handlePrecipitation`: cauldrons fill with water or powder snow.
fn handle_precipitation(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    precipitation: Precipitation,
) {
    let block_change = crate::level::game_event::events().block_change;
    if state.is(ids::CAULDRON) {
        if cauldron_fills(level, precipitation) {
            let filled = if precipitation == Precipitation::Rain {
                ids::WATER_CAULDRON
            } else {
                ids::POWDER_SNOW_CAULDRON
            };
            level.set_block_and_update(pos, filled.default_state());
            level.game_event_at(None, block_change, pos);
        }
    } else if state.is(ids::WATER_CAULDRON) || state.is(ids::POWDER_SNOW_CAULDRON) {
        let kind = if state.is(ids::WATER_CAULDRON) {
            Precipitation::Rain
        } else {
            Precipitation::Snow
        };
        let fill: i32 = state
            .value("level")
            .and_then(|v| v.parse().ok())
            .unwrap_or(3);
        if cauldron_fills(level, precipitation) && fill != 3 && precipitation == kind {
            let new = state
                .with("level", &(fill + 1).to_string())
                .unwrap_or(state);
            level.set_block_and_update(pos, new);
            level.game_event_block(
                block_change,
                pos,
                crate::level::game_event::GameEventContext::with_state(None, new),
            );
        }
    }
}

/// `Biome.shouldFreeze(level, pos)` with the biome at `biome_pos`.
fn should_freeze(level: &Level, biome_pos: BlockPos, pos: BlockPos) -> bool {
    let Some(climate) = level.climate() else {
        return false;
    };
    let biome = level.biome(biome_pos);
    let data = climate.biomes.get(biome);
    if data.warm_enough_to_rain(pos.x, pos.y, pos.z, climate.sea_level) {
        return false;
    }
    if !level.in_height(pos.y) || level.light(pos).1 >= 10 {
        return false;
    }
    let state = level.block(pos);
    if state.fluid_state().info().fluid != fluid::WATER || !is_liquid(state) {
        return false;
    }
    let water = |p: BlockPos| fluid::is_water(level.fluid(p));
    let surrounded = water(pos.relative(Direction::West))
        && water(pos.relative(Direction::East))
        && water(pos.relative(Direction::North))
        && water(pos.relative(Direction::South));
    !surrounded
}

/// `IceBlock.randomTick` (frosted ice too): melts next to bright block light.
pub(super) fn ice_random_tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    if level.light(pos).1 > 11 - i32::from(state.light_dampening()) {
        melt(level, state, pos);
    }
}

/// `IceBlock.melt`: water, or nothing where water evaporates. Plugins may cancel it
/// (`BLOCK_FADE`); `false` then.
fn melt(level: &mut Level, state: BlockState, pos: BlockPos) -> bool {
    let melted = if level.settings.water_evaporates {
        BlockState::AIR
    } else {
        ids::WATER.default_state()
    };
    if !he::allows(level, BLOCK_FADE, pos, melted) {
        return false;
    }
    if level.settings.water_evaporates {
        level.remove_block(pos, false);
    } else {
        level.set_block_and_update(pos, ids::WATER.default_state());
        level.neighbor_changed(pos, ids::WATER);
        level.game_event_block(
            crate::level::game_event::events().block_destroy,
            pos,
            crate::level::game_event::GameEventContext::with_state(None, state),
        );
    }
    true
}

/// `Mth.nextInt(random, min, max)` on the level random.
fn next_int_between(level: &mut Level, min: i32, max: i32) -> i32 {
    if min >= max {
        min
    } else {
        level.random().next_int_bounded(max - min + 1) + min
    }
}

/// `FrostedIceBlock.onPlace`.
pub(super) fn frosted_ice_on_place(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    _old: BlockState,
    _moved: bool,
) {
    let delay = next_int_between(level, 60, 120);
    level.schedule_block_tick(pos, state.block(), delay);
}

/// `FrostedIceBlock.tick`: ages and melts in bright light, faster with few frosted neighbours.
pub(super) fn frosted_ice_tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    let block = state.block();
    if level.random().next_int_bounded(3) == 0 || fewer_frosted_neighbours(level, pos, block, 4) {
        // `getMaxLocalRawBrightness`; the End would use block light (no frosted ice there).
        let brightness = level.max_local_raw_brightness(pos);
        let age = state.int("age").unwrap_or(0);
        if brightness > 11 - age - i32::from(state.light_dampening())
            && slightly_melt(level, state, pos)
        {
            for direction in Direction::ALL {
                let neighbour_pos = pos.relative(direction);
                let neighbour = level.block(neighbour_pos);
                if neighbour.is(block) && !slightly_melt(level, neighbour, neighbour_pos) {
                    let delay = next_int_between(level, 20, 40);
                    level.schedule_block_tick(neighbour_pos, block, delay);
                }
            }
            return;
        }
    }
    let delay = next_int_between(level, 20, 40);
    level.schedule_block_tick(pos, block, delay);
}

/// `FrostedIceBlock.slightlyMelt`: one age older, or melted at age 3.
fn slightly_melt(level: &mut Level, state: BlockState, pos: BlockPos) -> bool {
    let age = state.int("age").unwrap_or(0);
    if age < 3 {
        level.set_block(pos, state.set_int("age", age + 1), update::CLIENTS);
        false
    } else {
        melt(level, state, pos)
    }
}

/// `FrostedIceBlock.neighborChanged`: melts once fewer than two frosted neighbours are left.
pub(super) fn frosted_ice_neighbor_changed(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    source: BlockId,
    _moved: bool,
) {
    if source == state.block() && fewer_frosted_neighbours(level, pos, source, 2) {
        melt(level, state, pos);
    }
}

/// `FrostedIceBlock.fewerNeigboursThan`.
fn fewer_frosted_neighbours(level: &Level, pos: BlockPos, block: BlockId, limit: i32) -> bool {
    let mut count = 0;
    for direction in Direction::ALL {
        if level.block(pos.relative(direction)).is(block) {
            count += 1;
            if count >= limit {
                return false;
            }
        }
    }
    true
}

/// `SnowLayerBlock.randomTick`: melts next to bright block light.
pub(super) fn snow_random_tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    if level.light(pos).1 > 11 && he::allows(level, BLOCK_FADE, pos, BlockState::AIR) {
        drops::drop_resources(level, pos, state, None);
        level.remove_block(pos, false);
    }
}

/// `RedStoneOreBlock.interact` (`attack`, `stepOn`, `useItemOn`): the particles draw from the
/// level random for every side that is not a solid render block, then the ore lights up.
pub fn redstone_ore_interact(level: &mut Level, state: BlockState, pos: BlockPos) {
    use vanillite_base::Direction;
    use vanillite_base::random::RandomSource;
    for d in Direction::ALL {
        if !level
            .block(pos.relative(d))
            .has(vanillite_data::block::flags::SOLID_RENDER)
        {
            // Two of `dx`, `dy`, `dz` are random (the axis of the side is fixed).
            level.random().next_float();
            level.random().next_float();
        }
    }
    if !state.flag("lit") {
        level.set_block_and_update(pos, state.set_flag("lit", true));
    }
}

/// `RedStoneOreBlock.attack`.
pub(super) fn redstone_ore_attack(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    _: vanillite_world::entity::EntityId,
) {
    redstone_ore_interact(level, state, pos);
}

/// `RedStoneOreBlock.randomTick`: lit ore goes dark again.
pub(super) fn redstone_ore_random_tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    if state.flag("lit") {
        level.set_block_and_update(pos, state.set_flag("lit", false));
    }
}

/// The weathering age (0 unaffected .. 3 oxidized) of a copper block that weathers, and its
/// next stage.
#[derive(Clone, Copy, Debug)]
struct Weathering {
    age: u8,
    next: Option<BlockId>,
    /// The unaffected stage (`WeatheringCopper.getFirst`).
    first: BlockId,
    /// The stage before (`getPrevious`).
    previous: Option<BlockId>,
}

fn weathering(block: BlockId) -> Option<Weathering> {
    static TABLE: OnceLock<Box<[Option<Weathering>]>> = OnceLock::new();
    TABLE.get_or_init(|| {
        (0..BLOCK_COUNT)
            .map(|id| weathering_of(BlockId(id as u16).name()))
            .collect()
    })[usize::from(block.0)]
}

/// The weathering of a block by its name: copper blocks exist as `X`, `exposed_X`,
/// `weathered_X` and `oxidized_X` (the copper block itself is `copper_block`, `exposed_copper`,
/// …); waxed blocks do not weather.
fn weathering_of(name: &str) -> Option<Weathering> {
    const PREFIXES: [&str; 4] = ["", "exposed_", "weathered_", "oxidized_"];
    let name = name.strip_prefix("minecraft:")?;
    if name.starts_with("waxed_") {
        return None;
    }
    let (age, base) = PREFIXES
        .iter()
        .enumerate()
        .skip(1)
        .find_map(|(age, prefix)| name.strip_prefix(prefix).map(|base| (age, base)))
        .unwrap_or((0, name));
    let base = if base == "copper_block" {
        "copper"
    } else {
        base
    };
    let named = |age: usize| {
        let name = match (age, base) {
            (0, "copper") => "copper_block".to_owned(),
            _ => format!("{}{base}", PREFIXES[age]),
        };
        BlockId::by_name(&format!("minecraft:{name}"))
    };
    if !(0..4).all(|age| named(age).is_some()) {
        return None;
    }
    Some(Weathering {
        age: age as u8,
        next: (age < 3).then(|| named(age + 1)).flatten(),
        first: named(0)?,
        previous: (age > 0).then(|| named(age - 1)).flatten(),
    })
}

/// `WeatheringCopper.getPrevious`: the block one weathering age younger.
pub fn previous_weathering(block: BlockId) -> Option<BlockId> {
    weathering(block)?.previous
}

/// Whether a block is weathering copper of any age (`instanceof WeatheringCopper`).
pub fn is_weathering_copper(block: BlockId) -> bool {
    weathering(block).is_some()
}

/// Whether a block is waxed copper (`HoneycombItem.WAX_OFF_BY_BLOCK` has it).
pub fn is_waxed_copper(block: BlockId) -> bool {
    static TABLE: OnceLock<Box<[bool]>> = OnceLock::new();
    TABLE.get_or_init(|| {
        (0..BLOCK_COUNT)
            .map(|id| {
                BlockId(id as u16)
                    .name()
                    .strip_prefix("minecraft:waxed_")
                    .and_then(|base| BlockId::by_name(&format!("minecraft:{base}")))
                    .is_some_and(is_weathering_copper)
            })
            .collect()
    })[usize::from(block.0)]
}

/// `WeatheringCopper.getFirst(state)`: the unaffected stage with the same properties.
pub fn copper_first(state: BlockState) -> Option<BlockState> {
    weathering(state.block()).map(|w| with_properties_of(w.first, state))
}

/// `WeatheringCopper.getPrevious(state)`: the stage before with the same properties.
pub fn copper_previous(state: BlockState) -> Option<BlockState> {
    weathering(state.block())
        .and_then(|w| w.previous)
        .map(|b| with_properties_of(b, state))
}

/// Whether a block weathers (`WeatheringCopper` that is not oxidized yet).
pub(super) fn weathers(block: BlockId) -> bool {
    weathering(block).is_some_and(|w| w.next.is_some()) && block.default_state().random_ticks()
}

/// `ChangeOverTimeBlock.changeOverTime` of weathering copper.
pub(super) fn copper_random_tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    // Doors weather from their lower half only.
    if state.value("half") == Some("upper") {
        return;
    }
    if level.random().next_float() >= 0.056_888_89 {
        return;
    }
    let Some(own) = weathering(state.block()) else {
        return;
    };
    let Some(next) = own.next else {
        return;
    };
    // `getNextState`: neighbours within Manhattan distance 4.
    let (mut same, mut older) = (0, 0);
    for dy in -4i32..=4 {
        for dz in -4i32..=4 {
            for dx in -4i32..=4 {
                if dx.abs() + dy.abs() + dz.abs() > 4 || (dx, dy, dz) == (0, 0, 0) {
                    continue;
                }
                let Some(other) = weathering(level.block(pos.offset(dx, dy, dz)).block()) else {
                    continue;
                };
                if other.age < own.age {
                    return;
                }
                if other.age > own.age {
                    older += 1;
                } else {
                    same += 1;
                }
            }
        }
    }
    let chance = (older + 1) as f32 / (older + same + 1) as f32;
    let modifier = if own.age == 0 { 0.75 } else { 1.0 };
    if level.random().next_float() < chance * chance * modifier {
        let weathered = with_properties_of(next, state);
        he::set_block_and_update(level, BLOCK_FORM, pos, weathered);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copper_weathers_in_four_ages() {
        let next = |name: &str| weathering_of(name).and_then(|w| w.next).map(|b| b.name());
        assert_eq!(
            next("minecraft:copper_block"),
            Some("minecraft:exposed_copper")
        );
        assert_eq!(
            next("minecraft:exposed_copper"),
            Some("minecraft:weathered_copper")
        );
        assert_eq!(
            next("minecraft:cut_copper_slab"),
            Some("minecraft:exposed_cut_copper_slab")
        );
        assert_eq!(next("minecraft:oxidized_copper"), None);
        assert!(weathering_of("minecraft:oxidized_copper").is_some());
        assert!(weathering_of("minecraft:waxed_copper_block").is_none());
        assert!(weathering_of("minecraft:copper_ore").is_none());
    }
}

//! Blocks that react to light, cold and time: ice and snow melting, water freezing in cold
//! biomes (`ServerLevel.tickPrecipitation`), lit redstone ore going dark and copper weathering
//! (`ChangeOverTimeBlock`).

use std::sync::OnceLock;

use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Direction};
use vanillite_chunk::HeightmapKind;
use vanillite_data::block::{BLOCK_COUNT, BlockId, BlockState, ids};
use vanillite_worldgen::behaviour::{PosExt, StateExt, with_properties_of};
use vanillite_worldgen::biome::{BiomeManager, BiomeRegistry};

use super::{drops, fluid, is_liquid};
use crate::level::Level;

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

/// `ServerLevel.tickPrecipitation(pos)`: water at the surface of cold biomes freezes. Snow
/// layers and cauldrons fill only while it rains; there is no weather yet.
pub fn tick_precipitation(level: &mut Level, pos: BlockPos) {
    let Some(top_y) = level.chunks.chunk(pos.chunk()).and_then(|c| {
        let map = c.chunk.heightmaps().get(HeightmapKind::MotionBlocking)?;
        let relative = map.get((pos.x & 15) as u32, (pos.z & 15) as u32);
        Some(c.chunk.min_y() + relative as i32)
    }) else {
        return;
    };
    let top = BlockPos::new(pos.x, top_y, pos.z);
    let below = top.below();
    if should_freeze(level, top, below) {
        level.set_block_and_update(below, ids::ICE.default_state());
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

/// `IceBlock.randomTick`: melts next to bright block light.
pub(super) fn ice_random_tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    if level.light(pos).1 > 11 - i32::from(state.light_dampening()) {
        // `IceBlock.melt`; dimensions where water evaporates have no natural ice light.
        if level.settings.water_evaporates {
            level.remove_block(pos, false);
        } else {
            level.set_block_and_update(pos, ids::WATER.default_state());
            level.neighbor_changed(pos, ids::WATER);
        }
    }
}

/// `SnowLayerBlock.randomTick`: melts next to bright block light.
pub(super) fn snow_random_tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    if level.light(pos).1 > 11 {
        drops::drop_resources(level, pos, state, None);
        level.remove_block(pos, false);
    }
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
    let base = if base == "copper_block" { "copper" } else { base };
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
    })
}

/// Whether a block weathers (`WeatheringCopper` that is not oxidized yet).
pub(super) fn weathers(block: BlockId) -> bool {
    weathering(block).is_some_and(|w| w.next.is_some())
        && block.default_state().random_ticks()
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
        level.set_block_and_update(pos, weathered);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copper_weathers_in_four_ages() {
        let next = |name: &str| {
            weathering_of(name)
                .and_then(|w| w.next)
                .map(|b| b.name())
        };
        assert_eq!(next("minecraft:copper_block"), Some("minecraft:exposed_copper"));
        assert_eq!(next("minecraft:exposed_copper"), Some("minecraft:weathered_copper"));
        assert_eq!(next("minecraft:cut_copper_slab"), Some("minecraft:exposed_cut_copper_slab"));
        assert_eq!(next("minecraft:oxidized_copper"), None);
        assert!(weathering_of("minecraft:oxidized_copper").is_some());
        assert!(weathering_of("minecraft:waxed_copper_block").is_none());
        assert!(weathering_of("minecraft:copper_ore").is_none());
    }
}

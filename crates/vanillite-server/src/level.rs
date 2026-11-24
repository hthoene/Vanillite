//! The server's level data that all worlds share: the game time (`level.dat` `Time`), the world
//! clocks, the game rules, the weather and the difficulty, with the registries survival needs
//! (damage types, block facts). Saved like Vanilla's `SavedData` in
//! `<world>/data/minecraft/{world_clocks,game_rules,weather}.dat`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

use serde_json::Value;
use vanillite_datapack::LoadedPacks;
use vanillite_game::survival::blocks::BlockFacts;
use vanillite_game::survival::clock::{Clock, Clocks, TimeMarker, TimelineInfo};
use vanillite_game::survival::damage::{
    DamageType, DamageTypes, DeathMessageType, Effects, Scaling, tag,
};
use vanillite_game::survival::rules::{Difficulty, GameRules};
use vanillite_game::survival::weather::WeatherData;
use vanillite_nbt::{Compound, Tag};

use crate::server::DIMENSIONS;

const WORLD_CLOCKS: &str = "minecraft:world_clocks";
const GAME_RULES: &str = "minecraft:game_rules";
const WEATHER: &str = "minecraft:weather";

/// Shared level data. Each lock is held only for short reads or a command's change.
#[derive(Debug)]
pub struct LevelState {
    /// `level.dat` `Time`: advanced by the overworld's tick.
    pub game_time: AtomicI64,
    clocks: Mutex<Clocks>,
    rules: RwLock<GameRules>,
    weather: Mutex<WeatherData>,
    /// The difficulty of `server.properties`.
    pub difficulty: Difficulty,
    pub damage_types: DamageTypes,
    pub block_facts: BlockFacts,
    /// The default clock of each dimension (`DimensionType.defaultClock`), in [`DIMENSIONS`]
    /// order.
    pub default_clocks: [Option<usize>; 3],
    dir: PathBuf,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl LevelState {
    /// Loads the level data of `level_dir` with the registries of `packs`.
    pub(crate) fn load(
        level_dir: &Path,
        packs: Option<&LoadedPacks>,
        difficulty: Difficulty,
    ) -> LevelState {
        let level_dat = vanillite_storage::read_level_dat(level_dir)
            .unwrap_or_else(|e| {
                log::error!("Cannot read level.dat: {e}");
                None
            })
            .and_then(|mut root| match root.remove("Data") {
                Some(Tag::Compound(data)) => Some(data),
                _ => None,
            })
            .unwrap_or_default();
        let read = |id: &str| {
            vanillite_storage::read_saved_data(level_dir, id).unwrap_or_else(|e| {
                log::error!("Cannot read {id}: {e}");
                None
            })
        };
        let game_time = level_dat.get("Time").and_then(Tag::as_i64).unwrap_or(0);

        let mut clocks = packs.map(load_clocks).unwrap_or_default();
        match read(WORLD_CLOCKS) {
            Some(data) => clocks.load(&data),
            // Older worlds keep the day time in `level.dat` (`DayTimeToClockFix`).
            None => {
                if let (Some(day), Some(i)) = (
                    level_dat.get("DayTime").and_then(Tag::as_i64),
                    clocks.index("minecraft:overworld"),
                ) {
                    clocks.set_total_ticks(i, day);
                }
            }
        }
        let rules = read(GAME_RULES)
            .map(|data| GameRules::from_nbt(&data))
            .unwrap_or_default();
        let weather = match read(WEATHER) {
            Some(data) => WeatherData::load(&data),
            None => WeatherData::load(&legacy_weather(&level_dat)),
        };
        let default_clocks = std::array::from_fn(|i| {
            let dimension = DIMENSIONS.get(i)?;
            let json = packs?
                .registry("minecraft:dimension_type")?
                .get(dimension)?
                .json
                .get();
            let value: Value = serde_json::from_str(json).ok()?;
            clocks.index(value.get("default_clock")?.as_str()?)
        });
        let (damage_types, block_facts) = match packs {
            Some(packs) => (load_damage_types(packs), load_block_facts(packs)),
            None => (DamageTypes::built_in(), BlockFacts::without_tags()),
        };
        LevelState {
            game_time: AtomicI64::new(game_time),
            clocks: Mutex::new(clocks),
            rules: RwLock::new(rules),
            weather: Mutex::new(weather),
            difficulty,
            damage_types,
            block_facts,
            default_clocks,
            dir: level_dir.to_owned(),
        }
    }

    /// The game time.
    pub fn game_time(&self) -> i64 {
        self.game_time.load(Ordering::Relaxed)
    }

    /// The clocks.
    pub fn clocks(&self) -> MutexGuard<'_, Clocks> {
        lock(&self.clocks)
    }

    /// The weather timers.
    pub fn weather(&self) -> MutexGuard<'_, WeatherData> {
        lock(&self.weather)
    }

    /// The game rules.
    pub fn rules(&self) -> RwLockReadGuard<'_, GameRules> {
        self.rules.read().unwrap_or_else(PoisonError::into_inner)
    }

    /// The game rules, to change them.
    pub fn rules_mut(&self) -> RwLockWriteGuard<'_, GameRules> {
        self.rules.write().unwrap_or_else(PoisonError::into_inner)
    }

    /// Saves the clocks, rules, weather and the game time.
    pub(crate) fn save(&self) {
        let clocks = self.clocks().save();
        let rules = self.rules().to_nbt();
        let weather = self.weather().save();
        for (id, data) in [
            (WORLD_CLOCKS, clocks),
            (GAME_RULES, rules),
            (WEATHER, weather),
        ] {
            if let Err(e) = vanillite_storage::write_saved_data(&self.dir, id, data) {
                log::error!("Cannot save {id}: {e}");
            }
        }
        let mut root = vanillite_storage::read_level_dat(&self.dir)
            .ok()
            .flatten()
            .unwrap_or_default();
        let mut data = match root.remove("Data") {
            Some(Tag::Compound(data)) => data,
            _ => Compound::new(),
        };
        data.insert("Time", Tag::Long(self.game_time()));
        root.insert("Data", Tag::Compound(data));
        if let Err(e) = vanillite_storage::write_level_dat(&self.dir, &root) {
            log::error!("Cannot save level.dat: {e}");
        }
    }
}

/// The weather fields of an older `level.dat`, as the `weather.dat` compound.
fn legacy_weather(level_dat: &Compound) -> Compound {
    let mut out = Compound::new();
    for (old, new) in [
        ("clearWeatherTime", "clear_weather_time"),
        ("rainTime", "rain_time"),
        ("thunderTime", "thunder_time"),
        ("raining", "raining"),
        ("thundering", "thundering"),
    ] {
        if let Some(v) = level_dat.get(old) {
            out.insert(new, v.clone());
        }
    }
    out
}

fn json_of(packs: &LoadedPacks, registry: &str) -> Vec<(String, Value)> {
    packs
        .registry(registry)
        .map(|r| {
            r.entries()
                .iter()
                .filter_map(|e| {
                    serde_json::from_str(e.json.get())
                        .map_err(|err| log::error!("{registry} {}: {err}", e.id))
                        .ok()
                        .map(|v| (e.id.clone(), v))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The clocks of `minecraft:world_clock` with the time markers of the timelines.
fn load_clocks(packs: &LoadedPacks) -> Clocks {
    let mut clocks = Clocks {
        clocks: packs
            .registry("minecraft:world_clock")
            .map(|r| {
                r.entries()
                    .iter()
                    .map(|e| Clock::new(e.id.clone()))
                    .collect()
            })
            .unwrap_or_default(),
        timelines: Vec::new(),
    };
    for (id, json) in json_of(packs, "minecraft:timeline") {
        let Some(clock) = json
            .get("clock")
            .and_then(Value::as_str)
            .and_then(|c| clocks.index(c))
        else {
            continue;
        };
        let period = json
            .get("period_ticks")
            .and_then(Value::as_i64)
            .and_then(|p| i32::try_from(p).ok());
        if let Some(Value::Object(markers)) = json.get("time_markers") {
            for (marker, value) in markers {
                let (ticks, show) = match value {
                    Value::Number(n) => (n.as_i64(), false),
                    Value::Object(o) => (
                        o.get("ticks").and_then(Value::as_i64),
                        o.get("show_in_commands")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    ),
                    _ => (None, false),
                };
                let Some(ticks) = ticks.and_then(|t| i32::try_from(t).ok()) else {
                    continue;
                };
                if let Some(c) = clocks.clocks.get_mut(clock) {
                    c.markers.push(TimeMarker {
                        id: marker.clone(),
                        ticks,
                        period,
                        show_in_commands: show,
                    });
                }
            }
        }
        clocks.timelines.push(TimelineInfo { id, clock, period });
    }
    clocks
}

/// `minecraft:damage_type` with the tags survival checks.
fn load_damage_types(packs: &LoadedPacks) -> DamageTypes {
    let tags = packs.tags("minecraft:damage_type");
    let types = json_of(packs, "minecraft:damage_type")
        .into_iter()
        .map(|(id, json)| {
            let s = |k: &str| json.get(k).and_then(Value::as_str).unwrap_or("");
            let mut bits = 0;
            if let Some(tags) = tags {
                for (name, bit) in tag::NAMES {
                    if tags.get(name).is_some_and(|members| members.contains(&id)) {
                        bits |= bit;
                    }
                }
            }
            #[allow(clippy::cast_possible_truncation, clippy::as_conversions)]
            let exhaustion = json
                .get("exhaustion")
                .and_then(Value::as_f64)
                .unwrap_or(0.0) as f32;
            DamageType {
                msg_id: s("message_id").to_owned(),
                scaling: match s("scaling") {
                    "never" => Scaling::Never,
                    "always" => Scaling::Always,
                    _ => Scaling::WhenCausedByLivingNonPlayer,
                },
                exhaustion,
                effects: match s("effects") {
                    "thorns" => Effects::Thorns,
                    "drowning" => Effects::Drowning,
                    "burning" => Effects::Burning,
                    "poking" => Effects::Poking,
                    "freezing" => Effects::Freezing,
                    _ => Effects::Hurt,
                },
                death_message_type: match s("death_message_type") {
                    "fall_variants" => DeathMessageType::FallVariants,
                    "intentional_game_design" => DeathMessageType::IntentionalGameDesign,
                    _ => DeathMessageType::Default,
                },
                id,
                tags: bits,
            }
        })
        .collect();
    DamageTypes::new(types)
}

/// Block tags and fluid ids for survival.
fn load_block_facts(packs: &LoadedPacks) -> BlockFacts {
    let tag = |name: &str| {
        packs.tag_ids("minecraft:block", name).unwrap_or_else(|e| {
            log::error!("Block tag {name}: {e:?}");
            Vec::new()
        })
    };
    let fluid = |name: &str| vanillite_data::registry::FLUID.id(name).unwrap_or(u16::MAX);
    BlockFacts::new(
        &tag("minecraft:causes_suffocation"),
        &tag("minecraft:climbable"),
        &tag("minecraft:trapdoors"),
        [fluid("minecraft:water"), fluid("minecraft:flowing_water")],
        [fluid("minecraft:lava"), fluid("minecraft:flowing_lava")],
    )
}

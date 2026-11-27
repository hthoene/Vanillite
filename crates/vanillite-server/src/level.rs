//! The server's level data that all worlds share: the game time (`level.dat` `Time`), the world
//! clocks, the game rules, the weather and the difficulty, with the registries survival needs
//! (damage types, block facts). Saved like Vanilla 26.3: `level.dat` and the saved data
//! `<world>/data/minecraft/{world_clocks,game_rules,weather}.dat`; other world-wide saved data
//! is kept untouched until its system registers in [`LevelState::data`].

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
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
use vanillite_storage::level_dat::{LevelData, Spawn};
use vanillite_storage::saved_data::SavedDataStore;
use vanillite_storage::writer::FileWriter;

const WORLD_CLOCKS: &str = "minecraft:world_clocks";
const GAME_RULES: &str = "minecraft:game_rules";
const WEATHER: &str = "minecraft:weather";

/// The server brand recorded in `level.dat` (`ServerBrands`).
const BRAND: &str = "vanillite";

/// What `level.dat` gets from the server's settings.
#[derive(Clone, Debug)]
pub(crate) struct LevelSetup {
    /// `level-name`.
    pub(crate) level_name: String,
    /// `gamemode`: the default game mode id.
    pub(crate) game_type: i32,
    /// `hardcore`, for a new world.
    pub(crate) hardcore: bool,
    /// The spawn the server uses when `level.dat` has none.
    pub(crate) spawn: (vanillite_base::Vec3, f32),
}

/// What the server needs of a dimension type beyond the world's height and sky light.
#[derive(Clone, Copy, Debug)]
pub struct DimensionFacts {
    /// `coordinate_scale`: nether portals scale positions by the ratio of two of these.
    pub coordinate_scale: f64,
    /// `logical_height`: exit portals are built below it.
    pub logical_height: i32,
    /// `has_ceiling`: maps of such dimensions look below the roof.
    pub has_ceiling: bool,
    /// `has_ender_dragon_fight`: the world keeps an `EndDragonFight`.
    pub has_ender_dragon_fight: bool,
}

/// Shared level data. Each lock is held only for short reads or a command's change.
#[derive(Debug)]
pub struct LevelState {
    /// `level.dat` `Time`: advanced by the overworld's tick.
    pub game_time: AtomicI64,
    clocks: Mutex<Clocks>,
    rules: RwLock<GameRules>,
    weather: Mutex<WeatherData>,
    /// `Difficulty.getId` of the difficulty: `server.properties`', then `/difficulty`'s.
    difficulty: std::sync::atomic::AtomicI32,
    pub damage_types: DamageTypes,
    pub block_facts: BlockFacts,
    /// The default clock of each world's dimension type (`DimensionType.defaultClock`), by
    /// world index.
    pub default_clocks: Vec<Option<usize>>,
    /// What the server needs of each world's dimension type, by world index.
    pub dimension_types: Vec<DimensionFacts>,
    /// `level.dat`.
    level_data: Mutex<LevelData>,
    /// The world-wide saved data (`<world>/data/`).
    pub(crate) data: SavedDataStore,
    /// The maps of all worlds (`data/minecraft/maps/`).
    pub(crate) maps: std::sync::Arc<vanillite_game::map::Maps>,
    /// The files of clocks, rules and weather as they were read: fields this server does not
    /// know (rules of other servers, clocks of removed data packs) are written back.
    loaded: [Compound; 3],
    /// `false` after `/save-off`: autosaves skip the worlds' chunks (Vanilla's `noSave`).
    pub(crate) auto_save: AtomicBool,
    writer: FileWriter,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl LevelState {
    /// Loads the level data of `level_dir` with the registries of `packs`.
    pub(crate) fn load(
        level_dir: &Path,
        packs: Option<&LoadedPacks>,
        dimensions: &crate::dimensions::Dimensions,
        difficulty: Difficulty,
        setup: &LevelSetup,
        writer: FileWriter,
    ) -> LevelState {
        let mut level_data = LevelData::read(level_dir)
            .unwrap_or_else(|e| {
                log::error!("Cannot read level.dat, starting with a new one: {e}");
                None
            })
            .unwrap_or_else(|| {
                let mut new = LevelData::new(&setup.level_name);
                new.hardcore = setup.hardcore;
                new
            });
        level_data.add_brand(BRAND, true);
        level_data.game_type = setup.game_type;
        // Like `DedicatedServer.forceDifficulty`: `server.properties` decides.
        level_data.difficulty = difficulty_name(difficulty).to_owned();
        if level_data.spawn.is_none() {
            let (pos, yaw) = setup.spawn;
            #[allow(clippy::cast_possible_truncation, clippy::as_conversions)]
            let block = [pos.x, pos.y, pos.z].map(|v| v.floor() as i32);
            level_data.spawn = Some(Spawn {
                dimension: "minecraft:overworld".to_owned(),
                pos: block,
                yaw,
                pitch: 0.0,
            });
        }
        let data = SavedDataStore::new(level_dir, writer.clone());
        let read = |id: &str| {
            data.read(id).unwrap_or_else(|e| {
                log::error!("Cannot read {id}: {e}");
                None
            })
        };
        let game_time = level_data.game_time;
        let legacy = &level_data.other;

        let mut clocks = packs.map(load_clocks).unwrap_or_default();
        let clocks_data = read(WORLD_CLOCKS);
        match &clocks_data {
            Some(data) => clocks.load(data),
            // Older worlds keep the day time in `level.dat` (`DayTimeToClockFix`).
            None => {
                if let (Some(day), Some(i)) = (
                    legacy.get("DayTime").and_then(Tag::as_i64),
                    clocks.index("minecraft:overworld"),
                ) {
                    clocks.set_total_ticks(i, day);
                }
            }
        }
        let rules_data = read(GAME_RULES);
        let rules = rules_data
            .as_ref()
            .map(GameRules::from_nbt)
            .unwrap_or_default();
        let weather_data = read(WEATHER);
        let weather = match &weather_data {
            Some(data) => WeatherData::load(data),
            None => WeatherData::load(&legacy_weather(legacy)),
        };
        let type_json = |world| -> Option<Value> {
            let json = packs?
                .registry("minecraft:dimension_type")?
                .get(dimensions.kind(world)?)?
                .json
                .get();
            serde_json::from_str(json).ok()
        };
        let default_clocks = dimensions
            .worlds()
            .map(|world| clocks.index(type_json(world)?.get("default_clock")?.as_str()?))
            .collect();
        let dimension_types = dimensions
            .worlds()
            .map(|world| {
                let json = type_json(world);
                let field = |key: &str| json.as_ref().and_then(|v| v.get(key));
                let flag = |key: &str| field(key).and_then(Value::as_bool).unwrap_or(false);
                DimensionFacts {
                    coordinate_scale: field("coordinate_scale")
                        .and_then(Value::as_f64)
                        .unwrap_or(1.0),
                    logical_height: field("logical_height")
                        .and_then(Value::as_i64)
                        .and_then(|v| i32::try_from(v).ok())
                        .unwrap_or(256),
                    has_ceiling: flag("has_ceiling"),
                    has_ender_dragon_fight: flag("has_ender_dragon_fight"),
                }
            })
            .collect();
        let (damage_types, block_facts) = match packs {
            Some(packs) => (load_damage_types(packs), load_block_facts(packs)),
            None => (DamageTypes::built_in(), BlockFacts::without_tags()),
        };
        LevelState {
            game_time: AtomicI64::new(game_time),
            clocks: Mutex::new(clocks),
            rules: RwLock::new(rules),
            weather: Mutex::new(weather),
            difficulty: std::sync::atomic::AtomicI32::new(difficulty.id()),
            dimension_types,
            damage_types,
            block_facts,
            default_clocks,
            level_data: Mutex::new(level_data),
            maps: {
                let dir = data.dir().to_owned();
                std::sync::Arc::new(vanillite_game::map::Maps::new(Box::new(move |id| {
                    vanillite_storage::saved_data::read_saved_data(&dir, id).unwrap_or_else(|e| {
                        log::error!("Cannot read {id}: {e}");
                        None
                    })
                })))
            },
            data,
            loaded: [clocks_data, rules_data, weather_data].map(Option::unwrap_or_default),
            auto_save: AtomicBool::new(true),
            writer,
        }
    }

    /// The game time.
    pub fn game_time(&self) -> i64 {
        self.game_time.load(Ordering::Relaxed)
    }

    /// The difficulty (`WorldData.getDifficulty`).
    pub fn difficulty(&self) -> Difficulty {
        Difficulty::from_id(self.difficulty.load(Ordering::Relaxed))
    }

    /// `MinecraftServer.setDifficulty`: kept in `level.dat`; hardcore worlds stay hard.
    pub(crate) fn set_difficulty(&self, difficulty: Difficulty) {
        let mut data = lock(&self.level_data);
        let difficulty = if data.hardcore {
            Difficulty::Hard
        } else {
            difficulty
        };
        self.difficulty.store(difficulty.id(), Ordering::Relaxed);
        data.difficulty = difficulty_name(difficulty).to_owned();
    }

    /// A field of `level.dat` the server does not model (fields of older versions).
    pub(crate) fn legacy(&self, key: &str) -> Option<Tag> {
        lock(&self.level_data).other.get(key).cloned()
    }

    /// `WorldData.isDifficultyLocked`.
    pub(crate) fn difficulty_locked(&self) -> bool {
        lock(&self.level_data).difficulty_locked
    }

    /// `MinecraftServer.setDifficultyLocked`.
    pub(crate) fn set_difficulty_locked(&self, locked: bool) {
        lock(&self.level_data).difficulty_locked = locked;
    }

    /// `level.dat`'s `GameType`: the default game mode id.
    pub(crate) fn default_game_type(&self) -> i32 {
        lock(&self.level_data).game_type
    }

    /// `setDefaultGameType`.
    pub(crate) fn set_default_game_type(&self, game_type: i32) {
        lock(&self.level_data).game_type = game_type;
    }

    /// `level.dat`'s `spawn` (`LevelData.RespawnData`).
    pub(crate) fn spawn(&self) -> Option<Spawn> {
        lock(&self.level_data).spawn.clone()
    }

    /// `setRespawnData`.
    pub(crate) fn set_spawn(&self, spawn: Spawn) {
        lock(&self.level_data).spawn = Some(spawn);
    }

    /// `getDataConfiguration().dataPacks().getDisabled()`.
    pub(crate) fn disabled_packs(&self) -> Vec<String> {
        lock(&self.level_data)
            .data_packs()
            .map(|(_, disabled)| disabled)
            .unwrap_or_default()
    }

    /// `setDataConfiguration`: `level.dat`'s `DataPacks` and, when given, `enabled_features`.
    pub(crate) fn set_data_configuration(
        &self,
        packs: &vanillite_datapack::repository::DataPackConfig,
        features: Option<&[String]>,
    ) {
        let mut data = lock(&self.level_data);
        data.set_data_packs(&packs.enabled, &packs.disabled);
        if let Some(features) = features {
            data.set_enabled_features(features);
        }
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

    /// The background writer of small files.
    pub(crate) fn writer(&self) -> &FileWriter {
        &self.writer
    }

    /// Queues the writes of `level.dat`, the clocks, rules and weather and of every
    /// registered world-wide saved data (Vanilla saves these also with `/save-off`).
    pub(crate) fn save(&self) {
        let files = [
            (WORLD_CLOCKS, self.clocks().save()),
            (GAME_RULES, self.rules().to_nbt()),
            (WEATHER, self.weather().save()),
        ];
        for ((id, data), loaded) in files.into_iter().zip(&self.loaded) {
            let mut merged = loaded.clone();
            for (key, tag) in data.iter() {
                merged.insert(key, tag.clone());
            }
            self.data.write(id, merged);
        }
        for (id, data) in self.maps.take_dirty() {
            self.data.write(&id, data);
        }
        self.data.save();
        let level_data = {
            let mut level_data = lock(&self.level_data);
            level_data.game_time = self.game_time();
            level_data.clone()
        };
        let dir = self.data.dir().to_owned();
        self.writer.submit(move || {
            if let Err(e) = level_data.write(&dir) {
                log::error!("Cannot save level.dat: {e}");
            }
        });
    }
}

/// The name of a difficulty in `level.dat`.
fn difficulty_name(difficulty: Difficulty) -> &'static str {
    match difficulty {
        Difficulty::Peaceful => "peaceful",
        Difficulty::Easy => "easy",
        Difficulty::Normal => "normal",
        Difficulty::Hard => "hard",
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
pub fn load_damage_types(packs: &LoadedPacks) -> DamageTypes {
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
pub fn load_block_facts(packs: &LoadedPacks) -> BlockFacts {
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

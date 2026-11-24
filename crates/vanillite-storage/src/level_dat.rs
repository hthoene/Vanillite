//! `level.dat` in Vanilla 26.3's format (`PrimaryLevelData`).
//!
//! 26.x keeps little in `level.dat`: spawn, game time, game mode, difficulty, data packs and
//! version info. Day time, weather, game rules, the world border, the wandering trader and the
//! world generation settings moved into saved data (`data/minecraft/*.dat`, see
//! [`saved_data`](crate::saved_data)). Fields this server does not handle are kept as they are.

use std::io;
use std::path::Path;

use vanillite_data::version::{DATA_VERSION, VERSION_ID};
use vanillite_nbt::{Compound, List, Tag};

use crate::files;

/// `version`: the NBT format of `level.dat` (Anvil).
const ANVIL_VERSION: i32 = 19133;

/// The world spawn (`LevelData.RespawnData`).
#[derive(Clone, Debug, PartialEq)]
pub struct Spawn {
    /// `dimension`, e.g. `minecraft:overworld`.
    pub dimension: String,
    /// `pos`: the block.
    pub pos: [i32; 3],
    /// `yaw`.
    pub yaw: f32,
    /// `pitch`.
    pub pitch: f32,
}

/// The contents of `level.dat`'s `Data` compound.
#[derive(Clone, Debug, PartialEq)]
pub struct LevelData {
    /// `LevelName`.
    pub level_name: String,
    /// `Time`: the game time in ticks.
    pub game_time: i64,
    /// `GameType`: the default game mode id.
    pub game_type: i32,
    /// `spawn`; `None` if the world has none yet.
    pub spawn: Option<Spawn>,
    /// `difficulty_settings.difficulty`: `peaceful`, `easy`, `normal` or `hard`.
    pub difficulty: String,
    /// `difficulty_settings.hardcore`.
    pub hardcore: bool,
    /// `difficulty_settings.locked`.
    pub difficulty_locked: bool,
    /// `allowCommands` (cheats of a singleplayer world).
    pub allow_commands: bool,
    /// `initialized`: the spawn was set up.
    pub initialized: bool,
    /// `WasModded`.
    pub was_modded: bool,
    /// `ServerBrands`: brands of every server that opened the world.
    pub server_brands: Vec<String>,
    /// `version_history`: data versions the world was saved with.
    pub version_history: Vec<i32>,
    /// `DataVersion` of the file as it was read (0 for a new world).
    pub data_version: i32,
    /// Every other field (`DataPacks`, `enabled_features`, `removed_features`,
    /// `singleplayer_uuid`, fields of other servers, ...).
    pub other: Compound,
}

impl LevelData {
    /// The data of a new world.
    pub fn new(level_name: &str) -> LevelData {
        LevelData {
            level_name: level_name.to_owned(),
            game_time: 0,
            game_type: 0,
            spawn: None,
            difficulty: "normal".to_owned(),
            hardcore: false,
            difficulty_locked: false,
            allow_commands: false,
            initialized: true,
            was_modded: false,
            server_brands: Vec::new(),
            version_history: Vec::new(),
            data_version: 0,
            other: Compound::new().with(
                "DataPacks",
                Compound::new()
                    .with("Enabled", List(vec![Tag::from("vanilla")]))
                    .with("Disabled", List(Vec::new())),
            ),
        }
    }

    /// Reads `<world_dir>/level.dat` (or `level.dat_old` if it is missing or damaged);
    /// `None` if the world has neither.
    pub fn read(world_dir: &Path) -> io::Result<Option<LevelData>> {
        let root = files::read_gzip_nbt_or_old(&world_dir.join("level.dat"))?;
        Ok(root.map(|mut root| match root.remove("Data") {
            Some(Tag::Compound(data)) => LevelData::from_nbt(data),
            _ => LevelData::from_nbt(Compound::new()),
        }))
    }

    /// Writes `<world_dir>/level.dat` like Vanilla: atomically, the previous file kept as
    /// `level.dat_old`. Sets `LastPlayed` to now.
    pub fn write(&self, world_dir: &Path) -> io::Result<()> {
        let root = Compound::new().with("Data", self.to_nbt());
        files::write_gzip_nbt(&world_dir.join("level.dat"), &root, true)
    }

    /// Records that `brand` saved the world (`setModdedInfo`).
    pub fn add_brand(&mut self, brand: &str, modded: bool) {
        if !self.server_brands.iter().any(|b| b == brand) {
            self.server_brands.push(brand.to_owned());
        }
        self.was_modded |= modded;
    }

    /// Reads the `Data` compound, including the fields of worlds before 26.1 that 26.x keeps
    /// elsewhere in `level.dat` (`SpawnX`, `Difficulty`, ...).
    pub fn from_nbt(mut data: Compound) -> LevelData {
        let mut take = |name: &str| data.remove(name);
        let int = |tag: Option<Tag>| tag.as_ref().and_then(Tag::as_i64);
        let flag = |tag: Option<Tag>, default: bool| int(tag).map_or(default, |v| v != 0);
        let string = |tag: Option<Tag>| match tag {
            Some(Tag::String(s)) => Some(s),
            _ => None,
        };
        let mut out = LevelData::new("");
        out.other = Compound::new();
        out.level_name = string(take("LevelName")).unwrap_or_default();
        out.game_time = int(take("Time")).unwrap_or(0);
        out.game_type = int(take("GameType"))
            .and_then(|v| i32::try_from(v).ok())
            .unwrap_or(0);
        out.allow_commands = flag(take("allowCommands"), out.game_type == 1);
        out.initialized = flag(take("initialized"), true);
        out.was_modded = flag(take("WasModded"), false);
        out.data_version = int(take("DataVersion"))
            .and_then(|v| i32::try_from(v).ok())
            .unwrap_or(0);
        out.server_brands = match take("ServerBrands") {
            Some(Tag::List(List(items))) => items
                .into_iter()
                .filter_map(|t| match t {
                    Tag::String(s) => Some(s),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };
        out.version_history = match take("version_history") {
            Some(Tag::List(List(items))) => items
                .iter()
                .filter_map(|t| t.as_i64().and_then(|v| i32::try_from(v).ok()))
                .collect(),
            Some(Tag::IntArray(items)) => items,
            _ => Vec::new(),
        };
        // Written anew from the current version.
        take("Version");
        take("version");
        take("LastPlayed");

        out.spawn = match take("spawn") {
            Some(Tag::Compound(spawn)) => Some(Spawn::from_nbt(&spawn)),
            _ => None,
        };
        let legacy_spawn = [take("SpawnX"), take("SpawnY"), take("SpawnZ")].map(int);
        let legacy_angle = take("SpawnAngle");
        if out.spawn.is_none()
            && let [Some(x), Some(y), Some(z)] = legacy_spawn
        {
            let angle = match legacy_angle {
                Some(Tag::Float(v)) => v,
                _ => 0.0,
            };
            out.spawn = Some(Spawn {
                dimension: "minecraft:overworld".to_owned(),
                pos: [x, y, z].map(|v| i32::try_from(v).unwrap_or(0)),
                yaw: angle,
                pitch: 0.0,
            });
        }

        let legacy_difficulty = int(take("Difficulty"));
        let legacy_locked = take("DifficultyLocked");
        let legacy_hardcore = take("hardcore");
        match take("difficulty_settings") {
            Some(Tag::Compound(settings)) => {
                if let Some(name) = settings.get("difficulty").and_then(Tag::as_str) {
                    name.clone_into(&mut out.difficulty);
                }
                let flag = |name| {
                    settings
                        .get(name)
                        .and_then(Tag::as_i64)
                        .is_some_and(|v| v != 0)
                };
                out.hardcore = flag("hardcore");
                out.difficulty_locked = flag("locked");
            }
            _ => {
                if let Some(id) = legacy_difficulty {
                    out.difficulty = match id {
                        0 => "peaceful",
                        1 => "easy",
                        3 => "hard",
                        _ => "normal",
                    }
                    .to_owned();
                }
                out.difficulty_locked = flag(legacy_locked, false);
                out.hardcore = flag(legacy_hardcore, false);
            }
        }
        out.other = data;
        out
    }

    /// The `Data` compound in 26.3's format (`PrimaryLevelData.setTagData`).
    pub fn to_nbt(&self) -> Compound {
        let mut data = self.other.clone();
        let strings = |values: &[String]| {
            List(
                values
                    .iter()
                    .map(|s| Tag::String(s.clone()))
                    .collect::<Vec<_>>(),
            )
        };
        data.insert("ServerBrands", strings(&self.server_brands));
        data.insert("WasModded", self.was_modded);
        data.insert(
            "Version",
            Compound::with_capacity(4)
                .with("Name", VERSION_ID)
                .with("Id", DATA_VERSION)
                .with("Snapshot", false)
                .with("Series", "main"),
        );
        let mut history = self.version_history.clone();
        if history.last() != Some(&DATA_VERSION) {
            history.push(DATA_VERSION);
        }
        data.insert(
            "version_history",
            List(history.into_iter().map(Tag::Int).collect()),
        );
        data.insert("DataVersion", DATA_VERSION);
        data.insert("GameType", self.game_type);
        let spawn = self.spawn.clone().unwrap_or(Spawn {
            dimension: "minecraft:overworld".to_owned(),
            pos: [0; 3],
            yaw: 0.0,
            pitch: 0.0,
        });
        data.insert("spawn", spawn.to_nbt());
        data.insert("Time", self.game_time);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
        data.insert("LastPlayed", now);
        data.insert("LevelName", self.level_name.as_str());
        data.insert("version", ANVIL_VERSION);
        data.insert("allowCommands", self.allow_commands);
        data.insert("initialized", self.initialized);
        data.insert(
            "difficulty_settings",
            Compound::with_capacity(3)
                .with("difficulty", self.difficulty.as_str())
                .with("hardcore", self.hardcore)
                .with("locked", self.difficulty_locked),
        );
        data
    }
}

impl Spawn {
    fn from_nbt(spawn: &Compound) -> Spawn {
        let float = |name| match spawn.get(name) {
            Some(Tag::Float(v)) if v.is_finite() => *v,
            _ => 0.0,
        };
        Spawn {
            dimension: spawn
                .get("dimension")
                .and_then(Tag::as_str)
                .unwrap_or("minecraft:overworld")
                .to_owned(),
            pos: match spawn.get("pos") {
                Some(Tag::IntArray(pos)) if pos.len() == 3 => [pos[0], pos[1], pos[2]],
                _ => [0; 3],
            },
            yaw: float("yaw").clamp(-180.0, 180.0),
            pitch: float("pitch").clamp(-90.0, 90.0),
        }
    }

    fn to_nbt(&self) -> Compound {
        Compound::with_capacity(4)
            .with("dimension", self.dimension.as_str())
            .with("pos", self.pos.to_vec())
            .with("yaw", self.yaw)
            .with("pitch", self.pitch)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    /// `tag` with the entries of its compounds sorted by name (the order is not stored).
    fn sorted(tag: &Tag) -> Tag {
        match tag {
            Tag::Compound(c) => {
                let mut entries: Vec<_> = c.iter().collect();
                entries.sort_by_key(|(k, _)| *k);
                let mut out = Compound::new();
                for (k, v) in entries {
                    out.insert(k, sorted(v));
                }
                Tag::Compound(out)
            }
            other => other.clone(),
        }
    }

    /// Every tag of `a` is in `b` with the same value, except `ignored`.
    fn assert_kept(a: &Compound, b: &Compound, ignored: &[&str]) {
        for (key, tag) in a.iter() {
            if !ignored.contains(&key) {
                assert_eq!(b.get(key).map(sorted), Some(sorted(tag)), "{key}");
            }
        }
    }

    #[test]
    fn round_trip_keeps_every_field() {
        let mut level = LevelData::new("world");
        level.spawn = Some(Spawn {
            dimension: "minecraft:overworld".to_owned(),
            pos: [10, 64, -3],
            yaw: 90.0,
            pitch: 0.0,
        });
        level.game_time = 123_456;
        level.difficulty = "hard".to_owned();
        level.difficulty_locked = true;
        level.add_brand("vanillite", true);
        level
            .other
            .insert("paperSpawnDimension", "minecraft:overworld");
        let nbt = level.to_nbt();
        let mut again = LevelData::from_nbt(nbt.clone());
        assert_eq!(again.data_version, DATA_VERSION);
        assert_eq!(again.version_history, [DATA_VERSION]);
        again.data_version = 0;
        again.version_history.clear();
        assert_eq!(again, level);
        assert_kept(
            &nbt,
            &LevelData::from_nbt(nbt.clone()).to_nbt(),
            &["LastPlayed"],
        );
    }

    #[test]
    fn legacy_fields_are_read() {
        let data = Compound::new()
            .with("SpawnX", 5)
            .with("SpawnY", 70)
            .with("SpawnZ", -8)
            .with("SpawnAngle", 45.0f32)
            .with("Difficulty", 3i8)
            .with("DifficultyLocked", true)
            .with("DayTime", 9000i64);
        let level = LevelData::from_nbt(data);
        assert_eq!(level.spawn.as_ref().map(|s| s.pos), Some([5, 70, -8]));
        assert_eq!(level.difficulty, "hard");
        assert!(level.difficulty_locked);
        // Moved into saved data by the systems that own it.
        assert_eq!(level.other.get("DayTime"), Some(&Tag::Long(9000)));
        assert!(level.to_nbt().get("SpawnX").is_none());
    }

    /// `level.dat` of a real 26.x world (`VANILLITE_TEST_WORLD`, a copy that is only read)
    /// survives reading and writing: every field comes back the same except the ones 26.3
    /// writes anew.
    #[test]
    fn real_world_round_trip() {
        let Some(world) = std::env::var_os("VANILLITE_TEST_WORLD").map(PathBuf::from) else {
            return;
        };
        let root = files::read_gzip_nbt(&world.join("level.dat"))
            .unwrap()
            .expect("level.dat");
        let Some(Tag::Compound(data)) = root.get("Data") else {
            panic!("no Data")
        };
        let level = LevelData::from_nbt(data.clone());
        assert!(level.spawn.is_some());
        let out = std::env::temp_dir().join(format!("vanillite-leveldat-{}", std::process::id()));
        level.write(&out).unwrap();
        let again = LevelData::read(&out).unwrap().expect("written");
        assert_eq!(again.spawn, level.spawn);
        assert_eq!(again.game_time, level.game_time);
        assert_kept(data, &again.to_nbt(), &["LastPlayed"]);
        std::fs::remove_dir_all(&out).unwrap();
    }
}

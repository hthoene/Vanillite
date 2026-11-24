//! Player data files: `<world>/players/data/<uuid>.dat` (26.x layout), gzip-compressed named NBT in Vanilla's
//! layout, so worlds stay interchangeable with Vanilla.
//!
//! Only the fields the server keeps so far are read and written; unknown fields of an existing
//! file are kept when it is saved again.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use vanillite_base::hash::FxHashMap;
use vanillite_nbt::{Compound, List, Tag};

use crate::files;
use crate::writer::FileWriter;

/// The persisted state of one player.
#[derive(Clone, Debug, PartialEq)]
pub struct PlayerData {
    /// `Pos`.
    pub pos: [f64; 3],
    /// `Rotation`: yaw, pitch.
    pub rotation: [f32; 2],
    /// `OnGround`.
    pub on_ground: bool,
    /// `Dimension`, e.g. `minecraft:overworld`.
    pub dimension: String,
    /// `playerGameType`.
    pub game_mode: i32,
    /// Fields of the file this server does not handle yet.
    pub other: Compound,
}

impl Default for PlayerData {
    fn default() -> Self {
        PlayerData {
            pos: [0.0; 3],
            rotation: [0.0; 2],
            on_ground: false,
            dimension: "minecraft:overworld".to_owned(),
            game_mode: 0,
            other: Compound::new(),
        }
    }
}

/// Player data files of one world. With a [`FileWriter`], [`update`](Self::update) writes in
/// the background; a load of a player with queued writes waits for them.
#[derive(Clone, Debug)]
pub struct PlayerDataStore {
    dir: PathBuf,
    writer: Option<FileWriter>,
    /// Queued writes per player.
    pending: Arc<Mutex<FxHashMap<u128, u32>>>,
}

impl PlayerDataStore {
    /// The store in `<level_dir>/players/data`; writes run on the calling thread.
    pub fn new(level_dir: &Path) -> PlayerDataStore {
        PlayerDataStore {
            dir: level_dir.join("players").join("data"),
            writer: None,
            pending: Arc::default(),
        }
    }

    /// Writes through `writer` from now on.
    #[must_use]
    pub fn with_writer(mut self, writer: FileWriter) -> PlayerDataStore {
        self.writer = Some(writer);
        self
    }

    fn path(&self, uuid: u128) -> PathBuf {
        self.dir.join(format!("{}.dat", uuid_string(uuid)))
    }

    /// Loads a player's data (from `<uuid>.dat_old` if `<uuid>.dat` is missing or damaged,
    /// like Vanilla). `Ok(None)` if the player has none; a damaged file is an error.
    pub fn load(&self, uuid: u128) -> io::Result<Option<PlayerData>> {
        let queued = self
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains_key(&uuid);
        if queued && let Some(writer) = &self.writer {
            writer.flush();
        }
        Ok(files::read_gzip_nbt_or_old(&self.path(uuid))?.map(PlayerData::from_nbt))
    }

    /// Saves a player's data like Vanilla: atomically, the previous file kept as
    /// `<uuid>.dat_old`.
    pub fn save(&self, uuid: u128, data: &PlayerData) -> io::Result<()> {
        files::write_gzip_nbt(&self.path(uuid), &data.to_nbt(), true)
    }

    /// Changes a player's file: reads it (keeping the fields this server does not handle),
    /// applies `change` and writes it, in the background when the store has a writer.
    pub fn update(&self, uuid: u128, change: impl FnOnce(&mut PlayerData) + Send + 'static) {
        let Some(writer) = &self.writer else {
            self.apply(uuid, change);
            return;
        };
        *self
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(uuid)
            .or_default() += 1;
        let store = PlayerDataStore {
            writer: None,
            ..self.clone()
        };
        writer.submit(move || {
            store.apply(uuid, change);
            let mut pending = store.pending.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(n) = pending.get_mut(&uuid) {
                *n -= 1;
                if *n == 0 {
                    pending.remove(&uuid);
                }
            }
        });
    }

    fn apply(&self, uuid: u128, change: impl FnOnce(&mut PlayerData)) {
        let path = self.path(uuid);
        let mut data = match files::read_gzip_nbt_or_old(&path) {
            Ok(root) => root.map(PlayerData::from_nbt).unwrap_or_default(),
            Err(e) => {
                log::error!(
                    "Player data {} is damaged, writing it anew: {e}",
                    path.display()
                );
                PlayerData::default()
            }
        };
        change(&mut data);
        if let Err(e) = self.save(uuid, &data) {
            log::error!("Failed to save player data {}: {e}", path.display());
        }
    }
}

/// UUID in Java's `UUID.toString()` form.
pub fn uuid_string(v: u128) -> String {
    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        v >> 96,
        (v >> 80) & 0xffff,
        (v >> 64) & 0xffff,
        (v >> 48) & 0xffff,
        v & 0xffff_ffff_ffff
    )
}

fn number(tag: Option<&Tag>) -> Option<f64> {
    match tag? {
        Tag::Double(v) => Some(*v),
        Tag::Float(v) => Some(f64::from(*v)),
        _ => None,
    }
}

impl PlayerData {
    fn from_nbt(mut root: Compound) -> PlayerData {
        let list = |root: &mut Compound, name: &str| match root.remove(name) {
            Some(Tag::List(List(items))) => items,
            _ => Vec::new(),
        };
        let pos_list = list(&mut root, "Pos");
        let mut pos = [0.0; 3];
        for (slot, tag) in pos.iter_mut().zip(&pos_list) {
            *slot = number(Some(tag)).filter(|v| v.is_finite()).unwrap_or(0.0);
        }
        let rot_list = list(&mut root, "Rotation");
        let mut rotation = [0.0f32; 2];
        for (slot, tag) in rotation.iter_mut().zip(&rot_list) {
            *slot = match tag {
                Tag::Float(v) if v.is_finite() => *v,
                _ => 0.0,
            };
        }
        let on_ground = root
            .remove("OnGround")
            .and_then(|t| t.as_i64())
            .is_some_and(|v| v != 0);
        let dimension = root
            .remove("Dimension")
            .and_then(|t| t.as_str().map(str::to_owned))
            .unwrap_or_else(|| "minecraft:overworld".to_owned());
        let game_mode = root
            .remove("playerGameType")
            .and_then(|t| t.as_i64())
            .and_then(|v| i32::try_from(v).ok())
            .unwrap_or(-1);
        PlayerData {
            pos,
            rotation,
            on_ground,
            dimension,
            game_mode,
            other: root,
        }
    }

    fn to_nbt(&self) -> Compound {
        let mut root = self.other.clone();
        root.insert(
            "Pos",
            List(self.pos.iter().map(|&v| Tag::Double(v)).collect()),
        );
        root.insert(
            "Rotation",
            List(self.rotation.iter().map(|&v| Tag::Float(v)).collect()),
        );
        root.insert("OnGround", self.on_ground);
        root.insert("Dimension", self.dimension.as_str());
        root.insert("playerGameType", self.game_mode);
        root
    }
}

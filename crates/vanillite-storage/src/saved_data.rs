//! Vanilla's saved data files: `<dir>/data/<namespace>/<path>.dat`, gzip NBT with `DataVersion`
//! and `data`.
//!
//! 26.x keeps world-wide data in `<world>/data/` (game rules, weather, world clocks, world
//! generation settings, scoreboard, command storage, maps, random sequences, boss bars,
//! scheduled functions, the wandering trader) and per-dimension data in
//! `<dimension>/data/` (world border, chunk tickets, raids, the dragon fight, structures).
//!
//! Each system owns the files of its ids and writes only those. Files of systems that do not
//! exist yet are never touched, so data of a world made by Vanilla survives until they come.
//! A later system implements [`SavedData`] and registers it in the [`SavedDataStore`] of its
//! scope; the store writes it with every save.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use vanillite_nbt::{Compound, Tag};

use crate::files;
use crate::writer::FileWriter;

/// The file of saved data `id` (e.g. `minecraft:world_gen_settings`) below `dir`:
/// `<dir>/data/<namespace>/<path>.dat`.
pub fn saved_data_path(dir: &Path, id: &str) -> PathBuf {
    let (namespace, path) = id.split_once(':').unwrap_or(("minecraft", id));
    dir.join("data").join(namespace).join(format!("{path}.dat"))
}

/// Reads the `data` compound of saved data `id` below `dir`; `None` if there is none.
pub fn read_saved_data(dir: &Path, id: &str) -> io::Result<Option<Compound>> {
    let root = files::read_gzip_nbt(&saved_data_path(dir, id))?;
    Ok(root.map(|mut root| match root.remove("data") {
        Some(Tag::Compound(data)) => data,
        _ => Compound::default(),
    }))
}

/// Writes saved data `id` below `dir` like Vanilla (`DataVersion` and `data`), atomically.
pub fn write_saved_data(dir: &Path, id: &str, data: Compound) -> io::Result<()> {
    let root = Compound::with_capacity(2)
        .with("DataVersion", vanillite_data::version::DATA_VERSION)
        .with("data", data);
    files::write_gzip_nbt(&saved_data_path(dir, id), &root, false)
}

/// The state of a system that is stored as saved data (Vanilla's `SavedData`).
pub trait SavedData: Send + Sync {
    /// The id, e.g. `minecraft:raids`; the file is `data/minecraft/raids.dat`.
    fn id(&self) -> &str;

    /// The `data` compound to write, `None` when nothing changed since the last call
    /// (Vanilla's `isDirty`).
    fn save(&self) -> Option<Compound>;
}

/// The saved data of one scope: the world (`<world>/data/`) or a dimension
/// (`<dimension>/data/`). Writes go through the background [`FileWriter`].
pub struct SavedDataStore {
    dir: PathBuf,
    writer: FileWriter,
    registered: Mutex<Vec<Arc<dyn SavedData>>>,
}

impl std::fmt::Debug for SavedDataStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SavedDataStore")
            .field("dir", &self.dir)
            .finish_non_exhaustive()
    }
}

impl SavedDataStore {
    /// The store whose files live in `<dir>/data/`.
    pub fn new(dir: impl Into<PathBuf>, writer: FileWriter) -> SavedDataStore {
        SavedDataStore {
            dir: dir.into(),
            writer,
            registered: Mutex::new(Vec::new()),
        }
    }

    /// The folder that holds `data/`.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Reads the data of `id` (when its system starts).
    pub fn read(&self, id: &str) -> io::Result<Option<Compound>> {
        read_saved_data(&self.dir, id)
    }

    /// Registers a system's data; it is written by every [`save`](Self::save) from now on.
    pub fn register(&self, data: Arc<dyn SavedData>) {
        self.registered
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(data);
    }

    /// Queues the write of `data` as `id`.
    pub fn write(&self, id: &str, data: Compound) {
        let dir = self.dir.clone();
        let id = id.to_owned();
        self.writer.submit(move || {
            if let Err(e) = write_saved_data(&dir, &id, data) {
                log::error!("Could not save data {id}: {e}");
            }
        });
    }

    /// Queues the writes of every registered system whose data changed.
    pub fn save(&self) {
        let registered = self
            .registered
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        for data in registered {
            if let Some(compound) = data.save() {
                self.write(data.id(), compound);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    struct Counter(AtomicBool);

    impl SavedData for Counter {
        fn id(&self) -> &str {
            "vanillite:counter"
        }

        fn save(&self) -> Option<Compound> {
            self.0
                .swap(false, Ordering::Relaxed)
                .then(|| Compound::new().with("n", 7))
        }
    }

    /// Every saved data file of a real 26.x world (`VANILLITE_TEST_WORLD`, a copy that is only
    /// read), world-wide and per dimension, is written back with the same contents.
    #[test]
    fn real_world_saved_data_round_trip() {
        let Some(world) = std::env::var_os("VANILLITE_TEST_WORLD").map(PathBuf::from) else {
            return;
        };
        let mut scopes = vec![world.clone()];
        for dimension in ["overworld", "the_nether", "the_end"] {
            scopes.push(world.join("dimensions/minecraft").join(dimension));
        }
        let out = std::env::temp_dir().join(format!("vanillite-saved-rt-{}", std::process::id()));
        let mut count = 0;
        for scope in scopes {
            let Ok(namespaces) = std::fs::read_dir(scope.join("data")) else {
                continue;
            };
            for namespace in namespaces {
                let namespace = namespace.unwrap();
                let ns = namespace.file_name().into_string().unwrap();
                for file in std::fs::read_dir(namespace.path()).unwrap() {
                    let name = file.unwrap().file_name().into_string().unwrap();
                    let Some(path) = name.strip_suffix(".dat") else {
                        continue;
                    };
                    let id = format!("{ns}:{path}");
                    let original = files::read_gzip_nbt(&saved_data_path(&scope, &id))
                        .unwrap()
                        .unwrap();
                    let data = read_saved_data(&scope, &id).unwrap().unwrap();
                    write_saved_data(&out, &id, data).unwrap();
                    let again = files::read_gzip_nbt(&saved_data_path(&out, &id))
                        .unwrap()
                        .unwrap();
                    assert_eq!(again, original, "{}", scope.join(&name).display());
                    count += 1;
                }
            }
        }
        assert!(count > 10, "only {count} files");
        std::fs::remove_dir_all(&out).unwrap();
    }

    #[test]
    fn registered_data_is_written_when_dirty_and_others_are_untouched() {
        let dir = std::env::temp_dir().join(format!("vanillite-saved-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        write_saved_data(&dir, "minecraft:raids", Compound::new().with("tick", 5)).unwrap();
        let before = std::fs::read(saved_data_path(&dir, "minecraft:raids")).unwrap();
        let writer = FileWriter::start().unwrap();
        let store = SavedDataStore::new(&dir, writer.clone());
        store.register(Arc::new(Counter(AtomicBool::new(true))));
        store.save();
        writer.flush();
        let path = saved_data_path(&dir, "vanillite:counter");
        assert_eq!(path, dir.join("data/vanillite/counter.dat"));
        assert_eq!(
            store.read("vanillite:counter").unwrap(),
            Some(Compound::new().with("n", 7))
        );
        std::fs::remove_file(&path).unwrap();
        store.save();
        writer.flush();
        assert!(!path.exists());
        assert_eq!(
            std::fs::read(saved_data_path(&dir, "minecraft:raids")).unwrap(),
            before
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

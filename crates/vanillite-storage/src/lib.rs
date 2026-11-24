//! Persistence providers: Anvil region files (deflate, LZ4, none), chunk NBT, player data,
//! `level.dat` and the import of older and Spigot world layouts.
//!
//! - [`AnvilStorage`] is the built-in `chunk-storage` slot (architecture 9.3).
//! - [`player_data`] reads and writes `players/data/<uuid>.dat`.
//! - [`import`] moves older Vanilla and Spigot/Paper layouts into the 26.x layout.
//! - [`layout`] places dimensions in Vanilla's or Spigot's folder layout.
//!
//! Stored files are untrusted input: decompression and NBT reads run under the file budgets
//! (SEC-26), and errors are reported per chunk instead of panicking.

pub mod chunk_nbt;
pub mod compression;
pub mod import;
pub mod layout;
pub mod player_data;
pub mod region;

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use vanillite_base::ChunkPos;
use vanillite_base::hash::FxHashMap;
use vanillite_chunk::ProtoChunk;
use vanillite_chunk::slots::{ChunkStorage, ChunkStorageProvider, WorldInfo};
use vanillite_nbt::{Budget, Compound};

pub use crate::chunk_nbt::Codec;
pub use crate::compression::Compression;
pub use crate::layout::WorldLayout;
use crate::region::RegionFile;

/// An open region file, shared by the jobs that use it.
type SharedRegion = Arc<Mutex<RegionFile>>;

/// Region files kept open per world while no job uses them; files in use stay open above it.
const MAX_OPEN_REGIONS: usize = 256;

/// The folder of a dimension inside a world folder (26.x layout):
/// `dimensions/<namespace>/<path>`.
pub fn dimension_dir(world_dir: &Path, dimension: &str) -> PathBuf {
    let (namespace, path) = dimension
        .split_once(':')
        .unwrap_or(("minecraft", dimension));
    world_dir.join("dimensions").join(namespace).join(path)
}

/// Reads `level.dat` (gzip NBT); `None` if the world has none yet.
pub fn read_level_dat(world_dir: &Path) -> io::Result<Option<Compound>> {
    read_gzip_nbt(&world_dir.join("level.dat"))
}

/// Writes `level.dat` like Vanilla: the previous file becomes `level.dat_old`, the new one is
/// written through a temporary file.
pub fn write_level_dat(world_dir: &Path, root: &Compound) -> io::Result<()> {
    std::fs::create_dir_all(world_dir)?;
    let mut raw = Vec::new();
    vanillite_nbt::write::write_named("", root, &mut raw).map_err(io::Error::other)?;
    let path = world_dir.join("level.dat");
    let tmp = world_dir.join("level.dat_new");
    std::fs::write(&tmp, Compression::Gzip.compress(&raw)?)?;
    if path.exists() {
        std::fs::copy(&path, world_dir.join("level.dat_old"))?;
    }
    std::fs::rename(&tmp, &path)
}

/// The file of Vanilla saved data `id` (e.g. `minecraft:world_gen_settings`) below `dir`:
/// `<dir>/data/<namespace>/<path>.dat`.
pub fn saved_data_path(dir: &Path, id: &str) -> PathBuf {
    let (namespace, path) = id.split_once(':').unwrap_or(("minecraft", id));
    dir.join("data").join(namespace).join(format!("{path}.dat"))
}

/// Reads the `data` compound of saved data `id` below `dir`; `None` if there is none.
pub fn read_saved_data(dir: &Path, id: &str) -> io::Result<Option<Compound>> {
    let path = saved_data_path(dir, id);
    Ok(
        read_gzip_nbt(&path)?.map(|mut root| match root.remove("data") {
            Some(vanillite_nbt::Tag::Compound(data)) => data,
            _ => Compound::default(),
        }),
    )
}

/// Writes saved data `id` below `dir` like Vanilla (`DataVersion` and `data`), through a
/// temporary file.
pub fn write_saved_data(dir: &Path, id: &str, data: Compound) -> io::Result<()> {
    let path = saved_data_path(dir, id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let root = Compound::default()
        .with("DataVersion", vanillite_data::version::DATA_VERSION)
        .with("data", data);
    let mut raw = Vec::new();
    vanillite_nbt::write::write_named("", &root, &mut raw).map_err(io::Error::other)?;
    let tmp = path.with_extension("dat_tmp");
    std::fs::write(&tmp, Compression::Gzip.compress(&raw)?)?;
    std::fs::rename(&tmp, &path)
}

pub(crate) fn read_gzip_nbt(path: &Path) -> io::Result<Option<Compound>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let raw = Compression::Gzip.decompress(&bytes)?;
    vanillite_nbt::read::read_named(&mut raw.as_slice(), &mut Budget::file())
        .map(Some)
        .map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: {e:?}", path.display()),
            )
        })
}

/// The built-in `chunk-storage` slot: Anvil region files in `<dimension>/region`.
#[derive(Debug)]
pub struct AnvilStorage {
    dir: PathBuf,
    compression: Compression,
    codec: Codec,
    /// Open region files; at most one instance per file, so jobs never write past each other.
    regions: Mutex<FxHashMap<(i32, i32), SharedRegion>>,
    max_open: usize,
}

impl AnvilStorage {
    /// The storage of a world; new chunks are written with `compression`.
    pub fn new(world: &WorldInfo, compression: Compression) -> Self {
        AnvilStorage {
            dir: world.dir.join("region"),
            compression,
            codec: Codec::new(world),
            regions: Mutex::new(FxHashMap::default()),
            max_open: MAX_OPEN_REGIONS,
        }
    }

    fn region(&self, pos: ChunkPos, create: bool) -> io::Result<Option<SharedRegion>> {
        let key = pos.region();
        let mut regions = self.regions.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(region) = regions.get(&key) {
            return Ok(Some(Arc::clone(region)));
        }
        if create {
            std::fs::create_dir_all(&self.dir)?;
        }
        let path = self.dir.join(format!("r.{}.{}.mca", key.0, key.1));
        let Some(region) = RegionFile::open(&path, create)? else {
            return Ok(None);
        };
        if regions.len() >= self.max_open {
            // Close only files no job holds: a job's handle must stay the one instance of its
            // file (a second instance would allocate the same sectors). Handles are only cloned
            // under this lock, so a count of 1 cannot grow while it is held.
            regions.retain(|_, region| Arc::strong_count(region) > 1);
        }
        let region = Arc::new(Mutex::new(region));
        regions.insert(key, Arc::clone(&region));
        Ok(Some(region))
    }

    fn load_one(&self, pos: ChunkPos) -> io::Result<Option<ProtoChunk>> {
        let Some(region) = self.region(pos, false)? else {
            return Ok(None);
        };
        let raw = region
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .read(pos)?;
        let Some((compression, data)) = raw else {
            return Ok(None);
        };
        let bytes = compression.decompress(&data)?;
        let root = vanillite_nbt::read::read_named(&mut bytes.as_slice(), &mut Budget::file())
            .map_err(|e| {
                io::Error::new(io::ErrorKind::InvalidData, format!("chunk {pos:?}: {e:?}"))
            })?;
        self.codec.read(pos, root).map(Some)
    }

    fn save_one(&self, chunk: &ProtoChunk) -> io::Result<()> {
        let root = self.codec.write(chunk)?;
        let mut bytes = Vec::with_capacity(16 * 1024);
        vanillite_nbt::write::write_named("", &root, &mut bytes)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{e:?}")))?;
        let data = self.compression.compress(&bytes)?;
        let region = self
            .region(chunk.pos, true)?
            .ok_or_else(|| io::Error::other("region file not created"))?;
        let mut region = region.lock().unwrap_or_else(PoisonError::into_inner);
        region.write(chunk.pos, self.compression, &data)
    }
}

impl ChunkStorage for AnvilStorage {
    fn load(&self, positions: &[ChunkPos], out: &mut Vec<io::Result<Option<ProtoChunk>>>) {
        out.extend(positions.iter().map(|&pos| self.load_one(pos)));
    }

    fn save(&self, chunks: &[Arc<ProtoChunk>], out: &mut Vec<io::Result<()>>) {
        out.extend(chunks.iter().map(|chunk| self.save_one(chunk)));
    }

    fn flush(&self) -> io::Result<()> {
        let regions: Vec<_> = self
            .regions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .cloned()
            .collect();
        for region in regions {
            region
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .sync()?;
        }
        Ok(())
    }
}

/// Provider of [`AnvilStorage`].
#[derive(Debug, Clone, Copy)]
pub struct AnvilStorageProvider {
    /// Compression of written chunks (`region-file-compression`).
    pub compression: Compression,
}

impl Default for AnvilStorageProvider {
    fn default() -> Self {
        AnvilStorageProvider {
            compression: Compression::Deflate,
        }
    }
}

impl ChunkStorageProvider for AnvilStorageProvider {
    fn create(&self, world: &WorldInfo) -> io::Result<Arc<dyn ChunkStorage>> {
        Ok(Arc::new(AnvilStorage::new(world, self.compression)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Many threads writing into more region files than stay open: every chunk reads back
    /// intact (#50: evicting files in use opened a second instance that overwrote sectors).
    #[test]
    fn region_cache_keeps_one_instance_per_file() {
        let dir = std::env::temp_dir().join(format!("vanillite-regions-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let info = WorldInfo {
            dimension: "minecraft:overworld".to_owned(),
            dir: dir.clone(),
            min_y: 0,
            height: 16,
            seed: 0,
            has_sky_light: true,
            biomes: Arc::from(Vec::<String>::new()),
            default_biome: Default::default(),
            air: Default::default(),
        };
        let mut storage = AnvilStorage::new(&info, Compression::None);
        storage.max_open = 4;
        let (files, threads, per_file) = (64, 16, 24);
        let data = |pos: ChunkPos| {
            let tag = (pos.x as u8).wrapping_mul(31) ^ (pos.z as u8);
            vec![tag; 3000 + (pos.x as usize & 7) * 1000]
        };
        let pos = |file: i32, i: i32| ChunkPos::new((file % 8) * 32 + i, (file / 8) * 32 + i);
        std::thread::scope(|scope| {
            for t in 0..threads {
                let storage = &storage;
                scope.spawn(move || {
                    // Each file gets chunks from several threads, interleaved with other files.
                    for i in 0..per_file {
                        for file in (0..files).filter(|f| (f + i) % threads == t) {
                            let pos = pos(file, i);
                            let region = storage.region(pos, true).unwrap().unwrap();
                            // A job holds its handle a while (compression, waiting for the lock).
                            std::thread::sleep(std::time::Duration::from_millis(1));
                            let mut region = region.lock().unwrap();
                            region.write(pos, Compression::None, &data(pos)).unwrap();
                        }
                    }
                });
            }
        });
        drop(storage);
        let storage = AnvilStorage::new(&info, Compression::None);
        for file in 0..files {
            for i in 0..per_file {
                let pos = pos(file, i);
                let region = storage.region(pos, false).unwrap().unwrap();
                let read = region.lock().unwrap().read(pos).unwrap();
                assert_eq!(read, Some((Compression::None, data(pos))), "chunk {pos:?}");
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}

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

/// Region files kept open per world.
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
    regions: Mutex<FxHashMap<(i32, i32), SharedRegion>>,
}

impl AnvilStorage {
    /// The storage of a world; new chunks are written with `compression`.
    pub fn new(world: &WorldInfo, compression: Compression) -> Self {
        AnvilStorage {
            dir: world.dir.join("region"),
            compression,
            codec: Codec::new(world),
            regions: Mutex::new(FxHashMap::default()),
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
        if regions.len() >= MAX_OPEN_REGIONS {
            regions.clear();
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

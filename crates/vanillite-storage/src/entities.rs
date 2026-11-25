//! Entity chunks: `<dimension>/entities/r.<x>.<z>.mca`, one compound per chunk with
//! `DataVersion`, `Position` and the `Entities` list (Vanilla's `EntityStorage`).

use std::io;
use std::path::PathBuf;

use vanillite_base::ChunkPos;
use vanillite_base::hash::FxHashMap;
use vanillite_data::version::DATA_VERSION;
use vanillite_nbt::{Budget, Compound, List, Tag};

use crate::compression::Compression;
use crate::region::RegionFile;

/// Most region files kept open at once.
const MAX_OPEN: usize = 64;

/// The entity region files of one world. Used by one I/O thread at a time.
#[derive(Debug)]
pub struct EntityStorage {
    dir: PathBuf,
    compression: Compression,
    regions: FxHashMap<(i32, i32), RegionFile>,
}

impl EntityStorage {
    /// The entity storage in `dimension_dir/entities`.
    pub fn new(dimension_dir: &std::path::Path, compression: Compression) -> EntityStorage {
        EntityStorage {
            dir: dimension_dir.join("entities"),
            compression,
            regions: FxHashMap::default(),
        }
    }

    fn region(&mut self, pos: ChunkPos, create: bool) -> io::Result<Option<&mut RegionFile>> {
        let key = pos.region();
        if !self.regions.contains_key(&key) {
            if create {
                std::fs::create_dir_all(&self.dir)?;
            }
            let path = self.dir.join(format!("r.{}.{}.mca", key.0, key.1));
            let Some(region) = RegionFile::open(&path, create)? else {
                return Ok(None);
            };
            if self.regions.len() >= MAX_OPEN {
                for r in self.regions.values_mut() {
                    r.sync()?;
                }
                self.regions.clear();
            }
            self.regions.insert(key, region);
        }
        Ok(self.regions.get_mut(&key))
    }

    /// The entity compounds of a chunk; empty if none were saved.
    pub fn load(&mut self, pos: ChunkPos) -> io::Result<Vec<Compound>> {
        let Some(region) = self.region(pos, false)? else {
            return Ok(Vec::new());
        };
        let Some((compression, data)) = region.read(pos)? else {
            return Ok(Vec::new());
        };
        let bytes = compression.decompress(&data)?;
        let root = vanillite_nbt::read::read_named(&mut bytes.as_slice(), &mut Budget::file())
            .map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("entities {pos:?}: {e:?}"),
                )
            })?;
        Ok(match root.get("Entities") {
            Some(Tag::List(list)) => list
                .0
                .iter()
                .filter_map(|t| t.as_compound().cloned())
                .collect(),
            _ => Vec::new(),
        })
    }

    /// Writes the entities of a chunk (an empty list when it has none left).
    pub fn save(&mut self, pos: ChunkPos, entities: Vec<Compound>) -> io::Result<()> {
        let empty = entities.is_empty();
        if empty && self.region(pos, false)?.is_none() {
            return Ok(());
        }
        let mut root = Compound::new();
        root.insert("DataVersion", DATA_VERSION);
        root.insert("Position", vec![pos.x, pos.z]);
        root.insert(
            "Entities",
            List(entities.into_iter().map(Tag::Compound).collect()),
        );
        let mut bytes = Vec::with_capacity(1024);
        vanillite_nbt::write::write_named("", &root, &mut bytes)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{e:?}")))?;
        let compression = self.compression;
        let data = compression.compress(&bytes)?;
        let region = self
            .region(pos, true)?
            .ok_or_else(|| io::Error::other("entity region file not created"))?;
        region.write(pos, compression, &data)
    }

    /// Writes everything buffered to disk.
    pub fn flush(&mut self) -> io::Result<()> {
        for r in self.regions.values_mut() {
            r.sync()?;
        }
        Ok(())
    }
}

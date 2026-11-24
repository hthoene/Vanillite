//! Persistence providers: Anvil region files (deflate, LZ4, none), chunk NBT, player data,
//! `level.dat` and the import of older and Spigot world layouts.
//!
//! - [`service`] is the storage service (#47): the built-in `chunk-storage` slot
//!   ([`AnvilStore`]) on the disk IO threads ([`IoPool`]).
//! - [`AnvilCodec`] turns chunks into Anvil records and back on the compute pool.
//! - [`blocking`] waits for store results on the calling thread (tools, tests, import).
//! - [`player_data`] reads and writes `players/data/<uuid>.dat`.
//! - [`level_dat`] reads and writes `level.dat`, [`saved_data`] the files below `data/`.
//! - [`writer`] writes small files in the background, [`files`] makes every write atomic.
//! - [`backup`] archives a running world.
//! - [`import`] moves older Vanilla and Spigot/Paper layouts into the 26.x layout.
//! - [`layout`] places dimensions in Vanilla's or Spigot's folder layout.
//!
//! Stored files are untrusted input: decompression and NBT reads run under the file budgets
//! (SEC-26), and errors are reported per chunk instead of panicking.

pub mod backup;
pub mod blocking;
pub mod chunk_nbt;
pub mod compression;
pub mod files;
pub mod import;
pub mod layout;
pub mod level_dat;
pub mod player_data;
pub mod region;
pub mod saved_data;
pub mod service;
pub mod writer;

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use vanillite_base::ChunkPos;
use vanillite_chunk::ProtoChunk;
use vanillite_chunk::slots::{ChunkCodec, RecordMeta, WorldInfo};
use vanillite_data::version::DATA_VERSION;
use vanillite_nbt::Budget;

pub use crate::chunk_nbt::Codec;
pub use crate::compression::Compression;
pub use crate::layout::WorldLayout;
pub use crate::saved_data::{read_saved_data, saved_data_path, write_saved_data};
pub use crate::service::{AnvilStore, AnvilStoreProvider, IoPool, StoreSettings, StoreStats};

/// The folder of a dimension inside a world folder (26.x layout):
/// `dimensions/<namespace>/<path>`.
pub fn dimension_dir(world_dir: &Path, dimension: &str) -> PathBuf {
    let (namespace, path) = dimension
        .split_once(':')
        .unwrap_or(("minecraft", dimension));
    world_dir.join("dimensions").join(namespace).join(path)
}

/// The Anvil chunk codec: a record is the compression id followed by the compressed chunk NBT,
/// exactly what a region file keeps after the length.
#[derive(Debug)]
pub struct AnvilCodec {
    codec: Codec,
    compression: Compression,
}

impl AnvilCodec {
    /// The codec of a world; chunks are written with `compression`.
    pub fn new(world: &WorldInfo, compression: Compression) -> Self {
        AnvilCodec {
            codec: Codec::new(world),
            compression,
        }
    }

    /// The chunk NBT codec.
    pub fn nbt(&self) -> &Codec {
        &self.codec
    }
}

impl ChunkCodec for AnvilCodec {
    fn decode(&self, pos: ChunkPos, _: &RecordMeta, record: &[u8]) -> io::Result<ProtoChunk> {
        let Some((&id, data)) = record.split_first() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("chunk {pos:?}: empty record"),
            ));
        };
        let compression = Compression::from_id(id).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("chunk {pos:?}: unknown compression {id}"),
            )
        })?;
        let bytes = compression.decompress(data)?;
        let root = vanillite_nbt::read::read_named(&mut bytes.as_slice(), &mut Budget::file())
            .map_err(|e| {
                io::Error::new(io::ErrorKind::InvalidData, format!("chunk {pos:?}: {e:?}"))
            })?;
        self.codec.read(pos, root)
    }

    fn encode(&self, chunk: &ProtoChunk) -> io::Result<(RecordMeta, Arc<[u8]>)> {
        let root = self.codec.write(chunk)?;
        let mut bytes = Vec::with_capacity(16 * 1024);
        vanillite_nbt::write::write_named("", &root, &mut bytes)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{e:?}")))?;
        let data = self.compression.compress(&bytes)?;
        let mut record = Vec::with_capacity(data.len() + 1);
        record.push(self.compression.id());
        record.extend_from_slice(&data);
        let meta = RecordMeta {
            data_version: DATA_VERSION as u32,
            raw_len: u32::try_from(bytes.len()).unwrap_or(u32::MAX),
            checksum: 0,
            status: chunk.status as u8,
            flags: 0,
        };
        Ok((meta, Arc::from(record)))
    }

    fn chunks_per_job(&self) -> usize {
        // Jobs of about 1 ms: deflate takes ~0.5 ms per chunk, LZ4 and none far less.
        match self.compression {
            Compression::Deflate => 2,
            _ => 8,
        }
    }
}

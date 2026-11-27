//! Keeps records that cannot be decoded (#142). The world generates such a chunk again and
//! later saves it over the old record, so the original bytes are copied aside first:
//! `<dimension>/unreadable/<stream>.<x>.<z>.<unix ms>.bin`, exactly as stored (Anvil: the
//! compression byte and the compressed NBT; own format: the zstd frame). Records the region
//! file itself cannot read (a bad length, a failed checksum) are kept the same way by the file,
//! as `.raw` with the bytes found on disk ([`keep_raw`]).

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use vanillite_base::ChunkPos;
use vanillite_chunk::ProtoChunk;
use vanillite_chunk::store::{ChunkCodec, RecordMeta};

use crate::files;

/// Copies the bytes of a record that the region file in `folder` (`<dimension>/region`,
/// `entities` or `poi`) cannot read into `<dimension>/unreadable/`, before the chunk is
/// generated again and saved over it.
pub fn keep_raw(folder: &Path, pos: ChunkPos, bytes: &[u8]) {
    let stream = match folder.file_name().and_then(|n| n.to_str()) {
        Some("region") => "chunk",
        Some(name) => name,
        None => "chunk",
    };
    let dir = folder.parent().unwrap_or(folder).join("unreadable");
    write_kept(&dir, stream, pos, bytes, "raw");
}

fn write_kept(dir: &Path, stream: &str, pos: ChunkPos, bytes: &[u8], extension: &str) {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let path = dir.join(format!("{stream}.{}.{}.{millis}.{extension}", pos.x, pos.z));
    match files::write_atomic(&path, bytes, false) {
        Ok(()) => log::warn!(
            "Kept the unreadable {stream} record of chunk {}, {} in {}",
            pos.x,
            pos.z,
            path.display()
        ),
        Err(e) => log::error!(
            "Couldn't keep the unreadable {stream} record of chunk {}, {} in {}: {e}",
            pos.x,
            pos.z,
            path.display()
        ),
    }
}

/// A codec that copies every record it cannot decode into `dir` before it returns the error.
#[derive(Debug)]
pub struct KeepUnreadable<C> {
    inner: C,
    dir: PathBuf,
}

impl<C: ChunkCodec> KeepUnreadable<C> {
    /// `inner` with unreadable records kept in `dir` (created when needed).
    pub fn new(inner: C, dir: PathBuf) -> Self {
        KeepUnreadable { inner, dir }
    }

    fn keep<T>(
        &self,
        stream: &str,
        pos: ChunkPos,
        bytes: &[u8],
        result: io::Result<T>,
    ) -> io::Result<T> {
        if result.is_err() {
            write_kept(&self.dir, stream, pos, bytes, "bin");
        }
        result
    }
}

impl<C: ChunkCodec> ChunkCodec for KeepUnreadable<C> {
    fn decode(&self, pos: ChunkPos, meta: &RecordMeta, bytes: &[u8]) -> io::Result<ProtoChunk> {
        self.keep("chunk", pos, bytes, self.inner.decode(pos, meta, bytes))
    }

    fn encode(&self, chunk: &ProtoChunk) -> io::Result<(RecordMeta, Arc<[u8]>)> {
        self.inner.encode(chunk)
    }

    fn decode_entities(
        &self,
        pos: ChunkPos,
        meta: &RecordMeta,
        bytes: &[u8],
    ) -> io::Result<Vec<Arc<[u8]>>> {
        self.keep(
            "entities",
            pos,
            bytes,
            self.inner.decode_entities(pos, meta, bytes),
        )
    }

    fn encode_entities(&self, chunk: &ProtoChunk) -> io::Result<(RecordMeta, Arc<[u8]>)> {
        self.inner.encode_entities(chunk)
    }

    fn decode_poi(
        &self,
        pos: ChunkPos,
        meta: &RecordMeta,
        bytes: &[u8],
    ) -> io::Result<Option<Arc<[u8]>>> {
        self.keep("poi", pos, bytes, self.inner.decode_poi(pos, meta, bytes))
    }

    fn encode_poi(&self, chunk: &ProtoChunk) -> io::Result<(RecordMeta, Arc<[u8]>)> {
        self.inner.encode_poi(chunk)
    }

    fn chunks_per_job(&self) -> usize {
        self.inner.chunks_per_job()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk_nbt::tests::{codec, golden};
    use crate::own_codec::{DEFAULT_LEVEL, OwnCodec};

    /// A record that does not decode is copied aside unchanged; the error is passed on.
    #[test]
    fn unreadable_records_are_kept() {
        let chunks = golden();
        let anvil = codec(&chunks);
        let dir = std::env::temp_dir().join(format!("vanillite-unreadable-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let keeping = KeepUnreadable::new(OwnCodec::new(anvil.info(), DEFAULT_LEVEL), dir.clone());
        let pos = ChunkPos::new(-8, -15);
        let bytes = b"not a zstd frame";
        assert!(keeping.decode(pos, &RecordMeta::default(), bytes).is_err());
        let kept: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(kept.len(), 1);
        let name = kept[0].file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.starts_with("chunk.-8.-15.") && name.ends_with(".bin"),
            "{name}"
        );
        assert_eq!(std::fs::read(&kept[0]).unwrap(), bytes);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

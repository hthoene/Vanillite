//! Anvil region files (`r.<x>.<z>.mca`): 32 × 32 chunks in 4 KiB sectors.
//!
//! Vanilla: `RegionFile`. The header holds 1024 sector offsets (3 bytes offset, 1 byte count)
//! and 1024 timestamps. A chunk starts with its length (4 bytes, including the compression byte)
//! and its compression id; chunks above 255 sectors live in an external file `c.<x>.<z>.mcc`
//! and set bit 128 of the compression id.

use std::fs::{self, File, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use vanillite_base::ChunkPos;

use crate::compression::{Compression, MAX_DECOMPRESSED};

const SECTOR: usize = 4096;
const HEADER_SECTORS: usize = 2;
const MAX_SECTORS: usize = 255;
const EXTERNAL: u8 = 128;

/// Locks that keep a backup from copying a region file while a chunk is written into it, by
/// file name (the same name in other dimensions only shares the lock).
static GATES: [Mutex<()>; 64] = [const { Mutex::new(()) }; 64];

/// Holds the backup lock of the region file `name` (e.g. `r.0.-1.mca`).
pub fn gate(name: &str) -> MutexGuard<'static, ()> {
    let mut hasher = std::hash::DefaultHasher::new();
    name.hash(&mut hasher);
    let index = (hasher.finish() % GATES.len() as u64) as usize;
    GATES[index].lock().unwrap_or_else(PoisonError::into_inner)
}

/// One open region file.
#[derive(Debug)]
pub struct RegionFile {
    file: File,
    /// The file name, for [`gate`].
    name: String,
    dir: PathBuf,
    offsets: [u32; 1024],
    timestamps: [u32; 1024],
    used: Vec<bool>,
}

/// Index of a chunk in its region.
#[inline]
fn slot(pos: ChunkPos) -> usize {
    ((pos.x & 31) + (pos.z & 31) * 32) as usize
}

fn invalid(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

impl RegionFile {
    /// Opens a region file; creates it if `create` is set, otherwise returns `None` when it does
    /// not exist.
    pub fn open(path: &Path, create: bool) -> io::Result<Option<RegionFile>> {
        let file = match OpenOptions::new()
            .read(true)
            .write(true)
            .create(create)
            .truncate(false)
            .open(path)
        {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound && !create => return Ok(None),
            Err(e) => return Err(e),
        };
        let mut region = RegionFile {
            file,
            name: path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            dir: path.parent().map(Path::to_path_buf).unwrap_or_default(),
            offsets: [0; 1024],
            timestamps: [0; 1024],
            used: vec![true; HEADER_SECTORS],
        };
        let len = region.file.metadata()?.len() as usize;
        if len < HEADER_SECTORS * SECTOR {
            // New or truncated: start with an empty header (Vanilla pads the same way).
            region.file.set_len((HEADER_SECTORS * SECTOR) as u64)?;
            region.file.seek(SeekFrom::Start(0))?;
            region.file.write_all(&[0; HEADER_SECTORS * SECTOR])?;
            return Ok(Some(region));
        }
        let mut header = vec![0u8; HEADER_SECTORS * SECTOR];
        region.file.seek(SeekFrom::Start(0))?;
        region.file.read_exact(&mut header)?;
        let sectors_in_file = len.div_ceil(SECTOR);
        for i in 0..1024 {
            let offset =
                u32::from_be_bytes(header[i * 4..i * 4 + 4].try_into().unwrap_or_default());
            region.timestamps[i] = u32::from_be_bytes(
                header[SECTOR + i * 4..SECTOR + i * 4 + 4]
                    .try_into()
                    .unwrap_or_default(),
            );
            let (start, count) = ((offset >> 8) as usize, (offset & 255) as usize);
            // Vanilla drops entries that point into the header or past the file end.
            if offset == 0
                || start < HEADER_SECTORS
                || count == 0
                || start + count > sectors_in_file
            {
                continue;
            }
            if region.is_free(start, count) {
                region.mark(start, count, true);
                region.offsets[i] = offset;
            }
        }
        Ok(Some(region))
    }

    fn is_free(&self, start: usize, count: usize) -> bool {
        (start..start + count).all(|s| !self.used.get(s).copied().unwrap_or(false))
    }

    fn mark(&mut self, start: usize, count: usize, used: bool) {
        if self.used.len() < start + count {
            self.used.resize(start + count, false);
        }
        self.used[start..start + count].fill(used);
    }

    fn allocate(&mut self, count: usize) -> usize {
        let mut start = HEADER_SECTORS;
        while !self.is_free(start, count) {
            start += 1;
        }
        self.mark(start, count, true);
        start
    }

    fn external_path(&self, pos: ChunkPos) -> PathBuf {
        self.dir.join(format!("c.{}.{}.mcc", pos.x, pos.z))
    }

    /// Reads the raw data of a chunk: its compression and the compressed bytes.
    pub fn read(&mut self, pos: ChunkPos) -> io::Result<Option<(Compression, Vec<u8>)>> {
        let offset = self.offsets[slot(pos)];
        if offset == 0 {
            return Ok(None);
        }
        let (start, count) = ((offset >> 8) as u64, (offset & 255) as usize);
        let mut head = [0u8; 5];
        self.file.seek(SeekFrom::Start(start * SECTOR as u64))?;
        self.file.read_exact(&mut head)?;
        let length = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as usize;
        let id = head[4];
        if length == 0 || length > count * SECTOR - 4 {
            return Err(invalid(format!(
                "chunk {pos:?}: invalid length {length} for {count} sectors"
            )));
        }
        let compression = Compression::from_id(id & !EXTERNAL)
            .ok_or_else(|| invalid(format!("chunk {pos:?}: unknown compression {id}")))?;
        let data = if id & EXTERNAL != 0 {
            let path = self.external_path(pos);
            let size = fs::metadata(&path)?.len() as usize;
            if size > MAX_DECOMPRESSED {
                return Err(invalid(format!("{}: too large", path.display())));
            }
            fs::read(path)?
        } else {
            let mut data = vec![0u8; length - 1];
            self.file.read_exact(&mut data)?;
            data
        };
        Ok(Some((compression, data)))
    }

    /// Writes the compressed data of a chunk. New sectors are written before the old ones are
    /// freed, so a crash leaves the old version readable.
    pub fn write(
        &mut self,
        pos: ChunkPos,
        compression: Compression,
        data: &[u8],
    ) -> io::Result<()> {
        let _gate = gate(&self.name);
        let index = slot(pos);
        let external_path = self.external_path(pos);
        let needed = (data.len() + 5).div_ceil(SECTOR);
        let (id, body): (u8, &[u8]) = if needed > MAX_SECTORS {
            let tmp = external_path.with_extension("mcc.tmp");
            fs::write(&tmp, data)?;
            fs::rename(&tmp, &external_path)?;
            (compression.id() | EXTERNAL, &[])
        } else {
            (compression.id(), data)
        };
        let count = (body.len() + 5).div_ceil(SECTOR);
        let start = self.allocate(count);
        let mut buffer = Vec::with_capacity(count * SECTOR);
        buffer.extend_from_slice(&(body.len() as u32 + 1).to_be_bytes());
        buffer.push(id);
        buffer.extend_from_slice(body);
        buffer.resize(count * SECTOR, 0);
        self.file.seek(SeekFrom::Start((start * SECTOR) as u64))?;
        self.file.write_all(&buffer)?;

        let old = self.offsets[index];
        let offset = (start as u32) << 8 | count as u32;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as u32);
        self.offsets[index] = offset;
        self.timestamps[index] = now;
        self.file.seek(SeekFrom::Start((index * 4) as u64))?;
        self.file.write_all(&offset.to_be_bytes())?;
        self.file
            .seek(SeekFrom::Start((SECTOR + index * 4) as u64))?;
        self.file.write_all(&now.to_be_bytes())?;
        if old != 0 {
            self.mark((old >> 8) as usize, (old & 255) as usize, false);
        }
        if id & EXTERNAL == 0 && external_path.exists() {
            fs::remove_file(external_path)?;
        }
        Ok(())
    }

    /// Flushes written data to the disk.
    pub fn sync(&mut self) -> io::Result<()> {
        self.file.sync_data()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_read_and_external() {
        let dir = std::env::temp_dir().join(format!("vanillite-region-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("r.0.0.mca");
        let _ = fs::remove_file(&path);
        let mut region = RegionFile::open(&path, true).unwrap().unwrap();
        let small = vec![7u8; 10_000];
        let big = vec![9u8; 2 * 1024 * 1024];
        region
            .write(ChunkPos::new(1, 2), Compression::None, &small)
            .unwrap();
        region
            .write(ChunkPos::new(3, 4), Compression::None, &big)
            .unwrap();
        region
            .write(ChunkPos::new(1, 2), Compression::Lz4, &small[..100])
            .unwrap();
        drop(region);
        let mut region = RegionFile::open(&path, false).unwrap().unwrap();
        assert_eq!(
            region.read(ChunkPos::new(1, 2)).unwrap(),
            Some((Compression::Lz4, small[..100].to_vec()))
        );
        assert_eq!(region.read(ChunkPos::new(3, 4)).unwrap().unwrap().1, big);
        assert_eq!(region.read(ChunkPos::new(5, 5)).unwrap(), None);
        fs::remove_dir_all(dir).unwrap();
    }
}

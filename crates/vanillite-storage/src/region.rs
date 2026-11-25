//! Anvil region files (`r.<x>.<z>.mca`): 32 × 32 chunks in 4 KiB sectors.
//!
//! Vanilla: `RegionFile`. The header holds 1024 sector offsets (3 bytes offset, 1 byte count)
//! and 1024 timestamps. A chunk starts with its length (4 bytes, including the compression byte)
//! and its compression id; chunks above 255 sectors live in an external file `c.<x>.<z>.mcc`
//! and set bit 128 of the compression id.
//!
//! A record is what follows the length: the compression id and the compressed chunk NBT. All IO
//! is positional, so one open instance serves reads and writes without seeking. Sectors a write
//! replaces stay reserved until the next [`RegionFile::sync`] (or [`RegionFile::release_freed`]):
//! until the new header is durable, the old record on disk must not be overwritten.

use std::fs::{self, File, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use vanillite_base::ChunkPos;
use vanillite_chunk::store::RecordMeta;

use crate::compression::{Compression, MAX_DECOMPRESSED};

const SECTOR: usize = 4096;
const HEADER_SECTORS: usize = 2;
const MAX_SECTORS: usize = 255;
const EXTERNAL: u8 = 128;
/// Free sectors between two records that are still read in one go: 32 KiB more per read cost
/// less than a second read on IOPS-bound disks (#56 measurements).
const MAX_GAP_SECTORS: usize = 8;
/// Largest single read of neighbouring records.
const MAX_RUN_SECTORS: usize = 256;

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

#[cfg(unix)]
pub(crate) fn read_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(file, buf, offset)
}

#[cfg(unix)]
pub(crate) fn write_at(file: &File, buf: &[u8], offset: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(file, buf, offset)
}

#[cfg(windows)]
pub(crate) fn read_at(file: &File, mut buf: &mut [u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match file.seek_read(buf, offset)? {
            0 => return Err(io::ErrorKind::UnexpectedEof.into()),
            n => {
                buf = &mut buf[n..];
                offset += n as u64;
            }
        }
    }
    Ok(())
}

#[cfg(windows)]
pub(crate) fn write_at(file: &File, mut buf: &[u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match file.seek_write(buf, offset)? {
            0 => return Err(io::ErrorKind::WriteZero.into()),
            n => {
                buf = &buf[n..];
                offset += n as u64;
            }
        }
    }
    Ok(())
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
    /// Sectors of replaced records, free after the next sync.
    freed: Vec<(usize, usize)>,
}

/// A read of [`RegionFile::read_many`].
#[derive(Debug)]
pub struct RecordRead {
    /// Slot of the chunk in the region (`x & 31 + (z & 31) * 32`).
    pub slot: usize,
    /// Read along with a wanted record (read-ahead).
    pub ahead: bool,
    /// What the file keeps about the record (Anvil: nothing, the status is unknown).
    pub meta: RecordMeta,
    /// The record, `None` if the chunk is not stored.
    pub record: io::Result<Option<Vec<u8>>>,
}

/// The metadata of Anvil records: Anvil keeps none.
const ANVIL_META: RecordMeta = RecordMeta {
    data_version: 0,
    raw_len: 0,
    checksum: 0,
    status: RecordMeta::STATUS_UNKNOWN,
    flags: 0,
};

/// Index of a chunk in its region.
#[inline]
pub fn slot(pos: ChunkPos) -> usize {
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
            freed: Vec::new(),
        };
        let len = region.file.metadata()?.len() as usize;
        if len < HEADER_SECTORS * SECTOR {
            // New or truncated: start with an empty header (Vanilla pads the same way).
            region.file.set_len((HEADER_SECTORS * SECTOR) as u64)?;
            write_at(&region.file, &[0; HEADER_SECTORS * SECTOR], 0)?;
            return Ok(Some(region));
        }
        let mut header = vec![0u8; HEADER_SECTORS * SECTOR];
        read_at(&region.file, &mut header, 0)?;
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

    /// The file name (`r.<x>.<z>.mca`).
    pub fn name(&self) -> &str {
        &self.name
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

    /// The chunk position of `slot` in a region.
    fn pos_of(slot: usize, region: (i32, i32)) -> ChunkPos {
        ChunkPos::new(
            region.0 * 32 + (slot & 31) as i32,
            region.1 * 32 + (slot / 32) as i32,
        )
    }

    /// Whether a chunk is stored, without reading it.
    pub fn contains(&self, pos: ChunkPos) -> bool {
        self.offsets[slot(pos)] != 0
    }

    /// Reads the raw data of a chunk: its compression and the compressed bytes.
    pub fn read(&mut self, pos: ChunkPos) -> io::Result<Option<(Compression, Vec<u8>)>> {
        let Some(mut record) = self.read_record(pos)? else {
            return Ok(None);
        };
        let id = record.first().copied().unwrap_or(0);
        let compression = Compression::from_id(id)
            .ok_or_else(|| invalid(format!("chunk {pos:?}: unknown compression {id}")))?;
        record.remove(0);
        Ok(Some((compression, record)))
    }

    /// Reads the record of a chunk: its compression id and the compressed bytes.
    pub fn read_record(&self, pos: ChunkPos) -> io::Result<Option<Vec<u8>>> {
        let mut out = Vec::with_capacity(1);
        self.read_many(pos.region(), &[slot(pos)], &[], &mut out);
        out.pop().map_or(Ok(None), |r| r.record)
    }

    /// Reads the records of the `wanted` slots of region `region` with as few reads as
    /// possible: records in neighbouring sectors are read together. Records of `ahead` that
    /// lie next to a wanted one are read along (flagged `ahead`). Returns the number of reads
    /// and the bytes read.
    pub fn read_many(
        &self,
        region: (i32, i32),
        wanted: &[usize],
        ahead: &[usize],
        out: &mut Vec<RecordRead>,
    ) -> (usize, usize) {
        // (start, count, slot, ahead)
        let mut entries: Vec<(usize, usize, usize, bool)> = Vec::new();
        for &slot in wanted {
            let offset = self.offsets[slot & 1023];
            if offset == 0 {
                out.push(RecordRead {
                    slot,
                    ahead: false,
                    meta: ANVIL_META,
                    record: Ok(None),
                });
            } else {
                entries.push(((offset >> 8) as usize, (offset & 255) as usize, slot, false));
            }
        }
        for &slot in ahead {
            let offset = self.offsets[slot & 1023];
            if offset != 0 && !wanted.contains(&slot) {
                entries.push(((offset >> 8) as usize, (offset & 255) as usize, slot, true));
            }
        }
        entries.sort_unstable_by_key(|e| e.0);
        entries.dedup_by_key(|e| e.2);
        let (mut reads, mut bytes) = (0, 0);
        let mut index = 0;
        let mut buffer = Vec::new();
        while index < entries.len() {
            let run_start = entries[index].0;
            let mut run_end = run_start + entries[index].1;
            let mut end = index + 1;
            while end < entries.len() {
                let (start, count, ..) = entries[end];
                if start > run_end + MAX_GAP_SECTORS
                    || (start + count).max(run_end) - run_start > MAX_RUN_SECTORS
                {
                    break;
                }
                run_end = run_end.max(start + count);
                end += 1;
            }
            let run = &entries[index..end];
            index = end;
            // Runs of read-ahead records only are not read.
            if run.iter().all(|e| e.3) {
                continue;
            }
            buffer.clear();
            buffer.resize((run_end - run_start) * SECTOR, 0);
            reads += 1;
            bytes += buffer.len();
            let result = read_at(&self.file, &mut buffer, (run_start * SECTOR) as u64);
            for &(start, count, slot, ahead) in run {
                let record = match &result {
                    Ok(()) => {
                        let at = (start - run_start) * SECTOR;
                        let pos = Self::pos_of(slot, region);
                        self.parse(pos, &buffer[at..at + count * SECTOR], count)
                            .map(Some)
                    }
                    Err(e) => Err(io::Error::new(e.kind(), e.to_string())),
                };
                // A damaged neighbour read ahead is simply not cached.
                if ahead && record.is_err() {
                    continue;
                }
                out.push(RecordRead {
                    slot,
                    ahead,
                    meta: ANVIL_META,
                    record,
                });
            }
        }
        (reads, bytes)
    }

    /// The record in the sectors of a chunk.
    fn parse(&self, pos: ChunkPos, sectors: &[u8], count: usize) -> io::Result<Vec<u8>> {
        let length = u32::from_be_bytes([sectors[0], sectors[1], sectors[2], sectors[3]]) as usize;
        let id = sectors[4];
        if length == 0 || length > count * SECTOR - 4 {
            return Err(invalid(format!(
                "chunk {pos:?}: invalid length {length} for {count} sectors"
            )));
        }
        if id & EXTERNAL != 0 {
            let path = self.external_path(pos);
            let size = fs::metadata(&path)?.len() as usize;
            if size > MAX_DECOMPRESSED {
                return Err(invalid(format!("{}: too large", path.display())));
            }
            let mut record = Vec::with_capacity(size + 1);
            record.push(id & !EXTERNAL);
            record.extend_from_slice(&fs::read(path)?);
            Ok(record)
        } else {
            Ok(sectors[4..4 + length].to_vec())
        }
    }

    /// Writes the compressed data of a chunk.
    pub fn write(
        &mut self,
        pos: ChunkPos,
        compression: Compression,
        data: &[u8],
    ) -> io::Result<()> {
        let mut record = Vec::with_capacity(data.len() + 1);
        record.push(compression.id());
        record.extend_from_slice(data);
        self.write_record(pos, &record)
    }

    /// Writes the record of a chunk (compression id, then the compressed bytes). New sectors
    /// are written before the header points to them, and the old ones stay reserved until the
    /// next sync, so a crash leaves the old version readable.
    pub fn write_record(&mut self, pos: ChunkPos, record: &[u8]) -> io::Result<()> {
        let Some((&id, data)) = record.split_first() else {
            return Err(invalid(format!("chunk {pos:?}: empty record")));
        };
        if id & EXTERNAL != 0 || Compression::from_id(id).is_none() {
            return Err(invalid(format!("chunk {pos:?}: unknown compression {id}")));
        }
        let _gate = gate(&self.name);
        let index = slot(pos);
        let external_path = self.external_path(pos);
        let needed = (data.len() + 5).div_ceil(SECTOR);
        let (id, body): (u8, &[u8]) = if needed > MAX_SECTORS {
            let tmp = external_path.with_extension("mcc.tmp");
            fs::write(&tmp, data)?;
            fs::rename(&tmp, &external_path)?;
            (id | EXTERNAL, &[])
        } else {
            (id, data)
        };
        let count = (body.len() + 5).div_ceil(SECTOR);
        let start = self.allocate(count);
        let mut buffer = Vec::with_capacity(count * SECTOR);
        buffer.extend_from_slice(&(body.len() as u32 + 1).to_be_bytes());
        buffer.push(id);
        buffer.extend_from_slice(body);
        buffer.resize(count * SECTOR, 0);
        if let Err(e) = write_at(&self.file, &buffer, (start * SECTOR) as u64) {
            self.mark(start, count, false);
            return Err(e);
        }
        let offset = (start as u32) << 8 | count as u32;
        self.set_entry(index, offset)?;
        if id & EXTERNAL == 0 && external_path.exists() {
            fs::remove_file(external_path)?;
        }
        Ok(())
    }

    /// Removes a chunk.
    pub fn delete(&mut self, pos: ChunkPos) -> io::Result<()> {
        let _gate = gate(&self.name);
        if self.offsets[slot(pos)] == 0 {
            return Ok(());
        }
        self.set_entry(slot(pos), 0)?;
        let external_path = self.external_path(pos);
        if external_path.exists() {
            fs::remove_file(external_path)?;
        }
        Ok(())
    }

    /// Points the header entry of `index` to `offset`; the old sectors wait for the next sync.
    fn set_entry(&mut self, index: usize, offset: u32) -> io::Result<()> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as u32);
        write_at(&self.file, &offset.to_be_bytes(), (index * 4) as u64)?;
        write_at(&self.file, &now.to_be_bytes(), (SECTOR + index * 4) as u64)?;
        let old = std::mem::replace(&mut self.offsets[index], offset);
        self.timestamps[index] = now;
        if old != 0 {
            self.freed.push(((old >> 8) as usize, (old & 255) as usize));
        }
        Ok(())
    }

    /// Flushes written data to the disk and frees the sectors of replaced records.
    pub fn sync(&mut self) -> io::Result<()> {
        self.file.sync_data()?;
        self.release_freed();
        Ok(())
    }

    /// Frees the sectors of replaced records without syncing (`sync-chunk-writes=false`).
    pub fn release_freed(&mut self) {
        for (start, count) in std::mem::take(&mut self.freed) {
            self.mark(start, count, false);
        }
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

    /// Neighbouring records come in one read; read-ahead takes only those near a wanted one.
    #[test]
    fn neighbours_are_read_together() {
        let dir = std::env::temp_dir().join(format!("vanillite-region-n-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("r.0.0.mca");
        let _ = fs::remove_file(&path);
        let mut region = RegionFile::open(&path, true).unwrap().unwrap();
        for x in 0..8 {
            region
                .write(ChunkPos::new(x, 0), Compression::None, &vec![x as u8; 5000])
                .unwrap();
        }
        let mut out = Vec::new();
        let (reads, _) = region.read_many((0, 0), &[0, 1, 2], &[3, 5, 40], &mut out);
        assert_eq!(reads, 1);
        let mut slots: Vec<(usize, bool)> = out.iter().map(|r| (r.slot, r.ahead)).collect();
        slots.sort_unstable();
        // Slot 5 lies two sectors behind slot 3 (slot 4 between them is not wanted).
        assert_eq!(
            slots,
            [(0, false), (1, false), (2, false), (3, true), (5, true)]
        );
        for read in &out {
            let record = read.record.as_ref().unwrap().as_ref().unwrap();
            assert_eq!(record.len(), 5001);
            assert_eq!(record[1], read.slot as u8);
        }
        fs::remove_dir_all(dir).unwrap();
    }
}

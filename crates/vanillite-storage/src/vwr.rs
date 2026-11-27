//! Region files of the own world format (`r.<x>.<z>.vwr`, #25): 32 × 32 chunks, like Anvil's
//! grid, so conversion is 1:1 per file.
//!
//! ```text
//! offset 0          header (4 KiB): magic "VWR\x01", format version
//! offset 4 KiB      index copy A: generation u64, checksum u32, reserved u32,
//!                   then 1024 entries of 24 B
//! offset INDEX_B    index copy B, the same
//! offset DATA_START records, 256 B aligned
//! ```
//!
//! An entry is offset u48, stored length u32, raw length u32, data version u32, checksum u32
//! (low 32 bits of xxh3-64 of the stored bytes), status u8 and flags u8 (§4.1 of the storage
//! design). Every read checks the record's checksum.
//!
//! Crash safety (copy on write, A/B index like LMDB's meta pages): a record is never written
//! over a record the index on disk points to. Writes change only the index in memory;
//! [`VwrFile::flush`] syncs the records, writes the index into the older copy with the next
//! generation and its checksum, and syncs again. Opening takes the newest copy whose checksum
//! is valid, so a crash at any point leaves the file as of its last flush. Space of replaced
//! records is reused only after that flush. A record whose checksum fails is taken from the
//! older index copy if that one still has an intact version of it.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use vanillite_base::ChunkPos;
use vanillite_chunk::store::RecordMeta;

use crate::compression::MAX_DECOMPRESSED;
use crate::region::{RecordRead, gate, read_at, slot, write_at};

/// Magic and format version at the start of every file.
const MAGIC: [u8; 4] = *b"VWR\x01";
const FORMAT_VERSION: u32 = 1;
const HEADER: u64 = 4096;
const ENTRIES: usize = 1024;
const ENTRY: usize = 24;
/// Generation, checksum and a reserved word in front of the entries of an index copy.
const TRAILER: usize = 16;
const INDEX_BYTES: usize = TRAILER + ENTRIES * ENTRY;
/// Space of one index copy, aligned.
const INDEX_SPACE: u64 = (INDEX_BYTES as u64).div_ceil(ALIGN) * ALIGN;
const INDEX_A: u64 = HEADER;
const INDEX_B: u64 = HEADER + INDEX_SPACE;
/// Where records start.
pub const DATA_START: u64 = HEADER + 2 * INDEX_SPACE;
/// Record alignment: ~128 B lost per record instead of Anvil's ~2 KiB.
const ALIGN: u64 = 256;
/// Free space between two records that are still read in one go.
const MAX_GAP: u64 = 32 * 1024;
/// Largest single read of neighbouring records.
const MAX_RUN: u64 = 1024 * 1024;

/// The checksum of stored bytes: the low 32 bits of xxh3-64 (88 ns for a 4.5 KB record).
pub fn checksum(bytes: &[u8]) -> u32 {
    twox_hash::XxHash3_64::oneshot(bytes) as u32
}

fn invalid(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// Where a record lies and what is known about it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Entry {
    /// 0 = no record.
    offset: u64,
    len: u32,
    meta: RecordMeta,
}

impl Entry {
    fn space(&self) -> u64 {
        u64::from(self.len).div_ceil(ALIGN) * ALIGN
    }

    fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.offset.to_le_bytes()[..6]);
        out.extend_from_slice(&self.len.to_le_bytes());
        out.extend_from_slice(&self.meta.raw_len.to_le_bytes());
        out.extend_from_slice(&self.meta.data_version.to_le_bytes());
        out.extend_from_slice(&self.meta.checksum.to_le_bytes());
        out.push(self.meta.status);
        out.push(self.meta.flags);
    }

    fn read(bytes: &[u8]) -> Entry {
        let u32_at = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        let mut offset = [0u8; 8];
        offset[..6].copy_from_slice(&bytes[..6]);
        Entry {
            offset: u64::from_le_bytes(offset),
            len: u32_at(6),
            meta: RecordMeta {
                raw_len: u32_at(10),
                data_version: u32_at(14),
                checksum: u32_at(18),
                status: bytes[22],
                flags: bytes[23],
            },
        }
    }
}

/// One parsed index copy: generation and entries; `None` if its checksum fails.
fn parse_index(bytes: &[u8]) -> Option<(u64, Vec<Entry>)> {
    let generation = u64::from_le_bytes(bytes[..8].try_into().ok()?);
    let stored = u32::from_le_bytes(bytes[8..12].try_into().ok()?);
    if generation == 0 || stored != index_checksum(generation, &bytes[TRAILER..INDEX_BYTES]) {
        return None;
    }
    let entries = bytes[TRAILER..INDEX_BYTES]
        .as_chunks::<ENTRY>()
        .0
        .iter()
        .map(|e| Entry::read(e))
        .collect();
    Some((generation, entries))
}

fn index_checksum(generation: u64, entries: &[u8]) -> u32 {
    twox_hash::XxHash3_64::oneshot_with_seed(generation, entries) as u32
}

fn index_offset(copy: usize) -> u64 {
    if copy == 0 { INDEX_A } else { INDEX_B }
}

/// One open own-format region file.
#[derive(Debug)]
pub struct VwrFile {
    file: File,
    /// The file name, for the backup lock ([`gate`]).
    name: String,
    /// The folder of the file, for records kept when they are unreadable.
    dir: PathBuf,
    entries: Vec<Entry>,
    /// Generation of the newest valid index copy on disk and which copy it is.
    generation: u64,
    active: usize,
    /// Free gaps (start, length) between records, sorted by start.
    free: Vec<(u64, u64)>,
    /// End of the record space in use.
    end: u64,
    /// Space of replaced records the index on disk may still point to; free after a flush.
    freed: Vec<(u64, u64)>,
    /// Slots whose record was written since the last flush (no index on disk points to it).
    fresh: Vec<bool>,
    /// The index in memory differs from the one on disk.
    dirty: bool,
}

impl VwrFile {
    /// Opens a file; creates it if `create` is set, otherwise returns `None` when it does not
    /// exist.
    pub fn open(path: &Path, create: bool) -> io::Result<Option<VwrFile>> {
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
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let length = file.metadata()?.len();
        let mut region = VwrFile {
            file,
            name,
            dir: path.parent().map(Path::to_path_buf).unwrap_or_default(),
            entries: vec![Entry::default(); ENTRIES],
            generation: 0,
            active: 1,
            free: Vec::new(),
            end: DATA_START,
            freed: Vec::new(),
            fresh: vec![false; ENTRIES],
            dirty: false,
        };
        if length == 0 {
            // A new file: header and a valid empty index, so a file without one is damaged.
            let mut header = vec![0u8; HEADER as usize];
            header[..4].copy_from_slice(&MAGIC);
            header[4..8].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
            write_at(&region.file, &header, 0)?;
            region.dirty = true;
            region.flush(true)?;
            return Ok(Some(region));
        }
        if length < DATA_START {
            return Err(invalid(format!("{}: truncated header", region.name)));
        }
        let mut head = vec![0u8; DATA_START as usize];
        read_at(&region.file, &mut head, 0)?;
        if head[..4] != MAGIC {
            return Err(invalid(format!(
                "{}: not a Vanillite region file",
                region.name
            )));
        }
        let version = u32::from_le_bytes(head[4..8].try_into().unwrap());
        if version != FORMAT_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("{}: format version {version} is not supported", region.name),
            ));
        }
        let copies = [0, 1].map(|copy| {
            let at = index_offset(copy) as usize;
            parse_index(&head[at..at + INDEX_BYTES])
        });
        let newest = (0..2)
            .filter_map(|copy| copies[copy].as_ref().map(|(g, _)| (*g, copy)))
            .max();
        let Some((generation, active)) = newest else {
            // Both copies damaged: keep the file untouched for recovery.
            return Err(invalid(format!("{}: no valid index", region.name)));
        };
        let [a, b] = copies;
        region.entries = if active == 0 { a } else { b }.expect("valid copy").1;
        region.generation = generation;
        region.active = active;
        region.rebuild_free(length)?;
        Ok(Some(region))
    }

    /// Rebuilds the free space from the entries; checks that records lie inside the file and
    /// do not overlap.
    fn rebuild_free(&mut self, length: u64) -> io::Result<()> {
        let mut used: Vec<(u64, u64, u64)> = self
            .entries
            .iter()
            .filter(|e| e.offset != 0)
            .map(|e| (e.offset, e.space(), u64::from(e.len)))
            .collect();
        used.sort_unstable();
        let mut at = DATA_START;
        self.free.clear();
        for (start, space, len) in used {
            if start < at || start % ALIGN != 0 || start + len > length {
                return Err(invalid(format!("{}: overlapping records", self.name)));
            }
            if start > at {
                self.free.push((at, start - at));
            }
            at = start + space;
        }
        self.end = at;
        Ok(())
    }

    /// The file name (`r.<x>.<z>.vwr`).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether a chunk is stored.
    pub fn contains(&self, pos: ChunkPos) -> bool {
        self.entries[slot(pos)].offset != 0
    }

    /// The metadata of every stored record by slot.
    pub fn metas(&self) -> impl Iterator<Item = (usize, RecordMeta)> + '_ {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.offset != 0)
            .map(|(slot, e)| (slot, e.meta))
    }

    /// Bytes of the file not used by records (gaps and replaced records).
    pub fn free_bytes(&self) -> u64 {
        self.free.iter().map(|f| f.1).sum::<u64>() + self.freed.iter().map(|f| f.1).sum::<u64>()
    }

    fn allocate(&mut self, space: u64) -> u64 {
        if let Some(index) = self.free.iter().position(|f| f.1 >= space) {
            let (start, length) = self.free[index];
            if length == space {
                self.free.remove(index);
            } else {
                self.free[index] = (start + space, length - space);
            }
            return start;
        }
        let start = self.end;
        self.end += space;
        start
    }

    fn release(&mut self, start: u64, space: u64) {
        if start + space == self.end {
            self.end = start;
            // The gap before it may now end at the end too.
            if let Some(&(last, length)) = self.free.last()
                && last + length == self.end
            {
                self.end = last;
                self.free.pop();
            }
            return;
        }
        let index = self.free.partition_point(|f| f.0 < start);
        self.free.insert(index, (start, space));
        // Merge with the neighbours.
        if index + 1 < self.free.len() && start + space == self.free[index + 1].0 {
            self.free[index].1 += self.free[index + 1].1;
            self.free.remove(index + 1);
        }
        if index > 0 && self.free[index - 1].0 + self.free[index - 1].1 == start {
            self.free[index - 1].1 += self.free[index].1;
            self.free.remove(index);
        }
    }

    /// Writes a record; the checksum is computed here. It becomes durable with the next
    /// [`flush`](Self::flush).
    pub fn write_record(
        &mut self,
        pos: ChunkPos,
        meta: RecordMeta,
        record: &[u8],
    ) -> io::Result<()> {
        if record.is_empty() || record.len() > MAX_DECOMPRESSED {
            return Err(invalid(format!(
                "chunk {pos:?}: record of {} bytes",
                record.len()
            )));
        }
        let _gate = gate(&self.name);
        let index = slot(pos);
        let entry = Entry {
            offset: 0,
            len: record.len() as u32,
            meta: RecordMeta {
                checksum: checksum(record),
                ..meta
            },
        };
        let space = entry.space();
        let start = self.allocate(space);
        if let Err(e) = write_at(&self.file, record, start) {
            self.release(start, space);
            return Err(e);
        }
        self.replace(
            index,
            Entry {
                offset: start,
                ..entry
            },
        );
        Ok(())
    }

    /// Removes a chunk; durable with the next flush.
    pub fn delete(&mut self, pos: ChunkPos) -> io::Result<()> {
        let index = slot(pos);
        if self.entries[index].offset != 0 {
            let _gate = gate(&self.name);
            self.replace(index, Entry::default());
        }
        Ok(())
    }

    fn replace(&mut self, index: usize, entry: Entry) {
        let old = std::mem::replace(&mut self.entries[index], entry);
        if old.offset != 0 {
            if self.fresh[index] {
                // No index on disk points to it.
                self.release(old.offset, old.space());
            } else {
                self.freed.push((old.offset, old.space()));
            }
        }
        self.fresh[index] = entry.offset != 0;
        self.dirty = true;
    }

    /// Makes every write durable: syncs the records (with `sync`), writes the index into the
    /// older copy with the next generation, syncs again, then frees replaced records. Without
    /// `sync` (`sync-chunk-writes=false`) the order on disk is left to the operating system.
    pub fn flush(&mut self, sync: bool) -> io::Result<()> {
        if !self.dirty {
            return Ok(());
        }
        let _gate = gate(&self.name);
        if sync {
            self.file.sync_data()?;
        }
        let generation = self.generation + 1;
        let copy = 1 - self.active;
        let mut bytes = Vec::with_capacity(INDEX_BYTES);
        bytes.extend_from_slice(&generation.to_le_bytes());
        bytes.extend_from_slice(&[0; 8]);
        for entry in &self.entries {
            entry.write(&mut bytes);
        }
        let sum = index_checksum(generation, &bytes[TRAILER..]);
        bytes[8..12].copy_from_slice(&sum.to_le_bytes());
        write_at(&self.file, &bytes, index_offset(copy))?;
        if sync {
            self.file.sync_data()?;
        }
        self.generation = generation;
        self.active = copy;
        self.dirty = false;
        self.fresh.fill(false);
        for (start, space) in std::mem::take(&mut self.freed) {
            self.release(start, space);
        }
        Ok(())
    }

    /// Whether writes wait for a flush.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Reads the record of a chunk with its metadata.
    pub fn read_record(&self, pos: ChunkPos) -> io::Result<Option<(RecordMeta, Vec<u8>)>> {
        let mut out = Vec::with_capacity(1);
        self.read_many(pos.region(), &[slot(pos)], &[], &mut out);
        match out.pop() {
            Some(read) => Ok(read.record?.map(|r| (read.meta, r))),
            None => Ok(None),
        }
    }

    /// Reads the records of the `wanted` slots with as few reads as possible: records close
    /// to each other are read together. Records of `ahead` that lie next to a wanted one are
    /// read along (flagged `ahead`). Returns the number of reads and the bytes read.
    pub fn read_many(
        &self,
        region: (i32, i32),
        wanted: &[usize],
        ahead: &[usize],
        out: &mut Vec<RecordRead>,
    ) -> (usize, usize) {
        // (offset, len, slot, ahead)
        let mut records: Vec<(u64, u64, usize, bool)> = Vec::new();
        for &slot in wanted {
            let entry = self.entries[slot & 1023];
            if entry.offset == 0 {
                out.push(RecordRead {
                    slot,
                    ahead: false,
                    meta: RecordMeta::default(),
                    record: Ok(None),
                });
            } else {
                records.push((entry.offset, u64::from(entry.len), slot, false));
            }
        }
        for &slot in ahead {
            let entry = self.entries[slot & 1023];
            if entry.offset != 0 && !wanted.contains(&slot) {
                records.push((entry.offset, u64::from(entry.len), slot, true));
            }
        }
        records.sort_unstable_by_key(|r| r.0);
        records.dedup_by_key(|r| r.2);
        let (mut reads, mut bytes) = (0, 0);
        let mut index = 0;
        let mut buffer = Vec::new();
        while index < records.len() {
            let run_start = records[index].0;
            let mut run_end = run_start + records[index].1;
            let mut end = index + 1;
            while end < records.len() {
                let (start, len, ..) = records[end];
                if start > run_end + MAX_GAP || (start + len).max(run_end) - run_start > MAX_RUN {
                    break;
                }
                run_end = run_end.max(start + len);
                end += 1;
            }
            let run = &records[index..end];
            index = end;
            if run.iter().all(|r| r.3) {
                continue;
            }
            buffer.clear();
            buffer.resize((run_end - run_start) as usize, 0);
            reads += 1;
            bytes += buffer.len();
            let result = read_at(&self.file, &mut buffer, run_start);
            for &(start, len, slot, ahead) in run {
                let entry = self.entries[slot & 1023];
                let record = match &result {
                    Ok(()) => {
                        let at = (start - run_start) as usize;
                        let record = &buffer[at..at + len as usize];
                        if checksum(record) == entry.meta.checksum {
                            Ok((entry.meta, record.to_vec()))
                        } else {
                            let older = self.older_version(slot, region);
                            if older.is_err() && !ahead {
                                crate::unreadable::keep_raw(
                                    &self.dir,
                                    ChunkPos::new(
                                        region.0 * 32 + (slot & 31) as i32,
                                        region.1 * 32 + (slot / 32) as i32,
                                    ),
                                    record,
                                );
                            }
                            older
                        }
                    }
                    Err(e) => Err(io::Error::new(e.kind(), e.to_string())),
                };
                // A damaged neighbour read ahead is simply not cached.
                if ahead && record.is_err() {
                    continue;
                }
                let (meta, record) = match record {
                    Ok((meta, record)) => (meta, Ok(Some(record))),
                    Err(e) => (RecordMeta::default(), Err(e)),
                };
                out.push(RecordRead {
                    slot,
                    ahead,
                    meta,
                    record,
                });
            }
        }
        (reads, bytes)
    }

    /// The record of `slot` from the older index copy, for a record whose checksum failed;
    /// an error if that one is gone or damaged too.
    fn older_version(&self, slot: usize, region: (i32, i32)) -> io::Result<(RecordMeta, Vec<u8>)> {
        let pos = ChunkPos::new(
            region.0 * 32 + (slot & 31) as i32,
            region.1 * 32 + (slot / 32) as i32,
        );
        let damaged = || {
            invalid(format!(
                "{}: chunk {}, {}: checksum mismatch, the record is damaged",
                self.name, pos.x, pos.z
            ))
        };
        let mut bytes = vec![0u8; INDEX_BYTES];
        read_at(&self.file, &mut bytes, index_offset(1 - self.active))?;
        let Some((generation, entries)) = parse_index(&bytes) else {
            return Err(damaged());
        };
        let entry = entries[slot];
        if generation >= self.generation
            || entry.offset == 0
            || entry.offset == self.entries[slot].offset
        {
            return Err(damaged());
        }
        let mut record = vec![0u8; entry.len as usize];
        read_at(&self.file, &mut record, entry.offset)?;
        if checksum(&record) != entry.meta.checksum {
            return Err(damaged());
        }
        log::warn!(
            "{}: chunk {}, {} is damaged; using its previous version",
            self.name,
            pos.x,
            pos.z
        );
        Ok((entry.meta, record))
    }
}

impl Drop for VwrFile {
    /// Writes the index of writes that were never flushed (a safety net; the store flushes
    /// on commit and before it closes a file).
    fn drop(&mut self) {
        if self.dirty
            && let Err(e) = self.flush(true)
        {
            log::error!("Couldn't write the index of {}: {e}", self.name);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("vanillite-vwr-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn record(slot: usize, version: u8, len: usize) -> Vec<u8> {
        (0..len)
            .map(|i| (i as u8).wrapping_mul(7) ^ slot as u8 ^ version)
            .collect()
    }

    fn meta(version: u8) -> RecordMeta {
        RecordMeta {
            data_version: 4000 + u32::from(version),
            raw_len: 1,
            status: 9,
            flags: 1,
            checksum: 0,
        }
    }

    fn pos(slot: usize) -> ChunkPos {
        ChunkPos::new((slot & 31) as i32, (slot / 32) as i32)
    }

    /// Records survive a reopen with their metadata; neighbours come in one read.
    #[test]
    fn write_flush_reopen() {
        let dir = temp("basic");
        let path = dir.join("r.0.0.vwr");
        let mut file = VwrFile::open(&path, true).unwrap().unwrap();
        for slot in 0..40 {
            file.write_record(pos(slot), meta(1), &record(slot, 1, 300 + slot * 50))
                .unwrap();
        }
        file.delete(pos(3)).unwrap();
        file.flush(true).unwrap();
        drop(file);
        let file = VwrFile::open(&path, false).unwrap().unwrap();
        let mut out = Vec::new();
        let (reads, _) = file.read_many((0, 0), &[0, 1, 2, 3, 4], &[5, 6], &mut out);
        assert_eq!(reads, 1);
        for read in &out {
            match read.slot {
                3 => assert!(read.record.as_ref().unwrap().is_none()),
                slot => {
                    assert_eq!(
                        read.record.as_ref().unwrap().as_deref(),
                        Some(&record(slot, 1, 300 + slot * 50)[..])
                    );
                    assert_eq!(read.meta.data_version, 4001);
                    assert_eq!(read.meta.status, 9);
                }
            }
        }
        assert_eq!(out.len(), 7);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A crash before a flush leaves the file as of the last flush, whatever was written
    /// after it; a crash while the index copy is written (a torn copy) too.
    #[test]
    fn crash_leaves_the_last_flush() {
        let dir = temp("crash");
        let path = dir.join("r.0.0.vwr");
        let mut file = VwrFile::open(&path, true).unwrap().unwrap();
        for slot in 0..64 {
            file.write_record(pos(slot), meta(1), &record(slot, 1, 4000))
                .unwrap();
        }
        file.flush(true).unwrap();
        let flushed = std::fs::read(&path).unwrap();
        // Overwrite every chunk twice, then crash without a flush.
        for version in 2..4 {
            for slot in 0..64 {
                file.write_record(pos(slot), meta(version), &record(slot, version, 5000))
                    .unwrap();
            }
        }
        file.dirty = false;
        drop(file);
        let check = |version: u8, len: usize| {
            let file = VwrFile::open(&path, false).unwrap().unwrap();
            for slot in 0..64 {
                let (meta, bytes) = file.read_record(pos(slot)).unwrap().unwrap();
                assert_eq!(bytes, record(slot, version, len), "slot {slot}");
                assert_eq!(meta.data_version, 4000 + u32::from(version));
            }
        };
        check(1, 4000);
        // The new records never touched the flushed ones.
        let now = std::fs::read(&path).unwrap();
        assert_eq!(now[..flushed.len()], flushed[..]);

        // Flush a new version, but tear its index copy: the previous one is used.
        let mut file = VwrFile::open(&path, false).unwrap().unwrap();
        for slot in 0..64 {
            file.write_record(pos(slot), meta(5), &record(slot, 5, 3000))
                .unwrap();
        }
        let copy = 1 - file.active;
        file.flush(true).unwrap();
        drop(file);
        check(5, 3000);
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[index_offset(copy) as usize + TRAILER + 100] ^= 0xff;
        std::fs::write(&path, &bytes).unwrap();
        check(1, 4000);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A damaged record is reported (or replaced by its previous version), never returned.
    #[test]
    fn checksums_catch_damage() {
        let dir = temp("damage");
        let path = dir.join("r.0.0.vwr");
        let mut file = VwrFile::open(&path, true).unwrap().unwrap();
        file.write_record(pos(0), meta(1), &record(0, 1, 2000))
            .unwrap();
        file.write_record(pos(1), meta(1), &record(1, 1, 2000))
            .unwrap();
        file.flush(true).unwrap();
        file.write_record(pos(1), meta(2), &record(1, 2, 2000))
            .unwrap();
        file.flush(true).unwrap();
        let at = |slot: usize| file.entries[slot].offset as usize;
        let (first, second) = (at(0), at(1));
        drop(file);
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[first + 10] ^= 1;
        bytes[second + 10] ^= 1;
        std::fs::write(&path, &bytes).unwrap();
        let file = VwrFile::open(&path, false).unwrap().unwrap();
        let error = file.read_record(pos(0)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("checksum"), "{error}");
        // Chunk 1 still has its version 1 in the older index copy.
        let (meta, bytes) = file.read_record(pos(1)).unwrap().unwrap();
        assert_eq!((meta.data_version, bytes), (4001, record(1, 1, 2000)));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Replaced space is reused after a flush, so a file rewritten many times stays small.
    #[test]
    fn space_is_reused() {
        let dir = temp("reuse");
        let path = dir.join("r.0.0.vwr");
        let mut file = VwrFile::open(&path, true).unwrap().unwrap();
        for version in 0..50u8 {
            for slot in 0..32 {
                file.write_record(pos(slot), meta(version), &record(slot, version, 1000))
                    .unwrap();
            }
            file.flush(true).unwrap();
        }
        let length = std::fs::metadata(&path).unwrap().len();
        assert!(length <= DATA_START + 3 * 32 * 1024, "{length}");
        std::fs::remove_dir_all(dir).unwrap();
    }
}

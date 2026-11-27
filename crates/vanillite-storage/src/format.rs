//! World formats (#25): Anvil (`.mca`, Vanilla's) and the own format (`.vwr`, the default for
//! new worlds), their detection, and the offline conversion and export between them.
//!
//! A world is in the own format when `<level>/vanillite/format` exists. Next to it,
//! `vanillite/ids-<data version>.txt.zst` keeps the block state, block, fluid and block entity
//! type names in id order of the data version the records were written with. A server of a
//! later version reads such worlds ([`crate::upgrade`]): it stamps its data version into the
//! marker and lists the older ones (`older-data-versions`) until [`upgrade_world`] rewrote every
//! chunk; their id tables stay. Worlds of a later version are refused. `level.dat`, players and
//! `data/` stay Vanilla files in both formats.
//!
//! A conversion never risks the original: the new files are written next to the old ones,
//! synced, opened again and every record is read back and compared with the source (decoded
//! chunks by their canonical encoding, entities and POI by their NBT bytes). Only then the
//! marker is written (to the own format) or removed (to Anvil), which is the commit point, and
//! the originals are moved into a backup folder or deleted.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use vanillite_base::ChunkPos;
use vanillite_chunk::slots::WorldInfo;
use vanillite_chunk::store::{ChunkCodec, RecordMeta, Stream};
use vanillite_data::version::DATA_VERSION;

use crate::compression::{Compression, zstd_compress, zstd_decompress};
use crate::own_codec::OwnCodec;
use crate::region::RecordRead;
use crate::service::OpenFile;
use crate::upgrade::{Upgrades, id_table_path, id_table_text};
use crate::{AnvilCodec, files};

/// The format of a world's chunk, entity and POI files.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WorldFormat {
    /// Vanilla's region files (`r.<x>.<z>.mca`).
    Anvil,
    /// The own format (`r.<x>.<z>.vwr`, [`crate::vwr`]).
    Vanillite,
}

impl WorldFormat {
    /// The file extension of its region files.
    pub const fn extension(self) -> &'static str {
        match self {
            WorldFormat::Anvil => "mca",
            WorldFormat::Vanillite => "vwr",
        }
    }

    /// The name used in settings and commands.
    pub const fn name(self) -> &'static str {
        match self {
            WorldFormat::Anvil => "anvil",
            WorldFormat::Vanillite => "vanillite",
        }
    }

    fn other(self) -> Self {
        match self {
            WorldFormat::Anvil => WorldFormat::Vanillite,
            WorldFormat::Vanillite => WorldFormat::Anvil,
        }
    }
}

/// The folders of the streams inside a dimension folder.
const STREAMS: [(Stream, &str); 3] = [
    (Stream::Chunk, "region"),
    (Stream::Entities, "entities"),
    (Stream::Poi, "poi"),
];

/// `<level>/vanillite/format`.
pub fn marker_path(level_dir: &Path) -> PathBuf {
    level_dir.join("vanillite").join("format")
}

/// The contents of `vanillite/format`.
struct Marker {
    data_version: i32,
    older: Vec<i32>,
}

fn invalid_marker(level_dir: &Path, message: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("{}: {message}", marker_path(level_dir).display()),
    )
}

/// The world's marker, `None` without one.
fn read_marker(level_dir: &Path) -> io::Result<Option<Marker>> {
    let text = match fs::read_to_string(marker_path(level_dir)) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let value = |key: &str| {
        text.lines()
            .filter_map(|line| line.split_once('='))
            .find(|(k, _)| k.trim() == key)
            .map(|(_, v)| v.trim().trim_matches('"').to_owned())
    };
    if value("format").as_deref() != Some("vwr") {
        return Err(invalid_marker(level_dir, "unknown world format"));
    }
    let data_version = value("data-version")
        .and_then(|v| v.parse::<i32>().ok())
        .ok_or_else(|| invalid_marker(level_dir, "no data version"))?;
    let older = value("older-data-versions")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| {
            v.parse::<i32>()
                .map_err(|_| invalid_marker(level_dir, "invalid older data version"))
        })
        .collect::<io::Result<Vec<_>>>()?;
    Ok(Some(Marker {
        data_version,
        older,
    }))
}

/// Writes the marker of this data version with the older data versions records may have.
fn write_marker(level_dir: &Path, older: &[i32]) -> io::Result<()> {
    let mut text = format!("format = \"vwr\"\nversion = 1\ndata-version = {DATA_VERSION}\n");
    if !older.is_empty() {
        let list: Vec<String> = older.iter().map(i32::to_string).collect();
        text.push_str(&format!("older-data-versions = \"{}\"\n", list.join(",")));
    }
    files::write_atomic(&marker_path(level_dir), text.as_bytes(), false)
}

/// The older data versions whose records an own-format world may still have.
pub fn older_data_versions(level_dir: &Path) -> io::Result<Vec<i32>> {
    let Some(marker) = read_marker(level_dir)? else {
        return Ok(Vec::new());
    };
    let mut older = marker.older;
    older.push(marker.data_version);
    older.retain(|&v| v != DATA_VERSION);
    older.sort_unstable();
    older.dedup();
    Ok(older)
}

/// The format of an existing world, `None` for a new one. An own-format world written by a
/// later data version is an error: its ids are unknown here, and loading would regenerate
/// every chunk.
pub fn detect(level_dir: &Path, dimension_dirs: &[PathBuf]) -> io::Result<Option<WorldFormat>> {
    if let Some(marker) = read_marker(level_dir)? {
        if marker.data_version > DATA_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "the world was written in the own format by data version {}, this server is \
                     {DATA_VERSION}: worlds cannot go back to an older Minecraft version; export \
                     it with the newer server (`vanillite export`)",
                    marker.data_version
                ),
            ));
        }
        return Ok(Some(WorldFormat::Vanillite));
    }
    let anvil = level_dir.join("level.dat").exists()
        || dimension_dirs
            .iter()
            .any(|dir| count_files(dir, WorldFormat::Anvil) > 0);
    Ok(anvil.then_some(WorldFormat::Anvil))
}

/// Region files of `format` in a dimension folder.
pub fn count_files(dimension_dir: &Path, format: WorldFormat) -> usize {
    STREAMS
        .iter()
        .map(|(_, folder)| region_files(&dimension_dir.join(folder), format).len())
        .sum()
}

/// The region files `r.<x>.<z>.<ext>` of a folder with their region coordinates.
fn region_files(dir: &Path, format: WorldFormat) -> Vec<(PathBuf, (i32, i32))> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<(PathBuf, (i32, i32))> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let mut parts = name.split('.');
            let (r, x, z, ext) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
            (r == "r" && ext == format.extension() && parts.next().is_none())
                .then_some(())
                .and_then(|()| Some((entry.path(), (x.parse().ok()?, z.parse().ok()?))))
        })
        .collect();
    files.sort();
    files
}

/// Marks a world as an own-format world whose records all have this data version and writes
/// its id table.
pub fn mark_own_format(level_dir: &Path) -> io::Result<()> {
    write_id_table(level_dir)?;
    write_marker(level_dir, &[])
}

/// Before an own-format world of an older data version is written to: writes this version's id
/// table and stamps it into the marker, keeping the older versions its records may still have.
/// Returns the older data version the marker had.
pub fn stamp_current(level_dir: &Path) -> io::Result<Option<i32>> {
    let Some(marker) = read_marker(level_dir)? else {
        return Ok(None);
    };
    if marker.data_version == DATA_VERSION {
        return Ok(None);
    }
    let older = older_data_versions(level_dir)?;
    write_id_table(level_dir)?;
    write_marker(level_dir, &older)?;
    log::info!(
        "The world was written by data version {}, this server is {DATA_VERSION}: chunks are \
         upgraded when they are saved; `vanillite upgrade-world` rewrites all of them",
        marker.data_version
    );
    Ok(Some(marker.data_version))
}

/// `vanillite/ids-<data version>.txt.zst`: the names of the ids the records use, in id order.
fn write_id_table(level_dir: &Path) -> io::Result<()> {
    let path = id_table_path(level_dir, DATA_VERSION);
    if path.exists() {
        return Ok(());
    }
    files::write_atomic(
        &path,
        // A one-shot context: the per-thread one of `zstd_compress` would keep the level-19
        // context (about 50 MiB) alive for the server's lifetime.
        &zstd::bulk::compress(id_table_text().as_bytes(), 19)?,
        false,
    )
}

/// Settings of a conversion or an export.
#[derive(Clone, Debug)]
pub struct ConvertSettings {
    /// Threads that convert files in parallel.
    pub threads: usize,
    /// Compression of chunks written to Anvil.
    pub anvil_compression: Compression,
    /// zstd level of records written in the own format.
    pub level: i32,
    /// Folder the original files are moved into after a verified conversion, keeping their
    /// path below the server folder; `None` deletes them.
    pub backup: Option<PathBuf>,
}

/// What a conversion did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConvertStats {
    /// Region files converted.
    pub files: u64,
    /// Chunks converted.
    pub chunks: u64,
    /// Entity and POI records converted.
    pub records: u64,
    /// Bytes of the source files.
    pub bytes_in: u64,
    /// Bytes of the written files.
    pub bytes_out: u64,
}

impl ConvertStats {
    fn add(&mut self, other: ConvertStats) {
        self.files += other.files;
        self.chunks += other.chunks;
        self.records += other.records;
        self.bytes_in += other.bytes_in;
        self.bytes_out += other.bytes_out;
    }
}

/// One region file to convert.
struct Job {
    dimension: usize,
    stream: Stream,
    region: (i32, i32),
    source: PathBuf,
    target: PathBuf,
}

/// The codecs of one dimension.
struct Codecs {
    anvil: AnvilCodec,
    own: OwnCodec,
}

/// Converts the chunk, entity and POI files of a world in place into `to` (offline: the world
/// must not be running). `level_dir` holds the marker; `dimensions` are the dimensions with
/// their folders. On an error the original files are untouched and the new ones removed.
pub fn convert_world(
    server_dir: &Path,
    level_dir: &Path,
    dimensions: &[WorldInfo],
    to: WorldFormat,
    settings: &ConvertSettings,
) -> io::Result<ConvertStats> {
    let from = to.other();
    let mut jobs = Vec::new();
    for (index, info) in dimensions.iter().enumerate() {
        for (stream, folder) in STREAMS {
            let dir = info.dir.join(folder);
            // Files of the target format in the folder are not part of the world: without a
            // marker `.vwr` files are leftovers of an interrupted conversion; `.mca` files
            // next to an own-format world may be Vanilla's (opened by mistake), so they stop
            // the conversion instead of being overwritten.
            for (path, _) in region_files(&dir, to) {
                if to == WorldFormat::Vanillite {
                    fs::remove_file(&path)?;
                } else {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        format!(
                            "{} is not part of the own-format world; move it away first",
                            path.display()
                        ),
                    ));
                }
            }
            for (source, region) in region_files(&dir, from) {
                let target = source.with_extension(to.extension());
                jobs.push(Job {
                    dimension: index,
                    stream,
                    region,
                    source,
                    target,
                });
            }
        }
    }
    let started = Instant::now();
    log::info!(
        "Converting {} region files of {} from {} to {}",
        jobs.len(),
        level_dir.display(),
        from.name(),
        to.name()
    );
    let upgrades = match from {
        WorldFormat::Vanillite => Upgrades::load(level_dir)?,
        WorldFormat::Anvil => Upgrades::none(),
    };
    let stats = match run_jobs(&jobs, dimensions, from, to, settings, &upgrades) {
        Ok(stats) => stats,
        Err(e) => {
            for job in &jobs {
                let _ = fs::remove_file(&job.target);
            }
            return Err(e);
        }
    };
    // The commit point.
    match to {
        WorldFormat::Vanillite => mark_own_format(level_dir)?,
        WorldFormat::Anvil => match fs::remove_file(marker_path(level_dir)) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        },
    }
    for job in &jobs {
        let mut originals = vec![job.source.clone()];
        if from == WorldFormat::Anvil
            && let Some(dir) = job.source.parent()
        {
            // External chunks (`c.<x>.<z>.mcc`) of the region.
            for slot in 0..1024 {
                let pos = ChunkPos::new(
                    job.region.0 * 32 + (slot & 31),
                    job.region.1 * 32 + (slot / 32),
                );
                let external = dir.join(format!("c.{}.{}.mcc", pos.x, pos.z));
                if external.exists() {
                    originals.push(external);
                }
            }
        }
        for path in originals {
            let result = match &settings.backup {
                Some(backup) => {
                    let relative = path.strip_prefix(server_dir).unwrap_or(&path);
                    let moved = backup.join(relative);
                    moved
                        .parent()
                        .map_or(Ok(()), fs::create_dir_all)
                        .and_then(|()| fs::rename(&path, &moved))
                }
                None => fs::remove_file(&path),
            };
            if let Err(e) = result {
                log::warn!("Couldn't remove the converted file {}: {e}", path.display());
            }
        }
    }
    log::info!(
        "Converted {} chunks and {} entity and POI records in {:.1} s: {:.1} MiB of {} became \
         {:.1} MiB of {}{}",
        stats.chunks,
        stats.records,
        started.elapsed().as_secs_f64(),
        stats.bytes_in as f64 / 1048576.0,
        from.name(),
        stats.bytes_out as f64 / 1048576.0,
        to.name(),
        settings.backup.as_ref().map_or(String::new(), |b| format!(
            "; the original files are in {}",
            b.display()
        ))
    );
    Ok(stats)
}

/// Writes a complete Vanilla (Anvil) copy of an own-format world into `target`, keeping the
/// paths of `level_dirs` below `server_dir` (the first holds the marker): `level.dat`, players,
/// saved data and every other file are copied, chunks, entities and POI converted, chunks of
/// older data versions upgraded. The world is only read.
pub fn export_world(
    server_dir: &Path,
    level_dirs: &[PathBuf],
    dimensions: &[WorldInfo],
    target: &Path,
    settings: &ConvertSettings,
) -> io::Result<ConvertStats> {
    let relative = |path: &Path| path.strip_prefix(server_dir).unwrap_or(path).to_path_buf();
    for level in level_dirs {
        let out = target.join(relative(level));
        if out.exists() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} exists already", out.display()),
            ));
        }
    }
    for level in level_dirs {
        copy_tree(level, &target.join(relative(level)), true)?;
    }
    let mut jobs = Vec::new();
    for (index, info) in dimensions.iter().enumerate() {
        for (stream, folder) in STREAMS {
            let out = target.join(relative(&info.dir)).join(folder);
            for (source, region) in region_files(&info.dir.join(folder), WorldFormat::Vanillite) {
                fs::create_dir_all(&out)?;
                let target = out.join(format!("r.{}.{}.mca", region.0, region.1));
                jobs.push(Job {
                    dimension: index,
                    stream,
                    region,
                    source,
                    target,
                });
            }
        }
    }
    let upgrades = match level_dirs.first() {
        Some(level) => Upgrades::load(level)?,
        None => Upgrades::none(),
    };
    let started = Instant::now();
    let stats = run_jobs(
        &jobs,
        dimensions,
        WorldFormat::Vanillite,
        WorldFormat::Anvil,
        settings,
        &upgrades,
    )?;
    log::info!(
        "Exported {} chunks and {} entity and POI records to {} in {:.1} s",
        stats.chunks,
        stats.records,
        target.display(),
        started.elapsed().as_secs_f64()
    );
    Ok(stats)
}

/// Rewrites every chunk record of an own-format world in this data version (offline: the world
/// must not be running). Each region file is written next to the old one, verified and then
/// renamed over it (with `settings.backup` a copy of the old one is kept); records of not yet
/// rewritten files stay readable until the marker loses its older data versions at the end.
/// Entity and POI records are Vanilla NBT by name and keep their data version.
pub fn upgrade_world(
    server_dir: &Path,
    level_dir: &Path,
    dimensions: &[WorldInfo],
    settings: &ConvertSettings,
) -> io::Result<ConvertStats> {
    stamp_current(level_dir)?;
    let upgrades = Upgrades::load(level_dir)?;
    let mut jobs = Vec::new();
    for (index, info) in dimensions.iter().enumerate() {
        for (source, region) in region_files(&info.dir.join("region"), WorldFormat::Vanillite) {
            jobs.push(Job {
                dimension: index,
                stream: Stream::Chunk,
                region,
                target: source.with_extension("vwr.tmp"),
                source,
            });
        }
    }
    let started = Instant::now();
    log::info!(
        "Upgrading {} region files of {} to data version {DATA_VERSION}",
        jobs.len(),
        level_dir.display()
    );
    let stats = match run_jobs(
        &jobs,
        dimensions,
        WorldFormat::Vanillite,
        WorldFormat::Vanillite,
        settings,
        &upgrades,
    ) {
        Ok(stats) => stats,
        Err(e) => {
            for job in &jobs {
                let _ = fs::remove_file(&job.target);
            }
            return Err(e);
        }
    };
    for job in &jobs {
        if let Some(backup) = &settings.backup {
            let relative = job.source.strip_prefix(server_dir).unwrap_or(&job.source);
            let copy = backup.join(relative);
            copy.parent().map_or(Ok(()), fs::create_dir_all)?;
            fs::copy(&job.source, &copy)?;
        }
        fs::rename(&job.target, &job.source)?;
    }
    // The commit point: every record has this data version now.
    mark_own_format(level_dir)?;
    log::info!(
        "Upgraded {} chunks to data version {DATA_VERSION} in {:.1} s{}",
        stats.chunks,
        started.elapsed().as_secs_f64(),
        settings.backup.as_ref().map_or(String::new(), |b| format!(
            "; the old files are in {}",
            b.display()
        ))
    );
    Ok(stats)
}

/// Copies a folder; with `skip_own` without own-format files, `vanillite/` and the lock.
fn copy_tree(from: &Path, to: &Path, skip_own: bool) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let path = entry.path();
        if skip_own
            && (name.ends_with(".vwr")
                || name == "session.lock"
                || name.ends_with(".tmp")
                || (name == "vanillite" && path.join("format").exists()))
        {
            continue;
        }
        if entry.file_type()?.is_dir() {
            copy_tree(&path, &to.join(&*name), skip_own)?;
        } else {
            fs::copy(&path, to.join(&*name))?;
        }
    }
    Ok(())
}

/// Converts every job on `settings.threads` threads, largest files first.
fn run_jobs(
    jobs: &[Job],
    dimensions: &[WorldInfo],
    from: WorldFormat,
    to: WorldFormat,
    settings: &ConvertSettings,
    upgrades: &std::sync::Arc<Upgrades>,
) -> io::Result<ConvertStats> {
    let codecs: Vec<Codecs> = dimensions
        .iter()
        .map(|info| Codecs {
            anvil: AnvilCodec::new(info, settings.anvil_compression),
            own: OwnCodec::new(info, settings.level).with_upgrades(upgrades.clone()),
        })
        .collect();
    let mut order: Vec<(u64, usize)> = jobs
        .iter()
        .enumerate()
        .map(|(i, job)| (fs::metadata(&job.source).map_or(0, |m| m.len()), i))
        .collect();
    order.sort_unstable_by(|a, b| b.cmp(a));
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let result: Mutex<io::Result<ConvertStats>> = Mutex::new(Ok(ConvertStats::default()));
    let failed = || result.lock().map_or(true, |r| r.is_err());
    std::thread::scope(|scope| {
        for _ in 0..settings.threads.clamp(1, jobs.len().max(1)) {
            scope.spawn(|| {
                while !failed() {
                    let Some(&(_, index)) = order.get(next.fetch_add(1, Ordering::Relaxed)) else {
                        return;
                    };
                    let job = &jobs[index];
                    let outcome = convert_file(job, &codecs[job.dimension], from, to, settings)
                        .map_err(|e| {
                            io::Error::new(e.kind(), format!("{}: {e}", job.source.display()))
                        });
                    let mut result = result.lock().unwrap_or_else(|e| e.into_inner());
                    match (&mut *result, outcome) {
                        (Ok(total), Ok(stats)) => total.add(stats),
                        (Ok(_), Err(e)) => *result = Err(e),
                        (Err(_), _) => {}
                    }
                    drop(result);
                    let done = done.fetch_add(1, Ordering::Relaxed) + 1;
                    if done.is_multiple_of(64) {
                        log::info!("Converted {done} of {} region files", jobs.len());
                    }
                }
            });
        }
    });
    result.into_inner().unwrap_or_else(|e| e.into_inner())
}

/// Reads every record of a file, in 4 × 4 tile order.
fn read_all(
    file: &OpenFile,
    region: (i32, i32),
    mut each: impl FnMut(RecordRead) -> io::Result<()>,
) -> io::Result<()> {
    let mut out = Vec::with_capacity(16);
    for tile in 0..64 {
        let (tx, tz) = ((tile % 8) * 4, (tile / 8) * 4);
        let slots: Vec<usize> = (0..16).map(|i| tx + i % 4 + (tz + i / 4) * 32).collect();
        out.clear();
        file.read_many(region, &slots, &[], &mut out);
        for read in out.drain(..) {
            each(read)?;
        }
    }
    Ok(())
}

fn pos_of(region: (i32, i32), slot: usize) -> ChunkPos {
    ChunkPos::new(
        region.0 * 32 + (slot & 31) as i32,
        region.1 * 32 + (slot / 32) as i32,
    )
}

/// Decodes a chunk record of `format`.
fn decode_chunk(
    codecs: &Codecs,
    format: WorldFormat,
    pos: ChunkPos,
    meta: &RecordMeta,
    record: &[u8],
) -> io::Result<vanillite_chunk::ProtoChunk> {
    match format {
        WorldFormat::Anvil => codecs.anvil.decode(pos, meta, record),
        WorldFormat::Vanillite => codecs.own.decode(pos, meta, record),
    }
}

/// The NBT of an entity or POI record of `format`.
fn decode_nbt(format: WorldFormat, record: &[u8]) -> io::Result<Vec<u8>> {
    match format {
        WorldFormat::Anvil => {
            let (&id, data) = record
                .split_first()
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "empty record"))?;
            Compression::from_id(id)
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, format!("compression {id}"))
                })?
                .decompress(data)
        }
        WorldFormat::Vanillite => zstd_decompress(record),
    }
}

fn hash(bytes: &[u8]) -> u64 {
    twox_hash::XxHash3_64::oneshot(bytes)
}

/// Converts one region file and verifies the result.
fn convert_file(
    job: &Job,
    codecs: &Codecs,
    from: WorldFormat,
    to: WorldFormat,
    settings: &ConvertSettings,
) -> io::Result<ConvertStats> {
    let Some(source) = OpenFile::open(from, &job.source, false)? else {
        return Ok(ConvertStats::default());
    };
    let _ = fs::remove_file(&job.target);
    let mut target = OpenFile::open(to, &job.target, true)?
        .ok_or_else(|| io::Error::other("cannot create the file"))?;
    let mut stats = ConvertStats {
        files: 1,
        ..ConvertStats::default()
    };
    // Hash of the canonical form of every record written, by slot.
    let mut expected: Vec<Option<u64>> = vec![None; 1024];
    read_all(&source, job.region, |read| {
        let pos = pos_of(job.region, read.slot);
        let Some(record) = read.record? else {
            return Ok(());
        };
        let at =
            |e: io::Error| io::Error::new(e.kind(), format!("chunk {}, {}: {e}", pos.x, pos.z));
        if job.stream == Stream::Chunk {
            let chunk = decode_chunk(codecs, from, pos, &read.meta, &record).map_err(at)?;
            let (meta, raw) = codecs.own.encode_raw(&chunk);
            expected[read.slot] = Some(hash(&raw));
            let (meta, stored) = match to {
                WorldFormat::Vanillite => (meta, zstd_compress(&raw, settings.level)?),
                WorldFormat::Anvil => {
                    let (meta, stored) = codecs.anvil.encode(&chunk).map_err(at)?;
                    (meta, stored.to_vec())
                }
            };
            target.write(pos, meta, &stored).map_err(at)?;
            stats.chunks += 1;
        } else {
            let nbt = decode_nbt(from, &record).map_err(at)?;
            expected[read.slot] = Some(hash(&nbt));
            let (meta, stored) = match to {
                WorldFormat::Vanillite => (
                    RecordMeta {
                        raw_len: nbt.len() as u32,
                        ..RecordMeta::default()
                    },
                    zstd_compress(&nbt, settings.level)?,
                ),
                WorldFormat::Anvil => {
                    let mut stored = vec![settings.anvil_compression.id()];
                    stored.extend_from_slice(&settings.anvil_compression.compress(&nbt)?);
                    (RecordMeta::default(), stored)
                }
            };
            target.write(pos, meta, &stored).map_err(at)?;
            stats.records += 1;
        }
        Ok(())
    })?;
    target.commit(true)?;
    drop(target);
    drop(source);

    // Verify: every record read back from the new file decodes to what was written.
    let target = OpenFile::open(to, &job.target, false)?
        .ok_or_else(|| io::Error::other("the new file is gone"))?;
    let mut found = 0;
    read_all(&target, job.region, |read| {
        let pos = pos_of(job.region, read.slot);
        let Some(record) = read.record? else {
            return Ok(());
        };
        found += 1;
        let digest = if job.stream == Stream::Chunk {
            let chunk = decode_chunk(codecs, to, pos, &read.meta, &record)?;
            hash(&codecs.own.encode_raw(&chunk).1)
        } else {
            hash(&decode_nbt(to, &record)?)
        };
        if expected[read.slot] != Some(digest) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{}: chunk {}, {} differs after conversion",
                    job.target.display(),
                    pos.x,
                    pos.z
                ),
            ));
        }
        Ok(())
    })?;
    if found != expected.iter().flatten().count() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{}: records missing after conversion", job.target.display()),
        ));
    }
    stats.bytes_in = fs::metadata(&job.source)?.len();
    stats.bytes_out = fs::metadata(&job.target)?.len();
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk_nbt::tests::{codec, diff, golden, without};
    use crate::region::RegionFile;
    use vanillite_chunk::ChunkStatus;
    use vanillite_nbt::{Budget, Tag};

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("vanillite-format-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn region_path(dir: &Path, folder: &str, pos: ChunkPos, ext: &str) -> PathBuf {
        let (x, z) = pos.region();
        dir.join(folder).join(format!("r.{x}.{z}.{ext}"))
    }

    /// An Anvil world with Vanilla's golden chunks and an entity record per chunk.
    fn anvil_world(level: &Path) -> (WorldInfo, Vec<(ChunkPos, Vec<u8>, vanillite_nbt::Compound)>) {
        let chunks = golden();
        let mut info = codec(&chunks).info().clone();
        info.dir = level.to_path_buf();
        for (pos, bytes, _) in &chunks {
            for folder in ["region", "entities"] {
                let path = region_path(level, folder, *pos, "mca");
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                let mut file = RegionFile::open(&path, true).unwrap().unwrap();
                file.write(
                    *pos,
                    Compression::Deflate,
                    &Compression::Deflate.compress(bytes).unwrap(),
                )
                .unwrap();
            }
        }
        fs::write(level.join("level.dat"), b"level").unwrap();
        (info, chunks)
    }

    fn settings(backup: Option<PathBuf>) -> ConvertSettings {
        ConvertSettings {
            threads: 2,
            anvil_compression: Compression::Deflate,
            level: 3,
            backup,
        }
    }

    /// Reads a chunk of an Anvil world and compares it with Vanilla's original.
    fn assert_same_as_vanilla(
        dir: &Path,
        chunks: &[(ChunkPos, Vec<u8>, vanillite_nbt::Compound)],
        info: &WorldInfo,
    ) {
        let anvil = crate::Codec::new(info);
        for (pos, bytes, root) in chunks {
            let mut file = RegionFile::open(&region_path(dir, "region", *pos, "mca"), false)
                .unwrap()
                .unwrap();
            let (compression, data) = file.read(*pos).unwrap().unwrap();
            let nbt = compression.decompress(&data).unwrap();
            let read =
                vanillite_nbt::read::read_named(&mut nbt.as_slice(), &mut Budget::file()).unwrap();
            let full = anvil.read(*pos, read.clone()).unwrap().status == ChunkStatus::Full;
            let ignored: &[&str] = if full {
                &["DataVersion"]
            } else {
                &["DataVersion", "Heightmaps"]
            };
            let mut diffs = Vec::new();
            diff(
                &Tag::Compound(without(&read, ignored)),
                &Tag::Compound(without(root, ignored)),
                String::new(),
                &mut diffs,
            );
            assert!(diffs.is_empty(), "{pos:?}: {diffs:#?}");
            let (compression, data) =
                RegionFile::open(&region_path(dir, "entities", *pos, "mca"), false)
                    .unwrap()
                    .unwrap()
                    .read(*pos)
                    .unwrap()
                    .unwrap();
            assert_eq!(
                &compression.decompress(&data).unwrap(),
                bytes,
                "entities {pos:?}"
            );
        }
    }

    /// Anvil → own format → Anvil and an export give Vanilla's chunks and entities back; the
    /// originals are moved into the backup folder only after the new files were verified.
    #[test]
    fn convert_and_export_keep_every_chunk() {
        let server = temp("round-trip");
        let level = server.join("world");
        let (info, chunks) = anvil_world(&level);
        let dims = [level.clone()];
        assert_eq!(detect(&level, &dims).unwrap(), Some(WorldFormat::Anvil));

        let backup = server.join("backup");
        let stats = convert_world(
            &server,
            &level,
            std::slice::from_ref(&info),
            WorldFormat::Vanillite,
            &settings(Some(backup.clone())),
        )
        .unwrap();
        assert_eq!((stats.chunks, stats.records), (3, 3));
        assert_eq!(detect(&level, &dims).unwrap(), Some(WorldFormat::Vanillite));
        assert_eq!(count_files(&level, WorldFormat::Anvil), 0);
        assert_eq!(count_files(&backup.join("world"), WorldFormat::Anvil), 4);
        assert!(
            level
                .join("vanillite")
                .join(format!("ids-{DATA_VERSION}.txt.zst"))
                .exists()
        );

        let export = server.join("export");
        export_world(
            &server,
            std::slice::from_ref(&level),
            std::slice::from_ref(&info),
            &export,
            &settings(None),
        )
        .unwrap();
        assert!(export.join("world/level.dat").exists());
        assert!(!marker_path(&export.join("world")).exists());
        assert_eq!(
            count_files(&export.join("world"), WorldFormat::Vanillite),
            0
        );
        assert_same_as_vanilla(&export.join("world"), &chunks, &info);

        convert_world(
            &server,
            &level,
            std::slice::from_ref(&info),
            WorldFormat::Anvil,
            &settings(None),
        )
        .unwrap();
        assert_eq!(detect(&level, &dims).unwrap(), Some(WorldFormat::Anvil));
        assert_eq!(count_files(&level, WorldFormat::Vanillite), 0);
        assert_same_as_vanilla(&level, &chunks, &info);
        fs::remove_dir_all(server).unwrap();
    }

    /// A chunk that cannot be read stops the conversion; the world stays Anvil, untouched.
    #[test]
    fn a_failed_conversion_keeps_the_original() {
        let server = temp("failed");
        let level = server.join("world");
        let (info, chunks) = anvil_world(&level);
        let path = region_path(&level, "region", chunks[0].0, "mca");
        let mut file = RegionFile::open(&path, false).unwrap().unwrap();
        file.write(
            ChunkPos::new(chunks[0].0.x + 1, chunks[0].0.z),
            Compression::Deflate,
            b"garbage",
        )
        .unwrap();
        drop(file);
        let before = fs::read(&path).unwrap();
        let error = convert_world(
            &server,
            &level,
            std::slice::from_ref(&info),
            WorldFormat::Vanillite,
            &settings(Some(server.join("backup"))),
        )
        .unwrap_err();
        assert!(error.to_string().contains("chunk"), "{error}");
        assert_eq!(
            detect(&level, std::slice::from_ref(&level)).unwrap(),
            Some(WorldFormat::Anvil)
        );
        assert_eq!(count_files(&level, WorldFormat::Vanillite), 0);
        assert_eq!(fs::read(&path).unwrap(), before);
        assert!(!server.join("backup").exists());
        fs::remove_dir_all(server).unwrap();
    }

    /// A world marked by a later data version is refused instead of being regenerated; one of
    /// an older version is read.
    #[test]
    fn later_data_versions_are_refused() {
        let server = temp("version");
        let level = server.join("world");
        fs::create_dir_all(level.join("vanillite")).unwrap();
        let later = DATA_VERSION + 1;
        fs::write(
            marker_path(&level),
            format!("format = \"vwr\"\ndata-version = {later}\n"),
        )
        .unwrap();
        let error = detect(&level, std::slice::from_ref(&level)).unwrap_err();
        assert!(
            error.to_string().contains(&format!("data version {later}")),
            "{error}"
        );
        fs::write(marker_path(&level), "format = \"vwr\"\ndata-version = 1\n").unwrap();
        assert_eq!(
            detect(&level, std::slice::from_ref(&level)).unwrap(),
            Some(WorldFormat::Vanillite)
        );
        assert_eq!(detect(&server.join("new"), &[]).unwrap(), None);
        fs::remove_dir_all(server).unwrap();
    }

    /// An own-format world of an older data version (made-up id table; block entity types stay,
    /// as their stored data names them) is stamped with this
    /// one, exported with its chunks upgraded, and `upgrade_world` rewrites every chunk record
    /// in this data version, after which the marker lists no older version.
    #[test]
    fn older_worlds_are_exported_and_upgraded() {
        use crate::upgrade::tests::older_table;
        let server = temp("upgrade");
        let level = server.join("world");
        let (info, _) = anvil_world(&level);
        let dims = [level.clone()];
        convert_world(
            &server,
            &level,
            std::slice::from_ref(&info),
            WorldFormat::Vanillite,
            &settings(None),
        )
        .unwrap();

        // Make the chunk records and the marker those of an older data version.
        let old = DATA_VERSION - 1;
        files::write_atomic(
            &id_table_path(&level, old),
            &zstd_compress(older_table(10, false).as_bytes(), 3).unwrap(),
            false,
        )
        .unwrap();
        fs::write(
            marker_path(&level),
            format!("format = \"vwr\"\ndata-version = {old}\n"),
        )
        .unwrap();
        for (path, region) in region_files(&level.join("region"), WorldFormat::Vanillite) {
            let mut records = Vec::new();
            let file = OpenFile::open(WorldFormat::Vanillite, &path, false)
                .unwrap()
                .unwrap();
            read_all(&file, region, |read| {
                if let Some(record) = read.record? {
                    let mut raw = zstd_decompress(&record)?;
                    raw[3..7].copy_from_slice(&(old as u32).to_le_bytes());
                    records.push((read.slot, read.meta, raw));
                }
                Ok(())
            })
            .unwrap();
            drop(file);
            let mut file = OpenFile::open(WorldFormat::Vanillite, &path, true)
                .unwrap()
                .unwrap();
            for (slot, meta, raw) in records {
                let meta = RecordMeta {
                    data_version: old as u32,
                    ..meta
                };
                file.write(pos_of(region, slot), meta, &zstd_compress(&raw, 3).unwrap())
                    .unwrap();
            }
            file.commit(true).unwrap();
        }
        assert_eq!(detect(&level, &dims).unwrap(), Some(WorldFormat::Vanillite));
        assert_eq!(stamp_current(&level).unwrap(), Some(old));
        assert_eq!(stamp_current(&level).unwrap(), None);
        assert_eq!(older_data_versions(&level).unwrap(), [old]);

        // What the chunks are with the current ids.
        let upgraded = OwnCodec::new(&info, 3).with_upgrades(Upgrades::load(&level).unwrap());
        let chunks = |codec: &OwnCodec, version: u32| {
            let mut out = Vec::new();
            for (path, region) in region_files(&level.join("region"), WorldFormat::Vanillite) {
                let file = OpenFile::open(WorldFormat::Vanillite, &path, false)
                    .unwrap()
                    .unwrap();
                read_all(&file, region, |read| {
                    if let Some(record) = read.record? {
                        assert_eq!(read.meta.data_version, version);
                        let pos = pos_of(region, read.slot);
                        let chunk = codec.decode(pos, &read.meta, &record)?;
                        out.push((pos, codec.encode_raw(&chunk).1));
                    }
                    Ok(())
                })
                .unwrap();
            }
            out
        };
        let expected = chunks(&upgraded, old as u32);
        assert_eq!(expected.len(), 3);

        let stats = export_world(
            &server,
            std::slice::from_ref(&level),
            std::slice::from_ref(&info),
            &server.join("export"),
            &settings(None),
        )
        .unwrap();
        assert_eq!(stats.chunks, 3);

        let stats = upgrade_world(
            &server,
            &level,
            std::slice::from_ref(&info),
            &settings(Some(server.join("backup"))),
        )
        .unwrap();
        assert_eq!(stats.chunks, 3);
        assert!(older_data_versions(&level).unwrap().is_empty());
        assert_eq!(
            chunks(&OwnCodec::new(&info, 3), DATA_VERSION as u32),
            expected
        );
        assert_eq!(
            count_files(&server.join("backup/world"), WorldFormat::Vanillite),
            region_files(&level.join("region"), WorldFormat::Vanillite).len()
        );
        assert!(
            fs::read_dir(level.join("region")).unwrap().all(|e| !e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp"))
        );
        fs::remove_dir_all(server).unwrap();
    }
}

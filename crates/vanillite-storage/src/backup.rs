//! Backups: zip archives of a world's folders, made while the server runs.
//!
//! The server saves first, then a background thread copies the files. Small files are written
//! atomically (temp file and rename) and copied whole; region files are copied while no chunk
//! is written into them ([`gate`](crate::region::gate)), so every chunk in the archive is
//! complete. Ticks never wait for a backup.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, ZipWriter};

/// Where and how backups are written.
#[derive(Clone, Debug)]
pub struct BackupSettings {
    /// The folder of the archives.
    pub directory: PathBuf,
    /// Archives kept; older ones are deleted. 0 keeps all.
    pub keep: u32,
    /// Deflate level 1..=9; 0 stores the files uncompressed.
    pub compression_level: u8,
}

/// Writes `<directory>/<name>-<UTC date and time>.zip` with the folders `sources` (each under
/// its own folder name), then deletes the oldest archives of `name` beyond `keep`. Returns the
/// archive.
pub fn create_backup(
    name: &str,
    sources: &[PathBuf],
    settings: &BackupSettings,
) -> io::Result<PathBuf> {
    fs::create_dir_all(&settings.directory)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let (y, mo, d, h, mi, s) = utc(now);
    let file_name = format!("{name}-{y:04}-{mo:02}-{d:02}_{h:02}-{mi:02}-{s:02}.zip");
    let path = settings.directory.join(&file_name);
    let tmp = settings.directory.join(format!("{file_name}.tmp"));
    let skip = fs::canonicalize(&settings.directory).ok();
    let result = (|| {
        let mut zip = ZipWriter::new(io::BufWriter::new(File::create(&tmp)?));
        for source in sources {
            let Some(root) = source.file_name() else {
                continue;
            };
            if !source.is_dir() {
                continue;
            }
            add_dir(
                &mut zip,
                source,
                &root.to_string_lossy(),
                settings,
                skip.as_deref(),
            )?;
        }
        let mut out = zip.finish().map_err(io::Error::other)?;
        out.flush()?;
        out.into_inner()
            .map_err(io::IntoInnerError::into_error)?
            .sync_all()
    })();
    if let Err(e) = result {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    fs::rename(&tmp, &path)?;
    prune(&settings.directory, name, settings.keep)?;
    Ok(path)
}

fn add_dir<W: Write + io::Seek>(
    zip: &mut ZipWriter<W>,
    dir: &Path,
    prefix: &str,
    settings: &BackupSettings,
    skip: Option<&Path>,
) -> io::Result<()> {
    let mut entries: Vec<_> = fs::read_dir(dir)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        let kind = entry.file_type()?;
        let inner = format!("{prefix}/{name}");
        if kind.is_dir() {
            if skip.is_some_and(|s| fs::canonicalize(&path).is_ok_and(|p| p == s)) {
                continue;
            }
            add_dir(zip, &path, &inner, settings, skip)?;
        } else if kind.is_file() && name != "session.lock" && !name.ends_with(".tmp") {
            add_file(zip, &path, &inner, &name, settings)?;
        }
    }
    Ok(())
}

fn add_file<W: Write + io::Seek>(
    zip: &mut ZipWriter<W>,
    path: &Path,
    inner: &str,
    name: &str,
    settings: &BackupSettings,
) -> io::Result<()> {
    // Region files change in place: copy them while no chunk is written.
    let data = {
        let _gate = name.ends_with(".mca").then(|| crate::region::gate(name));
        match File::open(path) {
            Ok(mut file) => {
                let mut data =
                    Vec::with_capacity(usize::try_from(file.metadata()?.len()).unwrap_or_default());
                file.read_to_end(&mut data)?;
                data
            }
            // Renamed or removed since the folder was listed.
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        }
    };
    let modified = fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    let (y, mo, d, h, mi, s) = utc(modified);
    let time = DateTime::from_date_and_time(u16::try_from(y).unwrap_or(1980), mo, d, h, mi, s)
        .unwrap_or_default();
    let options = SimpleFileOptions::default()
        .compression_method(if settings.compression_level == 0 {
            CompressionMethod::Stored
        } else {
            CompressionMethod::Deflated
        })
        .compression_level(
            (settings.compression_level > 0).then(|| i64::from(settings.compression_level)),
        )
        .last_modified_time(time)
        .large_file(data.len() >= 0xFFFF_FFFF);
    zip.start_file(inner, options).map_err(io::Error::other)?;
    zip.write_all(&data)
}

/// Deletes the oldest archives `<name>-*.zip` in `dir` so that `keep` remain (0: none).
fn prune(dir: &Path, name: &str, keep: u32) -> io::Result<()> {
    if keep == 0 {
        return Ok(());
    }
    let prefix = format!("{name}-");
    let mut archives: Vec<PathBuf> = fs::read_dir(dir)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(&prefix) && n.ends_with(".zip"))
        })
        .collect();
    archives.sort();
    let excess = archives
        .len()
        .saturating_sub(usize::try_from(keep).unwrap_or(usize::MAX));
    for old in archives.into_iter().take(excess) {
        log::info!("Deleting old backup {}", old.display());
        fs::remove_file(old)?;
    }
    Ok(())
}

/// Year, month, day, hour, minute, second (UTC) of Unix time `secs`.
fn utc(secs: u64) -> (i64, u8, u8, u8, u8, u8) {
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rest = secs % 86_400;
    // Howard Hinnant's `civil_from_days`.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let small = |v: i64| u8::try_from(v).unwrap_or(0);
    let part = |v: u64| u8::try_from(v).unwrap_or(0);
    (
        year,
        small(month),
        small(day),
        part(rest / 3600),
        part(rest / 60 % 60),
        part(rest % 60),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_dates() {
        assert_eq!(utc(0), (1970, 1, 1, 0, 0, 0));
        assert_eq!(utc(1_795_556_538), (2026, 11, 24, 21, 42, 18));
        assert_eq!(utc(951_782_400), (2000, 2, 29, 0, 0, 0));
    }

    #[test]
    fn backup_archives_the_world_and_prunes() {
        let root = std::env::temp_dir().join(format!("vanillite-backup-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let world = root.join("world");
        fs::create_dir_all(world.join("dimensions/minecraft/overworld/region")).unwrap();
        fs::write(world.join("level.dat"), b"level").unwrap();
        fs::write(world.join("session.lock"), b"lock").unwrap();
        fs::write(
            world.join("dimensions/minecraft/overworld/region/r.0.0.mca"),
            vec![1u8; 10_000],
        )
        .unwrap();
        let settings = BackupSettings {
            directory: root.join("backups"),
            keep: 2,
            compression_level: 1,
        };
        for i in 0..3 {
            // Older archives of the same name, sorted before the new one.
            fs::create_dir_all(&settings.directory).unwrap();
            fs::write(
                settings
                    .directory
                    .join(format!("world-2000-01-0{i}_00-00-00.zip")),
                b"",
            )
            .unwrap();
        }
        let archive = create_backup("world", std::slice::from_ref(&world), &settings).unwrap();
        let mut zip = zip::ZipArchive::new(File::open(&archive).unwrap()).unwrap();
        let mut names: Vec<_> = zip.file_names().map(|n| n.unwrap().into_owned()).collect();
        names.sort();
        assert_eq!(
            names,
            [
                "world/dimensions/minecraft/overworld/region/r.0.0.mca",
                "world/level.dat"
            ]
        );
        let mut region = Vec::new();
        zip.by_name("world/dimensions/minecraft/overworld/region/r.0.0.mca")
            .unwrap()
            .read_to_end(&mut region)
            .unwrap();
        assert_eq!(region, vec![1u8; 10_000]);
        let left: Vec<_> = fs::read_dir(&settings.directory)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left.len(), 2, "{left:?}");
        assert!(left.contains(&archive.file_name().unwrap().to_owned()));
        fs::remove_dir_all(&root).unwrap();
    }
}

//! The per-machine cache of extracted Vanilla data.
//!
//! Each pinned version becomes one directory `<cache>/vanilla-data/<id>-<kind>-<key>/` with the
//! normalised data (the current version: [`crate::normalize`]; a kept older version:
//! [`crate::keep`]). `<key>` hashes the jar's SHA-1 and the pipeline that made the data (the
//! extractor sources compiled for it and this crate's extraction code), so a changed pipeline
//! makes a new entry and switching branches never mixes data. An entry is written to a temporary
//! directory and renamed when complete, under a file lock, so parallel builds (build scripts,
//! several checkouts) extract each version once.
//!
//! `<cache>` is `$VANILLITE_CACHE_DIR`, else `$XDG_CACHE_HOME/vanillite`, else
//! `~/.cache/vanillite` (`%LOCALAPPDATA%\vanillite` on Windows). `$VANILLITE_DATA_DIR`
//! replaces the cache with a pre-extracted directory in the layout `vanilla/` (current version)
//! and `multiversion/<id>/` (kept versions), e.g. made by `cargo xtask vanilla export <dir>`.

use std::fs::File;
use std::path::{Path, PathBuf};

use sha1::{Digest, Sha1};

use crate::vanilla::{self, Extractor};
use crate::{Result, read};

/// Bumped by hand when the format of the cached data changes in a way the hashed sources do not
/// show.
const FORMAT: &str = "1";

/// The Rust code that makes a cache entry: a change to it makes new entries.
const PIPELINE: &[&str] = &[
    include_str!("vanilla.rs"),
    include_str!("normalize.rs"),
    include_str!("keep.rs"),
];

/// A pinned version: `version.toml` of `data/vanilla/` or `data/multiversion/<id>/`.
#[derive(Debug, Clone)]
pub struct Pin {
    pub id: String,
    pub jar_sha1: String,
}

impl Pin {
    /// Reads `<dir>/version.toml`.
    pub fn load(dir: &Path) -> Result<Self> {
        let file = dir.join("version.toml");
        let table: toml::Table = read(&file)?
            .parse()
            .map_err(|e| format!("{}: {e}", file.display()))?;
        let s = |k: &str| {
            table
                .get(k)
                .and_then(toml::Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| format!("{}: `{k}` is missing", file.display()))
        };
        Ok(Self {
            id: s("id")?,
            jar_sha1: s("server_jar_sha1")?,
        })
    }
}

/// The pinned current version (`data/vanilla/version.toml`).
pub fn current_pin(root: &Path) -> Result<Pin> {
    Pin::load(&root.join("data/vanilla"))
}

/// The pinned older versions (`data/multiversion/<id>/version.toml`), sorted by directory.
pub fn kept_pins(root: &Path) -> Result<Vec<Pin>> {
    let dir = root.join("data/multiversion");
    let mut dirs: Vec<PathBuf> = match std::fs::read_dir(&dir) {
        Ok(entries) => entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.join("version.toml").exists())
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(format!("{}: {e}", dir.display())),
    };
    dirs.sort();
    dirs.iter().map(|d| Pin::load(d)).collect()
}

/// The cache root (see the module documentation).
pub fn cache_root(root: &Path) -> PathBuf {
    let var = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    if let Some(dir) = var("VANILLITE_CACHE_DIR") {
        return dir;
    }
    if let Some(dir) = var("XDG_CACHE_HOME") {
        return dir.join("vanillite");
    }
    if let Some(home) = var("HOME") {
        return home.join(".cache").join("vanillite");
    }
    if let Some(dir) = var("LOCALAPPDATA") {
        return dir.join("vanillite");
    }
    root.join("target").join("vanillite-cache")
}

/// The pre-extracted data directory of `$VANILLITE_DATA_DIR`, if set.
pub fn override_dir() -> Option<PathBuf> {
    std::env::var_os("VANILLITE_DATA_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// The data directory of the pinned current version, extracted if needed.
pub fn current(root: &Path) -> Result<PathBuf> {
    let pin = current_pin(root)?;
    match override_dir() {
        Some(dir) => checked(&dir.join("vanilla"), &pin),
        None => ensure(root, &pin, Extractor::Full),
    }
}

/// The data directories of the pinned older versions, extracted if needed (`jobs` at a time).
pub fn kept(root: &Path, jobs: usize) -> Result<Vec<PathBuf>> {
    let pins = kept_pins(root)?;
    if let Some(dir) = override_dir() {
        return pins
            .iter()
            .map(|p| checked(&dir.join("multiversion").join(&p.id), p))
            .collect();
    }
    let next = std::sync::atomic::AtomicUsize::new(0);
    let mut results: Vec<Option<Result<PathBuf>>> = vec![None; pins.len()];
    let slots: Vec<std::sync::Mutex<&mut Option<Result<PathBuf>>>> =
        results.iter_mut().map(std::sync::Mutex::new).collect();
    std::thread::scope(|s| {
        for _ in 0..jobs.max(1).min(pins.len()) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let (Some(pin), Some(slot)) = (pins.get(i), slots.get(i)) else {
                        break;
                    };
                    let r = ensure(root, pin, Extractor::Multiversion);
                    if let Ok(mut slot) = slot.lock() {
                        **slot = Some(r);
                    }
                }
            });
        }
    });
    drop(slots);
    results
        .into_iter()
        .map(|r| r.unwrap_or_else(|| Err("extraction did not run".to_owned())))
        .collect()
}

/// `dir` if its `version.toml` is the pinned jar's.
fn checked(dir: &Path, pin: &Pin) -> Result<PathBuf> {
    let found = Pin::load(dir)?;
    if found.jar_sha1 != pin.jar_sha1 {
        return Err(format!(
            "{}: data of server jar {} ({}), but {} is pinned to {} (VANILLITE_DATA_DIR)",
            dir.display(),
            found.jar_sha1,
            found.id,
            pin.id,
            pin.jar_sha1
        ));
    }
    Ok(dir.to_path_buf())
}

/// The cache directory for `pin` made by `extractor`, whether it exists yet or not.
pub fn entry_dir(root: &Path, pin: &Pin, extractor: Extractor) -> Result<PathBuf> {
    let kind = match extractor {
        Extractor::Full => "full",
        Extractor::Multiversion => "mv",
    };
    let mut h = Sha1::new();
    for part in [FORMAT, kind, pin.jar_sha1.as_str()] {
        h.update(part.as_bytes());
        h.update([0]);
    }
    for code in PIPELINE {
        h.update(code.as_bytes());
        h.update([0]);
    }
    let extractor_root = root.join("tools/extractor/src");
    for source in vanilla::extractor_sources(root, extractor)? {
        let rel = source.strip_prefix(&extractor_root).unwrap_or(&source);
        h.update(rel.to_string_lossy().replace('\\', "/").as_bytes());
        h.update([0]);
        h.update(read(&source)?.replace("\r\n", "\n").as_bytes());
        h.update([0]);
    }
    let key: String = h
        .finalize()
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok(cache_root(root)
        .join("vanilla-data")
        .join(format!("{}-{kind}-{key}", pin.id)))
}

/// The cache entry for `pin`, extracted from the official jar first if it does not exist.
pub fn ensure(root: &Path, pin: &Pin, extractor: Extractor) -> Result<PathBuf> {
    let dir = entry_dir(root, pin, extractor)?;
    if dir.join("version.toml").exists() {
        return Ok(dir);
    }
    store(root, pin, extractor, |out| {
        let work_root = suffixed(&dir, "work");
        let manifest = vanilla::manifest()?;
        let x = vanilla::extract(root, &work_root, &manifest, Some(&pin.id), None, extractor)?;
        if x.info.jar_sha1 != pin.jar_sha1 {
            return Err(format!(
                "{}: Mojang's manifest names server jar {}, but {} is pinned",
                pin.id, x.info.jar_sha1, pin.jar_sha1
            ));
        }
        match extractor {
            Extractor::Full => {
                crate::normalize::run(&x.generated.join("reports"), &x.extracted, &x.info, out)?
            }
            Extractor::Multiversion => crate::keep::keep(&x, out)?,
        }
        vanilla::remove_dir(&work_root)
    })?;
    Ok(dir)
}

/// Makes the cache entry for `pin` with `make` (which writes the data into the directory it is
/// given) unless it exists; under the entry's lock, renamed into place when complete.
pub fn store(
    root: &Path,
    pin: &Pin,
    extractor: Extractor,
    make: impl FnOnce(&Path) -> Result<()>,
) -> Result<PathBuf> {
    let dir = entry_dir(root, pin, extractor)?;
    let parent = dir.parent().ok_or("cache entry without parent")?;
    std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    let lock_path = suffixed(&dir, "lock");
    let lock = File::create(&lock_path).map_err(|e| format!("{}: {e}", lock_path.display()))?;
    lock.lock()
        .map_err(|e| format!("{}: {e}", lock_path.display()))?;
    if dir.join("version.toml").exists() {
        return Ok(dir);
    }
    let tmp = suffixed(&dir, "tmp");
    vanilla::remove_dir(&tmp)?;
    make(&tmp)?;
    if !tmp.join("version.toml").exists() {
        return Err(format!("{}: no version.toml was written", tmp.display()));
    }
    backdate(&tmp)?;
    vanilla::remove_dir(&dir)?;
    std::fs::rename(&tmp, &dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    drop(lock);
    let _ = std::fs::remove_file(&lock_path);
    Ok(dir)
}

/// Sets the modification time of every file below `dir` to 2000-01-01. Cargo reruns a build
/// script when a file it watches is newer than the script's last start, and the entry is
/// written while the script runs: without this the next build would rerun it once and rebuild
/// every crate that depends on the tables.
fn backdate(dir: &Path) -> Result<()> {
    let time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(946_684_800);
    for entry in std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let path = entry.map_err(|e| format!("{}: {e}", dir.display()))?.path();
        if path.is_dir() {
            backdate(&path)?;
        } else {
            File::options()
                .write(true)
                .open(&path)
                .and_then(|f| f.set_modified(time))
                .map_err(|e| format!("{}: {e}", path.display()))?;
        }
    }
    Ok(())
}

/// `<dir>.<suffix>` (not `with_extension`: version ids contain dots).
fn suffixed(dir: &Path, suffix: &str) -> PathBuf {
    let mut name = dir.as_os_str().to_owned();
    name.push(".");
    name.push(suffix);
    PathBuf::from(name)
}

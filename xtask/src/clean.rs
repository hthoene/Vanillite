//! `cargo xtask clean-stale`: removes build artifacts that no build has used for a while from
//! cargo target directories, safely while other builds run.
//!
//! Every profile directory (`target/debug`, `target/<triple>/release`, ...) is swept only while
//! this process holds cargo's own locks on it, so no cargo build runs there meanwhile; a
//! profile directory cargo has locked is skipped until the next round. What goes:
//! - `incremental/<crate>-<id>/`: whole directories whose newest session is older than
//!   `--incremental-hours` (another branch, feature set or toolchain); of the others every
//!   session but the newest finished one. The next build of such a crate compiles it in full
//!   once.
//! - Build units (`.fingerprint/`, `build/`, `deps/`, `examples/` entries sharing one unit
//!   hash) none of whose files was read or written for `--unit-hours`. Cargo reads a unit's
//!   fingerprint on every build that includes it; with `relatime` mounts access times move at
//!   most once a day, so the default of 48 hours removes units unused for at least a day.
//!   Cargo notices the missing outputs and rebuilds a unit should it be needed again.

use std::collections::HashMap;
use std::fs::{self, File};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::Result;

/// Lock files cargo holds on a profile directory while it builds there.
const CARGO_LOCKS: [&str; 3] = [".cargo-lock", ".cargo-build-lock", ".cargo-artifact-lock"];
/// Per-unit directories of a profile directory, named `<name>-<unit hash>[.<ext>]`.
const UNIT_DIRS: [&str; 4] = [".fingerprint", "build", "deps", "examples"];
/// Directory depth searched below each root for profile directories.
const MAX_DEPTH: usize = 7;

struct Options {
    incremental: Duration,
    unit: Duration,
    dry_run: bool,
    every: Option<Duration>,
    roots: Vec<PathBuf>,
}

pub fn run(repo: &Path, args: &[&str]) -> Result<()> {
    let mut opts = Options {
        incremental: hours(4.0),
        unit: hours(48.0),
        dry_run: false,
        every: None,
        roots: Vec::new(),
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = |name: &str| -> Result<f64> {
            it.next()
                .and_then(|v| v.parse().ok())
                .filter(|v: &f64| *v >= 0.0)
                .ok_or(format!("{name} needs a number"))
        };
        match *arg {
            "--incremental-hours" => opts.incremental = hours(value(arg)?),
            "--unit-hours" => opts.unit = hours(value(arg)?),
            "--every" => opts.every = Some(Duration::from_secs_f64(value(arg)?.max(1.0) * 60.0)),
            "--dry-run" => opts.dry_run = true,
            dir if !dir.starts_with('-') => opts.roots.push(PathBuf::from(dir)),
            _ => return Err(crate::USAGE.to_owned()),
        }
    }
    if opts.roots.is_empty() {
        let target =
            std::env::var_os("CARGO_TARGET_DIR").map_or(repo.join("target"), PathBuf::from);
        opts.roots.push(target);
    }
    loop {
        sweep_all(&opts);
        match opts.every {
            Some(every) => std::thread::sleep(every),
            None => return Ok(()),
        }
    }
}

fn hours(h: f64) -> Duration {
    Duration::from_secs_f64(h * 3600.0)
}

fn sweep_all(opts: &Options) {
    let mut profiles = Vec::new();
    for root in &opts.roots {
        find_profiles(root, 0, &mut profiles);
    }
    let mut total = 0;
    for profile in &profiles {
        match sweep_profile(profile, opts) {
            Ok(Some(freed)) => {
                if freed > 0 {
                    println!("{}: {}", profile.display(), size(freed));
                }
                total += freed;
            }
            Ok(None) => println!("{}: busy (cargo is building), skipped", profile.display()),
            Err(e) => eprintln!("{}: {e}", profile.display()),
        }
    }
    let verb = if opts.dry_run { "would free" } else { "freed" };
    println!(
        "clean-stale: {verb} {} in {} profile directories",
        size(total),
        profiles.len()
    );
}

fn size(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / (1u64 << 30) as f64)
}

/// Collects the cargo profile directories (those with a `.fingerprint` directory) below `dir`.
fn find_profiles(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if dir.join(".fingerprint").is_dir() {
        out.push(dir.to_path_buf());
        return;
    }
    if depth >= MAX_DEPTH {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let skip = matches!(
            name.to_str(),
            Some(".git" | "node_modules" | "src" | "tests" | "data")
        );
        if !skip && entry.file_type().is_ok_and(|t| t.is_dir()) {
            find_profiles(&entry.path(), depth + 1, out);
        }
    }
}

/// Sweeps one profile directory under cargo's locks; `None` if a build holds them.
fn sweep_profile(profile: &Path, opts: &Options) -> Result<Option<u64>> {
    let mut locks = Vec::new();
    for name in CARGO_LOCKS {
        let path = profile.join(name);
        if !path.exists() {
            continue;
        }
        let file = File::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        if file.try_lock().is_err() {
            return Ok(None);
        }
        locks.push(file);
    }
    let now = SystemTime::now();
    let mut freed = sweep_incremental(&profile.join("incremental"), now, opts);
    freed += sweep_units(profile, now, opts);
    drop(locks);
    Ok(Some(freed))
}

fn sweep_incremental(dir: &Path, now: SystemTime, opts: &Options) -> u64 {
    let Ok(crates) = fs::read_dir(dir) else {
        return 0;
    };
    let mut freed = 0;
    for krate in crates.flatten() {
        let path = krate.path();
        let Ok(entries) = fs::read_dir(&path) else {
            continue;
        };
        let entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        // A rustc of a build outside cargo's locks still holds its session lock.
        let in_use = entries.iter().any(|p| {
            p.extension().is_some_and(|e| e == "lock")
                && File::open(p).is_ok_and(|f| f.try_lock().is_err())
        });
        if in_use {
            continue;
        }
        let newest = entries
            .iter()
            .filter(|p| p.is_dir() && !name_of(p).ends_with("-working"))
            .filter_map(|p| Some((fs::metadata(p).ok()?.modified().ok()?, p)))
            .max();
        match newest {
            Some((time, keep)) if age(now, time) <= opts.incremental => {
                // Sessions are `s-<time>-<id>-<hash>` with the lock file `s-<time>-<id>.lock`.
                let keep_name = name_of(keep);
                let keep_lock = keep_name.rsplit_once('-').map(|(p, _)| format!("{p}.lock"));
                for entry in &entries {
                    let name = name_of(entry);
                    if name != keep_name && Some(&name) != keep_lock.as_ref() {
                        freed += remove(entry, opts.dry_run);
                    }
                }
            }
            _ => freed += remove(&path, opts.dry_run),
        }
    }
    freed
}

fn sweep_units(profile: &Path, now: SystemTime, opts: &Options) -> u64 {
    // Unit hash -> (last use, entries).
    let mut units: HashMap<String, (SystemTime, Vec<PathBuf>)> = HashMap::new();
    for sub in UNIT_DIRS {
        let Ok(entries) = fs::read_dir(profile.join(sub)) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(hash) = unit_hash(&name_of(&path)) else {
                continue;
            };
            let used = last_use(&path);
            let unit = units
                .entry(hash)
                .or_insert((SystemTime::UNIX_EPOCH, Vec::new()));
            unit.0 = unit.0.max(used);
            unit.1.push(path);
        }
    }
    let mut freed = 0;
    for (used, paths) in units.into_values() {
        if age(now, used) > opts.unit {
            for path in &paths {
                freed += remove(path, opts.dry_run);
            }
        }
    }
    freed
}

/// The unit hash of `name-0123456789abcdef[.ext...]`.
fn unit_hash(name: &str) -> Option<String> {
    let stem = name.split('.').next()?;
    let (_, hash) = stem.rsplit_once('-')?;
    (hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit())).then(|| hash.to_owned())
}

/// The latest access or modification of `path` and everything below it.
fn last_use(path: &Path) -> SystemTime {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return SystemTime::UNIX_EPOCH;
    };
    let mut latest = [meta.accessed(), meta.modified()]
        .into_iter()
        .flatten()
        .max();
    if meta.is_dir() {
        for entry in fs::read_dir(path).into_iter().flatten().flatten() {
            latest = latest.max(Some(last_use(&entry.path())));
        }
    }
    latest.unwrap_or(SystemTime::UNIX_EPOCH)
}

/// Removes a file or directory tree; returns the bytes it occupied on disk.
fn remove(path: &Path, dry_run: bool) -> u64 {
    let bytes = disk_usage(path);
    if dry_run {
        return bytes;
    }
    let result = if fs::symlink_metadata(path).is_ok_and(|m| m.is_dir()) {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };
    match result {
        Ok(()) => bytes,
        Err(e) => {
            eprintln!("{}: {e}", path.display());
            0
        }
    }
}

fn disk_usage(path: &Path) -> u64 {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return 0;
    };
    let mut bytes = meta.blocks() * 512;
    if meta.is_dir() {
        for entry in fs::read_dir(path).into_iter().flatten().flatten() {
            bytes += disk_usage(&entry.path());
        }
    }
    bytes
}

fn age(now: SystemTime, time: SystemTime) -> Duration {
    now.duration_since(time).unwrap_or_default()
}

fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

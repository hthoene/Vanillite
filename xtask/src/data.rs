//! `cargo xtask vanilla update|export`, `multiversion add`, `codegen`: the Vanilla data commands
//! on top of `vanillite-datagen` (the data itself lives in the per-machine cache, the
//! repository pins the versions).

use std::path::{Path, PathBuf};

use crate::cache::{self, Pin};
use crate::normalize::{self, Snapshot, VersionInfo};
use crate::vanilla::{self, Extracted, Extractor};
use crate::{Result, write};

/// The pin of an extracted version.
pub fn pin(info: &VersionInfo) -> Pin {
    Pin {
        id: info.id.clone(),
        jar_sha1: info.jar_sha1.clone(),
    }
}

/// Stores the normalised data of a fully extracted version in the cache.
pub fn store_full(root: &Path, x: &Extracted) -> Result<PathBuf> {
    cache::store(root, &pin(&x.info), Extractor::Full, |out| {
        normalize::run(&x.generated.join("reports"), &x.extracted, &x.info, out)
    })
}

/// `cargo xtask vanilla update [<version>] [--jar <server.jar>]`: extracts a version, stores
/// its data in the cache, pins it in `data/vanilla/version.toml` and prints a change report.
pub fn update(root: &Path, version: Option<&str>, local_jar: Option<&Path>) -> Result<()> {
    let before = Snapshot::load(&cache::current(root)?);
    let manifest = vanilla::manifest()?;
    let x = vanilla::extract(
        root,
        &root.join("target/vanilla"),
        &manifest,
        version,
        local_jar,
        Extractor::Full,
    )?;
    let dir = store_full(root, &x)?;
    write(&root.join("data/vanilla/version.toml"), &x.info.to_toml())?;
    let after = Snapshot::load(&dir);

    let report = normalize::change_report(&before, &after);
    let report_path = x.work.join("change-report.md");
    write(&report_path, &report)?;
    println!("{report}");
    println!("data: {}", dir.display());
    println!("change report: {}", report_path.display());
    Ok(())
}

/// `cargo xtask multiversion add <id>...`: extracts older versions for the multi-version layer,
/// stores their data in the cache and pins them in `data/multiversion/<id>/version.toml`.
pub fn multiversion_add(root: &Path, args: &[&str]) -> Result<()> {
    if args.is_empty() {
        return Err(crate::USAGE.to_owned());
    }
    let manifest = vanilla::manifest()?;
    for id in args {
        let x = vanilla::extract(
            root,
            &root.join("target/vanilla"),
            &manifest,
            Some(id),
            None,
            Extractor::Multiversion,
        )?;
        let dir = cache::store(root, &pin(&x.info), Extractor::Multiversion, |out| {
            crate::keep::keep(&x, out)
        })?;
        write(
            &root
                .join("data/multiversion")
                .join(&x.info.id)
                .join("version.toml"),
            &x.info.to_toml(),
        )?;
        println!("multiversion: pinned {} ({})", x.info.id, dir.display());
    }
    Ok(())
}

/// `cargo xtask vanilla export <dir>`: the data of all pinned versions in the layout
/// `VANILLITE_DATA_DIR` expects (`vanilla/`, `multiversion/<id>/`), extracted first if needed:
/// for offline builds and build machines without Java.
pub fn export(root: &Path, args: &[&str]) -> Result<()> {
    let [dir] = args else {
        return Err(crate::USAGE.to_owned());
    };
    let out = PathBuf::from(dir);
    vanilla::remove_dir(&out)?;
    copy_dir(&cache::current(root)?, &out.join("vanilla"))?;
    let jobs = std::thread::available_parallelism().map_or(2, |n| n.get().min(4));
    for (pin, data) in cache::kept_pins(root)?.iter().zip(cache::kept(root, jobs)?) {
        copy_dir(&data, &out.join("multiversion").join(&pin.id))?;
    }
    println!("vanilla data exported to {}", out.display());
    Ok(())
}

/// `cargo xtask codegen [--check]`: generates the tables of `vanillite-data` and
/// `vanillite-multiversion` from the cached data into `target/generated/` for reading (the
/// build generates them itself); `--check` only generates them in memory.
pub fn codegen(root: &Path, check: bool) -> Result<()> {
    let current = cache::current(root)?;
    let jobs = std::thread::available_parallelism().map_or(2, |n| n.get().min(4));
    let kept = cache::kept(root, jobs)?;
    let sets = [
        ("vanillite-data", crate::codegen::generate(&current)?),
        (
            "vanillite-multiversion",
            crate::multiversion::generate(&current, &kept)?,
        ),
    ];
    if check {
        println!("codegen: the tables generate from the pinned data");
        return Ok(());
    }
    for (krate, files) in sets {
        let out = root.join("target/generated").join(krate);
        vanilla::remove_dir(&out)?;
        for (name, contents) in files {
            write(&out.join(name), &contents)?;
        }
        println!("codegen: wrote {}", out.display());
    }
    Ok(())
}

fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to).map_err(|e| format!("{}: {e}", to.display()))?;
    for entry in std::fs::read_dir(from).map_err(|e| format!("{}: {e}", from.display()))? {
        let path = entry
            .map_err(|e| format!("{}: {e}", from.display()))?
            .path();
        let Some(name) = path.file_name() else {
            continue;
        };
        if path.is_dir() {
            copy_dir(&path, &to.join(name))?;
        } else {
            std::fs::copy(&path, to.join(name)).map_err(|e| format!("{}: {e}", path.display()))?;
        }
    }
    Ok(())
}

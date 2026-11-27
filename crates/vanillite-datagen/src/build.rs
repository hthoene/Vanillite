//! The build scripts of `vanillite-data` and `vanillite-multiversion`: make sure the pinned
//! versions are extracted ([`crate::cache`]) and write the generated tables to `OUT_DIR`.
//!
//! The scripts rerun only when a pin, the extractor sources, a cache entry or one of the
//! environment variables changes, so warm builds do no extraction and no code generation.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::vanilla::{self, Extractor};
use crate::{Result, cache};

/// `vanillite-data/build.rs`: the tables of the current version in `OUT_DIR`, and
/// `VANILLITE_VANILLA_DATA` (the data directory) for the crate.
pub fn vanilla_data() {
    exit_on_error(|| {
        let (root, out) = setup()?;
        rerun_for_pin(&root, &root.join("data/vanilla"), Extractor::Full)?;
        let data = announce(
            &root,
            &[cache::current_pin(&root)?],
            Extractor::Full,
            || cache::current(&root),
        )?;
        rerun_if_changed(&data.join("version.toml"));
        write_module(&out, crate::codegen::generate(&data)?)?;
        println!("cargo::rustc-env=VANILLITE_VANILLA_DATA={}", data.display());
        Ok(())
    });
}

/// `vanillite-multiversion/build.rs`: the adapter tables of the kept versions in `OUT_DIR`, and
/// `paths.rs` with the data directories (for the tests).
pub fn multiversion() {
    exit_on_error(|| {
        let (root, out) = setup()?;
        rerun_for_pin(&root, &root.join("data/vanilla"), Extractor::Full)?;
        println!(
            "cargo::rerun-if-changed={}",
            root.join("data/multiversion").display()
        );
        let pins = cache::kept_pins(&root)?;
        for pin in &pins {
            rerun_for_pin(
                &root,
                &root.join("data/multiversion").join(&pin.id),
                Extractor::Multiversion,
            )?;
        }
        let current = cache::current(&root)?;
        let jobs = std::thread::available_parallelism()
            .map_or(2, |n| n.get())
            .min(4);
        let kept = announce(&root, &pins, Extractor::Multiversion, || {
            cache::kept(&root, jobs)
        })?;
        rerun_if_changed(&current.join("version.toml"));
        for dir in &kept {
            rerun_if_changed(&dir.join("version.toml"));
        }
        let mut files = crate::multiversion::generate(&current, &kept)?;
        let mut paths =
            String::from("/// Data directory of the current version.\npub const VANILLA: &str = ");
        let _ = writeln!(paths, "{:?};", current.display().to_string());
        paths.push_str("/// Data directories of the kept versions: (id, directory).\npub const KEPT: &[(&str, &str)] = &[\n");
        for (pin, dir) in pins.iter().zip(&kept) {
            let _ = writeln!(
                paths,
                "    ({:?}, {:?}),",
                pin.id,
                dir.display().to_string()
            );
        }
        paths.push_str("];\n");
        files.push(("paths.rs".to_owned(), paths));
        write_module(&out, files)
    });
}

/// The workspace root and `OUT_DIR`, quiet mode and the environment the scripts depend on.
fn setup() -> Result<(PathBuf, PathBuf)> {
    vanilla::set_quiet(true);
    for var in ["VANILLITE_DATA_DIR", "VANILLITE_CACHE_DIR"] {
        println!("cargo::rerun-if-env-changed={var}");
    }
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR").ok_or("CARGO_MANIFEST_DIR is not set")?;
    let root = Path::new(&manifest)
        .ancestors()
        .find(|p| p.join("data/vanilla/version.toml").exists())
        .ok_or("data/vanilla/version.toml not found above the crate")?
        .to_path_buf();
    let out = PathBuf::from(std::env::var_os("OUT_DIR").ok_or("OUT_DIR is not set")?);
    Ok((root, out))
}

/// Reruns for the pin in `dir` and the extractor sources its entry depends on.
fn rerun_for_pin(root: &Path, dir: &Path, extractor: Extractor) -> Result<()> {
    rerun_if_changed(&dir.join("version.toml"));
    for source in vanilla::extractor_sources(root, extractor)? {
        rerun_if_changed(&source);
    }
    Ok(())
}

fn rerun_if_changed(path: &Path) {
    println!("cargo::rerun-if-changed={}", path.display());
}

/// Runs `f`; if it had to extract (an entry was missing), says so in a build warning.
fn announce<T>(
    root: &Path,
    pins: &[cache::Pin],
    extractor: Extractor,
    f: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let missing: Vec<&str> = if cache::override_dir().is_some() {
        Vec::new()
    } else {
        pins.iter()
            .filter(|p| {
                cache::entry_dir(root, p, extractor).is_ok_and(|d| !d.join("version.toml").exists())
            })
            .map(|p| p.id.as_str())
            .collect()
    };
    let start = std::time::Instant::now();
    let result = f()?;
    if !missing.is_empty() {
        println!(
            "cargo::warning=extracted the Vanilla data of {} from Mojang's server jar into {} \
             in {:.0?} (once per machine and pipeline version)",
            missing.join(", "),
            cache::cache_root(root).join("vanilla-data").display(),
            start.elapsed()
        );
    }
    Ok(result)
}

/// Writes generated files to `out`. `mod.rs` is included into the crate with `include!`, so
/// its submodules get absolute `#[path]`s and its inner doc comments become plain comments.
fn write_module(out: &Path, files: Vec<(String, String)>) -> Result<()> {
    for (name, contents) in files {
        let contents = if name == "mod.rs" {
            let mut s = String::new();
            for line in contents.lines() {
                if let Some(m) = line
                    .strip_prefix("pub mod ")
                    .and_then(|l| l.strip_suffix(';'))
                {
                    let path = out.join(format!("{m}.rs")).display().to_string();
                    let _ = writeln!(s, "#[path = {path:?}]\npub mod {m};");
                } else if let Some(doc) = line.strip_prefix("//!") {
                    let _ = writeln!(s, "//{doc}");
                } else {
                    let _ = writeln!(s, "{line}");
                }
            }
            s
        } else {
            contents
        };
        let path = out.join(&name);
        if std::fs::read_to_string(&path).ok().as_deref() != Some(contents.as_str()) {
            crate::write(&path, &contents)?;
        }
    }
    Ok(())
}

fn exit_on_error(f: impl FnOnce() -> Result<()>) {
    if let Err(e) = f() {
        eprintln!(
            "error: Vanilla data: {e}\n\n\
             The build extracts Mojang's data from the official server jar (network and a JDK; \
             one is downloaded if none is found) into a cache, once per machine. Offline, point \
             VANILLITE_DATA_DIR at a directory made by `cargo xtask vanilla export <dir>`. See \
             docs/development.md, \"Vanilla data\"."
        );
        std::process::exit(1);
    }
}

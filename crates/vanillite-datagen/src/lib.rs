//! The Vanilla data pipeline (#9, #213).
//!
//! The data Mojang's server jar holds (block states, registries, packets, ...) is not in the
//! repository. The repository pins the versions (`data/vanilla/version.toml`,
//! `data/multiversion/<id>/version.toml`: id, jar URL and SHA-1); this crate downloads each
//! pinned jar from Mojang, verifies its SHA-1, runs Mojang's data generator and our extractor
//! (`tools/extractor`), normalises the output into a per-machine cache ([`cache`]) and generates
//! the Rust tables of `vanillite-data` and `vanillite-multiversion` from it ([`build`], called
//! by their build scripts). A cache entry is made once per machine and pipeline version, so warm
//! builds never touch the network or Java.

pub mod build;
pub mod cache;
pub mod codegen;
pub mod keep;
pub mod multiversion;
pub mod normalize;
pub mod vanilla;

use std::path::Path;

/// Errors are messages for the console.
pub type Result<T> = std::result::Result<T, String>;

/// Reads a file to a string with the path in the error.
pub fn read(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Parses a JSON file.
pub fn read_json(path: &Path) -> Result<serde_json::Value> {
    serde_json::from_str(&read(path)?).map_err(|e| format!("{}: {e}", path.display()))
}

/// Writes a file, creating parent directories.
pub fn write(path: &Path, contents: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(path, contents).map_err(|e| format!("{}: {e}", path.display()))
}

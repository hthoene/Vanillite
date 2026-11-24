//! Data pack loader: dynamic registries, tags, recipes, loot tables, advancements and worldgen
//! settings with a compiled cache.
//!
//! The Vanilla data pack is never shipped: [`vanilla::ensure_vanilla_pack`] fetches Mojang's
//! server jar (pinned URL and SHA-1 from `vanillite_data::version`, or a local jar when offline)
//! and extracts its `data/` tree into the cache on first start. [`load`] then reads the Vanilla
//! pack and user packs the same way.
//!
//! This first version loads what the configuration phase and world generation need: the dynamic
//! registries (including all worldgen settings) and the tags of every registry.

mod loader;
pub mod vanilla;

use std::fmt;
use std::path::PathBuf;

pub use loader::{DynamicRegistry, Entry, LoadedPacks, load};

/// Errors of fetching and loading data packs.
#[derive(Debug)]
pub enum Error {
    /// Reading or writing a file failed.
    Io(PathBuf, std::io::Error),
    /// A JSON file is malformed.
    Json(PathBuf, serde_json::Error),
    /// The server jar does not have the pinned SHA-1.
    Checksum {
        path: PathBuf,
        expected: &'static str,
        actual: String,
    },
    /// Downloading the server jar failed (or the `fetch` feature is off).
    Download(String),
    /// The server jar is not a valid bundler jar.
    Jar(PathBuf, String),
    /// A tag or registry reference cannot be resolved.
    Unknown(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(p, e) => write!(f, "{}: {e}", p.display()),
            Self::Json(p, e) => write!(f, "{}: {e}", p.display()),
            Self::Checksum {
                path,
                expected,
                actual,
            } => write!(
                f,
                "{}: SHA-1 is {actual}, expected {expected} (wrong Minecraft version?)",
                path.display()
            ),
            Self::Download(e) => write!(f, "downloading the Minecraft server jar failed: {e}"),
            Self::Jar(p, e) => write!(f, "{}: {e}", p.display()),
            Self::Unknown(e) => f.write_str(e),
        }
    }
}

impl std::error::Error for Error {}

fn io_err(path: impl Into<PathBuf>) -> impl FnOnce(std::io::Error) -> Error {
    let path = path.into();
    move |e| Error::Io(path, e)
}

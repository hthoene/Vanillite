//! Golden storage.
//!
//! Every golden is keyed by (Minecraft version, server jar SHA-1, harness version, entry hash).
//! The entry hash is the SHA-1 of the scenario file, or of the Java source of a golden part.
//!
//! - **Repository tier** (small goldens): a directory with the golden files and a `KEYS.toml`
//!   holding the first three key parts once and the entry hash per golden. A golden whose key
//!   differs from the current one is stale.
//! - **Bulk tier**: `$VANILLITE_GOLDENS` (default `<workspace>/target/goldens`), cached by CI.
//!   The whole key is in the path, so a stale entry is simply missing.
//!
//! A missing or stale golden fails its test with the command that regenerates it; it is never
//! skipped. `cargo xtask golden regen` (Java 25) records what is missing; `cargo xtask golden
//! check` verifies the repository tier without Java.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sha1::{Digest, Sha1};

/// Name of the key file of a repository golden directory.
pub const KEYS_FILE: &str = "KEYS.toml";

/// The workspace root.
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// This crate's directory (`scenarios/`, `goldens/`).
pub fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Version of the Java harness (`tools/extractor/harness-version`). Bumped when the shared
/// recording code changes what it records; every golden becomes stale.
pub fn harness_version() -> u32 {
    include_str!("../../../tools/extractor/harness-version")
        .trim()
        .parse()
        .expect("tools/extractor/harness-version holds a number")
}

/// The key parts shared by all goldens of a run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Key {
    pub minecraft: String,
    pub jar_sha1: String,
    pub harness: u32,
}

impl Key {
    /// The key goldens must have for the pinned Vanilla version and this harness.
    pub fn current() -> Self {
        Self {
            minecraft: vanillite_data::version::VERSION_ID.to_owned(),
            jar_sha1: vanillite_data::version::SERVER_JAR_SHA1.to_owned(),
            harness: harness_version(),
        }
    }

    /// Directory name of this key in the bulk store.
    pub fn dir_name(&self) -> String {
        format!(
            "{}-{}-h{}",
            self.minecraft,
            &self.jar_sha1[..self.jar_sha1.len().min(12)],
            self.harness
        )
    }
}

/// SHA-1 of a text file with line endings normalised to `\n`, as hex.
pub fn text_sha1(text: &str) -> String {
    let mut h = Sha1::new();
    h.update(text.replace('\r', "").as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// The bulk golden store for the current key.
pub fn bulk_dir() -> PathBuf {
    let base = std::env::var_os("VANILLITE_GOLDENS")
        .map_or_else(|| workspace_root().join("target/goldens"), PathBuf::from);
    base.join(Key::current().dir_name())
}

/// A parsed `KEYS.toml`.
#[derive(Debug)]
pub struct Keys {
    pub key: Key,
    /// Golden name → entry hash.
    pub entries: BTreeMap<String, String>,
}

impl Keys {
    /// Reads `dir/KEYS.toml`.
    pub fn read(dir: &Path) -> Result<Self, String> {
        let path = dir.join(KEYS_FILE);
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let table: toml::Table = text
            .parse()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let str_of = |k: &str| {
            table
                .get(k)
                .and_then(|v| v.as_str())
                .map(str::to_owned)
                .ok_or_else(|| format!("{}: `{k}` is missing", path.display()))
        };
        let harness = table
            .get("harness")
            .and_then(toml::Value::as_integer)
            .and_then(|v| u32::try_from(v).ok())
            .ok_or_else(|| format!("{}: `harness` is missing", path.display()))?;
        let entries = table
            .get("entries")
            .and_then(|v| v.as_table())
            .map(|t| {
                t.iter()
                    .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
                    .collect()
            })
            .unwrap_or_default();
        Ok(Self {
            key: Key {
                minecraft: str_of("minecraft")?,
                jar_sha1: str_of("jar_sha1")?,
                harness,
            },
            entries,
        })
    }

    /// Checks that golden `name` in `dir` exists and has the current key and `entry_hash`.
    pub fn check(dir: &Path, name: &str, entry_hash: &str) -> Result<(), String> {
        let regen = "run `cargo xtask golden regen` (needs Java 25)";
        let keys = Self::read(dir).map_err(|e| format!("{e}; {regen}"))?;
        let current = Key::current();
        if keys.key != current {
            return Err(format!(
                "goldens in {} were recorded for {:?}, the current key is {current:?}; {regen}",
                dir.display(),
                keys.key
            ));
        }
        match keys.entries.get(name) {
            Some(h) if h == entry_hash => Ok(()),
            Some(_) => Err(format!(
                "golden `{name}` in {} is stale (its scenario or recorder changed); {regen}",
                dir.display()
            )),
            None => Err(format!(
                "golden `{name}` in {} is missing; {regen}",
                dir.display()
            )),
        }
    }
}

/// A data golden part (`goldens/data/<name>.json`, recorded by
/// `tools/extractor/.../golden/data/<Name>Goldens.java`), after checking its key.
pub fn data(name: &str) -> Result<serde_json::Value, String> {
    let dir = crate_dir().join("goldens/data");
    let class: String = name
        .split('_')
        .map(|w| {
            let mut c = w.chars();
            c.next()
                .map(|f| f.to_ascii_uppercase().to_string() + c.as_str())
                .unwrap_or_default()
        })
        .collect();
    let source = workspace_root().join(format!(
        "tools/extractor/src/dev/vanillite/extractor/golden/data/{class}Goldens.java"
    ));
    let source =
        std::fs::read_to_string(&source).map_err(|e| format!("{}: {e}", source.display()))?;
    Keys::check(&dir, name, &text_sha1(&source))?;
    let path = dir.join(format!("{name}.json"));
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// FNV-1a 64 over every line followed by `\n`, as 16 hex digits: how data goldens store long
/// lists (`DataGoldens.hash` on the Java side).
pub fn lines_hash<'a>(lines: impl IntoIterator<Item = &'a str>) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for line in lines {
        for &b in line.as_bytes().iter().chain(b"\n") {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    format!("{h:016x}")
}

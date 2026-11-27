//! Discovery, `plugin.toml`, package layout, load validation (architecture 6.6) and the load
//! order.
//!
//! A package is a directory or a zip file (`.vlp`) with `plugin.toml` and the library at
//! `native/<target-triple>/<file>` (a directory may also hold the library next to
//! `plugin.toml`, which suits development builds).

use std::collections::BTreeMap;
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use vanillite_abi as abi;

/// The target triple of this server; libraries for other targets are refused.
pub const TARGET: &str = env!("VANILLITE_TARGET");

/// Largest `plugin.toml` accepted.
const MAX_MANIFEST: u64 = 64 * 1024;
/// Largest library accepted from a `.vlp`.
const MAX_LIBRARY: u64 = 512 * 1024 * 1024;

/// How a plugin is called from several threads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Threading {
    /// Never on two threads at once (the host holds a per-plugin lock around every call).
    Serialized,
    /// The plugin is thread-safe and may be called from several world threads at once.
    Concurrent,
}

/// `plugin.toml`.
#[derive(Clone, Debug)]
pub struct Manifest {
    /// Unique id: `[a-z0-9_-]`, 1 to 64 characters; also the name of the data folder.
    pub id: String,
    /// Display name.
    pub name: String,
    /// The plugin's version.
    pub version: String,
    /// Plugin API version the plugin needs, `major.minor`.
    pub api: (u16, u16),
    /// Base name of the library (`hello` → `libhello.so`, `hello.dll`, `libhello.dylib`).
    pub library: String,
    /// Plugins that must be loaded before this one.
    pub depends: Vec<String>,
    /// Plugins loaded before this one when present.
    pub soft_depends: Vec<String>,
    /// Threading mode.
    pub threading: Threading,
    /// What the plugin declares it uses (`files`, `network`, `admin`): shown, not enforced.
    pub uses: Vec<String>,
    /// Slots the plugin may fill (`chunk-generator`).
    pub provides: Vec<String>,
    /// What the plugin does, for `/version <plugin>`; may be empty.
    pub description: String,
    /// Its authors.
    pub authors: Vec<String>,
    /// Its website; may be empty.
    pub website: String,
}

fn strings(table: &toml::Table, key: &str) -> Result<Vec<String>, String> {
    match table.get(key) {
        None => Ok(Vec::new()),
        Some(toml::Value::Array(items)) if items.len() <= 64 => items
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| format!("{key}: expected strings"))
            })
            .collect(),
        Some(_) => Err(format!("{key}: expected a list of at most 64 strings")),
    }
}

fn valid_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

impl Manifest {
    /// Parses `plugin.toml`.
    pub fn parse(text: &str) -> Result<Manifest, String> {
        let table: toml::Table = text.parse().map_err(|e: toml::de::Error| e.to_string())?;
        let string = |key: &str| -> Result<Option<String>, String> {
            match table.get(key) {
                None => Ok(None),
                Some(toml::Value::String(s)) if s.len() <= 256 => Ok(Some(s.clone())),
                Some(_) => Err(format!("{key}: expected a string of at most 256 bytes")),
            }
        };
        let id = string("id")?.ok_or("id is missing")?;
        if !valid_id(&id) {
            return Err(format!("id {id:?}: use 1 to 64 of a-z, 0-9, _ and -"));
        }
        let api = string("api")?.ok_or("api is missing (e.g. api = \"1.0\")")?;
        let api = api
            .split_once('.')
            .and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?)))
            .ok_or_else(|| format!("api {api:?}: expected major.minor"))?;
        let threading = match string("threading")?.as_deref() {
            None | Some("serialized") => Threading::Serialized,
            Some("concurrent") => Threading::Concurrent,
            Some(other) => {
                return Err(format!(
                    "threading {other:?}: expected \"serialized\" or \"concurrent\""
                ));
            }
        };
        let depends = strings(&table, "depends")?;
        if let Some(bad) = depends.iter().find(|d| !valid_id(d)) {
            return Err(format!("depends: invalid id {bad:?}"));
        }
        Ok(Manifest {
            name: string("name")?.unwrap_or_else(|| id.clone()),
            version: string("version")?.unwrap_or_else(|| "0.0.0".to_owned()),
            library: string("library")?.unwrap_or_else(|| id.replace('-', "_")),
            api,
            depends,
            soft_depends: strings(&table, "soft-depends")?,
            threading,
            uses: strings(&table, "uses")?,
            provides: strings(&table, "provides")?,
            description: string("description")?.unwrap_or_default(),
            authors: strings(&table, "authors")?,
            website: string("website")?.unwrap_or_default(),
            id,
        })
    }
}

/// The library file name of a base name on this platform.
pub fn library_file(name: &str) -> String {
    format!(
        "{}{name}{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    )
}

/// A discovered package, before loading.
#[derive(Debug)]
pub struct Package {
    /// The directory or `.vlp` file.
    pub path: PathBuf,
    /// Its manifest.
    pub manifest: Manifest,
    /// The library for this target, extracted from a `.vlp` into `plugins/.cache/`.
    pub library: PathBuf,
    /// SHA-256 of the library, hex.
    pub sha256: String,
}

/// Finds the packages in `dir`. Broken packages are logged and skipped.
pub fn discover(dir: &Path) -> Vec<Package> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    let mut packages = Vec::new();
    for path in paths {
        let hidden = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_none_or(|n| n.starts_with('.'));
        let is_vlp = path.extension().is_some_and(|e| e == "vlp");
        if hidden || !(is_vlp || path.join("plugin.toml").is_file()) {
            continue;
        }
        let result = if is_vlp {
            open_vlp(dir, &path)
        } else {
            open_dir(&path)
        };
        match result {
            Ok(package) => packages.push(package),
            Err(e) => log::error!("Skipping plugin {}: {e}", path.display()),
        }
    }
    packages
}

fn read_limited(path: &Path, max: u64) -> Result<Vec<u8>, String> {
    let file = fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if bytes.len() as u64 > max {
        return Err(format!("{} is larger than {max} bytes", path.display()));
    }
    Ok(bytes)
}

fn open_dir(path: &Path) -> Result<Package, String> {
    let text = String::from_utf8(read_limited(&path.join("plugin.toml"), MAX_MANIFEST)?)
        .map_err(|_| "plugin.toml is not UTF-8".to_owned())?;
    let manifest = Manifest::parse(&text).map_err(|e| format!("plugin.toml: {e}"))?;
    let file = library_file(&manifest.library);
    let candidates = [
        path.join("native").join(TARGET).join(&file),
        path.join(&file),
    ];
    let library = candidates
        .into_iter()
        .find(|p| p.is_file())
        .ok_or_else(|| format!("no library native/{TARGET}/{file}"))?;
    let sha256 = sha256_file(&library)?;
    Ok(Package {
        path: path.to_owned(),
        manifest,
        library,
        sha256,
    })
}

fn open_vlp(dir: &Path, path: &Path) -> Result<Package, String> {
    let file = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
    let mut read = |name: &str, max: u64| -> Result<Vec<u8>, String> {
        let entry = zip
            .by_name(name)
            .map_err(|_| format!("{name} is missing"))?;
        let mut bytes = Vec::new();
        entry
            .take(max + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > max {
            return Err(format!("{name} is larger than {max} bytes"));
        }
        Ok(bytes)
    };
    let text = String::from_utf8(read("plugin.toml", MAX_MANIFEST)?)
        .map_err(|_| "plugin.toml is not UTF-8".to_owned())?;
    let manifest = Manifest::parse(&text).map_err(|e| format!("plugin.toml: {e}"))?;
    let file = library_file(&manifest.library);
    let bytes = read(&format!("native/{TARGET}/{file}"), MAX_LIBRARY)?;
    let sha256 = hex(&Sha256::digest(&bytes));
    let cache = dir.join(".cache");
    fs::create_dir_all(&cache).map_err(|e| e.to_string())?;
    let library = cache.join(format!("{}-{}-{file}", manifest.id, &sha256[..16]));
    if !library.is_file() {
        fs::write(&library, &bytes).map_err(|e| format!("{}: {e}", library.display()))?;
    }
    Ok(Package {
        path: path.to_owned(),
        manifest,
        library,
        sha256,
    })
}

/// SHA-256 of a file, hex.
pub fn sha256_file(path: &Path) -> Result<String, String> {
    let bytes = read_limited(path, MAX_LIBRARY)?;
    Ok(hex(&Sha256::digest(&bytes)))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Orders packages so that dependencies come first; packages with missing dependencies or in
/// a cycle are logged and dropped. Ties keep the discovery order (sorted by path).
pub fn order(packages: Vec<Package>) -> Vec<Package> {
    let mut by_id: BTreeMap<String, Package> = BTreeMap::new();
    let mut discovery = Vec::new();
    for p in packages {
        if by_id.contains_key(&p.manifest.id) {
            log::error!(
                "Skipping plugin {}: a plugin with the id {} is already loaded",
                p.path.display(),
                p.manifest.id
            );
            continue;
        }
        discovery.push(p.manifest.id.clone());
        by_id.insert(p.manifest.id.clone(), p);
    }
    // Drop packages whose hard dependencies are missing, until nothing changes.
    loop {
        let missing: Vec<(String, String)> = by_id
            .values()
            .filter_map(|p| {
                p.manifest
                    .depends
                    .iter()
                    .find(|d| !by_id.contains_key(*d))
                    .map(|d| (p.manifest.id.clone(), d.clone()))
            })
            .collect();
        if missing.is_empty() {
            break;
        }
        for (id, dependency) in missing {
            log::error!("Skipping plugin {id}: it depends on {dependency}, which is not loaded");
            by_id.remove(&id);
        }
    }
    let mut ordered = Vec::new();
    let mut done: Vec<String> = Vec::new();
    while !by_id.is_empty() {
        let ready = discovery.iter().find(|id| {
            by_id.get(*id).is_some_and(|p| {
                p.manifest
                    .depends
                    .iter()
                    .chain(&p.manifest.soft_depends)
                    .all(|d| done.contains(d) || !by_id.contains_key(d) || d == *id)
            })
        });
        match ready.cloned() {
            Some(id) => {
                if let Some(p) = by_id.remove(&id) {
                    ordered.push(p);
                }
                done.push(id);
            }
            None => {
                for id in by_id.keys() {
                    log::error!("Skipping plugin {id}: its dependencies form a cycle");
                }
                break;
            }
        }
    }
    ordered
}

/// Checks the library's [`abi::PluginInfo`] against this host and the manifest (architecture
/// 6.6, steps 3 and 4).
pub fn validate(
    info: &abi::PluginInfo,
    manifest: &Manifest,
    data_version: i32,
) -> Result<(), String> {
    if info.abi_major != abi::ABI_MAJOR || info.abi_minor > abi::ABI_MINOR {
        return Err(format!(
            "built for plugin API {}.{}, this server has {}.{}",
            info.abi_major,
            info.abi_minor,
            abi::ABI_MAJOR,
            abi::ABI_MINOR
        ));
    }
    if manifest.api.0 != abi::ABI_MAJOR || manifest.api.1 > abi::ABI_MINOR {
        return Err(format!(
            "plugin.toml asks for plugin API {}.{}, this server has {}.{}",
            manifest.api.0,
            manifest.api.1,
            abi::ABI_MAJOR,
            abi::ABI_MINOR
        ));
    }
    if info.panic_strategy != abi::panic_strategy::UNWIND {
        return Err(
            "built with panic = \"abort\"; a panic would kill the server. Build it with \
             panic = \"unwind\""
                .to_owned(),
        );
    }
    // SAFETY: the plugin's strings are static.
    let target = unsafe { crate::api::str(info.target) };
    if target != TARGET {
        return Err(format!("built for {target}, this server runs on {TARGET}"));
    }
    if info.data_version != 0 && info.data_version != data_version {
        return Err(format!(
            "built for data version {}, this server has {data_version}",
            info.data_version
        ));
    }
    Ok(())
}

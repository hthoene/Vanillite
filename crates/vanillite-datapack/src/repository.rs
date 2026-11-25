//! The data pack repository (`PackRepository` with `ServerPacksSource` and
//! `FolderRepositorySource`): the Vanilla pack, the feature packs it ships, the world's packs in
//! `<world>/datapacks/` (folders with `pack.mcmeta` and zip files, id `file/<name>`), and which
//! of them are selected, in load order.
//!
//! World packs are untrusted: symbolic links are refused (Vanilla's default
//! `DirectoryValidator`), `pack.mcmeta` reads, file counts and the bytes a zip pack extracts to
//! are bounded. Zip packs are extracted into a cache directory when they are opened, so all
//! loaders read plain directories.

// SEC-07: pack files are untrusted input.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::as_conversions
)]

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use sha1::{Digest, Sha1};

use crate::{Error, OpenedPack, io_err};

/// `BuiltInPackSource.VANILLA_ID`.
pub const VANILLA: &str = "vanilla";
/// The feature flags of `FeatureFlags.REGISTRY`, in registry order.
pub const FEATURE_FLAGS: [&str; 4] = [
    "minecraft:vanilla",
    "minecraft:trade_rebalance",
    "minecraft:redstone_experiments",
    "minecraft:minecart_improvements",
];
/// `FeatureFlags.DEFAULT_FLAGS`.
pub const DEFAULT_FEATURES: [&str; 1] = ["minecraft:vanilla"];

/// Largest `pack.mcmeta` that is read.
const MAX_METADATA: u64 = 1 << 20;
/// Most entries a world pack (folder or zip) may have.
const MAX_FILES: usize = 100_000;
/// Deepest directory nesting of a world pack folder that is checked.
const MAX_DEPTH: usize = 64;
/// Most bytes a zip pack may extract to.
const MAX_EXTRACTED: u64 = 1 << 30;
/// Most entries of a packs folder that are looked at.
const MAX_PACKS: usize = 4096;
/// Marks a completely extracted zip pack.
const COMPLETE: &str = ".complete";
/// `PackFormat.lastPreMinorVersion(SERVER_DATA)`.
const LAST_PRE_MINOR: i64 = 81;
/// `Integer.MAX_VALUE`: the default minor version of `max_format`, and the unknown format.
const UNKNOWN: i64 = 0x7fff_ffff;

/// A pack format version (`PackFormat`): major and minor.
type Format = (i64, i64);

/// The data pack version of this game version.
fn current_format() -> Format {
    let (major, minor) = vanillite_data::version::DATA_PACK_VERSION;
    (i64::from(major), i64::from(minor))
}

/// Where a pack comes from (`PackSource`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackSource {
    /// The Vanilla pack (`BUILT_IN`).
    BuiltIn,
    /// A feature pack of the Vanilla jar (`FEATURE`); never added automatically.
    Feature,
    /// A pack of the world's `datapacks` folder (`WORLD`).
    World,
}

impl PackSource {
    /// `shouldAddAutomatically`.
    pub fn adds_automatically(self) -> bool {
        self != PackSource::Feature
    }

    /// The translation key of `decorateWithSource`.
    pub fn key(self) -> &'static str {
        match self {
            PackSource::BuiltIn => "pack.source.builtin",
            PackSource::Feature => "pack.source.feature",
            PackSource::World => "pack.source.world",
        }
    }
}

/// `PackCompatibility`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compatibility {
    TooOld,
    TooNew,
    Unknown,
    Compatible,
}

/// `Pack.Position`: where a newly enabled pack goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Position {
    Top,
    Bottom,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Location {
    Dir(PathBuf),
    Zip(PathBuf),
}

/// An available pack (`Pack`) with its metadata.
#[derive(Clone, Debug, PartialEq)]
pub struct Pack {
    /// `vanilla`, the feature pack's name or `file/<file name>`.
    pub id: String,
    /// The title as text component JSON.
    pub title: Value,
    /// The description as text component JSON.
    pub description: Value,
    pub compatibility: Compatibility,
    /// Requested feature flags, in registry order.
    pub features: Vec<String>,
    /// The overlay directories that apply to this version, in `pack.mcmeta` order.
    pub overlays: Vec<String>,
    pub source: PackSource,
    /// `PackSelectionConfig.defaultPosition` (no pack has a fixed position on servers).
    pub position: Position,
    /// The known pack id (`minecraft:<id>`) when clients ship the pack.
    pub known_pack: Option<String>,
    location: Location,
}

impl Pack {
    /// The Vanilla pack (`ServerPacksSource.createVanillaPack` with `BUILT_IN_METADATA`).
    fn vanilla(dir: &Path) -> Pack {
        Pack {
            id: VANILLA.to_owned(),
            title: json!({"translate": "dataPack.vanilla.name"}),
            description: json!({"translate": "dataPack.vanilla.description"}),
            compatibility: Compatibility::Compatible,
            features: DEFAULT_FEATURES.iter().map(|f| (*f).to_owned()).collect(),
            overlays: Vec::new(),
            source: PackSource::BuiltIn,
            position: Position::Bottom,
            known_pack: Some("core".to_owned()),
            location: Location::Dir(dir.to_path_buf()),
        }
    }

    /// `Pack.readMetaAndCreate`: `None` (logged) when `pack.mcmeta` is missing or invalid.
    fn read(
        id: String,
        title: Value,
        source: PackSource,
        known_pack: Option<String>,
        location: Location,
    ) -> Option<Pack> {
        let text = match read_metadata(&location) {
            Ok(Some(text)) => text,
            Ok(None) => {
                log::warn!("Missing metadata in pack {id}");
                return None;
            }
            Err(e) => {
                log::warn!("Failed to read pack {id} metadata: {e}");
                return None;
            }
        };
        match parse_metadata(&text) {
            Ok(Some(meta)) => Some(Pack {
                id,
                title,
                description: meta.description,
                compatibility: meta.compatibility,
                features: meta.features,
                overlays: meta.overlays,
                source,
                position: Position::Top,
                known_pack,
                location,
            }),
            Ok(None) => {
                log::warn!("Missing metadata in pack {id}");
                None
            }
            Err(e) => {
                log::warn!("Failed to read pack {id} metadata: {e}");
                None
            }
        }
    }

    /// `Pack.open`: the pack's directory (a zip pack is extracted below `cache` first) and its
    /// overlay directories.
    pub fn open(&self, cache: &Path) -> Result<OpenedPack, Error> {
        let base = match &self.location {
            Location::Dir(dir) => dir.clone(),
            Location::Zip(file) => extract(file, cache)?,
        };
        let mut layers = vec![base.clone()];
        layers.extend(self.overlays.iter().map(|o| base.join(o)));
        Ok(OpenedPack {
            layers,
            known_pack: self.known_pack.clone(),
        })
    }
}

/// `DataPackConfig`: the enabled packs in load order and the disabled ones.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DataPackConfig {
    pub enabled: Vec<String>,
    pub disabled: Vec<String>,
}

impl DataPackConfig {
    /// `DataPackConfig.DEFAULT`.
    pub fn vanilla() -> DataPackConfig {
        DataPackConfig {
            enabled: vec![VANILLA.to_owned()],
            disabled: Vec::new(),
        }
    }
}

/// The packs the server can use and the selected ones (`PackRepository`).
#[derive(Debug)]
pub struct PackRepository {
    vanilla: Option<PathBuf>,
    folder: PathBuf,
    available: BTreeMap<String, Pack>,
    selected: Vec<String>,
}

impl PackRepository {
    /// A repository of the Vanilla pack in `vanilla` (with its feature packs) and the world
    /// packs in `folder`. Nothing is discovered before [`PackRepository::reload`].
    pub fn new(vanilla: Option<PathBuf>, folder: PathBuf) -> PackRepository {
        PackRepository {
            vanilla,
            folder,
            available: BTreeMap::new(),
            selected: Vec::new(),
        }
    }

    /// `reload`: discovers the packs again; selected packs that are gone are dropped.
    pub fn reload(&mut self) {
        let selected = std::mem::take(&mut self.selected);
        self.available = self.discover();
        self.set_selected(&selected);
    }

    fn discover(&self) -> BTreeMap<String, Pack> {
        let mut found = BTreeMap::new();
        if let Some(vanilla) = &self.vanilla {
            found.insert(VANILLA.to_owned(), Pack::vanilla(vanilla));
            // `BuiltInPackSource.listBundledPacks`: `data/minecraft/datapacks/*`.
            let bundled = vanilla.join("data").join("minecraft").join("datapacks");
            discover(&bundled, false, |name, location| {
                let id = name.strip_suffix(".zip").unwrap_or(name).to_owned();
                let title = Value::String(id.clone());
                let pack = Pack::read(id.clone(), title, PackSource::Feature, Some(id), location);
                if let Some(pack) = pack {
                    found.insert(pack.id.clone(), pack);
                }
            });
        }
        // `FolderRepositorySource.loadPacks`.
        if let Err(e) = std::fs::create_dir_all(&self.folder) {
            log::warn!("Failed to list packs in {}: {e}", self.folder.display());
        }
        discover(&self.folder, true, |name, location| {
            let title = Value::String(name.to_owned());
            let pack = Pack::read(
                format!("file/{name}"),
                title,
                PackSource::World,
                None,
                location,
            );
            if let Some(pack) = pack {
                found.insert(pack.id.clone(), pack);
            }
        });
        found
    }

    /// `setSelected` (`rebuildSelected`; no pack is required on a server): the available packs
    /// of `ids` in that order.
    pub fn set_selected(&mut self, ids: &[String]) {
        self.selected = ids
            .iter()
            .filter(|id| self.available.contains_key(*id))
            .cloned()
            .collect();
    }

    /// The selected pack ids in load order.
    pub fn selected(&self) -> &[String] {
        &self.selected
    }

    /// The selected packs in load order.
    pub fn selected_packs(&self) -> Vec<&Pack> {
        self.selected
            .iter()
            .filter_map(|id| self.available.get(id))
            .collect()
    }

    /// Whether `id` is selected.
    pub fn is_selected(&self, id: &str) -> bool {
        self.selected.iter().any(|s| s == id)
    }

    /// All available packs in id order.
    pub fn available(&self) -> impl Iterator<Item = &Pack> {
        self.available.values()
    }

    /// The available pack `id`.
    pub fn get(&self, id: &str) -> Option<&Pack> {
        self.available.get(id)
    }

    /// Copies of the available packs of `ids`, in that order (to open them on another thread).
    pub fn packs(&self, ids: &[String]) -> Vec<Pack> {
        ids.iter()
            .filter_map(|id| self.available.get(id))
            .cloned()
            .collect()
    }

    /// `getRequestedFeatureFlags`: the features of the selected packs, in registry order.
    pub fn requested_features(&self) -> Vec<String> {
        let packs = self.selected_packs();
        features_in_order(|f| packs.iter().any(|p| p.features.iter().any(|g| g == f)))
    }

    /// `MinecraftServer.getSelectedPacks`: the selected packs, and the other available ones as
    /// disabled when `disable_inactive`.
    pub fn config(&self, disable_inactive: bool) -> DataPackConfig {
        DataPackConfig {
            enabled: self.selected.clone(),
            disabled: if disable_inactive {
                self.available
                    .keys()
                    .filter(|id| !self.is_selected(id))
                    .cloned()
                    .collect()
            } else {
                Vec::new()
            },
        }
    }
}

/// The known feature flags `has` accepts, in registry order.
fn features_in_order(has: impl Fn(&str) -> bool) -> Vec<String> {
    FEATURE_FLAGS
        .iter()
        .filter(|f| has(f))
        .map(|f| (*f).to_owned())
        .collect()
}

/// Whether every flag of `requested` is in `allowed` (`FeatureFlagSet.isSubsetOf`).
pub fn features_allowed(requested: &[String], allowed: &[String]) -> bool {
    requested.iter().all(|f| allowed.contains(f))
}

/// `FeatureFlags.printMissingFlags`.
pub fn missing_features(allowed: &[String], requested: &[String]) -> String {
    requested
        .iter()
        .filter(|f| !allowed.contains(f))
        .cloned()
        .collect::<Vec<_>>()
        .join(", ")
}

/// `MinecraftServer.configurePackRepository` (without safe mode): selects the enabled packs of
/// `config` that exist, then every new pack that is not disabled and adds automatically, and
/// drops packs whose features the world lacks. A new world (`init`) allows every feature.
/// Returns the new configuration and the world's features.
pub fn configure(
    repository: &mut PackRepository,
    config: &DataPackConfig,
    features: &[String],
    init: bool,
) -> (DataPackConfig, Vec<String>) {
    let forced: Vec<String> = if init { Vec::new() } else { features.to_vec() };
    let allowed: Vec<String> = if init {
        FEATURE_FLAGS.iter().map(|f| (*f).to_owned()).collect()
    } else {
        features.to_vec()
    };
    repository.reload();
    let mut selected: Vec<String> = Vec::new();
    for id in &config.enabled {
        if repository.available.contains_key(id) {
            if !selected.contains(id) {
                selected.push(id.clone());
            }
        } else {
            log::warn!("Missing data pack {id}");
        }
    }
    for pack in repository.available.values() {
        if config.disabled.contains(&pack.id) {
            continue;
        }
        let allowed_here = features_allowed(&pack.features, &allowed);
        let was_selected = selected.contains(&pack.id);
        if !was_selected && pack.source.adds_automatically() {
            if allowed_here {
                log::info!("Found new data pack {}, loading it automatically", pack.id);
                selected.push(pack.id.clone());
            } else {
                log::info!(
                    "Found new data pack {}, but can't load it due to missing features {}",
                    pack.id,
                    missing_features(&allowed, &pack.features)
                );
            }
        }
        if was_selected && !allowed_here {
            log::warn!(
                "Pack {} requires features {} that are not enabled for this world, disabling pack.",
                pack.id,
                missing_features(&allowed, &pack.features)
            );
            selected.retain(|id| *id != pack.id);
        }
    }
    if selected.is_empty() {
        log::info!("No datapacks selected, forcing vanilla");
        selected.push(VANILLA.to_owned());
    }
    repository.set_selected(&selected);
    enable_forced_feature_packs(repository, &forced);
    let provided = repository.requested_features();
    let features = features_in_order(|f| provided.iter().chain(&forced).any(|g| g == f));
    (repository.config(true), features)
}

/// `MinecraftServer.enableForcedFeaturePacks`: feature packs for features the world has but
/// no selected pack provides.
fn enable_forced_feature_packs(repository: &mut PackRepository, forced: &[String]) {
    let provided = repository.requested_features();
    let mut missing: Vec<String> = forced
        .iter()
        .filter(|f| !provided.contains(f))
        .cloned()
        .collect();
    if missing.is_empty() {
        return;
    }
    let mut selected = repository.selected.clone();
    for pack in repository.available.values() {
        if missing.is_empty() {
            break;
        }
        if pack.source == PackSource::Feature
            && !pack.features.is_empty()
            && pack.features.iter().any(|f| missing.contains(f))
            && features_allowed(&pack.features, forced)
            && !selected.contains(&pack.id)
        {
            log::info!(
                "Found feature pack ('{}') for requested feature, forcing to enabled",
                pack.id
            );
            selected.push(pack.id.clone());
            missing.retain(|f| !pack.features.contains(f));
        }
    }
    repository.set_selected(&selected);
}

/// `FolderRepositorySource.discoverPacks`: folders with a `pack.mcmeta` and `.zip` files of
/// `folder`, by file name. With `validate`, symbolic links are refused like Vanilla's default
/// `DirectoryValidator`, inside pack folders too.
fn discover(folder: &Path, validate: bool, mut found: impl FnMut(&str, Location)) {
    let Ok(read) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in read.filter_map(Result::ok).take(MAX_PACKS) {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()).map(str::to_owned) else {
            continue;
        };
        let Ok(mut meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            if validate {
                log::warn!(
                    "Ignoring potential pack entry: {}: symbolic links are not allowed",
                    path.display()
                );
                continue;
            }
            let Ok(target) = std::fs::metadata(&path) else {
                continue;
            };
            meta = target;
        }
        if meta.is_dir() {
            if validate && let Err(e) = check_tree(&path) {
                log::warn!("Ignoring potential pack entry: {}: {e}", path.display());
            } else if path.join("pack.mcmeta").is_file() {
                found(&name, Location::Dir(path));
            } else {
                log::info!("Found non-pack entry '{}', ignoring", path.display());
            }
        } else if meta.is_file() && name.ends_with(".zip") {
            found(&name, Location::Zip(path));
        } else {
            log::info!("Found non-pack entry '{}', ignoring", path.display());
        }
    }
}

/// `DirectoryValidator.validateKnownDirectory`: no symbolic links in a pack folder (bounded).
fn check_tree(dir: &Path) -> Result<(), String> {
    let mut stack = vec![(dir.to_path_buf(), 0usize)];
    let mut seen = 0usize;
    while let Some((dir, depth)) = stack.pop() {
        let read = std::fs::read_dir(&dir).map_err(|e| e.to_string())?;
        for entry in read {
            let entry = entry.map_err(|e| e.to_string())?;
            seen = seen.saturating_add(1);
            if seen > MAX_FILES {
                return Err(format!("more than {MAX_FILES} files"));
            }
            let kind = entry.file_type().map_err(|e| e.to_string())?;
            if kind.is_symlink() {
                return Err(format!(
                    "symbolic link {} is not allowed",
                    entry.path().display()
                ));
            }
            if kind.is_dir() {
                if depth >= MAX_DEPTH {
                    return Err("directories nested too deep".to_owned());
                }
                stack.push((entry.path(), depth.saturating_add(1)));
            }
        }
    }
    Ok(())
}

/// The text of `pack.mcmeta`, at most [`MAX_METADATA`] bytes; `None` if there is none.
fn read_metadata(location: &Location) -> Result<Option<String>, String> {
    let mut text = String::new();
    match location {
        Location::Dir(dir) => {
            let Ok(file) = File::open(dir.join("pack.mcmeta")) else {
                return Ok(None);
            };
            file.take(MAX_METADATA)
                .read_to_string(&mut text)
                .map_err(|e| e.to_string())?;
        }
        Location::Zip(file) => {
            let mut zip = crate::vanilla::open_zip(file).map_err(|e| e.to_string())?;
            if zip.len() > MAX_FILES {
                return Err(format!("more than {MAX_FILES} files"));
            }
            let Ok(entry) = zip.by_name("pack.mcmeta") else {
                return Ok(None);
            };
            entry
                .take(MAX_METADATA)
                .read_to_string(&mut text)
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(Some(text))
}

/// What `pack.mcmeta` declares (`Pack.Metadata`).
struct Metadata {
    description: Value,
    compatibility: Compatibility,
    features: Vec<String>,
    overlays: Vec<String>,
}

/// `Pack.readPackMetadata`: `None` without a `pack` section; an error for broken sections.
/// A broken format declaration falls back to the description only (`FALLBACK_TYPE`), whose
/// compatibility is unknown.
fn parse_metadata(text: &str) -> Result<Option<Metadata>, String> {
    let root: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let Some(pack) = root.get("pack") else {
        return Ok(None);
    };
    let description = pack
        .get("description")
        .filter(|d| d.is_string() || d.is_object() || d.is_array())
        .cloned()
        .ok_or("pack: missing or invalid description")?;
    let compatibility = match pack_formats(pack) {
        Ok((min, max)) => compatibility(min, max),
        Err(e) => {
            log::warn!("Error reading pack metadata, attempting fallback type: {e}");
            Compatibility::Unknown
        }
    };
    let features = match root.get("features") {
        None => Vec::new(),
        Some(section) => {
            let ids = section
                .get("enabled")
                .and_then(Value::as_array)
                .ok_or("features: missing enabled")?;
            let mut ids: Vec<String> = ids
                .iter()
                .map(|v| {
                    v.as_str()
                        .map(normalize)
                        .ok_or("features: not an identifier")
                })
                .collect::<Result<_, _>>()?;
            if let Some(unknown) = ids.iter().find(|f| !FEATURE_FLAGS.contains(&f.as_str())) {
                return Err(format!("Unknown feature ids: [{unknown}]"));
            }
            ids.dedup();
            features_in_order(|f| ids.iter().any(|g| g == f))
        }
    };
    let overlays = match root.get("overlays") {
        None => Vec::new(),
        Some(section) => overlays(section)?,
    };
    Ok(Some(Metadata {
        description,
        compatibility,
        features,
        overlays,
    }))
}

fn normalize(id: &str) -> String {
    if id.contains(':') {
        id.to_owned()
    } else {
        format!("minecraft:{id}")
    }
}

/// `PackCompatibility.forVersion`.
fn compatibility(min: Format, max: Format) -> Compatibility {
    let current = current_format();
    if min.0 == UNKNOWN {
        Compatibility::Unknown
    } else if max < current {
        Compatibility::TooOld
    } else if current < min {
        Compatibility::TooNew
    } else {
        Compatibility::Compatible
    }
}

/// A non-negative int.
fn non_negative(v: &Value) -> Option<i64> {
    v.as_i64().filter(|n| (0..=UNKNOWN).contains(n))
}

/// `PackFormat.BOTTOM_CODEC`/`TOP_CODEC`: `major` or `[major, minor, ...]`.
fn format_value(v: &Value, default_minor: i64) -> Option<Format> {
    match v {
        Value::Array(list) if !list.is_empty() && list.len() <= 256 => {
            let parts: Vec<i64> = list.iter().map(non_negative).collect::<Option<_>>()?;
            Some((
                *parts.first()?,
                parts.get(1).copied().unwrap_or(default_minor),
            ))
        }
        other => Some((non_negative(other)?, default_minor)),
    }
}

/// `InclusiveRange.codec(Codec.INT)`: `n`, `[min, max]` or `{min_inclusive, max_inclusive}`.
fn int_range(v: &Value) -> Option<(i64, i64)> {
    let int = |v: &Value| v.as_i64().filter(|n| i32::try_from(*n).is_ok());
    let (min, max) = match v {
        Value::Array(list) if list.len() == 2 => (int(list.first()?)?, int(list.get(1)?)?),
        Value::Object(o) => (int(o.get("min_inclusive")?)?, int(o.get("max_inclusive")?)?),
        other => (int(other)?, int(other)?),
    };
    (min <= max).then_some((min, max))
}

/// An optional field: `Ok(None)` when missing, an error when present but invalid.
fn field<T>(
    v: &Value,
    name: &str,
    parse: impl Fn(&Value) -> Option<T>,
) -> Result<Option<T>, String> {
    v.get(name)
        .map(|f| parse(f).ok_or_else(|| format!("invalid {name}")))
        .transpose()
}

/// The declared formats of an overlay or the pack (`PackFormat.IntermediaryFormat`).
struct Formats {
    min: Option<Format>,
    max: Option<Format>,
    format: Option<i64>,
    supported: Option<(i64, i64)>,
}

impl Formats {
    /// `effectiveMinMajorVersion`.
    fn effective_min_major(&self) -> i64 {
        match (self.min, self.supported) {
            (Some(min), Some(s)) => min.0.min(s.0),
            (Some(min), None) => min.0,
            (None, Some(s)) => s.0,
            (None, None) => UNKNOWN,
        }
    }

    /// `IntermediaryFormat.validate`.
    fn validate(
        &self,
        has_format_field: bool,
        require_old: bool,
    ) -> Result<(Format, Format), String> {
        let check_format = |min: i64, max: i64| -> Result<(), String> {
            if !has_format_field {
                return Ok(());
            }
            let Some(f) = self.format else {
                return Err("missing pack_format".to_owned());
            };
            if f < min || f > max {
                return Err(format!(
                    "Pack declared support for versions {min} to {max} but declared main format is {f}"
                ));
            }
            if f < 15 {
                return Err(
                    "Multi-version packs cannot support minimum version of less than 15".to_owned(),
                );
            }
            Ok(())
        };
        match (self.min, self.max, self.supported) {
            (Some(_), None, _) | (None, Some(_), _) => {
                Err("missing field, must declare both min_format and max_format".to_owned())
            }
            (_, _, None) if require_old => Err("missing required field".to_owned()),
            (Some(min), Some(max), supported) => {
                if min > max {
                    return Err("min_format is greater than max_format".to_owned());
                }
                if min.0 > LAST_PRE_MINOR && !require_old {
                    if supported.is_some() {
                        return Err("supported formats are deprecated".to_owned());
                    }
                    if has_format_field && self.format.is_some() {
                        check_format(min.0, max.0)?;
                    }
                } else {
                    let Some((old_min, old_max)) = supported else {
                        return Err("missing supported formats".to_owned());
                    };
                    if old_min != min.0 || (old_max != max.0 && old_max != LAST_PRE_MINOR) {
                        return Err("version declaration mismatch".to_owned());
                    }
                    check_format(min.0, max.0)?;
                }
                Ok((min, max))
            }
            (None, None, Some((min, max))) => {
                if max > LAST_PRE_MINOR {
                    return Err("missing mandatory fields min_format and max_format".to_owned());
                }
                check_format(min, max)?;
                Ok(((min, 0), (max, 0)))
            }
            (None, None, None) => match self.format {
                Some(f) if has_format_field && f <= LAST_PRE_MINOR => Ok(((f, 0), (f, 0))),
                _ => Err("missing format version information".to_owned()),
            },
        }
    }
}

/// `PackFormat.packCodec(SERVER_DATA)`: the supported range of the `pack` section.
fn pack_formats(pack: &Value) -> Result<(Format, Format), String> {
    Formats {
        min: field(pack, "min_format", |v| format_value(v, 0))?,
        max: field(pack, "max_format", |v| format_value(v, UNKNOWN))?,
        format: field(pack, "pack_format", |v| {
            v.as_i64().filter(|n| i32::try_from(*n).is_ok())
        })?,
        supported: field(pack, "supported_formats", int_range)?,
    }
    .validate(true, false)
}

/// `OverlayMetadataSection.overlaysForVersion`: the overlay directories for this version.
fn overlays(section: &Value) -> Result<Vec<String>, String> {
    let entries = section
        .get("entries")
        .and_then(Value::as_array)
        .ok_or("overlays: missing entries")?;
    let mut parsed = Vec::with_capacity(entries.len());
    for entry in entries {
        let dir = entry
            .get("directory")
            .and_then(Value::as_str)
            .filter(|d| {
                !d.is_empty()
                    && d.chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            })
            .ok_or("overlays: invalid directory")?;
        let min = field(entry, "min_format", |v| format_value(v, 0))?;
        let formats = Formats {
            min,
            max: field(entry, "max_format", |v| format_value(v, UNKNOWN))?,
            format: min.map(|m| m.0),
            supported: field(entry, "formats", int_range)?,
        };
        parsed.push((dir.to_owned(), formats));
    }
    let min_version = parsed
        .iter()
        .map(|(_, f)| f.effective_min_major())
        .min()
        .unwrap_or(UNKNOWN);
    let current = current_format();
    let mut out = Vec::new();
    for (dir, formats) in parsed {
        if formats.min.is_none() && formats.max.is_none() && formats.supported.is_none() {
            log::warn!("Unknown or broken overlay entry {dir}");
            continue;
        }
        let (min, max) = formats
            .validate(false, min_version <= LAST_PRE_MINOR)
            .map_err(|e| format!("Overlay \"{dir}\": {e}"))?;
        if min <= current && current <= max {
            out.push(dir);
        }
    }
    Ok(out)
}

/// Extracts a zip pack into `cache` once per file version (path, size and modification time);
/// older extractions of the same file are removed. Bounded by [`MAX_FILES`] and
/// [`MAX_EXTRACTED`].
fn extract(file: &Path, cache: &Path) -> Result<PathBuf, Error> {
    let meta = std::fs::metadata(file).map_err(io_err(file))?;
    let name: String = file
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("pack")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let mut hasher = Sha1::new();
    hasher.update(
        format!(
            "{}|{}|{:?}",
            file.display(),
            meta.len(),
            meta.modified().ok()
        )
        .as_bytes(),
    );
    let key: String = hasher
        .finalize()
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect();
    let prefix = format!("{name}-");
    let dir = cache.join(format!("{prefix}{key}"));
    if dir.join(COMPLETE).exists() {
        return Ok(dir);
    }
    std::fs::create_dir_all(cache).map_err(io_err(cache))?;
    if let Ok(read) = std::fs::read_dir(cache) {
        for old in read.filter_map(Result::ok) {
            let old_name = old.file_name();
            let old_name = old_name.to_string_lossy();
            if old_name.starts_with(&prefix) && old_name.len() == dir_name_len(&prefix) {
                let _ = std::fs::remove_dir_all(old.path());
            }
        }
    }
    let tmp = cache.join(format!("{prefix}{key}.{}.tmp", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let result = extract_into(file, &tmp);
    if let Err(e) = result {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(e);
    }
    File::create(tmp.join(COMPLETE)).map_err(io_err(&tmp))?;
    if let Err(e) = std::fs::rename(&tmp, &dir) {
        let _ = std::fs::remove_dir_all(&tmp);
        if !dir.join(COMPLETE).exists() {
            return Err(io_err(&dir)(e));
        }
    }
    Ok(dir)
}

/// The length of an extraction directory name with `prefix` (the key is 16 hex digits).
fn dir_name_len(prefix: &str) -> usize {
    prefix.len().saturating_add(16)
}

fn extract_into(file: &Path, dir: &Path) -> Result<(), Error> {
    let mut zip = crate::vanilla::open_zip(file)?;
    if zip.len() > MAX_FILES {
        return Err(Error::Jar(
            file.to_path_buf(),
            format!("more than {MAX_FILES} files"),
        ));
    }
    std::fs::create_dir_all(dir).map_err(io_err(dir))?;
    let mut left = MAX_EXTRACTED;
    for i in 0..zip.len() {
        let entry = zip
            .by_index(i)
            .map_err(|e| Error::Jar(file.to_path_buf(), e.to_string()))?;
        if !entry.is_file() {
            continue;
        }
        // `enclosed_name` rejects absolute paths and `..` components.
        let Some(rel) = entry.enclosed_name() else {
            continue;
        };
        let to = dir.join(rel);
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent).map_err(io_err(parent))?;
        }
        let mut out = BufWriter::new(File::create(&to).map_err(io_err(&to))?);
        let written = std::io::copy(&mut entry.take(left.saturating_add(1)), &mut out)
            .map_err(io_err(&to))?;
        out.flush().map_err(io_err(&to))?;
        left = left.checked_sub(written).ok_or_else(|| {
            Error::Jar(
                file.to_path_buf(),
                format!("extracts to more than {MAX_EXTRACTED} bytes"),
            )
        })?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("vanillite-packs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn mcmeta(extra: &str) -> String {
        format!(
            r#"{{"pack": {{"description": "test", "min_format": 121, "max_format": 121}}{extra}}}"#
        )
    }

    fn folder_pack(folder: &Path, name: &str, meta: &str) {
        let dir = folder.join(name);
        std::fs::create_dir_all(dir.join("data")).unwrap();
        std::fs::write(dir.join("pack.mcmeta"), meta).unwrap();
    }

    fn zip_pack(folder: &Path, name: &str, files: &[(&str, &str)]) {
        use zip::write::{SimpleFileOptions, ZipWriter};
        let mut zip = ZipWriter::new(File::create(folder.join(name)).unwrap());
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (path, text) in files {
            zip.start_file(*path, options).unwrap();
            zip.write_all(text.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }

    /// A Vanilla pack with the feature pack `trade_rebalance`.
    fn vanilla(root: &Path) -> PathBuf {
        let vanilla = root.join("vanilla");
        let bundled = vanilla.join("data/minecraft/datapacks");
        std::fs::create_dir_all(&bundled).unwrap();
        folder_pack(
            &bundled,
            "trade_rebalance",
            &mcmeta(r#", "features": {"enabled": ["minecraft:trade_rebalance"]}"#),
        );
        vanilla
    }

    #[test]
    fn discovers_and_configures_like_vanilla() {
        let root = temp("configure");
        let folder = root.join("datapacks");
        std::fs::create_dir_all(&folder).unwrap();
        folder_pack(&folder, "b", &mcmeta(""));
        folder_pack(&folder, "not_a_pack", "{}");
        std::fs::create_dir_all(folder.join("empty")).unwrap();
        zip_pack(
            &folder,
            "a.zip",
            &[
                ("pack.mcmeta", &mcmeta("")),
                ("data/x/function/f.mcfunction", "say hi"),
            ],
        );
        let mut repo = PackRepository::new(Some(vanilla(&root)), folder.clone());
        let (config, features) = configure(
            &mut repo,
            &DataPackConfig {
                enabled: vec!["vanilla".into(), "file/gone".into()],
                disabled: vec!["file/b".into()],
            },
            &["minecraft:vanilla".to_owned()],
            false,
        );
        assert_eq!(config.enabled, ["vanilla", "file/a.zip"]);
        assert_eq!(config.disabled, ["file/b", "trade_rebalance"]);
        assert_eq!(features, ["minecraft:vanilla"]);
        let ids: Vec<&str> = repo.available().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["file/a.zip", "file/b", "trade_rebalance", "vanilla"]);
        let feature = repo.get("trade_rebalance").unwrap();
        assert_eq!(feature.source, PackSource::Feature);
        assert_eq!(feature.features, ["minecraft:trade_rebalance"]);
        assert_eq!(feature.known_pack.as_deref(), Some("trade_rebalance"));

        // The zip pack opens to an extracted directory, reused while the file is unchanged.
        let cache = root.join("cache");
        let opened = repo.get("file/a.zip").unwrap().open(&cache).unwrap();
        let base = opened.layers.first().unwrap().clone();
        assert!(base.join("data/x/function/f.mcfunction").is_file());
        let again = repo.get("file/a.zip").unwrap().open(&cache).unwrap();
        assert_eq!(again.layers, opened.layers);

        // A world whose features include trade rebalance gets its feature pack forced on.
        let (config, features) = configure(
            &mut repo,
            &DataPackConfig::vanilla(),
            &[
                "minecraft:vanilla".to_owned(),
                "minecraft:trade_rebalance".to_owned(),
            ],
            false,
        );
        assert_eq!(
            config.enabled,
            ["vanilla", "file/a.zip", "file/b", "trade_rebalance"]
        );
        assert_eq!(features, ["minecraft:vanilla", "minecraft:trade_rebalance"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn metadata_formats_and_overlays() {
        let meta = |text: &str| parse_metadata(text).unwrap().unwrap();
        assert_eq!(meta(&mcmeta("")).compatibility, Compatibility::Compatible);
        assert_eq!(
            meta(r#"{"pack": {"description": "", "pack_format": 48}}"#).compatibility,
            Compatibility::TooOld
        );
        assert_eq!(
            meta(r#"{"pack": {"description": "", "min_format": [200, 1], "max_format": 300}}"#)
                .compatibility,
            Compatibility::TooNew
        );
        // A broken declaration falls back to the description.
        assert_eq!(
            meta(r#"{"pack": {"description": "", "min_format": 121}}"#).compatibility,
            Compatibility::Unknown
        );
        assert!(parse_metadata(r#"{"pack": {"min_format": 121, "max_format": 121}}"#).is_err());
        assert!(parse_metadata(r#"{"other": 1}"#).unwrap().is_none());
        assert!(parse_metadata(&mcmeta(r#", "features": {"enabled": ["nope"]}"#)).is_err());
        let overlays = meta(&mcmeta(
            r#", "overlays": {"entries": [
                {"directory": "new", "min_format": 121, "max_format": [121, 5]},
                {"directory": "later", "min_format": 122, "max_format": 130},
                {"directory": "any", "min_format": 100, "max_format": 200}
            ]}"#,
        ))
        .overlays;
        assert_eq!(overlays, ["new", "any"]);
        assert!(
            parse_metadata(&mcmeta(
                r#", "overlays": {"entries": [{"directory": "../x", "min_format": 121, "max_format": 121}]}"#
            ))
            .is_err()
        );
    }
}

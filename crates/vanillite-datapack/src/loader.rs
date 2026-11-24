//! Loading dynamic registries and tags from data pack directories.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde_json::Value;
use serde_json::value::RawValue;
use vanillite_data::dynamic_registries::{DIMENSION, WORLD};
use vanillite_data::registry::Registry;

use crate::{Error, io_err};

/// One registry entry: its id and its JSON, kept as validated raw text until it is used.
#[derive(Debug)]
pub struct Entry {
    pub id: String,
    pub json: Box<RawValue>,
}

/// A data-driven registry; the index of an entry is its protocol id.
#[derive(Debug)]
pub struct DynamicRegistry {
    pub name: &'static str,
    entries: Vec<Entry>,
    index: HashMap<String, usize>,
}

impl DynamicRegistry {
    /// Entries in protocol id order (Vanilla's identifier order: path, then namespace).
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn get(&self, id: &str) -> Option<&Entry> {
        self.index.get(id).map(|&i| &self.entries[i])
    }

    /// Protocol id of `id`.
    pub fn id_of(&self, id: &str) -> Option<usize> {
        self.index.get(id).copied()
    }
}

/// Everything loaded from a set of data packs.
#[derive(Debug)]
pub struct LoadedPacks {
    registries: Vec<DynamicRegistry>,
    /// Registry name → tag id → resolved entry ids.
    tags: BTreeMap<String, BTreeMap<String, Vec<String>>>,
    /// Problems that Vanilla logs and skips (for example tags with missing references).
    pub warnings: Vec<String>,
}

impl LoadedPacks {
    /// The dynamic registry `name` (e.g. `minecraft:worldgen/biome`).
    pub fn registry(&self, name: &str) -> Option<&DynamicRegistry> {
        self.registries.iter().find(|r| r.name == name)
    }

    /// All loaded dynamic registries, in Vanilla's load order.
    pub fn registries(&self) -> &[DynamicRegistry] {
        &self.registries
    }

    /// Tags of a registry: tag id → resolved entry ids.
    pub fn tags(&self, registry: &str) -> Option<&BTreeMap<String, Vec<String>>> {
        self.tags.get(registry)
    }

    /// All registries that have tags.
    pub fn tagged_registries(&self) -> impl Iterator<Item = &str> {
        self.tags.keys().map(String::as_str)
    }

    /// Protocol ids of the entries of a tag, for the client's tag packets.
    pub fn tag_ids(&self, registry: &str, tag: &str) -> Result<Vec<u32>, Error> {
        let entries = self
            .tags
            .get(registry)
            .and_then(|t| t.get(tag))
            .ok_or_else(|| Error::Unknown(format!("unknown tag #{tag} of {registry}")))?;
        entries
            .iter()
            .map(|e| {
                self.entry_id(registry, e)
                    .and_then(|i| u32::try_from(i).ok())
                    .ok_or_else(|| Error::Unknown(format!("{registry}: unknown entry {e}")))
            })
            .collect()
    }

    fn entry_id(&self, registry: &str, entry: &str) -> Option<usize> {
        match Registry::by_name(registry) {
            Some(r) => r.id(entry).map(usize::from),
            None => self.registry(registry)?.id_of(entry),
        }
    }
}

/// Loads the dynamic registries of a world and the tags of all registries from `packs`, each a
/// directory that contains `data/`. Later packs override earlier ones, as in Vanilla.
pub fn load(packs: &[&Path]) -> Result<LoadedPacks, Error> {
    let mut registries = Vec::new();
    for &name in WORLD.iter().chain(DIMENSION.iter()) {
        let mut files: BTreeMap<String, PathBuf> = BTreeMap::new();
        for pack in packs {
            list_entries(pack, &elements_dir(name), &mut files)?;
        }
        let mut ids: Vec<String> = files.keys().cloned().collect();
        ids.sort_by(|a, b| identifier_cmp(a, b));
        let mut entries = Vec::with_capacity(ids.len());
        let mut index = HashMap::with_capacity(ids.len());
        for id in ids {
            let path = &files[&id];
            let text = std::fs::read_to_string(path).map_err(io_err(path))?;
            let json = RawValue::from_string(text).map_err(|e| Error::Json(path.clone(), e))?;
            index.insert(id.clone(), entries.len());
            entries.push(Entry { id, json });
        }
        registries.push(DynamicRegistry {
            name,
            entries,
            index,
        });
    }

    let mut loaded = LoadedPacks {
        registries,
        tags: BTreeMap::new(),
        warnings: Vec::new(),
    };
    let tagged: Vec<&'static str> = vanillite_data::registry::ALL
        .iter()
        .map(|r| r.name)
        .chain(WORLD.iter().copied())
        .collect();
    for registry in tagged {
        let raw = read_tags(packs, registry)?;
        if raw.is_empty() {
            continue;
        }
        let resolved = resolve_tags(&raw, |e| loaded.entry_id(registry, e).is_some());
        let mut tags = BTreeMap::new();
        for (tag, result) in resolved {
            match result {
                Ok(entries) => {
                    tags.insert(tag, entries);
                }
                Err(missing) => loaded.warnings.push(format!(
                    "couldn't load tag {tag} of {registry}: missing references {missing:?}"
                )),
            }
        }
        loaded.tags.insert(registry.to_owned(), tags);
    }
    Ok(loaded)
}

/// Vanilla's `Identifier` order: path first, then namespace.
fn identifier_cmp(a: &str, b: &str) -> Ordering {
    let split = |s: &str| -> (String, String) {
        match s.split_once(':') {
            Some((ns, path)) => (path.to_owned(), ns.to_owned()),
            None => (s.to_owned(), "minecraft".to_owned()),
        }
    };
    split(a).cmp(&split(b))
}

/// `minecraft:worldgen/biome` → `worldgen/biome`; other namespaces keep theirs as a prefix.
fn elements_dir(registry: &str) -> String {
    match registry.split_once(':') {
        Some(("minecraft", path)) => path.to_owned(),
        Some((ns, path)) => format!("{ns}/{path}"),
        None => registry.to_owned(),
    }
}

fn normalize_id(id: &str) -> String {
    if id.contains(':') {
        id.to_owned()
    } else {
        format!("minecraft:{id}")
    }
}

/// Collects `data/<ns>/<dir>/**/*.json` of a pack as `ns:relative/path` → file.
fn list_entries(pack: &Path, dir: &str, out: &mut BTreeMap<String, PathBuf>) -> Result<(), Error> {
    let data = pack.join("data");
    let Ok(namespaces) = std::fs::read_dir(&data) else {
        return Ok(());
    };
    for ns in namespaces {
        let ns = ns.map_err(io_err(&data))?;
        let Some(ns_name) = ns.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let base = ns.path().join(dir);
        if base.is_dir() {
            walk(&base, &base, &ns_name, out)?;
        }
    }
    Ok(())
}

fn walk(
    base: &Path,
    dir: &Path,
    ns: &str,
    out: &mut BTreeMap<String, PathBuf>,
) -> Result<(), Error> {
    for entry in std::fs::read_dir(dir).map_err(io_err(dir))? {
        let path = entry.map_err(io_err(dir))?.path();
        if path.is_dir() {
            walk(base, &path, ns, out)?;
        } else if path.extension().is_some_and(|e| e == "json") {
            let Ok(rel) = path
                .with_extension("")
                .strip_prefix(base)
                .map(Path::to_path_buf)
            else {
                continue;
            };
            let parts: Vec<&str> = rel.iter().filter_map(|p| p.to_str()).collect();
            out.insert(format!("{ns}:{}", parts.join("/")), path);
        }
    }
    Ok(())
}

/// A tag value: an entry or `#tag`, and whether it must exist.
type TagValue = (String, bool);

/// Tag files of all packs merged in pack order (`replace` drops earlier values).
fn read_tags(packs: &[&Path], registry: &str) -> Result<BTreeMap<String, Vec<TagValue>>, Error> {
    let dir = format!("tags/{}", elements_dir(registry));
    let mut tags: BTreeMap<String, Vec<TagValue>> = BTreeMap::new();
    for pack in packs {
        let mut files = BTreeMap::new();
        list_entries(pack, &dir, &mut files)?;
        for (tag, path) in files {
            let text = std::fs::read_to_string(&path).map_err(io_err(&path))?;
            let json: Value =
                serde_json::from_str(&text).map_err(|e| Error::Json(path.clone(), e))?;
            let values = tags.entry(tag).or_default();
            if json["replace"].as_bool() == Some(true) {
                values.clear();
            }
            for v in json["values"].as_array().into_iter().flatten() {
                let (id, required) = match v {
                    Value::String(s) => (s.as_str(), true),
                    other => (
                        other["id"].as_str().unwrap_or_default(),
                        other["required"].as_bool().unwrap_or(true),
                    ),
                };
                let id = match id.strip_prefix('#') {
                    Some(t) => format!("#{}", normalize_id(t)),
                    None => normalize_id(id),
                };
                values.push((id, required));
            }
        }
    }
    Ok(tags)
}

/// Resolves nested tags. A tag with missing required references (or a cycle) fails with the
/// list of what is missing, like Vanilla's tag loader.
fn resolve_tags(
    raw: &BTreeMap<String, Vec<TagValue>>,
    exists: impl Fn(&str) -> bool,
) -> BTreeMap<String, Result<Vec<String>, Vec<String>>> {
    fn visit(
        tag: &str,
        raw: &BTreeMap<String, Vec<TagValue>>,
        exists: &dyn Fn(&str) -> bool,
        done: &mut BTreeMap<String, Result<Vec<String>, Vec<String>>>,
        stack: &mut Vec<String>,
    ) {
        if done.contains_key(tag) {
            return;
        }
        if stack.iter().any(|t| t == tag) {
            done.insert(tag.to_owned(), Err(vec![format!("#{tag} (cycle)")]));
            return;
        }
        stack.push(tag.to_owned());
        let mut entries: Vec<String> = Vec::new();
        let mut missing = Vec::new();
        for (value, required) in raw.get(tag).into_iter().flatten() {
            if let Some(inner) = value.strip_prefix('#') {
                if raw.contains_key(inner) {
                    visit(inner, raw, exists, done, stack);
                }
                match done.get(inner) {
                    Some(Ok(list)) => {
                        for e in list {
                            if !entries.contains(e) {
                                entries.push(e.clone());
                            }
                        }
                    }
                    _ if *required => missing.push(value.clone()),
                    _ => {}
                }
            } else if exists(value) {
                if !entries.contains(value) {
                    entries.push(value.clone());
                }
            } else if *required {
                missing.push(value.clone());
            }
        }
        stack.pop();
        let result = if missing.is_empty() {
            Ok(entries)
        } else {
            Err(missing)
        };
        done.entry(tag.to_owned()).or_insert(result);
    }

    let mut done = BTreeMap::new();
    for tag in raw.keys() {
        visit(tag, raw, &exists, &mut done, &mut Vec::new());
    }
    done
}

//! `StructureTemplateManager`: loads each template of the data packs once and shares it.

use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError, RwLock};

use rustc_hash::FxHashMap;
use vanillite_datapack::LoadedPacks;

use super::structure::StructureTemplate;
use crate::Error;
use crate::blocks::qualify;

/// A thread safe cache of structure templates read from the world's
/// `generated/<ns>/structure/<path>.nbt` (when it has one) and then from
/// `data/<ns>/structure/<path>.nbt` of the data packs (later packs override earlier ones, like
/// Vanilla's resource manager).
#[derive(Debug, Default)]
pub struct StructureTemplateManager {
    roots: RwLock<Vec<PathBuf>>,
    /// The world's `generated` directory: read first, structure blocks save there.
    generated: Option<PathBuf>,
    /// Loaded templates; `None` for ids without a file.
    cache: RwLock<FxHashMap<String, Option<Arc<StructureTemplate>>>>,
}

impl StructureTemplateManager {
    /// A manager over the packs' directories.
    pub fn new(packs: &LoadedPacks) -> Self {
        Self::from_roots(packs.roots().to_vec())
    }

    /// A manager over pack directories (each contains `data/`), in load order.
    pub fn from_roots(roots: Vec<PathBuf>) -> Self {
        Self {
            roots: RwLock::new(roots),
            generated: None,
            cache: RwLock::new(FxHashMap::default()),
        }
    }

    /// A manager of a world: its `generated` directory first, then the pack directories.
    pub fn for_world(roots: Vec<PathBuf>, generated: PathBuf) -> Self {
        Self {
            generated: Some(generated),
            ..Self::from_roots(roots)
        }
    }

    /// `onResourceManagerReload`: new pack directories, every cached template goes.
    pub fn reload(&self, roots: Vec<PathBuf>) {
        *self.roots.write().unwrap_or_else(PoisonError::into_inner) = roots;
        self.cache
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    /// The file of `id` below a `generated` directory (`<ns>/structure/<path>.nbt`).
    fn generated_file(dir: &Path, id: &str) -> Option<PathBuf> {
        let (namespace, path) = id.split_once(':')?;
        if !portable(namespace) || !path.split('/').all(portable) {
            return None;
        }
        Some(
            dir.join(namespace)
                .join("structure")
                .join(format!("{path}.nbt")),
        )
    }

    /// Puts `template` into the cache as `id` (`getOrCreate(id)` filled in place).
    pub fn put(&self, id: &str, template: StructureTemplate) {
        self.cache
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(qualify(id), Some(Arc::new(template)));
    }

    /// `remove(id)`: drops the cached template, the next `get` reads the files again.
    pub fn remove(&self, id: &str) {
        self.cache
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&qualify(id));
    }

    /// `save(id)`: writes the cached template of `id` to the world's `generated` directory.
    /// `Ok(false)` without a cached template, a `generated` directory or a valid file name; an
    /// error when writing fails.
    pub fn save(&self, id: &str) -> Result<bool, Error> {
        let id = qualify(id);
        let Some(Some(template)) = self
            .cache
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&id)
            .cloned()
        else {
            return Ok(false);
        };
        let Some(file) = self
            .generated
            .as_deref()
            .and_then(|dir| Self::generated_file(dir, &id))
        else {
            return Ok(false);
        };
        let failed = |e: &dyn std::fmt::Display| {
            Error::new(format!("structure file {id} to {}: {e}", file.display()))
        };
        let bytes = template.to_gzip().map_err(|e| failed(&e))?;
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).map_err(|e| failed(&e))?;
        }
        std::fs::write(&file, bytes).map_err(|e| failed(&e))?;
        Ok(true)
    }

    fn file(root: &Path, id: &str) -> Option<PathBuf> {
        let (namespace, path) = id.split_once(':')?;
        if path
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
        {
            return None;
        }
        Some(
            root.join("data")
                .join(namespace)
                .join("structure")
                .join(format!("{path}.nbt")),
        )
    }

    fn load(&self, id: &str) -> Result<Option<Arc<StructureTemplate>>, Error> {
        if let Some(file) = self
            .generated
            .as_deref()
            .and_then(|dir| Self::generated_file(dir, id))
        {
            match std::fs::read(&file) {
                Ok(bytes) => {
                    // `tryLoad`: a source that fails counts as not having the template.
                    if let Ok(template) = StructureTemplate::from_gzip(&bytes) {
                        return Ok(Some(Arc::new(template)));
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(Error::new(format!("{}: {e}", file.display()))),
            }
        }
        let roots = self
            .roots
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        for root in roots.iter().rev() {
            let Some(file) = Self::file(root, id) else {
                return Ok(None);
            };
            match std::fs::read(&file) {
                Ok(bytes) => {
                    let template = StructureTemplate::from_gzip(&bytes)
                        .map_err(|e| Error::new(format!("structure {id}: {e}")))?;
                    return Ok(Some(Arc::new(template)));
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(Error::new(format!("{}: {e}", file.display()))),
            }
        }
        Ok(None)
    }

    /// `get(id)`: the template, loaded on first use; `None` if no pack has it. A file that
    /// cannot be read is an error (Vanilla logs it and treats the template as missing).
    pub fn get(&self, id: &str) -> Result<Option<Arc<StructureTemplate>>, Error> {
        let id = qualify(id);
        if let Some(cached) = self
            .cache
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&id)
        {
            return Ok(cached.clone());
        }
        let loaded = self.load(&id)?;
        let mut cache = self.cache.write().unwrap_or_else(PoisonError::into_inner);
        Ok(cache.entry(id).or_insert(loaded).clone())
    }

    /// `getOrCreate(id)`: like [`Self::get`], with an empty template for missing ids (placing it
    /// does nothing).
    pub fn get_or_create(&self, id: &str) -> Result<Arc<StructureTemplate>, Error> {
        let id = qualify(id);
        if let Some(template) = self.get(&id)? {
            return Ok(template);
        }
        let mut cache = self.cache.write().unwrap_or_else(PoisonError::into_inner);
        let entry = cache.entry(id).or_insert(None);
        Ok(Arc::clone(entry.get_or_insert_with(|| {
            Arc::new(StructureTemplate::default())
        })))
    }

    /// `listTemplates()`: the ids of every template file of the packs, sorted.
    pub fn list_templates(&self) -> Result<Vec<String>, Error> {
        let mut ids = Vec::new();
        if let Some(dir) = &self.generated
            && let Ok(namespaces) = std::fs::read_dir(dir)
        {
            for namespace in namespaces.flatten() {
                let Some(ns) = namespace.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                let base = namespace.path().join("structure");
                if base.is_dir() {
                    collect(&base, &base, &ns, &mut ids)?;
                }
            }
        }
        let roots = self
            .roots
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        for root in &roots {
            let data = root.join("data");
            let Ok(namespaces) = std::fs::read_dir(&data) else {
                continue;
            };
            for namespace in namespaces {
                let namespace =
                    namespace.map_err(|e| Error::new(format!("{}: {e}", data.display())))?;
                let Some(ns) = namespace.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                let base = namespace.path().join("structure");
                if base.is_dir() {
                    collect(&base, &base, &ns, &mut ids)?;
                }
            }
        }
        ids.sort();
        ids.dedup();
        Ok(ids)
    }
}

/// `FileUtil.isPathPartPortable` and the identifier rules a path part must meet.
fn portable(part: &str) -> bool {
    !part.is_empty()
        && !part.ends_with('.')
        && part
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' | b'.'))
        && !RESERVED.iter().any(|r| {
            part.split('.')
                .next()
                .is_some_and(|stem| stem.eq_ignore_ascii_case(r))
        })
}

/// Windows' reserved file names (`FileUtil.RESERVED_WINDOWS_FILENAMES`).
const RESERVED: [&str; 24] = [
    "com", "clock$", "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6",
    "com7", "com8", "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

fn collect(base: &Path, dir: &Path, ns: &str, out: &mut Vec<String>) -> Result<(), Error> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| Error::new(format!("{}: {e}", dir.display())))?;
    for entry in entries {
        let path = entry
            .map_err(|e| Error::new(format!("{}: {e}", dir.display())))?
            .path();
        if path.is_dir() {
            collect(base, &path, ns, out)?;
        } else if path.extension().is_some_and(|e| e == "nbt")
            && let Ok(rel) = path.with_extension("").strip_prefix(base)
        {
            let parts: Vec<&str> = rel.iter().filter_map(|p| p.to_str()).collect();
            out.push(format!("{ns}:{}", parts.join("/")));
        }
    }
    Ok(())
}

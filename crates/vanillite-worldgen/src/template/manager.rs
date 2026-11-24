//! `StructureTemplateManager`: loads each template of the data packs once and shares it.

use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError, RwLock};

use rustc_hash::FxHashMap;
use vanillite_datapack::LoadedPacks;

use super::structure::StructureTemplate;
use crate::Error;
use crate::blocks::qualify;

/// A thread safe cache of structure templates read from `data/<ns>/structure/<path>.nbt` of the
/// data packs (later packs override earlier ones, like Vanilla's resource manager).
#[derive(Debug, Default)]
pub struct StructureTemplateManager {
    roots: Vec<PathBuf>,
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
            roots,
            cache: RwLock::new(FxHashMap::default()),
        }
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
        for root in self.roots.iter().rev() {
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
        for root in &self.roots {
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

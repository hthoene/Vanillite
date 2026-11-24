//! Static (built-in) registries with their protocol ids.
//!
//! One `static` per registry, named after it (`BLOCK`, `ITEM`, `WORLDGEN_BIOME_SOURCE`, …), and
//! [`ALL`] with every registry sorted by name.

pub use crate::generated::registries::*;

/// A static registry: entry names in protocol id order.
#[derive(Debug)]
pub struct Registry {
    pub name: &'static str,
    /// Id of the registry itself in `minecraft:root`.
    pub protocol_id: u32,
    /// Default entry of a defaulted registry.
    pub default: Option<&'static str>,
    /// Entry names; the index is the protocol id.
    pub entries: &'static [&'static str],
    /// Entry ids sorted by name, for lookups.
    pub sorted: &'static [u16],
}

impl Registry {
    /// Protocol id of `name`.
    pub fn id(&self, name: &str) -> Option<u16> {
        let i = self
            .sorted
            .binary_search_by(|&i| self.entries[usize::from(i)].cmp(name))
            .ok()?;
        Some(self.sorted[i])
    }

    /// Name of the entry with protocol id `id`.
    #[inline]
    pub fn name(&self, id: u16) -> Option<&'static str> {
        self.entries.get(usize::from(id)).copied()
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Looks a registry up by name, e.g. `minecraft:block`.
    pub fn by_name(name: &str) -> Option<&'static Registry> {
        ALL.binary_search_by(|r| r.name.cmp(name))
            .ok()
            .map(|i| ALL[i])
    }
}

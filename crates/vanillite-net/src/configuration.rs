//! Pre-encoded data of the configuration phase. Registries, tags, feature flags and known packs
//! are immutable during a server run, so they are encoded once and shared by every connection.

use std::sync::Arc;
use vanillite_datapack::LoadedPacks;
use vanillite_nbt::{Compound, List, Tag};
use vanillite_protocol::SharedPacket;
use vanillite_protocol::frame::encode_body;
use vanillite_protocol::packets::common::{RegistryTags, TagEntries};
use vanillite_protocol::packets::configuration::clientbound::{
    RegistryData, RegistryEntry, SelectKnownPacks, UpdateEnabledFeatures, UpdateTags,
};
use vanillite_protocol::types::{Identifier, KnownPack, RawNbt, VarIntList};

/// Everything the configuration phase sends besides per-connection packets.
#[derive(Debug, Clone)]
pub struct ConfigurationData {
    pub(crate) offered_packs: Vec<KnownPack>,
    pub(crate) features: Arc<SharedPacket>,
    pub(crate) select_known_packs: Arc<SharedPacket>,
    /// Registry data without entry contents, for clients that have the offered packs.
    pub(crate) registries_known: Vec<Arc<SharedPacket>>,
    /// Registry data with entry contents.
    pub(crate) registries_full: Vec<Arc<SharedPacket>>,
    pub(crate) tags: Arc<SharedPacket>,
}

impl ConfigurationData {
    /// Encodes the configuration data from already built packets.
    pub fn new(
        offered_packs: Vec<KnownPack>,
        features: Vec<Identifier>,
        registries_known: Vec<RegistryData>,
        registries_full: Vec<RegistryData>,
        tags: UpdateTags,
    ) -> Self {
        let shared = |body: Vec<u8>| Arc::new(SharedPacket::new(body));
        Self {
            select_known_packs: shared(encode_body(&SelectKnownPacks {
                packs: offered_packs.clone(),
            })),
            offered_packs,
            features: shared(encode_body(&UpdateEnabledFeatures { features })),
            registries_known: registries_known
                .iter()
                .map(|r| shared(encode_body(r)))
                .collect(),
            registries_full: registries_full
                .iter()
                .map(|r| shared(encode_body(r)))
                .collect(),
            tags: shared(encode_body(&tags)),
        }
    }

    /// Builds the data from loaded data packs: every synchronised registry in Vanilla's order
    /// and the tags of all registries the client knows. Every entry is assumed to come from the
    /// Vanilla pack (`minecraft:core`), which is offered as known pack.
    pub fn from_packs(packs: &LoadedPacks, features: &[&str]) -> Self {
        use vanillite_data::dynamic_registries::{SYNCHRONIZED, WORLD};
        let mut known = Vec::new();
        let mut full = Vec::new();
        for &name in &SYNCHRONIZED {
            let registry = Identifier::vanilla(name.trim_start_matches("minecraft:"));
            let entries = packs
                .registry(name)
                .map(|r| r.entries())
                .unwrap_or_default();
            let mut known_entries = Vec::with_capacity(entries.len());
            let mut full_entries = Vec::with_capacity(entries.len());
            for e in entries {
                let Ok(id) = Identifier::new(&e.id) else {
                    continue;
                };
                let data = serde_json::from_str::<serde_json::Value>(e.json.get())
                    .ok()
                    .map(|v| {
                        let mut bytes = Vec::new();
                        // Registry contents come from our own packs; a string too long for NBT
                        // degrades to an empty compound.
                        if vanillite_nbt::write::write_network(&json_to_nbt(&v), &mut bytes)
                            .is_err()
                        {
                            bytes = vec![10, 0];
                        }
                        RawNbt(bytes)
                    });
                known_entries.push(RegistryEntry {
                    id: id.clone(),
                    data: None,
                });
                full_entries.push(RegistryEntry { id, data });
            }
            known.push(RegistryData {
                registry: registry.clone(),
                entries: known_entries,
            });
            full.push(RegistryData {
                registry,
                entries: full_entries,
            });
        }

        let mut tags = Vec::new();
        for registry in packs.tagged_registries() {
            let networked = SYNCHRONIZED.contains(&registry) || !WORLD.contains(&registry);
            let (true, Ok(registry_id), Some(registry_tags)) =
                (networked, Identifier::new(registry), packs.tags(registry))
            else {
                continue;
            };
            let entries: Vec<TagEntries> = registry_tags
                .keys()
                .filter_map(|tag| {
                    let ids = packs.tag_ids(registry, tag).ok()?;
                    Some(TagEntries {
                        name: Identifier::new(tag).ok()?,
                        entries: VarIntList(
                            ids.into_iter()
                                .filter_map(|i| i32::try_from(i).ok())
                                .collect(),
                        ),
                    })
                })
                .collect();
            tags.push(RegistryTags {
                registry: registry_id,
                tags: entries,
            });
        }

        let core = KnownPack {
            namespace: "minecraft".into(),
            id: "core".into(),
            version: vanillite_protocol::VERSION_ID.into(),
        };
        let features = features
            .iter()
            .filter_map(|f| Identifier::new(f).ok())
            .collect();
        Self::new(
            vec![core],
            features,
            known,
            full,
            UpdateTags { registries: tags },
        )
    }
}

/// Converts data pack JSON to NBT the way Vanilla's codecs accept it: integers become ints (or
/// longs), other numbers doubles, booleans bytes.
pub fn json_to_nbt(v: &serde_json::Value) -> Tag {
    use serde_json::Value;
    match v {
        Value::Null => Tag::Compound(Compound::new()),
        Value::Bool(b) => Tag::from(*b),
        Value::Number(n) => match n.as_i64() {
            Some(i) => i32::try_from(i).map_or(Tag::Long(i), Tag::Int),
            None => Tag::Double(n.as_f64().unwrap_or(0.0)),
        },
        Value::String(s) => Tag::String(s.clone()),
        Value::Array(a) => Tag::List(List(a.iter().map(json_to_nbt).collect())),
        Value::Object(o) => {
            let mut c = Compound::with_capacity(o.len());
            for (k, v) in o {
                c.insert(k.as_str(), json_to_nbt(v));
            }
            Tag::Compound(c)
        }
    }
}

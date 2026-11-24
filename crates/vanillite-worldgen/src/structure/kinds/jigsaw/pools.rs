//! `StructureTemplatePool` (registry `worldgen/template_pool`) and pool aliases
//! (`pools/alias`).
//!
//! Each jigsaw structure parses the pools it can reach once: its start pool, every pool a
//! jigsaw block of a reachable element names, the fallbacks and the alias targets. Pool and
//! jigsaw names are resolved to ids at parse time, so placement does no string lookups.

use std::sync::Arc;

use rustc_hash::FxHashMap;
use serde_json::Value;
use vanillite_base::BlockPos;
use vanillite_base::random::{LegacyRandom, PositionalRandomFactory, RandomSource};

use super::element::{EMPTY_ID, PoolElement, shuffle};
use crate::Error;
use crate::blocks::qualify;
use crate::carver::providers::WeightedList;
use crate::feature::{field, int_field};
use crate::structure::StructureParser;
use crate::template::Rotation;

const TEMPLATE_POOL_REGISTRY: &str = "minecraft:worldgen/template_pool";

/// A template pool of a structure's [`TemplatePools`]: a registry key (which may have no
/// entry) or an inline pool.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PoolId(u32);

/// An interned jigsaw name or target (`Identifier`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Symbol(u32);

/// `StructureTemplatePool`.
#[derive(Debug)]
pub struct StructureTemplatePool {
    /// The distinct elements (`rawTemplates` without weights).
    elements: Vec<Arc<PoolElement>>,
    /// `templates`: each element repeated by its weight, as indices into `elements`.
    templates: Vec<u32>,
    /// `fallback`.
    fallback: PoolId,
    /// `getMaxSize(manager)` (computed at parse time; it only reads template sizes).
    max_size: i32,
}

impl StructureTemplatePool {
    /// `size()`.
    pub fn size(&self) -> usize {
        self.templates.len()
    }

    /// `getFallback()`.
    pub fn fallback(&self) -> PoolId {
        self.fallback
    }

    /// `getMaxSize(manager)`: the largest y span of a non-empty element.
    pub fn max_size(&self) -> i32 {
        self.max_size
    }

    /// `getRandomTemplate(random)`; `None` for `EmptyPoolElement.INSTANCE` (an empty pool
    /// draws nothing).
    pub fn random_template<R: RandomSource + ?Sized>(
        &self,
        random: &mut R,
    ) -> Option<&Arc<PoolElement>> {
        if self.templates.is_empty() {
            return None;
        }
        let index = random.next_int_bounded(self.templates.len() as i32) as usize;
        Some(&self.elements[self.templates[index] as usize])
    }

    /// `getShuffledTemplates(random)` (`Util.shuffledCopy`), appended to `out`.
    pub fn shuffled_templates<'p, R: RandomSource + ?Sized>(
        &'p self,
        random: &mut R,
        out: &mut Vec<&'p Arc<PoolElement>>,
    ) {
        let start = out.len();
        out.extend(
            self.templates
                .iter()
                .map(|&index| &self.elements[index as usize]),
        );
        shuffle(&mut out[start..], random);
    }
}

/// A pool slot: the registry key (`None` for inline pools) and the pool (`None` for a key
/// without registry entry).
#[derive(Debug)]
struct PoolSlot {
    name: Option<String>,
    pool: Option<StructureTemplatePool>,
}

/// The template pools a jigsaw structure can reach (Vanilla's `Registry<StructureTemplatePool>`
/// as `JigsawPlacement` sees it).
#[derive(Debug)]
pub struct TemplatePools {
    slots: Vec<PoolSlot>,
    empty: PoolId,
}

impl TemplatePools {
    /// `pools.get(key)`: the pool, `None` for a key without registry entry.
    pub fn get(&self, id: PoolId) -> Option<&StructureTemplatePool> {
        self.slots[id.0 as usize].pool.as_ref()
    }

    /// Whether the pool has a registry key (`unwrapKey().isPresent()`).
    pub fn is_registered(&self, id: PoolId) -> bool {
        self.slots[id.0 as usize].name.is_some()
    }

    /// `holder.is(Pools.EMPTY)`.
    pub fn is_empty_pool(&self, id: PoolId) -> bool {
        id == self.empty
    }

    /// The registry key of a pool (`None` for inline pools).
    pub fn name(&self, id: PoolId) -> Option<&str> {
        self.slots[id.0 as usize].name.as_deref()
    }
}

/// Builds [`TemplatePools`]: interns pool keys and jigsaw names and parses every reachable
/// pool once.
#[derive(Debug)]
pub struct PoolsBuilder {
    slots: Vec<PoolSlot>,
    by_name: FxHashMap<String, PoolId>,
    symbols: FxHashMap<String, Symbol>,
    /// Keys whose registry entry is not parsed yet.
    pending: Vec<PoolId>,
    /// Pools that must have a registry entry (fallbacks and start pools are bound holders).
    required: Vec<PoolId>,
    empty: PoolId,
}

impl Default for PoolsBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl PoolsBuilder {
    /// An empty builder (with `minecraft:empty` interned).
    pub fn new() -> Self {
        let mut builder = Self {
            slots: Vec::new(),
            by_name: FxHashMap::default(),
            symbols: FxHashMap::default(),
            pending: Vec::new(),
            required: Vec::new(),
            empty: PoolId(0),
        };
        builder.empty = builder.named(EMPTY_ID);
        builder
    }

    /// `Pools.EMPTY`.
    pub fn empty_pool(&self) -> PoolId {
        self.empty
    }

    /// The id of a pool key (`ResourceKey<StructureTemplatePool>`); its entry is parsed later.
    pub fn named(&mut self, name: &str) -> PoolId {
        let name = qualify(name);
        if let Some(&id) = self.by_name.get(&name) {
            return id;
        }
        let id = PoolId(self.slots.len() as u32);
        self.slots.push(PoolSlot {
            name: Some(name.clone()),
            pool: None,
        });
        self.by_name.insert(name, id);
        self.pending.push(id);
        id
    }

    /// An interned identifier.
    pub fn symbol(&mut self, name: &str) -> Symbol {
        let name = qualify(name);
        let next = Symbol(self.symbols.len() as u32);
        *self.symbols.entry(name).or_insert(next)
    }

    /// A `Holder<StructureTemplatePool>` that must be bound: a registry key or an inline pool.
    pub fn holder(
        &mut self,
        json: &Value,
        parser: &mut StructureParser<'_>,
    ) -> Result<PoolId, Error> {
        if let Some(name) = json.as_str() {
            let id = self.named(name);
            self.required.push(id);
            return Ok(id);
        }
        let pool = self.parse_pool(json, parser)?;
        let id = PoolId(self.slots.len() as u32);
        self.slots.push(PoolSlot {
            name: None,
            pool: Some(pool),
        });
        Ok(id)
    }

    /// `StructureTemplatePool.DIRECT_CODEC`.
    fn parse_pool(
        &mut self,
        json: &Value,
        parser: &mut StructureParser<'_>,
    ) -> Result<StructureTemplatePool, Error> {
        let fallback = self.holder(field(json, "fallback")?, parser)?;
        let entries = field(json, "elements")?
            .as_array()
            .ok_or_else(|| Error::new("pool elements is not a list"))?;
        let mut elements = Vec::with_capacity(entries.len());
        let mut templates = Vec::new();
        for entry in entries {
            let weight = int_field(entry, "weight")?;
            if !(1..=150).contains(&weight) {
                return Err(Error::new(format!(
                    "pool element weight {weight} outside [1, 150]"
                )));
            }
            let element = PoolElement::parse(field(entry, "element")?, None, parser, self)?;
            let index = elements.len() as u32;
            elements.push(Arc::new(element));
            templates.extend(std::iter::repeat_n(index, weight as usize));
        }
        let max_size = elements
            .iter()
            .filter(|e| !matches!(***e, PoolElement::Empty))
            .map(|e| e.bounding_box(BlockPos::ZERO, Rotation::None).y_span())
            .max()
            .unwrap_or(0);
        Ok(StructureTemplatePool {
            elements,
            templates,
            fallback,
            max_size,
        })
    }

    /// Parses the registry entry of every interned key (and of the keys their elements name).
    pub fn finish(mut self, parser: &mut StructureParser<'_>) -> Result<TemplatePools, Error> {
        while let Some(id) = self.pending.pop() {
            let Some(name) = self.slots[id.0 as usize].name.clone() else {
                continue;
            };
            let Some(entry) = parser
                .packs()
                .registry(TEMPLATE_POOL_REGISTRY)
                .and_then(|r| r.get(&name))
            else {
                continue;
            };
            let json: Value = serde_json::from_str(entry.json.get())
                .map_err(|e| Error::new(format!("template pool {name}: {e}")))?;
            let pool = self
                .parse_pool(&json, parser)
                .map_err(|e| Error::new(format!("template pool {name}: {e}")))?;
            self.slots[id.0 as usize].pool = Some(pool);
        }
        for id in &self.required {
            let slot = &self.slots[id.0 as usize];
            if slot.pool.is_none() {
                return Err(Error::new(format!(
                    "unknown template pool {}",
                    slot.name.as_deref().unwrap_or("<inline>")
                )));
            }
        }
        Ok(TemplatePools {
            slots: self.slots,
            empty: self.empty,
        })
    }
}

/// `PoolAliasBinding` (registry `pool_alias_binding`).
#[derive(Debug)]
pub enum PoolAliasBinding {
    /// `direct`.
    Direct { alias: PoolId, target: PoolId },
    /// `random`.
    Random {
        alias: PoolId,
        targets: WeightedList<PoolId>,
    },
    /// `random_group`.
    RandomGroup(WeightedList<Vec<PoolAliasBinding>>),
}

impl PoolAliasBinding {
    /// Parses `PoolAliasBinding.CODEC`; every target is interned (and so parsed).
    pub fn parse(json: &Value, pools: &mut PoolsBuilder) -> Result<Self, Error> {
        let kind = json
            .get("type")
            .and_then(Value::as_str)
            .map(qualify)
            .ok_or_else(|| Error::new("pool alias binding without type"))?;
        let key = |json: &Value, pools: &mut PoolsBuilder| -> Result<PoolId, Error> {
            json.as_str()
                .map(|name| pools.named(name))
                .ok_or_else(|| Error::new(format!("invalid template pool key {json}")))
        };
        Ok(match kind.as_str() {
            "minecraft:direct" => Self::Direct {
                alias: key(field(json, "alias")?, pools)?,
                target: key(field(json, "target")?, pools)?,
            },
            "minecraft:random" => Self::Random {
                alias: key(field(json, "alias")?, pools)?,
                targets: WeightedList::from_json(field(json, "targets")?, |t| key(t, pools))?,
            },
            "minecraft:random_group" => {
                Self::RandomGroup(WeightedList::from_json(field(json, "groups")?, |group| {
                    group
                        .as_array()
                        .ok_or_else(|| Error::new("pool alias group is not a list"))?
                        .iter()
                        .map(|binding| Self::parse(binding, pools))
                        .collect()
                })?)
            }
            other => return Err(Error::new(format!("unknown pool alias binding {other}"))),
        })
    }

    /// `forEachResolved(random, consumer)`.
    fn for_each_resolved(&self, random: &mut LegacyRandom, out: &mut Vec<(PoolId, PoolId)>) {
        match self {
            Self::Direct { alias, target } => out.push((*alias, *target)),
            Self::Random { alias, targets } => out.push((*alias, *targets.get_random(random))),
            Self::RandomGroup(groups) => {
                for binding in groups.get_random(random) {
                    binding.for_each_resolved(random, out);
                }
            }
        }
    }
}

/// `PoolAliasLookup`: the aliases resolved for one start.
#[derive(Clone, Debug, Default)]
pub struct PoolAliasLookup {
    mappings: Vec<(PoolId, PoolId)>,
}

impl PoolAliasLookup {
    /// `PoolAliasLookup.create(bindings, pos, seed)`: resolved with
    /// `RandomSource.create(seed).forkPositional().at(pos)`. Vanilla's map builder throws on
    /// an alias bound twice; the first binding wins here.
    pub fn create(bindings: &[PoolAliasBinding], pos: BlockPos, seed: i64) -> Self {
        if bindings.is_empty() {
            return Self::default();
        }
        let mut random = LegacyRandom::new(seed)
            .fork_positional()
            .at(pos.x, pos.y, pos.z);
        let mut mappings = Vec::new();
        for binding in bindings {
            binding.for_each_resolved(&mut random, &mut mappings);
        }
        Self { mappings }
    }

    /// `lookup(alias)`.
    pub fn lookup(&self, pool: PoolId) -> PoolId {
        self.mappings
            .iter()
            .find(|(alias, _)| *alias == pool)
            .map_or(pool, |(_, target)| *target)
    }
}

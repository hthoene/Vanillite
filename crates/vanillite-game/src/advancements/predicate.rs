//! The predicates of advancement criteria (`advancements.predicates`) and the loot conditions
//! they are wrapped in (`LootItemCondition`). Parts the server cannot evaluate yet (structures,
//! light, data components, named predicates, ...) parse into [`Unsupported`](Condition) parts
//! that never match, so such criteria are never granted by triggers.

use std::borrow::Cow;

use serde_json::{Map, Value};
use vanillite_base::{BlockPos, Vec3};
use vanillite_config::GameMode;
use vanillite_data::block::{BlockId, BlockState};
use vanillite_data::registry;

use crate::item::ItemStack;
use crate::stats::{Stat, StatType, StatsCounter};

/// A parse result; the error names what is wrong.
pub type Res<T> = Result<T, String>;

pub(crate) fn object(v: &Value) -> Res<&Map<String, Value>> {
    v.as_object().ok_or_else(|| format!("not an object: {v}"))
}

pub(crate) fn string(v: &Value) -> Res<&str> {
    v.as_str().ok_or_else(|| format!("not a string: {v}"))
}

/// `namespace:path` with the default namespace.
pub(crate) fn qualify(id: &str) -> String {
    if id.contains(':') {
        id.to_owned()
    } else {
        format!("minecraft:{id}")
    }
}

/// What the predicates read of the world.
pub trait World {
    /// The dimension key.
    fn dimension(&self) -> &str;
    /// The block at `pos`; `None` where the chunk is not loaded.
    fn block(&self, pos: BlockPos) -> Option<BlockState>;
    /// The biome at `pos` (`Level.getBiome`).
    fn biome(&self, pos: BlockPos) -> Option<String>;
    /// Whether `id` of `registry` is in `#tag`.
    fn in_tag(&self, registry: &str, tag: &str, id: &str) -> bool;
}

/// An active effect as `MobEffectsPredicate` sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EffectView {
    pub effect: crate::effect::MobEffect,
    pub amplifier: i32,
    pub duration: i32,
    pub ambient: bool,
    pub visible: bool,
}

/// The player part of an entity view (`PlayerPredicate`).
#[derive(Clone, Copy, Debug)]
pub struct PlayerFacts<'a> {
    pub game_mode: GameMode,
    pub level: i32,
    /// The statistics; `None` in snapshots of other players (stat conditions fail).
    pub stats: Option<&'a StatsCounter>,
}

/// An entity as the entity predicates see it. With `'static` it is a snapshot that events
/// carry (`EntityPredicate.createContext` at the time of the event).
#[derive(Clone, Debug)]
pub struct EntityView<'a> {
    /// The type id, e.g. `minecraft:player`.
    pub kind: &'static str,
    pub pos: Vec3,
    pub on_ground: bool,
    pub on_fire: bool,
    pub crouching: bool,
    pub sprinting: bool,
    pub swimming: bool,
    pub flying: bool,
    pub fall_flying: bool,
    pub in_water: bool,
    pub baby: bool,
    /// Head, chest, legs, feet, body, main hand, off hand.
    pub equipment: [Cow<'a, ItemStack>; 7],
    /// The team name.
    pub team: Option<String>,
    /// Game mode, level and statistics of a player.
    pub player: Option<PlayerFacts<'a>>,
    /// The active effects of a living entity.
    pub effects: Vec<EffectView>,
    /// The vehicle (with its passengers).
    pub vehicle: Option<Box<EntityView<'a>>>,
    /// The passengers (without their own vehicle or passengers).
    pub passengers: Vec<EntityView<'a>>,
    /// Exact entity components the predicates can test, e.g. `minecraft:cat/variant`.
    pub components: Vec<(&'static str, String)>,
    /// `LightningBolt.getBlocksSetOnFire` of a lightning bolt.
    pub lightning_fires: Option<i32>,
}

impl<'a> EntityView<'a> {
    /// A view of `kind` at `pos` with nothing else set.
    pub fn new(kind: &'static str, pos: Vec3) -> EntityView<'a> {
        EntityView {
            kind,
            pos,
            on_ground: false,
            on_fire: false,
            crouching: false,
            sprinting: false,
            swimming: false,
            flying: false,
            fall_flying: false,
            in_water: false,
            baby: false,
            equipment: std::array::from_fn(|_| Cow::Owned(ItemStack::EMPTY)),
            team: None,
            player: None,
            effects: Vec::new(),
            vehicle: None,
            passengers: Vec::new(),
            components: Vec::new(),
            lightning_fires: None,
        }
    }
}

/// `MinMaxBounds`: a number or `{min, max}`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Bounds {
    pub min: Option<f64>,
    pub max: Option<f64>,
}

impl Bounds {
    /// Parses `n` or `{"min": a, "max": b}`.
    pub fn parse(v: &Value) -> Res<Bounds> {
        if let Some(n) = v.as_f64() {
            return Ok(Bounds {
                min: Some(n),
                max: Some(n),
            });
        }
        let o = object(v)?;
        let num = |k: &str| {
            o.get(k)
                .map(|n| n.as_f64().ok_or_else(|| format!("{k} is not a number")))
                .transpose()
        };
        Ok(Bounds {
            min: num("min")?,
            max: num("max")?,
        })
    }

    fn field(o: &Map<String, Value>, key: &str) -> Res<Bounds> {
        o.get(key)
            .map(Bounds::parse)
            .transpose()
            .map(Option::unwrap_or_default)
    }

    /// `matches`.
    pub fn matches(self, v: f64) -> bool {
        self.min.is_none_or(|m| m <= v) && self.max.is_none_or(|m| v <= m)
    }

    /// `matchesSqr`: against the squared bounds.
    pub fn matches_sqr(self, v: f64) -> bool {
        self.min.is_none_or(|m| m * m <= v) && self.max.is_none_or(|m| v <= m * m)
    }
}

/// A `HolderSet`: ids or a `#tag`.
#[derive(Clone, Debug, PartialEq)]
pub enum IdSet {
    Ids(Vec<String>),
    Tag(String),
}

impl IdSet {
    /// Parses `"id"`, `["a", "b"]` or `"#tag"`.
    pub fn parse(v: &Value) -> Res<IdSet> {
        if let Some(s) = v.as_str() {
            return Ok(match s.strip_prefix('#') {
                Some(tag) => IdSet::Tag(qualify(tag)),
                None => IdSet::Ids(vec![qualify(s)]),
            });
        }
        let list = v.as_array().ok_or_else(|| format!("not an id list: {v}"))?;
        list.iter()
            .map(|e| string(e).map(qualify))
            .collect::<Res<Vec<_>>>()
            .map(IdSet::Ids)
    }

    /// Whether `id` of `registry` is in the set.
    pub fn contains(&self, registry: &str, id: &str, world: &dyn World) -> bool {
        match self {
            IdSet::Ids(ids) => ids.iter().any(|i| i == id),
            IdSet::Tag(tag) => world.in_tag(registry, tag, id),
        }
    }
}

/// `EnchantmentPredicate` of the `minecraft:enchantments` item sub-predicate.
#[derive(Clone, Debug, PartialEq)]
struct EnchantmentMatcher {
    enchantments: Option<IdSet>,
    levels: Bounds,
}

/// `ItemPredicate`.
#[derive(Clone, Debug, PartialEq)]
pub struct ItemPredicate {
    /// Item ids, or a tag.
    items: Option<Result<Vec<u16>, String>>,
    count: Bounds,
    /// The exact `minecraft:damage` component.
    damage: Option<i32>,
    /// The `minecraft:enchantments` sub-predicate: every entry must match an enchantment.
    enchantments: Vec<EnchantmentMatcher>,
    /// Other component matchers are not evaluated: the predicate never matches.
    supported: bool,
}

impl ItemPredicate {
    /// Parses an item predicate.
    pub fn parse(v: &Value) -> Res<ItemPredicate> {
        let o = object(v)?;
        let items = match o.get("items").map(IdSet::parse).transpose()? {
            None => None,
            Some(IdSet::Tag(tag)) => Some(Err(tag)),
            Some(IdSet::Ids(ids)) => Some(Ok(ids
                .iter()
                .map(|n| {
                    registry::ITEM
                        .id(n)
                        .ok_or_else(|| format!("unknown item {n}"))
                })
                .collect::<Res<Vec<_>>>()?)),
        };
        let mut supported = true;
        let mut damage = None;
        for (k, v) in o
            .get("components")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            match (
                qualify(k).as_str(),
                v.as_i64().and_then(|n| i32::try_from(n).ok()),
            ) {
                ("minecraft:damage", Some(n)) => damage = Some(n),
                _ => supported = false,
            }
        }
        let mut enchantments = Vec::new();
        for (k, v) in o
            .get("predicates")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            match (qualify(k).as_str(), v) {
                ("minecraft:enchantments", Value::Array(list)) => {
                    for e in list {
                        let e = object(e)?;
                        enchantments.push(EnchantmentMatcher {
                            enchantments: e.get("enchantments").map(IdSet::parse).transpose()?,
                            levels: Bounds::field(e, "levels")?,
                        });
                    }
                }
                _ => supported = false,
            }
        }
        Ok(ItemPredicate {
            items,
            count: Bounds::field(o, "count")?,
            damage,
            enchantments,
            supported,
        })
    }

    /// `test`.
    pub fn test(&self, stack: &ItemStack, world: &dyn World) -> bool {
        let item = if stack.is_empty() { 0 } else { stack.item };
        let items_ok = match &self.items {
            None => true,
            Some(Ok(ids)) => ids.contains(&item),
            Some(Err(tag)) => registry::ITEM
                .name(item)
                .is_some_and(|n| world.in_tag("minecraft:item", tag, n)),
        };
        let count = if stack.is_empty() { 0 } else { stack.count };
        self.supported
            && items_ok
            && self.count.matches(f64::from(count))
            && self.damage.is_none_or(|d| {
                crate::item::max_damage(stack) > 0 && crate::item::damage(stack) == d
            })
            && (self.enchantments.is_empty() || self.enchantments_match(stack, world))
    }

    /// `EnchantmentsPredicate.matches`: each entry finds an enchantment of the stack.
    fn enchantments_match(&self, stack: &ItemStack, world: &dyn World) -> bool {
        let present = crate::item::enchantments(stack);
        let data = crate::player::play::game_data();
        let name = |id: u32| {
            data.as_ref().and_then(|d| {
                d.enchantments
                    .iter()
                    .find(|(_, v)| **v == id)
                    .map(|(n, _)| n.clone())
            })
        };
        self.enchantments.iter().all(|m| {
            present.iter().any(|&(id, level)| {
                m.levels.matches(f64::from(level))
                    && m.enchantments.as_ref().is_none_or(|set| {
                        name(id).is_some_and(|n| set.contains("minecraft:enchantment", &n, world))
                    })
            })
        })
    }
}

/// `MobEffectsPredicate`: every listed effect is active and matches.
#[derive(Clone, Debug, PartialEq)]
pub struct MobEffectsPredicate(Vec<(crate::effect::MobEffect, [Bounds; 2], [Option<bool>; 2])>);

impl MobEffectsPredicate {
    /// Parses `{"minecraft:speed": {"amplifier": .., "duration": .., "ambient": .., "visible": ..}}`.
    pub fn parse(v: &Value) -> Res<MobEffectsPredicate> {
        let mut out = Vec::new();
        for (name, p) in object(v)? {
            let effect = crate::effect::MobEffect::by_name(&qualify(name))
                .ok_or_else(|| format!("unknown effect {name}"))?;
            let p = object(p)?;
            out.push((
                effect,
                [
                    Bounds::field(p, "amplifier")?,
                    Bounds::field(p, "duration")?,
                ],
                [
                    p.get("ambient").and_then(Value::as_bool),
                    p.get("visible").and_then(Value::as_bool),
                ],
            ));
        }
        Ok(MobEffectsPredicate(out))
    }

    /// `matches(activeEffects)`.
    pub fn matches(&self, effects: &[EffectView]) -> bool {
        self.0
            .iter()
            .all(|(effect, [amp, dur], [ambient, visible])| {
                effects
                    .iter()
                    .find(|e| e.effect == *effect)
                    .is_some_and(|e| {
                        amp.matches(f64::from(e.amplifier))
                            && dur.matches(f64::from(e.duration))
                            && ambient.is_none_or(|a| a == e.ambient)
                            && visible.is_none_or(|v| v == e.visible)
                    })
            })
    }
}

/// A damage source as `DamageSourcePredicate` sees it.
#[derive(Clone, Debug)]
pub struct DamageView {
    /// The damage type id.
    pub kind: String,
    /// The built-in damage type tag bits (`survival::damage::tag`).
    pub tags: u32,
    /// `getDirectEntity`.
    pub direct: Option<Box<EntityView<'static>>>,
    /// `getEntity`: the causing entity.
    pub cause: Option<Box<EntityView<'static>>>,
    /// `isDirect`: the direct entity is the causing entity.
    pub is_direct: bool,
    /// `getSourcePosition`.
    pub position: Option<Vec3>,
}

/// `DamageSourcePredicate`.
#[derive(Clone, Debug, PartialEq)]
pub struct DamageSourcePredicate {
    tags: Vec<(String, bool)>,
    direct: Option<EntityPredicate>,
    cause: Option<EntityPredicate>,
    is_direct: Option<bool>,
}

impl DamageSourcePredicate {
    /// Parses a damage source predicate.
    pub fn parse(v: &Value) -> Res<DamageSourcePredicate> {
        let o = object(v)?;
        let mut tags = Vec::new();
        for t in o
            .get("tags")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let t = object(t)?;
            let id = string(t.get("id").ok_or("tag without id")?)?;
            let expected = t
                .get("expected")
                .and_then(Value::as_bool)
                .ok_or("tag without expected")?;
            tags.push((qualify(id.trim_start_matches('#')), expected));
        }
        Ok(DamageSourcePredicate {
            tags,
            direct: o
                .get("direct_entity")
                .map(EntityPredicate::parse)
                .transpose()?,
            cause: o
                .get("source_entity")
                .map(EntityPredicate::parse)
                .transpose()?,
            is_direct: o.get("is_direct").and_then(Value::as_bool),
        })
    }

    /// `matches(level, position, source)`.
    pub fn matches(&self, source: &DamageView, world: &dyn World, origin: Vec3) -> bool {
        let entity = |p: &Option<EntityPredicate>, e: &Option<Box<EntityView<'static>>>| {
            p.as_ref().is_none_or(|p| {
                e.as_ref()
                    .is_some_and(|e| p.matches(e, world, Some(origin)))
            })
        };
        self.tags.iter().all(|(tag, expected)| {
            let has = match crate::survival::damage::tag::NAMES
                .iter()
                .find(|(n, _)| *n == tag.as_str())
            {
                Some(&(_, bit)) => source.tags & bit != 0,
                None => world.in_tag("minecraft:damage_type", tag, &source.kind),
            };
            has == *expected
        }) && entity(&self.direct, &source.direct)
            && entity(&self.cause, &source.cause)
            && self.is_direct.is_none_or(|d| d == source.is_direct)
    }
}

/// `DamagePredicate`.
#[derive(Clone, Debug, PartialEq)]
pub struct DamagePredicate {
    dealt: Bounds,
    taken: Bounds,
    source_entity: Option<EntityPredicate>,
    blocked: Option<bool>,
    kind: Option<DamageSourcePredicate>,
}

/// A damage event: the source, the original and the actual damage, and whether it was
/// blocked.
#[derive(Clone, Debug)]
pub struct DamageEvent {
    pub source: DamageView,
    pub dealt: f32,
    pub taken: f32,
    pub blocked: bool,
}

impl DamagePredicate {
    /// Parses a damage predicate.
    pub fn parse(v: &Value) -> Res<DamagePredicate> {
        let o = object(v)?;
        Ok(DamagePredicate {
            dealt: Bounds::field(o, "dealt")?,
            taken: Bounds::field(o, "taken")?,
            source_entity: o
                .get("source_entity")
                .map(EntityPredicate::parse)
                .transpose()?,
            blocked: o.get("blocked").and_then(Value::as_bool),
            kind: o
                .get("type")
                .map(DamageSourcePredicate::parse)
                .transpose()?,
        })
    }

    /// `matches(player, source, originalDamage, actualDamage, blocked)`.
    pub fn matches(&self, d: &DamageEvent, world: &dyn World, origin: Vec3) -> bool {
        self.dealt.matches(f64::from(d.dealt))
            && self.taken.matches(f64::from(d.taken))
            && self.source_entity.as_ref().is_none_or(|p| {
                d.source
                    .cause
                    .as_ref()
                    .is_some_and(|e| p.matches(e, world, Some(origin)))
            })
            && self.blocked.is_none_or(|b| b == d.blocked)
            && self
                .kind
                .as_ref()
                .is_none_or(|k| k.matches(&d.source, world, origin))
    }
}

/// A property value matcher of `StatePropertiesPredicate`.
#[derive(Clone, Debug, PartialEq)]
enum ValueMatcher {
    Exact(String),
    Range(Option<String>, Option<String>),
}

/// `StatePropertiesPredicate`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StateMatcher(Vec<(String, ValueMatcher)>);

impl StateMatcher {
    /// Parses `{"name": "value" | {"min": .., "max": ..}}`.
    pub fn parse(v: &Value) -> Res<StateMatcher> {
        let text = |v: &Value| -> Res<String> {
            Ok(match v {
                Value::String(s) => s.clone(),
                Value::Bool(b) => b.to_string(),
                Value::Number(n) => n.to_string(),
                other => return Err(format!("bad state value {other}")),
            })
        };
        let mut out = Vec::new();
        for (name, value) in object(v)? {
            let matcher = match value {
                Value::Object(range) => ValueMatcher::Range(
                    range.get("min").map(text).transpose()?,
                    range.get("max").map(text).transpose()?,
                ),
                other => ValueMatcher::Exact(text(other)?),
            };
            out.push((name.clone(), matcher));
        }
        Ok(StateMatcher(out))
    }

    /// `matches(BlockState)`.
    pub fn matches(&self, state: BlockState) -> bool {
        use crate::loot::{property_key, property_values};
        let block = state.block();
        self.0.iter().all(|(name, matcher)| {
            let (Some(values), Some(actual)) = (property_values(block, name), state.value(name))
            else {
                return false;
            };
            let Some(actual) = property_key(values, actual) else {
                return false;
            };
            match matcher {
                ValueMatcher::Exact(expected) => property_key(values, expected) == Some(actual),
                ValueMatcher::Range(min, max) => {
                    min.as_ref()
                        .is_none_or(|m| property_key(values, m).is_some_and(|m| actual >= m))
                        && max
                            .as_ref()
                            .is_none_or(|m| property_key(values, m).is_some_and(|m| actual <= m))
                }
            }
        })
    }
}

/// `BlockPredicate`.
#[derive(Clone, Debug, PartialEq)]
pub struct BlockPredicate {
    blocks: Option<IdSet>,
    state: Option<StateMatcher>,
    /// Block entity data is not evaluated: the predicate never matches.
    supported: bool,
}

impl BlockPredicate {
    /// Parses the fields of a block predicate.
    pub fn parse(o: &Map<String, Value>) -> Res<BlockPredicate> {
        let non_empty = |k: &str| {
            o.get(k)
                .and_then(Value::as_object)
                .is_some_and(|m| !m.is_empty())
        };
        Ok(BlockPredicate {
            blocks: o.get("blocks").map(IdSet::parse).transpose()?,
            state: o.get("state").map(StateMatcher::parse).transpose()?,
            supported: !o.contains_key("nbt")
                && !non_empty("components")
                && !non_empty("predicates"),
        })
    }

    /// `matchesState`.
    pub fn matches_state(&self, state: BlockState, world: &dyn World) -> bool {
        self.supported
            && self
                .blocks
                .as_ref()
                .is_none_or(|b| b.contains("minecraft:block", state.block().name(), world))
            && self.state.as_ref().is_none_or(|s| s.matches(state))
    }

    /// `matches(level, pos)`: the block must be loaded.
    pub fn matches(&self, world: &dyn World, pos: BlockPos) -> bool {
        world
            .block(pos)
            .is_some_and(|state| self.matches_state(state, world))
    }
}

/// `LocationPredicate`.
#[derive(Clone, Debug, PartialEq)]
pub struct LocationPredicate {
    position: [Bounds; 3],
    biomes: Option<IdSet>,
    dimension: Option<String>,
    block: Option<BlockPredicate>,
    /// Structures, smoke, light, fluids and the sky are not evaluated.
    supported: bool,
}

impl LocationPredicate {
    /// Parses a location predicate.
    pub fn parse(v: &Value) -> Res<LocationPredicate> {
        let o = object(v)?;
        let position = match o.get("position") {
            Some(p) => {
                let p = object(p)?;
                [
                    Bounds::field(p, "x")?,
                    Bounds::field(p, "y")?,
                    Bounds::field(p, "z")?,
                ]
            }
            None => [Bounds::default(); 3],
        };
        Ok(LocationPredicate {
            position,
            biomes: o.get("biomes").map(IdSet::parse).transpose()?,
            dimension: o.get("dimension").map(string).transpose()?.map(qualify),
            block: o
                .get("block")
                .map(|b| object(b).and_then(BlockPredicate::parse))
                .transpose()?,
            supported: ["structures", "smokey", "light", "fluid", "can_see_sky"]
                .iter()
                .all(|k| !o.contains_key(*k)),
        })
    }

    /// `matches(level, x, y, z)`.
    pub fn matches(&self, world: &dyn World, at: Vec3) -> bool {
        if !self.supported
            || !(self.position[0].matches(at.x)
                && self.position[1].matches(at.y)
                && self.position[2].matches(at.z))
        {
            return false;
        }
        if self
            .dimension
            .as_ref()
            .is_some_and(|d| d != world.dimension())
        {
            return false;
        }
        let pos = crate::player::play::block_pos_of(at);
        let loaded = world.block(pos).is_some();
        if let Some(biomes) = &self.biomes {
            let inside = loaded
                && world
                    .biome(pos)
                    .is_some_and(|b| biomes.contains("minecraft:worldgen/biome", &b, world));
            if !inside {
                return false;
            }
        }
        self.block.as_ref().is_none_or(|b| b.matches(world, pos))
    }
}

/// `DistancePredicate`.
#[derive(Clone, Debug, PartialEq)]
pub struct DistancePredicate {
    x: Bounds,
    y: Bounds,
    z: Bounds,
    horizontal: Bounds,
    absolute: Bounds,
}

impl DistancePredicate {
    pub fn parse(v: &Value) -> Res<DistancePredicate> {
        let o = object(v)?;
        Ok(DistancePredicate {
            x: Bounds::field(o, "x")?,
            y: Bounds::field(o, "y")?,
            z: Bounds::field(o, "z")?,
            horizontal: Bounds::field(o, "horizontal")?,
            absolute: Bounds::field(o, "absolute")?,
        })
    }

    /// `matches(x0, y0, z0, x1, y1, z1)` with float differences.
    #[allow(clippy::cast_possible_truncation, clippy::as_conversions)]
    pub fn matches(&self, a: Vec3, b: Vec3) -> bool {
        let (dx, dy, dz) = ((a.x - b.x) as f32, (a.y - b.y) as f32, (a.z - b.z) as f32);
        self.x.matches(f64::from(dx.abs()))
            && self.y.matches(f64::from(dy.abs()))
            && self.z.matches(f64::from(dz.abs()))
            && self.horizontal.matches_sqr(f64::from(dx * dx + dz * dz))
            && self
                .absolute
                .matches_sqr(f64::from(dx * dx + dy * dy + dz * dz))
    }
}

/// One part of an `EntityPredicate`.
#[derive(Clone, Debug, PartialEq)]
enum EntityPart {
    Type(IdSet),
    Location(LocationPredicate),
    SteppingOn(LocationPredicate),
    Distance(DistancePredicate),
    Flags(Vec<(String, bool)>),
    /// Slot index into [`EntityView::equipment`] and the predicate.
    Equipment(Vec<(usize, ItemPredicate)>),
    Team(String),
    Player {
        game_modes: Option<Vec<GameMode>>,
        level: Bounds,
        stats: Vec<(Stat, Bounds)>,
    },
    Vehicle(Box<EntityPredicate>),
    Passenger(Box<EntityPredicate>),
    Effects(MobEffectsPredicate),
    /// Exact entity components (variants) by name and expected value.
    Components(Vec<(String, String)>),
    /// `type_specific/lightning`: blocks set on fire (`entity_struck` is not evaluated).
    Lightning(Bounds),
    Unsupported,
}

/// `EntityPredicate`: all parts must match.
#[derive(Clone, Debug, PartialEq)]
pub struct EntityPredicate(Vec<EntityPart>);

const EQUIPMENT: [&str; 7] = [
    "head", "chest", "legs", "feet", "body", "mainhand", "offhand",
];

fn game_mode(name: &str) -> Option<GameMode> {
    Some(match name {
        "survival" => GameMode::Survival,
        "creative" => GameMode::Creative,
        "adventure" => GameMode::Adventure,
        "spectator" => GameMode::Spectator,
        _ => return None,
    })
}

impl EntityPredicate {
    /// Parses the map of entity sub-predicates.
    pub fn parse(v: &Value) -> Res<EntityPredicate> {
        let mut parts = Vec::new();
        for (key, value) in object(v)? {
            parts.push(match qualify(key).as_str() {
                "minecraft:entity_type" => EntityPart::Type(IdSet::parse(value)?),
                "minecraft:location" => EntityPart::Location(LocationPredicate::parse(value)?),
                "minecraft:stepping_on" => EntityPart::SteppingOn(LocationPredicate::parse(value)?),
                "minecraft:distance" => EntityPart::Distance(DistancePredicate::parse(value)?),
                "minecraft:flags" => EntityPart::Flags(
                    object(value)?
                        .iter()
                        .map(|(k, v)| {
                            v.as_bool()
                                .map(|b| (k.clone(), b))
                                .ok_or_else(|| format!("flag {k} is not a boolean"))
                        })
                        .collect::<Res<_>>()?,
                ),
                "minecraft:equipment" => {
                    let o = object(value)?;
                    let mut slots = Vec::new();
                    for (slot, name) in EQUIPMENT.iter().enumerate() {
                        if let Some(p) = o.get(*name) {
                            slots.push((slot, ItemPredicate::parse(p)?));
                        }
                    }
                    EntityPart::Equipment(slots)
                }
                "minecraft:team" => EntityPart::Team(string(value)?.to_owned()),
                "minecraft:type_specific/player" => Self::player(object(value)?)?,
                "minecraft:vehicle" => {
                    EntityPart::Vehicle(Box::new(EntityPredicate::parse(value)?))
                }
                "minecraft:passenger" => {
                    EntityPart::Passenger(Box::new(EntityPredicate::parse(value)?))
                }
                "minecraft:effects" => EntityPart::Effects(MobEffectsPredicate::parse(value)?),
                "minecraft:components" => {
                    let mut out = Vec::new();
                    let mut supported = true;
                    for (k, v) in object(value)? {
                        match v.as_str() {
                            Some(s) => out.push((qualify(k), qualify(s))),
                            None => supported = false,
                        }
                    }
                    if supported {
                        EntityPart::Components(out)
                    } else {
                        EntityPart::Unsupported
                    }
                }
                "minecraft:type_specific/lightning" => {
                    let o = object(value)?;
                    if o.contains_key("entity_struck") {
                        EntityPart::Unsupported
                    } else {
                        EntityPart::Lightning(Bounds::field(o, "blocks_set_on_fire")?)
                    }
                }
                _ => EntityPart::Unsupported,
            });
        }
        Ok(EntityPredicate(parts))
    }

    /// `PlayerPredicate`: game mode, level and statistics.
    fn player(o: &Map<String, Value>) -> Res<EntityPart> {
        let supported = ["food", "recipes", "advancements", "looking_at", "input"]
            .iter()
            .all(|k| !o.contains_key(*k));
        if !supported {
            return Ok(EntityPart::Unsupported);
        }
        let game_modes = match o.get("gamemode") {
            None => None,
            Some(Value::String(s)) => Some(vec![game_mode(s).ok_or("unknown game mode")?]),
            Some(Value::Array(list)) => Some(
                list.iter()
                    .map(|m| {
                        string(m)
                            .ok()
                            .and_then(game_mode)
                            .ok_or("unknown game mode")
                    })
                    .collect::<Result<_, _>>()?,
            ),
            Some(other) => return Err(format!("bad gamemode {other}")),
        };
        let mut stats = Vec::new();
        for s in o
            .get("stats")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let s = object(s)?;
            let kind = StatType::by_name(&qualify(string(s.get("type").ok_or("stat type")?)?))
                .ok_or("unknown stat type")?;
            let name = qualify(string(s.get("stat").ok_or("stat")?)?);
            let value = kind.registry().id(&name).ok_or("unknown stat")?;
            stats.push((Stat { kind, value }, Bounds::field(s, "value")?));
        }
        Ok(EntityPart::Player {
            game_modes,
            level: Bounds::field(o, "level")?,
            stats,
        })
    }

    /// `matches(entity, level, position)`; `origin` is the position of the context.
    pub fn matches(&self, e: &EntityView<'_>, world: &dyn World, origin: Option<Vec3>) -> bool {
        self.0.iter().all(|part| match part {
            EntityPart::Type(types) => types.contains("minecraft:entity_type", e.kind, world),
            EntityPart::Location(l) => l.matches(world, e.pos),
            EntityPart::SteppingOn(l) => {
                let below =
                    crate::player::play::block_pos_of(Vec3::new(e.pos.x, e.pos.y - 0.2, e.pos.z));
                let center = Vec3::new(
                    f64::from(below.x) + 0.5,
                    f64::from(below.y) + 0.5,
                    f64::from(below.z) + 0.5,
                );
                l.matches(world, center)
            }
            EntityPart::Distance(d) => origin.is_some_and(|o| d.matches(o, e.pos)),
            EntityPart::Flags(flags) => flags.iter().all(|(name, want)| {
                let have = match name.as_str() {
                    "is_on_ground" => e.on_ground,
                    "is_on_fire" => e.on_fire,
                    "is_sneaking" => e.crouching,
                    "is_sprinting" => e.sprinting,
                    "is_swimming" => e.swimming,
                    "is_flying" => e.flying || e.fall_flying,
                    "is_fall_flying" => e.fall_flying,
                    "is_in_water" => e.in_water,
                    "is_baby" => e.baby,
                    _ => return false,
                };
                have == *want
            }),
            EntityPart::Equipment(slots) => slots.iter().all(|(slot, p)| {
                e.equipment
                    .get(*slot)
                    .is_some_and(|stack| p.test(stack, world))
            }),
            EntityPart::Team(team) => e.team.as_deref() == Some(team.as_str()),
            EntityPart::Player {
                game_modes,
                level,
                stats,
            } => e.player.is_some_and(|p| {
                game_modes.as_ref().is_none_or(|m| m.contains(&p.game_mode))
                    && level.matches(f64::from(p.level))
                    && (stats.is_empty()
                        || p.stats.is_some_and(|counter| {
                            stats
                                .iter()
                                .all(|(stat, range)| range.matches(f64::from(counter.get(*stat))))
                        }))
            }),
            EntityPart::Vehicle(p) => e
                .vehicle
                .as_ref()
                .is_some_and(|v| p.matches(v, world, origin)),
            EntityPart::Passenger(p) => e.passengers.iter().any(|v| p.matches(v, world, origin)),
            EntityPart::Effects(p) => p.matches(&e.effects),
            EntityPart::Components(expected) => expected.iter().all(|(name, value)| {
                e.components
                    .iter()
                    .any(|(n, v)| *n == name.as_str() && v == value)
            }),
            EntityPart::Lightning(fires) => e
                .lightning_fires
                .is_some_and(|n| fires.matches(f64::from(n))),
            EntityPart::Unsupported => false,
        })
    }
}

/// What a loot condition is tested against (`LootContext`).
#[derive(Clone, Copy)]
pub struct Context<'a> {
    pub world: &'a dyn World,
    /// `this`.
    pub this: Option<&'a EntityView<'a>>,
    /// `origin`.
    pub origin: Option<Vec3>,
    /// `block_state`.
    pub block_state: Option<BlockState>,
    /// `tool`.
    pub tool: Option<&'a ItemStack>,
}

impl std::fmt::Debug for Context<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Context")
            .field("this", &self.this)
            .field("origin", &self.origin)
            .finish_non_exhaustive()
    }
}

/// A loot condition (`LootItemCondition`).
#[derive(Clone, Debug, PartialEq)]
pub enum Condition {
    /// `entity_properties` of `this`; without a predicate it needs the entity only.
    EntityProperties(Option<EntityPredicate>),
    /// `location_check`: the predicate at the origin moved by the offset.
    LocationCheck(Option<LocationPredicate>, [i32; 3]),
    /// `match_block` on the block state.
    MatchBlock(BlockPredicate),
    /// `match_tool`.
    MatchTool(Option<ItemPredicate>),
    AllOf(Vec<Condition>),
    AnyOf(Vec<Condition>),
    Inverted(Box<Condition>),
    /// Named predicates, other entity targets and other condition types: never true.
    Unsupported,
}

impl Condition {
    /// Parses a condition holder: an inline condition (named predicates are unsupported).
    pub fn parse(v: &Value) -> Res<Condition> {
        if v.is_string() {
            return Ok(Condition::Unsupported);
        }
        let o = object(v)?;
        let kind = qualify(string(o.get("type").ok_or("condition without type")?)?);
        let list = |key: &str| -> Res<Vec<Condition>> {
            match o.get(key) {
                Some(Value::Array(items)) => items.iter().map(Condition::parse).collect(),
                _ => Err(format!("{key} is not a list")),
            }
        };
        Ok(match kind.as_str() {
            "minecraft:entity_properties" => {
                if o.get("entity").and_then(Value::as_str) != Some("this") {
                    return Ok(Condition::Unsupported);
                }
                Condition::EntityProperties(
                    o.get("predicate").map(EntityPredicate::parse).transpose()?,
                )
            }
            "minecraft:location_check" => {
                let offset = |k: &str| {
                    o.get(k)
                        .map_or(Some(0), |v| v.as_i64().and_then(|v| i32::try_from(v).ok()))
                        .ok_or_else(|| format!("{k} is not an int"))
                };
                Condition::LocationCheck(
                    o.get("predicate")
                        .map(LocationPredicate::parse)
                        .transpose()?,
                    [offset("offsetX")?, offset("offsetY")?, offset("offsetZ")?],
                )
            }
            "minecraft:match_block" => Condition::MatchBlock(BlockPredicate::parse(o)?),
            "minecraft:match_tool" => {
                Condition::MatchTool(o.get("predicate").map(ItemPredicate::parse).transpose()?)
            }
            "minecraft:all_of" => Condition::AllOf(list("terms")?),
            "minecraft:any_of" => Condition::AnyOf(list("terms")?),
            "minecraft:inverted" => {
                Condition::Inverted(Box::new(Condition::parse(o.get("term").ok_or("no term")?)?))
            }
            _ => Condition::Unsupported,
        })
    }

    /// `test(context)`: conditions that need a missing parameter are false.
    pub fn test(&self, ctx: &Context<'_>) -> bool {
        match self {
            Condition::EntityProperties(p) => ctx.this.is_some_and(|e| {
                p.as_ref()
                    .is_none_or(|p| p.matches(e, ctx.world, ctx.origin))
            }),
            Condition::LocationCheck(p, [dx, dy, dz]) => ctx.origin.is_some_and(|o| {
                p.as_ref().is_none_or(|p| {
                    let at = Vec3::new(
                        o.x + f64::from(*dx),
                        o.y + f64::from(*dy),
                        o.z + f64::from(*dz),
                    );
                    p.matches(ctx.world, at)
                })
            }),
            Condition::MatchBlock(p) => ctx
                .block_state
                .is_some_and(|s| p.matches_state(s, ctx.world)),
            Condition::MatchTool(p) => ctx
                .tool
                .is_some_and(|t| p.as_ref().is_none_or(|p| p.test(t, ctx.world))),
            Condition::AllOf(terms) => terms.iter().all(|t| t.test(ctx)),
            Condition::AnyOf(terms) => terms.iter().any(|t| t.test(ctx)),
            Condition::Inverted(term) => !term.test(ctx),
            Condition::Unsupported => false,
        }
    }
}

/// `blocks`/`state` of `enter_block`.
pub fn block_matches(
    blocks: Option<&IdSet>,
    state_matcher: Option<&StateMatcher>,
    state: BlockState,
    world: &dyn World,
) -> bool {
    let block: BlockId = state.block();
    blocks.is_none_or(|b| b.contains("minecraft:block", block.name(), world))
        && state_matcher.is_none_or(|s| s.matches(state))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Flat;

    impl World for Flat {
        fn dimension(&self) -> &str {
            "minecraft:overworld"
        }
        fn block(&self, pos: BlockPos) -> Option<BlockState> {
            (pos.y < 0).then(|| BlockId::by_name("minecraft:stone").unwrap().default_state())
        }
        fn biome(&self, _pos: BlockPos) -> Option<String> {
            Some("minecraft:plains".to_owned())
        }
        fn in_tag(&self, _registry: &str, tag: &str, id: &str) -> bool {
            tag == "minecraft:base_stone_overworld" && id == "minecraft:stone"
        }
    }

    #[test]
    fn location_conditions() {
        let c = Condition::parse(&json!({
            "type": "minecraft:location_check",
            "offsetY": -1,
            "predicate": {"block": {"blocks": "#minecraft:base_stone_overworld"}, "biomes": "minecraft:plains"}
        }))
        .unwrap();
        let ctx = |y: f64| Context {
            world: &Flat,
            this: None,
            origin: Some(Vec3::new(0.5, y, 0.5)),
            block_state: None,
            tool: None,
        };
        assert!(c.test(&ctx(0.5)));
        assert!(!c.test(&ctx(1.5)));
        let structures = Condition::parse(&json!({
            "type": "minecraft:location_check",
            "predicate": {"structures": "minecraft:fortress"}
        }))
        .unwrap();
        assert!(!structures.test(&ctx(-3.0)));
        let inverted = Condition::parse(&json!({"type": "minecraft:inverted", "term": {"type": "minecraft:random_chance", "chance": 1.0}})).unwrap();
        assert!(inverted.test(&ctx(0.0)));
    }

    #[test]
    fn item_predicates() {
        let stack = ItemStack::new(registry::ITEM.id("minecraft:crafting_table").unwrap(), 2);
        let p = ItemPredicate::parse(
            &json!({"items": "minecraft:crafting_table", "count": {"min": 2}}),
        )
        .unwrap();
        assert!(p.test(&stack, &Flat));
        let p = ItemPredicate::parse(&json!({"items": ["minecraft:stone"]})).unwrap();
        assert!(!p.test(&stack, &Flat));
        let p = ItemPredicate::parse(&json!({"components": {"minecraft:damage": 0}})).unwrap();
        assert!(!p.test(&stack, &Flat));
        assert!(ItemPredicate::parse(&json!({"items": "minecraft:nope"})).is_err());
    }
}

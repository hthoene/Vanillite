//! Parity scenarios: a world, a region to observe, a setup and actions per tick.
//!
//! A scenario is a JSON file in `crates/vanillite-test/scenarios/`; its file name is its name.
//! The Java recorder (`ScenarioRecorder.java`) reads the same file.
//!
//! ```json
//! {
//!   "description": "What the scenario shows.",
//!   "tier": "fast",
//!   "profile": "vanilla",
//!   "world": {"seed": 0, "biome": "minecraft:plains",
//!             "layers": [["minecraft:bedrock", 1], ["minecraft:dirt", 2], ["minecraft:grass_block", 1]]},
//!   "rules": {"random_tick_speed": "0"},
//!   "region": [[0, 0, 0], [5, 3, 5]],
//!   "ticks": 4,
//!   "light": false,
//!   "setup": [{"op": "fill", "from": [0, 0, 0], "to": [5, 0, 5], "block": "minecraft:stone"}],
//!   "actions": [{"tick": 1, "op": "place", "pos": [3, 1, 3], "block": "minecraft:sand"}]
//! }
//! ```
//!
//! - `tier`: `fast` (golden in the repository, runs on every push) or `bulk` (golden in the
//!   bulk store, runs nightly). Default `fast`.
//! - `profile`: the configuration profile. Only `vanilla` can be recorded on Vanilla; any other
//!   profile needs a blessed golden (`cargo xtask golden bless`). Default `vanilla`.
//! - `world`: a flat world without structures or features; layers bottom up from the world's
//!   minimum y. Defaults: seed 0, plains, the classic layers above.
//! - `rules`: game rules on top of the defaults every scenario has (`random_tick_speed 0`,
//!   `spawn_mobs false`, `advance_time false`, `advance_weather false`).
//! - `region`: the observed box (inclusive). Its chunks and their neighbours are loaded and
//!   ticking before the setup.
//! - `light`: frames also hold the settled sky and block light of the region, the levels a
//!   client sees. Default `false`.
//! - `setup`: applied without updates before frame 0 (`set`, `fill`; `place` with updates).
//! - `actions`: before world tick `tick` (1-based): `place` (with neighbour and shape updates,
//!   Vanilla `setBlock` flags 3), `set` (without updates), `fill` (with updates).
//!
//! Block states are written `name[key=value,...]` like in commands.
//!
//! Entities (mob AI and path finding parity):
//!
//! ```json
//! {
//!   "time": 18000, "difficulty": "normal", "entity_seed": 7,
//!   "entities": [
//!     {"name": "steve", "type": "minecraft:player", "pos": [8.5, -60, 2.5]},
//!     {"name": "zombie", "type": "minecraft:zombie", "pos": [2.5, -60, 2.5], "rot": [0, 0],
//!      "seed": 1}
//!   ],
//!   "actions": [{"tick": 5, "op": "hurt", "entity": "zombie", "by": "steve", "amount": 1}]
//! }
//! ```
//!
//! - `time`: the day time set before the entities come (`/time set`); none = the world's start.
//! - `difficulty`: `peaceful`, `easy`, `normal` (default) or `hard`.
//! - `entities`: after the setup, players first (survival, on a connection nobody reads), then
//!   the level random is reseeded with the world seed, then the other entities in order, each
//!   like `/summon` makes it: created with its random seeded with `seed`, placed at `pos` with
//!   `rot` (yaw, pitch; head and body face the yaw), `finalizeSpawn` (unless `"finalize":
//!   false`), made a baby if `baby` says so. The `name` identifies the entity in the trace.
//! - `entity_seed`: every entity created while the scenario runs (arrows, drops, orbs, babies,
//!   spawned mobs) seeds its random with the next `nextLong()` of a legacy random seeded with
//!   it, in creation order (default: the world seed). The Java recorder enforces this in
//!   Vanilla with its agent (`ParityAgent.java`).
//!
//! Known divergences from Vanilla live in `scenarios/divergences.toml`, not in the scenario
//! (so they change without a new recording):
//!
//! ```toml
//! [mob_zombie_chase]
//! tick = 1
//! cause = "the zombie lacks the villager, iron golem and turtle target goals"
//! owner = "gameplay-developer #58"
//! ```
//!
//! The check passes while the first difference is at exactly that tick and fails when it
//! moves: earlier is a regression; later is progress (raise the tick, or remove the entry once
//! the scenario matches Vanilla).
//! - `hurt` action: `amount` damage to the entity `entity`, by `by` (player attack or mob
//!   attack) or generic damage without `by`.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;
use vanillite_base::BlockPos;

use crate::golden;

/// Where a scenario's golden lives and when it runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    /// Golden in the repository; every push.
    Fast,
    /// Golden in the bulk store; nightly.
    Bulk,
}

/// The flat world a scenario runs in.
#[derive(Clone, Debug)]
pub struct WorldSpec {
    pub seed: i64,
    pub biome: String,
    /// Layers bottom up: block name and thickness.
    pub layers: Vec<(String, u32)>,
    /// Game rules on top of the scenario defaults.
    pub rules: BTreeMap<String, String>,
    /// Configuration profile (`vanilla` unless the scenario says otherwise).
    pub profile: String,
    /// Day time set before the entities come.
    pub time: Option<i64>,
    /// The observed region (inclusive): its chunks and their neighbours are forced.
    pub region: (BlockPos, BlockPos),
    /// Whether the world computes light: with entities (mobs see it) or `"light": true`.
    /// Block-only scenarios run without it, which is much faster.
    pub light: bool,
    /// Whether frames observe light (`"light": true`).
    pub observe_light: bool,
    /// `peaceful`, `easy`, `normal` or `hard`.
    pub difficulty: String,
}

/// What an operation does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpKind {
    /// One block without updates.
    Set,
    /// One block with neighbour and shape updates.
    Place,
    /// A box: without updates in the setup, with updates as an action.
    Fill,
    /// Damage to a scenario entity ([`Op::hurt`]).
    Hurt,
}

/// The damage of a `hurt` action.
#[derive(Clone, Debug, PartialEq)]
pub struct Hurt {
    /// Name of the entity hurt.
    pub entity: String,
    /// Name of the attacker, if any.
    pub by: Option<String>,
    pub amount: f32,
}

/// A known divergence from Vanilla (`scenarios/divergences.toml`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Divergence {
    /// The tick of the first difference.
    pub tick: u32,
    /// Why it differs.
    pub cause: String,
    /// Who fixes it.
    pub owner: String,
}

/// A scenario entity.
#[derive(Clone, Debug, PartialEq)]
pub struct EntitySpec {
    /// Name in the trace.
    pub name: String,
    /// Entity type, e.g. `minecraft:zombie`.
    pub kind: String,
    pub pos: [f64; 3],
    /// Yaw and pitch.
    pub rot: [f32; 2],
    /// Seed of the entity's random.
    pub seed: i64,
    /// Made a baby (or an adult) after spawning, if set.
    pub baby: Option<bool>,
    /// `finalizeSpawn` like `/summon`.
    pub finalize: bool,
}

impl EntitySpec {
    /// Whether the entity is a player.
    pub fn is_player(&self) -> bool {
        self.kind == "minecraft:player"
    }

    /// The fixed UUID of the scenario entity with index `index` (`ScenarioEntities.uuid`).
    pub fn uuid(&self, index: usize) -> u128 {
        let most = 0x5641_4E49_0000_0000_u64 | (index as u64 + 1);
        (u128::from(most) << 64) | u128::from(self.seed as u64)
    }
}

/// A setup step or an action.
#[derive(Clone, Debug)]
pub struct Op {
    /// Tick before which an action runs (0 for setup steps).
    pub tick: u32,
    pub kind: OpKind,
    pub from: BlockPos,
    pub to: BlockPos,
    /// Block state, `name[key=value,...]` (empty for `hurt`).
    pub block: String,
    /// The damage of a `hurt` action.
    pub hurt: Option<Hurt>,
}

/// A parsed scenario.
#[derive(Clone, Debug)]
pub struct Scenario {
    pub name: String,
    /// SHA-1 of the scenario file: the scenario part of its golden key.
    pub sha1: String,
    pub description: String,
    pub tier: Tier,
    pub world: WorldSpec,
    pub min: BlockPos,
    pub max: BlockPos,
    pub ticks: u32,
    pub setup: Vec<Op>,
    pub actions: Vec<Op>,
    pub entities: Vec<EntitySpec>,
    /// Seed of the stream new entities take their seeds from.
    pub entity_seed: i64,
    /// A known divergence from Vanilla.
    pub diverges: Option<Divergence>,
}

impl Scenario {
    /// Directory of the scenario files.
    pub fn dir() -> std::path::PathBuf {
        golden::crate_dir().join("scenarios")
    }

    /// Every scenario, sorted by name.
    pub fn all() -> Result<Vec<Self>, String> {
        let dir = Self::dir();
        let mut paths: Vec<_> = std::fs::read_dir(&dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .collect();
        paths.sort();
        paths.iter().map(|p| Self::load(p)).collect()
    }

    /// Reads and parses a scenario file.
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| format!("{}: bad file name", path.display()))?;
        Self::parse(name, &text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Parses a scenario from its JSON text.
    pub fn parse(name: &str, text: &str) -> Result<Self, String> {
        let v: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let tier = match v.get("tier").and_then(Value::as_str).unwrap_or("fast") {
            "fast" => Tier::Fast,
            "bulk" => Tier::Bulk,
            t => return Err(format!("unknown tier `{t}`")),
        };
        let world = v.get("world");
        let layers = match world.and_then(|w| w.get("layers")) {
            Some(l) => l
                .as_array()
                .ok_or("world.layers must be an array")?
                .iter()
                .map(|l| {
                    let block = l.get(0).and_then(Value::as_str);
                    let height = l.get(1).and_then(Value::as_u64);
                    match (block, height) {
                        (Some(b), Some(h)) => Ok((b.to_owned(), u32::try_from(h).unwrap_or(0))),
                        _ => Err("a layer is [block, height]".to_owned()),
                    }
                })
                .collect::<Result<_, _>>()?,
            None => vec![
                ("minecraft:bedrock".to_owned(), 1),
                ("minecraft:dirt".to_owned(), 2),
                ("minecraft:grass_block".to_owned(), 1),
            ],
        };
        let rules = v
            .get("rules")
            .and_then(Value::as_object)
            .map(|r| {
                r.iter()
                    .map(|(k, v)| {
                        let v = v.as_str().map_or_else(|| v.to_string(), str::to_owned);
                        (k.clone(), v)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let region = v.get("region").ok_or("region is missing")?;
        let ops = |key: &str, actions: bool| -> Result<Vec<Op>, String> {
            v.get(key)
                .and_then(Value::as_array)
                .map(|a| a.iter().map(|o| op(o, actions)).collect())
                .unwrap_or(Ok(Vec::new()))
        };
        Ok(Self {
            name: name.to_owned(),
            sha1: golden::text_sha1(text),
            description: v
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            tier,
            world: WorldSpec {
                seed: world
                    .and_then(|w| w.get("seed"))
                    .and_then(Value::as_i64)
                    .unwrap_or(0),
                biome: world
                    .and_then(|w| w.get("biome"))
                    .and_then(Value::as_str)
                    .unwrap_or("minecraft:plains")
                    .to_owned(),
                layers,
                rules,
                profile: v
                    .get("profile")
                    .and_then(Value::as_str)
                    .unwrap_or("vanilla")
                    .to_owned(),
                time: v.get("time").and_then(Value::as_i64),
                region: (pos(region.get(0))?, pos(region.get(1))?),
                light: v.get("entities").is_some()
                    || v.get("light").and_then(Value::as_bool).unwrap_or(false),
                observe_light: v.get("light").and_then(Value::as_bool).unwrap_or(false),
                difficulty: v
                    .get("difficulty")
                    .and_then(Value::as_str)
                    .unwrap_or("normal")
                    .to_owned(),
            },
            min: pos(region.get(0))?,
            max: pos(region.get(1))?,
            ticks: v
                .get("ticks")
                .and_then(Value::as_u64)
                .and_then(|t| u32::try_from(t).ok())
                .ok_or("ticks is missing")?,
            setup: ops("setup", false)?,
            actions: ops("actions", true)?,
            entities: v
                .get("entities")
                .and_then(Value::as_array)
                .map(|a| a.iter().map(entity).collect())
                .unwrap_or(Ok(Vec::new()))?,
            diverges: divergence(name)?,
            entity_seed: v
                .get("entity_seed")
                .and_then(Value::as_i64)
                .or_else(|| world.and_then(|w| w.get("seed")).and_then(Value::as_i64))
                .unwrap_or(0),
        })
    }
}

fn pos(v: Option<&Value>) -> Result<BlockPos, String> {
    let c = |i: usize| {
        v.and_then(|v| v.get(i))
            .and_then(Value::as_i64)
            .and_then(|c| i32::try_from(c).ok())
            .ok_or_else(|| format!("bad position {v:?}"))
    };
    Ok(BlockPos::new(c(0)?, c(1)?, c(2)?))
}

/// The known divergence of a scenario from `scenarios/divergences.toml`.
fn divergence(name: &str) -> Result<Option<Divergence>, String> {
    let path = Scenario::dir().join("divergences.toml");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let table: toml::Table = text
        .parse()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let Some(entry) = table.get(name) else {
        return Ok(None);
    };
    let field = |k: &str| {
        entry
            .get(k)
            .and_then(toml::Value::as_str)
            .unwrap_or_default()
    };
    Ok(Some(Divergence {
        tick: entry
            .get("tick")
            .and_then(toml::Value::as_integer)
            .and_then(|t| u32::try_from(t).ok())
            .ok_or_else(|| format!("{}: [{name}] needs a tick", path.display()))?,
        cause: field("cause").to_owned(),
        owner: field("owner").to_owned(),
    }))
}

fn vec3(v: Option<&Value>) -> Result<[f64; 3], String> {
    let c = |i: usize| {
        v.and_then(|v| v.get(i))
            .and_then(Value::as_f64)
            .ok_or_else(|| format!("bad position {v:?}"))
    };
    Ok([c(0)?, c(1)?, c(2)?])
}

fn entity(v: &Value) -> Result<EntitySpec, String> {
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
    let kind = s("type").ok_or("an entity needs a type")?;
    let seed = v.get("seed").and_then(Value::as_i64);
    let rot = |i: usize| {
        v.get("rot")
            .and_then(|r| r.get(i))
            .and_then(Value::as_f64)
            .unwrap_or(0.0) as f32
    };
    Ok(EntitySpec {
        name: s("name").ok_or("an entity needs a name")?,
        pos: vec3(v.get("pos"))?,
        rot: [rot(0), rot(1)],
        seed: match seed {
            Some(s) => s,
            None if kind == "minecraft:player" => 0,
            None => return Err(format!("entity {kind} needs a seed")),
        },
        baby: v.get("baby").and_then(Value::as_bool),
        finalize: v.get("finalize").and_then(Value::as_bool).unwrap_or(true),
        kind,
    })
}

fn op(v: &Value, action: bool) -> Result<Op, String> {
    let kind = match v.get("op").and_then(Value::as_str) {
        Some("set") => OpKind::Set,
        Some("place") => OpKind::Place,
        Some("fill") => OpKind::Fill,
        Some("hurt") if action => OpKind::Hurt,
        o => return Err(format!("unknown op {o:?}")),
    };
    let tick = if action {
        v.get("tick")
            .and_then(Value::as_u64)
            .and_then(|t| u32::try_from(t).ok())
            .filter(|&t| t >= 1)
            .ok_or("an action needs a tick >= 1")?
    } else {
        0
    };
    if kind == OpKind::Hurt {
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
        return Ok(Op {
            tick,
            kind,
            from: BlockPos::new(0, 0, 0),
            to: BlockPos::new(0, 0, 0),
            block: String::new(),
            hurt: Some(Hurt {
                entity: s("entity").ok_or("hurt needs an entity")?,
                by: s("by"),
                amount: v
                    .get("amount")
                    .and_then(Value::as_f64)
                    .ok_or("hurt needs an amount")? as f32,
            }),
        });
    }
    let (from, to) = if kind == OpKind::Fill {
        (pos(v.get("from"))?, pos(v.get("to"))?)
    } else {
        let p = pos(v.get("pos"))?;
        (p, p)
    };
    Ok(Op {
        tick,
        kind,
        from,
        to,
        block: v
            .get("block")
            .and_then(Value::as_str)
            .ok_or("an op needs a block")?
            .to_owned(),
        hurt: None,
    })
}

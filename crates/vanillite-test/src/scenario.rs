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
//! - `region`: the observed box (inclusive). Its chunks and their neighbours are forced
//!   (`/forceload`) and ticking before the setup.
//! - `setup`: applied without updates before frame 0 (`set`, `fill`; `place` with updates).
//! - `actions`: before world tick `tick` (1-based): `place` (with neighbour and shape updates,
//!   Vanilla `setBlock` flags 3), `set` (without updates), `fill` (with updates), `force` and
//!   `unforce` (`"chunks": [[x, z], ...]`: adds or removes forced tickets like `/forceload`,
//!   then the chunk system runs without a game tick until every chunk is loaded, unloaded and
//!   has its entities like its new ticket level says).
//!
//! Positions in chunks that are not accessible (unloaded or above full level) are traced as
//! the state [`UNLOADED`].
//!
//! Block states are written `name[key=value,...]` like in commands.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;
use vanillite_base::{BlockPos, ChunkPos};

/// The traced state of a position in a chunk that is not accessible.
pub const UNLOADED: &str = "<unloaded>";

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
    /// Chunks forced before the setup: the region's chunks and one chunk around them.
    pub forced: Vec<ChunkPos>,
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
    /// Forces chunks (an action).
    Force,
    /// Removes the forced tickets of chunks (an action).
    Unforce,
}

/// A setup step or an action.
#[derive(Clone, Debug)]
pub struct Op {
    /// Tick before which an action runs (0 for setup steps).
    pub tick: u32,
    pub kind: OpKind,
    pub from: BlockPos,
    pub to: BlockPos,
    /// Block state, `name[key=value,...]`; empty for chunk ops.
    pub block: String,
    /// The chunks of `force` and `unforce`.
    pub chunks: Vec<ChunkPos>,
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
        let (min, max) = (pos(region.get(0))?, pos(region.get(1))?);
        let mut forced = Vec::new();
        for x in (min.x >> 4) - 1..=(max.x >> 4) + 1 {
            for z in (min.z >> 4) - 1..=(max.z >> 4) + 1 {
                forced.push(ChunkPos::new(x, z));
            }
        }
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
                forced,
            },
            min,
            max,
            ticks: v
                .get("ticks")
                .and_then(Value::as_u64)
                .and_then(|t| u32::try_from(t).ok())
                .ok_or("ticks is missing")?,
            setup: ops("setup", false)?,
            actions: ops("actions", true)?,
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

fn op(v: &Value, action: bool) -> Result<Op, String> {
    let kind = match v.get("op").and_then(Value::as_str) {
        Some("set") => OpKind::Set,
        Some("place") => OpKind::Place,
        Some("fill") => OpKind::Fill,
        Some("force") if action => OpKind::Force,
        Some("unforce") if action => OpKind::Unforce,
        o => return Err(format!("unknown op {o:?}")),
    };
    if matches!(kind, OpKind::Force | OpKind::Unforce) {
        let chunks = v
            .get("chunks")
            .and_then(Value::as_array)
            .ok_or("a chunk op needs chunks")?
            .iter()
            .map(|c| {
                let c = |i: usize| {
                    c.get(i)
                        .and_then(Value::as_i64)
                        .and_then(|v| i32::try_from(v).ok())
                        .ok_or_else(|| format!("bad chunk {c:?}"))
                };
                Ok(ChunkPos::new(c(0)?, c(1)?))
            })
            .collect::<Result<_, String>>()?;
        let tick = v
            .get("tick")
            .and_then(Value::as_u64)
            .and_then(|t| u32::try_from(t).ok())
            .filter(|&t| t >= 1)
            .ok_or("an action needs a tick >= 1")?;
        return Ok(Op {
            tick,
            kind,
            from: BlockPos::new(0, 0, 0),
            to: BlockPos::new(0, 0, 0),
            block: String::new(),
            chunks,
        });
    }
    let (from, to) = if kind == OpKind::Fill {
        (pos(v.get("from"))?, pos(v.get("to"))?)
    } else {
        let p = pos(v.get("pos"))?;
        (p, p)
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
        chunks: Vec::new(),
    })
}

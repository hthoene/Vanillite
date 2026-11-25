//! The trace format: what a scenario looked like after every tick.
//!
//! Written by the Java recorder for Vanilla and by [`Trace::to_json`] for blessed goldens; one
//! JSON line per frame so diffs stay readable:
//!
//! ```text
//! {"format":1,
//! "region":[[x0,y0,z0],[x1,y1,z1]],
//! "palette":["minecraft:stone", "minecraft:oak_log[axis=x]", ...],
//! "frames":[
//! {"tick":0,"blocks":[count,palette,...],"block_ticks":[...],"fluid_ticks":[...],"entities":[...]},
//! {"tick":1,"changes":[[x,y,z,palette],...],"block_ticks":[...],"fluid_ticks":[...],"entities":[...]}]}
//! ```
//!
//! - Frame 0 holds every block of the region as run-length pairs in y, z, x order (x fastest);
//!   later frames hold the blocks that changed.
//! - Block states are `name[key=value,...]` with properties sorted by name.
//! - `level_rng` (scenarios with entities): the state of the level random after the tick.
//! - Ticks: `[x, y, z, block or fluid, delay, priority]` in Vanilla's drain order; the delay is
//!   relative to the game time of the frame.
//! - Entities: `[type, x, y, z, dx, dy, dz]` or `[type, x, y, z, dx, dy, dz, {detail}]`,
//!   doubles as IEEE bit patterns (signed 64-bit), so the comparison is exact. Their order is
//!   not significant. The detail object (living entities and items):
//!   - `name`: the scenario name of the entity;
//!   - `item`, `count`: the stack of an item entity;
//!   - `rot`: yaw, pitch, head yaw, body yaw; `hp`: health (floats as signed 32-bit bit
//!     patterns); `death`: the death animation tick once dying;
//!   - mobs: `rng` (the state of the entity's random: the first tick it differs is the first
//!     diverging random call), `target` (name or type of the attack target), `goals` and `targets` (the running
//!     goals of the goal and target selector by Vanilla class name, sorted), `path` (the
//!     navigation path while not done: `to` target block, `next` node index, `reach`, `nodes`
//!     as `[x, y, z, path type]`).

use std::collections::HashMap;
use std::fmt::Write as _;

use serde_json::{Map, Value};
use vanillite_base::BlockPos;

/// Format version the recorder writes.
pub const FORMAT: u64 = 1;

/// A pending block or fluid tick.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TickObs {
    pub pos: BlockPos,
    /// Block or fluid name.
    pub kind: String,
    /// Trigger tick minus the current game time.
    pub delay: i64,
    /// `TickPriority` value (-3 extremely high … 3 extremely low).
    pub priority: i32,
}

/// An entity in the region.
#[derive(Clone, Debug, PartialEq)]
pub struct EntityObs {
    /// Entity type name.
    pub kind: String,
    pub pos: [f64; 3],
    pub motion: [f64; 3],
    /// The detail object of living entities and items (see the module documentation).
    pub detail: Option<Map<String, Value>>,
}

impl EntityObs {
    /// Exact sort and comparison key.
    fn key(&self) -> (&str, [u64; 6], String) {
        let p = self.pos.map(f64::to_bits);
        let m = self.motion.map(f64::to_bits);
        let detail = self
            .detail
            .as_ref()
            .map(|d| canonical(&Value::Object(d.clone())))
            .unwrap_or_default();
        (&self.kind, [p[0], p[1], p[2], m[0], m[1], m[2]], detail)
    }

    /// The scenario name of the entity, if it has one.
    pub fn name(&self) -> Option<&str> {
        self.detail.as_ref()?.get("name")?.as_str()
    }

    /// A float of the detail written as bit pattern (`hp`) or an element of an array of them
    /// (`rot`).
    pub fn float_bits(v: &Value) -> Option<f32> {
        v.as_i64()
            .and_then(|b| i32::try_from(b).ok())
            .map(|b| f32::from_bits(b as u32))
    }

    /// The differences of `actual` from this (expected) observation of the same entity.
    fn differences(&self, actual: &Self) -> Vec<String> {
        let mut out = Vec::new();
        for (what, e, a) in [
            ("pos", self.pos, actual.pos),
            ("motion", self.motion, actual.motion),
        ] {
            if e.map(f64::to_bits) != a.map(f64::to_bits) {
                out.push(format!("{what} {a:?}, expected {e:?}"));
            }
        }
        let empty = Map::new();
        let (ed, ad) = (
            self.detail.as_ref().unwrap_or(&empty),
            actual.detail.as_ref().unwrap_or(&empty),
        );
        let mut keys: Vec<&String> = ed.keys().chain(ad.keys()).collect();
        keys.sort();
        keys.dedup();
        for k in keys {
            let (e, a) = (ed.get(k), ad.get(k));
            if e == a {
                continue;
            }
            let show = |v: Option<&Value>| match (k.as_str(), v) {
                (_, None) => "none".to_owned(),
                ("hp", Some(v)) => format!("{:?}", Self::float_bits(v)),
                ("rot", Some(Value::Array(a))) => {
                    format!("{:?}", a.iter().map(Self::float_bits).collect::<Vec<_>>())
                }
                (_, Some(v)) => v.to_string(),
            };
            out.push(format!("{k} {}, expected {}", show(a), show(e)));
        }
        out
    }
}

/// The state of the region after one tick.
#[derive(Clone, Debug, Default)]
pub struct Frame {
    pub tick: u32,
    /// Palette index per block of the region, y, z, x order.
    pub blocks: Vec<u32>,
    pub block_ticks: Vec<TickObs>,
    pub fluid_ticks: Vec<TickObs>,
    pub entities: Vec<EntityObs>,
    /// State of the level random after the tick (scenarios with entities).
    pub level_rng: Option<i64>,
}

/// A recorded scenario run.
#[derive(Clone, Debug)]
pub struct Trace {
    pub min: BlockPos,
    pub max: BlockPos,
    pub palette: Vec<String>,
    index: HashMap<String, u32>,
    pub frames: Vec<Frame>,
    /// For blessed traces: SHA-1 of the scenario they were blessed for.
    pub blessed_for: Option<String>,
}

impl Trace {
    /// An empty trace of a region.
    pub fn new(min: BlockPos, max: BlockPos) -> Self {
        Self {
            min,
            max,
            palette: Vec::new(),
            index: HashMap::new(),
            frames: Vec::new(),
            blessed_for: None,
        }
    }

    /// Size of the region in x, y and z.
    pub fn size(&self) -> [usize; 3] {
        [
            (self.max.x - self.min.x + 1).max(0) as usize,
            (self.max.y - self.min.y + 1).max(0) as usize,
            (self.max.z - self.min.z + 1).max(0) as usize,
        ]
    }

    /// Position of the block with index `i` (y, z, x order).
    pub fn pos_of(&self, i: usize) -> BlockPos {
        let [sx, _, sz] = self.size();
        BlockPos::new(
            self.min.x + (i % sx) as i32,
            self.min.y + (i / (sx * sz)) as i32,
            self.min.z + (i / sx % sz) as i32,
        )
    }

    /// Palette index of a block state name, adding it if new.
    pub fn intern(&mut self, state: &str) -> u32 {
        if let Some(&i) = self.index.get(state) {
            return i;
        }
        let i = self.palette.len() as u32;
        self.palette.push(state.to_owned());
        self.index.insert(state.to_owned(), i);
        i
    }

    /// Parses a trace written by the recorder or by [`Trace::to_json`].
    pub fn parse(text: &str) -> Result<Self, String> {
        let v: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        if v.get("format").and_then(Value::as_u64) != Some(FORMAT) {
            return Err(format!("trace format is not {FORMAT}"));
        }
        let region = v.get("region").ok_or("region is missing")?;
        let mut trace = Self::new(pos(&region[0])?, pos(&region[1])?);
        trace.blessed_for = v
            .get("blessed_for")
            .and_then(Value::as_str)
            .map(str::to_owned);
        for s in v
            .get("palette")
            .and_then(Value::as_array)
            .ok_or("palette")?
        {
            trace.intern(s.as_str().ok_or("palette entries are strings")?);
        }
        let volume: usize = trace.size().iter().product();
        let mut last: Vec<u32> = Vec::new();
        for f in v.get("frames").and_then(Value::as_array).ok_or("frames")? {
            let tick = f
                .get("tick")
                .and_then(Value::as_u64)
                .and_then(|t| u32::try_from(t).ok())
                .ok_or("frame tick")?;
            let blocks = if let Some(runs) = f.get("blocks").and_then(Value::as_array) {
                let mut blocks = Vec::with_capacity(volume);
                for pair in runs.chunks(2) {
                    let count = pair[0].as_u64().ok_or("run count")? as usize;
                    let p = pair
                        .get(1)
                        .and_then(Value::as_u64)
                        .ok_or("run palette index")? as u32;
                    blocks.extend(std::iter::repeat_n(p, count));
                }
                blocks
            } else {
                let mut blocks = last.clone();
                for c in f
                    .get("changes")
                    .and_then(Value::as_array)
                    .ok_or("changes")?
                {
                    let p = pos(c)?;
                    let i = trace.index_of(p).ok_or("change outside the region")?;
                    let state = c.get(3).and_then(Value::as_u64).ok_or("change state")?;
                    *blocks.get_mut(i).ok_or("changes before a full frame")? = state as u32;
                }
                blocks
            };
            if blocks.len() != volume || blocks.iter().any(|&b| b as usize >= trace.palette.len()) {
                return Err(format!("frame {tick}: blocks do not match the region"));
            }
            last.clone_from(&blocks);
            trace.frames.push(Frame {
                tick,
                blocks,
                block_ticks: ticks(f.get("block_ticks"))?,
                fluid_ticks: ticks(f.get("fluid_ticks"))?,
                entities: entities(f.get("entities"))?,
                level_rng: f.get("level_rng").and_then(Value::as_i64),
            });
        }
        Ok(trace)
    }

    /// Index of a position in the block list, if it is inside the region.
    pub fn index_of(&self, p: BlockPos) -> Option<usize> {
        let [sx, sy, sz] = self.size();
        let (x, y, z) = (p.x - self.min.x, p.y - self.min.y, p.z - self.min.z);
        if x < 0 || y < 0 || z < 0 || x as usize >= sx || y as usize >= sy || z as usize >= sz {
            return None;
        }
        Some((y as usize * sz + z as usize) * sx + x as usize)
    }

    /// The trace in the recorder's format.
    pub fn to_json(&self) -> String {
        let mut s = format!("{{\"format\":{FORMAT},");
        if let Some(b) = &self.blessed_for {
            let _ = write!(s, "\n\"blessed_for\":\"{b}\",");
        }
        let (a, b) = (self.min, self.max);
        let _ = write!(
            s,
            "\n\"region\":[[{},{},{}],[{},{},{}]],\n\"palette\":[",
            a.x, a.y, a.z, b.x, b.y, b.z
        );
        for (i, p) in self.palette.iter().enumerate() {
            let _ = write!(s, "{}\n\"{p}\"", if i == 0 { "" } else { "," });
        }
        s.push_str("],\n\"frames\":[");
        let mut last: Option<&Vec<u32>> = None;
        for (fi, f) in self.frames.iter().enumerate() {
            let _ = write!(
                s,
                "{}\n{{\"tick\":{}",
                if fi == 0 { "" } else { "," },
                f.tick
            );
            match last {
                None => {
                    s.push_str(",\"blocks\":[");
                    let mut first = true;
                    let mut run = 0;
                    for (i, &b) in f.blocks.iter().enumerate() {
                        run += 1;
                        if f.blocks.get(i + 1) != Some(&b) {
                            let _ = write!(s, "{}{run},{b}", if first { "" } else { "," });
                            first = false;
                            run = 0;
                        }
                    }
                }
                Some(prev) => {
                    s.push_str(",\"changes\":[");
                    let mut first = true;
                    for (i, (&b, &p)) in f.blocks.iter().zip(prev).enumerate() {
                        if b != p {
                            let q = self.pos_of(i);
                            let sep = if first { "" } else { "," };
                            let _ = write!(s, "{sep}[{},{},{},{b}]", q.x, q.y, q.z);
                            first = false;
                        }
                    }
                }
            }
            last = Some(&f.blocks);
            s.push_str("],\"block_ticks\":");
            write_ticks(&mut s, &f.block_ticks);
            s.push_str(",\"fluid_ticks\":");
            write_ticks(&mut s, &f.fluid_ticks);
            s.push_str(",\"entities\":[");
            for (i, e) in f.entities.iter().enumerate() {
                let p = e.pos.map(|v| v.to_bits() as i64);
                let m = e.motion.map(|v| v.to_bits() as i64);
                let _ = write!(
                    s,
                    "{}[\"{}\",{},{},{},{},{},{}",
                    if i == 0 { "" } else { "," },
                    e.kind,
                    p[0],
                    p[1],
                    p[2],
                    m[0],
                    m[1],
                    m[2]
                );
                if let Some(d) = &e.detail {
                    let _ = write!(s, ",{}", Value::Object(d.clone()));
                }
                s.push(']');
            }
            s.push(']');
            if let Some(r) = f.level_rng {
                let _ = write!(s, ",\"level_rng\":{r}");
            }
            s.push('}');
        }
        s.push_str("]}\n");
        s
    }

    /// The tick of the first difference of `actual` from this (expected) trace, `None` if
    /// they are equal. A difference outside the frames (region, frame count) counts as tick 0.
    pub fn first_divergence(&self, actual: &Self) -> Option<u32> {
        let first = self.diff(actual, 1).into_iter().next()?;
        Some(
            first
                .strip_prefix("tick ")
                .and_then(|r| r.split(' ').next())
                .and_then(|t| t.parse().ok())
                .unwrap_or(0),
        )
    }

    /// Every difference of `actual` from this (expected) trace, at most `limit` lines, frame by
    /// frame. Blocks compare by state name, ticks in order, entities as a set; all exactly.
    pub fn diff(&self, actual: &Self, limit: usize) -> Vec<String> {
        let mut out = Vec::new();
        if (self.min, self.max) != (actual.min, actual.max) {
            out.push(format!(
                "region {:?}..{:?} != {:?}..{:?}",
                actual.min, actual.max, self.min, self.max
            ));
            return out;
        }
        if self.frames.len() != actual.frames.len() {
            out.push(format!(
                "{} frames, expected {}",
                actual.frames.len(),
                self.frames.len()
            ));
        }
        for (e, a) in self.frames.iter().zip(&actual.frames) {
            let t = e.tick;
            for (i, (&eb, &ab)) in e.blocks.iter().zip(&a.blocks).enumerate() {
                let (es, as_) = (&self.palette[eb as usize], &actual.palette[ab as usize]);
                if es != as_ {
                    let p = self.pos_of(i);
                    out.push(format!(
                        "tick {t} block {},{},{}: {as_}, expected {es}",
                        p.x, p.y, p.z
                    ));
                }
            }
            for (what, ev, av) in [
                ("block ticks", &e.block_ticks, &a.block_ticks),
                ("fluid ticks", &e.fluid_ticks, &a.fluid_ticks),
            ] {
                if ev != av {
                    out.push(format!("tick {t} {what}: {av:?}, expected {ev:?}"));
                }
            }
            let mut ee: Vec<_> = e.entities.iter().map(EntityObs::key).collect();
            let mut ae: Vec<_> = a.entities.iter().map(EntityObs::key).collect();
            ee.sort_unstable();
            ae.sort_unstable();
            if ee != ae {
                entity_diff(t, &e.entities, &a.entities, &mut out);
            }
            if e.level_rng != a.level_rng {
                out.push(format!(
                    "tick {t} level random state {:?}, expected {:?}",
                    a.level_rng, e.level_rng
                ));
            }
            if out.len() >= limit {
                break;
            }
        }
        out.truncate(limit);
        out
    }
}

/// JSON text with the object keys sorted, whatever order the map keeps.
fn canonical(v: &Value) -> String {
    match v {
        Value::Object(m) => {
            let mut kv: Vec<_> = m.iter().collect();
            kv.sort_by(|a, b| a.0.cmp(b.0));
            let inner: Vec<String> = kv
                .iter()
                .map(|(k, v)| format!("{k:?}:{}", canonical(v)))
                .collect();
            format!("{{{}}}", inner.join(","))
        }
        Value::Array(a) => format!(
            "[{}]",
            a.iter().map(canonical).collect::<Vec<_>>().join(",")
        ),
        v => v.to_string(),
    }
}

/// The differences of the entities of one frame: named entities field by field, the others
/// as the lists of what is only on one side.
fn entity_diff(t: u32, expected: &[EntityObs], actual: &[EntityObs], out: &mut Vec<String>) {
    let mut names: Vec<&str> = expected
        .iter()
        .chain(actual)
        .filter_map(EntityObs::name)
        .collect();
    names.sort_unstable();
    names.dedup();
    for name in names {
        let e = expected.iter().find(|o| o.name() == Some(name));
        let a = actual.iter().find(|o| o.name() == Some(name));
        match (e, a) {
            (Some(e), Some(a)) => {
                for d in e.differences(a) {
                    out.push(format!("tick {t} {name} ({}): {d}", e.kind));
                }
            }
            (Some(e), None) => out.push(format!("tick {t} {name} ({}): missing", e.kind)),
            (None, Some(a)) => out.push(format!("tick {t} {name} ({}): unexpected", a.kind)),
            (None, None) => {}
        }
    }
    let unnamed = |list: &[EntityObs]| -> Vec<EntityObs> {
        let mut v: Vec<EntityObs> = list
            .iter()
            .filter(|o| o.name().is_none())
            .cloned()
            .collect();
        v.sort_by(|a, b| a.key().cmp(&b.key()));
        v
    };
    let (ue, ua) = (unnamed(expected), unnamed(actual));
    let only = |of: &[EntityObs], other: &[EntityObs]| -> Vec<EntityObs> {
        of.iter()
            .filter(|o| !other.iter().any(|x| x.key() == o.key()))
            .cloned()
            .collect()
    };
    let (missing, extra) = (only(&ue, &ua), only(&ua, &ue));
    if missing.len() == 1 && extra.len() == 1 && missing[0].kind == extra[0].kind {
        for d in missing[0].differences(&extra[0]) {
            out.push(format!("tick {t} {}: {d}", missing[0].kind));
        }
        return;
    }
    if !missing.is_empty() {
        out.push(format!("tick {t} entities missing: {missing:?}"));
    }
    if !extra.is_empty() {
        out.push(format!("tick {t} entities unexpected: {extra:?}"));
    }
}

fn pos(v: &Value) -> Result<BlockPos, String> {
    let c = |i: usize| {
        v.get(i)
            .and_then(Value::as_i64)
            .and_then(|c| i32::try_from(c).ok())
            .ok_or_else(|| format!("bad position {v}"))
    };
    Ok(BlockPos::new(c(0)?, c(1)?, c(2)?))
}

fn ticks(v: Option<&Value>) -> Result<Vec<TickObs>, String> {
    let Some(a) = v.and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    a.iter()
        .map(|t| {
            Ok(TickObs {
                pos: pos(t)?,
                kind: t
                    .get(3)
                    .and_then(Value::as_str)
                    .ok_or("tick type")?
                    .to_owned(),
                delay: t.get(4).and_then(Value::as_i64).ok_or("tick delay")?,
                priority: t
                    .get(5)
                    .and_then(Value::as_i64)
                    .and_then(|p| i32::try_from(p).ok())
                    .ok_or("tick priority")?,
            })
        })
        .collect()
}

fn entities(v: Option<&Value>) -> Result<Vec<EntityObs>, String> {
    let Some(a) = v.and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    a.iter()
        .map(|e| {
            let bits = |i: usize| {
                e.get(i)
                    .and_then(Value::as_i64)
                    .map(|b| f64::from_bits(b as u64))
                    .ok_or_else(|| format!("bad entity {e}"))
            };
            Ok(EntityObs {
                kind: e
                    .get(0)
                    .and_then(Value::as_str)
                    .ok_or("entity type")?
                    .to_owned(),
                pos: [bits(1)?, bits(2)?, bits(3)?],
                motion: [bits(4)?, bits(5)?, bits(6)?],
                detail: e.get(7).and_then(Value::as_object).cloned(),
            })
        })
        .collect()
}

fn write_ticks(s: &mut String, ticks: &[TickObs]) {
    s.push('[');
    for (i, t) in ticks.iter().enumerate() {
        let _ = write!(
            s,
            "{}[{},{},{},\"{}\",{},{}]",
            if i == 0 { "" } else { "," },
            t.pos.x,
            t.pos.y,
            t.pos.z,
            t.kind,
            t.delay,
            t.priority
        );
    }
    s.push(']');
}

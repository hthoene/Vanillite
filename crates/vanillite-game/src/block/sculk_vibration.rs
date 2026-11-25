//! The parts of `VibrationSystem` shared by every vibration listener (sculk sensors and
//! shriekers, wardens): `VibrationSystem.Data` with its `VibrationSelector`, the validity and
//! occlusion checks of `VibrationSystem.Listener`, and the `listener` NBT.

use vanillite_base::{BlockPos, ChunkPos, Vec3};
use vanillite_config::GameMode;
use vanillite_nbt::{Compound, List, Tag};
use vanillite_world::entity::{EntityFlags, EntityId};
use vanillite_worldgen::behaviour::tags;

use crate::level::Level;
use crate::level::game_event::{GameEventContext, frequency};
use crate::level::signal::DIRECTIONS;

/// `VibrationInfo`: a vibration on its way.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vibration {
    /// The game event.
    pub event: u16,
    /// Distance from its origin to the listener.
    pub distance: f32,
    /// Where it started.
    pub pos: Vec3,
    /// The entity that caused it.
    pub source: Option<EntityId>,
    /// The owner of the projectile that caused it (`projectileOwnerUuid`).
    pub projectile_owner: Option<EntityId>,
}

/// `VibrationSystem.Data` with its `VibrationSelector`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct VibrationData {
    /// `currentVibration`.
    pub current: Option<Vibration>,
    /// `travelTimeInTicks`.
    pub travel_time: i32,
    /// `VibrationSelector.currentVibrationData`: the candidate and the game time it came.
    pub candidate: Option<(Vibration, i64)>,
}

/// Which `getListenableEvents()` tag a listener hears.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Listenable {
    /// `#vibrations` (sculk sensors).
    Vibrations,
    /// `#warden_can_listen`.
    Warden,
    /// `#shrieker_can_listen`.
    Shrieker,
    /// `#allay_can_listen`.
    Allay,
}

impl VibrationData {
    /// `VibrationSelector.addCandidate(vibration, tickTime)`: the nearest of a tick wins, ties
    /// go to the higher frequency.
    pub fn add_candidate(&mut self, vibration: Vibration, time: i64) {
        let replace = match &self.candidate {
            None => true,
            Some((previous, tick)) => {
                if time != *tick {
                    false
                } else if vibration.distance < previous.distance {
                    true
                } else if vibration.distance > previous.distance {
                    false
                } else {
                    frequency(vibration.event) > frequency(previous.event)
                }
            }
        };
        if replace {
            self.candidate = Some((vibration, time));
        }
    }

    /// `Ticker.trySelectAndScheduleVibration` with `calculateTravelTimeInTicks` = floor of the
    /// distance: a candidate of an earlier tick starts travelling. Returns whether it did.
    pub fn try_select(&mut self, time: i64) -> bool {
        if self.current.is_some() {
            return false;
        }
        match self.candidate {
            Some((vibration, tick)) if tick < time => {
                self.current = Some(vibration);
                self.travel_time = vibration.distance.floor() as i32;
                self.candidate = None;
                true
            }
            _ => false,
        }
    }

    /// The `listener` NBT (`VibrationSystem.Data.CODEC`); entity references become UUIDs.
    pub fn save(&self, uuid: impl Fn(EntityId) -> Option<u128>) -> Compound {
        let mut out = Compound::new();
        if let Some(v) = &self.current {
            out.insert("event", Tag::Compound(save_info(v, &uuid)));
        }
        let mut selector = Compound::new();
        if let Some((v, tick)) = &self.candidate {
            selector.insert("event", Tag::Compound(save_info(v, &uuid)));
            selector.insert("tick", Tag::Long(*tick));
        } else {
            selector.insert("tick", Tag::Long(-1));
        }
        out.insert("selector", Tag::Compound(selector));
        out.insert("event_delay", Tag::Int(self.travel_time.max(0)));
        out
    }

    /// Reads the `listener` NBT; `entity` resolves stored UUIDs (unknown ones are dropped).
    pub fn load(tag: &Compound, entity: impl Fn(u128) -> Option<EntityId>) -> VibrationData {
        let mut data = VibrationData::default();
        if let Some(Tag::Compound(e)) = tag.get("event") {
            data.current = load_info(e, &entity);
        }
        if let Some(Tag::Compound(s)) = tag.get("selector")
            && let Some(Tag::Compound(e)) = s.get("event")
            && let Some(v) = load_info(e, &entity)
        {
            let tick = match s.get("tick") {
                Some(Tag::Long(t)) => *t,
                _ => -1,
            };
            data.candidate = Some((v, tick));
        }
        if let Some(Tag::Int(d)) = tag.get("event_delay") {
            data.travel_time = (*d).max(0);
        }
        data
    }
}

fn uuid_ints(uuid: u128) -> Vec<i32> {
    (0..4)
        .map(|i| (uuid >> (96 - 32 * i)) as u32 as i32)
        .collect()
}

fn ints_uuid(v: &[i32]) -> Option<u128> {
    (v.len() == 4).then(|| {
        v.iter()
            .fold(0u128, |acc, &w| acc << 32 | u128::from(w as u32))
    })
}

fn save_info(v: &Vibration, uuid: &impl Fn(EntityId) -> Option<u128>) -> Compound {
    let mut c = Compound::new();
    if let Some(name) = vanillite_data::registry::GAME_EVENT.name(v.event) {
        c.insert("game_event", Tag::String(name.to_owned()));
    }
    c.insert("distance", Tag::Float(v.distance));
    c.insert(
        "pos",
        Tag::List(List(vec![
            Tag::Double(v.pos.x),
            Tag::Double(v.pos.y),
            Tag::Double(v.pos.z),
        ])),
    );
    if let Some(u) = v.source.and_then(uuid) {
        c.insert("source", Tag::IntArray(uuid_ints(u)));
    }
    if let Some(u) = v.projectile_owner.and_then(uuid) {
        c.insert("projectile_owner", Tag::IntArray(uuid_ints(u)));
    }
    c
}

fn load_info(c: &Compound, entity: &impl Fn(u128) -> Option<EntityId>) -> Option<Vibration> {
    let Some(Tag::String(name)) = c.get("game_event") else {
        return None;
    };
    let event = vanillite_data::registry::GAME_EVENT.id(name)?;
    let distance = match c.get("distance") {
        Some(Tag::Float(d)) if *d >= 0.0 => *d,
        _ => return None,
    };
    let pos = match c.get("pos") {
        Some(Tag::List(l)) if l.0.len() == 3 => match (&l.0[0], &l.0[1], &l.0[2]) {
            (Tag::Double(x), Tag::Double(y), Tag::Double(z)) => Vec3::new(*x, *y, *z),
            _ => return None,
        },
        _ => return None,
    };
    let uuid_of = |key: &str| match c.get(key) {
        Some(Tag::IntArray(v)) => ints_uuid(v).and_then(entity),
        _ => None,
    };
    Some(Vibration {
        event,
        distance,
        pos,
        source: uuid_of("source"),
        projectile_owner: uuid_of("projectile_owner"),
    })
}

/// The owner of a projectile entity (`VibrationInfo.getProjectileOwner`).
pub fn projectile_owner(level: &Level, entity: Option<EntityId>) -> Option<EntityId> {
    level.entities.arrow(entity?).and_then(|a| a.owner)
}

/// `VibrationSystem.User.isValidVibration` for a listener hearing `listenable`.
pub fn is_valid_vibration(
    level: &Level,
    event: u16,
    context: GameEventContext,
    listenable: Listenable,
) -> bool {
    let event_tags = &tags().game_events;
    let hears = match listenable {
        Listenable::Vibrations => event_tags.vibrations.contains(event),
        Listenable::Warden => event_tags.warden_can_listen.contains(event),
        Listenable::Shrieker => event_tags.shrieker_can_listen.contains(event),
        Listenable::Allay => event_tags.allay_can_listen.contains(event),
    };
    if !hears {
        return false;
    }
    if let Some(entity) = context.source {
        if level
            .entities
            .player(entity)
            .is_some_and(|p| p.game_mode == GameMode::Spectator)
        {
            return false;
        }
        let crouching = level
            .entities
            .store
            .get(entity)
            .is_some_and(|e| e.flags().contains(EntityFlags::CROUCHING));
        if crouching && event_tags.ignore_vibrations_sneaking.contains(event) {
            return false;
        }
        // `dampensVibrations`: wardens.
        if level
            .entities
            .store
            .get(entity)
            .is_some_and(|e| e.kind() == vanillite_data::entity::ids::WARDEN)
        {
            return false;
        }
    }
    context
        .affected
        .is_none_or(|state| !tags().blocks.dampens_vibrations.contains(state))
}

/// `Listener.isOccluded`: every line from the source block (nudged towards each face) to the
/// listener crosses a block that occludes vibrations.
pub fn is_occluded(level: &Level, origin: Vec3, destination: Vec3) -> bool {
    let centred = |v: Vec3| Vec3::new(v.x.floor() + 0.5, v.y.floor() + 0.5, v.z.floor() + 0.5);
    let from = centred(origin);
    let to = centred(destination);
    let nudge = f64::from(1.0e-5f32);
    for direction in DIRECTIONS {
        let [x, y, z] = direction.normal();
        let nudged = Vec3::new(
            from.x + f64::from(x) * nudge,
            from.y + f64::from(y) * nudge,
            from.z + f64::from(z) * nudge,
        );
        if !occluder_in_line(level, nudged, to) {
            return false;
        }
    }
    true
}

/// `isBlockInLine(new ClipBlockStateContext(from, to, OCCLUDES_VIBRATION_SIGNALS))` hits a
/// block (`BlockGetter.traverseBlocks`).
fn occluder_in_line(level: &Level, from: Vec3, to: Vec3) -> bool {
    if from == to {
        return false;
    }
    let occludes = |x: i32, y: i32, z: i32| {
        tags()
            .blocks
            .occludes_vibration_signals
            .contains(level.block(BlockPos::new(x, y, z)))
    };
    let lerp = |delta: f64, a: f64, b: f64| a + delta * (b - a);
    let (to_x, to_y, to_z) = (
        lerp(-1.0e-7, to.x, from.x),
        lerp(-1.0e-7, to.y, from.y),
        lerp(-1.0e-7, to.z, from.z),
    );
    let (from_x, from_y, from_z) = (
        lerp(-1.0e-7, from.x, to.x),
        lerp(-1.0e-7, from.y, to.y),
        lerp(-1.0e-7, from.z, to.z),
    );
    let (mut bx, mut by, mut bz) = (
        from_x.floor() as i32,
        from_y.floor() as i32,
        from_z.floor() as i32,
    );
    if occludes(bx, by, bz) {
        return true;
    }
    let (dx, dy, dz) = (to_x - from_x, to_y - from_y, to_z - from_z);
    let sign = |v: f64| {
        if v == 0.0 {
            0
        } else if v > 0.0 {
            1
        } else {
            -1
        }
    };
    let (sx, sy, sz) = (sign(dx), sign(dy), sign(dz));
    let delta = |s: i32, d: f64| {
        if s == 0 { f64::MAX } else { f64::from(s) / d }
    };
    let (tdx, tdy, tdz) = (delta(sx, dx), delta(sy, dy), delta(sz, dz));
    let frac = |v: f64| v - v.floor();
    let start = |s: i32, td: f64, v: f64| td * if s > 0 { 1.0 - frac(v) } else { frac(v) };
    let (mut tx, mut ty, mut tz) = (
        start(sx, tdx, from_x),
        start(sy, tdy, from_y),
        start(sz, tdz, from_z),
    );
    while tx <= 1.0 || ty <= 1.0 || tz <= 1.0 {
        if tx < ty {
            if tx < tz {
                bx += sx;
                tx += tdx;
            } else {
                bz += sz;
                tz += tdz;
            }
        } else if ty < tz {
            by += sy;
            ty += tdy;
        } else {
            bz += sz;
            tz += tdz;
        }
        if occludes(bx, by, bz) {
            return true;
        }
    }
    false
}

/// `Ticker.areAdjacentChunksTicking`: the 3×3 chunks around `pos` are loaded and tick blocks.
pub fn adjacent_chunks_ticking(level: &Level, pos: BlockPos) -> bool {
    let chunk = pos.chunk();
    for x in chunk.x - 1..=chunk.x + 1 {
        for z in chunk.z - 1..=chunk.z + 1 {
            let neighbour = ChunkPos::new(x, z);
            if !level.is_block_ticking(neighbour) || level.chunks.chunk(neighbour).is_none() {
                return false;
            }
        }
    }
    true
}

//! The built-in tracker: Vanilla's `ChunkMap.TrackedEntity` and `ServerEntity` over
//! chunk-indexed nearby-player sets.
//!
//! Scaling: every viewer is registered in the chunks of a square around it that covers the
//! largest tracking range, updated incrementally when it changes chunk. An entity that
//! changes section checks only the viewers registered in its chunk and those that see it; a
//! viewer that changes section checks only the entities in its square and those it sees. No
//! step iterates all pairs. The per-tick cost is one pass over the entities (Vanilla's
//! `sendChanges`) plus work proportional to the section changes.

use rustc_hash::{FxHashMap, FxHashSet};
use vanillite_base::math::{java_round, pack_degrees};
use vanillite_base::{ChunkPos, SectionPos, Vec3};
use vanillite_data::entity::EntityTypeId;

use super::{
    EntityTracker, EntityTrackerProvider, EntityUpdate, TrackerInput, TrackerOutput,
    TrackerSettings, Viewer, ViewerSpan,
};
use crate::entity::{CoreColumns, EntityFlags, EntityId, EntityStore, NetworkId};

/// `ServerEntity`: movements below this squared distance are not sent.
const MIN_MOVE_SQR: f64 = 7.629_394_5e-6_f32 as f64;
/// `ServerEntity`: ticks after which an absolute position is forced.
const TELEPORT_DELAY: i32 = 400;
/// `ServerEntity`: the position is resent at least this often (in update ticks).
const FORCE_POS_INTERVAL: i32 = 60;

/// Provides [`ChunkTracker`]s.
#[derive(Debug, Default)]
pub struct ChunkTrackerProvider;

impl EntityTrackerProvider for ChunkTrackerProvider {
    fn create(&self, settings: &TrackerSettings) -> Box<dyn EntityTracker> {
        Box::new(ChunkTracker::new(settings.clone()))
    }
}

/// What was last sent for an entity (`ServerEntity` fields).
#[derive(Debug)]
struct SyncState {
    tick_count: i32,
    teleport_delay: i32,
    base: Vec3,
    y_rot: i8,
    x_rot: i8,
    y_head_rot: i8,
    movement: Vec3,
    was_on_ground: bool,
    was_riding: bool,
}

impl SyncState {
    fn new(core: &CoreColumns, row: usize) -> SyncState {
        let rot = core.rot[row];
        SyncState {
            tick_count: 0,
            teleport_delay: 0,
            base: core.pos[row],
            y_rot: pack_degrees(rot.y_rot),
            x_rot: pack_degrees(rot.x_rot),
            y_head_rot: pack_degrees(rot.y_head_rot),
            movement: core.velocity[row],
            was_on_ground: core.flags[row].contains(EntityFlags::ON_GROUND),
            was_riding: false,
        }
    }
}

/// One tracked entity.
#[derive(Debug)]
struct Tracked {
    network_id: NetworkId,
    kind: EntityTypeId,
    /// Section at the last update; `None` before the first.
    section: Option<SectionPos>,
    /// Chunk it is registered under in `by_chunk`.
    chunk: Option<ChunkPos>,
    /// Viewers that see it.
    seen_by: Vec<EntityId>,
    sync: SyncState,
}

/// One viewer.
#[derive(Debug)]
struct ViewerState {
    view: Viewer,
    /// Centre of the square it is registered in (`nearby`).
    square: Option<ChunkPos>,
    /// Entities it sees.
    viewing: FxHashSet<EntityId>,
    present: bool,
}

/// The built-in `entity-tracker`.
#[derive(Debug)]
pub struct ChunkTracker {
    settings: TrackerSettings,
    /// Chebyshev radius in chunks of a viewer's registration square.
    radius: i32,
    tracked: FxHashMap<EntityId, Tracked>,
    /// Tracked entities per chunk.
    by_chunk: FxHashMap<i64, Vec<EntityId>>,
    viewers: FxHashMap<EntityId, ViewerState>,
    /// Viewers whose square covers a chunk.
    nearby: FxHashMap<i64, Vec<EntityId>>,
    // Scratch buffers, kept for their capacity.
    moved: Vec<EntityId>,
    candidates: Vec<EntityId>,
    added: Vec<EntityId>,
    gone: Vec<EntityId>,
}

impl ChunkTracker {
    /// A tracker with the given ranges.
    pub fn new(settings: TrackerSettings) -> ChunkTracker {
        let radius = settings.max_range().div_euclid(16) + 2;
        ChunkTracker {
            settings,
            radius,
            tracked: FxHashMap::default(),
            by_chunk: FxHashMap::default(),
            viewers: FxHashMap::default(),
            nearby: FxHashMap::default(),
            moved: Vec::new(),
            candidates: Vec::new(),
            added: Vec::new(),
            gone: Vec::new(),
        }
    }

    /// Sends `Remove` for despawned entities and forgets them.
    fn forget_removed(&mut self, store: &EntityStore, out: &mut TrackerOutput) {
        for removed in store.removed() {
            let Some(tracked) = self.tracked.remove(&removed.id) else {
                continue;
            };
            if let Some(chunk) = tracked.chunk {
                remove_from(&mut self.by_chunk, chunk.as_long(), removed.id);
            }
            for viewer in &tracked.seen_by {
                if let Some(state) = self.viewers.get_mut(viewer) {
                    state.viewing.remove(&removed.id);
                }
            }
            let span = out.push_viewers(tracked.seen_by.iter().copied());
            out.push(removed.id, tracked.network_id, EntityUpdate::Remove, span);
        }
    }

    /// Takes over the current viewers: registers squares, notes new viewers and view changes
    /// in `moved`, and drops viewers that left (without packets: their client is gone or
    /// changes dimension, which clears its entities).
    fn sync_viewers(&mut self, input: &TrackerInput<'_>) {
        let core = input.entities.columns();
        for state in self.viewers.values_mut() {
            state.present = false;
        }
        for viewer in input.viewers {
            let Some(row) = input.entities.row(viewer.entity) else {
                continue;
            };
            let chunk = core.section[row].chunk();
            let state = self
                .viewers
                .entry(viewer.entity)
                .or_insert_with(|| ViewerState {
                    view: *viewer,
                    square: None,
                    viewing: FxHashSet::default(),
                    present: false,
                });
            if state.square.is_none() || state.view != *viewer {
                state.view = *viewer;
                push_unique(&mut self.moved, viewer.entity);
            }
            state.present = true;
            if state.square != Some(chunk) {
                move_square(
                    &mut self.nearby,
                    viewer.entity,
                    state.square,
                    Some(chunk),
                    self.radius,
                );
                state.square = Some(chunk);
            }
        }
        self.gone.clear();
        self.gone.extend(
            self.viewers
                .iter()
                .filter(|(_, state)| !state.present)
                .map(|(&id, _)| id),
        );
        for id in &self.gone {
            let Some(state) = self.viewers.remove(id) else {
                continue;
            };
            move_square(&mut self.nearby, *id, state.square, None, self.radius);
            for entity in state.viewing {
                if let Some(tracked) = self.tracked.get_mut(&entity) {
                    remove_unordered(&mut tracked.seen_by, *id);
                }
            }
        }
        self.gone.clear();
    }
}

impl EntityTracker for ChunkTracker {
    fn update(&mut self, input: &TrackerInput<'_>, out: &mut TrackerOutput) {
        let store = input.entities;
        let core = store.columns();
        self.moved.clear();
        self.forget_removed(store, out);
        self.sync_viewers(input);

        let ChunkTracker {
            settings,
            radius,
            tracked,
            by_chunk,
            viewers,
            nearby,
            moved,
            candidates,
            added,
            gone,
        } = self;
        let radius = *radius;
        let mut pairing = Pairing {
            input,
            settings,
            viewers,
            added,
            gone,
        };

        // `ChunkMap.tick`: section changes update the entity's viewers, then `sendChanges`.
        for row in 0..core.id.len() {
            let id = core.id[row];
            let section = core.section[row];
            let entry = tracked.entry(id).or_insert_with(|| Tracked {
                network_id: core.network_id[row],
                kind: core.kind[row],
                section: None,
                chunk: None,
                seen_by: Vec::new(),
                sync: SyncState::new(core, row),
            });
            if entry.section != Some(section) {
                let chunk = section.chunk();
                if entry.chunk != Some(chunk) {
                    if let Some(old) = entry.chunk {
                        remove_from(by_chunk, old.as_long(), id);
                    }
                    by_chunk.entry(chunk.as_long()).or_default().push(id);
                    entry.chunk = Some(chunk);
                }
                entry.section = Some(section);
                candidates.clear();
                if let Some(list) = nearby.get(&chunk.as_long()) {
                    candidates.extend_from_slice(list);
                }
                // Viewers that see it but are no longer registered near its chunk.
                for &viewer in &entry.seen_by {
                    let near = pairing
                        .viewers
                        .get(&viewer)
                        .and_then(|state| state.square)
                        .is_some_and(|square| in_square(square, radius, chunk));
                    if !near {
                        candidates.push(viewer);
                    }
                }
                pairing.update(entry, id, row, candidates, out);
                if pairing.viewers.contains_key(&id) {
                    push_unique(moved, id);
                }
            }
            let is_viewer = pairing.viewers.contains_key(&id);
            send_changes(entry, core, row, id, is_viewer, out);
        }

        // Viewers that moved to another section (or joined) re-check what they can see.
        for &viewer in moved.iter() {
            let Some(state) = pairing.viewers.get(&viewer) else {
                continue;
            };
            let Some(square) = state.square else {
                continue;
            };
            candidates.clear();
            for_square(square, radius, |chunk| {
                if let Some(list) = by_chunk.get(&chunk.as_long()) {
                    candidates.extend_from_slice(list);
                }
            });
            candidates.extend(state.viewing.iter().copied().filter(|&entity| {
                tracked
                    .get(&entity)
                    .and_then(|t| t.chunk)
                    .is_none_or(|chunk| !in_square(square, radius, chunk))
            }));
            for &entity in candidates.iter() {
                let (Some(entry), Some(row)) = (tracked.get_mut(&entity), store.row(entity)) else {
                    continue;
                };
                pairing.update(entry, entity, row, &[viewer], out);
            }
        }
    }
}

/// Visibility changes between entities and viewers (`TrackedEntity.updatePlayer`).
struct Pairing<'a, 'i> {
    input: &'a TrackerInput<'i>,
    settings: &'a TrackerSettings,
    viewers: &'a mut FxHashMap<EntityId, ViewerState>,
    added: &'a mut Vec<EntityId>,
    gone: &'a mut Vec<EntityId>,
}

impl Pairing<'_, '_> {
    /// Re-checks `candidates` as viewers of one entity and emits `Remove` and `Spawn`.
    fn update(
        &mut self,
        entry: &mut Tracked,
        id: EntityId,
        row: usize,
        candidates: &[EntityId],
        out: &mut TrackerOutput,
    ) {
        self.added.clear();
        self.gone.clear();
        for &viewer in candidates {
            if viewer == id {
                continue;
            }
            let Some(state) = self.viewers.get_mut(&viewer) else {
                continue;
            };
            let visible = is_visible(self.input, self.settings, row, &state.view);
            let seen = state.viewing.contains(&id);
            if visible && !seen {
                state.viewing.insert(id);
                entry.seen_by.push(viewer);
                self.added.push(viewer);
            } else if !visible && seen {
                state.viewing.remove(&id);
                remove_unordered(&mut entry.seen_by, viewer);
                self.gone.push(viewer);
            }
        }
        if !self.gone.is_empty() {
            let span = out.push_viewers(self.gone.iter().copied());
            out.push(id, entry.network_id, EntityUpdate::Remove, span);
        }
        if !self.added.is_empty() {
            let core = self.input.entities.columns();
            let sync = &entry.sync;
            let spawn = EntityUpdate::Spawn {
                kind: entry.kind,
                uuid: core.uuid[row],
                pos: sync.base,
                velocity: sync.movement,
                y_rot: sync.y_rot,
                x_rot: sync.x_rot,
                y_head_rot: sync.y_head_rot,
            };
            let span = out.push_viewers(self.added.iter().copied());
            out.push(id, entry.network_id, spawn, span);
        }
    }
}

/// `TrackedEntity.updatePlayer`'s condition: horizontal distance within the range (capped by
/// the view distance), the entity's chunk inside the viewer's view and already sent.
fn is_visible(
    input: &TrackerInput<'_>,
    settings: &TrackerSettings,
    row: usize,
    view: &Viewer,
) -> bool {
    let Some(viewer_row) = input.entities.row(view.entity) else {
        return false;
    };
    let core = input.entities.columns();
    let delta = core.pos[viewer_row] - core.pos[row];
    let distance = delta.x * delta.x + delta.z * delta.z;
    let range = f64::from(settings.range(core.kind[row]));
    let chunk = core.section[row].chunk();
    distance <= range * range
        && view.sees_chunk(chunk)
        && !(input.chunk_pending)(view.entity, chunk)
}

/// Lazily stored viewer lists of one entity for one tick.
#[derive(Default)]
struct Spans {
    seen: Option<ViewerSpan>,
    with_self: Option<ViewerSpan>,
}

impl Spans {
    /// `broadcast`: the viewers.
    fn seen(&mut self, out: &mut TrackerOutput, seen_by: &[EntityId]) -> ViewerSpan {
        *self
            .seen
            .get_or_insert_with(|| out.push_viewers(seen_by.iter().copied()))
    }

    /// `broadcastAndSend`: the viewers and, for players, the entity itself.
    fn with_self(
        &mut self,
        out: &mut TrackerOutput,
        seen_by: &[EntityId],
        id: EntityId,
        is_viewer: bool,
    ) -> ViewerSpan {
        if !is_viewer {
            return self.seen(out, seen_by);
        }
        *self.with_self.get_or_insert_with(|| {
            out.push_viewers(seen_by.iter().copied().chain(std::iter::once(id)))
        })
    }
}

/// `ServerEntity.sendChanges` (passengers' list and maps aside).
fn send_changes(
    entry: &mut Tracked,
    core: &CoreColumns,
    row: usize,
    id: EntityId,
    is_viewer: bool,
    out: &mut TrackerOutput,
) {
    let info = entry.kind.info();
    let flags = core.flags[row];
    let network_id = entry.network_id;
    let seen_by = entry.seen_by.as_slice();
    let sync = &mut entry.sync;
    let mut spans = Spans::default();
    let mut broadcast = |out: &mut TrackerOutput, update: EntityUpdate| {
        let span = spans.seen(out, seen_by);
        out.push(id, network_id, update, span);
    };

    let interval = info.update_interval.max(1);
    if sync.tick_count.wrapping_rem(interval) == 0
        || flags.contains(EntityFlags::HAS_IMPULSE)
        || flags.contains(EntityFlags::DATA_DIRTY)
    {
        let rot = core.rot[row];
        let pos = core.pos[row];
        let on_ground = flags.contains(EntityFlags::ON_GROUND);
        let y_rot = pack_degrees(rot.y_rot);
        let x_rot = pack_degrees(rot.x_rot);
        let rot_changed = y_rot != sync.y_rot || x_rot != sync.x_rot;
        let mut send_data = flags.contains(EntityFlags::DATA_DIRTY);

        if flags.contains(EntityFlags::PASSENGER) {
            if rot_changed {
                broadcast(
                    out,
                    EntityUpdate::Rot {
                        y_rot,
                        x_rot,
                        on_ground,
                    },
                );
                sync.y_rot = y_rot;
                sync.x_rot = x_rot;
            }
            sync.base = pos;
            sync.was_riding = true;
        } else {
            sync.teleport_delay = sync.teleport_delay.wrapping_add(1);
            let pos_changed = (pos - sync.base).length_sqr() >= MIN_MOVE_SQR;
            let send_pos = pos_changed || sync.tick_count.wrapping_rem(FORCE_POS_INTERVAL) == 0;
            let mut sent_pos = false;
            let mut sent_rot = false;
            let mut packet = None;
            if sync.tick_count > 0 {
                let encode =
                    |now: f64, base: f64| java_round(now * 4096.0) - java_round(base * 4096.0);
                let dx = encode(pos.x, sync.base.x);
                let dy = encode(pos.y, sync.base.y);
                let dz = encode(pos.z, sync.base.z);
                let short = |d: i64| i16::try_from(d).ok();
                let deltas = short(dx).zip(short(dy)).zip(short(dz));
                match deltas {
                    Some(((dx, dy), dz))
                        if !flags.contains(EntityFlags::PRECISE_POSITION)
                            && sync.teleport_delay <= TELEPORT_DELAY
                            && !sync.was_riding
                            && sync.was_on_ground == on_ground =>
                    {
                        if send_pos && rot_changed {
                            packet = Some(EntityUpdate::PosRot {
                                dx,
                                dy,
                                dz,
                                y_rot,
                                x_rot,
                                on_ground,
                            });
                            sent_pos = true;
                            sent_rot = true;
                        } else if send_pos {
                            packet = Some(EntityUpdate::Pos {
                                dx,
                                dy,
                                dz,
                                on_ground,
                            });
                            sent_pos = true;
                        } else if rot_changed {
                            packet = Some(EntityUpdate::Rot {
                                y_rot,
                                x_rot,
                                on_ground,
                            });
                            sent_rot = true;
                        }
                    }
                    _ => {
                        sync.was_on_ground = on_ground;
                        sync.teleport_delay = 0;
                        packet = Some(EntityUpdate::Sync {
                            pos,
                            velocity: core.velocity[row],
                            y_rot: rot.y_rot,
                            x_rot: rot.x_rot,
                            on_ground,
                        });
                        sent_pos = true;
                        sent_rot = true;
                    }
                }
            }
            if (flags.contains(EntityFlags::HAS_IMPULSE)
                || info.track_deltas
                || flags.contains(EntityFlags::FALL_FLYING))
                && sync.tick_count > 0
            {
                let velocity = core.velocity[row];
                let change = velocity.distance_to_sqr(sync.movement);
                if change > 1.0e-7 || (change > 0.0 && velocity.length_sqr() == 0.0) {
                    sync.movement = velocity;
                    broadcast(out, EntityUpdate::Motion(velocity));
                }
            }
            if let Some(packet) = packet {
                broadcast(out, packet);
            }
            if send_data {
                let span = spans.with_self(out, seen_by, id, is_viewer);
                out.push(id, network_id, EntityUpdate::Data, span);
                send_data = false;
            }
            if sent_pos {
                sync.base = pos;
            }
            if sent_rot {
                sync.y_rot = y_rot;
                sync.x_rot = x_rot;
            }
            sync.was_riding = false;
        }
        if send_data {
            let span = spans.with_self(out, seen_by, id, is_viewer);
            out.push(id, network_id, EntityUpdate::Data, span);
        }

        let y_head_rot = pack_degrees(rot.y_head_rot);
        if y_head_rot != sync.y_head_rot {
            let span = spans.seen(out, seen_by);
            out.push(id, network_id, EntityUpdate::HeadRot(y_head_rot), span);
            sync.y_head_rot = y_head_rot;
        }
    }
    sync.tick_count = sync.tick_count.wrapping_add(1);
    if flags.contains(EntityFlags::HURT_MARKED) {
        let span = spans.with_self(out, seen_by, id, is_viewer);
        out.push(
            id,
            network_id,
            EntityUpdate::Motion(core.velocity[row]),
            span,
        );
    }
}

fn in_square(center: ChunkPos, radius: i32, chunk: ChunkPos) -> bool {
    (chunk.x - center.x).abs() <= radius && (chunk.z - center.z).abs() <= radius
}

fn for_square(center: ChunkPos, radius: i32, mut f: impl FnMut(ChunkPos)) {
    for x in center.x - radius..=center.x + radius {
        for z in center.z - radius..=center.z + radius {
            f(ChunkPos::new(x, z));
        }
    }
}

/// Moves a viewer's registration from the square around `old` to the one around `new`,
/// touching only the chunks that differ.
fn move_square(
    nearby: &mut FxHashMap<i64, Vec<EntityId>>,
    viewer: EntityId,
    old: Option<ChunkPos>,
    new: Option<ChunkPos>,
    radius: i32,
) {
    if let Some(old) = old {
        for_square(old, radius, |chunk| {
            if !new.is_some_and(|new| in_square(new, radius, chunk)) {
                remove_from(nearby, chunk.as_long(), viewer);
            }
        });
    }
    if let Some(new) = new {
        for_square(new, radius, |chunk| {
            if !old.is_some_and(|old| in_square(old, radius, chunk)) {
                nearby.entry(chunk.as_long()).or_default().push(viewer);
            }
        });
    }
}

fn remove_from(map: &mut FxHashMap<i64, Vec<EntityId>>, key: i64, id: EntityId) {
    if let Some(list) = map.get_mut(&key) {
        remove_unordered(list, id);
        if list.is_empty() {
            map.remove(&key);
        }
    }
}

fn remove_unordered(list: &mut Vec<EntityId>, id: EntityId) {
    if let Some(index) = list.iter().position(|&e| e == id) {
        list.swap_remove(index);
    }
}

fn push_unique(list: &mut Vec<EntityId>, id: EntityId) {
    if !list.contains(&id) {
        list.push(id);
    }
}

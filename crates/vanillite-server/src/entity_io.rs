//! Entity persistence of a world (Vanilla's `PersistentEntitySectionManager` with
//! `EntityStorage`): when a chunk becomes live its entities load from `entities/` on an I/O
//! thread; when it unloads its entities are saved and leave the world. Entity types without
//! behaviour yet (items, villagers, … of imported worlds) stay as NBT and are written back
//! unchanged.

use std::sync::mpsc::{Receiver, Sender, channel};

use vanillite_base::hash::{FxHashMap, FxHashSet};
use vanillite_base::{Aabb, BlockPos, ChunkPos};
use vanillite_data::entity::ids;
use vanillite_game::entities::is_mob_kind;
use vanillite_game::level::Level;
use vanillite_game::mob::{self, nbt};
use vanillite_nbt::Compound;
use vanillite_storage::compression::Compression;
use vanillite_storage::entities::EntityStorage;
use vanillite_world::entity::{EntityFlags, EntityId, NewEntity, Rotation};

enum Request {
    Load(ChunkPos),
    Save(ChunkPos, Vec<Compound>),
    Flush(Sender<()>),
}

/// The entity chunks of one world.
pub(crate) struct EntityChunks {
    tx: Option<Sender<Request>>,
    rx: Receiver<(ChunkPos, Vec<Compound>)>,
    /// Chunks whose entities are in the world.
    loaded: FxHashSet<ChunkPos>,
    /// Chunks whose entities are being read.
    pending: FxHashSet<ChunkPos>,
    /// Entities of types without behaviour, kept as NBT per chunk.
    raw: FxHashMap<ChunkPos, Vec<Compound>>,
    scratch: Vec<ChunkPos>,
    ids: Vec<EntityId>,
    seed: i64,
}

impl std::fmt::Debug for EntityChunks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EntityChunks")
            .field("loaded", &self.loaded.len())
            .field("pending", &self.pending.len())
            .finish_non_exhaustive()
    }
}

impl EntityChunks {
    /// The entity chunks of the world stored in `dimension_dir`, with an I/O thread.
    pub(crate) fn new(
        dimension_dir: &std::path::Path,
        compression: Compression,
        name: &str,
    ) -> EntityChunks {
        let (tx, requests) = channel::<Request>();
        let (done, rx) = channel();
        let mut storage = EntityStorage::new(dimension_dir, compression);
        let spawned = std::thread::Builder::new()
            .name(format!("Entity IO {name}"))
            .spawn(move || {
                for request in requests {
                    match request {
                        Request::Load(pos) => {
                            let entities = storage.load(pos).unwrap_or_else(|e| {
                                log::error!(
                                    "Couldn't load the entities of chunk {}, {}: {e}",
                                    pos.x,
                                    pos.z
                                );
                                Vec::new()
                            });
                            if done.send((pos, entities)).is_err() {
                                return;
                            }
                        }
                        Request::Save(pos, entities) => {
                            if let Err(e) = storage.save(pos, entities) {
                                log::error!(
                                    "Couldn't save the entities of chunk {}, {}: {e}",
                                    pos.x,
                                    pos.z
                                );
                            }
                        }
                        Request::Flush(reply) => {
                            if let Err(e) = storage.flush() {
                                log::error!("Couldn't flush the entity regions: {e}");
                            }
                            let _ = reply.send(());
                        }
                    }
                }
            });
        let tx = match spawned {
            Ok(_) => Some(tx),
            Err(e) => {
                log::error!("Couldn't start the entity I/O thread: {e}");
                None
            }
        };
        EntityChunks {
            tx,
            rx,
            loaded: FxHashSet::default(),
            pending: FxHashSet::default(),
            raw: FxHashMap::default(),
            scratch: Vec::new(),
            ids: Vec::new(),
            seed: 0,
        }
    }

    fn send(&self, request: Request) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(request);
        }
    }

    /// Requests the entities of chunks that became live, adds those that arrived, and saves
    /// and removes the entities of chunks that unloaded.
    pub(crate) fn tick(&mut self, level: &mut Level) {
        self.scratch.extend_from_slice(level.promoted_chunks());
        for pos in self.scratch.drain(..) {
            if !self.loaded.contains(&pos)
                && self.pending.insert(pos)
                && let Some(tx) = &self.tx
            {
                let _ = tx.send(Request::Load(pos));
            }
        }
        while let Ok((pos, entities)) = self.rx.try_recv() {
            if !self.pending.remove(&pos) || level.chunks.chunk(pos).is_none() {
                continue;
            }
            self.loaded.insert(pos);
            self.add(level, pos, entities);
        }
        level.chunks.take_unloaded(&mut self.scratch);
        let unloaded = std::mem::take(&mut self.scratch);
        for &pos in &unloaded {
            self.pending.remove(&pos);
            if !self.loaded.remove(&pos) {
                continue;
            }
            let entities = self.collect(level, pos, true);
            self.send(Request::Save(pos, entities));
        }
        self.scratch = unloaded;
        self.scratch.clear();
    }

    /// Adds loaded entities to the world.
    fn add(&mut self, level: &mut Level, pos: ChunkPos, entities: Vec<Compound>) {
        let mut raw = Vec::new();
        for tag in entities {
            self.seed = self.seed.wrapping_add(0x9E37_79B9_7F4A_7C15_u64 as i64);
            if let Some(loaded) = nbt::load_mob(&tag, self.seed ^ level.game_time()) {
                let network_ids = std::sync::Arc::clone(level.network_ids_arc());
                let id = mob::spawn(
                    &mut level.entities,
                    &network_ids,
                    loaded.uuid,
                    loaded.pos,
                    loaded.rot,
                    loaded.data,
                );
                if let Some(mut e) = level.entities.store.get_mut(id) {
                    e.set_velocity(loaded.velocity);
                    e.set_flags(EntityFlags::ON_GROUND, loaded.on_ground);
                }
            } else if let Some((p, v, uuid, orb)) = nbt::load_orb(&tag) {
                let mut new =
                    NewEntity::new(ids::EXPERIENCE_ORB, level.network_ids().next(), uuid, p);
                new.velocity = v;
                new.rot = Rotation::default();
                level.entities.spawn_orb(new, orb);
            } else if let Some((e, data)) = nbt::load_item(&tag) {
                let id = level.entities.spawn_item(
                    level.network_ids().next(),
                    e.uuid,
                    e.pos,
                    e.velocity,
                    e.rot.y_rot,
                    data,
                );
                if let Some(mut ent) = level.entities.store.get_mut(id) {
                    ent.set_flags(EntityFlags::ON_GROUND, e.on_ground);
                }
            } else if let Some((e, data)) = vanillite_game::mob::missile::load(&tag) {
                let mut new = NewEntity::new(
                    data.kind.type_id(),
                    level.network_ids().next(),
                    e.uuid,
                    e.pos,
                );
                new.velocity = e.velocity;
                new.rot = e.rot;
                level.entities.spawn_missile(new, data);
            } else if let Some((e, data)) = nbt::load_arrow(&tag) {
                let mut new = NewEntity::new(ids::ARROW, level.network_ids().next(), e.uuid, e.pos);
                new.velocity = e.velocity;
                new.rot = e.rot;
                level.entities.spawn_arrow(new, data);
            } else if let Some((e, data)) = nbt::load_boat(&tag) {
                let leash = match tag.get("leash") {
                    Some(vanillite_nbt::Tag::Compound(c)) => {
                        nbt::uuid_of(c).map(mob::LeashRef::Entity)
                    }
                    _ => None,
                };
                let network_ids = std::sync::Arc::clone(level.network_ids_arc());
                let id = vanillite_game::vehicle::spawn_boat(
                    &mut level.entities,
                    &network_ids,
                    e.uuid,
                    e.pos,
                    e.rot.y_rot,
                    data,
                );
                if let Some(mut ent) = level.entities.store.get_mut(id) {
                    ent.set_velocity(e.velocity);
                }
                let _ = leash;
            } else if nbt::type_of(&tag).is_some_and(|t| t == ids::PLAYER || t == ids::LEASH_KNOT) {
                // Players are stored apart; knots come back with their leashes.
            } else {
                raw.push(tag);
            }
        }
        if !raw.is_empty() {
            self.raw.insert(pos, raw);
        }
    }

    /// The NBT of the saved entities in a chunk column; with `remove` they leave the world.
    fn collect(&mut self, level: &mut Level, pos: ChunkPos, remove: bool) -> Vec<Compound> {
        let min = f64::from(level.min_y()) - 64.0;
        let max = f64::from(level.max_y()) + 64.0;
        let area = Aabb::new(
            f64::from(pos.x * 16),
            min,
            f64::from(pos.z * 16),
            f64::from(pos.x * 16 + 16),
            max,
            f64::from(pos.z * 16 + 16),
        );
        let mut ids = std::mem::take(&mut self.ids);
        ids.clear();
        level
            .entities
            .store
            .for_each_in(&area, false, |id| ids.push(id));
        let mut out = self.raw.get(&pos).cloned().unwrap_or_default();
        if remove {
            self.raw.remove(&pos);
        }
        for &id in &ids {
            let Some(e) = level.entities.store.get(id) else {
                continue;
            };
            let p = e.pos();
            if BlockPos::containing(p.x, p.y, p.z).chunk() != pos {
                continue;
            }
            let kind = e.kind();
            let ents = &level.entities;
            let leash = ents.leashes.get(&id).and_then(|&h| {
                if let Some(&pos) = ents.knots.get(ents.store.get(h)?.kind_row())
                    && ents.store.get(h)?.kind() == ids::LEASH_KNOT
                {
                    return Some(mob::LeashRef::Knot(pos));
                }
                Some(mob::LeashRef::Entity(ents.store.get(h)?.uuid()))
            });
            let tag = if is_mob_kind(kind) {
                ents.mob(id).filter(|m| m.is_alive()).map(|m| {
                    if leash.is_some() {
                        let mut m = m.clone();
                        m.pending_leash = leash;
                        nbt::save_mob(&e, &m)
                    } else {
                        nbt::save_mob(&e, m)
                    }
                })
            } else if kind == ids::EXPERIENCE_ORB {
                ents.orb(id).map(|o| nbt::save_orb(&e, o))
            } else if kind == ids::ARROW {
                ents.arrow(id).map(|a| {
                    let owner = a.owner.and_then(|o| ents.store.get(o)).map(|o| o.uuid());
                    nbt::save_arrow(&e, a, owner)
                })
            } else if let Some(d) = ents.missile(id) {
                let owner = d.owner.and_then(|o| ents.store.get(o)).map(|o| o.uuid());
                Some(vanillite_game::mob::missile::save(&e, d, owner))
            } else if kind == ids::ITEM {
                ents.item(id)
                    .filter(|d| !d.stack.is_empty())
                    .map(|d| nbt::save_item(&e, d))
            } else if let Some(b) = ents.boat(id) {
                Some(nbt::save_boat(&e, b, leash))
            } else if kind == ids::LEASH_KNOT {
                None
            } else {
                continue;
            };
            if let Some(tag) = tag {
                out.push(tag);
            }
            if remove {
                level.entities.despawn(id);
            }
        }
        self.ids = ids;
        out
    }

    /// Saves the entities of every loaded chunk; `flush` waits until they are on disk.
    pub(crate) fn save_all(&mut self, level: &mut Level, flush: bool) {
        let chunks: Vec<ChunkPos> = self.loaded.iter().copied().collect();
        for pos in chunks {
            let entities = self.collect(level, pos, false);
            self.send(Request::Save(pos, entities));
        }
        if flush {
            let (reply, wait) = channel();
            self.send(Request::Flush(reply));
            let _ = wait.recv();
        }
    }
}

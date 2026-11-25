//! Entities stored with their chunk (Vanilla's `entities/` records and
//! `PersistentEntitySectionManager`).
//!
//! - When a chunk becomes live its stored entities are created in stored order; kinds the game
//!   does not model yet stay parked as stored data and are written back unchanged.
//! - Before a chunk unloads and when the world saves, the live entities of the chunk are written
//!   back section by section from the bottom (`getExistingSectionsInChunk`), each in section
//!   order; after the unload they are removed.
//! - An entity ticks only while its chunk is entity ticking (`Visibility.TICKING`): the chunk
//!   runtime reports the changes and the entity store keeps its tick order like Vanilla's
//!   `startTicking` and `stopTicking`.
//!
//! Stored fields the game does not model (custom names, tags, custom data, …) are kept from the
//! loaded data and written back.

use std::sync::Arc;

use vanillite_base::{BlockPos, ChunkPos, Vec3};
use vanillite_data::entity::{EntityTypeId, ids};
use vanillite_nbt::{Budget, Compound, List, Tag};
use vanillite_world::chunks::Visibility;
use vanillite_world::entity::{EntityFlags, EntityId, NewEntity, Rotation};

use super::Level;
use super::block_entity::{read_block_state, write_block_state};
use crate::entities::{FallingBlockData, PrimedTntData};
use crate::item_entity::ItemData;

impl Level {
    /// Applies the visibility changes of the chunk runtime
    /// (`PersistentEntitySectionManager.updateChunkStatus`): a chunk that becomes visible
    /// loads its stored entities, one that becomes hidden writes them back and removes them;
    /// entity ticking chunks put their entities in the tick order.
    pub(super) fn apply_visibility(&mut self) {
        let mut changes = std::mem::take(&mut self.visibility_scratch);
        self.chunks.take_visibility_changes(&mut changes);
        for change in &changes {
            let pos = change.pos;
            let ticking = change.new == Visibility::Ticking;
            if change.new == Visibility::Hidden {
                self.write_entities(pos);
                self.remove_entities(pos);
                self.write_poi(pos);
                self.entities.poi.unload_chunk(pos);
            }
            self.entities.store.set_chunk_ticking(pos.x, pos.z, ticking);
            if change.old == Visibility::Hidden {
                self.load_poi(pos);
                self.load_entities(pos);
            }
        }
        changes.clear();
        self.visibility_scratch = changes;
    }

    /// Creates the stored entities of a chunk that became live (`addEntityWithoutEvent` in
    /// stored order).
    pub(super) fn load_entities(&mut self, chunk: ChunkPos) {
        let stored: Vec<Arc<[u8]>> = self.chunks.stored_entities(chunk).to_vec();
        if stored.is_empty() {
            return;
        }
        self.entity_chunks.insert(chunk);
        let mut parked = Vec::new();
        for data in stored {
            let mut input = &data[..];
            let Ok(Some(Tag::Compound(nbt))) =
                vanillite_nbt::read::read_network(&mut input, &mut Budget::file())
            else {
                continue;
            };
            if !self.load_entity(nbt) {
                parked.push(data);
            }
        }
        if !parked.is_empty() {
            self.parked_entities.insert(chunk, parked);
        }
    }

    /// `EntityType.loadEntityRecursive`: creates a live entity of a modelled kind; `false` for
    /// kinds the game does not model.
    /// Whether the entities of a chunk are in the world (`areEntitiesLoaded`).
    pub fn entities_loaded(&self, chunk: ChunkPos) -> bool {
        self.entity_chunks.contains(&chunk)
    }

    fn load_entity(&mut self, nbt: Compound) -> bool {
        let seed = uuid_of(&nbt, "UUID").map_or(0, |uuid| uuid as i64) ^ self.game_time();
        if let Some(loaded) = crate::mob::nbt::load_mob(&nbt, seed) {
            let network_ids = Arc::clone(self.network_ids_arc());
            let id = crate::mob::spawn(
                &mut self.entities,
                &network_ids,
                loaded.uuid,
                loaded.pos,
                loaded.rot,
                loaded.data,
            );
            if let Some(mut e) = self.entities.store.get_mut(id) {
                e.set_velocity(loaded.velocity);
                e.set_flags(EntityFlags::ON_GROUND, loaded.on_ground);
            }
            return true;
        }
        if let Some((kind, pos, velocity, rot, uuid, data)) =
            crate::mob::nbt::load_special(&nbt, seed)
        {
            let mut new = NewEntity::new(kind, self.network_ids().next(), uuid, pos);
            new.velocity = velocity;
            new.rot = rot;
            self.entities.spawn_special(new, data);
            return true;
        }
        if let Some((pos, uuid, cloud)) = crate::mob::nbt::load_cloud(&nbt) {
            let radius = cloud.radius;
            let new = NewEntity::new(ids::AREA_EFFECT_CLOUD, self.network_ids().next(), uuid, pos);
            let id = self.entities.spawn_cloud(new, cloud);
            if let Some(mut e) = self.entities.store.get_mut(id) {
                e.set_dims(vanillite_world::entity::Dimensions {
                    width: radius * 2.0,
                    height: 0.5,
                });
            }
            return true;
        }
        if let Some((pos, velocity, uuid, orb)) = crate::mob::nbt::load_orb(&nbt) {
            let mut new = NewEntity::new(ids::EXPERIENCE_ORB, self.network_ids().next(), uuid, pos);
            new.velocity = velocity;
            self.entities.spawn_experience_orb(new, orb);
            return true;
        }
        if let Some((e, data)) = crate::mob::nbt::load_arrow(&nbt) {
            let mut new = NewEntity::new(ids::ARROW, self.network_ids().next(), e.uuid, e.pos);
            new.velocity = e.velocity;
            new.rot = e.rot;
            self.entities.spawn_arrow(new, data);
            return true;
        }
        if let Some((e, data)) = crate::mob::missile::load(&nbt) {
            let mut new = NewEntity::new(
                data.kind.type_id(),
                self.network_ids().next(),
                e.uuid,
                e.pos,
            );
            new.velocity = e.velocity;
            new.rot = e.rot;
            self.entities.spawn_missile(new, data);
            return true;
        }
        if let Some((e, data)) = crate::mob::nbt::load_boat(&nbt) {
            let network_ids = Arc::clone(self.network_ids_arc());
            let id = crate::vehicle::spawn_boat(
                &mut self.entities,
                &network_ids,
                e.uuid,
                e.pos,
                e.rot.y_rot,
                data,
            );
            if let Some(mut ent) = self.entities.store.get_mut(id) {
                ent.set_velocity(e.velocity);
            }
            return true;
        }
        if let Some((e, data)) = crate::mob::nbt::load_minecart(&nbt, seed) {
            let network_ids = Arc::clone(self.network_ids_arc());
            let id = crate::minecart::spawn(
                &mut self.entities,
                &network_ids,
                e.uuid,
                e.pos,
                e.rot,
                data,
            );
            if let Some(mut ent) = self.entities.store.get_mut(id) {
                ent.set_velocity(e.velocity);
                ent.set_flags(EntityFlags::ON_GROUND, e.on_ground);
            }
            return true;
        }
        let Some(kind) = nbt
            .get("id")
            .and_then(Tag::as_str)
            .and_then(EntityTypeId::by_name)
        else {
            return false;
        };
        if kind == ids::LEASH_KNOT {
            // Knots come back with the leashes of their mobs.
            return true;
        }
        if kind != ids::ITEM && kind != ids::FALLING_BLOCK && kind != ids::TNT {
            return false;
        }
        let base = Base::load(&nbt);
        let network_id = self.network_ids().next();
        let uuid = base.uuid.unwrap_or_else(|| self.new_uuid());
        let mut new = NewEntity::new(kind, network_id, uuid, base.pos);
        new.velocity = base.motion;
        new.rot = base.rot;
        new.flags.set(EntityFlags::ON_GROUND, base.on_ground);
        new.flags.set(EntityFlags::NO_GRAVITY, base.no_gravity);
        let short = |name: &str, default: i16| {
            nbt.get(name)
                .and_then(Tag::as_i64)
                .map_or(default, |v| v as i16)
        };
        let id = if kind == ids::ITEM {
            let stack = match nbt.get("Item") {
                Some(Tag::Compound(c)) => crate::inventory::stack_from_nbt(c),
                _ => crate::item::ItemStack::EMPTY,
            };
            // `readAdditionalSaveData`: an item entity without an item is discarded.
            if stack.is_empty() {
                return true;
            }
            let mut data = ItemData::new(stack, short("PickupDelay", 0), uuid_of(&nbt, "Thrower"));
            data.age = short("Age", 0);
            data.target = uuid_of(&nbt, "Owner");
            let items = &mut self.entities.items;
            self.entities.store.spawn(new, |id| items.push(id, data))
        } else if kind == ids::FALLING_BLOCK {
            let state = match nbt.get("BlockState") {
                Some(Tag::Compound(c)) => read_block_state(c),
                _ => vanillite_data::block::ids::SAND.default_state(),
            };
            let data = FallingBlockData {
                state,
                time: int(&nbt, "Time").unwrap_or(0),
                drop_item: flag(&nbt, "DropItem").unwrap_or(true),
                start: BlockPos::containing(base.pos.x, base.pos.y, base.pos.z),
            };
            self.entities.spawn_falling_block(new, data)
        } else {
            let state = match nbt.get("block_state") {
                Some(Tag::Compound(c)) => read_block_state(c),
                _ => vanillite_data::block::ids::TNT.default_state(),
            };
            let power = match nbt.get("explosion_power") {
                Some(Tag::Float(v)) => v.clamp(0.0, 128.0),
                _ => 4.0,
            };
            let data = PrimedTntData {
                fuse: i32::from(short("fuse", 80)),
                state,
                owner: None,
                explosion_power: power,
            };
            self.entities.spawn_primed_tnt(new, data)
        };
        self.entity_nbt.insert(id, nbt);
        true
    }

    /// Writes the live entities of a chunk and its parked ones back into the chunk's stored
    /// entities (`storeChunkSections`).
    pub(super) fn write_entities(&mut self, chunk: ChunkPos) {
        if self.chunks.chunk(chunk).is_none() {
            return;
        }
        let ids: Vec<EntityId> = self
            .entities
            .store
            .sections()
            .in_chunk(chunk.x, chunk.z)
            .collect();
        let mut list: Vec<Arc<[u8]>> = Vec::with_capacity(ids.len());
        for id in ids {
            let Some(nbt) = self.save_entity(id) else {
                continue;
            };
            let mut bytes = Vec::new();
            if vanillite_nbt::write::write_network_compound(&nbt, &mut bytes).is_ok() {
                list.push(bytes.into());
            }
        }
        if let Some(parked) = self.parked_entities.get(&chunk) {
            list.extend(parked.iter().cloned());
        }
        if list.is_empty() {
            self.entity_chunks.remove(&chunk);
        } else {
            self.entity_chunks.insert(chunk);
        }
        self.chunks.set_stored_entities(chunk, list);
    }

    /// `Entity.save`: the stored data of a live entity of a modelled kind, `None` for players
    /// and kinds that are not saved.
    fn save_entity(&self, id: EntityId) -> Option<Compound> {
        let entity = self.entities.store.get(id)?;
        let kind = entity.kind();
        let ents = &self.entities;
        if crate::entities::is_mob_kind(kind) {
            let leash = self.leash_of(id);
            return ents.mob(id).filter(|m| m.is_alive()).map(|m| {
                if leash.is_some() {
                    let mut m = m.clone();
                    m.pending_leash = leash;
                    crate::mob::nbt::save_mob(&entity, &m)
                } else {
                    crate::mob::nbt::save_mob(&entity, m)
                }
            });
        }
        if let Some(b) = ents.boat(id) {
            return Some(crate::mob::nbt::save_boat(&entity, b, self.leash_of(id)));
        }
        if let Some(m) = ents.minecart(id) {
            return Some(crate::mob::nbt::save_minecart(&entity, m));
        }
        if kind == ids::ARROW {
            return ents.arrow(id).map(|a| {
                let owner = a.owner.and_then(|o| ents.store.get(o)).map(|o| o.uuid());
                crate::mob::nbt::save_arrow(&entity, a, owner)
            });
        }
        if let Some(d) = ents.missile(id) {
            let owner = d.owner.and_then(|o| ents.store.get(o)).map(|o| o.uuid());
            return Some(crate::mob::missile::save(&entity, d, owner));
        }
        if kind == ids::EXPERIENCE_ORB {
            return self
                .entities
                .experience_orb(id)
                .map(|o| crate::mob::nbt::save_orb(&entity, o));
        }
        if let Some(s) = ents.special(id) {
            return crate::mob::nbt::save_special(&entity, s);
        }
        if let Some(c) = ents.cloud(id) {
            return Some(crate::mob::nbt::save_cloud(&entity, c));
        }
        if kind != ids::ITEM && kind != ids::FALLING_BLOCK && kind != ids::TNT {
            return None;
        }
        if entity.flags().contains(EntityFlags::PASSENGER) {
            return None;
        }
        let mut nbt = self.entity_nbt.get(&id).cloned().unwrap_or_default();
        nbt.insert("id", kind.name());
        let pos = entity.pos();
        let motion = entity.velocity();
        let rot = entity.rot();
        nbt.insert("Pos", doubles(pos));
        nbt.insert("Motion", doubles(motion));
        nbt.insert(
            "Rotation",
            List(vec![Tag::Float(rot.y_rot), Tag::Float(rot.x_rot)]),
        );
        nbt.insert("OnGround", entity.flags().contains(EntityFlags::ON_GROUND));
        if entity.flags().contains(EntityFlags::NO_GRAVITY) {
            nbt.insert("NoGravity", true);
        } else {
            nbt.remove("NoGravity");
        }
        nbt.insert("UUID", uuid_tag(entity.uuid()));
        // Fields the game does not model keep their loaded value, else Vanilla's default.
        or_default(&mut nbt, "fall_distance", Tag::Double(0.0));
        or_default(&mut nbt, "Fire", Tag::Short(0));
        or_default(&mut nbt, "Air", Tag::Short(300));
        or_default(&mut nbt, "Invulnerable", Tag::Byte(0));
        or_default(&mut nbt, "PortalCooldown", Tag::Int(0));
        if kind == ids::ITEM {
            let data = self.entities.item(id)?;
            or_default(&mut nbt, "Health", Tag::Short(5));
            nbt.insert("Age", data.age);
            nbt.insert("PickupDelay", data.pickup_delay);
            match data.thrower {
                Some(thrower) => nbt.insert("Thrower", uuid_tag(thrower)),
                None => {
                    nbt.remove("Thrower");
                }
            }
            match data.target {
                Some(target) => nbt.insert("Owner", uuid_tag(target)),
                None => {
                    nbt.remove("Owner");
                }
            }
            if data.stack.is_empty() {
                nbt.remove("Item");
            } else {
                nbt.insert("Item", crate::inventory::stack_to_nbt(&data.stack));
            }
        } else if kind == ids::FALLING_BLOCK {
            let data = self.entities.falling_block(id)?;
            nbt.insert("BlockState", write_block_state(data.state));
            nbt.insert("Time", data.time);
            nbt.insert("DropItem", data.drop_item);
            // `#minecraft:anvil` hurts entities by default.
            let anvil = matches!(
                data.state.block().name(),
                "minecraft:anvil" | "minecraft:chipped_anvil" | "minecraft:damaged_anvil"
            );
            or_default(&mut nbt, "HurtEntities", Tag::from(anvil));
            or_default(&mut nbt, "FallHurtAmount", Tag::Float(0.0));
            or_default(&mut nbt, "FallHurtMax", Tag::Int(40));
            or_default(&mut nbt, "CancelDrop", Tag::Byte(0));
        } else {
            let data = self.entities.primed_tnt(id)?;
            nbt.insert("fuse", data.fuse as i16);
            nbt.insert("block_state", write_block_state(data.state));
            if data.explosion_power != 4.0 {
                nbt.insert("explosion_power", data.explosion_power);
            } else {
                nbt.remove("explosion_power");
            }
            if let Some(owner) = data.owner.and_then(|o| self.entities.store.get(o)) {
                nbt.insert("owner", uuid_tag(owner.uuid()));
            }
        }
        Some(nbt)
    }

    /// The leash holder of a mob or boat as stored: a knot by its block, else the holder's UUID.
    fn leash_of(&self, id: EntityId) -> Option<crate::mob::LeashRef> {
        let ents = &self.entities;
        let holder = ents.store.get(*ents.leashes.get(&id)?)?;
        if holder.kind() == ids::LEASH_KNOT
            && let Some(&pos) = ents.knots.get(holder.kind_row())
        {
            return Some(crate::mob::LeashRef::Knot(pos));
        }
        Some(crate::mob::LeashRef::Entity(holder.uuid()))
    }

    /// Removes the entities of a chunk that unloaded (they were written back before).
    pub(super) fn remove_entities(&mut self, chunk: ChunkPos) {
        let ids: Vec<EntityId> = self
            .entities
            .store
            .sections()
            .in_chunk(chunk.x, chunk.z)
            .filter(|&id| {
                self.entities
                    .store
                    .get(id)
                    .is_some_and(|e| e.kind() != ids::PLAYER)
            })
            .collect();
        for id in ids {
            self.entities.despawn(id);
            self.entity_nbt.remove(&id);
        }
        self.parked_entities.remove(&chunk);
        self.entity_chunks.remove(&chunk);
    }

    /// Loads the POI of a chunk that became live and checks them against its blocks
    /// (`PoiManager.checkConsistencyWithBlocks` of every section).
    pub(super) fn load_poi(&mut self, chunk: ChunkPos) {
        self.flush_poi_changes();
        let Some(c) = self.chunks.chunk(chunk) else {
            return;
        };
        let stored = c.poi.as_deref().and_then(|bytes| {
            match vanillite_nbt::read::read_network(&mut &bytes[..], &mut Budget::file()) {
                Ok(Some(Tag::Compound(root))) => Some(root),
                _ => {
                    log::warn!("Unreadable POI of chunk {}, {}", chunk.x, chunk.z);
                    None
                }
            }
        });
        let min = c.chunk.min_section();
        let blocks: Vec<SectionBlocks<'_>> = c
            .chunk
            .sections()
            .iter()
            .map(|s| SectionBlocks(s))
            .collect();
        let mut iter = blocks.iter().enumerate().map(|(i, s)| {
            #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
            let y = min + i as i32;
            (y, s as &dyn crate::poi::ChunkSectionBlocks)
        });
        self.entities.poi.load_chunk(chunk, stored, &mut iter);
    }

    /// Writes the POI of a live chunk back into its stored record if they changed.
    pub(super) fn write_poi(&mut self, chunk: ChunkPos) {
        if !self.entities.poi.is_dirty(chunk) {
            return;
        }
        let bytes = self.entities.poi.save_chunk(chunk).and_then(|root| {
            let mut bytes = Vec::new();
            vanillite_nbt::write::write_network_compound(&root, &mut bytes).ok()?;
            Some(Arc::<[u8]>::from(bytes))
        });
        self.chunks.set_stored_poi(chunk, bytes);
    }

    /// Writes the changed POI of every live chunk back (before a save).
    pub(super) fn write_all_poi(&mut self) {
        self.flush_poi_changes();
        for chunk in self.entities.poi.dirty_chunks() {
            self.write_poi(chunk);
        }
    }

    /// Writes the entities of every chunk that has or had entities back (before a save).
    pub(super) fn write_all_entities(&mut self) {
        let mut chunks: Vec<ChunkPos> = self.entity_chunks.iter().copied().collect();
        chunks.extend(
            self.entities
                .store
                .sections()
                .chunks()
                .map(|(x, z)| ChunkPos::new(x, z)),
        );
        chunks.sort_unstable_by_key(|c| (c.x, c.z));
        chunks.dedup();
        for chunk in chunks {
            if self.chunks.visibility(chunk) != Visibility::Hidden {
                self.write_entities(chunk);
            }
        }
        let store = &self.entities.store;
        self.entity_nbt.retain(|&id, _| store.contains(id));
    }
}

/// The fields of `Entity.load` the game models.
struct Base {
    pos: Vec3,
    motion: Vec3,
    rot: Rotation,
    on_ground: bool,
    no_gravity: bool,
    uuid: Option<u128>,
}

impl Base {
    fn load(nbt: &Compound) -> Base {
        let list = |name: &str| -> Vec<f64> {
            match nbt.get(name) {
                Some(Tag::List(list)) => list
                    .0
                    .iter()
                    .map(|t| match t {
                        Tag::Double(v) => *v,
                        Tag::Float(v) => f64::from(*v),
                        _ => 0.0,
                    })
                    .collect(),
                _ => Vec::new(),
            }
        };
        let vec = |v: Vec<f64>| {
            if v.len() == 3 {
                Vec3::new(v[0], v[1], v[2])
            } else {
                Vec3::ZERO
            }
        };
        let pos = vec(list("Pos"));
        let motion = vec(list("Motion"));
        // Motion above 10 blocks per tick is dropped per axis.
        let clamp = |v: f64| if v.abs() > 10.0 { 0.0 } else { v };
        let rotation = list("Rotation");
        let (y_rot, x_rot) = match rotation[..] {
            [y, x] => (y as f32, x as f32),
            _ => (0.0, 0.0),
        };
        let y_rot = if y_rot.is_finite() {
            y_rot % 360.0
        } else {
            0.0
        };
        let x_rot = if x_rot.is_finite() {
            x_rot.clamp(-90.0, 90.0) % 360.0
        } else {
            0.0
        };
        Base {
            pos: Vec3::new(
                pos.x.clamp(-3.000_051_2e7, 3.000_051_2e7),
                pos.y.clamp(-2.0e7, 2.0e7),
                pos.z.clamp(-3.000_051_2e7, 3.000_051_2e7),
            ),
            motion: Vec3::new(clamp(motion.x), clamp(motion.y), clamp(motion.z)),
            rot: Rotation {
                y_rot,
                x_rot,
                y_head_rot: y_rot,
            },
            on_ground: flag(nbt, "OnGround").unwrap_or(false),
            no_gravity: flag(nbt, "NoGravity").unwrap_or(false),
            uuid: uuid_of(nbt, "UUID"),
        }
    }
}

/// Inserts `value` unless the compound has `name`.
fn or_default(nbt: &mut Compound, name: &str, value: Tag) {
    if nbt.get(name).is_none() {
        nbt.insert(name, value);
    }
}

fn int(nbt: &Compound, name: &str) -> Option<i32> {
    nbt.get(name)
        .and_then(Tag::as_i64)
        .and_then(|v| i32::try_from(v).ok())
}

fn flag(nbt: &Compound, name: &str) -> Option<bool> {
    nbt.get(name).and_then(Tag::as_i64).map(|v| v != 0)
}

fn doubles(v: Vec3) -> List {
    List(vec![Tag::Double(v.x), Tag::Double(v.y), Tag::Double(v.z)])
}

/// `UUIDUtil.CODEC`: four ints, most significant first.
fn uuid_tag(uuid: u128) -> Tag {
    Tag::IntArray(
        (0..4)
            .map(|i| (uuid >> (96 - 32 * i)) as u32 as i32)
            .collect(),
    )
}

fn uuid_of(nbt: &Compound, name: &str) -> Option<u128> {
    match nbt.get(name) {
        Some(Tag::IntArray(v)) if v.len() == 4 => Some(
            v.iter()
                .fold(0u128, |acc, &part| acc << 32 | u128::from(part as u32)),
        ),
        _ => None,
    }
}

/// A chunk section as the POI manager scans it.
struct SectionBlocks<'a>(&'a vanillite_chunk::Section);

impl crate::poi::ChunkSectionBlocks for SectionBlocks<'_> {
    fn may_have_poi(&self) -> bool {
        self.0
            .blocks()
            .may_contain(|v| crate::poi::PoiType::has_poi(vanillite_data::block::BlockState(v)))
    }

    fn state(&self, x: u32, y: u32, z: u32) -> vanillite_data::block::BlockState {
        vanillite_data::block::BlockState(self.0.block(x, y, z))
    }
}

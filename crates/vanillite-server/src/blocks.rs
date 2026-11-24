//! The world as the player handlers see it ([`PlayLevel`] over the world's [`Level`]), sounds
//! and crack progress of the tick, and the item entity tick.

use std::collections::{HashMap, HashSet};

use vanillite_base::hash::FxHashSet;
use vanillite_base::random::{RandomSource, XoroshiroRandom};
use vanillite_base::{Aabb, BlockPos, Vec3};
use vanillite_data::block::BlockState;
use vanillite_data::entity::ids as entity_ids;
use vanillite_datapack::LoadedPacks;
use vanillite_game::entities::Entities;
use vanillite_game::item::{ItemStack, ToolTable};
use vanillite_game::item_entity;
use vanillite_game::level::Level;
use vanillite_game::loot::LootTables;
use vanillite_game::player::play::GameData;
use vanillite_game::player::world::{PlayLevel, SoundSource};
use vanillite_protocol::SharedPacket;
use vanillite_protocol::packets::play::clientbound::{BlockDestruction, Sound, SoundEvent};
use vanillite_protocol::types::BlockPos as WirePos;
use vanillite_world::chunks::Chunks;
use vanillite_world::entity::{EntityFlags, EntityId, EntityStore};

/// Builds the gameplay data from the loaded data packs and installs the registry sizes and
/// tags the protocol needs to validate client item stacks.
pub(crate) fn game_data(packs: Option<&LoadedPacks>) -> GameData {
    let Some(packs) = packs else {
        return GameData::default();
    };
    let tag_ids = |registry: &str, tag: &str| -> Vec<u16> {
        packs
            .tag_ids(registry, tag)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|i| u16::try_from(i).ok())
            .collect()
    };
    let tools = ToolTable::new(&|tag| tag_ids("minecraft:block", tag));
    let loot = LootTables::load(packs.roots());
    log::info!("Loaded {} block loot tables", loot.len());
    let enchantments = packs
        .registry("minecraft:enchantment")
        .map(|r| {
            r.entries()
                .iter()
                .zip(0u32..)
                .map(|(e, i)| (e.id.clone(), i))
                .collect()
        })
        .unwrap_or_default();
    let item_tags: HashMap<String, Vec<u16>> = packs
        .tags("minecraft:item")
        .map(|tags| {
            tags.keys()
                .map(|t| (t.clone(), tag_ids("minecraft:item", t)))
                .collect()
        })
        .unwrap_or_default();
    let mut info = vanillite_protocol::item::RegistryInfo::default();
    for r in packs.registries() {
        info.sizes.insert(r.name.to_owned(), r.entries().len());
    }
    for registry in packs.tagged_registries() {
        let tags: HashSet<String> = packs
            .tags(registry)
            .map(|t| t.keys().cloned().collect())
            .unwrap_or_default();
        info.tags.insert(registry.to_owned(), tags);
    }
    vanillite_protocol::item::install_registries(info);
    let enchantment_info = packs
        .registry("minecraft:enchantment")
        .map(|r| {
            r.entries()
                .iter()
                .filter_map(|e| Some((e.id.clone(), enchantment_info(packs, e.json.get())?)))
                .collect()
        })
        .unwrap_or_default();
    GameData {
        tools,
        loot,
        enchantments,
        item_tags,
        enchantment_info,
    }
}

/// The entries of a holder set in JSON (`"#tag"`, an id or a list of ids) of `registry`.
fn holder_set(
    packs: &LoadedPacks,
    registry: &str,
    json: Option<&serde_json::Value>,
) -> Vec<String> {
    match json {
        Some(serde_json::Value::String(s)) => match s.strip_prefix('#') {
            Some(tag) => packs
                .tags(registry)
                .and_then(|t| t.get(tag))
                .cloned()
                .unwrap_or_default(),
            None => vec![s.clone()],
        },
        Some(serde_json::Value::Array(list)) => list
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    }
}

/// What `/enchant` reads of an enchantment definition.
fn enchantment_info(
    packs: &LoadedPacks,
    json: &str,
) -> Option<vanillite_game::player::play::EnchantmentInfo> {
    let json: serde_json::Value = serde_json::from_str(json).ok()?;
    let supported_items = holder_set(packs, "minecraft:item", json.get("supported_items"))
        .iter()
        .filter_map(|name| vanillite_data::registry::ITEM.id(name))
        .collect();
    Some(vanillite_game::player::play::EnchantmentInfo {
        description: vanillite_protocol::text::Component::try_from_json(json.get("description")?)
            .ok()?,
        max_level: json
            .get("max_level")
            .and_then(serde_json::Value::as_i64)
            .and_then(|v| i32::try_from(v).ok())?,
        supported_items,
        exclusive_set: holder_set(packs, "minecraft:enchantment", json.get("exclusive_set")),
    })
}

/// A sound to send.
#[derive(Clone, Debug)]
struct SoundEffect {
    except: Option<i32>,
    pos: Vec3,
    sound: u16,
    source: SoundSource,
    volume: f32,
    pitch: f32,
    seed: i64,
}

/// Sounds and crack progress of the player handlers, sent once the players are back in the
/// level. Block changes and level events go through the level's output.
#[derive(Debug, Default)]
pub(crate) struct LevelEffects {
    progress: Vec<(i32, BlockPos, i32)>,
    sounds: Vec<SoundEffect>,
}

/// [`PlayLevel`] over a world's [`Level`] while its players are handled.
pub(crate) struct WorldLevel<'a> {
    pub(crate) level: &'a mut Level,
    pub(crate) effects: &'a mut LevelEffects,
    /// Players in spectator mode (they never block building).
    pub(crate) spectators: &'a FxHashSet<EntityId>,
    /// The acting player with its current box (its stored position may lag behind).
    pub(crate) actor: Option<(EntityId, Aabb)>,
}

impl PlayLevel for WorldLevel<'_> {
    fn block(&self, pos: BlockPos) -> BlockState {
        self.level.block(pos)
    }

    fn is_loaded(&self, pos: BlockPos) -> bool {
        self.level.is_loaded(pos)
    }

    fn min_y(&self) -> i32 {
        self.level.min_y()
    }

    fn max_y(&self) -> i32 {
        self.level.max_y()
    }

    fn set_block(&mut self, pos: BlockPos, state: BlockState, update_flags: u32) -> bool {
        self.level.set_block(pos, state, update_flags)
    }

    fn level_event(&mut self, except: Option<i32>, event: i32, pos: BlockPos, data: i32) {
        // The handlers only ever exclude the acting player.
        let source = except.and(self.actor.map(|(id, _)| id));
        self.level.level_event(source, event, pos, data);
    }

    fn destroy_progress(&mut self, breaker: i32, pos: BlockPos, progress: i32) {
        self.effects.progress.push((breaker, pos, progress));
    }

    fn play_sound(
        &mut self,
        except: Option<i32>,
        pos: Vec3,
        sound: u16,
        source: SoundSource,
        volume: f32,
        pitch: f32,
    ) {
        let seed = self.level.random().next_long();
        self.effects.sounds.push(SoundEffect {
            except,
            pos,
            sound,
            source,
            volume,
            pitch,
            seed,
        });
    }

    fn spawn_item(
        &mut self,
        pos: Vec3,
        stack: ItemStack,
        velocity: Vec3,
        pickup_delay: i16,
        thrower: Option<u128>,
    ) {
        vanillite_game::block::drops::spawn_item(
            self.level,
            pos,
            stack,
            velocity,
            pickup_delay,
            thrower,
        );
    }

    fn is_unobstructed(&self, state: BlockState, pos: BlockPos) -> bool {
        let (dx, dy, dz) = (f64::from(pos.x), f64::from(pos.y), f64::from(pos.z));
        let store = &self.level.entities.store;
        for b in state.collision_shape() {
            let shape = Aabb::new(
                b[0] + dx,
                b[1] + dy,
                b[2] + dz,
                b[3] + dx,
                b[4] + dy,
                b[5] + dz,
            );
            if let Some((_, own)) = self.actor
                && own.intersects(&shape)
            {
                return false;
            }
            let mut blocked = false;
            store.for_each_in(&shape, true, |id| {
                let is_actor = self.actor.is_some_and(|(a, _)| a == id);
                if !is_actor && !self.spectators.contains(&id) {
                    blocked = true;
                }
            });
            if blocked {
                return false;
            }
        }
        true
    }

    fn random_float(&mut self) -> f32 {
        self.level.random().next_float()
    }

    fn random_double(&mut self) -> f64 {
        self.level.random().next_double()
    }

    fn random_boolean(&mut self) -> bool {
        self.level.random().next_boolean()
    }

    fn random_int(&mut self, bound: i32) -> i32 {
        self.level.random().next_int_bounded(bound.max(1))
    }
}

/// Players near `pos` within `range` blocks (by their feet position), except `except`.
fn players_near(
    store: &EntityStore,
    pos: Vec3,
    range: f64,
    except: Option<i32>,
    mut f: impl FnMut(EntityId),
) {
    let area = Aabb::new(
        pos.x - range,
        pos.y - range - 2.0,
        pos.z - range,
        pos.x + range,
        pos.y + range,
        pos.z + range,
    );
    store.for_each_in(&area, true, |id| {
        let Some(e) = store.get(id) else {
            return;
        };
        if except.is_some_and(|n| n == e.network_id().0) {
            return;
        }
        let p = e.pos();
        let (dx, dy, dz) = (p.x - pos.x, p.y - pos.y, p.z - pos.z);
        if dx * dx + dy * dy + dz * dz < range * range {
            f(id);
        }
    });
}

fn wire(pos: BlockPos) -> WirePos {
    WirePos {
        x: pos.x,
        y: pos.y,
        z: pos.z,
    }
}

fn center(pos: BlockPos) -> Vec3 {
    Vec3::new(
        f64::from(pos.x) + 0.5,
        f64::from(pos.y) + 0.5,
        f64::from(pos.z) + 0.5,
    )
}

impl LevelEffects {
    /// Sends the sounds and crack progress collected while the players were handled.
    pub(crate) fn flush(&mut self, entities: &Entities) {
        let store = &entities.store;
        for (breaker, pos, progress) in self.progress.drain(..) {
            let packet = SharedPacket::of(&BlockDestruction {
                id: breaker,
                pos: wire(pos),
                progress: u8::try_from(progress).unwrap_or(255),
            });
            players_near(store, center(pos), 32.0, Some(breaker), |id| {
                if let Some(p) = entities.player(id) {
                    p.connection.sink.send_shared(&packet);
                }
            });
        }
        for s in self.sounds.drain(..) {
            let packet = SharedPacket::of(&Sound::at(
                SoundEvent(i32::from(s.sound)),
                s.source,
                s.pos.x,
                s.pos.y,
                s.pos.z,
                s.volume,
                s.pitch,
                s.seed,
            ));
            let range = if s.volume > 1.0 {
                16.0 * f64::from(s.volume)
            } else {
                16.0
            };
            players_near(store, s.pos, range, s.except, |id| {
                if let Some(p) = entities.player(id) {
                    p.connection.sink.send_shared(&packet);
                }
            });
        }
    }
}

/// A random version 4 UUID (`Mth.createInsecureUUID`).
pub(crate) fn is_item(store: &EntityStore, id: EntityId) -> bool {
    store.get(id).is_some_and(|e| e.kind() == entity_ids::ITEM)
}

/// `ItemEntity.tick` for every item entity of a world: movement, merging with neighbours and
/// despawning.
pub(crate) fn tick_items(
    entities: &mut Entities,
    chunks: &Chunks,
    random: &mut XoroshiroRandom,
    min_y: i32,
    lifetime: i16,
) {
    let ids: Vec<EntityId> = entities.items.iter().map(|(id, _)| id).collect();
    if ids.is_empty() {
        return;
    }
    let block = |p: BlockPos| BlockState(chunks.block(p).unwrap_or(0));
    let mut scratch = Vec::new();
    let mut gone = Vec::new();
    let mut near = Vec::new();
    for id in ids {
        let Entities { store, items, .. } = entities;
        let Some(entity) = store.get(id) else {
            continue;
        };
        let row = entity.kind_row();
        let input = item_entity::TickInput {
            pos: entity.pos(),
            velocity: entity.velocity(),
            on_ground: entity.flags().contains(EntityFlags::ON_GROUND),
            network_id: entity.network_id().0,
            min_y,
            lifetime,
        };
        let Some(data) = items.get_mut(row) else {
            continue;
        };
        let ticked = item_entity::tick(
            data,
            input,
            &block,
            &mut || random.next_float(),
            &mut scratch,
        );
        if ticked.discard {
            gone.push(id);
            continue;
        }
        let rate = if ticked.moved_block { 2 } else { 40 };
        let merge = data.tick_count.is_multiple_of(rate) && data.is_mergable(lifetime);
        if let Some(mut e) = store.get_mut(id) {
            e.set_pos(ticked.pos);
            e.set_velocity(ticked.velocity);
            e.set_flags(EntityFlags::ON_GROUND, ticked.on_ground);
            if ticked.needs_sync {
                e.set_flags(EntityFlags::HAS_IMPULSE, true);
            }
        }
        if merge {
            merge_with_neighbours(entities, id, lifetime, &mut near, &mut gone);
        }
    }
    for id in gone {
        entities.despawn(id);
    }
}

/// `ItemEntity.mergeWithNeighbours`.
fn merge_with_neighbours(
    entities: &mut Entities,
    id: EntityId,
    lifetime: i16,
    near: &mut Vec<EntityId>,
    gone: &mut Vec<EntityId>,
) {
    let Some(aabb) = entities.store.get(id).map(|e| e.aabb()) else {
        return;
    };
    near.clear();
    entities
        .store
        .for_each_in(&aabb.inflate(0.5, 0.0, 0.5), false, |other| {
            if other != id {
                near.push(other);
            }
        });
    for &other in near.iter() {
        if gone.contains(&other) {
            continue;
        }
        let (Some(this), Some(that)) = (entities.item(id), entities.item(other)) else {
            continue;
        };
        if !that.is_mergable(lifetime) || !this.is_mergable(lifetime) {
            continue;
        }
        // `tryToMerge`.
        if this.target != that.target || !item_entity::are_mergable(&this.stack, &that.stack) {
            continue;
        }
        let (to, from) = if that.stack.count < this.stack.count {
            (id, other)
        } else {
            (other, id)
        };
        let Some(mut from_data) = entities.item(from).cloned() else {
            continue;
        };
        let Some(to_data) = entities.item_mut(to) else {
            continue;
        };
        to_data.stack = item_entity::merge_stacks(&to_data.stack, &mut from_data.stack);
        to_data.pickup_delay = to_data.pickup_delay.max(from_data.pickup_delay);
        to_data.age = to_data.age.min(from_data.age);
        if let Some(mut e) = entities.store.get_mut(to) {
            e.mark_data_dirty();
        }
        if let Some(from_item) = entities.item_mut(from) {
            from_item.stack = from_data.stack.clone();
        }
        if from_data.stack.is_empty() {
            gone.push(from);
            if from == id {
                break;
            }
        } else if let Some(mut e) = entities.store.get_mut(from) {
            e.mark_data_dirty();
        }
    }
}

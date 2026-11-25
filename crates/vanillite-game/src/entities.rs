//! The entities of one world: the core store plus one kind store per entity kind.

use vanillite_base::{BlockPos, Vec3};
use vanillite_data::block::BlockState;
use vanillite_data::entity::{EntityTypeId, ids};
use vanillite_world::entity::{
    Dense, EntityId, EntityStore, NetworkId, NetworkIds, NewEntity, Removed, Rotation,
};

use crate::experience_orb::ExperienceOrbData;
use vanillite_base::hash::FxHashMap;

use crate::item_entity::ItemData;
use crate::mob::MobData;
use crate::mob::projectile::ArrowData;
use crate::player::PlayerData;

/// The kind data of a `falling_block` (`FallingBlockEntity`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FallingBlockData {
    /// The falling block.
    pub state: BlockState,
    /// Ticks since it started falling.
    pub time: i32,
    /// Whether it drops as an item where it cannot land (`dropItem`).
    pub drop_item: bool,
    /// Where it started falling (`DATA_START_POS`).
    pub start: BlockPos,
}

/// The kind data of a `tnt` entity (`PrimedTnt`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PrimedTntData {
    /// Ticks until it explodes (`DATA_FUSE_ID`).
    pub fuse: i32,
    /// The block it shows (`DATA_BLOCK_STATE_ID`).
    pub state: BlockState,
    /// The entity that primed it (`owner`).
    pub owner: Option<EntityId>,
    /// `explosionPower`.
    pub explosion_power: f32,
}

/// All entities of a world. Kinds without own data yet only live in the core store.
#[derive(Debug, Default)]
pub struct Entities {
    /// Core columns, tick order and section index.
    pub store: EntityStore,
    /// Player kind data.
    pub players: Dense<PlayerData>,
    /// Item entity kind data.
    pub items: Dense<ItemData>,
    /// Falling block kind data.
    pub falling_blocks: Dense<FallingBlockData>,
    /// Mob kind data (every type with a [`crate::mob::Kind`]).
    pub mobs: Dense<MobData>,
    /// Arrow kind data.
    pub arrows: Dense<ArrowData>,
    /// Network id → entity of mobs (attack and interaction packets name their target by it).
    pub by_network_id: FxHashMap<i32, EntityId>,
    /// Primed TNT kind data.
    pub primed_tnts: Dense<PrimedTntData>,
    /// Experience orb kind data.
    pub experience_orbs: Dense<ExperienceOrbData>,
}

impl Entities {
    /// No entities.
    pub fn new() -> Entities {
        Entities::default()
    }

    /// Adds a player entity.
    pub fn spawn_player(
        &mut self,
        network_ids: &NetworkIds,
        uuid: u128,
        pos: Vec3,
        rot: Rotation,
        data: PlayerData,
    ) -> EntityId {
        self.spawn_player_as(network_ids.next(), uuid, pos, rot, data)
    }

    /// Adds a player entity with a given network id (a respawned player keeps its id).
    pub fn spawn_player_as(
        &mut self,
        network_id: vanillite_world::entity::NetworkId,
        uuid: u128,
        pos: Vec3,
        rot: Rotation,
        data: PlayerData,
    ) -> EntityId {
        let mut new = NewEntity::new(ids::PLAYER, network_id, uuid, pos);
        new.rot = rot;
        let players = &mut self.players;
        self.store.spawn(new, |id| players.push(id, data))
    }

    /// Adds an item entity (`new ItemEntity`): a random yaw like Vanilla's constructor.
    pub fn spawn_item(
        &mut self,
        network_id: NetworkId,
        uuid: u128,
        pos: Vec3,
        velocity: Vec3,
        y_rot: f32,
        data: ItemData,
    ) -> EntityId {
        let mut new = NewEntity::new(ids::ITEM, network_id, uuid, pos);
        new.velocity = velocity;
        new.rot = Rotation {
            y_rot,
            x_rot: 0.0,
            y_head_rot: y_rot,
        };
        let items = &mut self.items;
        self.store.spawn(new, |id| items.push(id, data))
    }

    /// Adds a falling block entity.
    pub fn spawn_falling_block(&mut self, new: NewEntity, data: FallingBlockData) -> EntityId {
        let falling_blocks = &mut self.falling_blocks;
        self.store.spawn(new, |id| falling_blocks.push(id, data))
    }

    /// The falling block data of an entity.
    pub fn falling_block(&self, id: EntityId) -> Option<&FallingBlockData> {
        let entity = self.store.get(id)?;
        (entity.kind() == ids::FALLING_BLOCK)
            .then(|| self.falling_blocks.get(entity.kind_row()))
            .flatten()
    }

    /// The falling block data of an entity, mutably.
    pub fn falling_block_mut(&mut self, id: EntityId) -> Option<&mut FallingBlockData> {
        let entity = self.store.get(id)?;
        if entity.kind() != ids::FALLING_BLOCK {
            return None;
        }
        let row = entity.kind_row();
        self.falling_blocks.get_mut(row)
    }

    /// Adds a mob.
    pub fn spawn_mob(&mut self, new: NewEntity, data: MobData) -> EntityId {
        let network_id = new.network_id.0;
        let mobs = &mut self.mobs;
        let id = self.store.spawn(new, |id| mobs.push(id, data));
        self.by_network_id.insert(network_id, id);
        id
    }

    /// Adds an arrow.
    pub fn spawn_arrow(&mut self, new: NewEntity, data: ArrowData) -> EntityId {
        let arrows = &mut self.arrows;
        self.store.spawn(new, |id| arrows.push(id, data))
    }

    /// The mob data of an entity.
    pub fn mob(&self, id: EntityId) -> Option<&MobData> {
        let entity = self.store.get(id)?;
        if !is_mob_kind(entity.kind()) {
            return None;
        }
        self.mobs.get(entity.kind_row())
    }

    /// The mob data of an entity, mutably.
    pub fn mob_mut(&mut self, id: EntityId) -> Option<&mut MobData> {
        let entity = self.store.get(id)?;
        if !is_mob_kind(entity.kind()) {
            return None;
        }
        let row = entity.kind_row();
        self.mobs.get_mut(row)
    }

    /// The arrow data of an entity.
    pub fn arrow(&self, id: EntityId) -> Option<&ArrowData> {
        let entity = self.store.get(id)?;
        (entity.kind() == ids::ARROW)
            .then(|| self.arrows.get(entity.kind_row()))
            .flatten()
    }

    /// The arrow data of an entity, mutably.
    pub fn arrow_mut(&mut self, id: EntityId) -> Option<&mut ArrowData> {
        let entity = self.store.get(id)?;
        if entity.kind() != ids::ARROW {
            return None;
        }
        let row = entity.kind_row();
        self.arrows.get_mut(row)
    }

    /// The entity with a network id, for mobs.
    pub fn by_network_id(&self, network_id: i32) -> Option<EntityId> {
        self.by_network_id
            .get(&network_id)
            .copied()
            .filter(|&id| self.store.contains(id))
    }

    /// Adds a primed TNT entity.
    pub fn spawn_primed_tnt(&mut self, new: NewEntity, data: PrimedTntData) -> EntityId {
        let primed_tnts = &mut self.primed_tnts;
        self.store.spawn(new, |id| primed_tnts.push(id, data))
    }

    /// The primed TNT data of an entity.
    pub fn primed_tnt(&self, id: EntityId) -> Option<&PrimedTntData> {
        let entity = self.store.get(id)?;
        (entity.kind() == ids::TNT)
            .then(|| self.primed_tnts.get(entity.kind_row()))
            .flatten()
    }

    /// The primed TNT data of an entity, mutably.
    pub fn primed_tnt_mut(&mut self, id: EntityId) -> Option<&mut PrimedTntData> {
        let entity = self.store.get(id)?;
        if entity.kind() != ids::TNT {
            return None;
        }
        let row = entity.kind_row();
        self.primed_tnts.get_mut(row)
    }

    /// Adds an experience orb.
    pub fn spawn_experience_orb(&mut self, new: NewEntity, data: ExperienceOrbData) -> EntityId {
        let orbs = &mut self.experience_orbs;
        self.store.spawn(new, |id| orbs.push(id, data))
    }

    /// The experience orb data of an entity.
    pub fn experience_orb(&self, id: EntityId) -> Option<&ExperienceOrbData> {
        let entity = self.store.get(id)?;
        (entity.kind() == ids::EXPERIENCE_ORB)
            .then(|| self.experience_orbs.get(entity.kind_row()))
            .flatten()
    }

    /// The experience orb data of an entity, mutably.
    pub fn experience_orb_mut(&mut self, id: EntityId) -> Option<&mut ExperienceOrbData> {
        let entity = self.store.get(id)?;
        if entity.kind() != ids::EXPERIENCE_ORB {
            return None;
        }
        let row = entity.kind_row();
        self.experience_orbs.get_mut(row)
    }

    /// Removes an entity of any kind together with its kind data.
    pub fn despawn(&mut self, id: EntityId) -> Option<Removed> {
        let removed = self.store.despawn(id)?;
        if is_mob_kind(removed.kind) {
            self.by_network_id.remove(&removed.network_id.0);
        }
        let moved = self.remove_kind_row(removed.kind, removed.kind_row);
        if let Some(moved) = moved {
            self.store.set_kind_row(moved, removed.kind_row);
        }
        Some(removed)
    }

    /// Removes a player entity and returns its player data (respawn moves it to a new entity).
    pub fn despawn_player(&mut self, id: EntityId) -> Option<(Removed, PlayerData)> {
        let entity = self.store.get(id)?;
        if entity.kind() != ids::PLAYER {
            return None;
        }
        let removed = self.store.despawn(id)?;
        let (data, moved) = self.players.swap_remove(removed.kind_row)?;
        if let Some(moved) = moved {
            self.store.set_kind_row(moved, removed.kind_row);
        }
        Some((removed, data))
    }

    /// Removes a kind store row; returns the entity that moved into it.
    fn remove_kind_row(&mut self, kind: EntityTypeId, row: u32) -> Option<EntityId> {
        if kind == ids::PLAYER {
            self.players.swap_remove(row).and_then(|(_, moved)| moved)
        } else if kind == ids::ITEM {
            self.items.swap_remove(row).and_then(|(_, moved)| moved)
        } else if kind == ids::FALLING_BLOCK {
            self.falling_blocks
                .swap_remove(row)
                .and_then(|(_, moved)| moved)
        } else if kind == ids::ARROW {
            self.arrows.swap_remove(row).and_then(|(_, moved)| moved)
        } else if is_mob_kind(kind) {
            self.mobs.swap_remove(row).and_then(|(_, moved)| moved)
        } else if kind == ids::TNT {
            self.primed_tnts
                .swap_remove(row)
                .and_then(|(_, moved)| moved)
        } else if kind == ids::EXPERIENCE_ORB {
            self.experience_orbs
                .swap_remove(row)
                .and_then(|(_, moved)| moved)
        } else {
            None
        }
    }

    /// The player data of an entity, `None` if it is no (live) player.
    pub fn player(&self, id: EntityId) -> Option<&PlayerData> {
        let entity = self.store.get(id)?;
        (entity.kind() == ids::PLAYER)
            .then(|| self.players.get(entity.kind_row()))
            .flatten()
    }

    /// The player data of an entity, mutably.
    pub fn player_mut(&mut self, id: EntityId) -> Option<&mut PlayerData> {
        let entity = self.store.get(id)?;
        if entity.kind() != ids::PLAYER {
            return None;
        }
        let row = entity.kind_row();
        self.players.get_mut(row)
    }
}

/// Whether entities of `kind` keep their data in the mob store.
#[inline]
pub fn is_mob_kind(kind: EntityTypeId) -> bool {
    crate::mob::Kind::of(kind).is_some()
}

/// `tickNonPassenger` of the non-player entity kinds that tick: the one dispatch the world tick
/// and the scenario world share.
pub fn tick_entity(level: &mut crate::level::Level, id: EntityId, kind: EntityTypeId) {
    if kind == ids::FALLING_BLOCK {
        crate::block::falling::tick_entity(level, id);
    } else if kind == ids::TNT {
        crate::block::tnt::tick_entity(level, id);
    } else if kind == ids::EXPERIENCE_ORB {
        crate::experience_orb::tick_entity(level, id);
    }
}

impl Entities {
    /// The item data of an entity, `None` if it is no (live) item entity.
    pub fn item(&self, id: EntityId) -> Option<&ItemData> {
        let entity = self.store.get(id)?;
        (entity.kind() == ids::ITEM)
            .then(|| self.items.get(entity.kind_row()))
            .flatten()
    }

    /// The item data of an entity, mutably.
    pub fn item_mut(&mut self, id: EntityId) -> Option<&mut ItemData> {
        let entity = self.store.get(id)?;
        if entity.kind() != ids::ITEM {
            return None;
        }
        let row = entity.kind_row();
        self.items.get_mut(row)
    }
}

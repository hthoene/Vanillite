//! The entities of one world: the core store plus one kind store per entity kind.

use vanillite_base::{BlockPos, Vec3};
use vanillite_data::block::BlockState;
use vanillite_data::entity::{EntityTypeId, ids};
use vanillite_world::entity::{
    Dense, EntityId, EntityStore, NetworkId, NetworkIds, NewEntity, Removed, Rotation,
};

use crate::item_entity::ItemData;
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
    /// Primed TNT kind data.
    pub primed_tnts: Dense<PrimedTntData>,
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

    /// Removes an entity of any kind together with its kind data.
    pub fn despawn(&mut self, id: EntityId) -> Option<Removed> {
        let removed = self.store.despawn(id)?;
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
        } else if kind == ids::TNT {
            self.primed_tnts
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

/// The tick loop's check of an entity in the tick order (`ServerLevel.tick`): players always
/// tick, other entities only in entity ticking range (`inEntityTickingRange`).
pub fn in_ticking_range(level: &crate::level::Level, id: EntityId) -> bool {
    level.entities.store.get(id).is_some_and(|entity| {
        entity.kind() == ids::PLAYER || {
            let pos = entity.pos();
            let chunk = vanillite_base::ChunkPos::new(
                vanillite_base::math::floor(pos.x) >> 4,
                vanillite_base::math::floor(pos.z) >> 4,
            );
            level.chunks.in_entity_ticking_range(chunk)
        }
    })
}

/// `tickNonPassenger` of the non-player entity kinds that tick: the one dispatch the world tick
/// and the scenario world share.
pub fn tick_entity(level: &mut crate::level::Level, id: EntityId, kind: EntityTypeId) {
    if kind == ids::FALLING_BLOCK {
        crate::block::falling::tick_entity(level, id);
    } else if kind == ids::TNT {
        crate::block::tnt::tick_entity(level, id);
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

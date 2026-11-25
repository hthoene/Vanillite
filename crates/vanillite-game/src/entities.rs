//! The entities of one world: the core store plus one kind store per entity kind.

use vanillite_base::{BlockPos, Vec3};
use vanillite_data::block::BlockState;
use vanillite_data::entity::{EntityTypeId, ids};
use vanillite_world::entity::{
    Dense, EntityId, EntityStore, NetworkId, NetworkIds, NewEntity, Removed, Rotation,
};

use vanillite_base::hash::FxHashMap;

use crate::item_entity::ItemData;
use crate::mob::MobData;
use crate::mob::boss::special::{SpecialData, is_special_kind};
use crate::mob::projectile::{ArrowData, OrbData};
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
    /// Experience orb kind data.
    pub orbs: Dense<OrbData>,
    /// Network id → entity of mobs (attack and interaction packets name their target by it).
    pub by_network_id: FxHashMap<i32, EntityId>,
    /// Primed TNT kind data.
    pub primed_tnts: Dense<PrimedTntData>,
    /// Vehicles and their passengers.
    pub riding: Riding,
    /// Player entities by UUID (owners and leash holders are found by it).
    pub players_by_uuid: FxHashMap<u128, EntityId>,
    /// Leashed mobs and what holds them (`leashHolder`): a player, mob or leash knot.
    pub leashes: FxHashMap<EntityId, EntityId>,
    /// Boat kind data (every type of [`crate::vehicle::BoatKind`]).
    pub boats: Dense<crate::vehicle::BoatData>,
    /// Leash knot kind data (their position block).
    pub knots: Dense<BlockPos>,
    /// Projectile kind data besides arrows (every type of [`crate::mob::missile::MissileKind`]).
    pub missiles: Dense<crate::mob::missile::MissileData>,
    /// Kind data of the bosses' non-living entities (skulls, crystals, fireballs, clouds).
    pub specials: Dense<SpecialData>,
}

/// Who rides what (`Entity.vehicle`, `Entity.passengers`).
#[derive(Debug, Default)]
pub struct Riding {
    vehicle: FxHashMap<EntityId, EntityId>,
    passengers: FxHashMap<EntityId, Vec<EntityId>>,
}

impl Riding {
    /// The vehicle `id` rides.
    pub fn vehicle_of(&self, id: EntityId) -> Option<EntityId> {
        self.vehicle.get(&id).copied()
    }

    /// The passengers of `id` in order (the first one controls).
    pub fn passengers_of(&self, id: EntityId) -> &[EntityId] {
        self.passengers.get(&id).map_or(&[], Vec::as_slice)
    }

    /// Adds `passenger` to `vehicle` (`addPassenger`); players come first.
    pub fn add(&mut self, passenger: EntityId, vehicle: EntityId, is_player: bool) {
        self.vehicle.insert(passenger, vehicle);
        let list = self.passengers.entry(vehicle).or_default();
        if is_player {
            list.insert(0, passenger);
        } else {
            list.push(passenger);
        }
    }

    /// Removes `passenger` from its vehicle (`removeVehicle`); returns the vehicle.
    pub fn remove(&mut self, passenger: EntityId) -> Option<EntityId> {
        let vehicle = self.vehicle.remove(&passenger)?;
        if let Some(list) = self.passengers.get_mut(&vehicle) {
            list.retain(|&p| p != passenger);
            if list.is_empty() {
                self.passengers.remove(&vehicle);
            }
        }
        Some(vehicle)
    }

    /// The vehicles that have passengers.
    pub fn vehicles(&self) -> Vec<EntityId> {
        self.passengers.keys().copied().collect()
    }

    /// Whether anything rides anything.
    pub fn is_empty(&self) -> bool {
        self.vehicle.is_empty()
    }
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
        let id = self.store.spawn(new, |id| players.push(id, data));
        self.by_network_id.insert(network_id.0, id);
        self.players_by_uuid.insert(uuid, id);
        id
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
        let parts = if data.kind == crate::mob::Kind::EnderDragon {
            8
        } else {
            0
        };
        let mobs = &mut self.mobs;
        let id = self.store.spawn(new, |id| mobs.push(id, data));
        self.by_network_id.insert(network_id, id);
        for i in 1..=parts {
            self.by_network_id.insert(network_id.wrapping_add(i), id);
        }
        id
    }

    /// Adds an arrow.
    pub fn spawn_arrow(&mut self, new: NewEntity, data: ArrowData) -> EntityId {
        let arrows = &mut self.arrows;
        self.store.spawn(new, |id| arrows.push(id, data))
    }

    /// Adds an experience orb.
    pub fn spawn_orb(&mut self, new: NewEntity, data: OrbData) -> EntityId {
        let orbs = &mut self.orbs;
        self.store.spawn(new, |id| orbs.push(id, data))
    }

    /// Adds a projectile of the missile store.
    pub fn spawn_missile(
        &mut self,
        new: NewEntity,
        data: crate::mob::missile::MissileData,
    ) -> EntityId {
        let missiles = &mut self.missiles;
        self.store.spawn(new, |id| missiles.push(id, data))
    }

    /// The missile data of an entity.
    pub fn missile(&self, id: EntityId) -> Option<&crate::mob::missile::MissileData> {
        let entity = self.store.get(id)?;
        if !crate::mob::missile::is_missile_kind(entity.kind()) {
            return None;
        }
        self.missiles.get(entity.kind_row())
    }

    /// The missile data of an entity, mutably.
    pub fn missile_mut(&mut self, id: EntityId) -> Option<&mut crate::mob::missile::MissileData> {
        let entity = self.store.get(id)?;
        if !crate::mob::missile::is_missile_kind(entity.kind()) {
            return None;
        }
        let row = entity.kind_row();
        self.missiles.get_mut(row)
    }

    /// Adds a special entity (see [`crate::mob::boss::special`]).
    pub fn spawn_special(&mut self, new: NewEntity, data: SpecialData) -> EntityId {
        let network_id = new.network_id.0;
        let specials = &mut self.specials;
        let id = self.store.spawn(new, |id| specials.push(id, data));
        // End crystals are attacked by players.
        self.by_network_id.insert(network_id, id);
        id
    }

    /// The special data of an entity.
    pub fn special(&self, id: EntityId) -> Option<&SpecialData> {
        let entity = self.store.get(id)?;
        if !is_special_kind(entity.kind()) {
            return None;
        }
        self.specials.get(entity.kind_row())
    }

    /// The special data of an entity, mutably.
    pub fn special_mut(&mut self, id: EntityId) -> Option<&mut SpecialData> {
        let entity = self.store.get(id)?;
        if !is_special_kind(entity.kind()) {
            return None;
        }
        let row = entity.kind_row();
        self.specials.get_mut(row)
    }

    /// `addEffect` on a living entity (player or mob): merges it and, for players, tells the
    /// client. `can_be_affected` is the target's `canBeAffected` verdict (checked by the
    /// caller for kinds with immunities). Returns whether it was added or changed.
    pub fn add_effect(
        &mut self,
        id: EntityId,
        effect: crate::effect::EffectInstance,
        can_be_affected: bool,
    ) -> bool {
        if !can_be_affected {
            return false;
        }
        if let Some(m) = self.mob_mut(id) {
            return crate::effect::add(m, effect);
        }
        let Entities { store, players, .. } = self;
        let Some(entity) = store.get_mut(id) else {
            return false;
        };
        if entity.as_ref().kind() != ids::PLAYER {
            return false;
        }
        let row = entity.as_ref().kind_row();
        let Some(p) = players.get_mut(row) else {
            return false;
        };
        crate::survival::player::add_effect(&entity, p, effect)
    }

    /// The active effects of a living entity (player or mob).
    pub fn effects(&self, id: EntityId) -> Option<&crate::effect::MobEffects> {
        if let Some(m) = self.mob(id) {
            return Some(&m.effects);
        }
        self.player(id).map(|p| &p.survival.effects)
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

    /// The experience orb data of an entity.
    pub fn orb(&self, id: EntityId) -> Option<&OrbData> {
        let entity = self.store.get(id)?;
        (entity.kind() == ids::EXPERIENCE_ORB)
            .then(|| self.orbs.get(entity.kind_row()))
            .flatten()
    }

    /// The experience orb data of an entity, mutably.
    pub fn orb_mut(&mut self, id: EntityId) -> Option<&mut OrbData> {
        let entity = self.store.get(id)?;
        if entity.kind() != ids::EXPERIENCE_ORB {
            return None;
        }
        let row = entity.kind_row();
        self.orbs.get_mut(row)
    }

    /// The entity with a network id: mobs, players, boats and leash knots.
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

    /// Removes an entity of any kind together with its kind data.
    pub fn despawn(&mut self, id: EntityId) -> Option<Removed> {
        self.unlink(id);
        let removed = self.store.despawn(id)?;
        if self.by_network_id.get(&removed.network_id.0) == Some(&id) {
            self.by_network_id.remove(&removed.network_id.0);
            if removed.kind == ids::ENDER_DRAGON {
                for i in 1..=8 {
                    self.by_network_id
                        .remove(&removed.network_id.0.wrapping_add(i));
                }
            }
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
        let uuid = entity.uuid();
        self.unlink(id);
        let removed = self.store.despawn(id)?;
        if self.by_network_id.get(&removed.network_id.0) == Some(&id) {
            self.by_network_id.remove(&removed.network_id.0);
        }
        if self.players_by_uuid.get(&uuid) == Some(&id) {
            self.players_by_uuid.remove(&uuid);
        }
        let (data, moved) = self.players.swap_remove(removed.kind_row)?;
        if let Some(moved) = moved {
            self.store.set_kind_row(moved, removed.kind_row);
        }
        Some((removed, data))
    }

    /// Drops the riding and leash links of a removed entity (`ejectPassengers`,
    /// `removeVehicle`, `dropLeash`); clients learn it from the removal itself.
    fn unlink(&mut self, id: EntityId) {
        if self.riding.is_empty() && self.leashes.is_empty() {
            return;
        }
        self.riding.remove(id);
        let passengers: Vec<EntityId> = self.riding.passengers_of(id).to_vec();
        for p in passengers {
            self.riding.remove(p);
        }
        self.leashes.remove(&id);
    }

    /// The player entity with a UUID.
    pub fn player_by_uuid(&self, uuid: u128) -> Option<EntityId> {
        self.players_by_uuid
            .get(&uuid)
            .copied()
            .filter(|&id| self.store.contains(id))
    }

    /// The vehicle `id` rides (`getVehicle`).
    pub fn vehicle_of(&self, id: EntityId) -> Option<EntityId> {
        self.riding.vehicle_of(id)
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
        } else if kind == ids::EXPERIENCE_ORB {
            self.orbs.swap_remove(row).and_then(|(_, moved)| moved)
        } else if is_mob_kind(kind) {
            self.mobs.swap_remove(row).and_then(|(_, moved)| moved)
        } else if crate::mob::missile::is_missile_kind(kind) {
            self.missiles.swap_remove(row).and_then(|(_, moved)| moved)
        } else if is_special_kind(kind) {
            self.specials.swap_remove(row).and_then(|(_, moved)| moved)
        } else if kind == ids::TNT {
            self.primed_tnts
                .swap_remove(row)
                .and_then(|(_, moved)| moved)
        } else if crate::vehicle::BoatKind::of(kind).is_some() {
            self.boats.swap_remove(row).and_then(|(_, moved)| moved)
        } else if kind == ids::LEASH_KNOT {
            self.knots.swap_remove(row).and_then(|(_, moved)| moved)
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

#[cfg(test)]
mod tests {
    use super::*;
    use vanillite_base::slot_map::SlotMap;

    #[test]
    fn players_ride_in_front() {
        let mut ids: SlotMap<EntityId, ()> = SlotMap::new();
        let (v, mob, player) = (ids.insert(()), ids.insert(()), ids.insert(()));
        let mut r = Riding::default();
        r.add(mob, v, false);
        r.add(player, v, true);
        assert_eq!(r.passengers_of(v), &[player, mob]);
        assert_eq!(r.remove(player), Some(v));
        assert_eq!(r.passengers_of(v), &[mob]);
        assert_eq!(r.remove(mob), Some(v));
        assert!(r.is_empty());
    }
}

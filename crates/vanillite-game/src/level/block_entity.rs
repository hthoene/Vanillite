//! The live block entities of a level (`LevelChunk.blockEntities` and `Level.blockEntityTickers`).
//!
//! A block entity is plain data of its kind ([`BlockEntity`]); its behaviour are functions in
//! [`crate::block`] like every other block behaviour. Block entities are created and removed by
//! [`Level::set_block`] as in `LevelChunk.setBlockState`, and kinds with a ticker tick in
//! [`Level::tick_block_entities`] in Vanilla's order: in the order their tickers were added,
//! tickers added during a tick run from the next tick on.

use vanillite_base::hash::{FxHashMap, FxHashSet};
use vanillite_base::{BlockPos, ChunkPos, Direction};
use vanillite_chunk::slots::BlockEntityData;
use vanillite_data::block::BlockState;
use vanillite_nbt::{Compound, Tag};
use vanillite_worldgen::behaviour::StateExt;

use super::Level;
use super::game_event::{GameEventListeners, Listener};
use crate::block::bell::Bell;
use crate::block::lectern::Lectern;
use crate::block::piston::MovingPiston;
use crate::block::sculk::SculkSensor;
use crate::item::ItemStack;
use vanillite_protocol::components::ids as components;
use vanillite_protocol::text::Component;

/// The data of one block entity, by kind. Kinds whose behaviour does not exist yet have no
/// variant; their blocks simply have no live block entity.
#[derive(Clone, Debug, PartialEq)]
pub enum BlockEntity {
    /// `ComparatorBlockEntity`: the output signal.
    Comparator {
        /// `output`.
        output: i32,
    },
    /// `PistonMovingBlockEntity`.
    MovingPiston(Box<MovingPiston>),
    /// `DaylightDetectorBlockEntity` (no data; it ticks).
    DaylightDetector,
    /// `BellBlockEntity`.
    Bell(Bell),
    /// `LecternBlockEntity`.
    Lectern(Lectern),
    /// `SculkSensorBlockEntity` and `CalibratedSculkSensorBlockEntity`.
    SculkSensor(Box<SculkSensor>),
    /// `HopperBlockEntity`: its stacks and the transfer timing.
    Hopper(Box<crate::block::hopper::Hopper>),
    /// Chests, trapped chests, barrels, shulker boxes, dispensers and droppers.
    Container(Box<crate::block::container::Container>),
    /// `EnderChestBlockEntity`: the viewers of the lid (the stacks are the player's).
    EnderChest {
        /// Players that have it open.
        openers: i32,
    },
    /// Furnaces, blast furnaces and smokers.
    Furnace(Box<crate::block::furnace::Furnace>),
    /// Brewing stands.
    BrewingStand(Box<crate::block::brewing::BrewingStand>),
    /// Signs and hanging signs.
    Sign(Box<crate::block::sign::Sign>),
    /// Heads and skulls.
    Skull(Box<crate::block::decor::Skull>),
    /// Banners.
    Banner(Box<crate::block::decor::Banner>),
    /// Decorated pots.
    DecoratedPot(Box<crate::block::pot::DecoratedPot>),
    /// Chiseled bookshelves.
    ChiseledBookshelf(Box<crate::block::bookshelf::Bookshelf>),
    /// Campfires and soul campfires.
    Campfire(Box<crate::block::campfire::Campfire>),
    /// Suspicious sand and gravel.
    Brushable(Box<crate::block::brushable::Brushable>),
    /// Crafters.
    Crafter(Box<crate::block::crafter::Crafter>),
    /// Beacons.
    Beacon(Box<crate::block::beacon::Beacon>),
    /// Conduits.
    Conduit(Box<crate::block::beacon::Conduit>),
    /// `SpawnerBlockEntity` (`BaseSpawner`).
    Spawner(Box<crate::block::spawner::MobSpawner>),
    /// `TrialSpawnerBlockEntity`.
    TrialSpawner(Box<crate::block::trial_spawner::TrialSpawner>),
    /// `VaultBlockEntity`.
    Vault(Box<crate::block::vault::Vault>),
    /// `SculkShriekerBlockEntity`.
    SculkShrieker(Box<crate::block::sculk_shrieker::Shrieker>),
    /// `SculkCatalystBlockEntity` with its `SculkSpreader`.
    SculkCatalyst(Box<crate::block::sculk_catalyst::Catalyst>),
    /// `TheEndPortalBlockEntity` (no data; clients render the portal from it).
    EndPortal,
    /// `TheEndGatewayBlockEntity`.
    EndGateway(crate::block::portal::Gateway),
}

/// `BlockEntityTicker.tick(level, pos, state, blockEntity)`: the block entity is looked up at
/// `pos`.
pub type TickerFn = fn(&mut Level, BlockPos, BlockState);

#[derive(Debug)]
struct Entry {
    entity: BlockEntity,
    /// The ticker slot of this block entity, if it ticks.
    ticker: Option<u32>,
}

/// The block entities of a level, per chunk.
#[derive(Debug, Default)]
pub struct BlockEntities {
    chunks: FxHashMap<ChunkPos, FxHashMap<BlockPos, Entry>>,
    /// Chunks whose block entities changed in a way the stored list does not show yet
    /// (removed or added ones); written back on save.
    touched: FxHashSet<ChunkPos>,
    /// `blockEntityTickers`: positions and slot ids in tick order; a slot is live while the
    /// entry at its position still holds its id.
    tickers: Vec<(BlockPos, u32)>,
    /// `pendingBlockEntityTickers`.
    pending: Vec<(BlockPos, u32)>,
    next_ticker: u32,
    /// Game event listeners, of block entities and (later) entities.
    pub(super) listeners: GameEventListeners,
}

impl BlockEntities {
    #[inline]
    fn entry(&self, pos: BlockPos) -> Option<&Entry> {
        self.chunks.get(&pos.chunk())?.get(&pos)
    }

    /// The block entity at `pos`.
    #[inline]
    pub fn get(&self, pos: BlockPos) -> Option<&BlockEntity> {
        self.entry(pos).map(|e| &e.entity)
    }

    /// The block entity at `pos`, mutably.
    #[inline]
    pub fn get_mut(&mut self, pos: BlockPos) -> Option<&mut BlockEntity> {
        self.chunks
            .get_mut(&pos.chunk())?
            .get_mut(&pos)
            .map(|e| &mut e.entity)
    }

    /// Number of live block entities.
    pub fn len(&self) -> usize {
        self.chunks.values().map(FxHashMap::len).sum()
    }

    /// Whether there are no live block entities.
    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }

    /// `setBlockEntity` / `addAndRegisterBlockEntity`: replaces the block entity at `pos`; a
    /// ticking kind keeps the ticker slot of the one it replaces (`RebindableTickingBlockEntity
    /// Wrapper.rebind`) or gets a new one at the end of the tick order.
    pub fn insert(&mut self, pos: BlockPos, entity: BlockEntity) {
        let ticks = ticker_of(&entity).is_some();
        let chunk = pos.chunk();
        self.touched.insert(chunk);
        let entries = self.chunks.entry(chunk).or_default();
        let old_ticker = entries.get(&pos).and_then(|e| e.ticker);
        let ticker = match (ticks, old_ticker) {
            (true, Some(id)) => Some(id),
            (true, None) => {
                let id = self.next_ticker;
                self.next_ticker = self.next_ticker.wrapping_add(1);
                self.pending.push((pos, id));
                Some(id)
            }
            (false, _) => None,
        };
        let new_listener = listener_of(pos, &entity);
        if let Some(old) = entries.insert(pos, Entry { entity, ticker })
            && let Some(listener) = listener_of(pos, &old.entity)
            && Some(listener) != new_listener
        {
            self.listeners.unregister(listener);
        }
        // `addGameEventListener`.
        if let Some(listener) = new_listener {
            self.listeners.register(listener);
        }
    }

    /// `removeBlockEntity(pos)`: the block entity, its ticker and its game event listener go.
    pub fn remove(&mut self, pos: BlockPos) -> Option<BlockEntity> {
        let chunk = pos.chunk();
        let entries = self.chunks.get_mut(&chunk)?;
        let removed = entries.remove(&pos)?;
        if entries.is_empty() {
            self.chunks.remove(&chunk);
        }
        self.touched.insert(chunk);
        if let Some(listener) = listener_of(pos, &removed.entity) {
            self.listeners.unregister(listener);
        }
        Some(removed.entity)
    }

    /// The block entities of a chunk.
    pub fn in_chunk(&self, chunk: ChunkPos) -> impl Iterator<Item = (BlockPos, &BlockEntity)> {
        self.chunks
            .get(&chunk)
            .into_iter()
            .flat_map(|entries| entries.iter().map(|(&pos, e)| (pos, &e.entity)))
    }

    /// Marks a chunk whose block entity data changed (written back on save).
    pub fn touch(&mut self, chunk: ChunkPos) {
        self.touched.insert(chunk);
    }

    /// Drops the block entities of an unloaded chunk.
    fn remove_chunk(&mut self, chunk: ChunkPos) {
        if let Some(entries) = self.chunks.remove(&chunk) {
            for (pos, entry) in entries {
                if let Some(listener) = listener_of(pos, &entry.entity) {
                    self.listeners.unregister(listener);
                }
            }
        }
        self.touched.remove(&chunk);
    }
}

/// The ticker of a block entity kind (`EntityBlock.getTicker`).
fn ticker_of(entity: &BlockEntity) -> Option<TickerFn> {
    match entity {
        BlockEntity::Comparator { .. }
        | BlockEntity::Lectern(_)
        | BlockEntity::Container(_)
        | BlockEntity::EnderChest { .. }
        | BlockEntity::Skull(_)
        | BlockEntity::Banner(_)
        | BlockEntity::DecoratedPot(_)
        | BlockEntity::ChiseledBookshelf(_)
        | BlockEntity::Brushable(_)
        | BlockEntity::EndPortal => None,
        BlockEntity::Campfire(_) => Some(crate::block::campfire::tick),
        BlockEntity::Crafter(_) => Some(crate::block::crafter::server_tick),
        BlockEntity::Beacon(_) => Some(crate::block::beacon::tick),
        BlockEntity::Conduit(_) => Some(crate::block::beacon::conduit_tick),
        BlockEntity::Furnace(_) => Some(crate::block::furnace::server_tick),
        BlockEntity::BrewingStand(_) => Some(crate::block::brewing::server_tick),
        BlockEntity::Sign(_) => Some(crate::block::sign::tick),
        BlockEntity::EndGateway(_) => Some(crate::block::portal::gateway_tick),
        BlockEntity::MovingPiston(_) => Some(crate::block::piston::moving_piston_tick),
        BlockEntity::Hopper(_) => Some(crate::block::hopper::push_items_tick),
        BlockEntity::DaylightDetector => Some(crate::block::sensor::daylight_detector_tick),
        BlockEntity::Bell(_) => Some(crate::block::bell::tick),
        BlockEntity::SculkSensor(_) => Some(crate::block::sculk::tick_block_entity),
        BlockEntity::Spawner(_) => Some(crate::block::spawner::tick),
        BlockEntity::TrialSpawner(_) => Some(crate::block::trial_spawner::tick),
        BlockEntity::Vault(_) => Some(crate::block::vault::tick),
        BlockEntity::SculkShrieker(_) => Some(crate::block::sculk_shrieker::tick_block_entity),
        BlockEntity::SculkCatalyst(_) => Some(crate::block::sculk_catalyst::tick_block_entity),
    }
}

/// The game event listener of a block entity kind (`EntityBlock.getListener`).
fn listener_of(pos: BlockPos, entity: &BlockEntity) -> Option<Listener> {
    match entity {
        BlockEntity::SculkSensor(sensor) => Some(Listener::Block {
            pos,
            radius: sensor.listener_radius(),
        }),
        BlockEntity::SculkShrieker(_) => Some(Listener::Block {
            pos,
            radius: crate::block::sculk_shrieker::LISTENER_RADIUS,
        }),
        BlockEntity::SculkCatalyst(_) => Some(Listener::Block {
            pos,
            radius: crate::block::sculk_catalyst::LISTENER_RADIUS,
        }),
        _ => None,
    }
}

/// `EntityBlock.newBlockEntity(pos, state)` for the kinds that exist.
pub fn new_block_entity(state: BlockState) -> Option<BlockEntity> {
    use vanillite_data::block::ids;
    let block = state.block();
    if block == ids::COMPARATOR {
        Some(BlockEntity::Comparator { output: 0 })
    } else if block == ids::DAYLIGHT_DETECTOR {
        Some(crate::block::sensor::new_daylight_detector())
    } else if block == ids::BELL {
        Some(BlockEntity::Bell(Bell::default()))
    } else if block == ids::LECTERN {
        Some(BlockEntity::Lectern(Lectern::default()))
    } else if block == ids::SCULK_SENSOR || block == ids::CALIBRATED_SCULK_SENSOR {
        Some(BlockEntity::SculkSensor(Box::new(SculkSensor::new(
            block == ids::CALIBRATED_SCULK_SENSOR,
        ))))
    } else if block == ids::HOPPER {
        Some(BlockEntity::Hopper(Box::default()))
    } else if block == ids::SPAWNER {
        Some(BlockEntity::Spawner(Box::default()))
    } else if block == ids::TRIAL_SPAWNER {
        Some(BlockEntity::TrialSpawner(Box::default()))
    } else if block == ids::VAULT {
        Some(BlockEntity::Vault(Box::default()))
    } else if block == ids::SCULK_SHRIEKER {
        Some(BlockEntity::SculkShrieker(Box::default()))
    } else if block == ids::SCULK_CATALYST {
        Some(BlockEntity::SculkCatalyst(Box::default()))
    } else if crate::block::sign::is_sign(block) {
        Some(BlockEntity::Sign(Box::default()))
    } else if block == ids::DECORATED_POT {
        Some(BlockEntity::DecoratedPot(Box::default()))
    } else if block == ids::CHISELED_BOOKSHELF {
        Some(BlockEntity::ChiseledBookshelf(Box::default()))
    } else if block == ids::BEACON {
        Some(BlockEntity::Beacon(Box::default()))
    } else if block == ids::CONDUIT {
        Some(BlockEntity::Conduit(Box::default()))
    } else if block == ids::CRAFTER {
        Some(BlockEntity::Crafter(Box::new(
            crate::block::crafter::Crafter {
                triggered: state.value("triggered") == Some("true"),
                ..Default::default()
            },
        )))
    } else if crate::block::brushable::is_brushable(state) {
        Some(BlockEntity::Brushable(Box::default()))
    } else if crate::block::campfire::is_campfire(block) {
        Some(BlockEntity::Campfire(Box::default()))
    } else if crate::block::decor::is_skull(block) {
        Some(BlockEntity::Skull(Box::default()))
    } else if crate::block::decor::is_banner(block) {
        Some(BlockEntity::Banner(Box::default()))
    } else if block == ids::END_PORTAL {
        Some(BlockEntity::EndPortal)
    } else if block == ids::END_GATEWAY {
        Some(BlockEntity::EndGateway(
            crate::block::portal::Gateway::default(),
        ))
    } else {
        crate::block::container::new_block_entity(block)
    }
}

/// Whether `entity` belongs to a block of `state` (`BlockEntityType.isValid(state)`).
fn is_valid(entity: &BlockEntity, state: BlockState) -> bool {
    use vanillite_data::block::ids;
    match entity {
        BlockEntity::Comparator { .. } => state.block() == ids::COMPARATOR,
        BlockEntity::MovingPiston(_) => state.block() == ids::MOVING_PISTON,
        BlockEntity::DaylightDetector => state.block() == ids::DAYLIGHT_DETECTOR,
        BlockEntity::Bell(_) => state.block() == ids::BELL,
        BlockEntity::Lectern(_) => state.block() == ids::LECTERN,
        BlockEntity::SculkSensor(sensor) => {
            state.block()
                == if sensor.calibrated {
                    ids::CALIBRATED_SCULK_SENSOR
                } else {
                    ids::SCULK_SENSOR
                }
        }
        BlockEntity::Hopper(_) => state.block() == ids::HOPPER,
        BlockEntity::Spawner(_) => state.block() == ids::SPAWNER,
        BlockEntity::TrialSpawner(_) => state.block() == ids::TRIAL_SPAWNER,
        BlockEntity::Vault(_) => state.block() == ids::VAULT,
        BlockEntity::SculkShrieker(_) => state.block() == ids::SCULK_SHRIEKER,
        BlockEntity::SculkCatalyst(_) => state.block() == ids::SCULK_CATALYST,
        BlockEntity::Container(c) => {
            use crate::block::container::{Kind, kind_of, size_of};
            matches!(
                kind_of(state.block()),
                Some(
                    k @ (Kind::Chest
                    | Kind::TrappedChest
                    | Kind::Barrel
                    | Kind::ShulkerBox
                    | Kind::Dispenser
                    | Kind::Dropper)
                ) if size_of(k) == c.items.len()
            )
        }
        BlockEntity::EnderChest { .. } => state.block() == ids::ENDER_CHEST,
        BlockEntity::Furnace(f) => {
            crate::block::container::kind_of(state.block())
                == Some(crate::block::container::Kind::Furnace(f.kind))
        }
        BlockEntity::BrewingStand(_) => state.block() == ids::BREWING_STAND,
        BlockEntity::Sign(_) => crate::block::sign::is_sign(state.block()),
        BlockEntity::Skull(_) => crate::block::decor::is_skull(state.block()),
        BlockEntity::Banner(_) => crate::block::decor::is_banner(state.block()),
        BlockEntity::DecoratedPot(_) => state.block() == ids::DECORATED_POT,
        BlockEntity::ChiseledBookshelf(_) => state.block() == ids::CHISELED_BOOKSHELF,
        BlockEntity::Campfire(_) => crate::block::campfire::is_campfire(state.block()),
        BlockEntity::Brushable(_) => crate::block::brushable::is_brushable(state),
        BlockEntity::Crafter(_) => state.block() == ids::CRAFTER,
        BlockEntity::Beacon(_) => state.block() == ids::BEACON,
        BlockEntity::Conduit(_) => state.block() == ids::CONDUIT,
        BlockEntity::EndPortal => state.block() == ids::END_PORTAL,
        BlockEntity::EndGateway(_) => state.block() == ids::END_GATEWAY,
    }
}

/// `shouldChangedStateKeepBlockEntity`: copper chests keep their block entity (and stacks) when
/// they weather or get waxed.
fn keeps_block_entity(old: BlockState, new: BlockState) -> bool {
    let copper_chests = &vanillite_worldgen::behaviour::tags().blocks.copper_chests;
    copper_chests.contains(old) && copper_chests.contains(new)
}

impl Level {
    /// `getBlockEntity(pos)`.
    #[inline]
    pub fn block_entity(&self, pos: BlockPos) -> Option<&BlockEntity> {
        self.block_entities.get(pos)
    }

    /// `getBlockEntity(pos)`, mutably.
    #[inline]
    pub fn block_entity_mut(&mut self, pos: BlockPos) -> Option<&mut BlockEntity> {
        self.block_entities.get_mut(pos)
    }

    /// `setBlockEntity(blockEntity)`: only where the block fits the block entity.
    pub fn set_block_entity(&mut self, pos: BlockPos, entity: BlockEntity) {
        if is_valid(&entity, self.block(pos)) {
            self.block_entities.insert(pos, entity);
        }
    }

    /// `removeBlockEntity(pos)`.
    pub fn remove_block_entity(&mut self, pos: BlockPos) -> Option<BlockEntity> {
        self.block_entities.remove(pos)
    }

    /// The block entity part of `LevelChunk.setBlockState` before `affectNeighborsAfterRemoval`:
    /// the old block's block entity goes when the block changes.
    pub(super) fn block_entity_removed(
        &mut self,
        old: BlockState,
        new: BlockState,
        pos: BlockPos,
        flags: u32,
    ) {
        if old.block() != new.block()
            && old.block().info().block_entity != u16::MAX
            && !keeps_block_entity(old, new)
        {
            let side_effects = flags & super::update::SKIP_BLOCK_ENTITY_SIDEEFFECTS == 0;
            if side_effects {
                // `preRemoveSideEffects`, while the block entity is still there:
                // `Containers.dropContents` reads the stacks, which rolls a loot table first
                // (shulker boxes drop nothing here).
                if crate::block::container::kind_of(old.block())
                    != Some(crate::block::container::Kind::ShulkerBox)
                {
                    crate::block::chest_loot::unpack(self, pos, None);
                }
                if let Some(entity) = self.block_entities.get(pos).cloned() {
                    crate::block::container::pre_remove_side_effects(self, pos, &entity);
                    if let BlockEntity::Lectern(lectern) = &entity {
                        crate::block::lectern::drop_book(self, pos, old, lectern);
                    }
                    if let BlockEntity::Beacon(_) = entity {
                        crate::block::beacon::removed(self, pos);
                    }
                    if let BlockEntity::SculkShrieker(_) = entity {
                        crate::block::sculk_shrieker::pre_remove(self, pos, old);
                    }
                    if let BlockEntity::Furnace(_) = entity {
                        let xp = crate::block::furnace::pop_experience(self, pos);
                        self.award_experience(crate::block::furnace::center(pos), xp);
                    }
                }
            }
            self.block_entities.remove(pos);
        }
    }

    /// `ExperienceOrb.award(level, pos, amount)`: experience orbs at `pos`.
    pub fn award_experience(&mut self, pos: vanillite_base::Vec3, amount: i32) {
        crate::experience_orb::award(self, pos, amount);
    }

    /// The block entity part of `LevelChunk.setBlockState` after `onPlace`: a block with a
    /// block entity gets a fresh one unless a fitting one is there.
    pub(super) fn block_entity_placed(&mut self, state: BlockState, pos: BlockPos) {
        if state.block().info().block_entity == u16::MAX {
            return;
        }
        if let Some(entity) = self.block_entities.get(pos) {
            if is_valid(entity, state) {
                return;
            }
            self.block_entities.remove(pos);
        }
        if let Some(entity) = new_block_entity(state) {
            // `ChunkHolder.broadcastBlockEntityIfNeeded`: a block entity with client data goes
            // out with the block change.
            if update_tag(&entity, state).is_some() {
                self.output().block_entity_updates.push(pos);
            }
            self.block_entities.insert(pos, entity);
        }
    }

    /// `tickBlockEntities`: pending tickers join, then every live ticker in a block ticking
    /// chunk runs if its block still fits its block entity; removed ones leave the list.
    pub fn tick_block_entities(&mut self) {
        let store = &mut self.block_entities;
        let pending = std::mem::take(&mut store.pending);
        store.tickers.extend(pending);
        let mut index = 0;
        let mut kept = 0;
        while index < self.block_entities.tickers.len() {
            let (pos, id) = self.block_entities.tickers[index];
            index += 1;
            let ticker = match self.block_entities.entry(pos) {
                Some(entry) if entry.ticker == Some(id) => ticker_of(&entry.entity),
                _ => continue,
            };
            let tickers = &mut self.block_entities.tickers;
            tickers[kept] = (pos, id);
            kept += 1;
            let Some(ticker) = ticker else {
                continue;
            };
            if !self.is_block_ticking(pos.chunk()) {
                continue;
            }
            let state = self.block(pos);
            if self
                .block_entities
                .get(pos)
                .is_some_and(|entity| is_valid(entity, state))
            {
                ticker(self, pos, state);
            }
        }
        self.block_entities.tickers.truncate(kept);
    }
}

// --- Persistence -----------------------------------------------------------------------------

/// Whether the live world models the block entity of `state` (its stored data is replaced by
/// the live one on save).
fn modelled(state: BlockState) -> bool {
    use vanillite_data::block::ids;
    state.is(ids::MOVING_PISTON) || new_block_entity(state).is_some()
}

/// `BlockEntity.loadWithComponents`: the live block entity of `state` from its stored data.
fn load(state: BlockState, nbt: &Compound) -> Option<BlockEntity> {
    use vanillite_data::block::ids;
    let int = |name| match nbt.get(name) {
        Some(Tag::Int(v)) => Some(*v),
        _ => None,
    };
    if state.is(ids::MOVING_PISTON) {
        let moved = match nbt.get("blockState") {
            Some(Tag::Compound(c)) => read_block_state(c),
            _ => BlockState::AIR,
        };
        let flag = |name| matches!(nbt.get(name), Some(Tag::Byte(v)) if *v != 0);
        let progress = match nbt.get("progress") {
            Some(Tag::Float(v)) => *v,
            _ => 0.0,
        };
        let direction = direction_from_3d(int("facing").unwrap_or(0));
        let mut piston = MovingPiston::new(moved, direction, flag("extending"), flag("source"));
        piston.progress = progress;
        piston.progress_o = progress;
        return Some(BlockEntity::MovingPiston(Box::new(piston)));
    }
    let mut entity = new_block_entity(state)?;
    match &mut entity {
        BlockEntity::Comparator { output } => *output = int("OutputSignal").unwrap_or(0),
        BlockEntity::Spawner(s) => **s = crate::block::spawner::MobSpawner::load(nbt),
        BlockEntity::TrialSpawner(s) => {
            **s = crate::block::trial_spawner::TrialSpawner::load(nbt);
        }
        BlockEntity::Vault(v) => **v = crate::block::vault::Vault::load(nbt),
        BlockEntity::SculkSensor(sensor) => {
            sensor.last_vibration_frequency = int("last_vibration_frequency").unwrap_or(0);
            if let Some(Tag::Compound(l)) = nbt.get("listener") {
                sensor.vibration = crate::block::sculk_vibration::VibrationData::load(l, |_| None);
            }
        }
        BlockEntity::SculkShrieker(shrieker) => shrieker.load(nbt),
        BlockEntity::SculkCatalyst(catalyst) => catalyst.load(nbt),
        BlockEntity::Container(c) => c.load(nbt),
        BlockEntity::Furnace(f) => f.load(nbt),
        BlockEntity::BrewingStand(b) => b.load(nbt),
        BlockEntity::Sign(sign) => sign.load(nbt),
        BlockEntity::Skull(skull) => skull.load(nbt),
        BlockEntity::Banner(banner) => banner.load(nbt),
        BlockEntity::DecoratedPot(pot) => pot.load(nbt),
        BlockEntity::ChiseledBookshelf(shelf) => shelf.load(nbt),
        BlockEntity::Campfire(campfire) => campfire.load(nbt),
        BlockEntity::Brushable(b) => b.load(nbt),
        BlockEntity::Crafter(c) => c.load(nbt),
        BlockEntity::Beacon(b) => b.load(nbt),
        BlockEntity::Conduit(c) => c.load(nbt),
        BlockEntity::Lectern(lectern) => lectern.load(nbt),
        BlockEntity::Hopper(h) => {
            h.custom_name = crate::block::container::load_name(nbt);
            (h.loot_table, h.loot_seed) = crate::block::chest_loot::load(nbt);
            if h.loot_table.is_none() {
                crate::block::container::load_items(nbt, &mut h.items);
            }
            h.cooldown_time = int("TransferCooldown").unwrap_or(-1);
        }
        BlockEntity::EndGateway(gateway) => *gateway = crate::block::portal::Gateway::load(nbt),
        _ => {}
    }
    Some(entity)
}

/// `BlockEntity.saveWithFullMetadata`: the stored data of a live block entity.
fn save(entity: &BlockEntity, state: BlockState, pos: BlockPos) -> Compound {
    let kind = if let BlockEntity::MovingPiston(_) = entity {
        "minecraft:piston"
    } else {
        vanillite_data::registry::BLOCK_ENTITY_TYPE
            .name(state.block().info().block_entity)
            .unwrap_or("minecraft:empty")
    };
    let mut nbt = Compound::new();
    nbt.insert("id", Tag::String(kind.to_owned()));
    nbt.insert("x", Tag::Int(pos.x));
    nbt.insert("y", Tag::Int(pos.y));
    nbt.insert("z", Tag::Int(pos.z));
    match entity {
        BlockEntity::Comparator { output } => nbt.insert("OutputSignal", Tag::Int(*output)),
        BlockEntity::MovingPiston(piston) => {
            nbt.insert(
                "blockState",
                Tag::Compound(write_block_state(piston.moved_state)),
            );
            nbt.insert("facing", Tag::Int(direction_to_3d(piston.direction)));
            nbt.insert("progress", Tag::Float(piston.progress_o));
            nbt.insert("extending", Tag::Byte(i8::from(piston.extending)));
            nbt.insert("source", Tag::Byte(i8::from(piston.source)));
        }
        BlockEntity::Spawner(s) => s.save(&mut nbt, true),
        BlockEntity::TrialSpawner(s) => s.save(&mut nbt),
        BlockEntity::Vault(v) => v.save(&mut nbt),
        BlockEntity::SculkSensor(sensor) => {
            nbt.insert(
                "last_vibration_frequency",
                Tag::Int(sensor.last_vibration_frequency),
            );
            nbt.insert("listener", Tag::Compound(sensor.vibration.save(|_| None)));
        }
        BlockEntity::SculkShrieker(shrieker) => shrieker.save(&mut nbt),
        BlockEntity::SculkCatalyst(catalyst) => catalyst.save(&mut nbt),
        BlockEntity::Container(c) => c.save(&mut nbt),
        BlockEntity::Furnace(f) => f.save(&mut nbt),
        BlockEntity::BrewingStand(b) => b.save(&mut nbt),
        BlockEntity::Sign(sign) => sign.save(&mut nbt),
        BlockEntity::Skull(skull) => skull.save(&mut nbt),
        BlockEntity::Banner(banner) => banner.save(&mut nbt),
        BlockEntity::DecoratedPot(pot) => pot.save(&mut nbt),
        BlockEntity::ChiseledBookshelf(shelf) => shelf.save(&mut nbt),
        BlockEntity::Campfire(campfire) => campfire.save(&mut nbt),
        BlockEntity::Brushable(b) => b.save(&mut nbt),
        BlockEntity::Crafter(c) => c.save(&mut nbt),
        BlockEntity::Beacon(b) => b.save(&mut nbt),
        BlockEntity::Conduit(c) => c.save(&mut nbt),
        BlockEntity::Lectern(lectern) => lectern.save(&mut nbt),
        BlockEntity::Hopper(h) => {
            crate::block::container::save_name(&h.custom_name, &mut nbt);
            if !crate::block::chest_loot::save(h.loot_table.as_deref(), h.loot_seed, &mut nbt) {
                crate::block::container::save_items(&h.items, &mut nbt);
            }
            nbt.insert("TransferCooldown", Tag::Int(h.cooldown_time));
        }
        BlockEntity::EndGateway(gateway) => gateway.save(&mut nbt),
        _ => {}
    }
    nbt
}

/// `Direction.from3DDataValue`.
fn direction_from_3d(value: i32) -> Direction {
    match value.rem_euclid(6) {
        0 => Direction::Down,
        1 => Direction::Up,
        2 => Direction::North,
        3 => Direction::South,
        4 => Direction::West,
        _ => Direction::East,
    }
}

/// `Direction.get3DDataValue`.
fn direction_to_3d(direction: Direction) -> i32 {
    match direction {
        Direction::Down => 0,
        Direction::Up => 1,
        Direction::North => 2,
        Direction::South => 3,
        Direction::West => 4,
        Direction::East => 5,
    }
}

/// `NbtUtils.readBlockState`.
pub(crate) fn read_block_state(c: &Compound) -> BlockState {
    let Some(Tag::String(name)) = c.get("Name") else {
        return BlockState::AIR;
    };
    let Some(block) = vanillite_data::block::BlockId::by_name(name) else {
        return BlockState::AIR;
    };
    let mut state = block.default_state();
    if let Some(Tag::Compound(props)) = c.get("Properties") {
        for (key, value) in props.iter() {
            if let Tag::String(value) = value {
                state = state.with(key, value).unwrap_or(state);
            }
        }
    }
    state
}

/// `NbtUtils.writeBlockState`.
pub(crate) fn write_block_state(state: BlockState) -> Compound {
    let mut c = Compound::new();
    c.insert("Name", Tag::String(state.block().name().to_owned()));
    let mut props = Compound::new();
    for (key, value) in state.properties() {
        props.insert(key, Tag::String(value.to_owned()));
    }
    if !props.is_empty() {
        c.insert("Properties", Tag::Compound(props));
    }
    c
}

impl Level {
    /// Creates the live block entities of a chunk that became live from its stored data.
    fn load_block_entities(&mut self, chunk: ChunkPos) {
        let Some(stored) = self.chunks.chunk(chunk).map(|c| c.block_entities.clone()) else {
            return;
        };
        for data in stored {
            let state = self.block(data.pos);
            if data.kind != Some(state.block().info().block_entity)
                && !state.is(vanillite_data::block::ids::MOVING_PISTON)
            {
                continue;
            }
            let mut input = &data.nbt[..];
            let mut budget = vanillite_nbt::Budget::file();
            let Ok(Some(Tag::Compound(nbt))) =
                vanillite_nbt::read::read_network(&mut input, &mut budget)
            else {
                continue;
            };
            if let Some(entity) = load(state, &nbt) {
                self.block_entities.insert(data.pos, entity);
            }
        }
        self.block_entities.touched.remove(&chunk);
    }

    /// Writes the live block entities of a chunk into its stored list: live ones replace
    /// their stored data, stored data of modelled kinds without a live block entity and of
    /// blocks that no longer have that block entity goes.
    fn write_block_entities(&mut self, chunk: ChunkPos) {
        let Some(stored) = self.chunks.chunk(chunk).map(|c| c.block_entities.clone()) else {
            return;
        };
        let mut list: Vec<BlockEntityData> = stored
            .into_iter()
            .filter(|data| {
                let state = self.block(data.pos);
                data.kind == Some(state.block().info().block_entity) && !modelled(state)
            })
            .collect();
        for (pos, entity) in self.block_entities.in_chunk(chunk) {
            let state = self.block(pos);
            let nbt = save(entity, state, pos);
            let mut bytes = Vec::new();
            if vanillite_nbt::write::write_network_compound(&nbt, &mut bytes).is_err() {
                continue;
            }
            let kind = match nbt.get("id") {
                Some(Tag::String(id)) => vanillite_data::registry::BLOCK_ENTITY_TYPE.id(id),
                _ => None,
            };
            list.push(BlockEntityData {
                pos,
                kind,
                nbt: bytes.into(),
            });
        }
        let unchanged = self.chunks.chunk(chunk).is_some_and(|c| {
            c.block_entities.len() == list.len()
                && c.block_entities
                    .iter()
                    .zip(&list)
                    .all(|(a, b)| a.pos == b.pos && a.kind == b.kind && a.nbt == b.nbt)
        });
        if !unchanged && let Some(target) = self.chunks.block_entities_mut(chunk) {
            *target = list;
        }
        self.block_entities.touched.remove(&chunk);
    }

    /// The chunk runtime's tick (`Chunks::tick`) with the block entities of chunks that unload
    /// written back first and those of chunks that became live loaded.
    pub fn tick_chunk_runtime(&mut self) {
        let mut chunks = std::mem::take(&mut self.chunk_scratch);
        chunks.clear();
        self.chunks.unloading_next_tick(&mut chunks);
        for &chunk in &chunks {
            self.write_block_entities(chunk);
        }
        self.chunks.tick();
        for &chunk in &chunks {
            if self.chunks.chunk(chunk).is_none() {
                self.block_entities.remove_chunk(chunk);
            }
        }
        chunks.clear();
        self.chunks.take_promoted(&mut chunks);
        for &chunk in &chunks {
            self.load_block_entities(chunk);
        }
        self.apply_visibility();
        self.chunk_scratch = chunks;
    }

    /// The chunks that became live in the last [`Level::tick_chunk_runtime`] (the entity
    /// storage loads their entities).
    pub fn promoted_chunks(&self) -> &[ChunkPos] {
        &self.chunk_scratch
    }

    /// Saves the world's chunks with their live block entities written back. `flush` waits
    /// until everything is on disk.
    pub fn save(&mut self, flush: bool) {
        let mut chunks: Vec<ChunkPos> = self.block_entities.chunks.keys().copied().collect();
        chunks.extend(self.block_entities.touched.iter().copied());
        chunks.sort_unstable_by_key(|c| (c.x, c.z));
        chunks.dedup();
        for chunk in chunks {
            self.write_block_entities(chunk);
        }
        self.write_all_entities();
        self.write_all_poi();
        self.chunks.save_all(flush);
    }
}

/// `BlockEntity.getUpdateTag`: what clients see of a block entity, `None` for kinds whose
/// client data is empty.
pub fn update_tag(entity: &BlockEntity, state: BlockState) -> Option<Compound> {
    match entity {
        BlockEntity::Sign(sign) => {
            let mut nbt = Compound::new();
            sign.save(&mut nbt);
            nbt.sort_java_hash_order();
            Some(nbt)
        }
        BlockEntity::Skull(skull) => {
            let mut nbt = Compound::new();
            skull.save(&mut nbt);
            Some(nbt)
        }
        BlockEntity::Banner(banner) => {
            let mut nbt = Compound::new();
            banner.save(&mut nbt);
            Some(nbt)
        }
        BlockEntity::DecoratedPot(pot) => {
            let mut nbt = Compound::new();
            pot.save(&mut nbt);
            Some(nbt)
        }
        BlockEntity::Campfire(campfire) => Some(campfire.update_tag()),
        BlockEntity::Brushable(b) => Some(b.update_tag()),
        // `saveCustomOnly`.
        BlockEntity::Beacon(b) => {
            let mut nbt = Compound::new();
            b.save(&mut nbt);
            Some(nbt)
        }
        BlockEntity::Conduit(c) => {
            let mut nbt = Compound::new();
            c.save(&mut nbt);
            Some(nbt)
        }
        BlockEntity::Spawner(s) => Some(crate::block::spawner::update_tag(s)),
        BlockEntity::TrialSpawner(s) => {
            Some(s.update_tag(crate::block::trial_spawner::TrialState::of(state)))
        }
        BlockEntity::Vault(v) => Some(v.update_tag()),
        // `saveCustomOnly`: the age for the beam and the exit.
        BlockEntity::EndGateway(gateway) => {
            let mut nbt = Compound::new();
            gateway.save(&mut nbt);
            nbt.sort_java_hash_order();
            Some(nbt)
        }
        _ => None,
    }
}

// --- Components ------------------------------------------------------------------------------

/// `BlockEntity.collectComponents`: the value of component `id` the block entity puts on the
/// item it drops (loot `copy_components`), as a wire value.
pub fn collect_component(entity: &BlockEntity, id: u16) -> Option<Box<[u8]>> {
    match entity {
        BlockEntity::Skull(skull) => skull.component(id),
        BlockEntity::Banner(banner) => banner.component(id),
        BlockEntity::DecoratedPot(pot) => pot.component(id),
        BlockEntity::ChiseledBookshelf(shelf) => {
            (id == components::CONTAINER).then(|| container_value(&shelf.items))
        }
        BlockEntity::Campfire(campfire) => {
            (id == components::CONTAINER).then(|| container_value(&campfire.items))
        }
        BlockEntity::Crafter(crafter) => {
            if id == components::CONTAINER {
                Some(container_value(&crafter.items))
            } else if id == components::CUSTOM_NAME {
                encode_name(crafter.custom_name.as_ref())
            } else if id == components::CONTAINER_LOOT {
                let table = crafter.loot_table.as_deref()?;
                Some(crate::block::chest_loot::component(
                    table,
                    crafter.loot_seed,
                ))
            } else {
                None
            }
        }
        _ => crate::block::container::component(entity, id),
    }
}

/// `ItemContainerContents.fromItems`: always a value (empty contents are an empty list).
fn container_value(items: &[ItemStack]) -> Box<[u8]> {
    vanillite_protocol::item::encode_container_contents(items).unwrap_or_else(|| Box::new([0]))
}

/// `BlockEntity.applyComponentsFromItemStack` of a placed block item.
pub fn apply_components(level: &mut Level, pos: BlockPos, stack: &ItemStack) {
    let contents = || {
        stack
            .component(components::CONTAINER)
            .and_then(|v| vanillite_protocol::item::decode_container_contents(v).ok())
    };
    match level.block_entity_mut(pos) {
        Some(BlockEntity::Skull(skull)) => skull.apply(stack),
        Some(BlockEntity::Banner(banner)) => banner.apply(stack),
        Some(BlockEntity::DecoratedPot(pot)) => pot.apply(stack),
        Some(BlockEntity::ChiseledBookshelf(shelf)) => {
            if let Some(items) = contents() {
                copy_into(&items, &mut shelf.items);
            }
        }
        Some(BlockEntity::Campfire(campfire)) => {
            if let Some(items) = contents() {
                copy_into(&items, &mut campfire.items);
            }
        }
        Some(BlockEntity::Crafter(crafter)) => {
            if let Some(name) = decode_name(stack) {
                crafter.custom_name = Some(name);
            }
            if let Some(items) = contents() {
                copy_into(&items, &mut crafter.items);
            }
            if let Some((table, seed)) = stack
                .component(components::CONTAINER_LOOT)
                .and_then(crate::block::chest_loot::read_component)
            {
                crafter.loot_table = Some(table);
                crafter.loot_seed = seed;
            }
        }
        _ => {
            crate::block::container::apply_item_components(level, pos, stack);
            return;
        }
    }
    level.block_entities.touch(pos.chunk());
    let state = level.block(pos);
    if level
        .block_entity(pos)
        .is_some_and(|e| update_tag(e, state).is_some())
    {
        level.output().block_entity_updates.push(pos);
    }
}

/// `ItemContainerContents.copyInto`: slots in order, the rest emptied.
fn copy_into(items: &[ItemStack], target: &mut [ItemStack]) {
    for (i, slot) in target.iter_mut().enumerate() {
        *slot = items.get(i).cloned().unwrap_or_default();
    }
}

/// The `custom_name` component of a stack.
pub fn decode_name(stack: &ItemStack) -> Option<Component> {
    let bytes = stack.component(components::CUSTOM_NAME)?;
    let mut r = vanillite_protocol::codec::Reader::new(bytes);
    <Component as vanillite_protocol::codec::DecodeLimited>::decode_limited(&mut r, 2_097_152).ok()
}

/// A `custom_name` component value.
pub fn encode_name(name: Option<&Component>) -> Option<Box<[u8]>> {
    use vanillite_protocol::codec::Encode;
    let mut out = Vec::new();
    name?.encode(&mut out);
    Some(out.into_boxed_slice())
}

/// Prefix of stored component values that have no persistent form (yet): their wire bytes.
const WIRE_PREFIX: &str = "vanillite:";

/// Stores a component value under `key` in its persistent form (or its wire bytes).
pub fn save_component(out: &mut Compound, key: &str, type_id: u16, wire: &[u8]) {
    match crate::item_nbt::component_to_nbt(type_id, wire) {
        Some(tag) => out.insert(key, tag),
        None => {
            let bytes: Vec<i8> = wire.iter().map(|b| i8::from_ne_bytes([*b])).collect();
            out.insert(format!("{WIRE_PREFIX}{key}"), Tag::ByteArray(bytes));
        }
    }
}

/// Reads what [`save_component`] stored; invalid values read as none.
pub fn load_component(nbt: &Compound, key: &str, type_id: u16) -> Option<Box<[u8]>> {
    if let Some(tag) = nbt.get(key) {
        return crate::item_nbt::component_from_nbt(type_id, tag);
    }
    let Some(Tag::ByteArray(bytes)) = nbt.get(&format!("{WIRE_PREFIX}{key}")) else {
        return None;
    };
    let wire: Vec<u8> = bytes.iter().map(|b| b.to_ne_bytes()[0]).collect();
    vanillite_protocol::components::validate(
        type_id,
        &wire,
        &vanillite_protocol::item::InstalledRegistries,
    )
    .ok()?;
    Some(wire.into_boxed_slice())
}

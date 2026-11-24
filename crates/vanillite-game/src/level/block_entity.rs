//! The live block entities of a level (`LevelChunk.blockEntities` and `Level.blockEntityTickers`).
//!
//! A block entity is plain data of its kind ([`BlockEntity`]); its behaviour are functions in
//! [`crate::block`] like every other block behaviour. Block entities are created and removed by
//! [`Level::set_block`] as in `LevelChunk.setBlockState`, and kinds with a ticker tick in
//! [`Level::tick_block_entities`] in Vanilla's order: in the order their tickers were added,
//! tickers added during a tick run from the next tick on.

use vanillite_base::BlockPos;
use vanillite_base::hash::FxHashMap;
use vanillite_data::block::BlockState;

use super::Level;
use super::game_event::{GameEventListeners, Listener};
use crate::block::bell::Bell;
use crate::block::lectern::Lectern;
use crate::block::piston::MovingPiston;
use crate::block::sculk::SculkSensor;

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
    /// `BellBlockEntity`.
    Bell(Bell),
    /// `LecternBlockEntity`.
    Lectern(Lectern),
    /// `SculkSensorBlockEntity` and `CalibratedSculkSensorBlockEntity`.
    SculkSensor(Box<SculkSensor>),
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

/// The block entities of a level.
#[derive(Debug, Default)]
pub struct BlockEntities {
    entries: FxHashMap<BlockPos, Entry>,
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
    /// The block entity at `pos`.
    #[inline]
    pub fn get(&self, pos: BlockPos) -> Option<&BlockEntity> {
        self.entries.get(&pos).map(|e| &e.entity)
    }

    /// The block entity at `pos`, mutably.
    #[inline]
    pub fn get_mut(&mut self, pos: BlockPos) -> Option<&mut BlockEntity> {
        self.entries.get_mut(&pos).map(|e| &mut e.entity)
    }

    /// Number of live block entities.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether there are no live block entities.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// `setBlockEntity` / `addAndRegisterBlockEntity`: replaces the block entity at `pos`; a
    /// ticking kind keeps the ticker slot of the one it replaces (`RebindableTickingBlockEntity
    /// Wrapper.rebind`) or gets a new one at the end of the tick order.
    pub fn insert(&mut self, pos: BlockPos, entity: BlockEntity) {
        let ticks = ticker_of(&entity).is_some();
        let old_ticker = self.entries.get(&pos).and_then(|e| e.ticker);
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
        if let Some(old) = self.entries.insert(pos, Entry { entity, ticker })
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
        let entity = self.entries.remove(&pos).map(|e| e.entity)?;
        if let Some(listener) = listener_of(pos, &entity) {
            self.listeners.unregister(listener);
        }
        Some(entity)
    }

    /// Positions of all block entities (unordered).
    pub fn positions(&self) -> impl Iterator<Item = BlockPos> + '_ {
        self.entries.keys().copied()
    }
}

/// The ticker of a block entity kind (`EntityBlock.getTicker`).
fn ticker_of(entity: &BlockEntity) -> Option<TickerFn> {
    match entity {
        BlockEntity::Comparator { .. } | BlockEntity::Lectern(_) => None,
        BlockEntity::MovingPiston(_) => Some(crate::block::piston::moving_piston_tick),
        BlockEntity::Bell(_) => Some(crate::block::bell::tick),
        BlockEntity::SculkSensor(_) => Some(crate::block::sculk::tick_block_entity),
    }
}

/// The game event listener of a block entity kind (`EntityBlock.getListener`).
fn listener_of(pos: BlockPos, entity: &BlockEntity) -> Option<Listener> {
    match entity {
        BlockEntity::SculkSensor(sensor) => Some(Listener::Block {
            pos,
            radius: sensor.listener_radius(),
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
    } else if block == ids::BELL {
        Some(BlockEntity::Bell(Bell::default()))
    } else if block == ids::LECTERN {
        Some(BlockEntity::Lectern(Lectern::default()))
    } else if block == ids::SCULK_SENSOR || block == ids::CALIBRATED_SCULK_SENSOR {
        Some(BlockEntity::SculkSensor(Box::new(SculkSensor::new(
            block == ids::CALIBRATED_SCULK_SENSOR,
        ))))
    } else {
        None
    }
}

/// Whether `entity` belongs to a block of `state` (`BlockEntityType.isValid(state)`).
fn is_valid(entity: &BlockEntity, state: BlockState) -> bool {
    use vanillite_data::block::ids;
    match entity {
        BlockEntity::Comparator { .. } => state.block() == ids::COMPARATOR,
        BlockEntity::MovingPiston(_) => state.block() == ids::MOVING_PISTON,
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
    }
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
    pub(super) fn block_entity_removed(&mut self, old: BlockState, new: BlockState, pos: BlockPos) {
        if old.block() != new.block() && old.block().info().block_entity != u16::MAX {
            self.block_entities.remove(pos);
        }
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
            let ticker = match self.block_entities.entries.get(&pos) {
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

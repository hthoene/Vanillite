//! The game state of one world and Vanilla's level operations on it (`Level`, `ServerLevel`).
//!
//! [`Level`] owns the world's chunks and entities plus everything the simulation keeps between
//! ticks: the neighbour updater, block events, the level random, the blocks changed for clients
//! and the level events to broadcast. It is owned by the world's thread; block behaviours are
//! plain functions over `&mut Level` ([`crate::block`]).
//!
//! - [`Level::set_block`] is Vanilla's `setBlock` with its update flags ([`update`]): the chunk
//!   write with `affectNeighborsAfterRemoval` and `onPlace`, the client update, neighbour
//!   updates and shape updates, all through Vanilla's chained [`neighbors`] updater.
//! - [`Level::tick`] runs the world's simulation phases in Vanilla's order (scheduled block and
//!   fluid ticks, random ticks, block events); entities tick in the world's entity phase.

pub mod block_entity;
pub mod explosion;
pub mod game_event;
mod neighbors;
pub mod signal;
mod tick;
pub mod update;

#[cfg(test)]
pub(crate) mod tests;

use std::sync::Arc;

use vanillite_base::hash::{FxHashMap, FxHashSet};
use vanillite_base::random::{LegacyRandom, RandomSource};
use vanillite_base::{BlockPos, ChunkPos, Direction, SectionPos};
use vanillite_data::block::{BlockId, BlockState};
use vanillite_data::fluid::FluidState;
use vanillite_world::chunks::Chunks;
use vanillite_world::entity::{EntityId, NetworkIds};
use vanillite_world::ticks::TickPriority;
use vanillite_worldgen::behaviour::{self as shared, BlockLevel, StateExt};

use crate::block::weather::Climate;
use crate::block::{self, Behaviours};
use crate::entities::Entities;

pub use block_entity::{BlockEntities, BlockEntity};
pub use neighbors::NeighborUpdater;

/// Vanilla's update depth of `setBlock` (`Block.UPDATE_LIMIT`).
pub const UPDATE_LIMIT: i32 = 512;

/// Vanilla mechanics that are commonly called exploits (decision D1): `true` keeps Vanilla's
/// behaviour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExploitSettings {
    /// TNT duplication with pistons and slime or honey blocks.
    pub tnt_duplication: bool,
    /// Carpet duplication with pistons.
    pub carpet_duplication: bool,
    /// Rail duplication with pistons.
    pub rail_duplication: bool,
    /// Pistons without a head.
    pub headless_pistons: bool,
    /// Bedrock and end portal frames broken with pistons and explosions.
    pub break_unbreakable_blocks: bool,
    /// String duplication with tripwire mechanics.
    pub string_duplication: bool,
}

impl Default for ExploitSettings {
    fn default() -> Self {
        ExploitSettings {
            tnt_duplication: true,
            carpet_duplication: true,
            rail_duplication: true,
            headless_pistons: true,
            break_unbreakable_blocks: true,
            string_duplication: true,
        }
    }
}

/// Rules and budgets of a world's simulation.
#[derive(Clone, Debug)]
pub struct LevelSettings {
    /// Scheduled block ticks run per tick (Vanilla: 65536).
    pub max_block_ticks: usize,
    /// Scheduled fluid ticks run per tick (Vanilla: 65536).
    pub max_fluid_ticks: usize,
    /// Chained neighbour updates before the rest is skipped (`max-chained-neighbor-updates`);
    /// negative means unlimited.
    pub max_chained_neighbor_updates: i32,
    /// `randomTickSpeed` game rule.
    pub random_tick_speed: i32,
    /// `waterSourceConversion` game rule.
    pub water_source_conversion: bool,
    /// `lavaSourceConversion` game rule.
    pub lava_source_conversion: bool,
    /// `doTileDrops` game rule (`BLOCK_DROPS`).
    pub block_drops: bool,
    /// `doEntityDrops` game rule (`ENTITY_DROPS`).
    pub entity_drops: bool,
    /// The dimension's `fast_lava` attribute (the Nether).
    pub fast_lava: bool,
    /// The dimension has sky light.
    pub has_sky_light: bool,
    /// The dimension's `water_evaporates` attribute (the Nether).
    pub water_evaporates: bool,
    /// Exploit switches (D1).
    pub exploits: ExploitSettings,
}

impl Default for LevelSettings {
    fn default() -> Self {
        LevelSettings {
            max_block_ticks: 65536,
            max_fluid_ticks: 65536,
            max_chained_neighbor_updates: 1_000_000,
            random_tick_speed: 3,
            water_source_conversion: true,
            lava_source_conversion: false,
            block_drops: true,
            entity_drops: true,
            fast_lava: false,
            has_sky_light: true,
            water_evaporates: false,
            exploits: ExploitSettings::default(),
        }
    }
}

/// A block event (`BlockEventData`): run at the end of the tick, then sent to nearby players.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BlockEvent {
    /// Position.
    pub pos: BlockPos,
    /// The block that queued it; the event is dropped if the block changed.
    pub block: BlockId,
    /// First parameter.
    pub a: i32,
    /// Second parameter.
    pub b: i32,
}

/// A level event (`levelEvent`: sounds and particles of the client), sent to players within 64
/// blocks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LevelEvent {
    /// Event id (`LevelEvent`).
    pub event: i32,
    /// Position.
    pub pos: BlockPos,
    /// Event data.
    pub data: i32,
    /// The player that caused it and predicted it locally; it is not sent to them.
    pub source: Option<EntityId>,
}

/// A sound at a position (`playSound`, `ClientboundSoundPacket`), sent to players within its
/// range.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sound {
    /// `minecraft:sound_event` id.
    pub sound: u16,
    /// `SoundSource` ordinal.
    pub source: u8,
    /// Position.
    pub x: f64,
    /// Position.
    pub y: f64,
    /// Position.
    pub z: f64,
    /// Volume; the range is `16 * max(volume, 1)` blocks.
    pub volume: f32,
    /// Pitch.
    pub pitch: f32,
    /// Seed of the client's random variation.
    pub seed: i64,
    /// The player that caused it and played it locally; it is not sent to them.
    pub except: Option<EntityId>,
}

/// `SoundSource.BLOCKS`.
pub const SOUND_BLOCKS: u8 = 4;
/// `SoundSource.NEUTRAL`.
pub const SOUND_NEUTRAL: u8 = 6;
/// `SoundSource.RECORDS`.
pub const SOUND_RECORDS: u8 = 2;

/// What the world sends after a tick: changed blocks per section, block events, level events
/// and sounds. Filled by the simulation, drained by the server.
#[derive(Debug, Default)]
pub struct LevelOutput {
    /// Changed positions (`ChunkHolder.blockChanged`), deduplicated per section, in change
    /// order.
    pub changed: FxHashMap<SectionPos, Vec<BlockPos>>,
    /// Block events that ran (`ClientboundBlockEventPacket`).
    pub block_events: Vec<BlockEvent>,
    /// Level events.
    pub level_events: Vec<LevelEvent>,
    /// Sounds.
    pub sounds: Vec<Sound>,
    /// Explosions.
    pub explosions: Vec<explosion::ExplosionEvent>,
    /// The damage of the living entities explosions hit, for the mob system.
    pub explosion_hurts: Vec<explosion::ExplosionHurt>,
    /// An end crystal was placed: the End fight tries to respawn the dragon (`tryRespawn`).
    pub try_dragon_respawn: bool,
}

/// The game state of one world.
pub struct Level {
    /// Chunks, tickets, scheduled ticks.
    pub chunks: Chunks,
    /// Entities.
    pub entities: Entities,
    /// Rules and budgets.
    pub settings: LevelSettings,
    behaviours: Arc<Behaviours>,
    network_ids: Arc<NetworkIds>,
    climate: Option<Arc<Climate>>,
    updater: NeighborUpdater,
    /// Live block entities.
    pub block_entities: BlockEntities,
    /// Reused buffers of the piston code.
    pub(crate) pistons: crate::block::piston::Scratch,
    /// `ServerLevel.handlingTick`: the simulation phases of the tick run (scheduled ticks to
    /// block events).
    handling_tick: bool,
    /// `RedstoneWireBlock.shouldSignal`: `false` while a wire computes its power.
    wire_signals: bool,
    /// `RedstoneTorchBlock.RECENT_TOGGLES` of this level: torch positions and when they turned
    /// off, oldest first.
    pub(crate) torch_toggles: Vec<(BlockPos, i64)>,
    /// Scratch list of [`Level::tick_chunk_runtime`].
    chunk_scratch: Vec<ChunkPos>,
    /// `blockEvents`, a linked set: an event already queued is not queued again.
    block_events: Vec<BlockEvent>,
    block_event_set: FxHashSet<BlockEvent>,
    random: LegacyRandom,
    /// `soundSeedGenerator`: the seeds of sounds without an explicit seed; separate from the
    /// level random so sounds do not shift the simulation's random sequence.
    sound_seeds: LegacyRandom,
    /// `randValue` of `getBlockRandomPos`.
    rand_value: i32,
    game_time: i64,
    output: LevelOutput,
    min_y: i32,
    max_y: i32,
}

impl std::fmt::Debug for Level {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Level")
            .field("chunks", &self.chunks)
            .field("game_time", &self.game_time)
            .finish_non_exhaustive()
    }
}

impl Level {
    /// The game state of a world over its chunk runtime.
    pub fn new(
        chunks: Chunks,
        settings: LevelSettings,
        behaviours: Arc<Behaviours>,
        network_ids: Arc<NetworkIds>,
        seed: i64,
    ) -> Level {
        let info = chunks.info();
        let min_y = info.min_y;
        let max_y = info.min_y + info.height as i32 - 1;
        Level {
            chunks,
            entities: Entities::new(),
            updater: NeighborUpdater::new(settings.max_chained_neighbor_updates),
            settings,
            behaviours,
            network_ids,
            climate: None,
            block_entities: BlockEntities::default(),
            pistons: crate::block::piston::Scratch::default(),
            handling_tick: false,
            wire_signals: true,
            torch_toggles: Vec::new(),
            chunk_scratch: Vec::new(),
            block_events: Vec::new(),
            block_event_set: FxHashSet::default(),
            random: LegacyRandom::new(seed),
            sound_seeds: LegacyRandom::new(seed ^ 0x50_4E_44_53),
            rand_value: LegacyRandom::new(seed ^ 0x5DEE_CE66).next_int(),
            game_time: 0,
            output: LevelOutput::default(),
            min_y,
            max_y,
        }
    }

    /// Replaces the rules and budgets (config reload between ticks).
    pub fn set_settings(&mut self, settings: LevelSettings) {
        self.updater
            .set_max_chained(settings.max_chained_neighbor_updates);
        self.settings = settings;
    }

    /// Sets the world's biomes, for the biome dependent simulation (freezing).
    pub fn set_climate(&mut self, climate: Arc<Climate>) {
        self.climate = Some(climate);
    }

    /// The world's biomes, if known.
    pub fn climate(&self) -> Option<&Climate> {
        self.climate.as_deref()
    }

    /// `getBiome(pos)`: the noise biome the biome manager picks for `pos`; the default biome
    /// outside live chunks.
    pub fn biome(&self, pos: BlockPos) -> u16 {
        let default = self.chunks.info().default_biome;
        let Some(climate) = &self.climate else {
            return default;
        };
        climate
            .manager
            .get_biome(pos.x, pos.y, pos.z, &mut |qx: i32, qy: i32, qz: i32| {
                let Some(chunk) = self.chunks.chunk(ChunkPos::new(qx >> 2, qz >> 2)) else {
                    return default;
                };
                // `LevelChunk.getNoiseBiome`: the quart Y is clamped to the chunk.
                let sections = chunk.chunk.sections();
                let min_quart = chunk.chunk.min_section() * 4;
                let qy = qy.clamp(min_quart, min_quart + sections.len() as i32 * 4 - 1);
                let index = (qy - min_quart) >> 2;
                sections[index as usize].biome((qx & 3) as u32, (qy & 3) as u32, (qz & 3) as u32)
            })
    }

    /// The block behaviours.
    pub fn behaviours(&self) -> &Arc<Behaviours> {
        &self.behaviours
    }

    /// The server-wide network id counter.
    pub fn network_ids(&self) -> &NetworkIds {
        &self.network_ids
    }

    /// The server-wide network id counter, shared.
    pub fn network_ids_arc(&self) -> &Arc<NetworkIds> {
        &self.network_ids
    }

    /// The level random (`level.getRandom()`).
    pub fn random(&mut self) -> &mut LegacyRandom {
        &mut self.random
    }

    /// The game time.
    pub fn game_time(&self) -> i64 {
        self.game_time
    }

    /// `ServerLevel.isHandlingTick()`: between the start of the tick and the end of the block
    /// events.
    pub fn is_handling_tick(&self) -> bool {
        self.handling_tick
    }

    /// Sets `handlingTick`: `true` from the start of the world tick until its block events ran
    /// (`ServerLevel.tick`), `false` while players' packets are handled.
    pub fn set_handling_tick(&mut self, handling: bool) {
        self.handling_tick = handling;
    }

    /// Lowest block Y.
    pub fn min_y(&self) -> i32 {
        self.min_y
    }

    /// Highest block Y.
    pub fn max_y(&self) -> i32 {
        self.max_y
    }

    /// `isInsideBuildHeight` / `isInValidBounds` for Y.
    #[inline]
    pub fn in_height(&self, y: i32) -> bool {
        y >= self.min_y && y <= self.max_y
    }

    /// What to send to clients; the server drains it after the tick.
    pub fn output(&mut self) -> &mut LevelOutput {
        &mut self.output
    }

    // --- Reading -------------------------------------------------------------------------

    /// `getBlockState(pos)`: void air below and above the world, air in chunks that are not
    /// live.
    #[inline]
    pub fn block(&self, pos: BlockPos) -> BlockState {
        match self.chunks.block(pos) {
            Some(state) => BlockState(state),
            None if self.in_height(pos.y) => BlockState::AIR,
            None => block::VOID_AIR,
        }
    }

    /// `getFluidState(pos)`.
    #[inline]
    pub fn fluid(&self, pos: BlockPos) -> FluidState {
        self.block(pos).fluid_state()
    }

    /// `isLoaded(pos)` / `hasChunkAt`: the position is in a live chunk.
    #[inline]
    pub fn is_loaded(&self, pos: BlockPos) -> bool {
        self.chunks.chunk(pos.chunk()).is_some()
    }

    /// `getBrightness(LightLayer.SKY / BLOCK, pos)` as `(sky, block)`.
    pub fn light(&self, pos: BlockPos) -> (i32, i32) {
        let Some(chunk) = self.chunks.chunk(pos.chunk()) else {
            return (if self.settings.has_sky_light { 15 } else { 0 }, 0);
        };
        let light = chunk.chunk.light();
        let index = (pos.y >> 4) - (chunk.chunk.min_section() - 1);
        let (x, y, z) = (
            (pos.x & 15) as u32,
            (pos.y & 15) as u32,
            (pos.z & 15) as u32,
        );
        let Ok(index) = usize::try_from(index) else {
            return (0, 0);
        };
        let block = light
            .block()
            .get(index)
            .map_or(0, |l| i32::from(l.get(x, y, z)));
        let sky = if !self.settings.has_sky_light {
            0
        } else if index >= light.len() {
            15
        } else {
            // Sections without sky data read like the data above them: full light above the
            // highest data, as the client does.
            light.sky()[index..]
                .iter()
                .find(|l| !matches!(l, vanillite_chunk::LightNibbles::Absent))
                .map_or(15, |l| {
                    if std::ptr::eq(l, &light.sky()[index]) {
                        i32::from(l.get(x, y, z))
                    } else {
                        i32::from(l.get(x, 0, z))
                    }
                })
        };
        (sky, block)
    }

    /// `getRawBrightness(pos, 0)`.
    pub fn raw_brightness(&self, pos: BlockPos) -> i32 {
        let (sky, block) = self.light(pos);
        sky.max(block)
    }

    /// `getMaxLocalRawBrightness(pos)`: like [`Self::raw_brightness`] with the sky darkened by
    /// the time of day. The simulation has no day cycle yet, so this is the daytime value.
    pub fn max_local_raw_brightness(&self, pos: BlockPos) -> i32 {
        self.raw_brightness(pos)
    }

    // --- Scheduled ticks and events --------------------------------------------------------

    /// `scheduleTick(pos, block, delay)`.
    pub fn schedule_block_tick(&mut self, pos: BlockPos, block: BlockId, delay: i32) {
        self.chunks
            .schedule_block_tick(pos, block.0, delay, TickPriority::NORMAL);
    }

    /// `scheduleTick(pos, block, delay, priority)`.
    pub fn schedule_block_tick_with(
        &mut self,
        pos: BlockPos,
        block: BlockId,
        delay: i32,
        priority: TickPriority,
    ) {
        self.chunks
            .schedule_block_tick(pos, block.0, delay, priority);
    }

    /// `scheduleTick(pos, fluid, delay)`; `fluid` is a fluid id.
    pub fn schedule_fluid_tick(&mut self, pos: BlockPos, fluid: u16, delay: i32) {
        self.chunks
            .schedule_fluid_tick(pos, fluid, delay, TickPriority::NORMAL);
    }

    /// `blockEvent(pos, block, a, b)`: queued unless the same event is queued already.
    pub fn block_event(&mut self, pos: BlockPos, block: BlockId, a: i32, b: i32) {
        let event = BlockEvent { pos, block, a, b };
        if self.block_event_set.insert(event) {
            self.block_events.push(event);
        }
    }

    /// `playSound(except, pos, sound, source, volume, pitch)` at the block's centre.
    pub fn play_block_sound(
        &mut self,
        except: Option<EntityId>,
        pos: BlockPos,
        sound: u16,
        source: u8,
        volume: f32,
        pitch: f32,
    ) {
        let (x, y, z) = (
            f64::from(pos.x) + 0.5,
            f64::from(pos.y) + 0.5,
            f64::from(pos.z) + 0.5,
        );
        self.play_sound(except, x, y, z, sound, source, volume, pitch);
    }

    /// `playSound(except, x, y, z, sound, source, volume, pitch)`.
    #[allow(clippy::too_many_arguments)]
    pub fn play_sound(
        &mut self,
        except: Option<EntityId>,
        x: f64,
        y: f64,
        z: f64,
        sound: u16,
        source: u8,
        volume: f32,
        pitch: f32,
    ) {
        let seed = self.sound_seeds.next_long();
        self.play_seeded_sound(except, x, y, z, sound, source, volume, pitch, seed);
    }

    /// `playSeededSound(except, x, y, z, sound, source, volume, pitch, seed)`.
    #[allow(clippy::too_many_arguments)]
    pub fn play_seeded_sound(
        &mut self,
        except: Option<EntityId>,
        x: f64,
        y: f64,
        z: f64,
        sound: u16,
        source: u8,
        volume: f32,
        pitch: f32,
        seed: i64,
    ) {
        self.output.sounds.push(Sound {
            sound,
            source,
            x,
            y,
            z,
            volume,
            pitch,
            seed,
            except,
        });
    }

    /// `levelEvent(source, event, pos, data)`.
    pub fn level_event(&mut self, source: Option<EntityId>, event: i32, pos: BlockPos, data: i32) {
        self.output.level_events.push(LevelEvent {
            event,
            pos,
            data,
            source,
        });
    }

    // --- Writing -------------------------------------------------------------------------

    /// `setBlock(pos, state, flags)` with Vanilla's update depth.
    pub fn set_block(&mut self, pos: BlockPos, state: BlockState, flags: u32) -> bool {
        self.set_block_depth(pos, state, flags, UPDATE_LIMIT)
    }

    /// `setBlockAndUpdate(pos, state)`: [`update::ALL`].
    pub fn set_block_and_update(&mut self, pos: BlockPos, state: BlockState) -> bool {
        self.set_block(pos, state, update::ALL)
    }

    /// `setBlock(pos, state, flags, updateLimit)` (`Level.setBlock` with
    /// `LevelChunk.setBlockState`). Returns whether the block was set.
    pub fn set_block_depth(
        &mut self,
        pos: BlockPos,
        state: BlockState,
        flags: u32,
        limit: i32,
    ) -> bool {
        if !self.in_height(pos.y) {
            return false;
        }
        // `LevelChunk.setBlockState`.
        let Some(old) = self.chunks.set_block(pos, state.0).map(BlockState) else {
            return false;
        };
        let new_block = state.block();
        let block_changed = old.block() != new_block;
        let moved = flags & update::MOVE_BY_PISTON != 0;
        self.block_entity_removed(old, state, pos);
        if (block_changed || block::is_rail(new_block)) && (flags & update::NEIGHBORS != 0 || moved)
        {
            self.affect_neighbors_after_removal(old, pos, moved);
        }
        if self.block(pos).block() != new_block {
            return false;
        }
        if flags & update::SKIP_ON_PLACE == 0 {
            self.on_place(state, pos, old, moved);
        }
        if self.block(pos).block() == new_block {
            self.block_entity_placed(state, pos);
        }
        // `Level.setBlock`.
        if self.block(pos) == state {
            if flags & update::CLIENTS != 0 {
                self.send_block_updated(pos);
            }
            if flags & update::NEIGHBORS != 0 {
                self.update_neighbors_at(pos, old.block());
                if Self::has_analog_output(state) {
                    self.update_neighbour_for_output_signal(pos, new_block);
                }
            }
            if flags & update::KNOWN_SHAPE == 0 && limit > 0 {
                let neighbour_flags = flags & !(update::NEIGHBORS | update::SUPPRESS_DROPS);
                self.update_indirect_neighbour_shapes(old, pos, neighbour_flags, limit - 1);
                self.update_neighbour_shapes(state, pos, neighbour_flags, limit - 1);
                self.update_indirect_neighbour_shapes(state, pos, neighbour_flags, limit - 1);
            }
        }
        true
    }

    /// `removeBlock(pos, movedByPiston)`: the block becomes the fluid it holds.
    pub fn remove_block(&mut self, pos: BlockPos, moved: bool) -> bool {
        let fluid = self.fluid(pos);
        let flags = update::ALL | if moved { update::MOVE_BY_PISTON } else { 0 };
        self.set_block(pos, fluid.block_state(), flags)
    }

    /// `destroyBlock(pos, drop, breaker)`.
    pub fn destroy_block(&mut self, pos: BlockPos, drop: bool, breaker: Option<EntityId>) -> bool {
        self.destroy_block_depth(pos, drop, breaker, UPDATE_LIMIT)
    }

    /// `destroyBlock(pos, drop, breaker, updateLimit)`: break particles and sound, drops, then
    /// the block becomes the fluid it holds.
    pub fn destroy_block_depth(
        &mut self,
        pos: BlockPos,
        drop: bool,
        breaker: Option<EntityId>,
        limit: i32,
    ) -> bool {
        let state = self.block(pos);
        if state.is_air() {
            return false;
        }
        // `spawnDestroyParticles`: `LevelEvent.PARTICLES_DESTROY_BLOCK`.
        if !block::is_fire(state) {
            self.level_event(None, 2001, pos, i32::from(state.0));
        }
        if drop {
            block::drops::drop_resources(self, pos, state, breaker);
        }
        let fluid = self.fluid(pos);
        self.set_block_depth(pos, fluid.block_state(), update::ALL, limit)
    }

    /// `sendBlockUpdated`: the position goes to the clients that see its chunk.
    pub fn send_block_updated(&mut self, pos: BlockPos) {
        let list = self.output.changed.entry(pos.section()).or_default();
        if !list.contains(&pos) {
            list.push(pos);
        }
    }

    // --- Updates -------------------------------------------------------------------------

    /// `updateNeighborsAt(pos, block)`: neighbour updates of the six neighbours in
    /// `NeighborUpdater.UPDATE_ORDER`.
    pub fn update_neighbors_at(&mut self, pos: BlockPos, source: BlockId) {
        self.update_neighbors_at_except(pos, source, None);
    }

    /// `updateNeighborsAtExceptFromFacing(pos, block, skip)`.
    pub fn update_neighbors_at_except(
        &mut self,
        pos: BlockPos,
        source: BlockId,
        skip: Option<Direction>,
    ) {
        self.add_and_run(neighbors::Update::multi(pos, source, skip));
    }

    /// `neighborChanged(pos, block)`.
    pub fn neighbor_changed(&mut self, pos: BlockPos, source: BlockId) {
        self.add_and_run(neighbors::Update::Simple { pos, source });
    }

    /// `neighborShapeChanged(direction, pos, neighbourPos, neighbourState, flags, limit)`.
    pub fn neighbor_shape_changed(
        &mut self,
        direction: Direction,
        pos: BlockPos,
        neighbour_pos: BlockPos,
        neighbour_state: BlockState,
        flags: u32,
        limit: i32,
    ) {
        self.add_and_run(neighbors::Update::Shape {
            direction,
            neighbour_state,
            pos,
            neighbour_pos,
            flags,
            limit,
        });
    }

    /// `state.updateNeighbourShapes(level, pos, flags, limit)`: every neighbour in
    /// `UPDATE_SHAPE_ORDER` learns that `state` is now at `pos`.
    pub fn update_neighbour_shapes(
        &mut self,
        state: BlockState,
        pos: BlockPos,
        flags: u32,
        limit: i32,
    ) {
        for direction in shared::UPDATE_SHAPE_ORDER {
            let neighbour = pos.relative(direction);
            self.neighbor_shape_changed(direction.opposite(), neighbour, pos, state, flags, limit);
        }
    }

    /// `state.updateIndirectNeighbourShapes(level, pos, flags, limit)`: only redstone wire has
    /// them (`RedstoneWireBlock.updateIndirectNeighbourShapes`): wire diagonally above and below
    /// a connected side learns about the block next to it.
    pub fn update_indirect_neighbour_shapes(
        &mut self,
        state: BlockState,
        pos: BlockPos,
        flags: u32,
        limit: i32,
    ) {
        if !state.is(vanillite_data::block::ids::REDSTONE_WIRE) {
            return;
        }
        for direction in shared::HORIZONTAL {
            if state.value(shared_direction_name(direction)) == Some("none") {
                continue;
            }
            let side = pos.relative(direction);
            if self
                .block(side)
                .is(vanillite_data::block::ids::REDSTONE_WIRE)
            {
                continue;
            }
            for wire in [side.offset(0, -1, 0), side.offset(0, 1, 0)] {
                if self
                    .block(wire)
                    .is(vanillite_data::block::ids::REDSTONE_WIRE)
                {
                    let neighbour = wire.relative(direction.opposite());
                    let neighbour_state = self.block(neighbour);
                    self.neighbor_shape_changed(
                        direction.opposite(),
                        wire,
                        neighbour,
                        neighbour_state,
                        flags,
                        limit,
                    );
                }
            }
        }
    }

    /// `Block.updateOrDestroy(old, new, level, pos, flags, limit)`.
    pub fn update_or_destroy(
        &mut self,
        old: BlockState,
        new: BlockState,
        pos: BlockPos,
        flags: u32,
        limit: i32,
    ) {
        if new == old {
            return;
        }
        if new.is_air() {
            self.destroy_block_depth(pos, flags & update::SUPPRESS_DROPS == 0, None, limit);
        } else {
            self.set_block_depth(pos, new, flags & !update::SUPPRESS_DROPS, limit);
        }
    }

    /// `state.updateShape(...)` against this level.
    pub fn update_shape(
        &mut self,
        state: BlockState,
        pos: BlockPos,
        direction: Direction,
        neighbour_pos: BlockPos,
        neighbour_state: BlockState,
    ) -> BlockState {
        shared::update_shape(self, state, pos, direction, neighbour_pos, neighbour_state)
    }

    /// `state.canSurvive(level, pos)`.
    pub fn can_survive(&self, state: BlockState, pos: BlockPos) -> bool {
        shared::can_survive(self, state, pos)
    }

    // --- Behaviour dispatch --------------------------------------------------------------

    fn on_place(&mut self, state: BlockState, pos: BlockPos, old: BlockState, moved: bool) {
        if let Some(f) = self.behaviours.get(state.block()).on_place {
            f(self, state, pos, old, moved);
        }
    }

    /// `state.useWithoutItem(level, pos, player, hitResult)` of the block at `pos`.
    pub fn use_without_item(
        &mut self,
        pos: BlockPos,
        context: &block::UseContext,
    ) -> block::Interaction {
        let state = self.block(pos);
        match self.behaviours.get(state.block()).use_without_item {
            Some(f) => f(self, state, pos, context),
            None => block::Interaction::Pass,
        }
    }

    /// `state.entityInside(level, pos, entity, ...)` of the block at `pos`.
    pub fn entity_inside(&mut self, pos: BlockPos, entity: EntityId) {
        let state = self.block(pos);
        if let Some(f) = self.behaviours.get(state.block()).entity_inside {
            f(self, state, pos, entity);
        }
    }

    /// `state.affectNeighborsAfterRemoval(level, pos, movedByPiston)`.
    pub(crate) fn affect_neighbors_after_removal(
        &mut self,
        state: BlockState,
        pos: BlockPos,
        moved: bool,
    ) {
        if let Some(f) = self
            .behaviours
            .get(state.block())
            .affect_neighbors_after_removal
        {
            f(self, state, pos, moved);
        }
    }

    /// `state.handleNeighborChanged(level, pos, block, orientation, movedByPiston)`.
    fn handle_neighbor_changed(
        &mut self,
        state: BlockState,
        pos: BlockPos,
        source: BlockId,
        moved: bool,
    ) {
        if let Some(f) = self.behaviours.get(state.block()).neighbor_changed {
            f(self, state, pos, source, moved);
        }
    }

    /// A random UUID for a new entity (`Mth.createInsecureUUID(entity.random)`): drawn from a
    /// random of its own, as Vanilla's entities draw it from theirs, so spawning entities does
    /// not shift the level random.
    pub fn new_uuid(&mut self) -> u128 {
        let most = self.sound_seeds.next_long() as u64;
        let least = self.sound_seeds.next_long() as u64;
        // Version 4, IETF variant.
        let most = (most & !0xF000) | 0x4000;
        let least = (least & !(0xC000_0000_0000_0000)) | 0x8000_0000_0000_0000;
        (u128::from(most) << 64) | u128::from(least)
    }

    /// Whether `pos` lies in a chunk where blocks tick.
    pub fn is_block_ticking(&self, chunk: ChunkPos) -> bool {
        self.chunks.is_block_ticking(chunk)
    }
}

impl BlockLevel for Level {
    #[inline]
    fn block(&self, pos: BlockPos) -> BlockState {
        Level::block(self, pos)
    }

    fn raw_brightness(&self, pos: BlockPos) -> i32 {
        Level::raw_brightness(self, pos)
    }

    fn random_int(&mut self, bound: i32) -> i32 {
        self.random.next_int_bounded(bound)
    }

    fn schedule_block_tick(&mut self, pos: BlockPos, block: BlockId, delay: i32) {
        Level::schedule_block_tick(self, pos, block, delay);
    }

    fn schedule_fluid_tick(&mut self, pos: BlockPos, fluid: u16) {
        let delay = block::fluid::tick_delay(self, fluid);
        Level::schedule_fluid_tick(self, pos, fluid, delay);
    }

    fn has_block_tick(&self, pos: BlockPos, block: BlockId) -> bool {
        self.chunks.has_block_tick(pos, block.0)
    }

    fn comparator_output(&self, pos: BlockPos) -> i32 {
        Level::comparator_output(self, pos)
    }
}

/// `direction.getSerializedName()` of a horizontal direction (the wire side properties).
fn shared_direction_name(direction: Direction) -> &'static str {
    match direction {
        Direction::North => "north",
        Direction::South => "south",
        Direction::West => "west",
        Direction::East => "east",
        Direction::Up => "up",
        Direction::Down => "down",
    }
}

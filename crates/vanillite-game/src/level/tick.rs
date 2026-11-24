//! The simulation phases of the world tick (`ServerLevel.tick`): game time, scheduled block and
//! fluid ticks, post-processing of new chunks, random ticks and block events.

use std::collections::VecDeque;

use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, ChunkPos};
use vanillite_worldgen::behaviour as shared;

use super::{BlockEvent, Level, update};
use crate::block::{self, fluid};

impl Level {
    /// `tickTime`: the game time advances.
    pub fn tick_time(&mut self) {
        self.game_time += 1;
        self.chunks.set_game_time(self.game_time);
    }

    /// `blockTicks.tick(...)` and `fluidTicks.tick(...)`: the scheduled ticks due now, block
    /// ticks first.
    pub fn run_scheduled_ticks(&mut self) {
        self.chunks
            .collect_block_ticks(self.settings.max_block_ticks);
        while let Some((pos, kind)) = self.chunks.next_block_tick() {
            // `tickBlock`: only if the block is still there.
            let state = self.block(pos);
            if state.block().0 == kind
                && let Some(tick) = self.behaviours.get(state.block()).tick
            {
                tick(self, state, pos);
            }
        }
        self.chunks
            .collect_fluid_ticks(self.settings.max_fluid_ticks);
        while let Some((pos, kind)) = self.chunks.next_fluid_tick() {
            // `tickFluid`: only if the fluid is still there.
            let state = self.block(pos);
            let fluid = state.fluid_state();
            if !fluid.is_empty() && fluid.info().fluid == kind {
                fluid::tick(self, pos, state, fluid);
            }
        }
    }

    /// `LevelChunk.postProcessGeneration` of chunks that became live and whose neighbours are
    /// live: fluids generation left behind tick, other marked blocks update their shape.
    pub fn post_process(&mut self) {
        let mut ready = Vec::new();
        self.chunks.take_post_processing(&mut ready);
        for (_, marks) in ready {
            for pos in marks {
                let state = self.block(pos);
                let fluid = state.fluid_state();
                if !fluid.is_empty() {
                    fluid::tick(self, pos, state, fluid);
                }
                if block::is_liquid(state) {
                    if let Some(tick) = self.behaviours.get(state.block()).tick {
                        tick(self, state, pos);
                    }
                } else {
                    let new = shared::update_from_neighbour_shapes(self, state, pos);
                    if new != state {
                        self.set_block(
                            pos,
                            new,
                            update::INVISIBLE
                                | update::KNOWN_SHAPE
                                | update::SKIP_BLOCK_ENTITY_SIDEEFFECTS,
                        );
                    }
                }
            }
        }
    }

    /// `ServerChunkCache.tickChunks` as far as blocks go: random ticks of every chunk in
    /// simulation distance (`ServerLevel.tickChunk`).
    pub fn tick_chunks(&mut self, scratch: &mut Vec<ChunkPos>) {
        let speed = self.settings.random_tick_speed;
        scratch.clear();
        self.chunks.random_ticking_chunks(scratch);
        for &chunk in scratch.iter() {
            self.tick_chunk(chunk, speed);
        }
    }

    /// `ServerLevel.tickChunk`: precipitation checks and `randomTickSpeed` random ticks per
    /// section that has randomly ticking blocks or fluids.
    fn tick_chunk(&mut self, chunk: ChunkPos, speed: i32) {
        let min_x = chunk.x * 16;
        let min_z = chunk.z * 16;
        for _ in 0..speed {
            if self.random.next_int_bounded(48) == 0 {
                let pos = self.block_random_pos(min_x, 0, min_z, 15);
                block::weather::tick_precipitation(self, pos);
            }
        }
        if speed <= 0 {
            return;
        }
        let Some(sections) = self.chunks.chunk(chunk).map(|c| c.chunk.sections().len()) else {
            return;
        };
        let min_section = self.min_y >> 4;
        for index in 0..sections {
            let ticking = self
                .chunks
                .chunk(chunk)
                .is_some_and(|c| c.chunk.sections()[index].summary().is_randomly_ticking());
            if !ticking {
                continue;
            }
            let min_y = (min_section + index as i32) * 16;
            for _ in 0..speed {
                let pos = self.block_random_pos(min_x, min_y, min_z, 15);
                let state = self.block(pos);
                if state.random_ticks()
                    && let Some(random_tick) = self.behaviours.get(state.block()).random_tick
                {
                    random_tick(self, state, pos);
                }
                let fluid = state.fluid_state();
                if fluid.info().random_ticks {
                    fluid::random_tick(self, pos, fluid);
                }
            }
        }
    }

    /// `getBlockRandomPos(x, y, z, mask)`: Vanilla's cheap LCG over `randValue`.
    pub fn block_random_pos(&mut self, x: i32, y: i32, z: i32, mask: i32) -> BlockPos {
        self.rand_value = self.rand_value.wrapping_mul(3).wrapping_add(1_013_904_223);
        let value = self.rand_value >> 2;
        BlockPos::new(
            x + (value & mask),
            y + (value >> 16 & mask),
            z + (value >> 8 & mask),
        )
    }

    /// `runBlockEvents`: every queued block event whose chunk ticks runs (events added meanwhile
    /// too); events in chunks that do not tick wait for a later tick.
    pub fn run_block_events(&mut self) {
        let mut queue: VecDeque<BlockEvent> = std::mem::take(&mut self.block_events).into();
        let mut reschedule = Vec::new();
        loop {
            // Events queued by running events run in the same loop.
            queue.extend(self.block_events.drain(..));
            let Some(event) = queue.pop_front() else {
                break;
            };
            // `removeFirst` of the linked set: the event may be queued again from now on.
            self.block_event_set.remove(&event);
            if !self.is_block_ticking(event.pos.chunk()) {
                reschedule.push(event);
                continue;
            }
            let state = self.block(event.pos);
            if state.block() != event.block {
                continue;
            }
            let Some(trigger) = self.behaviours.get(state.block()).trigger_event else {
                continue;
            };
            if trigger(self, state, event.pos, event.a, event.b) {
                self.output.block_events.push(event);
            }
        }
        // `blockEvents.addAll(blockEventsToReschedule)`.
        for event in reschedule {
            self.block_event(event.pos, event.block, event.a, event.b);
        }
    }
}

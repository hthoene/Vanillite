//! What players see of a world tick's simulation (`ChunkHolder.broadcastChanges`,
//! `ServerLevel.levelEvent`, `ServerLevel.runBlockEvents`): light updates, changed blocks per
//! section, block events and level events. Each packet is encoded once and sent to the players
//! that see it.

use std::sync::Arc;

use vanillite_base::hash::FxHashMap;
use vanillite_base::{BlockPos, ChunkPos, SectionPos};
use vanillite_game::entities::Entities;
use vanillite_game::level::{Level, LevelOutput};
use vanillite_protocol::SharedPacket;
use vanillite_protocol::codec::{write_varint, write_varlong};
use vanillite_protocol::ids::play::clientbound as ids;
use vanillite_world::chunks::LightChange;

/// Range of level events and block events in blocks (`PlayerList.broadcast(..., 64.0, ...)`).
const EVENT_RANGE: f64 = 64.0;

/// Scratch space of the broadcast, kept between ticks.
#[derive(Debug, Default)]
pub(crate) struct WorldUpdates {
    light: Vec<(ChunkPos, LightChange)>,
    per_chunk: FxHashMap<ChunkPos, Vec<Arc<SharedPacket>>>,
}

impl WorldUpdates {
    /// The light changes of this tick, for the chunk packet cache and the broadcast.
    pub(crate) fn light_changes(&mut self) -> &mut Vec<(ChunkPos, LightChange)> {
        &mut self.light
    }

    /// Sends the light changes collected with [`Self::light_changes`] and the level's output,
    /// then clears both.
    pub(crate) fn broadcast(&mut self, level: &mut Level) {
        let mut output = std::mem::take(level.output());
        self.encode_chunks(level, &mut output);
        let entities = &level.entities;
        if !self.per_chunk.is_empty() {
            for (_, player) in entities.players.iter() {
                let chunks = &player.connection.chunks;
                let Some(view) = chunks.view() else {
                    continue;
                };
                for (&pos, packets) in &self.per_chunk {
                    // `ChunkMap.isChunkTracked`.
                    if view.contains(pos, true) && !chunks.is_pending(pos) {
                        for packet in packets {
                            player.connection.sink.send_shared(packet);
                        }
                    }
                }
            }
            self.per_chunk.clear();
        }
        for event in output.block_events.drain(..) {
            let packet = block_event(event.pos, event.block.0, event.a, event.b);
            send_near(entities, event.pos, None, &packet);
        }
        for event in output.level_events.drain(..) {
            let packet = level_event(event.event, event.pos, event.data);
            send_near(entities, event.pos, event.source, &packet);
        }
        // Hand the emptied buffers back to the level.
        let reuse = level.output();
        if reuse.changed.is_empty() && reuse.block_events.is_empty() && reuse.level_events.is_empty()
        {
            output.changed.clear();
            *reuse = output;
        }
    }

    /// Light updates first, then the changed blocks of each section, per chunk.
    fn encode_chunks(&mut self, level: &Level, output: &mut LevelOutput) {
        for (pos, change) in self.light.drain(..) {
            let Some(chunk) = level.chunks.chunk(pos) else {
                continue;
            };
            let mut body = Vec::with_capacity(1024);
            write_varint(&mut body, ids::LIGHT_UPDATE);
            vanillite_chunk::packet::encode_light_update_filtered(
                &mut body,
                pos.x,
                pos.z,
                chunk.chunk.light(),
                &change.sky,
                &change.block,
            );
            self.per_chunk
                .entry(pos)
                .or_default()
                .push(Arc::new(SharedPacket::new(body)));
        }
        let mut sections: Vec<_> = output.changed.drain().collect();
        // Deterministic order for the clients: by section.
        sections.sort_unstable_by_key(|(s, _)| (s.x, s.z, s.y));
        for (section, positions) in sections {
            let chunk = section.chunk();
            if level.chunks.chunk(chunk).is_none() || positions.is_empty() {
                continue;
            }
            let packet = if let [pos] = positions[..] {
                block_update(pos, level.block(pos).0)
            } else {
                section_blocks_update(section, &positions, |pos| level.block(pos).0)
            };
            self.per_chunk.entry(chunk).or_default().push(packet);
        }
    }
}

/// Sends `packet` to the players within [`EVENT_RANGE`] of `pos`, except `except`.
fn send_near(
    entities: &Entities,
    pos: BlockPos,
    except: Option<vanillite_world::entity::EntityId>,
    packet: &Arc<SharedPacket>,
) {
    let (x, y, z) = (f64::from(pos.x), f64::from(pos.y), f64::from(pos.z));
    for (id, player) in entities.players.iter() {
        if Some(id) == except {
            continue;
        }
        let Some(entity) = entities.store.get(id) else {
            continue;
        };
        let p = entity.pos();
        let (dx, dy, dz) = (x - p.x, y - p.y, z - p.z);
        if dx * dx + dy * dy + dz * dz < EVENT_RANGE * EVENT_RANGE {
            player.connection.sink.send_shared(packet);
        }
    }
}

/// `BlockPos.asLong`.
fn block_pos_long(pos: BlockPos) -> i64 {
    ((i64::from(pos.x) & 0x3FF_FFFF) << 38)
        | ((i64::from(pos.z) & 0x3FF_FFFF) << 12)
        | (i64::from(pos.y) & 0xFFF)
}

/// `SectionPos.asLong`.
fn section_pos_long(pos: SectionPos) -> i64 {
    ((i64::from(pos.x) & 0x3F_FFFF) << 42)
        | ((i64::from(pos.z) & 0x3F_FFFF) << 20)
        | (i64::from(pos.y) & 0xF_FFFF)
}

/// `block_update`.
fn block_update(pos: BlockPos, state: u16) -> Arc<SharedPacket> {
    let mut body = Vec::with_capacity(16);
    write_varint(&mut body, ids::BLOCK_UPDATE);
    body.extend_from_slice(&block_pos_long(pos).to_be_bytes());
    write_varint(&mut body, i32::from(state));
    Arc::new(SharedPacket::new(body))
}

/// `section_blocks_update`.
fn section_blocks_update(
    section: SectionPos,
    positions: &[BlockPos],
    state: impl Fn(BlockPos) -> u16,
) -> Arc<SharedPacket> {
    let mut body = Vec::with_capacity(16 + positions.len() * 4);
    write_varint(&mut body, ids::SECTION_BLOCKS_UPDATE);
    body.extend_from_slice(&section_pos_long(section).to_be_bytes());
    write_varint(&mut body, i32::try_from(positions.len()).unwrap_or(i32::MAX));
    for &pos in positions {
        // `SectionPos.sectionRelativePos`.
        let relative = ((pos.x & 15) << 8) | ((pos.z & 15) << 4) | (pos.y & 15);
        write_varlong(
            &mut body,
            (i64::from(state(pos)) << 12) | i64::from(relative),
        );
    }
    Arc::new(SharedPacket::new(body))
}

/// `block_event`: the block's registry id.
fn block_event(pos: BlockPos, block: u16, a: i32, b: i32) -> Arc<SharedPacket> {
    let mut body = Vec::with_capacity(16);
    write_varint(&mut body, ids::BLOCK_EVENT);
    body.extend_from_slice(&block_pos_long(pos).to_be_bytes());
    body.push(a as u8);
    body.push(b as u8);
    write_varint(&mut body, i32::from(block));
    Arc::new(SharedPacket::new(body))
}

/// `level_event` (not global).
fn level_event(event: i32, pos: BlockPos, data: i32) -> Arc<SharedPacket> {
    let mut body = Vec::with_capacity(20);
    write_varint(&mut body, ids::LEVEL_EVENT);
    body.extend_from_slice(&event.to_be_bytes());
    body.extend_from_slice(&block_pos_long(pos).to_be_bytes());
    body.extend_from_slice(&data.to_be_bytes());
    body.push(0);
    Arc::new(SharedPacket::new(body))
}


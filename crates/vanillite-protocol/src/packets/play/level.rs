//! Clientbound play packets of the level's state: the world border, the tick rate and biome
//! changes.

use crate::Packet;
use crate::codec::{Decode, Encode, Error, Reader, Result, write_len};

/// `initialize_border`.
#[derive(Debug, Clone, PartialEq, Packet)]
#[packet(state = Play, dir = Clientbound, name = "initialize_border")]
pub struct InitializeBorder {
    pub center_x: f64,
    pub center_z: f64,
    pub old_size: f64,
    pub new_size: f64,
    #[field(varlong)]
    pub lerp_time: i64,
    #[field(varint)]
    pub absolute_max_size: i32,
    #[field(varint)]
    pub warning_blocks: i32,
    #[field(varint)]
    pub warning_time: i32,
}

/// `set_border_center`.
#[derive(Debug, Clone, PartialEq, Packet)]
#[packet(state = Play, dir = Clientbound, name = "set_border_center")]
pub struct SetBorderCenter {
    pub center_x: f64,
    pub center_z: f64,
}

/// `set_border_lerp_size`.
#[derive(Debug, Clone, PartialEq, Packet)]
#[packet(state = Play, dir = Clientbound, name = "set_border_lerp_size")]
pub struct SetBorderLerpSize {
    pub old_size: f64,
    pub new_size: f64,
    #[field(varlong)]
    pub lerp_time: i64,
}

/// `set_border_size`.
#[derive(Debug, Clone, PartialEq, Packet)]
#[packet(state = Play, dir = Clientbound, name = "set_border_size")]
pub struct SetBorderSize {
    pub size: f64,
}

/// `set_border_warning_delay`: ticks.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Play, dir = Clientbound, name = "set_border_warning_delay")]
pub struct SetBorderWarningDelay {
    #[field(varint)]
    pub warning_delay: i32,
}

/// `set_border_warning_distance`.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Play, dir = Clientbound, name = "set_border_warning_distance")]
pub struct SetBorderWarningDistance {
    #[field(varint)]
    pub warning_blocks: i32,
}

/// `ticking_state`.
#[derive(Debug, Clone, PartialEq, Packet)]
#[packet(state = Play, dir = Clientbound, name = "ticking_state")]
pub struct TickingState {
    pub tick_rate: f32,
    pub is_frozen: bool,
}

/// `ticking_step`.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Play, dir = Clientbound, name = "ticking_step")]
pub struct TickingStep {
    #[field(varint)]
    pub tick_steps: i32,
}

/// One chunk of `chunks_biomes`: its position and the biome containers of all its sections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkBiomeData {
    pub x: i32,
    pub z: i32,
    /// The sections' biome containers as in the chunk packet.
    pub buffer: Vec<u8>,
}

/// `chunks_biomes` (`ClientboundChunksBiomesPacket`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunksBiomes {
    pub chunks: Vec<ChunkBiomeData>,
}

/// `ChunkBiomeData`'s buffer limit (`ByteBufCodecs.byteArray(2097152)`).
const MAX_BUFFER: usize = 2_097_152;
/// A bound on the chunk count when decoding (more than any view holds).
const MAX_CHUNKS: usize = 16_384;

impl Encode for ChunksBiomes {
    fn encode(&self, out: &mut Vec<u8>) {
        write_len(out, self.chunks.len());
        for chunk in &self.chunks {
            // `ChunkPos.pack`.
            let packed =
                (i64::from(chunk.x) & 0xFFFF_FFFF) | ((i64::from(chunk.z) & 0xFFFF_FFFF) << 32);
            packed.encode(out);
            write_len(out, chunk.buffer.len());
            out.extend_from_slice(&chunk.buffer);
        }
    }
}

impl Decode for ChunksBiomes {
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        let count = usize::try_from(r.varint()?).map_err(|_| Error::Invalid("chunk count"))?;
        if count > MAX_CHUNKS {
            return Err(Error::Invalid("chunk count"));
        }
        let mut chunks = Vec::with_capacity(count);
        for _ in 0..count {
            let packed = r.i64()?;
            let len = usize::try_from(r.varint()?).map_err(|_| Error::Invalid("biome buffer"))?;
            if len > MAX_BUFFER {
                return Err(Error::Invalid("biome buffer"));
            }
            #[allow(clippy::cast_possible_truncation, clippy::as_conversions)]
            chunks.push(ChunkBiomeData {
                x: packed as i32,
                z: (packed >> 32) as i32,
                buffer: r.take(len)?.to_vec(),
            });
        }
        Ok(ChunksBiomes { chunks })
    }
}

impl Packet for ChunksBiomes {
    const ID: i32 = crate::ids::play::clientbound::CHUNKS_BIOMES;
    const NAME: &'static str = "chunks_biomes";
    const STATE: crate::State = crate::State::Play;
    const DIRECTION: crate::Direction = crate::Direction::Clientbound;
    const COST: u16 = 1;
    const BUCKET: crate::Bucket = crate::Bucket::Default;
}

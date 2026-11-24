//! A flat placeholder world (Vanilla's classic superflat layers: bedrock, two dirt, grass) so
//! players can join and walk before the world runtime (#14) provides real chunks. It is a
//! [`ChunkSource`] like the world runtime and drops out when that exists.
//!
//! Every chunk is the same, so the chunk data is encoded once; a packet only adds the position.

use std::sync::Arc;

use vanillite_base::{ChunkPos, Vec3};
use vanillite_chunk::facts::{self, Tables};
use vanillite_chunk::{Chunk, ChunkLight, LightNibbles, Section};
use vanillite_data::block::{BlockState, STATE_COUNT, flags, ids};
use vanillite_protocol::SharedPacket;
use vanillite_protocol::codec::Encode;

use crate::player::chunks::ChunkSource;

/// Lowest block of the overworld.
const MIN_Y: i32 = -64;
/// Height of the overworld.
const HEIGHT: u32 = 384;
/// Top of the grass layer: players stand on `SURFACE_Y`.
pub const SURFACE_Y: i32 = MIN_Y + 4;

/// Installs the per-state facts of the chunk format from the Vanilla data, unless the world
/// runtime installed its own first. Heightmap tags are approximated by "solid".
pub fn install_chunk_facts(biome_count: u32) {
    let table: Box<[u8]> = (0..STATE_COUNT)
        .map(|i| {
            let state = BlockState(u16::try_from(i).unwrap_or(0));
            let mut f = 0;
            if state.is_air() {
                f |= facts::AIR;
            }
            if state.random_ticks() {
                f |= facts::RANDOM_TICKING;
            }
            if !state.fluid_state().is_empty() {
                f |= facts::FLUID;
            }
            if state.has(flags::SOLID) {
                f |= facts::BLOCKS_MOTION | facts::BLOCKS_MOTION_NO_LEAVES;
            }
            f
        })
        .collect();
    facts::install(Tables {
        block_flags: Box::leak(table),
        biome_count,
    });
}

/// The flat world.
#[derive(Debug)]
pub struct FlatWorld {
    /// Chunk data after the position: heightmaps, sections, block entities and light.
    data: Box<[u8]>,
}

impl FlatWorld {
    /// A flat world with `biome` (its id in the biome registry). The chunk facts must be
    /// installed.
    pub fn new(biome: u16) -> FlatWorld {
        let air = BlockState::AIR.0;
        let sections = usize::try_from(HEIGHT / 16).unwrap_or(0);
        let mut bottom = Section::new(air, biome);
        let layers = [
            ids::BEDROCK.default_state().0,
            ids::DIRT.default_state().0,
            ids::DIRT.default_state().0,
            ids::GRASS_BLOCK.default_state().0,
        ];
        for (y, state) in (0u32..).zip(layers) {
            for x in 0..16 {
                for z in 0..16 {
                    bottom.set_block(x, y, z, state);
                }
            }
        }
        let empty = Arc::new(Section::new(air, biome));
        let mut all = vec![empty; sections];
        if let Some(first) = all.first_mut() {
            *first = Arc::new(bottom);
        }
        // Sky light like Vanilla stores it: data up to one section above the highest blocks;
        // the client treats everything higher as 15. No block light.
        let mut light = ChunkLight::new(sections);
        let mut surface = LightNibbles::Uniform(15);
        for y in 0..4 {
            for x in 0..16 {
                for z in 0..16 {
                    surface.set(x, y, z, 0);
                }
            }
        }
        let sky = light.sky_mut();
        if let Some(below) = sky.get_mut(0) {
            *below = LightNibbles::Uniform(0);
        }
        if let Some(first) = sky.get_mut(1) {
            *first = surface;
        }
        if let Some(above) = sky.get_mut(2) {
            *above = LightNibbles::Uniform(15);
        }
        for layer in light.block_mut() {
            *layer = LightNibbles::Uniform(0);
        }
        let chunk = Chunk::from_sections(MIN_Y >> 4, all, light);
        let mut body = Vec::new();
        vanillite_chunk::packet::encode_chunk_with_light(&mut body, 0, 0, &chunk, &[]);
        FlatWorld {
            data: body.get(8..).unwrap_or_default().into(),
        }
    }

    /// Where new players spawn.
    pub fn spawn(&self) -> Vec3 {
        Vec3::new(0.5, f64::from(SURFACE_Y), 0.5)
    }
}

impl ChunkSource for FlatWorld {
    fn chunk_packet(&self, pos: ChunkPos) -> Option<Arc<SharedPacket>> {
        use vanillite_protocol::Packet;
        use vanillite_protocol::packets::play::clientbound::LevelChunkWithLight;
        let mut body = Vec::with_capacity(self.data.len().saturating_add(16));
        vanillite_protocol::codec::write_varint(&mut body, LevelChunkWithLight::ID);
        pos.x.encode(&mut body);
        pos.z.encode(&mut body);
        body.extend_from_slice(&self.data);
        Some(Arc::new(SharedPacket::new(body)))
    }
}

//! In-memory chunk format: paletted containers, sections, light, heightmaps and chunk packet encoding.
//!
//! This crate is the shared data contract for chunks. Block states and biomes are kept in the
//! client's wire layout (palette plus big-endian packed longs), so encoding a section is a header
//! plus a copy. Per-section summaries let other systems skip sections that cannot matter, light is
//! stored as uniform or full nibble arrays, and heightmaps keep Vanilla's packed layout.
//!
//! The per-state facts the format needs (air, random ticking, fluids, heightmap predicates) and the
//! size of the global palettes are installed once at startup with [`facts::install_vanilla`].
//! [`slots`] holds the generation, light and storage contracts (slots) around the format.

pub mod bits;
pub mod chunk;
pub mod container;
pub mod facts;
pub mod heightmap;
pub mod layout;
pub mod light;
pub mod packet;
pub mod section;
pub mod slots;
pub mod store;
pub mod wire;

#[cfg(test)]
mod test_support;

pub use chunk::Chunk;
pub use container::{Biomes, BlockStates, ContainerData, PalettedContainer, Strategy};
pub use facts::{BiomeId, BlockStateId};
pub use heightmap::{Heightmap, HeightmapKind, Heightmaps};
pub use light::{ChunkLight, LightNibbles};
pub use packet::{BlockEntityWire, SectionMask};
pub use section::{Section, SectionSummary};
pub use slots::{ChunkStatus, ProtoChunk, WorldInfo};
pub use wire::DecodeError;

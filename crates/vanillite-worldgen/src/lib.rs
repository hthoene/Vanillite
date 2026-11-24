//! Vanilla world generation as a library: noise, density functions, biomes, surface rules, carvers, features and structures.
//!
//! The crate generates terrain bit-exactly like Vanilla 26.3 for the same seed. It knows nothing
//! about worlds or threads: the chunk scheduler and the parity tests run the [`Stage`]s of a
//! [`TerrainGenerator`] on batches of chunks.
//!
//! Vanilla 26.3 evaluates density functions in `float` over whole volumes; the port keeps Java's
//! operation order and `float`/`double` conversions everywhere.

pub mod anchor;
pub mod aquifer;
pub mod behaviour;
pub mod biome;
pub mod blocks;
pub mod carver;
pub mod density;
pub mod feature;
pub mod generator;
pub mod interval;
pub mod material;
pub mod mth;
pub mod noise;
pub mod proto;
pub mod random;
pub mod random_state;
pub mod settings;
pub mod template;
pub mod volume;

pub use generator::{
    DimensionSpec, NeighbourBiomes, Stage, TerrainGenerator, TerrainJob, TerrainStep,
};
pub use proto::ProtoChunk;
pub use random_state::RandomState;

use std::fmt;

/// An invalid or missing piece of world generation data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error(String);

impl Error {
    /// An error with a message.
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
extern crate self as vanillite_worldgen;

#[cfg(test)]
#[path = "../tests/common/mod.rs"]
pub(crate) mod test_support;

//! Generated Vanilla tables: block states, per-state facts, shapes, registries, protocol ids,
//! entity and item facts.
//!
//! The tables are generated at build time (`vanillite-datagen`, into `OUT_DIR`) from the data
//! of the Vanilla version pinned in `data/vanilla/version.toml`, extracted from Mojang's server
//! jar into a per-machine cache; the version they describe is in [`version`]. Everything is static data: no allocation, no
//! I/O, no initialisation at start.

#[rustfmt::skip]
#[allow(clippy::all)]
mod generated {
    include!(concat!(env!("OUT_DIR"), "/mod.rs"));
}

/// Directory of the extracted data of the pinned version (`items.json`, `registries.json`,
/// ...; the layout of `vanillite_datagen::normalize`). Only for tests and tools on the build
/// machine: it is not there at run time elsewhere.
pub const VANILLA_DATA_DIR: &str = env!("VANILLITE_VANILLA_DATA");

/// Raw Vanilla reports embedded in the binary.
pub mod raw {
    /// `items.json`: the default components of every item, one item per line.
    pub const ITEMS_JSON: &str =
        include_str!(concat!(env!("VANILLITE_VANILLA_DATA"), "/items.json"));
    /// `biome_parameters/overworld.json`: the overworld's biome parameter list.
    pub const OVERWORLD_BIOME_PARAMETERS_JSON: &str = include_str!(concat!(
        env!("VANILLITE_VANILLA_DATA"),
        "/biome_parameters/overworld.json"
    ));
}

pub mod block;
pub mod entity;
pub mod fluid;
pub mod item;
pub mod registry;

/// Version constants of the Vanilla release the tables describe.
pub mod version {
    pub use crate::generated::version::*;
}

/// Packet ids per connection state and direction (`packets::play::clientbound::ADD_ENTITY`).
pub mod packets {
    pub use crate::generated::packets::*;
}

/// Lists of data-driven (dynamic) registries, filled from data packs at run time.
pub mod dynamic_registries {
    pub use crate::generated::dynamic::*;
}

//! Generated Vanilla tables: block states, per-state facts, shapes, registries, protocol ids,
//! entity and item facts.
//!
//! The tables in `generated/` come from `data/vanilla/` through `cargo xtask codegen`; the
//! Vanilla version they describe is in [`version`]. Everything is static data: no allocation, no
//! I/O, no initialisation at start.

#[rustfmt::skip]
#[allow(clippy::all)]
mod generated;

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

//! World runtime infrastructure: chunk map, tickets, chunk scheduler, block access and updates, scheduled ticks and the entity store. No gameplay.
//!
//! - [`entity`]: the entity store (generational ids, core columns, kind stores, tick order,
//!   section index).
//! - [`tracking`]: the `EntityTracker` slot, its built-in implementation and the encode-once
//!   fan-out of its updates.

pub mod entity;
pub mod tracking;

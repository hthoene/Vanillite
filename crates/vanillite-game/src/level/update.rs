//! The update flags of `setBlock` (`Block.UPDATE_*`).

/// Neighbours get `neighborChanged` (`UPDATE_NEIGHBORS`).
pub const NEIGHBORS: u32 = 1;
/// Clients learn about the change (`UPDATE_CLIENTS`).
pub const CLIENTS: u32 = 2;
/// The client does not re-render the block (`UPDATE_INVISIBLE`).
pub const INVISIBLE: u32 = 4;
/// The client re-renders at once (`UPDATE_IMMEDIATE`).
pub const IMMEDIATE: u32 = 8;
/// No shape updates of the neighbours (`UPDATE_KNOWN_SHAPE`).
pub const KNOWN_SHAPE: u32 = 16;
/// Blocks broken by shape updates drop nothing (`UPDATE_SUPPRESS_DROPS`).
pub const SUPPRESS_DROPS: u32 = 32;
/// The change comes from a piston (`UPDATE_MOVE_BY_PISTON`).
pub const MOVE_BY_PISTON: u32 = 64;
/// Shape updates skip redstone wire (`UPDATE_SKIP_SHAPE_UPDATE_ON_WIRE`).
pub const SKIP_SHAPE_UPDATE_ON_WIRE: u32 = 128;
/// No block entity side effects (`UPDATE_SKIP_BLOCK_ENTITY_SIDEEFFECTS`).
pub const SKIP_BLOCK_ENTITY_SIDEEFFECTS: u32 = 256;
/// `onPlace` is not called (`UPDATE_SKIP_ON_PLACE`).
pub const SKIP_ON_PLACE: u32 = 512;
/// No updates at all (`UPDATE_NONE`).
pub const NONE: u32 = KNOWN_SHAPE | INVISIBLE;
/// Neighbours and clients (`UPDATE_ALL`).
pub const ALL: u32 = NEIGHBORS | CLIENTS;
/// Neighbours and clients, re-rendered at once (`UPDATE_ALL_IMMEDIATE`).
pub const ALL_IMMEDIATE: u32 = ALL | IMMEDIATE;

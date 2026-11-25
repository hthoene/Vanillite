//! Portal blocks: lighting a nether portal frame with fire (`BaseFireBlock.onPlace`), the End
//! portal frame filled with eyes of ender (`EnderEyeItem.useOn`) and the End gateway's block
//! entity (`TheEndGatewayBlockEntity`). Travelling through portals is [`crate::portal`].

use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Direction};
use vanillite_data::block::{BlockId, BlockState, ids};
use vanillite_nbt::{Compound, Tag};
use vanillite_worldgen::behaviour::{HorizontalAxis, PortalShape, StateExt};

use super::BlockBehaviour;
use crate::level::block_entity::BlockEntity;
use crate::level::{Level, update};

/// `TheEndGatewayBlockEntity.COOLDOWN_TIME`.
const GATEWAY_COOLDOWN: i32 = 40;
/// `TheEndGatewayBlockEntity.ATTENTION_INTERVAL`.
const GATEWAY_ATTENTION_INTERVAL: i64 = 2400;

/// Fills the portal behaviours of `block`.
pub(super) fn fill(block: BlockId, b: &mut BlockBehaviour) {
    if block == ids::FIRE || block == ids::SOUL_FIRE {
        b.on_place = Some(fire_on_place);
    } else if block == ids::END_GATEWAY {
        b.trigger_event = Some(gateway_event);
    }
}

/// `BaseFireBlock.onPlace` (and `FireBlock.onPlace`'s fire tick): fire placed inside an empty
/// obsidian frame of the Overworld or the Nether becomes a nether portal; fire that cannot stay
/// goes out.
fn fire_on_place(level: &mut Level, state: BlockState, pos: BlockPos, old: BlockState, _: bool) {
    'base: {
        if old.block() == state.block() {
            break 'base;
        }
        if level.settings.portal_dimension
            && let Some(shape) = PortalShape::find_empty(&*level, pos, HorizontalAxis::X)
        {
            create_portal_blocks(level, &shape);
            break 'base;
        }
        if !level.can_survive(state, pos) {
            level.remove_block(pos, false);
        }
    }
    if state.block() == ids::FIRE {
        // `FireBlock.getFireTickDelay`.
        let delay = 30 + level.random().next_int_bounded(10);
        level.schedule_block_tick(pos, ids::FIRE, delay);
    }
}

/// `PortalShape.createPortalBlocks`: portal blocks without neighbour or shape updates.
pub fn create_portal_blocks(level: &mut Level, shape: &PortalShape) {
    let state = shape.portal_state();
    for pos in shape.inner() {
        level.set_block(pos, state, update::CLIENTS | update::KNOWN_SHAPE);
    }
}

/// `BaseFireBlock.inPortalDimension`-gated `isPortal`: whether fire at `pos` would light a
/// portal (a frame block next to it and an empty frame around it). `forward` is the player's
/// horizontal facing.
pub fn lights_portal(level: &Level, pos: BlockPos, forward: Direction) -> bool {
    if !level.settings.portal_dimension {
        return false;
    }
    let next_to_frame = Direction::ALL
        .iter()
        .any(|&face| vanillite_worldgen::behaviour::is_frame(level.block(pos.relative(face))));
    if !next_to_frame {
        return false;
    }
    // `forwardDirection.getCounterClockWise().getAxis()` of a horizontal direction is the
    // other horizontal axis.
    let preferred =
        HorizontalAxis::of_direction(forward).map_or(HorizontalAxis::X, HorizontalAxis::other);
    PortalShape::find_empty(level, pos, preferred).is_some()
}

// --- End portal frames -------------------------------------------------------------------

/// `EndPortalFrameBlock.getOrCreatePortalShape().find(level, pos)`: the 3 × 3 inside of a
/// complete frame of twelve frame blocks with eyes facing inwards around the frame at `pos`,
/// as its north-west corner.
pub fn find_end_portal(level: &Level, pos: BlockPos) -> Option<BlockPos> {
    // The frame at `pos` is one of the twelve; try every inside that it can border.
    for dx in -4..=4 {
        for dz in -4..=4 {
            let corner = pos.offset(dx, 0, dz);
            if is_complete_end_frame(level, corner) {
                return Some(corner);
            }
        }
    }
    None
}

/// Whether the 3 × 3 area with north-west corner `corner` is surrounded by frames with eyes,
/// each facing into the area (the corners of the 5 × 5 ring stay free).
fn is_complete_end_frame(level: &Level, corner: BlockPos) -> bool {
    let frame = |pos: BlockPos, facing: Direction| {
        let state = level.block(pos);
        state.is(ids::END_PORTAL_FRAME)
            && state.flag("eye")
            && state.direction("facing") == Some(facing)
    };
    (0..3).all(|i| {
        frame(corner.offset(i, 0, -1), Direction::South)
            && frame(corner.offset(i, 0, 3), Direction::North)
            && frame(corner.offset(-1, 0, i), Direction::East)
            && frame(corner.offset(3, 0, i), Direction::West)
    })
}

/// The end portal of `EnderEyeItem.useOn` after a frame got its eye: the 3 × 3 portal blocks
/// (what was there is destroyed with drops) and the global portal sound (level event 1038)
/// at the returned centre.
pub fn open_end_portal(level: &mut Level, corner: BlockPos) -> BlockPos {
    let portal = ids::END_PORTAL.default_state();
    for x in 0..3 {
        for z in 0..3 {
            let pos = corner.offset(x, 0, z);
            level.destroy_block(pos, true, None);
            level.set_block(pos, portal, update::CLIENTS);
        }
    }
    let centre = corner.offset(1, 0, 1);
    level.global_level_event(1038, centre, 0);
    centre
}

// --- End gateways ------------------------------------------------------------------------

/// The data of an End gateway (`TheEndGatewayBlockEntity`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Gateway {
    /// `age`: ticks since it was placed (the beam fades in over the first 200).
    pub age: i64,
    /// `teleportCooldown`.
    pub cooldown: i32,
    /// `exitPortal`.
    pub exit: Option<BlockPos>,
    /// `exactTeleport`: arrive exactly at the exit instead of on the tallest block near it.
    pub exact: bool,
}

impl Gateway {
    /// `loadAdditional`.
    pub fn load(nbt: &Compound) -> Gateway {
        let exit = match nbt.get("exit_portal") {
            Some(Tag::IntArray(v)) if v.len() == 3 => {
                Some(BlockPos::new(v[0], v[1], v[2])).filter(|p| is_in_spawnable_bounds(*p))
            }
            _ => None,
        };
        Gateway {
            age: match nbt.get("Age") {
                Some(Tag::Long(v)) => *v,
                _ => 0,
            },
            cooldown: 0,
            exit,
            exact: matches!(nbt.get("ExactTeleport"), Some(Tag::Byte(v)) if *v != 0),
        }
    }

    /// `saveAdditional`.
    pub fn save(&self, nbt: &mut Compound) {
        nbt.insert("Age", Tag::Long(self.age));
        if let Some(exit) = self.exit {
            nbt.insert("exit_portal", Tag::IntArray(vec![exit.x, exit.y, exit.z]));
        }
        if self.exact {
            nbt.insert("ExactTeleport", Tag::Byte(1));
        }
    }

    /// `isCoolingDown`.
    pub fn is_cooling_down(&self) -> bool {
        self.cooldown > 0
    }
}

/// `Level.isInSpawnableBounds`.
fn is_in_spawnable_bounds(pos: BlockPos) -> bool {
    const MAX_HORIZONTAL: i32 = 30_000_000;
    const MAX_VERTICAL: i32 = 20_000_000;
    (-MAX_VERTICAL..MAX_VERTICAL).contains(&pos.y)
        && (-MAX_HORIZONTAL..MAX_HORIZONTAL).contains(&pos.x)
        && (-MAX_HORIZONTAL..MAX_HORIZONTAL).contains(&pos.z)
}

/// The gateway at `pos`.
pub fn gateway(level: &Level, pos: BlockPos) -> Option<&Gateway> {
    match level.block_entity(pos) {
        Some(BlockEntity::EndGateway(gateway)) => Some(gateway),
        _ => None,
    }
}

fn gateway_mut(level: &mut Level, pos: BlockPos) -> Option<&mut Gateway> {
    match level.block_entity_mut(pos) {
        Some(BlockEntity::EndGateway(gateway)) => Some(gateway),
        _ => None,
    }
}

/// `TheEndGatewayBlockEntity.portalTick`.
pub fn gateway_tick(level: &mut Level, pos: BlockPos, state: BlockState) {
    let Some(gateway) = gateway_mut(level, pos) else {
        return;
    };
    let cooling = gateway.is_cooling_down();
    gateway.age += 1;
    if cooling {
        gateway.cooldown -= 1;
    } else if gateway.age % GATEWAY_ATTENTION_INTERVAL == 0 {
        trigger_gateway_cooldown(level, pos, state);
    }
}

/// `TheEndGatewayBlockEntity.triggerCooldown`: the cooldown and the beam event for clients.
pub fn trigger_gateway_cooldown(level: &mut Level, pos: BlockPos, state: BlockState) {
    if let Some(gateway) = gateway_mut(level, pos) {
        gateway.cooldown = GATEWAY_COOLDOWN;
        level.block_event(pos, state.block(), 1, 0);
    }
}

/// `EndGatewayBlock.triggerEvent` → `TheEndGatewayBlockEntity.triggerEvent`.
fn gateway_event(level: &mut Level, _: BlockState, pos: BlockPos, a: i32, _: i32) -> bool {
    match gateway_mut(level, pos) {
        Some(gateway) if a == 1 => {
            gateway.cooldown = GATEWAY_COOLDOWN;
            true
        }
        _ => false,
    }
}

/// `TheEndGatewayBlockEntity.findTallestBlock`: the highest full-cube block within `dist`
/// columns around `around` (its own column only if `allow_bedrock`; bedrock only then too),
/// `around` if there is none.
pub fn find_tallest_block(
    level: &Level,
    around: BlockPos,
    dist: i32,
    allow_bedrock: bool,
) -> BlockPos {
    let mut tallest: Option<BlockPos> = None;
    for xd in -dist..=dist {
        for zd in -dist..=dist {
            if xd == 0 && zd == 0 && !allow_bedrock {
                continue;
            }
            let floor = tallest.map_or(level.min_y(), |t| t.y);
            let mut y = level.max_y();
            while y > floor {
                let pos = BlockPos::new(around.x + xd, y, around.z + zd);
                let state = level.block(pos);
                if vanillite_worldgen::behaviour::is_full_block(state.collision_shape())
                    && (allow_bedrock || !state.is(ids::BEDROCK))
                {
                    tallest = Some(pos);
                    break;
                }
                y -= 1;
            }
        }
    }
    tallest.unwrap_or(around)
}

/// `TheEndGatewayBlockEntity.getPortalPosition` for a gateway with a known exit: the exit
/// itself for exact gateways, else above the tallest block near it (`findExitPosition`).
/// Gateways without an exit get one when the dragon fight places them.
pub fn gateway_destination(level: &Level, gateway: &Gateway) -> Option<BlockPos> {
    let exit = gateway.exit?;
    Some(if gateway.exact {
        exit
    } else {
        find_tallest_block(level, exit.offset(0, 2, 0), 5, false).offset(0, 1, 0)
    })
}

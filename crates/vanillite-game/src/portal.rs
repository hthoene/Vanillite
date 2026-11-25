//! Travelling through portals: a player's portal state (`PortalProcessor` and the portal
//! cooldown), where a nether portal leads (`NetherPortalBlock.getPortalDestination` with the
//! `PortalForcer`'s search and creation of exit portals), the End's arrival platform and its exit
//! portal (`EndPlatformFeature`, `EndPodiumFeature`).
//!
//! The functions here work on one level each: the world a player leaves computes what it knows
//! (the entry portal), the world it arrives in finds or builds the exit. The server moves the
//! player between the two.

use vanillite_base::{BlockPos, ChunkPos, Direction, Vec3};
use vanillite_chunk::HeightmapKind;
use vanillite_data::block::{BlockState, ids};
use vanillite_nbt::{Compound, Tag};
use vanillite_worldgen::behaviour::{HorizontalAxis, StateExt, relative_n};

use crate::level::Level;

/// The portal a player stands in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PortalKind {
    /// `NetherPortalBlock`.
    Nether,
    /// `EndPortalBlock`.
    End,
    /// `EndGatewayBlock`.
    Gateway,
}

/// `Player.getDimensionChangingDelay`: the portal cooldown after a teleport, in ticks.
pub const PLAYER_PORTAL_COOLDOWN: i32 = 10;

/// `PortalForcer.TICKET_RADIUS`: chunks kept loaded around an exit portal after arrival.
pub const PORTAL_TICKET_RADIUS: i32 = 3;

/// `ServerLevel.END_SPAWN_POINT`: where players arrive in the End.
pub const END_SPAWN_POINT: BlockPos = BlockPos::new(100, 50, 0);

/// `PortalProcessor`: the portal an entity is in and for how long.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Process {
    kind: PortalKind,
    entry: BlockPos,
    time: i32,
    inside_this_tick: bool,
}

/// The portal state of a player (`portalProcess`, `portalCooldown`, `seenCredits`, `wonGame`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PortalState {
    /// `portalCooldown`.
    pub cooldown: i32,
    process: Option<Process>,
    /// `seenCredits`: the player saw the end credits.
    pub seen_credits: bool,
    /// `wonGame`: the credits are showing; the player waits for its respawn.
    pub won_game: bool,
}

impl PortalState {
    /// `setAsInsidePortal(portal, pos)`.
    pub fn set_inside(&mut self, kind: PortalKind, pos: BlockPos) {
        if self.cooldown > 0 {
            self.cooldown = PLAYER_PORTAL_COOLDOWN;
            return;
        }
        match &mut self.process {
            Some(p) if p.kind == kind => {
                if !p.inside_this_tick {
                    p.entry = pos;
                    p.inside_this_tick = true;
                }
            }
            _ => {
                self.process = Some(Process {
                    kind,
                    entry: pos,
                    time: 0,
                    inside_this_tick: true,
                });
            }
        }
    }

    /// `handlePortal` up to the destination: the portal kind and entry block when the player
    /// teleports this tick. `can_use` is `canUsePortal`, `transition_time` the portal's
    /// `getPortalTransitionTime` for this player.
    pub fn handle(
        &mut self,
        can_use: bool,
        transition_time: impl FnOnce(PortalKind) -> i32,
    ) -> Option<(PortalKind, BlockPos)> {
        if self.cooldown > 0 {
            self.cooldown -= 1;
        }
        let p = self.process.as_mut()?;
        // `processPortalTeleportation`.
        let teleport = if p.inside_this_tick {
            p.inside_this_tick = false;
            can_use && {
                let time = p.time;
                p.time += 1;
                time >= transition_time(p.kind)
            }
        } else {
            p.time = (p.time - 4).max(0);
            false
        };
        if teleport {
            self.cooldown = PLAYER_PORTAL_COOLDOWN;
            return Some((p.kind, p.entry));
        }
        if p.time <= 0 {
            self.process = None;
        }
        None
    }

    /// Reads `PortalCooldown` and `seenCredits` of a player data file.
    pub fn load(data: &Compound) -> PortalState {
        PortalState {
            cooldown: match data.get("PortalCooldown") {
                Some(Tag::Int(v)) => *v,
                _ => 0,
            },
            process: None,
            seen_credits: matches!(data.get("seenCredits"), Some(Tag::Byte(v)) if *v != 0),
            won_game: false,
        }
    }

    /// Writes `PortalCooldown` and `seenCredits`.
    pub fn save(&self, out: &mut Compound) {
        out.insert("PortalCooldown", Tag::Int(self.cooldown));
        out.insert("seenCredits", Tag::Byte(i8::from(self.seen_credits)));
    }
}

// --- Nether portals ----------------------------------------------------------------------

/// `BlockUtil.FoundRectangle`: a rectangle of blocks in a vertical plane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FoundRectangle {
    /// `minCorner`.
    pub min_corner: BlockPos,
    /// `axis1Size`: the horizontal size.
    pub width: i32,
    /// `axis2Size`: the height.
    pub height: i32,
}

fn negative(axis: HorizontalAxis) -> Direction {
    match axis {
        HorizontalAxis::X => Direction::West,
        HorizontalAxis::Z => Direction::North,
    }
}

/// `BlockUtil.getLimit`.
fn limit(test: &impl Fn(BlockPos) -> bool, from: BlockPos, direction: Direction, max: i32) -> i32 {
    let mut n = 0;
    while n < max && test(relative_n(from, direction, n + 1)) {
        n += 1;
    }
    n
}

/// `BlockUtil.getMaxRectangleLocation`: the largest rectangle under a histogram as (start,
/// end inclusive, height).
fn max_rectangle(columns: &[i32]) -> (i32, i32, i32) {
    let (mut max_start, mut max_end, mut max_height) = (0_i32, 0_i32, 0_i32);
    let mut stack: Vec<usize> = vec![0];
    for column in 1..=columns.len() {
        let height = columns.get(column).copied().unwrap_or(0);
        while let Some(&top) = stack.last() {
            let stack_height = columns[top];
            if height >= stack_height {
                stack.push(column);
                break;
            }
            stack.pop();
            let start = stack.last().map_or(0, |&t| t as i32 + 1);
            if stack_height * (column as i32 - start) > max_height * (max_end - max_start) {
                max_end = column as i32;
                max_start = start;
                max_height = stack_height;
            }
        }
        if stack.is_empty() {
            stack.push(column);
        }
    }
    (max_start, max_end - 1, max_height)
}

/// `BlockUtil.getLargestRectangleAround(center, axis, 21, Y, 21, test)`: the largest
/// rectangle of blocks passing `test` in the vertical plane of `axis` through `center`.
pub fn largest_rectangle_around(
    center: BlockPos,
    axis: HorizontalAxis,
    test: impl Fn(BlockPos) -> bool,
) -> FoundRectangle {
    const LIMIT: i32 = 21;
    let neg1 = negative(axis);
    let pos1 = neg1.opposite();
    let negative1 = limit(&test, center, neg1, LIMIT);
    let positive1 = limit(&test, center, pos1, LIMIT);
    let center1 = negative1 as usize;
    let len = (negative1 + 1 + positive1) as usize;
    let mut bounds = vec![(0_i32, 0_i32); len];
    bounds[center1] = (
        limit(&test, center, Direction::Down, LIMIT),
        limit(&test, center, Direction::Up, LIMIT),
    );
    let center2 = bounds[center1].0;
    for i in 1..=center1 {
        let last = bounds[center1 - (i - 1)];
        let from = relative_n(center, neg1, i as i32);
        bounds[center1 - i] = (
            limit(&test, from, Direction::Down, last.0),
            limit(&test, from, Direction::Up, last.1),
        );
    }
    for i in 1..=positive1 as usize {
        let last = bounds[center1 + i - 1];
        let from = relative_n(center, pos1, i as i32);
        bounds[center1 + i] = (
            limit(&test, from, Direction::Down, last.0),
            limit(&test, from, Direction::Up, last.1),
        );
    }
    let (mut min1, mut min2, mut size1, mut size2) = (0_i32, 0_i32, 0_i32, 0_i32);
    let mut columns = vec![0_i32; len];
    for i2 in (0..=center2).rev() {
        for (column, &(down, up)) in columns.iter_mut().zip(&bounds) {
            let (lo, hi) = (center2 - down, center2 + up);
            *column = if i2 >= lo && i2 <= hi { hi + 1 - i2 } else { 0 };
        }
        let (start, end, height) = max_rectangle(&columns);
        let new1 = 1 + end - start;
        if new1 * height > size1 * size2 {
            min1 = start;
            min2 = i2;
            size1 = new1;
            size2 = height;
        }
    }
    let corner = relative_n(center, pos1, min1 - center1 as i32).offset(0, min2 - center2, 0);
    FoundRectangle {
        min_corner: corner,
        width: size1,
        height: size2,
    }
}

/// The nether portal a player leaves through: its axis and where in it the player stands
/// (`getDimensionTransitionFromExit`, source side).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EntryPortal {
    /// The entry portal's axis.
    pub axis: HorizontalAxis,
    /// `getRelativePortalPosition`: right (0–1), up (0–1) and forward offset.
    pub offset: Vec3,
}

/// `PortalShape.getRelativePosition` for an entity of `width` × `height` at `pos`.
pub fn relative_position(
    rect: &FoundRectangle,
    axis: HorizontalAxis,
    pos: Vec3,
    width: f32,
    height: f32,
) -> Vec3 {
    let coord = |v: Vec3, axis: HorizontalAxis| match axis {
        HorizontalAxis::X => v.x,
        HorizontalAxis::Z => v.z,
    };
    // `axis1Size - width` is int minus float: a float.
    let free_width = f64::from(rect.width as f32 - width);
    let free_height = f64::from(rect.height as f32 - height);
    let bottom = rect.min_corner;
    let right = if free_width > 0.0 {
        let start = f64::from(axis.coord(bottom)) + f64::from(width) / 2.0;
        ((coord(pos, axis) - start) / free_width).clamp(0.0, 1.0)
    } else {
        0.5
    };
    let up = if free_height > 0.0 {
        ((pos.y - f64::from(bottom.y)) / free_height).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let forward_axis = axis.other();
    let forward = coord(pos, forward_axis) - (f64::from(forward_axis.coord(bottom)) + 0.5);
    Vec3::new(right, up, forward)
}

/// The entry portal at `entry` for an entity at `pos`: the largest rectangle of the same
/// portal state around it, or the default offset if the block is no longer a portal.
pub fn entry_portal(
    level: &Level,
    entry: BlockPos,
    pos: Vec3,
    width: f32,
    height: f32,
) -> EntryPortal {
    let state = level.block(entry);
    match HorizontalAxis::of(state) {
        Some(axis) => {
            let rect = largest_rectangle_around(entry, axis, |p| level.block(p) == state);
            EntryPortal {
                axis,
                offset: relative_position(&rect, axis, pos, width, height),
            }
        }
        None => EntryPortal {
            axis: HorizontalAxis::X,
            offset: Vec3::new(0.5, 0.0, 0.0),
        },
    }
}

/// `createDimensionTransition`: where an entity of `width` × `height` arrives in the exit
/// portal `exit` (whose blocks have `exit_axis`) and the yaw it turns by.
pub fn exit_position(
    exit: &FoundRectangle,
    exit_axis: HorizontalAxis,
    entry: &EntryPortal,
    width: f32,
    height: f32,
) -> (Vec3, f32) {
    let rotation = if entry.axis == exit_axis { 0.0 } else { 90.0 };
    let (w, h) = (f64::from(width), f64::from(height));
    let right = w / 2.0 + (f64::from(exit.width) - w) * entry.offset.x;
    let up = (f64::from(exit.height) - h) * entry.offset.y;
    let forward = 0.5 + entry.offset.z;
    let b = exit.min_corner;
    let x_aligned = exit_axis == HorizontalAxis::X;
    let pos = Vec3::new(
        f64::from(b.x) + if x_aligned { right } else { forward },
        f64::from(b.y) + up,
        f64::from(b.z) + if x_aligned { forward } else { right },
    );
    (pos, rotation)
}

/// `getPortalDestination`'s first guess: the entity's position scaled by
/// `DimensionType.getTeleportationScale` (the coordinate scale of the old dimension over the
/// new one), clamped to the world border.
#[cfg(test)]
fn scaled_exit(border: &crate::level::border::WorldBorder, pos: Vec3, scale: f64) -> BlockPos {
    border.clamp_to_bounds(pos.x * scale, pos.y, pos.z * scale)
}

/// `PortalForcer`'s search radius: 16 blocks into the Nether, 128 out of it.
pub fn search_radius(to_nether: bool) -> i32 {
    if to_nether { 16 } else { 128 }
}

/// The chunk radius around the scaled exit that must be loaded before the exit is searched
/// and built: the search (`ensureLoadedAndValid`) and the frame candidates of `createPortal`.
pub fn search_chunk_radius(to_nether: bool) -> i32 {
    (search_radius(to_nether) / 16).max(PORTAL_TICKET_RADIUS)
}

/// `PortalForcer.findClosestPortalPosition`: the portal block closest to `approx` within the
/// square of `radius` (lowest first among equally close ones) in loaded chunks.
pub fn find_closest_portal(level: &Level, approx: BlockPos, radius: i32) -> Option<BlockPos> {
    let center = approx.chunk();
    let chunks = radius.div_euclid(16) + 1;
    let portal = ids::NETHER_PORTAL;
    let mut best: Option<(f64, BlockPos)> = None;
    for cx in center.x - chunks..=center.x + chunks {
        for cz in center.z - chunks..=center.z + chunks {
            let Some(chunk) = level.chunks.chunk(ChunkPos::new(cx, cz)) else {
                continue;
            };
            let min_y = chunk.chunk.min_y();
            for (index, section) in chunk.chunk.sections().iter().enumerate() {
                if !section
                    .blocks()
                    .may_contain(|v| BlockState(v).block() == portal)
                {
                    continue;
                }
                let base_y = min_y + index as i32 * 16;
                for y in 0..16 {
                    for z in 0..16 {
                        for x in 0..16 {
                            if BlockState(section.block(x, y, z)).block() != portal {
                                continue;
                            }
                            let pos = BlockPos::new(
                                cx * 16 + x as i32,
                                base_y + y as i32,
                                cz * 16 + z as i32,
                            );
                            if (pos.x - approx.x).abs() > radius
                                || (pos.z - approx.z).abs() > radius
                                || !level.border.is_block_within_bounds(pos)
                            {
                                continue;
                            }
                            let dist = pos.dist_sqr(approx);
                            let closer =
                                best.is_none_or(|(d, b)| dist < d || (dist == d && pos.y < b.y));
                            if closer {
                                best = Some((dist, pos));
                            }
                        }
                    }
                }
            }
        }
    }
    best.map(|(_, pos)| pos)
}

/// `BlockPos.spiralAround(center, radius, first, second)`.
fn spiral_around(
    center: BlockPos,
    radius: i32,
    first: Direction,
    second: Direction,
) -> impl Iterator<Item = BlockPos> {
    let directions = [first, second, first.opposite(), second.opposite()];
    let legs = 4 * radius;
    let mut last = center.relative(second);
    let (mut leg, mut leg_size, mut leg_index) = (-1_i32, 0_i32, 0_i32);
    std::iter::from_fn(move || {
        let cursor = last.relative(directions[((leg + 4) % 4) as usize]);
        last = cursor;
        if leg_index >= leg_size {
            if leg >= legs {
                return None;
            }
            leg += 1;
            leg_index = 0;
            leg_size = leg / 2 + 1;
        }
        leg_index += 1;
        Some(cursor)
    })
}

/// `Direction.getClockWise` of a horizontal direction.
fn clockwise(direction: Direction) -> Direction {
    match direction {
        Direction::North => Direction::East,
        Direction::East => Direction::South,
        Direction::South => Direction::West,
        Direction::West => Direction::North,
        other => other,
    }
}

/// `level.getHeight(type, x, z)`: the first free block row of a column.
pub fn height_at(level: &Level, kind: HeightmapKind, x: i32, z: i32) -> i32 {
    let Some(chunk) = level.chunks.chunk(BlockPos::new(x, 0, z).chunk()) else {
        return level.min_y();
    };
    let Some(map) = chunk.chunk.heightmaps().get(kind) else {
        return level.min_y();
    };
    #[allow(clippy::cast_sign_loss, clippy::as_conversions)]
    let relative = map.get((x & 15) as u32, (z & 15) as u32) as i32;
    relative + chunk.chunk.min_y()
}

/// `canPortalReplaceBlock`.
fn can_portal_replace(level: &Level, pos: BlockPos) -> bool {
    let state = level.block(pos);
    crate::block::can_be_replaced(state) && state.fluid_state().is_empty()
}

/// `canHostFrame`.
fn can_host_frame(level: &Level, origin: BlockPos, direction: Direction, offset: i32) -> bool {
    let side = clockwise(direction);
    let [dx, _, dz] = direction.normal();
    let [sx, _, sz] = side.normal();
    for width in -1..3 {
        for height in -1..4 {
            let pos = origin.offset(dx * width + sx * offset, height, dz * width + sz * offset);
            if height < 0 && !crate::block::is_solid(level.block(pos)) {
                return false;
            }
            if height >= 0 && !can_portal_replace(level, pos) {
                return false;
            }
        }
    }
    true
}

/// `PortalForcer.createPortal`: builds an exit portal of `axis` near `origin` (on the closest
/// spot that hosts a whole frame, else a partial one, else on a platform at a clamped height)
/// and returns its inside. `logical_height` is the dimension type's.
pub fn create_portal(
    level: &mut Level,
    origin: BlockPos,
    axis: HorizontalAxis,
    logical_height: i32,
) -> Option<FoundRectangle> {
    let direction = axis.positive();
    let [dx, _, dz] = direction.normal();
    let min_y = level.min_y();
    let max_placeable = level.max_y().min(min_y + logical_height - 1);
    let mut full: Option<(f64, BlockPos)> = None;
    let mut partial: Option<(f64, BlockPos)> = None;
    for column in spiral_around(origin, 16, Direction::East, Direction::South) {
        let height = max_placeable.min(height_at(
            level,
            HeightmapKind::MotionBlocking,
            column.x,
            column.z,
        ));
        if !level.border.is_block_within_bounds(column)
            || !level
                .border
                .is_block_within_bounds(column.relative(direction))
        {
            continue;
        }
        let mut y = height;
        while y >= min_y {
            let at = BlockPos::new(column.x, y, column.z);
            if can_portal_replace(level, at) {
                let first_empty = y;
                while y > min_y
                    && can_portal_replace(level, BlockPos::new(column.x, y - 1, column.z))
                {
                    y -= 1;
                }
                if y + 4 <= max_placeable {
                    let delta = first_empty - y;
                    if delta <= 0 || delta >= 3 {
                        let candidate = BlockPos::new(column.x, y, column.z);
                        if can_host_frame(level, candidate, direction, 0) {
                            let distance = origin.dist_sqr(candidate);
                            if can_host_frame(level, candidate, direction, -1)
                                && can_host_frame(level, candidate, direction, 1)
                                && full.is_none_or(|(d, _)| d > distance)
                            {
                                full = Some((distance, candidate));
                            }
                            if full.is_none() && partial.is_none_or(|(d, _)| d > distance) {
                                partial = Some((distance, candidate));
                            }
                        }
                    }
                }
            }
            y -= 1;
        }
    }
    let obsidian = ids::OBSIDIAN.default_state();
    let corner = match full.or(partial) {
        Some((_, pos)) => pos,
        None => {
            let min_start = (min_y + 1).max(70);
            let max_start = max_placeable - 9;
            if max_start < min_start {
                return None;
            }
            let start = level.border.clamp_to_bounds(
                f64::from(origin.x - dx),
                f64::from(origin.y.clamp(min_start, max_start)),
                f64::from(origin.z - dz),
            );
            let [sx, _, sz] = clockwise(direction).normal();
            for side in -1..2 {
                for width in 0..2 {
                    for height in -1..3 {
                        let state = if height < 0 {
                            obsidian
                        } else {
                            BlockState::AIR
                        };
                        let pos =
                            start.offset(dx * width + sx * side, height, dz * width + sz * side);
                        level.set_block_and_update(pos, state);
                    }
                }
            }
            start
        }
    };
    for width in -1..3 {
        for height in -1..4 {
            if width == -1 || width == 2 || height == -1 || height == 3 {
                level.set_block_and_update(corner.offset(dx * width, height, dz * width), obsidian);
            }
        }
    }
    let portal = ids::NETHER_PORTAL.default_state().set("axis", axis.name());
    for width in 0..2 {
        for height in 0..3 {
            level.set_block(
                corner.offset(dx * width, height, dz * width),
                portal,
                crate::level::update::CLIENTS | crate::level::update::KNOWN_SHAPE,
            );
        }
    }
    Some(FoundRectangle {
        min_corner: corner,
        width: 2,
        height: 3,
    })
}

// --- The End -----------------------------------------------------------------------------

/// `EndPlatformFeature.createEndPlatform(level, origin, true)`: a 5 × 5 obsidian floor at
/// `origin.y - 1` with three rows of air above; what was in the way drops.
pub fn create_end_platform(level: &mut Level, origin: BlockPos) {
    for dz in -2..=2 {
        for dx in -2..=2 {
            for dy in -1..3 {
                let pos = origin.offset(dx, dy, dz);
                let block = if dy == -1 { ids::OBSIDIAN } else { ids::AIR };
                if level.block(pos).block() != block {
                    level.destroy_block(pos, true, None);
                    level.set_block_and_update(pos, block.default_state());
                }
            }
        }
    }
}

/// `EndPodiumFeature(active)` at `origin` on the live level: the bedrock exit portal frame,
/// filled with end portal blocks when `active` (what was there drops).
pub fn place_exit_portal(level: &mut Level, origin: BlockPos, active: bool) {
    let bedrock = ids::BEDROCK.default_state();
    let set_or_drop = |level: &mut Level, pos: BlockPos, state: BlockState| {
        if active {
            if level.block(pos).block() != state.block() {
                level.destroy_block(pos, true, None);
                level.set_block_and_update(pos, state);
            }
        } else {
            level.set_block_and_update(pos, state);
        }
    };
    // `BlockPos.betweenClosed`: x fastest, then y, then z.
    for z in origin.z - 4..=origin.z + 4 {
        for y in origin.y - 1..=origin.y + 32 {
            for x in origin.x - 4..=origin.x + 4 {
                let pos = BlockPos::new(x, y, z);
                let dist = pos.dist_sqr(origin);
                let inside_rim = dist < 2.5 * 2.5;
                if !inside_rim && dist >= 3.5 * 3.5 {
                    continue;
                }
                if y < origin.y {
                    if inside_rim {
                        level.set_block_and_update(pos, bedrock);
                    } else {
                        set_or_drop(level, pos, ids::END_STONE.default_state());
                    }
                } else if y > origin.y {
                    set_or_drop(level, pos, BlockState::AIR);
                } else if !inside_rim {
                    level.set_block_and_update(pos, bedrock);
                } else if active {
                    set_or_drop(level, pos, ids::END_PORTAL.default_state());
                } else {
                    level.set_block_and_update(pos, BlockState::AIR);
                }
            }
        }
    }
    for y in 0..4 {
        level.set_block_and_update(origin.offset(0, y, 0), bedrock);
    }
    let pillar = origin.offset(0, 2, 0);
    for facing in [
        Direction::North,
        Direction::East,
        Direction::South,
        Direction::West,
    ] {
        let torch = ids::WALL_TORCH
            .default_state()
            .set_direction("facing", facing);
        level.set_block_and_update(pillar.relative(facing), torch);
    }
}

/// `EnderDragonFight.spawnExitPortal`'s location: below the top of the column at 0, 0, down
/// through bedrock to y 63, at least one above the bottom.
pub fn exit_portal_location(level: &Level) -> BlockPos {
    let top = height_at(level, HeightmapKind::MotionBlockingNoLeaves, 0, 0);
    let mut pos = BlockPos::new(0, top - 1, 0);
    while level.block(pos).is(ids::BEDROCK) && pos.y > 63 {
        pos = pos.offset(0, -1, 0);
    }
    BlockPos::new(pos.x, pos.y.max(level.min_y() + 1), pos.z)
}

/// Whether an exit portal frame (`EnderDragonFight.exitPortalPattern`) is centred at `center`:
/// the bedrock pillar, the ring with the rim and the bedrock base below.
pub fn is_exit_portal_at(level: &Level, center: BlockPos) -> bool {
    let bedrock =
        |dx: i32, dy: i32, dz: i32| level.block(center.offset(dx, dy, dz)).is(ids::BEDROCK);
    const RING: [&str; 7] = [
        "  ###  ", " #   # ", "#     #", "#  #  #", "#     #", " #   # ", "  ###  ",
    ];
    const BASE: [&str; 7] = [
        "       ", "  ###  ", " ##### ", " ##### ", " ##### ", "  ###  ", "       ",
    ];
    let layer = |rows: &[&str; 7], dy: i32| {
        rows.iter().zip(-3..).all(|(row, dz)| {
            row.bytes()
                .zip(-3..)
                .all(|(c, dx)| c != b'#' || bedrock(dx, dy, dz))
        })
    };
    (1..=3).all(|dy| bedrock(0, dy, 0)) && layer(&RING, 0) && layer(&BASE, -1)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn coordinates_scale_by_eight_between_overworld_and_nether() {
        // Overworld (scale 1) to Nether (scale 8): 1 / 8.
        let into = scaled_exit(
            &crate::level::border::WorldBorder::default(),
            Vec3::new(100.7, 64.0, -35.2),
            1.0 / 8.0,
        );
        assert_eq!(into, BlockPos::new(12, 64, -5));
        // Back: 8.
        let out = scaled_exit(
            &crate::level::border::WorldBorder::default(),
            Vec3::new(12.5, 70.0, -4.4),
            8.0,
        );
        assert_eq!(out, BlockPos::new(100, 70, -36));
        // Clamped to the world border.
        let far = scaled_exit(
            &crate::level::border::WorldBorder::default(),
            Vec3::new(4_000_000.0, 80.0, -4_000_000.0),
            8.0,
        );
        assert_eq!(far, BlockPos::new(29_999_983, 80, -29_999_984));
    }

    #[test]
    fn largest_rectangle_finds_the_portal_inside() {
        // A 3 × 4 portal (x 10..=12, y 64..=67) at z 5; the entry block is in the middle.
        let inside = |p: BlockPos| (10..=12).contains(&p.x) && (64..=67).contains(&p.y) && p.z == 5;
        let rect = largest_rectangle_around(BlockPos::new(11, 66, 5), HorizontalAxis::X, inside);
        assert_eq!(
            rect,
            FoundRectangle {
                min_corner: BlockPos::new(10, 64, 5),
                width: 3,
                height: 4,
            }
        );
    }

    #[test]
    fn players_keep_their_place_in_the_portal() {
        let entry_rect = FoundRectangle {
            min_corner: BlockPos::new(0, 64, 0),
            width: 2,
            height: 3,
        };
        // A player at the right edge, standing on the bottom, centred in depth.
        let offset = relative_position(
            &entry_rect,
            HorizontalAxis::X,
            Vec3::new(1.7, 64.0, 0.5),
            0.6,
            1.8,
        );
        assert!((offset.x - 1.0).abs() < 1e-9 && offset.y == 0.0 && offset.z == 0.0);
        let entry = EntryPortal {
            axis: HorizontalAxis::X,
            offset,
        };
        let exit = FoundRectangle {
            min_corner: BlockPos::new(-20, 70, 8),
            width: 2,
            height: 3,
        };
        let (pos, yaw) = exit_position(&exit, HorizontalAxis::Z, &entry, 0.6, 1.8);
        // Turned by 90 degrees into a Z portal: right is along z, forward along x.
        assert_eq!(yaw, 90.0);
        assert!((pos.x - -19.5).abs() < 1e-9);
        assert!((pos.y - 70.0).abs() < 1e-9);
        assert!((pos.z - (8.0 + 0.3 + 1.4)).abs() < 1e-6);
    }

    #[test]
    fn portal_processor_waits_then_cools_down() {
        let mut state = PortalState::default();
        let pos = BlockPos::new(1, 2, 3);
        for tick in 0..=3 {
            state.set_inside(PortalKind::Nether, pos);
            let teleport = state.handle(true, |_| 3);
            assert_eq!(teleport.is_some(), tick == 3, "tick {tick}");
        }
        assert_eq!(state.cooldown, PLAYER_PORTAL_COOLDOWN);
        // Standing in the portal during the cooldown keeps it up.
        state.set_inside(PortalKind::Nether, pos);
        assert_eq!(state.handle(true, |_| 0), None);
        assert_eq!(state.cooldown, PLAYER_PORTAL_COOLDOWN - 1);
    }

    #[test]
    fn spiral_starts_at_the_centre_and_walks_outwards() {
        let first: Vec<BlockPos> =
            spiral_around(BlockPos::new(0, 0, 0), 1, Direction::East, Direction::South)
                .take(4)
                .collect();
        assert_eq!(
            first,
            vec![
                BlockPos::new(0, 0, 0),
                BlockPos::new(1, 0, 0),
                BlockPos::new(1, 0, 1),
                BlockPos::new(0, 0, 1),
            ]
        );
        assert_eq!(
            spiral_around(
                BlockPos::new(0, 0, 0),
                16,
                Direction::East,
                Direction::South
            )
            .count(),
            33 * 33
        );
    }
}

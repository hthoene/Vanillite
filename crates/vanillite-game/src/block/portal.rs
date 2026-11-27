//! Portal blocks: lighting a nether portal frame with fire (`BaseFireBlock.onPlace`), the End
//! portal frame filled with eyes of ender (`EnderEyeItem.useOn`) and the End gateway's block
//! entity (`TheEndGatewayBlockEntity`). Travelling through portals is [`crate::portal`].

use vanillite_base::random::{LegacyRandom, RandomSource};
use vanillite_base::{BlockPos, ChunkPos, Direction};
use vanillite_chunk::ProtoChunk;
use vanillite_data::block::{BlockId, BlockState, ids};
use vanillite_nbt::{Compound, Tag};
use vanillite_worldgen::behaviour::{HorizontalAxis, PortalShape, PosExt, StateExt};

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
    } else if block == ids::NETHER_PORTAL {
        b.random_tick = Some(nether_portal_random_tick);
    }
}

/// `NetherPortalBlock.randomTick`: now and then a zombified piglin spawns on the block below
/// the portal, with a portal cooldown (`EntityType.spawn(level, pos.above(), STRUCTURE)`).
fn nether_portal_random_tick(level: &mut Level, _state: BlockState, pos: BlockPos) {
    use crate::mob::Kind;
    use crate::survival::rules::Difficulty;
    let difficulty = level.settings.difficulty;
    if !level.settings.spawning_monsters
        || difficulty == Difficulty::Peaceful
        || !level.spawner_env.env.nether_portal_spawns_piglins
        || level.random().next_int_bounded(2000) >= difficulty.id()
        || !any_player_close_enough_for_spawning(level, pos.chunk())
    {
        return;
    }
    let mut pos = pos;
    while level.block(pos).is(ids::NETHER_PORTAL) {
        pos = pos.below();
    }
    let kind = Kind::ZombifiedPiglin;
    if !crate::mob::spawn::is_valid_spawn(level, level.block(pos), kind.type_id()) {
        return;
    }
    let at = pos.above();
    let mut mob = crate::mob::MobData::new(kind, crate::mob::new_entity_seed());
    let y_rot = vanillite_base::math::wrap_degrees_f(level.random().next_float() * 360.0);
    let env = level.spawner_env.env;
    let extra = crate::mob::spawn::with_level_random(level, |level, random| {
        crate::mob::spawn::finalize(
            level,
            None,
            &env,
            &mut mob,
            at,
            difficulty,
            0,
            crate::mob::fam::SpawnReason::Structure,
            random,
        )
    });
    let position = vanillite_base::Vec3::new(
        f64::from(at.x) + 0.5,
        f64::from(at.y),
        f64::from(at.z) + 0.5,
    );
    let id = crate::mob::spawn::add_mob_with(
        level,
        mob,
        position,
        y_rot,
        extra,
        crate::mob::fam::SpawnReason::Structure,
    );
    // `mob.playAmbientSound()`.
    if let Some(m) = level.entities.mob_mut(id) {
        let sound = crate::mob::kinds::ambient_sound(m);
        let a = m.random.next_float();
        let b = m.random.next_float();
        let pitch = if m.baby {
            (a - b) * 0.2 + 1.5
        } else {
            (a - b) * 0.2 + 1.0
        };
        let volume = crate::mob::fam::mob_sound_volume(m);
        let silent = m.silent;
        if let Some(sound) = sound.filter(|_| !silent) {
            let p = position;
            let sound = super::sound_id(sound.trim_start_matches("minecraft:"));
            let source = crate::level::SOUND_HOSTILE;
            level.play_sound(None, p.x, p.y, p.z, sound, source, volume, pitch);
        }
    }
    // `setPortalCooldown` of the piglin and its vehicle.
    let vehicle = level.entities.vehicle_of(id);
    for e in std::iter::once(id).chain(vehicle) {
        level.entities.portals.entry(e).or_default().cooldown = 300;
    }
}

/// `ServerLevel.anyPlayerCloseEnoughForSpawning(chunk)`: a player within 5 chunks, or within 8
/// chunks and 128 blocks of the chunk's centre (spectators do not count).
fn any_player_close_enough_for_spawning(level: &Level, chunk: ChunkPos) -> bool {
    level.entities.players.iter().any(|(id, p)| {
        if p.game_mode == vanillite_config::GameMode::Spectator {
            return false;
        }
        let Some(pos) = level.entities.store.get(id).map(|e| e.pos()) else {
            return false;
        };
        let own = BlockPos::containing(pos.x, pos.y, pos.z).chunk();
        let d = (own.x - chunk.x).abs().max((own.z - chunk.z).abs());
        d <= 5
            || (d <= crate::mob::nearby::RADIUS
                && crate::mob::nearby::chunk_center_distance_sqr(chunk, pos)
                    < crate::mob::nearby::SPAWN_DISTANCE_SQR)
    })
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
            && portal_may_form(level, &shape)
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

/// Whether plugins let fire light the portal `shape` (`PORTAL_CREATE` without an entity,
/// Bukkit's `PortalCreateEvent` of `FIRE`); a cancelled portal leaves the fire.
fn portal_may_form(level: &Level, shape: &PortalShape) -> bool {
    let Some(hooks) = level.hooks_for(vanillite_abi::event::PORTAL_CREATE) else {
        return true;
    };
    // The inside spans `width` blocks from `bottom_left` towards `right`.
    let [rx, _, rz] = shape.right.normal();
    let far = shape
        .bottom_left
        .offset(rx * (shape.width - 1), 0, rz * (shape.width - 1));
    let corner = BlockPos::new(
        shape.bottom_left.x.min(far.x),
        shape.bottom_left.y,
        shape.bottom_left.z.min(far.z),
    );
    let mut ev = crate::hooks::Event {
        pos: corner,
        ..crate::hooks::Event::new(vanillite_abi::event::PORTAL_CREATE)
    };
    hooks.event(&mut ev);
    !ev.cancelled
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

/// `BaseFireBlock.isPortal` for any direction (a dispenser's flint and steel): a vertical
/// direction takes a random horizontal axis (`Plane.HORIZONTAL.getRandomAxis`, one level
/// random draw) once a frame block is next to `pos`.
pub fn lights_portal_any(level: &mut Level, pos: BlockPos, forward: Direction) -> bool {
    if HorizontalAxis::of_direction(forward).is_some() {
        return lights_portal(level, pos, forward);
    }
    if !level.settings.portal_dimension {
        return false;
    }
    let next_to_frame = Direction::ALL
        .iter()
        .any(|&face| vanillite_worldgen::behaviour::is_frame(level.block(pos.relative(face))));
    if !next_to_frame {
        return false;
    }
    let preferred = if level.random().next_int_bounded(2) == 0 {
        HorizontalAxis::X
    } else {
        HorizontalAxis::Z
    };
    PortalShape::find_empty(&*level, pos, preferred).is_some()
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
/// Gateways in the End without an exit get one from [`create_gateway_exit`] first.
pub fn gateway_destination(level: &Level, gateway: &Gateway) -> Option<BlockPos> {
    let exit = gateway.exit?;
    Some(if gateway.exact {
        exit
    } else {
        find_tallest_block(level, exit.offset(0, 2, 0), 5, false).offset(0, 1, 0)
    })
}

/// Where a gateway sends an entity now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GatewayExit {
    /// To this block (its bottom centre).
    At(BlockPos),
    /// Nowhere: no gateway there, or no exit outside the End.
    Nowhere,
    /// The exit has to be created or its chunks loaded first.
    Wait,
}

/// `TheEndGatewayBlockEntity.getPortalPosition` as far as it needs no chunk loading and no
/// new exit: exact exits, and exits whose surroundings (`findExitPosition`) are loaded.
pub fn gateway_exit_now(level: &Level, entry: BlockPos, in_end: bool) -> GatewayExit {
    let Some(g) = gateway(level, entry) else {
        return GatewayExit::Nowhere;
    };
    match g.exit {
        None if in_end => GatewayExit::Wait,
        None => GatewayExit::Nowhere,
        Some(exit) => {
            let center = exit.chunk();
            let loaded = g.exact
                || (center.x - 1..=center.x + 1).all(|x| {
                    (center.z - 1..=center.z + 1)
                        .all(|z| level.chunks.chunk(ChunkPos::new(x, z)).is_some())
                });
            match gateway_destination(level, g) {
                Some(pos) if loaded => GatewayExit::At(pos),
                Some(_) => GatewayExit::Wait,
                None => GatewayExit::Nowhere,
            }
        }
    }
}

// --- The End's exit portal and the dragon fight's gateways --------------------------------

/// `EndPodiumFeature.place` at `origin` (the portal layer's centre) on the live world;
/// `active` fills the portal.
pub fn place_podium(level: &mut Level, origin: BlockPos, active: bool) {
    let bedrock = ids::BEDROCK.default_state();
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
                        set_or_drop(level, pos, ids::END_STONE, active);
                    }
                } else if y > origin.y {
                    set_or_drop(level, pos, ids::AIR, active);
                } else if !inside_rim {
                    level.set_block_and_update(pos, bedrock);
                } else if active {
                    drop_previous_and_set(level, pos, ids::END_PORTAL);
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
    // `Direction.Plane.HORIZONTAL` order.
    for (dir, facing) in [
        (Direction::North, "north"),
        (Direction::East, "east"),
        (Direction::South, "south"),
        (Direction::West, "west"),
    ] {
        let torch = ids::WALL_TORCH.default_state();
        let torch = torch.with("facing", facing).unwrap_or(torch);
        level.set_block_and_update(pillar.relative(dir), torch);
    }
}

fn set_or_drop(level: &mut Level, pos: BlockPos, block: BlockId, active: bool) {
    if active {
        drop_previous_and_set(level, pos, block);
    } else {
        level.set_block_and_update(pos, block.default_state());
    }
}

/// `dropPreviousAndSetBlock`.
fn drop_previous_and_set(level: &mut Level, pos: BlockPos, block: BlockId) {
    if level.block(pos).block() != block {
        level.destroy_block(pos, true, None);
        level.set_block_and_update(pos, block.default_state());
    }
}

/// The exit portal frame (`exitPortalPattern`) with its portal layer centred at `c`: the
/// bedrock pillar above, the ring with its centre, and the disc below.
pub fn portal_frame_at(level: &Level, c: BlockPos) -> bool {
    let bedrock = |p: BlockPos| level.block(p).block() == ids::BEDROCK;
    if !(1..=3).all(|dy| bedrock(c.offset(0, dy, 0))) || !bedrock(c) {
        return false;
    }
    const RING: [&str; 7] = [
        "  ###  ", " #   # ", "#     #", "#  #  #", "#     #", " #   # ", "  ###  ",
    ];
    const DISC: [&str; 7] = [
        "       ", "  ###  ", " ##### ", " ##### ", " ##### ", "  ###  ", "       ",
    ];
    for (layer, dy) in [(RING, 0), (DISC, -1)] {
        for (row, line) in layer.iter().enumerate() {
            for (col, ch) in line.bytes().enumerate() {
                if ch == b'#' {
                    #[allow(clippy::cast_possible_wrap, clippy::cast_possible_truncation)]
                    let p = c.offset(col as i32 - 3, dy, row as i32 - 3);
                    if !bedrock(p) {
                        return false;
                    }
                }
            }
        }
    }
    true
}

/// Every position of the frame's 7×5×7 box (for clearing it before a respawn).
pub fn portal_box(c: BlockPos) -> impl Iterator<Item = BlockPos> {
    (-3..=3).flat_map(move |x| (-1..=3).flat_map(move |y| (-3..=3).map(move |z| c.offset(x, y, z))))
}

/// `EndGatewayFeature.place` on the live world: the gateway at `origin` in its bedrock frame,
/// with `exit` (`knownExit(exit, exact)`) or without one (`END_GATEWAY_DELAYED`).
pub fn place_end_gateway(level: &mut Level, origin: BlockPos, exit: Option<BlockPos>, exact: bool) {
    let bedrock = ids::BEDROCK.default_state();
    for z in origin.z - 1..=origin.z + 1 {
        for y in origin.y - 2..=origin.y + 2 {
            for x in origin.x - 1..=origin.x + 1 {
                let pos = BlockPos::new(x, y, z);
                let same_x = x == origin.x;
                let same_y = y == origin.y;
                let same_z = z == origin.z;
                let end = (y - origin.y).abs() == 2;
                if same_x && same_y && same_z {
                    level.set_block_and_update(pos, ids::END_GATEWAY.default_state());
                    if let Some(exit) = exit
                        && let Some(gateway) = gateway_mut(level, pos)
                    {
                        gateway.exit = Some(exit);
                        gateway.exact = exact;
                    }
                    continue;
                }
                let s = if same_y {
                    BlockState::AIR
                } else if (end && same_x && same_z) || ((same_x || same_z) && !end) {
                    bedrock
                } else {
                    BlockState::AIR
                };
                level.set_block_and_update(pos, s);
            }
        }
    }
}

// --- Exits of gateways without one --------------------------------------------------------

/// `findExitPortalXZPosTentative`: chunks the search walks back, then forward.
const EXIT_SEARCH_STEPS: i32 = 16;
/// `findExitPortalXZPosTentative`: the distance of the first chunk looked at.
const EXIT_SEARCH_DISTANCE: f64 = 1024.0;
/// Chunks around the found chunk that the exit needs: the island (7 blocks), the tallest
/// block search (16), the exit gateway above it and `findExitPosition` (5) stay within 22
/// blocks of it.
const EXIT_AREA_RADIUS: i32 = 2;

/// The chunks `getPortalPosition` of a gateway without an exit reads (Vanilla loads them on
/// the spot), as far as they are not full yet: first those the search along the gateway's
/// direction may look at, then those around the chunk it found. Empty when
/// [`create_gateway_exit`] can run.
pub fn gateway_exit_chunks(level: &Level, entry: BlockPos) -> Vec<ChunkPos> {
    let loaded = |pos: &ChunkPos| level.chunks.chunk(*pos).is_some();
    match exit_xz_tentative(level, entry) {
        Err(missing) => {
            // Every chunk the walk can reach at once, instead of one after the other.
            let (dx, dz) = exit_direction(entry);
            let mut chunks = vec![missing];
            for step in -EXIT_SEARCH_STEPS..=EXIT_SEARCH_STEPS {
                let distance = EXIT_SEARCH_DISTANCE + f64::from(step) * 16.0;
                let pos = chunk_at(dx * distance, dz * distance);
                if !loaded(&pos) && !chunks.contains(&pos) {
                    chunks.push(pos);
                }
            }
            chunks
        }
        Ok((x, z)) => {
            let center = chunk_at(x, z);
            let r = EXIT_AREA_RADIUS;
            (center.x - r..=center.x + r)
                .flat_map(|cx| (center.z - r..=center.z + r).map(move |cz| ChunkPos::new(cx, cz)))
                .filter(|pos| !loaded(pos))
                .collect()
        }
    }
}

/// The part of `getPortalPosition` for a gateway at `entry` in the End without an exit
/// (`findOrCreateValidTeleportPos`): finds end stone to stand on in the first chunk with
/// blocks about 1024 blocks out in the gateway's direction, or places an `end_island` there;
/// builds the exit gateway 10 blocks above the tallest block near it, leading back, and makes
/// it the gateway's exit. Needs the chunks of [`gateway_exit_chunks`]; returns the exit.
pub fn create_gateway_exit(level: &mut Level, entry: BlockPos) -> Option<BlockPos> {
    let (x, z) = exit_xz_tentative(level, entry).ok()?;
    let chunk = std::sync::Arc::clone(level.chunks.chunk(chunk_at(x, z))?);
    let pos = match find_valid_spawn_in_chunk(level, &chunk) {
        Some(pos) => pos,
        None => {
            let pos = BlockPos::containing(x + 0.5, 75.0, z + 0.5);
            place_end_island(level, pos, &mut LegacyRandom::new(pos.as_long()));
            pos
        }
    };
    let exit = find_tallest_block(level, pos, 16, true).offset(0, 10, 0);
    // `spawnGatewayPortal(..., EndGatewayFeature.knownExit(portalEntryPos, false))`.
    place_end_gateway(level, exit, Some(entry), false);
    // `setExitPosition(exitPortalPos, this.exactTeleport)`.
    gateway_mut(level, entry)?.exit = Some(exit);
    Some(exit)
}

/// `teleportXZDirectionVector`: the gateway's horizontal direction from the world's centre
/// (`Vec3.normalize`).
fn exit_direction(entry: BlockPos) -> (f64, f64) {
    let (x, z) = (f64::from(entry.x), f64::from(entry.z));
    let dist = (x * x + 0.0 * 0.0 + z * z).sqrt();
    if dist < f64::from(1.0E-5_f32) {
        (0.0, 0.0)
    } else {
        (x / dist, z / dist)
    }
}

/// `getChunk(level, pos)`'s chunk position.
#[allow(clippy::cast_possible_truncation)]
fn chunk_at(x: f64, z: f64) -> ChunkPos {
    ChunkPos::new((x / 16.0).floor() as i32, (z / 16.0).floor() as i32)
}

/// `findExitPortalXZPosTentative`: from 1024 blocks out, back by a chunk while the chunk holds
/// blocks, then forward while it is empty (16 steps each at most). `Err` names a chunk that is
/// not full yet.
fn exit_xz_tentative(level: &Level, entry: BlockPos) -> Result<(f64, f64), ChunkPos> {
    let (dx, dz) = exit_direction(entry);
    let (mut x, mut z) = (dx * EXIT_SEARCH_DISTANCE, dz * EXIT_SEARCH_DISTANCE);
    let mut limit = EXIT_SEARCH_STEPS;
    while !chunk_is_empty(level, x, z)? && limit > 0 {
        limit -= 1;
        x += dx * -16.0;
        z += dz * -16.0;
    }
    let mut limit = EXIT_SEARCH_STEPS;
    while chunk_is_empty(level, x, z)? && limit > 0 {
        limit -= 1;
        x += dx * 16.0;
        z += dz * 16.0;
    }
    Ok((x, z))
}

/// `isChunkEmpty`: the chunk has only air.
fn chunk_is_empty(level: &Level, x: f64, z: f64) -> Result<bool, ChunkPos> {
    let pos = chunk_at(x, z);
    let chunk = level.chunks.chunk(pos).ok_or(pos)?;
    Ok(highest_filled_section(chunk).is_none())
}

/// `getHighestFilledSectionIndex`, `None` for -1.
fn highest_filled_section(chunk: &ProtoChunk) -> Option<usize> {
    chunk
        .chunk
        .sections()
        .iter()
        .rposition(|section| !section.summary().is_empty())
}

/// `findValidSpawnInChunk`: the end stone with two non-full blocks above closest to the
/// world's centre, from y 30 to the top of the highest section with blocks (the first of
/// equally close ones in `betweenClosed` order).
fn find_valid_spawn_in_chunk(level: &Level, chunk: &ProtoChunk) -> Option<BlockPos> {
    let (min_x, min_z) = (chunk.pos.x * 16, chunk.pos.z * 16);
    // `getHighestSectionPosition() + 16 - 1`.
    let top = match highest_filled_section(chunk) {
        #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
        Some(index) => (chunk.chunk.min_section() + index as i32) * 16,
        None => chunk.chunk.min_y(),
    } + 15;
    let full = |pos: BlockPos| {
        vanillite_worldgen::behaviour::is_full_block(level.block(pos).collision_shape())
    };
    let mut closest: Option<(BlockPos, f64)> = None;
    for z in min_z..=min_z + 15 {
        for y in top.min(30)..=top.max(30) {
            for x in min_x..=min_x + 15 {
                let pos = BlockPos::new(x, y, z);
                if level.block(pos).is(ids::END_STONE)
                    && !full(pos.offset(0, 1, 0))
                    && !full(pos.offset(0, 2, 0))
                {
                    let (cx, cy, cz) = (f64::from(x) + 0.5, f64::from(y) + 0.5, f64::from(z) + 0.5);
                    let dist = cx * cx + cy * cy + cz * cz;
                    if closest.is_none_or(|(_, best)| dist < best) {
                        closest = Some((pos, dist));
                    }
                }
            }
        }
    }
    closest.map(|(pos, _)| pos)
}

/// `EndIslandFeature.place` on the live world: a cone of end stone hanging below `origin`.
#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
fn place_end_island(level: &mut Level, origin: BlockPos, random: &mut LegacyRandom) {
    let end_stone = ids::END_STONE.default_state();
    let mut size = random.next_int_bounded(3) as f32 + 4.0;
    let mut y = 0;
    while size > 0.5 {
        for x in (-size).floor() as i32..=size.ceil() as i32 {
            for z in (-size).floor() as i32..=size.ceil() as i32 {
                if ((x * x + z * z) as f32) <= (size + 1.0) * (size + 1.0) {
                    level.set_block_and_update(origin.offset(x, y, z), end_stone);
                }
            }
        }
        size -= random.next_int_bounded(2) as f32 + 0.5;
        y -= 1;
    }
}

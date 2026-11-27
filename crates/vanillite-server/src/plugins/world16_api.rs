//! The world, menu and item functions of plugin ABI 1.6: biomes (`/fillbiome`), light, chunk
//! states, force-loaded chunks (`/forceload`) and plugin chunk tickets, the world border
//! (`/worldborder`), explosions, ray traces, plugin menus and item components.

use std::collections::HashMap;
use std::sync::OnceLock;

use vanillite_abi::{self as abi, chunk_state, explode_flag, ray_hit, raytrace_flag, status};
use vanillite_base::hash::FxHashMap;
use vanillite_base::{Aabb, BlockPos, ChunkPos, Vec3};
use vanillite_config::GameMode;
use vanillite_game::item::ItemStack;
use vanillite_game::level::Level;
use vanillite_game::level::border::{MAX_CENTER_COORDINATE, MAX_SIZE};
use vanillite_game::level::explosion::{BlockInteraction, ExplosionStyle};
use vanillite_game::level::forced::FORCED_TICKET_LEVEL;
use vanillite_game::menu::container;
use vanillite_nbt::{Compound, Tag};
use vanillite_plugin_host::{BlockBox, Menu};
use vanillite_protocol::components_nbt as conv;
use vanillite_protocol::packets::play::clientbound::{
    ContainerSetSlot, SetBorderCenter, SetBorderLerpSize, SetBorderSize, SetBorderWarningDelay,
    SetBorderWarningDistance,
};
use vanillite_world::chunks::{
    BLOCK_TICKING_LEVEL, Chunks, ENTITY_TICKING_LEVEL, TicketId, Visibility,
};
use vanillite_world::entity::EntityId;

use super::PluginWorld;

type Result<T> = std::result::Result<T, i32>;

/// A namespaced id as Vanilla's `ResourceLocation` reads it: `plains` is `minecraft:plains`.
fn qualified(id: &str) -> String {
    if id.contains(':') {
        id.to_owned()
    } else {
        format!("minecraft:{id}")
    }
}

// --- Biomes and light ---------------------------------------------------------------------

/// The biome of the quart at a position (`getNoiseBiome(x >> 2, y >> 2, z >> 2)` as Bukkit's
/// `World.getBiome` reads it, the quart `/fillbiome` sets); [`status::NOT_FOUND`] outside
/// loaded chunks.
pub(super) fn biome(w: &PluginWorld<'_>, [x, y, z]: [i32; 3]) -> Result<String> {
    let level = w.0.level();
    let pos = BlockPos::new(x, y, z);
    if !level.is_loaded(pos) {
        return Err(status::NOT_FOUND);
    }
    let climate = level.climate().ok_or(status::UNSUPPORTED)?;
    let chunk = level.chunks.chunk(pos.chunk()).ok_or(status::NOT_FOUND)?;
    // `LevelChunk.getNoiseBiome`: the quart Y is clamped to the chunk.
    let sections = chunk.chunk.sections();
    let min_quart = chunk.chunk.min_section().saturating_mul(4);
    let quarts = i32::try_from(sections.len()).unwrap_or(0).saturating_mul(4);
    let qy = (y >> 2).clamp(min_quart, min_quart + quarts - 1);
    let section = usize::try_from((qy - min_quart) >> 2)
        .ok()
        .and_then(|i| sections.get(i))
        .ok_or(status::NOT_FOUND)?;
    let local = |q: i32| u32::try_from(q & 3).unwrap_or(0);
    let id = section.biome(local(x >> 2), local(qy), local(z >> 2));
    climate
        .biomes
        .all()
        .get(usize::from(id))
        .map(|b| b.name.clone())
        .ok_or(status::NOT_FOUND)
}

/// `/fillbiome` without a filter over the box (quantized to quarts like the command): every
/// chunk of it must be loaded; clients get the changed chunks' biomes again.
pub(super) fn set_biome(w: &mut PluginWorld<'_>, area: BlockBox, biome: &str) -> Result<()> {
    let level = w.0.level_mut();
    let climate = level.climate().ok_or(status::UNSUPPORTED)?;
    let biome = climate
        .biomes
        .id_of(&qualified(biome))
        .ok_or(status::INVALID_ARGUMENT)?;
    // `FillBiomeCommand.quantize`.
    let quantize = |[x, y, z]: [i32; 3]| BlockPos::new(x & !3, y & !3, z & !3);
    let (min, max) = (quantize(area.min), quantize(area.max));
    crate::commands::fill_biome_box(level, min, max, biome, &|_| true)
        .map(|_| ())
        .map_err(|_| status::NOT_FOUND)
}

/// `getBrightness(SKY)` and `getBrightness(BLOCK)`; [`status::NOT_FOUND`] outside loaded
/// chunks.
pub(super) fn light(w: &PluginWorld<'_>, [x, y, z]: [i32; 3]) -> Result<(u32, u32)> {
    let level = w.0.level();
    let pos = BlockPos::new(x, y, z);
    if !level.is_loaded(pos) {
        return Err(status::NOT_FOUND);
    }
    let (sky, block) = level.light(pos);
    let clamp = |v: i32| u32::try_from(v.clamp(0, 15)).unwrap_or(0);
    Ok((clamp(sky), clamp(block)))
}

// --- Chunks -------------------------------------------------------------------------------

/// `ChunkHolder.getFullStatus` of a chunk: unloaded unless it is accessible, then by its
/// ticket level.
pub(super) fn chunk_state(w: &PluginWorld<'_>, x: i32, z: i32) -> Result<u32> {
    let chunks = &w.0.level().chunks;
    let pos = ChunkPos::new(x, z);
    if chunks.visibility(pos) == Visibility::Hidden {
        return Ok(chunk_state::UNLOADED);
    }
    let level = chunks.level(pos);
    Ok(if level <= ENTITY_TICKING_LEVEL {
        chunk_state::ENTITY_TICKING
    } else if level == BLOCK_TICKING_LEVEL {
        chunk_state::BLOCK_TICKING
    } else {
        chunk_state::FULL
    })
}

/// `ServerLevel.setChunkForced` as `/forceload add` and `remove` of one chunk (saved with the
/// world's chunk tickets).
pub(super) fn force_load(w: &mut PluginWorld<'_>, x: i32, z: i32, on: bool) -> Result<()> {
    let Level { chunks, forced, .. } = w.0.level_mut();
    forced.set(chunks, ChunkPos::new(x, z), on);
    Ok(())
}

/// The chunk tickets plugins hold in a world: while at least one plugin holds one on a chunk,
/// a load and a simulation ticket keep it loaded and entity ticking like a forced chunk. Each
/// plugin holds at most one per chunk; they are not saved.
#[derive(Debug, Default)]
pub(crate) struct PluginTickets {
    chunks: FxHashMap<ChunkPos, (Vec<Box<str>>, TicketId, TicketId)>,
}

impl PluginTickets {
    /// Adds or removes `plugin`'s ticket on a chunk.
    fn set(&mut self, chunks: &mut Chunks, plugin: &str, pos: ChunkPos, add: bool) {
        use std::collections::hash_map::Entry;
        match self.chunks.entry(pos) {
            Entry::Occupied(mut entry) => {
                let holders = &mut entry.get_mut().0;
                let held = holders.iter().position(|p| &**p == plugin);
                match (add, held) {
                    (true, None) => holders.push(plugin.into()),
                    (false, Some(i)) => {
                        holders.swap_remove(i);
                        if holders.is_empty() {
                            let (_, load, simulation) = entry.remove();
                            chunks.remove_ticket(load);
                            chunks.remove_simulation_ticket(simulation);
                        }
                    }
                    _ => {}
                }
            }
            Entry::Vacant(slot) if add => {
                let load = chunks.add_ticket(pos, FORCED_TICKET_LEVEL);
                let simulation = chunks.add_simulation_ticket(pos, FORCED_TICKET_LEVEL);
                slot.insert((vec![plugin.into()], load, simulation));
            }
            Entry::Vacant(_) => {}
        }
    }

    /// Drops every ticket `plugin` holds (it was disabled).
    pub(crate) fn drop_plugin(&mut self, chunks: &mut Chunks, plugin: &str) {
        let held: Vec<ChunkPos> = self
            .chunks
            .iter()
            .filter(|(_, (holders, ..))| holders.iter().any(|p| &**p == plugin))
            .map(|(pos, _)| *pos)
            .collect();
        for pos in held {
            self.set(chunks, plugin, pos, false);
        }
    }
}

/// Adds or removes a plugin's ticket on a chunk.
pub(super) fn chunk_ticket(
    w: &mut PluginWorld<'_>,
    plugin: &str,
    x: i32,
    z: i32,
    add: bool,
) -> Result<()> {
    let mut tickets = std::mem::take(&mut w.0.plugin_tickets);
    tickets.set(w.0.chunks_mut(), plugin, ChunkPos::new(x, z), add);
    w.0.plugin_tickets = tickets;
    Ok(())
}

/// Drops a disabled plugin's chunk tickets in every world, each on its own thread.
pub(super) fn plugin_disabled(global: &crate::global::Global, plugin: &str) {
    for id in global.dimensions.worlds() {
        let plugin = plugin.to_owned();
        global.run_in_world(id, move |w| {
            let mut tickets = std::mem::take(&mut w.plugin_tickets);
            tickets.drop_plugin(w.chunks_mut(), &plugin);
            w.plugin_tickets = tickets;
        });
    }
}

// --- World border -------------------------------------------------------------------------

/// The world border as `/worldborder get` and the border packets show it (the warning time
/// in whole seconds).
pub(super) fn border(w: &PluginWorld<'_>) -> Result<abi::BorderInfo> {
    let border = &w.0.level().border;
    Ok(abi::BorderInfo {
        size: abi::size_of::<abi::BorderInfo>(),
        center_x: border.center_x(),
        center_z: border.center_z(),
        diameter: border.size(),
        target: border.lerp_target(),
        lerp_ticks: border.lerp_time(),
        damage_per_block: border.damage_per_block(),
        safe_zone: border.safe_zone(),
        warning_blocks: border.warning_blocks(),
        warning_time: border.warning_time() / 20,
    })
}

/// `/worldborder center`, `set`, `damage`, `warning` at once: what changed is applied and the
/// world's players are told as the border's listeners tell them. Values outside the command's
/// ranges are [`status::INVALID_ARGUMENT`].
pub(super) fn set_border(w: &mut PluginWorld<'_>, b: &abi::BorderInfo) -> Result<()> {
    let size_ok = |v: f64| (1.0..=MAX_SIZE).contains(&v);
    let center_ok = |v: f64| v.abs() <= MAX_CENTER_COORDINATE;
    let non_negative = |v: f64| v.is_finite() && v >= 0.0;
    if !center_ok(b.center_x)
        || !center_ok(b.center_z)
        || !size_ok(b.diameter)
        || !size_ok(b.target)
        || !non_negative(b.damage_per_block)
        || !non_negative(b.safe_zone)
        || b.warning_blocks < 0
        || b.warning_time < 0
        || b.lerp_ticks < 0
    {
        return Err(status::INVALID_ARGUMENT);
    }
    let level = w.0.level_mut();
    let game_time = level.game_time();
    let players = &level.entities.players;
    let border = &mut level.border;
    if border.center_x() != b.center_x || border.center_z() != b.center_z {
        border.set_center(b.center_x, b.center_z);
        broadcast(
            players,
            &SetBorderCenter {
                center_x: b.center_x,
                center_z: b.center_z,
            },
        );
    }
    let moving = b.lerp_ticks > 0 && b.target != b.diameter;
    let same_size = if moving {
        border.size() == b.diameter
            && border.lerp_target() == b.target
            && border.lerp_time() == b.lerp_ticks
    } else {
        border.lerp_time() == 0 && border.size() == b.target
    };
    if !same_size {
        if moving {
            border.lerp_size_between(b.diameter, b.target, b.lerp_ticks, game_time);
            broadcast(
                players,
                &SetBorderLerpSize {
                    old_size: border.size(),
                    new_size: border.lerp_target(),
                    lerp_time: border.lerp_time(),
                },
            );
        } else {
            border.set_size(b.target);
            broadcast(
                players,
                &SetBorderSize {
                    size: border.lerp_target(),
                },
            );
        }
    }
    if border.damage_per_block() != b.damage_per_block {
        border.set_damage_per_block(b.damage_per_block);
    }
    if border.safe_zone() != b.safe_zone {
        border.set_safe_zone(b.safe_zone);
    }
    if border.warning_blocks() != b.warning_blocks {
        border.set_warning_blocks(b.warning_blocks);
        broadcast(
            players,
            &SetBorderWarningDistance {
                warning_blocks: b.warning_blocks,
            },
        );
    }
    // The border keeps the warning time in ticks (`TimeArgument`); a value read back
    // unchanged (whole seconds) leaves it alone.
    let warning_ticks = b.warning_time.saturating_mul(20);
    if border.warning_time() / 20 != b.warning_time {
        border.set_warning_time(warning_ticks);
        broadcast(
            players,
            &SetBorderWarningDelay {
                warning_delay: warning_ticks,
            },
        );
    }
    Ok(())
}

/// `broadcastAll(packet, dimension)`: a border packet to the players of one world.
fn broadcast(
    players: &vanillite_world::entity::Dense<vanillite_game::player::PlayerData>,
    packet: &impl vanillite_protocol::Packet,
) {
    crate::border::broadcast_world(players.iter(), packet);
}

// --- Explosions ---------------------------------------------------------------------------

/// `ServerExplosion.getIndirectSourceEntity`: a primed TNT's owner, a living source itself.
fn indirect_source(
    entities: &vanillite_game::entities::Entities,
    id: EntityId,
) -> Option<EntityId> {
    if let Some(tnt) = entities.primed_tnt(id) {
        return tnt.owner;
    }
    (entities.player(id).is_some() || entities.mob(id).is_some()).then_some(id)
}

/// `ServerLevel.explode` as `PrimedTnt.explode` makes it (`ExplosionInteraction.TNT`: drops
/// decay with `tnt_explosion_drop_decay`), without breaking blocks unless asked, with fire
/// when asked; the damage applies at once.
pub(super) fn explode(
    w: &mut PluginWorld<'_>,
    [x, y, z]: [f64; 3],
    power: f32,
    flags: u32,
    source: Option<u128>,
) -> Result<()> {
    let global = w.1;
    let (source, indirect) = match source {
        Some(uuid) => {
            let entities = w.0.entities();
            let id = super::any_entity(entities, uuid).ok_or(status::NOT_FOUND)?;
            (Some(id), indirect_source(entities, id))
        }
        None => (None, None),
    };
    let dimension = w.0.dimension().to_owned();
    let (level, survival, mobs) = w.0.level_systems();
    let interaction = if flags & explode_flag::BREAK_BLOCKS == 0 {
        BlockInteraction::Keep
    } else if level.settings.tnt_explosion_drop_decay {
        BlockInteraction::DestroyWithDecay
    } else {
        BlockInteraction::Destroy
    };
    let style = ExplosionStyle {
        fire: flags & explode_flag::FIRE != 0,
        ..ExplosionStyle::default()
    };
    level.explode_with(
        source,
        indirect,
        Vec3::new(x, y, z),
        power,
        interaction,
        style,
    );
    mobs.after_explosion(level, survival, global, &dimension);
    Ok(())
}

// --- Ray traces ---------------------------------------------------------------------------

/// `Direction.getApproximateNearest`: the first of down, up, north, south, west and east
/// (their 3D data values) whose normal is closest to `d`.
fn nearest_face(d: Vec3) -> u32 {
    let dots = [-d.y, d.y, -d.z, d.z, -d.x, d.x];
    let mut best = 2;
    // `Float.MIN_VALUE`.
    let mut max = f64::from(f32::from_bits(1));
    for (face, dot) in (0u32..).zip(dots) {
        #[allow(clippy::cast_possible_truncation, clippy::as_conversions)]
        let dot = f64::from(dot as f32);
        if dot > max {
            max = dot;
            best = face;
        }
    }
    best
}

/// `VoxelShape.clip` of block-local boxes at `pos`: the fraction of the segment where it
/// enters them and the face it enters through (a start inside hits at once, the face facing
/// the ray).
fn shape_clip(boxes: &[[f64; 6]], pos: BlockPos, from: Vec3, to: Vec3) -> Option<(f64, u32)> {
    if boxes.is_empty() {
        return None;
    }
    let d = [to.x - from.x, to.y - from.y, to.z - from.z];
    if d[0] * d[0] + d[1] * d[1] + d[2] * d[2] < 1.0e-7 {
        return None;
    }
    let o = [f64::from(pos.x), f64::from(pos.y), f64::from(pos.z)];
    let f = [from.x, from.y, from.z];
    let test = [
        f[0] + d[0] * 0.001 - o[0],
        f[1] + d[1] * 0.001 - o[1],
        f[2] + d[2] * 0.001 - o[2],
    ];
    if boxes
        .iter()
        .any(|b| (0..3).all(|a| b[a] <= test[a] && test[a] < b[a + 3]))
    {
        let face = nearest_face(Vec3::new(d[0], d[1], d[2])) ^ 1;
        return Some((0.001, face));
    }
    // `AABB.clip` → `getDirection`: x (west, east), then y (down, up), then z (north, south).
    const FACES: [[u32; 2]; 3] = [[4, 5], [0, 1], [2, 3]];
    let mut scale = 1.0;
    let mut hit = None;
    for b in boxes {
        for a in 0..3 {
            let (plane, face) = if d[a] > 1.0e-7 {
                (b[a] + o[a], FACES[a][0])
            } else if d[a] < -1.0e-7 {
                (b[a + 3] + o[a], FACES[a][1])
            } else {
                continue;
            };
            let s = (plane - f[a]) / d[a];
            let (ib, ic) = ((a + 1) % 3, (a + 2) % 3);
            let pb = f[ib] + s * d[ib];
            let pc = f[ic] + s * d[ic];
            if 0.0 < s
                && s < scale
                && b[ib] + o[ib] - 1.0e-7 < pb
                && pb < b[ib + 3] + o[ib] + 1.0e-7
                && b[ic] + o[ic] - 1.0e-7 < pc
                && pc < b[ic + 3] + o[ic] + 1.0e-7
            {
                scale = s;
                hit = Some(face);
            }
        }
    }
    hit.map(|face| (scale, face))
}

/// The hit in one block: its collision shape and its fluid's shape, the nearer of the two
/// (the block's on a tie), as `Level.clip` with `ClipContext.Block.COLLIDER`.
fn block_hit(
    level: &Level,
    pos: BlockPos,
    from: Vec3,
    to: Vec3,
    blocks: bool,
    fluids: bool,
) -> Option<(f64, u32)> {
    let state = level.block(pos);
    let block = if blocks {
        shape_clip(state.collision_shape(), pos, from, to)
    } else {
        None
    };
    let fluid = state.fluid_state();
    let liquid = if fluids && !fluid.is_empty() {
        let above = level
            .block(BlockPos::new(pos.x, pos.y + 1, pos.z))
            .fluid_state();
        let height = if !above.is_empty() && above.info().fluid == fluid.info().fluid {
            1.0
        } else {
            f64::from(fluid.info().own_height)
        };
        shape_clip(&[[0.0, 0.0, 0.0, 1.0, height, 1.0]], pos, from, to)
    } else {
        None
    };
    match (block, liquid) {
        (Some(b), Some(l)) => Some(if b.0 <= l.0 { b } else { l }),
        (b, l) => b.or(l),
    }
}

/// `BlockGetter.traverseBlocks` from `from` to `to`: the first block hit with its fraction of
/// the segment and face.
fn trace_blocks(
    level: &Level,
    from: Vec3,
    to: Vec3,
    blocks: bool,
    fluids: bool,
) -> Option<(BlockPos, f64, u32)> {
    use vanillite_base::math::{floor, frac};
    if from == to {
        return None;
    }
    let lerp = |t: f64, a: f64, b: f64| a + t * (b - a);
    let (to_x, to_y, to_z) = (
        lerp(-1.0e-7, to.x, from.x),
        lerp(-1.0e-7, to.y, from.y),
        lerp(-1.0e-7, to.z, from.z),
    );
    let (from_x, from_y, from_z) = (
        lerp(-1.0e-7, from.x, to.x),
        lerp(-1.0e-7, from.y, to.y),
        lerp(-1.0e-7, from.z, to.z),
    );
    let (mut bx, mut by, mut bz) = (floor(from_x), floor(from_y), floor(from_z));
    let hit =
        |pos: BlockPos| block_hit(level, pos, from, to, blocks, fluids).map(|(s, f)| (pos, s, f));
    if let Some(h) = hit(BlockPos::new(bx, by, bz)) {
        return Some(h);
    }
    let (dx, dy, dz) = (to_x - from_x, to_y - from_y, to_z - from_z);
    let sign = |d: f64| -> i32 {
        if d > 0.0 {
            1
        } else if d < 0.0 {
            -1
        } else {
            0
        }
    };
    let (sx, sy, sz) = (sign(dx), sign(dy), sign(dz));
    let delta = |s: i32, d: f64| {
        if s == 0 { f64::MAX } else { f64::from(s) / d }
    };
    let (tdx, tdy, tdz) = (delta(sx, dx), delta(sy, dy), delta(sz, dz));
    let start = |s: i32, td: f64, f: f64| td * if s > 0 { 1.0 - frac(f) } else { frac(f) };
    let (mut tx, mut ty, mut tz) = (
        start(sx, tdx, from_x),
        start(sy, tdy, from_y),
        start(sz, tdz, from_z),
    );
    while tx <= 1.0 || ty <= 1.0 || tz <= 1.0 {
        if tx < ty {
            if tx < tz {
                bx = bx.wrapping_add(sx);
                tx += tdx;
            } else {
                bz = bz.wrapping_add(sz);
                tz += tdz;
            }
        } else if ty < tz {
            by = by.wrapping_add(sy);
            ty += tdy;
        } else {
            bz = bz.wrapping_add(sz);
            tz += tdz;
        }
        if let Some(h) = hit(BlockPos::new(bx, by, bz)) {
            return Some(h);
        }
    }
    None
}

/// The nearest entity whose box the segment enters (`ProjectileUtil.getEntityHitResult`: a
/// box the segment starts in is hit at its start), not a spectator nor `ignore`.
fn trace_entities(
    level: &Level,
    from: Vec3,
    to: Vec3,
    ignore: Option<u128>,
) -> Option<(u128, f64, Vec3)> {
    let entities = &level.entities;
    let area = Aabb::new(
        from.x.min(to.x),
        from.y.min(to.y),
        from.z.min(to.z),
        from.x.max(to.x),
        from.y.max(to.y),
        from.z.max(to.z),
    );
    let mut best: Option<(u128, f64, Vec3)> = None;
    entities.store.for_each_in(&area, false, |id| {
        let Some(e) = entities.store.get(id) else {
            return;
        };
        if Some(e.uuid()) == ignore
            || entities
                .player(id)
                .is_some_and(|p| p.game_mode == GameMode::Spectator)
        {
            return;
        }
        let b = e.aabb();
        let hit = if b.contains(from.x, from.y, from.z) {
            Some((0.0, from))
        } else {
            vanillite_game::mob::projectile::aabb_clip(&b, from, to)
                .map(|p| (p.distance_to_sqr(from), p))
        };
        if let Some((distance, pos)) = hit
            && best.as_ref().is_none_or(|(_, d, _)| distance < *d)
        {
            best = Some((e.uuid(), distance, pos));
        }
    });
    best
}

/// The nearest hit of a ray: blocks first, then the entities before the block hit.
pub(super) fn raytrace(
    w: &PluginWorld<'_>,
    [fx, fy, fz]: [f64; 3],
    [tx, ty, tz]: [f64; 3],
    flags: u32,
    ignore: Option<u128>,
) -> Result<abi::RayHit> {
    let level = w.0.level();
    let (from, to) = (Vec3::new(fx, fy, fz), Vec3::new(tx, ty, tz));
    let blocks = flags & raytrace_flag::BLOCKS != 0;
    let fluids = flags & raytrace_flag::FLUIDS != 0;
    let vec = |v: Vec3| abi::Vec3 {
        x: v.x,
        y: v.y,
        z: v.z,
    };
    let mut hit = abi::RayHit {
        size: abi::size_of::<abi::RayHit>(),
        kind: ray_hit::MISS,
        pos: vec(to),
        block: abi::BlockPos { x: 0, y: 0, z: 0 },
        face: 0,
        entity: abi::Uuid::NIL,
    };
    let mut end = to;
    if (blocks || fluids)
        && let Some((pos, scale, face)) = trace_blocks(level, from, to, blocks, fluids)
    {
        end = Vec3::new(
            from.x + (to.x - from.x) * scale,
            from.y + (to.y - from.y) * scale,
            from.z + (to.z - from.z) * scale,
        );
        hit.kind = ray_hit::BLOCK;
        hit.pos = vec(end);
        hit.block = abi::BlockPos {
            x: pos.x,
            y: pos.y,
            z: pos.z,
        };
        hit.face = face;
    }
    if flags & raytrace_flag::ENTITIES != 0
        && let Some((entity, _, pos)) = trace_entities(level, from, end, ignore)
    {
        hit.kind = ray_hit::ENTITY;
        hit.pos = vec(pos);
        hit.block = abi::BlockPos { x: 0, y: 0, z: 0 };
        hit.face = 0;
        hit.entity = abi::Uuid::from_u128(entity);
    }
    Ok(hit)
}

// --- Menus --------------------------------------------------------------------------------

/// A player of this world by UUID.
fn player_id(w: &PluginWorld<'_>, player: u128) -> Result<EntityId> {
    w.0.entities()
        .player_by_uuid(player)
        .ok_or(status::NOT_FOUND)
}

/// An item stack from its compound (`{}`: none); [`status::INVALID_ARGUMENT`] for an unknown
/// item.
fn stack_of(item: &Compound) -> Result<ItemStack> {
    if item.is_empty() {
        return Ok(ItemStack::EMPTY);
    }
    let stack = vanillite_game::inventory::stack_from_nbt(item);
    let air = item.get("id").and_then(Tag::as_str) == Some("minecraft:air");
    if stack.is_empty() && !air {
        return Err(status::INVALID_ARGUMENT);
    }
    Ok(stack)
}

/// The stacks of a plugin menu from an SNBT list of stacks with `Slot`.
fn menu_items(snbt: &str, slots: usize) -> Result<Vec<ItemStack>> {
    let mut items = vec![ItemStack::EMPTY; slots];
    if snbt.trim().is_empty() {
        return Ok(items);
    }
    let Ok(Tag::List(list)) = vanillite_nbt::snbt::parse(snbt) else {
        return Err(status::INVALID_ARGUMENT);
    };
    for tag in &list.0 {
        let Tag::Compound(c) = tag else {
            return Err(status::INVALID_ARGUMENT);
        };
        let slot = c
            .get("Slot")
            .and_then(Tag::as_i64)
            .and_then(|s| usize::try_from(s).ok())
            .filter(|&s| s < slots)
            .ok_or(status::INVALID_ARGUMENT)?;
        let mut item = c.clone();
        item.remove("Slot");
        if let Some(target) = items.get_mut(slot) {
            *target = stack_of(&item)?;
        }
    }
    Ok(items)
}

/// Opens a plugin menu like `ServerPlayer.openMenu`; returns the container id.
pub(super) fn open_menu(w: &mut PluginWorld<'_>, player: u128, menu: &Menu<'_>) -> Result<u32> {
    let kind =
        container::plugin_kind(&qualified(menu.menu_type)).ok_or(status::INVALID_ARGUMENT)?;
    let items = menu_items(menu.items, kind.container_slots())?;
    let title = super::component(menu.title);
    let id = player_id(w, player)?;
    let container =
        container::open_plugin(w.0.level_mut(), id, kind, title, items).ok_or(status::NOT_FOUND)?;
    u32::try_from(container).map_err(|_| status::ERROR)
}

/// The stack in a slot of a player's open menu (`{}` for none).
pub(super) fn menu_item(w: &PluginWorld<'_>, player: u128, slot: u32) -> Result<Compound> {
    let id = player_id(w, player)?;
    let slot = usize::try_from(slot).map_err(|_| status::INVALID_ARGUMENT)?;
    let stack = container::open_menu_item(w.0.level(), id, slot).ok_or(status::INVALID_ARGUMENT)?;
    Ok(if stack.is_empty() {
        Compound::new()
    } else {
        vanillite_game::inventory::stack_to_nbt(&stack)
    })
}

/// Puts a stack into a slot of a player's open menu (`Slot.set`) and sends the slot.
pub(super) fn set_menu_item(
    w: &mut PluginWorld<'_>,
    player: u128,
    slot: u32,
    item: &Compound,
) -> Result<()> {
    let id = player_id(w, player)?;
    let slot = usize::try_from(slot).map_err(|_| status::INVALID_ARGUMENT)?;
    let stack = stack_of(item)?;
    let set = container::with_open_menu(w.0.level_mut(), id, |slots, sink| {
        if slot >= slots.slot_count() {
            return false;
        }
        slots.set_item(slot, stack);
        slots.set_changed(slot);
        let current = slots.item(slot).clone();
        let state = slots.state();
        state.set_remote(slot, &current);
        let state_id = state.next_state_id();
        sink.send(&ContainerSetSlot {
            container_id: state.container_id,
            state_id,
            slot: i16::try_from(slot).unwrap_or(0),
            item: current,
        });
        true
    })
    .ok_or(status::NOT_FOUND)?;
    if set {
        Ok(())
    } else {
        Err(status::INVALID_ARGUMENT)
    }
}

/// `ServerPlayer.closeContainer` of a player's open container menu.
pub(super) fn close_menu(w: &mut PluginWorld<'_>, player: u128) -> Result<()> {
    let id = player_id(w, player)?;
    if container::close_for(w.0.level_mut(), id) {
        Ok(())
    } else {
        Err(status::NOT_FOUND)
    }
}

// --- Item components ----------------------------------------------------------------------

/// The default components of every item as Vanilla's data generator writes them
/// (`items.json`, one item per line), read on first use.
const ITEMS_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../data/vanilla/items.json"
));

/// The `items.json` line of each item by name.
fn item_lines() -> &'static HashMap<&'static str, &'static str> {
    static LINES: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    LINES.get_or_init(|| {
        ITEMS_JSON
            .lines()
            .filter_map(|line| {
                let rest = line.strip_prefix("{\"name\":\"")?;
                let name = &rest[..rest.find('"')?];
                Some((name, line.trim_end_matches(',')))
            })
            .collect()
    })
}

/// A JSON value as NBT (numbers as int, long or double; the component codecs read any).
fn json_to_nbt(v: &serde_json::Value) -> Tag {
    use serde_json::Value;
    match v {
        Value::Null => Tag::Compound(Compound::new()),
        Value::Bool(b) => Tag::Byte(i8::from(*b)),
        Value::Number(n) => match n.as_i64() {
            Some(i) => i32::try_from(i).map_or(Tag::Long(i), Tag::Int),
            None => Tag::Double(n.as_f64().unwrap_or(0.0)),
        },
        Value::String(s) => Tag::String(s.clone()),
        Value::Array(a) => Tag::List(vanillite_nbt::List(a.iter().map(json_to_nbt).collect())),
        Value::Object(o) => {
            let mut c = Compound::new();
            for (k, v) in o {
                c.insert(k.clone(), json_to_nbt(v));
            }
            Tag::Compound(c)
        }
    }
}

/// The wire value of an item type's default component, if it has one.
fn default_component(item: u16, type_id: u16) -> Option<Box<[u8]>> {
    let name = vanillite_data::registry::ITEM.name(item)?;
    let component = conv::type_name(type_id)?;
    let line = item_lines().get(name)?;
    let json: serde_json::Value = serde_json::from_str(line).ok()?;
    let value = json.get("components")?.get(component)?;
    vanillite_game::item_nbt::component_from_nbt(type_id, &json_to_nbt(value))
}

/// An item and a persistent component type from the plugin's arguments.
fn item_and_type(item: &Compound, component: &str) -> Result<(ItemStack, u16)> {
    let stack = vanillite_game::inventory::stack_from_nbt(item);
    if stack.is_empty() {
        return Err(status::INVALID_ARGUMENT);
    }
    let type_id = conv::type_by_name(component)
        .filter(|&t| !conv::is_transient(t))
        .ok_or(status::INVALID_ARGUMENT)?;
    Ok((stack, type_id))
}

/// `ItemStack.get(type)` (its patch over its type's defaults) in the persistent form.
pub(super) fn item_component(item: &Compound, component: &str) -> Result<Option<Tag>> {
    let (stack, type_id) = item_and_type(item, component)?;
    let patch = stack.patch();
    if patch.is_some_and(|p| p.is_removed(type_id)) {
        return Ok(None);
    }
    let wire = match stack.component(type_id) {
        Some(wire) => Some(Box::from(wire)),
        None => default_component(stack.item, type_id),
    };
    Ok(wire.and_then(|w| vanillite_game::item_nbt::component_to_nbt(type_id, &w)))
}

/// `ItemStack.set(type, value)` (the value read by the component's codec; a value equal to
/// the default leaves no patch entry) or `remove(type)` (a default is marked removed).
pub(super) fn item_set_component(
    item: &Compound,
    component: &str,
    value: Option<&Tag>,
) -> Result<Compound> {
    let (mut stack, type_id) = item_and_type(item, component)?;
    let default = default_component(stack.item, type_id);
    let patch = stack.patch_mut();
    match value {
        Some(tag) => {
            let wire = vanillite_game::item_nbt::component_from_nbt(type_id, tag)
                .ok_or(status::INVALID_ARGUMENT)?;
            if default.as_deref() == Some(&*wire) {
                patch.unset(type_id);
                if let Ok(i) = patch.removed.binary_search(&type_id) {
                    patch.removed.remove(i);
                }
            } else {
                patch.set(type_id, wire);
            }
        }
        None => {
            patch.unset(type_id);
            if default.is_some()
                && let Err(i) = patch.removed.binary_search(&type_id)
            {
                patch.removed.insert(i, type_id);
            }
        }
    }
    if stack.patch().is_some_and(|p| p.is_empty()) {
        stack.patch = None;
    }
    Ok(vanillite_game::inventory::stack_to_nbt(&stack))
}

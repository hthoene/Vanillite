//! The block commands: `setblock` (`SetBlockCommand`), `fill` (`FillCommand`), `clone`
//! (`CloneCommands`) and `fillbiome` (`FillBiomeCommand`).
//!
//! They work on the world the command runs on: an execution moves to its source's world first
//! (`execute in`). `clone` between two worlds continues on the other world's thread.

use std::collections::VecDeque;

use vanillite_base::{BlockPos, ChunkPos};
use vanillite_command::error::CommandError;
use vanillite_command::{Arg as ErrArg, Args, BlockPredicateArg, BlockStateArg, GameValue};
use vanillite_data::block::{BlockState, ids};
use vanillite_game::commands::{CloneFilter, CloneMode, FillMode, SetBlockMode};
use vanillite_game::level::{Level, update};
use vanillite_game::survival::rules::Rule;
use vanillite_protocol::packets::play::clientbound::{ChunkBiomeData, ChunksBiomes};
use vanillite_protocol::text::{Arg, Component};
use vanillite_worldgen::behaviour as shared;

use super::execute::{block_matches, dimension, loaded_block_pos};
use super::source::CommandSource;
use super::{Env, data};

pub(super) fn missing(name: &str) -> CommandError {
    CommandError::new(
        "command.exception",
        vec![ErrArg::Str(format!("missing argument {name}"))],
    )
}

/// `BlockPosArgument.ERROR_NOT_LOADED`.
pub(super) fn not_loaded() -> CommandError {
    CommandError::new("argument.pos.unloaded", vec![])
}

/// The command must run on its source's world (see the module comment).
pub(super) fn this_world(source: &CommandSource, env: &Env<'_>) -> Result<(), CommandError> {
    if source.world == env.world {
        Ok(())
    } else {
        Err(not_loaded())
    }
}

/// The game rule `max_block_modifications`.
pub(super) fn max_block_modifications(env: &Env<'_>) -> i32 {
    Rule::by_name("max_block_modifications")
        .map_or(32768, |rule| env.global.level.rules().get(rule))
}

fn block_input<'a>(args: &'a Args, name: &str) -> Result<&'a BlockStateArg, CommandError> {
    match args.game(name) {
        Some(GameValue::BlockState(block)) => Ok(block),
        _ => Err(missing(name)),
    }
}

fn block_predicate<'a>(args: &'a Args, name: &str) -> Option<&'a BlockPredicateArg> {
    match args.game(name) {
        Some(GameValue::BlockPredicate(pred)) => Some(pred),
        _ => None,
    }
}

/// A `BoundingBox.fromCorners`: min and max corner.
fn bounds(a: BlockPos, b: BlockPos) -> (BlockPos, BlockPos) {
    (
        BlockPos::new(a.x.min(b.x), a.y.min(b.y), a.z.min(b.z)),
        BlockPos::new(a.x.max(b.x), a.y.max(b.y), a.z.max(b.z)),
    )
}

/// `getXSpan() * getYSpan() * getZSpan()` as a long.
fn volume(min: BlockPos, max: BlockPos) -> i64 {
    let span = |a: i32, b: i32| i64::from(b).saturating_sub(i64::from(a)).saturating_add(1);
    span(min.x, max.x)
        .saturating_mul(span(min.y, max.y))
        .saturating_mul(span(min.z, max.z))
}

/// `BlockInput.place(level, pos, flags)`: the state with its shape from the neighbours (unless
/// `KNOWN_SHAPE`), the typed properties kept, then the input's block entity data loaded into
/// the block entity there; affected when the block or the block entity data changed.
pub(super) fn place(level: &mut Level, pos: BlockPos, input: &BlockStateArg, flags: u32) -> bool {
    let state = BlockState(input.state);
    let mut placed = if flags & update::KNOWN_SHAPE != 0 {
        state
    } else {
        shared::update_from_neighbour_shapes(level, state, pos)
    };
    if placed.is_air() {
        placed = state;
    }
    // `overwriteWithDefinedProperties`.
    if placed != state {
        for (property, _) in &input.properties {
            if let Some(value) = state.value(property) {
                placed = placed.with(property, value).unwrap_or(placed);
            }
        }
    }
    let mut affected = level.set_block(pos, placed, flags);
    if let Some(tag) = &input.nbt
        && let Some(before) = level.block_entity_nbt(pos)
    {
        level.load_block_entity_nbt(pos, tag);
        if level.block_entity_nbt(pos).as_ref() != Some(&before) {
            affected = true;
        }
    }
    affected
}

/// The flags of a command placement: `2 | (strict ? 816 : 256)`.
fn place_flags(strict: bool) -> u32 {
    update::CLIENTS
        | if strict {
            update::SKIP_ON_PLACE
                | update::SKIP_BLOCK_ENTITY_SIDEEFFECTS
                | update::SUPPRESS_DROPS
                | update::KNOWN_SHAPE
        } else {
            update::SKIP_BLOCK_ENTITY_SIDEEFFECTS
        }
}

/// `SetBlockCommand.setBlock`.
pub(super) fn set_block(
    mode: SetBlockMode,
    args: &Args,
    source: &CommandSource,
    env: &mut Env<'_>,
) -> Result<i32, CommandError> {
    let pos = loaded_block_pos(args, "pos", source, env)?;
    let input = block_input(args, "block")?;
    let failed = || CommandError::new("commands.setblock.failed", vec![]);
    let level = &mut *env.level;
    if mode == SetBlockMode::Keep && !level.block(pos).is_air() {
        return Err(failed());
    }
    let place_needed = if mode == SetBlockMode::Destroy {
        level.destroy_block(pos, true, None);
        !BlockState(input.state).is_air() || !level.block(pos).is_air()
    } else {
        true
    };
    let strict = mode == SetBlockMode::Strict;
    let old = level.block(pos);
    if place_needed && !place(level, pos, input, place_flags(strict)) {
        return Err(failed());
    }
    if !strict {
        level.update_neighbours_on_block_set(pos, old);
    }
    source.send_success(
        env.global,
        Component::translatable_args(
            "commands.setblock.success",
            vec![Arg::Int(pos.x), Arg::Int(pos.y), Arg::Int(pos.z)],
        ),
        true,
    );
    Ok(1)
}

/// `FillCommand.fillBlocks`.
pub(super) fn fill(
    mode: FillMode,
    args: &Args,
    source: &CommandSource,
    env: &mut Env<'_>,
) -> Result<i32, CommandError> {
    let from = loaded_block_pos(args, "from", source, env)?;
    let to = loaded_block_pos(args, "to", source, env)?;
    let (min, max) = bounds(from, to);
    let input = block_input(args, "block")?;
    let area = volume(min, max);
    let limit = max_block_modifications(env);
    if area > i64::from(limit) {
        return Err(CommandError::new(
            "commands.fill.toobig",
            vec![ErrArg::Int(limit), ErrArg::Long(area)],
        ));
    }
    let filter = block_predicate(args, "filter");
    let strict = mode == FillMode::Strict;
    let flags = place_flags(strict);
    // `HOLLOW_CORE`.
    let air = BlockStateArg {
        block: "minecraft:air".to_owned(),
        state: BlockState::AIR.0,
        properties: Vec::new(),
        nbt: None,
    };
    let mut updated = Vec::new();
    let mut count = 0i32;
    // `BlockPos.betweenClosed`: x fastest, then y, then z.
    for z in min.z..=max.z {
        for y in min.y..=max.y {
            for x in min.x..=max.x {
                let pos = BlockPos::new(x, y, z);
                let state = env.level.block(pos);
                let passes = match (mode, filter) {
                    (FillMode::Keep, _) => state.is_air(),
                    (_, Some(pred)) => block_matches(env, pred, pos, state),
                    (_, None) => true,
                };
                if !passes {
                    continue;
                }
                let affected =
                    mode == FillMode::Destroy && env.level.destroy_block(pos, true, None);
                let edge = x == min.x
                    || x == max.x
                    || y == min.y
                    || y == max.y
                    || z == min.z
                    || z == max.z;
                let target = match mode {
                    FillMode::Outline if !edge => None,
                    FillMode::Hollow if !edge => Some(&air),
                    _ => Some(input),
                };
                match target {
                    Some(block) if place(env.level, pos, block, flags) => {
                        if !strict {
                            updated.push((pos, state));
                        }
                        count = count.saturating_add(1);
                    }
                    _ => {
                        if affected {
                            count = count.saturating_add(1);
                        }
                    }
                }
            }
        }
    }
    for (pos, old) in updated {
        env.level.update_neighbours_on_block_set(pos, old);
    }
    if count == 0 {
        return Err(CommandError::new("commands.fill.failed", vec![]));
    }
    source.send_success(
        env.global,
        Component::translatable_args("commands.fill.success", vec![Arg::Int(count)]),
        true,
    );
    Ok(count)
}

/// `Level.hasChunksAt(from, to)`.
fn has_chunks_at(level: &Level, a: BlockPos, b: BlockPos) -> bool {
    let (min, max) = bounds(a, b);
    if max.y < level.min_y() || min.y > level.max_y() {
        return false;
    }
    ((min.z >> 4)..=(max.z >> 4)).all(|cz| {
        ((min.x >> 4)..=(max.x >> 4)).all(|cx| level.chunks.chunk(ChunkPos::new(cx, cz)).is_some())
    })
}

/// A clone position (`BlockPosArgument.getLoadedBlockPos(context, level, name)`): checked
/// against `level` when that world is the command's.
fn clone_pos(
    args: &Args,
    name: &str,
    source: &CommandSource,
    level: Option<&Level>,
) -> Result<BlockPos, CommandError> {
    let c = args.coordinates(name).ok_or_else(|| missing(name))?;
    let pos = data::block_pos(source, c);
    if let Some(level) = level {
        if !level.in_height(pos.y) {
            return Err(CommandError::new("argument.pos.outofworld", vec![]));
        }
        if !level.is_loaded(pos) {
            return Err(not_loaded());
        }
    }
    Ok(pos)
}

/// `CloneCommands.CloneBlockInfo`: the block entity as its `saveCustomOnly` data.
struct CloneInfo {
    pos: BlockPos,
    state: BlockState,
    entity: Option<vanillite_nbt::Compound>,
    previous: BlockState,
}

/// What one clone copies: the source box, the offset to the destination and the options.
#[derive(Clone)]
struct ClonePlan {
    min: BlockPos,
    max: BlockPos,
    offset: BlockPos,
    filter: CloneFilter,
    pred: Option<vanillite_command::BlockPredicateArg>,
    mode: CloneMode,
    strict: bool,
}

/// The blocks a clone copies, in Vanilla's three lists, and the positions it clears.
#[derive(Default)]
struct CloneBlocks {
    solid: Vec<CloneInfo>,
    entities: Vec<CloneInfo>,
    other: Vec<CloneInfo>,
    clear: VecDeque<BlockPos>,
}

/// `saveCustomOnly`: the block entity data without `id`, `x`, `y`, `z`.
fn custom_only(mut nbt: vanillite_nbt::Compound) -> vanillite_nbt::Compound {
    for key in ["id", "x", "y", "z"] {
        nbt.remove(key);
    }
    nbt
}

/// The source part of `CloneCommands.clone`: collects the blocks of the source world (and
/// the destination's previous states when it is the same world), then clears the source for
/// `move`.
fn clone_collect(env: &mut Env<'_>, plan: &ClonePlan, same_world: bool) -> CloneBlocks {
    let mut out = CloneBlocks::default();
    let ClonePlan {
        min, max, offset, ..
    } = *plan;
    for z in min.z..=max.z {
        for y in min.y..=max.y {
            for x in min.x..=max.x {
                let from = BlockPos::new(x, y, z);
                let to = BlockPos::new(
                    x.saturating_add(offset.x),
                    y.saturating_add(offset.y),
                    z.saturating_add(offset.z),
                );
                let state = env.level.block(from);
                let copies = match plan.filter {
                    CloneFilter::All => true,
                    CloneFilter::Masked => !state.is_air(),
                    CloneFilter::Filtered => plan
                        .pred
                        .as_ref()
                        .is_some_and(|p| block_matches(env, p, from, state)),
                };
                if !copies {
                    continue;
                }
                let info = CloneInfo {
                    pos: to,
                    state,
                    entity: None,
                    previous: if same_world {
                        env.level.block(to)
                    } else {
                        BlockState::AIR
                    },
                };
                if let Some(nbt) = env.level.block_entity_nbt(from) {
                    out.entities.push(CloneInfo {
                        entity: Some(custom_only(nbt)),
                        ..info
                    });
                    out.clear.push_back(from);
                } else if !shared::is_solid_render(state)
                    && !shared::is_full_block(state.collision_shape())
                {
                    out.other.push(info);
                    out.clear.push_front(from);
                } else {
                    out.solid.push(info);
                    out.clear.push_back(from);
                }
            }
        }
    }
    if plan.mode == CloneMode::Move {
        let flags = clone_flags(plan.strict);
        let level = &mut *env.level;
        let barrier = ids::BARRIER.default_state();
        for &pos in &out.clear {
            level.set_block(pos, barrier, place_flags(true));
        }
        let standard = if plan.strict { flags } else { update::ALL };
        for &pos in &out.clear {
            level.set_block(pos, BlockState::AIR, standard);
        }
    }
    out
}

/// `2 | (strict ? 816 : 0)`.
fn clone_flags(strict: bool) -> u32 {
    if strict {
        place_flags(true)
    } else {
        update::CLIENTS
    }
}

/// The destination part of `CloneCommands.clone`: barriers, the blocks, the block entity data,
/// the neighbour updates; returns the number of blocks set.
fn clone_place(level: &mut Level, blocks: CloneBlocks, strict: bool, same_world: bool) -> i32 {
    let flags = clone_flags(strict);
    let barrier_flags = place_flags(true);
    let barrier = ids::BARRIER.default_state();
    let CloneBlocks {
        solid,
        mut entities,
        mut other,
        ..
    } = blocks;
    let solid_count = solid.len();
    let entity_count = entities.len();
    let mut all = solid;
    all.append(&mut entities);
    all.append(&mut other);
    if !same_world {
        for info in &mut all {
            info.previous = level.block(info.pos);
        }
    }
    for info in all.iter().rev() {
        level.set_block(info.pos, barrier, barrier_flags);
    }
    let mut count = 0i32;
    for info in &all {
        if level.set_block(info.pos, info.state, flags) {
            count = count.saturating_add(1);
        }
    }
    let with_entities = all
        .get(solid_count..solid_count.saturating_add(entity_count))
        .unwrap_or_default();
    for info in with_entities {
        // `loadCustomOnly` + `setComponents` + `setChanged`.
        if let Some(nbt) = &info.entity
            && level.block_entity_nbt(info.pos).is_some()
        {
            level.load_block_entity_nbt(info.pos, nbt);
        }
        level.set_block(info.pos, info.state, flags);
    }
    if !strict {
        for info in all.iter().rev() {
            level.update_neighbours_on_block_set(info.pos, info.previous);
        }
    }
    count
}

/// `CloneCommands.clone`. A source or destination in another world than the command's is
/// handled on that world's thread without waiting: the blocks are collected there, placed in
/// the destination world, and the feedback follows from there; the command counts the blocks
/// of the area then. Scheduled block ticks are copied within one world only.
pub(super) fn clone(
    (filter, mode, strict): (CloneFilter, CloneMode, bool),
    args: &Args,
    source: &CommandSource,
    env: &mut Env<'_>,
) -> Result<i32, CommandError> {
    let world_of = |name: &str| {
        if args.has(name) {
            dimension(args, name)
        } else {
            Ok(source.world)
        }
    };
    let from_world = world_of("sourceDimension")?;
    let to_world = world_of("targetDimension")?;
    let local = |w: crate::world::WorldId| (w == env.world).then_some(&*env.level);
    let begin = clone_pos(args, "begin", source, local(from_world))?;
    let end = clone_pos(args, "end", source, local(from_world))?;
    let destination = clone_pos(args, "destination", source, local(to_world))?;
    let (min, max) = bounds(begin, end);
    let dest_end = BlockPos::new(
        destination.x.saturating_add(max.x.saturating_sub(min.x)),
        destination.y.saturating_add(max.y.saturating_sub(min.y)),
        destination.z.saturating_add(max.z.saturating_sub(min.z)),
    );
    let (dest_min, dest_max) = bounds(destination, dest_end);
    let intersects = max.x >= dest_min.x
        && min.x <= dest_max.x
        && max.y >= dest_min.y
        && min.y <= dest_max.y
        && max.z >= dest_min.z
        && min.z <= dest_max.z;
    if mode == CloneMode::Normal && from_world == to_world && intersects {
        return Err(CommandError::new("commands.clone.overlap", vec![]));
    }
    let area = volume(min, max);
    let limit = max_block_modifications(env);
    if area > i64::from(limit) {
        return Err(CommandError::new(
            "commands.clone.toobig",
            vec![ErrArg::Int(limit), ErrArg::Long(area)],
        ));
    }
    if local(from_world).is_some_and(|l| !has_chunks_at(l, begin, end))
        || local(to_world).is_some_and(|l| !has_chunks_at(l, destination, dest_end))
    {
        return Err(not_loaded());
    }
    let plan = ClonePlan {
        min,
        max,
        offset: BlockPos::new(
            dest_min.x.saturating_sub(min.x),
            dest_min.y.saturating_sub(min.y),
            dest_min.z.saturating_sub(min.z),
        ),
        filter,
        pred: block_predicate(args, "filter").cloned(),
        mode,
        strict,
    };
    if from_world == env.world && to_world == env.world {
        let blocks = clone_collect(env, &plan, true);
        let count = clone_place(env.level, blocks, strict, true);
        env.level.chunks.copy_block_ticks(min, max, plan.offset);
        if count == 0 {
            return Err(CommandError::new("commands.clone.failed", vec![]));
        }
        source.send_success(
            env.global,
            Component::translatable_args("commands.clone.success", vec![Arg::Int(count)]),
            true,
        );
        return Ok(count);
    }
    let source = source.clone();
    let collect = move |env: &mut Env<'_>| {
        let blocks = clone_collect(env, &plan, false);
        let place = move |level: &mut Level, global: &crate::global::Global| {
            let count = clone_place(level, blocks, strict, false);
            if count == 0 {
                source.send_failure(
                    global,
                    Component::translatable("commands.clone.failed", vec![]),
                );
            } else {
                source.send_success(
                    global,
                    Component::translatable_args("commands.clone.success", vec![Arg::Int(count)]),
                    true,
                );
            }
        };
        if to_world == env.world {
            place(env.level, env.global);
        } else {
            super::entities_cmd::on_level(env, to_world, move |level, global, _| {
                place(level, global);
            });
        }
    };
    if from_world == env.world {
        collect(env);
    } else {
        super::entities_cmd::on_level(env, from_world, move |level, global, _| {
            let mut env = Env {
                level,
                global,
                world: from_world,
                others: Vec::new(),
            };
            collect(&mut env);
        });
    }
    Ok(i32::try_from(area).unwrap_or(i32::MAX))
}

/// `FillBiomeCommand.quantize`: the block at the start of its quart.
fn quantize(pos: BlockPos) -> BlockPos {
    BlockPos::new(pos.x & !3, pos.y & !3, pos.z & !3)
}

/// `FillBiomeCommand.fill`.
pub(super) fn fill_biome(
    args: &Args,
    source: &CommandSource,
    env: &mut Env<'_>,
) -> Result<i32, CommandError> {
    let from = quantize(loaded_block_pos(args, "from", source, env)?);
    let to = quantize(loaded_block_pos(args, "to", source, env)?);
    let name = args.identifier("biome").ok_or_else(|| missing("biome"))?;
    let filter = match args.game("filter") {
        Some(GameValue::ResourceOrTag(f)) => Some(f),
        _ => None,
    };
    let (min, max) = bounds(from, to);
    let area = volume(min, max);
    let limit = max_block_modifications(env);
    if area > i64::from(limit) {
        return Err(CommandError::new(
            "commands.fillbiome.toobig",
            vec![ErrArg::Int(limit), ErrArg::Long(area)],
        ));
    }
    let Some(climate) = env.level.climate() else {
        return Err(CommandError::new("commands.fillbiome.no_changes", vec![]));
    };
    let biomes = &climate.biomes;
    let biome = biomes.id_of(name).ok_or_else(|| {
        CommandError::new(
            "argument.resource.not_found",
            vec![
                ErrArg::Str(name.clone()),
                ErrArg::Str("minecraft:worldgen/biome".into()),
            ],
        )
    })?;
    // The ids the filter accepts.
    let accepted: Vec<bool> = biomes
        .all()
        .iter()
        .map(|b| match filter {
            None => true,
            Some(vanillite_command::ResourceOrTag::Resource(id)) => *id == b.name,
            Some(vanillite_command::ResourceOrTag::Tag(tag)) => {
                env.global
                    .systems
                    .in_tag("minecraft:worldgen/biome", tag, &b.name)
            }
        })
        .collect();
    let count = fill_biome_box(env.level, min, max, biome, &|b| {
        accepted.get(usize::from(b)).copied().unwrap_or(false)
    })?;
    if count == 0 {
        return Err(CommandError::new("commands.fillbiome.no_changes", vec![]));
    }
    source.send_success(
        env.global,
        Component::translatable_args(
            "commands.fillbiome.success.count",
            vec![
                Arg::Int(count),
                Arg::Int(min.x),
                Arg::Int(min.y),
                Arg::Int(min.z),
                Arg::Int(max.x),
                Arg::Int(max.y),
                Arg::Int(max.z),
            ],
        ),
        true,
    );
    Ok(count)
}

/// The loop of `FillBiomeCommand.fill` over quantized bounds: sets the biome of the quarts in
/// `min..=max` whose biome `accepted` lets through and sends the changed chunks' biomes to
/// their viewers. Returns the number of changed quarts; [`not_loaded`] when a chunk of the
/// box is not loaded (nothing changed then).
pub(crate) fn fill_biome_box(
    level: &mut Level,
    min: BlockPos,
    max: BlockPos,
    biome: vanillite_chunk::BiomeId,
    accepted: &dyn Fn(vanillite_chunk::BiomeId) -> bool,
) -> Result<i32, CommandError> {
    let mut chunks = Vec::new();
    for cz in (min.z >> 4)..=(max.z >> 4) {
        for cx in (min.x >> 4)..=(max.x >> 4) {
            let pos = ChunkPos::new(cx, cz);
            if level.chunks.chunk(pos).is_none() {
                return Err(not_loaded());
            }
            chunks.push(pos);
        }
    }
    let inside = |x: i32, y: i32, z: i32| {
        (min.x..=max.x).contains(&x) && (min.y..=max.y).contains(&y) && (min.z..=max.z).contains(&z)
    };
    let mut count = 0i32;
    let mut changed = Vec::new();
    for pos in chunks {
        let before = count;
        level.chunks.modify_chunk(pos, |proto| {
            let chunk = &mut proto.chunk;
            let min_section = chunk.min_section();
            for index in 0..chunk.sections().len() {
                let section_y = i32::try_from(index)
                    .unwrap_or(0)
                    .saturating_add(min_section);
                let section = chunk.section_mut(index);
                for (qx, qy, qz) in (0..4u32)
                    .flat_map(|x| (0..4u32).flat_map(move |y| (0..4u32).map(move |z| (x, y, z))))
                {
                    let (dx, dy, dz) = (
                        i32::try_from(qx * 4).unwrap_or(0),
                        i32::try_from(qy * 4).unwrap_or(0),
                        i32::try_from(qz * 4).unwrap_or(0),
                    );
                    let (x, y, z) = (
                        (pos.x << 4).saturating_add(dx),
                        (section_y << 4).saturating_add(dy),
                        (pos.z << 4).saturating_add(dz),
                    );
                    let current = section.biome(qx, qy, qz);
                    let passes = accepted(current);
                    if inside(x, y, z) && passes {
                        if current != biome {
                            count = count.saturating_add(1);
                        }
                        section.biomes_mut().set_at(qx, qy, qz, biome);
                    }
                }
            }
        });
        if count != before {
            changed.push(pos);
        }
    }
    if !changed.is_empty() {
        resend_biomes(level, &changed);
    }
    Ok(count)
}

/// `ChunkMap.resendBiomesForChunks`: `chunks_biomes` to every player that has the chunks.
fn resend_biomes(level: &Level, changed: &[ChunkPos]) {
    let data: Vec<ChunkBiomeData> = changed
        .iter()
        .filter_map(|&pos| {
            let chunk = level.chunks.chunk(pos)?;
            let mut buffer = Vec::new();
            for section in chunk.chunk.sections() {
                section.biomes().write(&mut buffer);
            }
            Some(ChunkBiomeData {
                x: pos.x,
                z: pos.z,
                buffer,
            })
        })
        .collect();
    for (_, player) in level.entities.players.iter() {
        let chunks = &player.connection.chunks;
        let Some(view) = chunks.view() else {
            continue;
        };
        let seen: Vec<ChunkBiomeData> = data
            .iter()
            .filter(|d| {
                let pos = ChunkPos::new(d.x, d.z);
                view.contains(pos, false) && !chunks.is_pending(pos)
            })
            .cloned()
            .collect();
        if !seen.is_empty() {
            player.connection.sink.send(&ChunksBiomes { chunks: seen });
        }
    }
}

//! `locate` (`LocateCommand`) and `place` (`PlaceCommand`) on the world's terrain generator.
//!
//! `locate structure` searches like `ChunkGenerator.findNearestMapStructure` and `locate biome`
//! like `BiomeSource.findClosestBiome3d`. Points of interest are not tracked, so `locate poi`
//! finds nothing. `place` checks its arguments and positions like Vanilla, but features,
//! structures, jigsaw pieces and templates only generate into chunks being generated, not
//! into the live world, so it then fails with Vanilla's failure message.

use std::time::Instant;

use vanillite_base::{BlockPos, ChunkPos};
use vanillite_command::error::CommandError;
use vanillite_command::{Arg as ErrArg, Args, GameValue, ResourceOrTag};
use vanillite_game::commands::{LocateOp, PlaceOp};
use vanillite_protocol::text::{Arg, ClickEvent, Color, Component, HoverEvent, NamedColor};
use vanillite_worldgen::biome::BiomeResolver;
use vanillite_worldgen::generator::TerrainGenerator;
use vanillite_worldgen::structure::placement::StructurePlacement;
use vanillite_worldgen::structure::state::StructureState;
use vanillite_worldgen::structure::{LevelHeight, StructureId, StructureSetId};

use super::Env;
use super::blocks_cmd::{missing, not_loaded, this_world};
use super::execute::loaded_block_pos;
use super::source::CommandSource;

/// `LocateCommand.MAX_STRUCTURE_SEARCH_RADIUS`.
const MAX_STRUCTURE_SEARCH_RADIUS: i32 = 100;
/// `MAX_BIOME_SEARCH_RADIUS`, `BIOME_SAMPLE_RESOLUTION_HORIZONTAL` and `_VERTICAL`.
const BIOME_SEARCH: (i32, i32, i32) = (6400, 32, 64);

fn source_block(source: &CommandSource) -> BlockPos {
    let p = source.pos;
    BlockPos::new(
        vanillite_base::math::floor(p.x),
        vanillite_base::math::floor(p.y),
        vanillite_base::math::floor(p.z),
    )
}

/// `asPrintable` of a resource or tag argument.
fn printable(arg: &ResourceOrTag) -> String {
    match arg {
        ResourceOrTag::Resource(id) => id.clone(),
        ResourceOrTag::Tag(tag) => format!("#{tag}"),
    }
}

fn resource_or_tag<'a>(args: &'a Args, name: &str) -> Result<&'a ResourceOrTag, CommandError> {
    match args.game(name) {
        Some(GameValue::ResourceOrTag(r)) => Ok(r),
        _ => Err(missing(name)),
    }
}

fn level_height(env: &Env<'_>) -> LevelHeight {
    LevelHeight {
        min_y: env.level.min_y(),
        height: env
            .level
            .max_y()
            .saturating_sub(env.level.min_y())
            .saturating_add(1),
    }
}

/// `locate ...` around the source in the world it runs on.
pub(super) fn locate(
    op: LocateOp,
    args: &Args,
    source: &CommandSource,
    env: &mut Env<'_>,
) -> Result<i32, CommandError> {
    this_world(source, env)?;
    let origin = source_block(source);
    let started = Instant::now();
    match op {
        LocateOp::Structure => {
            let arg = resource_or_tag(args, "structure")?;
            let ids = match arg {
                ResourceOrTag::Resource(id) => vec![id.clone()],
                ResourceOrTag::Tag(tag) => {
                    let tags = env.global.systems.tags();
                    tags.get("minecraft:worldgen/structure")
                        .and_then(|t| t.get(tag))
                        .cloned()
                        .ok_or_else(|| {
                            CommandError::new(
                                "commands.locate.structure.invalid",
                                vec![ErrArg::Str(printable(arg))],
                            )
                        })?
                }
            };
            let found = env
                .level
                .generation
                .as_ref()
                .filter(|g| g.structures)
                .and_then(|g| {
                    let wanted: Vec<StructureId> = ids
                        .iter()
                        .filter_map(|id| g.terrain.structures().structure_id(id))
                        .collect();
                    nearest_structure(
                        &g.terrain,
                        &wanted,
                        origin,
                        level_height(env),
                        MAX_STRUCTURE_SEARCH_RADIUS,
                    )
                    .map(|(pos, id)| (pos, g.terrain.structures().structure(id).name.clone()))
                });
            let Some((pos, name)) = found else {
                return Err(CommandError::new(
                    "commands.locate.structure.not_found",
                    vec![ErrArg::Str(printable(arg))],
                ));
            };
            Ok(show_result(
                source,
                env,
                arg,
                origin,
                (pos, &name),
                "commands.locate.structure.success",
                false,
                started,
            ))
        }
        LocateOp::Biome => {
            let arg = resource_or_tag(args, "biome")?;
            let found = env.level.generation.as_ref().and_then(|g| {
                let terrain = &g.terrain;
                let accepted = |id: u16| {
                    let name = &terrain.biomes().get(id).name;
                    match arg {
                        ResourceOrTag::Resource(r) => r == name,
                        ResourceOrTag::Tag(tag) => {
                            env.global
                                .systems
                                .in_tag("minecraft:worldgen/biome", tag, name)
                        }
                    }
                };
                closest_biome(terrain, origin, env, &accepted)
                    .map(|(pos, id)| (pos, terrain.biomes().get(id).name.clone()))
            });
            let Some((pos, name)) = found else {
                return Err(CommandError::new(
                    "commands.locate.biome.not_found",
                    vec![ErrArg::Str(printable(arg))],
                ));
            };
            Ok(show_result(
                source,
                env,
                arg,
                origin,
                (pos, &name),
                "commands.locate.biome.success",
                true,
                started,
            ))
        }
        // `PoiManager.findClosestWithType`: no point of interest is tracked yet.
        LocateOp::Poi => {
            let arg = resource_or_tag(args, "poi")?;
            Err(CommandError::new(
                "commands.locate.poi.not_found",
                vec![ErrArg::Str(printable(arg))],
            ))
        }
    }
}

/// `LocateCommand.showLocateResult`.
#[allow(clippy::too_many_arguments)]
fn show_result(
    source: &CommandSource,
    env: &Env<'_>,
    arg: &ResourceOrTag,
    origin: BlockPos,
    (found, name): (BlockPos, &str),
    key: &str,
    include_y: bool,
    started: Instant,
) -> i32 {
    let found_name = match arg {
        ResourceOrTag::Resource(_) => printable(arg),
        ResourceOrTag::Tag(_) => format!("{} ({name})", printable(arg)),
    };
    let dx = found.x.wrapping_sub(origin.x);
    let dy = found.y.wrapping_sub(origin.y);
    let dz = found.z.wrapping_sub(origin.z);
    #[allow(clippy::cast_possible_truncation, clippy::as_conversions)]
    let distance = if include_y {
        let sqr = f64::from(dx) * f64::from(dx)
            + f64::from(dy) * f64::from(dy)
            + f64::from(dz) * f64::from(dz);
        vanillite_base::math::floor_f((sqr as f32).sqrt())
    } else {
        // `dist`: int arithmetic like Java.
        let sqr = dx.wrapping_mul(dx).wrapping_add(dz.wrapping_mul(dz));
        vanillite_base::math::floor_f((sqr as f32).sqrt())
    };
    let y = if include_y {
        found.y.to_string()
    } else {
        "~".to_owned()
    };
    let coordinates = Component::translatable(
        "chat.square_brackets",
        vec![Component::translatable_args(
            "chat.coordinates",
            vec![Arg::Int(found.x), Arg::String(y.clone()), Arg::Int(found.z)],
        )],
    )
    .color(Color::Named(NamedColor::Green))
    .click(ClickEvent::SuggestCommand(format!(
        "/tp @s {} {y} {}",
        found.x, found.z
    )))
    .hover(HoverEvent::ShowText(Component::translatable(
        "chat.coordinates.tooltip",
        vec![],
    )));
    source.send_success(
        env.global,
        Component::translatable_args(
            key,
            vec![
                Arg::String(found_name.clone()),
                Arg::from(coordinates),
                Arg::Int(distance),
            ],
        ),
        false,
    );
    log::info!(
        "Locating element {found_name} took {} ms",
        started.elapsed().as_millis()
    );
    distance
}

/// `BlockPos.distSqr`.
fn dist_sqr(a: BlockPos, b: BlockPos) -> f64 {
    let d = |p: i32, q: i32| f64::from(p) - f64::from(q);
    let (x, y, z) = (d(a.x, b.x), d(a.y, b.y), d(a.z, b.z));
    x * x + y * y + z * z
}

/// `StructurePlacement.getLocatePos`.
fn locate_pos(placement: &StructurePlacement, chunk: ChunkPos) -> BlockPos {
    let offset = placement.spreading().map_or([0, 0, 0], |s| s.locate_offset);
    BlockPos::new(
        (chunk.x << 4).saturating_add(offset[0]),
        offset[1],
        (chunk.z << 4).saturating_add(offset[2]),
    )
}

/// `getStructureGeneratingAt` without stored chunks: the set's start in `chunk`, if wanted.
fn generating_at(
    terrain: &TerrainGenerator,
    state: &StructureState<'_>,
    set: StructureSetId,
    wanted: &[StructureId],
    chunk: ChunkPos,
    level: LevelHeight,
) -> Option<(BlockPos, StructureId)> {
    let id = terrain.structure_start_in(state, set, chunk, level)?;
    wanted.contains(&id).then(|| {
        (
            locate_pos(&terrain.structures().set(set).placement, chunk),
            id,
        )
    })
}

/// `ChunkGenerator.findNearestMapStructure(level, structures, pos, radius, false)`.
fn nearest_structure(
    terrain: &TerrainGenerator,
    wanted: &[StructureId],
    origin: BlockPos,
    level: LevelHeight,
    max_radius: i32,
) -> Option<(BlockPos, StructureId)> {
    let state = StructureState::new(terrain);
    let registry = terrain.structures();
    // `getPlacementsForStructure` of the wanted structures, by set.
    let sets: Vec<StructureSetId> = state
        .possible_structure_sets()
        .iter()
        .copied()
        .filter(|&id| {
            registry
                .set(id)
                .structures
                .iter()
                .any(|(s, _)| wanted.contains(s))
        })
        .collect();
    let mut nearest: Option<(BlockPos, StructureId)> = None;
    let mut best = f64::MAX;
    let mut spreads = Vec::new();
    for &set in &sets {
        match &registry.set(set).placement {
            StructurePlacement::ConcentricRings { rings, .. } => {
                let Some(positions) = state.ring_positions_for(rings) else {
                    continue;
                };
                let mut closest: Option<((BlockPos, StructureId), f64)> = None;
                for &chunk in positions {
                    let center = BlockPos::new((chunk.x << 4) + 8, 32, (chunk.z << 4) + 8);
                    let dist = dist_sqr(center, origin);
                    if closest.as_ref().is_none_or(|(_, d)| dist < *d)
                        && let Some(found) =
                            generating_at(terrain, &state, set, wanted, chunk, level)
                    {
                        closest = Some((found, dist));
                    }
                }
                if let Some((found, _)) = closest {
                    let dist = dist_sqr(origin, found.0);
                    if dist < best {
                        best = dist;
                        nearest = Some(found);
                    }
                }
            }
            StructurePlacement::RandomSpread { spacing, .. } => spreads.push((set, *spacing)),
            StructurePlacement::DimensionOrigin => {}
        }
    }
    if spreads.is_empty() {
        return nearest;
    }
    let (cx, cz) = (origin.x >> 4, origin.z >> 4);
    let seed = state.level_seed();
    for radius in 0..=max_radius {
        let mut found_something = false;
        for &(set, spacing) in &spreads {
            let placement = &registry.set(set).placement;
            let mut found = None;
            'ring: for x in -radius..=radius {
                let x_edge = x == -radius || x == radius;
                for z in -radius..=radius {
                    if !x_edge && z != -radius && z != radius {
                        continue;
                    }
                    let sector_x = cx.saturating_add(spacing.saturating_mul(x));
                    let sector_z = cz.saturating_add(spacing.saturating_mul(z));
                    let Some(chunk) = placement.potential_structure_chunk(seed, sector_x, sector_z)
                    else {
                        continue;
                    };
                    found = generating_at(terrain, &state, set, wanted, chunk, level);
                    if found.is_some() {
                        break 'ring;
                    }
                }
            }
            if let Some(found) = found {
                found_something = true;
                let dist = dist_sqr(origin, found.0);
                if dist < best {
                    best = dist;
                    nearest = Some(found);
                }
            }
        }
        if found_something {
            return nearest;
        }
    }
    nearest
}

/// `Mth.outFromOrigin(origin, lower, upper, step)`.
fn out_from_origin(origin: i32, lower: i32, upper: i32, step: i32) -> Vec<i32> {
    let mut out = Vec::new();
    if lower > upper || step < 1 {
        return out;
    }
    let center = origin.clamp(lower, upper);
    let mut cursor = center;
    loop {
        let distance = center.abs_diff(cursor);
        let distance = i32::try_from(distance).unwrap_or(i32::MAX);
        if !(center.saturating_sub(distance) >= lower || center.saturating_add(distance) <= upper) {
            break;
        }
        out.push(cursor);
        let previous_negative = cursor <= center;
        let can_move_positive = center.saturating_add(distance).saturating_add(step) <= upper;
        cursor = 'next: {
            if !previous_negative || !can_move_positive {
                let attempt = center
                    .saturating_sub(distance)
                    .saturating_sub(if previous_negative { step } else { 0 });
                if attempt >= lower {
                    break 'next attempt;
                }
            }
            center.saturating_add(distance).saturating_add(step)
        };
    }
    out
}

/// `BlockPos.spiralAround(ZERO, radius, EAST, SOUTH)`: calls `f` with each `(x, z)` until it
/// returns a value.
fn spiral<T>(radius: i32, mut f: impl FnMut(i32, i32) -> Option<T>) -> Option<T> {
    const DIRECTIONS: [(i32, i32); 4] = [(1, 0), (0, 1), (-1, 0), (0, -1)];
    let (mut x, mut z) = (0i32, 1i32);
    let legs = radius.saturating_mul(4);
    let (mut leg, mut leg_size, mut leg_index) = (-1i32, 0i32, 0i32);
    loop {
        let index = usize::try_from(leg.saturating_add(4) % 4).unwrap_or(0);
        let (dx, dz) = DIRECTIONS.get(index).copied().unwrap_or((0, 0));
        x = x.saturating_add(dx);
        z = z.saturating_add(dz);
        if leg_index >= leg_size {
            if leg >= legs {
                return None;
            }
            leg = leg.saturating_add(1);
            leg_index = 0;
            leg_size = leg / 2 + 1;
        }
        leg_index = leg_index.saturating_add(1);
        if let Some(found) = f(x, z) {
            return Some(found);
        }
    }
}

/// `BiomeSource.findClosestBiome3d(origin, 6400, 32, 64, allowed, ...)`.
fn closest_biome(
    terrain: &TerrainGenerator,
    origin: BlockPos,
    env: &Env<'_>,
    allowed: &dyn Fn(u16) -> bool,
) -> Option<(BlockPos, u16)> {
    let source = terrain.biome_source();
    let candidates: Vec<u16> = source
        .possible_biomes()
        .into_iter()
        .filter(|&b| allowed(b))
        .collect();
    if candidates.is_empty() {
        return None;
    }
    let (radius, horizontal, vertical) = BIOME_SEARCH;
    let mut resolver = source.create_caching_resolver(terrain.program(), terrain.climate());
    let ys = out_from_origin(
        origin.y,
        env.level.min_y().saturating_add(1),
        env.level.max_y().saturating_add(1),
        vertical,
    );
    spiral(radius.div_euclid(horizontal), |sx, sz| {
        let x = origin.x.saturating_add(sx.saturating_mul(horizontal));
        let z = origin.z.saturating_add(sz.saturating_mul(horizontal));
        ys.iter().find_map(|&y| {
            let biome = resolver.noise_biome(x >> 2, y >> 2, z >> 2);
            candidates
                .contains(&biome)
                .then(|| (BlockPos::new(x, y, z), biome))
        })
    })
}

/// `place ...`: arguments and positions as Vanilla checks them; the generation itself cannot
/// write into the live world yet (see the module comment).
pub(super) fn place(
    op: PlaceOp,
    args: &Args,
    source: &CommandSource,
    env: &mut Env<'_>,
) -> Result<i32, CommandError> {
    this_world(source, env)?;
    let pos_arg = match op {
        PlaceOp::Jigsaw => "position",
        _ => "pos",
    };
    let pos = if args.has(pos_arg) {
        loaded_block_pos(args, pos_arg, source, env)?
    } else {
        source_block(source)
    };
    // `checkLoaded` over the chunks the placement starts in.
    let check = |min: ChunkPos, max: ChunkPos| {
        let loaded = (min.x..=max.x).all(|x| {
            (min.z..=max.z).all(|z| env.level.chunks.chunk(ChunkPos::new(x, z)).is_some())
        });
        if loaded { Ok(()) } else { Err(not_loaded()) }
    };
    let chunk = pos.chunk();
    let failed = |key: &'static str| Err(CommandError::new(key, vec![]));
    match op {
        PlaceOp::Feature => {
            check(
                ChunkPos::new(chunk.x.saturating_sub(1), chunk.z.saturating_sub(1)),
                ChunkPos::new(chunk.x.saturating_add(1), chunk.z.saturating_add(1)),
            )?;
            failed("commands.place.feature.failed")
        }
        PlaceOp::Jigsaw => {
            check(chunk, chunk)?;
            failed("commands.place.jigsaw.failed")
        }
        PlaceOp::Structure => {
            let id = args
                .identifier("structure")
                .ok_or_else(|| missing("structure"))?;
            let known = env
                .level
                .generation
                .as_ref()
                .is_some_and(|g| g.terrain.structures().structure_id(id).is_some());
            if !known {
                return Err(CommandError::new(
                    "commands.place.structure.invalid",
                    vec![ErrArg::Str(id.clone())],
                ));
            }
            failed("commands.place.structure.failed")
        }
        PlaceOp::Template => {
            check(chunk, chunk)?;
            failed("commands.place.template.failed")
        }
    }
}

/// The structure search and biome tags of exploration maps
/// ([`vanillite_game::map::Explorer`]), with the registry tags of the loaded data packs.
/// Structures already explored are not skipped: starts carry no reference counts yet.
pub(crate) struct MapExplorer {
    pub(crate) tags: std::sync::Arc<crate::systems::RegistryTags>,
}

impl vanillite_game::map::Explorer for MapExplorer {
    fn nearest_structure(
        &self,
        terrain: &TerrainGenerator,
        tag: &str,
        origin: BlockPos,
        radius: i32,
        min_y: i32,
        height: i32,
    ) -> Option<BlockPos> {
        let names = self.tags.get("minecraft:worldgen/structure")?.get(tag)?;
        let wanted: Vec<StructureId> = names
            .iter()
            .filter_map(|n| terrain.structures().structure_id(n))
            .collect();
        if wanted.is_empty() {
            return None;
        }
        nearest_structure(
            terrain,
            &wanted,
            origin,
            LevelHeight { min_y, height },
            radius,
        )
        .map(|(pos, _)| pos)
    }

    fn water_on_map_outlines<'a>(
        &'a self,
        terrain: &'a TerrainGenerator,
    ) -> Box<dyn FnMut(BlockPos) -> bool + 'a> {
        let watery = self
            .tags
            .get("minecraft:worldgen/biome")
            .and_then(|t| t.get("minecraft:water_on_map_outlines"));
        let mut resolver = terrain
            .biome_source()
            .create_caching_resolver(terrain.program(), terrain.climate());
        Box::new(move |pos: BlockPos| {
            let biome = resolver.noise_biome(pos.x >> 2, pos.y >> 2, pos.z >> 2);
            let name = &terrain.biomes().get(biome).name;
            watery.is_some_and(|w| w.iter().any(|n| n == name))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{out_from_origin, spiral};

    #[test]
    fn samples_out_from_origin_like_vanilla() {
        assert_eq!(out_from_origin(5, 0, 10, 2), vec![5, 7, 3, 9, 1]);
        assert_eq!(out_from_origin(0, 0, 4, 1), vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn spirals_from_the_centre() {
        let mut seen = Vec::new();
        let none: Option<()> = spiral(1, |x, z| {
            seen.push((x, z));
            None
        });
        assert!(none.is_none());
        assert_eq!(seen.first(), Some(&(0, 0)));
        assert_eq!(seen.len(), 9);
    }
}

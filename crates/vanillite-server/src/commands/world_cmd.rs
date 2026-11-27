//! Commands on the state of a world or the server: `forceload` (`ForceLoadCommand`),
//! `setworldspawn` (`SetWorldSpawnCommand`), `spawnpoint` (`SetSpawnCommand`), `difficulty`
//! (`DifficultyCommand`), `defaultgamemode` (`DefaultGameModeCommands`), `spreadplayers`
//! (`SpreadPlayersCommand`) and `tick` (`TickCommand`).

use vanillite_base::random::{LegacyRandom, RandomSource};
use vanillite_base::{BlockPos, ChunkPos, Vec3};
use vanillite_command::error::CommandError;
use vanillite_command::{Arg as ErrArg, Args};
use vanillite_data::block::BlockState;
use vanillite_game::commands::{ForceLoadOp, TickOp};
use vanillite_game::level::Level;
use vanillite_game::player::join::display_name;
use vanillite_game::survival::player::RespawnConfig;
use vanillite_game::survival::rules::Difficulty;
use vanillite_protocol::packets::play::clientbound::SetDefaultSpawnPosition;
use vanillite_protocol::text::{Arg, Component};
use vanillite_protocol::types::{GlobalPos, Identifier};
use vanillite_storage::level_dat::Spawn;

use super::blocks_cmd::{missing, this_world};
use super::border_cmd::java_fixed;
use super::select::{self, Target};
use super::source::{CommandSource, SourceKind};
use super::{Env, exec};
use crate::global::Global;
use crate::server::DIMENSIONS;
use crate::tick_rate::SprintReport;
use crate::world::WorldId;

fn dimension_name(world: WorldId) -> &'static str {
    DIMENSIONS
        .get(world.index())
        .copied()
        .unwrap_or("minecraft:overworld")
}

fn text(s: impl Into<String>) -> Arg {
    Arg::from(Component::text(s.into()))
}

/// `ChunkPos.toString`.
fn chunk_text(pos: ChunkPos) -> String {
    format!("[{}, {}]", pos.x, pos.z)
}

/// `ColumnPosArgument.getColumnPos`.
fn column(args: &Args, name: &str, source: &CommandSource) -> Result<(i32, i32), CommandError> {
    let c = args.coordinates(name).ok_or_else(|| missing(name))?;
    let pos = exec::resolve_position(source, c);
    Ok((
        vanillite_base::math::floor(pos.x),
        vanillite_base::math::floor(pos.z),
    ))
}

/// `forceload ...` on the world of the source.
pub(super) fn force_load(
    op: ForceLoadOp,
    args: &Args,
    source: &CommandSource,
    env: &mut Env<'_>,
) -> Result<i32, CommandError> {
    this_world(source, env)?;
    let dimension = dimension_name(env.world);
    let global = env.global;
    match op {
        ForceLoadOp::List => {
            let forced = env.level.forced.sorted();
            let count = i32::try_from(forced.len()).unwrap_or(i32::MAX);
            let list = forced
                .iter()
                .map(|p| chunk_text(*p))
                .collect::<Vec<_>>()
                .join(", ");
            let message = match forced.len() {
                0 => {
                    source.send_failure(
                        global,
                        Component::translatable_args(
                            "commands.forceload.added.none",
                            vec![text(dimension)],
                        ),
                    );
                    return Ok(0);
                }
                1 => Component::translatable_args(
                    "commands.forceload.list.single",
                    vec![text(dimension), Arg::String(list)],
                ),
                _ => Component::translatable_args(
                    "commands.forceload.list.multiple",
                    vec![Arg::Int(count), text(dimension), Arg::String(list)],
                ),
            };
            source.send_success(global, message, false);
            Ok(count)
        }
        ForceLoadOp::Query => {
            let (x, z) = column(args, "pos", source)?;
            let pos = ChunkPos::new(x >> 4, z >> 4);
            if !env.level.forced.contains(pos) {
                return Err(CommandError::new(
                    "commands.forceload.query.failure",
                    vec![
                        ErrArg::Str(chunk_text(pos)),
                        ErrArg::Str(dimension.to_owned()),
                    ],
                ));
            }
            source.send_success(
                global,
                Component::translatable_args(
                    "commands.forceload.query.success",
                    vec![text(chunk_text(pos)), text(dimension)],
                ),
                false,
            );
            Ok(1)
        }
        ForceLoadOp::RemoveAll => {
            let Level { chunks, forced, .. } = &mut *env.level;
            for pos in forced.sorted() {
                forced.set(chunks, pos, false);
            }
            source.send_success(
                global,
                Component::translatable_args(
                    "commands.forceload.removed.all",
                    vec![text(dimension)],
                ),
                true,
            );
            Ok(0)
        }
        ForceLoadOp::Add | ForceLoadOp::Remove => {
            let add = op == ForceLoadOp::Add;
            let from = column(args, "from", source)?;
            let to = if args.has("to") {
                column(args, "to", source)?
            } else {
                from
            };
            let (min_x, min_z) = (from.0.min(to.0), from.1.min(to.1));
            let (max_x, max_z) = (from.0.max(to.0), from.1.max(to.1));
            let limit = 30_000_000;
            if min_x < -limit || min_z < -limit || max_x >= limit || max_z >= limit {
                return Err(CommandError::new("argument.pos.outofworld", vec![]));
            }
            let (min, max) = (
                ChunkPos::new(min_x >> 4, min_z >> 4),
                ChunkPos::new(max_x >> 4, max_z >> 4),
            );
            let span = |a: i32, b: i32| i64::from(b) - i64::from(a) + 1;
            let count = span(min.x, max.x).saturating_mul(span(min.z, max.z));
            if count > 256 {
                return Err(CommandError::new(
                    "commands.forceload.toobig",
                    vec![ErrArg::Int(256), ErrArg::Long(count)],
                ));
            }
            // `CommandResponseTracker` over the changed chunks.
            let mut changed = Vec::new();
            let Level { chunks, forced, .. } = &mut *env.level;
            for x in min.x..=max.x {
                for z in min.z..=max.z {
                    let pos = ChunkPos::new(x, z);
                    if forced.set(chunks, pos, add) {
                        changed.push(pos);
                    }
                }
            }
            let (single, multiple, failure) = if add {
                (
                    "commands.forceload.added.single",
                    "commands.forceload.added.multiple",
                    "commands.forceload.added.failure",
                )
            } else {
                (
                    "commands.forceload.removed.single",
                    "commands.forceload.removed.multiple",
                    "commands.forceload.removed.failure",
                )
            };
            let total = i32::try_from(changed.len()).unwrap_or(i32::MAX);
            let message = match changed.as_slice() {
                [] => return Err(CommandError::new(failure, vec![])),
                [one] => Component::translatable_args(
                    single,
                    vec![text(chunk_text(*one)), text(dimension)],
                ),
                _ => Component::translatable_args(
                    multiple,
                    vec![
                        Arg::Int(total),
                        text(dimension),
                        text(chunk_text(min)),
                        text(chunk_text(max)),
                    ],
                ),
            };
            source.send_success(global, message, true);
            Ok(total)
        }
    }
}

/// `BlockPosArgument.getSpawnablePos`, or the source's block when `pos` is not given.
fn spawn_pos(args: &Args, source: &CommandSource) -> Result<BlockPos, CommandError> {
    let Some(c) = args.coordinates("pos") else {
        let p = source.pos;
        return Ok(BlockPos::new(
            vanillite_base::math::floor(p.x),
            vanillite_base::math::floor(p.y),
            vanillite_base::math::floor(p.z),
        ));
    };
    let pos = super::data::block_pos(source, c);
    let horizontal = |v: i32| (-30_000_000..30_000_000).contains(&v);
    if !(-20_000_000..20_000_000).contains(&pos.y) || !horizontal(pos.x) || !horizontal(pos.z) {
        return Err(CommandError::new("argument.pos.outofbounds", vec![]));
    }
    Ok(pos)
}

/// The rotation argument as `(yaw, pitch)`, zero when not given.
fn rotation(args: &Args, source: &CommandSource) -> (f32, f32) {
    args.coordinates("rotation")
        .map_or((0.0, 0.0), |c| exec::resolve_rotation(source, c))
}

/// `Mth.wrapDegrees`.
fn wrap_degrees(v: f32) -> f32 {
    let w = v % 360.0;
    if w >= 180.0 {
        w - 360.0
    } else if w < -180.0 {
        w + 360.0
    } else {
        w
    }
}

/// `SetWorldSpawnCommand.setSpawn` with `MinecraftServer.setRespawnData`: `level.dat`, the
/// server's command position, every world's spawn and the clients.
pub(super) fn set_world_spawn(
    args: &Args,
    source: &CommandSource,
    env: &mut Env<'_>,
) -> Result<i32, CommandError> {
    let pos = spawn_pos(args, source)?;
    let (yaw, pitch) = rotation(args, source);
    let (yaw, pitch) = (wrap_degrees(yaw), pitch.clamp(-90.0, 90.0));
    let dimension = dimension_name(source.world);
    let global = env.global;
    let spawn = Spawn {
        dimension: dimension.to_owned(),
        pos: [pos.x, pos.y, pos.z],
        yaw,
        pitch,
    };
    if global.level.spawn().as_ref() != Some(&spawn) {
        global.level.set_spawn(spawn);
        let at = Vec3::new(
            f64::from(pos.x) + 0.5,
            f64::from(pos.y),
            f64::from(pos.z) + 0.5,
        );
        global.systems.set_spawn(at);
        for index in 0..DIMENSIONS.len() {
            let Ok(index) = u16::try_from(index) else {
                continue;
            };
            global.run_in_world(WorldId(index), move |w| w.players.level.spawn = (at, yaw));
        }
        if let Ok(dimension) = Identifier::new(dimension) {
            global.players.broadcast(
                &SetDefaultSpawnPosition {
                    pos: GlobalPos {
                        dimension,
                        pos: vanillite_protocol::types::BlockPos {
                            x: pos.x,
                            y: pos.y,
                            z: pos.z,
                        },
                    },
                    yaw,
                    pitch,
                },
                None,
            );
        }
    }
    source.send_success(
        global,
        Component::translatable_args(
            "commands.setworldspawn.success",
            vec![
                Arg::Int(pos.x),
                Arg::Int(pos.y),
                Arg::Int(pos.z),
                Arg::Float(yaw),
                Arg::Float(pitch),
                Arg::String(dimension.to_owned()),
            ],
        ),
        true,
    );
    Ok(1)
}

/// `SetSpawnCommand.setSpawn`: the targets' respawn position (`setRespawnPosition`), forced.
pub(super) fn spawn_point(
    targets: &[Target],
    args: &Args,
    source: &CommandSource,
    env: &mut Env<'_>,
) -> Result<i32, CommandError> {
    let pos = spawn_pos(args, source)?;
    let (yaw, pitch) = rotation(args, source);
    let config = RespawnConfig {
        dimension: dimension_name(source.world).to_owned(),
        pos,
        yaw: wrap_degrees(yaw),
        pitch: pitch.clamp(-90.0, 90.0),
        forced: true,
    };
    for target in targets {
        let local = target
            .entity
            .and_then(|id| env.level.entities.player_mut(id));
        if let Some(player) = local {
            player.survival.respawn = Some(config.clone());
        } else {
            let (uuid, config) = (target.uuid, config.clone());
            env.global.run_in_world(target.world, move |w| {
                let entities = w.entities_mut();
                if let Some(id) = exec::find_player(entities, uuid)
                    && let Some(player) = entities.player_mut(id)
                {
                    player.survival.respawn = Some(config);
                }
            });
        }
    }
    let count = i32::try_from(targets.len()).unwrap_or(i32::MAX);
    let mut args = vec![
        Arg::Int(pos.x),
        Arg::Int(pos.y),
        Arg::Int(pos.z),
        Arg::Float(config.yaw),
        Arg::Float(config.pitch),
        text(config.dimension.clone()),
    ];
    let key = match targets {
        [one] => {
            args.push(Arg::from(display_name(&one.name, one.uuid)));
            "commands.spawnpoint.success.single"
        }
        _ => {
            args.push(Arg::Int(count));
            "commands.spawnpoint.success.multiple"
        }
    };
    source.send_success(env.global, Component::translatable_args(key, args), true);
    Ok(count)
}

/// `Difficulty.getDisplayName`.
fn difficulty_name(difficulty: Difficulty) -> Component {
    let name = match difficulty {
        Difficulty::Peaceful => "peaceful",
        Difficulty::Easy => "easy",
        Difficulty::Normal => "normal",
        Difficulty::Hard => "hard",
    };
    Component::translatable(format!("options.difficulty.{name}"), vec![])
}

/// `difficulty` without an argument: the difficulty and its id.
pub(super) fn difficulty_query(source: &CommandSource, global: &Global) -> i32 {
    let difficulty = global.level.difficulty();
    source.send_success(
        global,
        Component::translatable(
            "commands.difficulty.query",
            vec![difficulty_name(difficulty)],
        ),
        false,
    );
    difficulty.id()
}

/// `DifficultyCommand.setDifficulty` (`MinecraftServer.setDifficulty(difficulty, true)`).
pub(super) fn difficulty_set(
    source: &CommandSource,
    global: &Global,
    id: u8,
) -> Result<i32, CommandError> {
    let difficulty = Difficulty::from_id(i32::from(id));
    if global.level.difficulty() == difficulty {
        return Err(CommandError::new(
            "commands.difficulty.failure",
            vec![ErrArg::Component(Box::new(difficulty_name(difficulty)))],
        ));
    }
    global.level.set_difficulty(difficulty);
    global
        .players
        .broadcast(&crate::survival::change_difficulty(&global.level), None);
    source.send_success(
        global,
        Component::translatable(
            "commands.difficulty.success",
            vec![difficulty_name(difficulty)],
        ),
        true,
    );
    Ok(0)
}

/// `DefaultGameModeCommands.setMode`: `level.dat`'s game type; with `force-gamemode` every
/// online player gets it.
pub(super) fn default_game_mode(
    args: &Args,
    source: &CommandSource,
    env: &mut Env<'_>,
) -> Result<i32, CommandError> {
    let mode = exec::config_game_mode(
        *args
            .game_mode("gamemode")
            .ok_or_else(|| missing("gamemode"))?,
    );
    let global = env.global;
    global
        .level
        .set_default_game_type(crate::players::game_mode_id(mode));
    let mut count = 0i32;
    if global.force_game_mode {
        for target in select::online_players(&env.level.entities, global, env.world) {
            if exec::enforce_game_mode(&mut env.level.entities, global, &target, mode) {
                count = count.saturating_add(1);
            }
        }
    }
    let name = exec::mode_component(mode);
    source.send_success(
        global,
        Component::translatable("commands.defaultgamemode.success", vec![name]),
        true,
    );
    Ok(count)
}

/// `SpreadPlayersCommand.Position`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Spot {
    x: f64,
    z: f64,
}

impl Spot {
    fn dist(self, other: Spot) -> f64 {
        let (dx, dz) = (self.x - other.x, self.z - other.z);
        (dx * dx + dz * dz).sqrt()
    }

    fn randomize(&mut self, random: &mut LegacyRandom, (min_x, min_z, max_x, max_z): Area) {
        let next = |random: &mut LegacyRandom, min: f64, max: f64| {
            if min >= max {
                min
            } else {
                random.next_double() * (max - min) + min
            }
        };
        self.x = next(random, min_x, max_x);
        self.z = next(random, min_z, max_z);
    }

    fn clamp(&mut self, (min_x, min_z, max_x, max_z): Area) -> bool {
        let mut changed = false;
        if self.x < min_x {
            self.x = min_x;
            changed = true;
        } else if self.x > max_x {
            self.x = max_x;
            changed = true;
        }
        if self.z < min_z {
            self.z = min_z;
            changed = true;
        } else if self.z > max_z {
            self.z = max_z;
            changed = true;
        }
        changed
    }

    /// `getSpawnY`: above the highest block with two air blocks over it, below `max_height`.
    fn spawn_y(self, level: &Level, max_height: i32) -> i32 {
        let (x, z) = (
            vanillite_base::math::floor(self.x),
            vanillite_base::math::floor(self.z),
        );
        let air = |y: i32| level.block(BlockPos::new(x, y, z)).is_air();
        let mut y = max_height.saturating_add(1);
        let mut air2 = air(y);
        y = y.saturating_sub(1);
        let mut air1 = air(y);
        while y > level.min_y() {
            y = y.saturating_sub(1);
            let current = air(y);
            if !current && air1 && air2 {
                return y.saturating_add(1);
            }
            air2 = air1;
            air1 = current;
        }
        max_height.saturating_add(1)
    }

    /// `isSafe`.
    fn is_safe(self, level: &Level, global: &Global, max_height: i32) -> bool {
        let y = self.spawn_y(level, max_height).saturating_sub(1);
        let pos = BlockPos::new(
            vanillite_base::math::floor(self.x),
            y,
            vanillite_base::math::floor(self.z),
        );
        let state: BlockState = level.block(pos);
        y < max_height
            && !vanillite_game::block::is_liquid(state)
            && global.systems.in_tag(
                "minecraft:block",
                "minecraft:entities_can_teleport_to",
                state.block().name(),
            )
    }
}

/// `minX, minZ, maxX, maxZ`.
type Area = (f64, f64, f64, f64);

/// `SpreadPlayersCommand.spreadPlayers` on the world of the source.
pub(super) fn spread_players(
    targets: Vec<Target>,
    args: &Args,
    source: &CommandSource,
    env: &mut Env<'_>,
) -> Result<i32, CommandError> {
    this_world(source, env)?;
    let c = args
        .coordinates("center")
        .ok_or_else(|| missing("center"))?;
    let center = exec::resolve_position(source, c);
    // `Vec2Argument.getVec2`: floats.
    #[allow(clippy::cast_possible_truncation, clippy::as_conversions)]
    let (cx, cz) = (center.x as f32, center.z as f32);
    let distance = f64::from(
        *args
            .float("spreadDistance")
            .ok_or_else(|| missing("spreadDistance"))?,
    );
    let range = f64::from(*args.float("maxRange").ok_or_else(|| missing("maxRange"))?);
    let teams = *args
        .bool("respectTeams")
        .ok_or_else(|| missing("respectTeams"))?;
    let max_height = match args.int("maxHeight") {
        Some(h) => *h,
        None => env.level.max_y().saturating_add(1),
    };
    let min_y = env.level.min_y();
    if max_height < min_y {
        return Err(CommandError::new(
            "commands.spreadplayers.failed.invalid.height",
            vec![ErrArg::Int(max_height), ErrArg::Int(min_y)],
        ));
    }
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| {
            i64::from(d.subsec_nanos()) ^ i64::try_from(d.as_secs()).unwrap_or(0)
        });
    let mut random = LegacyRandom::new(seed);
    let area: Area = (
        f64::from(cx) - range,
        f64::from(cz) - range,
        f64::from(cx) + range,
        f64::from(cz) + range,
    );
    // `getNumberOfTeams`: the players' teams and one for all other entities.
    let team_of = |t: &Target| {
        t.is_player()
            .then(|| {
                env.global
                    .systems
                    .scoreboard()
                    .team_of(&t.name)
                    .map(|team| team.name.clone())
            })
            .flatten()
    };
    let team_keys: Vec<Option<String>> = targets.iter().map(team_of).collect();
    let count = if teams {
        let mut distinct: Vec<&Option<String>> = Vec::new();
        for key in &team_keys {
            if !distinct.contains(&key) {
                distinct.push(key);
            }
        }
        distinct.len()
    } else {
        targets.len()
    };
    let mut spots = vec![Spot::default(); count];
    for spot in &mut spots {
        spot.randomize(&mut random, area);
    }
    spread(
        &mut spots,
        distance,
        env,
        &mut random,
        area,
        max_height,
        teams,
        (cx, cz),
    )?;
    // `setPlayerPositions`.
    let mut total = 0.0;
    let mut next = 0usize;
    let mut assigned: Vec<(Option<String>, usize)> = Vec::new();
    for (target, key) in targets.iter().zip(&team_keys) {
        let index = if teams {
            match assigned.iter().find(|(k, _)| k == key) {
                Some((_, i)) => *i,
                None => {
                    assigned.push((key.clone(), next));
                    next = next.saturating_add(1);
                    next.saturating_sub(1)
                }
            }
        } else {
            next = next.saturating_add(1);
            next.saturating_sub(1)
        };
        let Some(spot) = spots.get(index).copied() else {
            continue;
        };
        let pos = Vec3::new(
            f64::from(vanillite_base::math::floor(spot.x)) + 0.5,
            f64::from(spot.spawn_y(env.level, max_height)),
            f64::from(vanillite_base::math::floor(spot.z)) + 0.5,
        );
        move_entity(env, target, pos)?;
        let closest = spots
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != index)
            .map(|(_, s)| spot.dist(*s))
            .fold(f64::MAX, f64::min);
        total += closest;
    }
    #[allow(clippy::cast_precision_loss, clippy::as_conversions)]
    let distance = if targets.len() < 2 {
        0.0
    } else {
        total / targets.len() as f64
    };
    let key = if teams {
        "commands.spreadplayers.success.teams"
    } else {
        "commands.spreadplayers.success.entities"
    };
    let positions = i32::try_from(spots.len()).unwrap_or(i32::MAX);
    source.send_success(
        env.global,
        Component::translatable_args(
            key,
            vec![
                Arg::Int(positions),
                Arg::Float(cx),
                Arg::Float(cz),
                Arg::String(java_fixed(distance, 2)),
            ],
        ),
        true,
    );
    Ok(positions)
}

/// `Entity.teleportTo` with the entity's rotation.
fn move_entity(env: &mut Env<'_>, target: &Target, pos: Vec3) -> Result<(), CommandError> {
    let Some(id) = target.entity else {
        return Ok(());
    };
    if env.level.entities.player(id).is_some() {
        let (y_rot, x_rot) = target.place.map_or((0.0, 0.0), |(_, y, x)| (y, x));
        return exec::teleport_local(env.level, target, pos, 0, y_rot, x_rot);
    }
    let entities = &mut env.level.entities;
    if let Some(mut entity) = entities.store.get_mut(id) {
        entity.set_pos(pos);
    }
    Ok(())
}

/// `spreadPositions`: moves the spots apart until none is closer than `spread` and all stand
/// safely, at most 10000 rounds.
#[allow(clippy::too_many_arguments)]
fn spread(
    spots: &mut [Spot],
    spread: f64,
    env: &Env<'_>,
    random: &mut LegacyRandom,
    area: Area,
    max_height: i32,
    teams: bool,
    (cx, cz): (f32, f32),
) -> Result<(), CommandError> {
    let max_float = f64::from(f32::MAX);
    let mut collisions = true;
    let mut min_distance = max_float;
    let mut iteration = 0;
    while iteration < 10_000 && collisions {
        collisions = false;
        min_distance = max_float;
        for i in 0..spots.len() {
            let Some(spot) = spots.get(i).copied() else {
                continue;
            };
            let mut neighbours = 0u32;
            let mut average = Spot::default();
            for (j, other) in spots.iter().enumerate() {
                if i == j {
                    continue;
                }
                let dist = spot.dist(*other);
                min_distance = min_distance.min(dist);
                if dist < spread {
                    neighbours = neighbours.saturating_add(1);
                    average.x += other.x - spot.x;
                    average.z += other.z - spot.z;
                }
            }
            let Some(spot) = spots.get_mut(i) else {
                continue;
            };
            if neighbours > 0 {
                average.x /= f64::from(neighbours);
                average.z /= f64::from(neighbours);
                let length = (average.x * average.x + average.z * average.z).sqrt();
                if length > 0.0 {
                    spot.x -= average.x / length;
                    spot.z -= average.z / length;
                } else {
                    spot.randomize(random, area);
                }
                collisions = true;
            }
            if spot.clamp(area) {
                collisions = true;
            }
        }
        if !collisions {
            for spot in spots.iter_mut() {
                if !spot.is_safe(env.level, env.global, max_height) {
                    spot.randomize(random, area);
                    collisions = true;
                }
            }
        }
        iteration += 1;
    }
    if min_distance == max_float {
        min_distance = 0.0;
    }
    if iteration >= 10_000 {
        let key = if teams {
            "commands.spreadplayers.failed.teams"
        } else {
            "commands.spreadplayers.failed.entities"
        };
        return Err(CommandError::new(
            key,
            vec![
                ErrArg::Int(i32::try_from(spots.len()).unwrap_or(i32::MAX)),
                ErrArg::Float(cx),
                ErrArg::Float(cz),
                ErrArg::Str(java_fixed(min_distance, 2)),
            ],
        ));
    }
    Ok(())
}

/// `updateStateToClients`.
fn broadcast_state(global: &Global) {
    let (state, _) = crate::border::ticking(global);
    global.players.broadcast(&state, None);
}

/// `updateStepTicks`.
fn broadcast_step(global: &Global) {
    let (_, step) = crate::border::ticking(global);
    global.players.broadcast(&step, None);
}

/// `finishTickSprint`'s report from the server's own source, and the restored freeze.
pub(crate) fn sprint_finished(global: &Global, report: &SprintReport) {
    if let Some(arc) = global.arc() {
        let source = CommandSource::new(
            arc,
            SourceKind::Console,
            4,
            WorldId(0),
            global.systems.spawn(),
            (0.0, 0.0),
            None,
        );
        source.send_success(
            global,
            Component::translatable_args(
                "commands.tick.sprint.report",
                vec![
                    Arg::Int(report.ticks_per_second),
                    Arg::String(report.millis_per_tick.clone()),
                ],
            ),
            true,
        );
    }
    broadcast_state(global);
}

/// `TickCommand.nanosToMilisString`.
fn millis(nanos: u64) -> String {
    #[allow(clippy::cast_precision_loss, clippy::as_conversions)]
    let ms = nanos as f32 / 1_000_000.0_f32;
    java_fixed(f64::from(ms), 1)
}

/// `tick ...`.
pub(super) fn tick(
    op: TickOp,
    args: &Args,
    source: &CommandSource,
    global: &Global,
) -> Result<i32, CommandError> {
    let say = |key: &str, args: Vec<Arg>, broadcast: bool| {
        source.send_success(global, Component::translatable_args(key, args), broadcast);
    };
    match op {
        TickOp::Query => {
            let (samples, average) = {
                let times = global
                    .tick_times
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                (*times.samples(), times.average_nanos())
            };
            let (rate, sprinting, frozen, nanos_per_tick) = {
                let r = global.tick_rate();
                (
                    r.tick_rate(),
                    r.is_sprinting(),
                    r.is_frozen(),
                    r.nanos_per_tick(),
                )
            };
            let busy = Arg::String(millis(average));
            let rate_text = Arg::String(java_fixed(f64::from(rate), 1));
            if sprinting {
                say("commands.tick.status.sprinting", vec![], false);
                say(
                    "commands.tick.query.rate.sprinting",
                    vec![rate_text, busy],
                    false,
                );
            } else {
                let status = if frozen {
                    "commands.tick.status.frozen"
                } else if nanos_per_tick < average {
                    "commands.tick.status.lagging"
                } else {
                    "commands.tick.status.running"
                };
                say(status, vec![], false);
                say(
                    "commands.tick.query.rate.running",
                    vec![rate_text, busy, Arg::String(millis(nanos_per_tick))],
                    false,
                );
            }
            let mut sorted = samples;
            sorted.sort_unstable();
            let at = |i: usize| Arg::String(millis(sorted.get(i).copied().unwrap_or(0)));
            say(
                "commands.tick.query.percentiles",
                vec![at(50), at(95), at(99), Arg::Int(100)],
                false,
            );
            #[allow(clippy::cast_possible_truncation, clippy::as_conversions)]
            Ok(rate as i32)
        }
        TickOp::Rate => {
            let rate = *args.float("rate").ok_or_else(|| missing("rate"))?;
            global.tick_rate().set_tick_rate(rate);
            broadcast_state(global);
            say(
                "commands.tick.rate.success",
                vec![Arg::String(java_fixed(f64::from(rate), 1))],
                true,
            );
            #[allow(clippy::cast_possible_truncation, clippy::as_conversions)]
            Ok(rate as i32)
        }
        TickOp::Step => {
            let advance = args.time("time").copied().unwrap_or(1);
            let ticks = u32::try_from(advance).unwrap_or(0);
            if !global.tick_rate().step(ticks) {
                return Err(CommandError::new("commands.tick.step.fail", vec![]));
            }
            broadcast_step(global);
            say("commands.tick.step.success", vec![Arg::Int(advance)], true);
            Ok(1)
        }
        TickOp::StepStop => {
            if !global.tick_rate().stop_stepping() {
                return Err(CommandError::new("commands.tick.step.stop.fail", vec![]));
            }
            broadcast_step(global);
            say("commands.tick.step.stop.success", vec![], true);
            Ok(1)
        }
        TickOp::Sprint => {
            let time = args.time("time").copied().ok_or_else(|| missing("time"))?;
            let ticks = u64::try_from(time).unwrap_or(0);
            let interrupted = global.tick_rate().request_sprint(ticks);
            broadcast_state(global);
            if interrupted {
                say("commands.tick.sprint.stop.success", vec![], true);
            }
            say("commands.tick.status.sprinting", vec![], true);
            Ok(1)
        }
        TickOp::SprintStop => {
            let report = global.tick_rate().stop_sprinting();
            let Some(report) = report else {
                return Err(CommandError::new("commands.tick.sprint.stop.fail", vec![]));
            };
            sprint_finished(global, &report);
            say("commands.tick.sprint.stop.success", vec![], true);
            Ok(1)
        }
        TickOp::Freeze | TickOp::Unfreeze => {
            let freeze = op == TickOp::Freeze;
            if freeze {
                let report = {
                    let mut r = global.tick_rate();
                    if r.is_sprinting() {
                        r.stop_sprinting()
                    } else {
                        None
                    }
                };
                if let Some(report) = report {
                    sprint_finished(global, &report);
                }
                let stopped = {
                    let mut r = global.tick_rate();
                    r.is_stepping() && r.stop_stepping()
                };
                if stopped {
                    broadcast_step(global);
                }
            }
            global.tick_rate().set_frozen(freeze);
            broadcast_state(global);
            let key = if freeze {
                "commands.tick.status.frozen"
            } else {
                "commands.tick.status.running"
            };
            say(key, vec![], true);
            Ok(i32::from(freeze))
        }
    }
}

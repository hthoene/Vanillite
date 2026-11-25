//! The entity commands: `summon`, `ride`, `rotate`, `damage`, `swing` (Vanilla's
//! `SummonCommand`, `RideCommand`, `RotateCommand`, `DamageCommand`, `SwingCommand`), and
//! `effect` and `bossbar`, whose syntax is complete but whose models do not exist yet.

use std::sync::Arc;

use vanillite_base::Vec3;
use vanillite_command::Args;
use vanillite_command::coords::Coordinates;
use vanillite_command::error::{Arg as ErrArg, CommandError};
use vanillite_command::selector::EntitySelector;
use vanillite_command::values::Anchor;
use vanillite_game::commands::CommandId;
use vanillite_game::entities::Entities;
use vanillite_game::survival::damage::DamageSource;
use vanillite_nbt::Compound;
use vanillite_protocol::SharedPacket;
use vanillite_protocol::packets::play::clientbound::{
    LookAtEntity, PlayerLookAt, PlayerRotation, SwingAnimation,
};
use vanillite_protocol::packets::play::serverbound::Hand;
use vanillite_protocol::text::{Arg, Component};
use vanillite_world::entity::{EntityId, Rotation};
use vanillite_world::tracking::Viewer;

use super::Env;
use super::exec::{resolve_position, resolve_rotation};
use super::response::Tracker;
use super::select::{self, Target};
use super::source::CommandSource;
use crate::global::Global;
use crate::server::DIMENSIONS;
use crate::survival::WorldSurvival;

fn missing(name: &str) -> CommandError {
    CommandError::new(
        "command.exception",
        vec![ErrArg::Str(format!("missing argument {name}"))],
    )
}

fn selector<'a>(args: &'a Args, name: &str) -> Result<&'a EntitySelector, CommandError> {
    args.selector(name).ok_or_else(|| missing(name))
}

fn component(c: Component) -> ErrArg {
    ErrArg::Component(Box::new(c))
}

/// `getEntityOrException`: the source's own entity.
fn own_entity(source: &CommandSource, env: &Env<'_>) -> Result<Target, CommandError> {
    super::dispatch::self_target(env, source)
}

/// Runs `f` on a target's entity: at once when it is in this world, else on its world's
/// thread without waiting (Vanilla acts on entities of every world).
fn on_entity(
    env: &mut Env<'_>,
    target: &Target,
    f: impl FnOnce(&mut Entities, EntityId, &Global, &str) + Send + 'static,
) -> bool {
    let dimension = DIMENSIONS
        .get(target.world.index())
        .copied()
        .unwrap_or("minecraft:overworld");
    if let Some(id) = target.entity {
        f(&mut env.level.entities, id, env.global, dimension);
        return true;
    }
    let Some(global) = env.global.arc() else {
        return false;
    };
    let uuid = target.uuid;
    env.global.run_in_world(target.world, move |w| {
        let entities = w.entities_mut();
        if let Some(id) = select::find_uuid(entities, uuid) {
            f(entities, id, &global, dimension);
        }
    });
    true
}

/// `ChunkMap.TrackedEntity.broadcast[AndSend]` without the tracker: the players whose view
/// holds the entity's chunk, within its tracking range (and the entity itself).
fn send_to_viewers(entities: &Entities, id: EntityId, with_self: bool, packet: &Arc<SharedPacket>) {
    let Some(entity) = entities.store.get(id) else {
        return;
    };
    let pos = entity.pos();
    let chunk = vanillite_base::ChunkPos::new(
        vanillite_base::math::floor(pos.x) >> 4,
        vanillite_base::math::floor(pos.z) >> 4,
    );
    let range = f64::from(entity.kind().info().tracking_range.saturating_mul(16));
    for (viewer, player) in entities.players.iter() {
        let sees = if viewer == id {
            with_self
        } else {
            let Some(at) = entities.store.get(viewer).map(|e| e.pos()) else {
                continue;
            };
            let view = player.chunk_view;
            let limit = range.min(f64::from(view.view_distance.saturating_mul(16)));
            let (dx, dz) = (at.x - pos.x, at.z - pos.z);
            Viewer {
                entity: viewer,
                view_center: view.center,
                view_distance: view.view_distance,
            }
            .sees_chunk(chunk)
                && dx * dx + dz * dz <= limit * limit
        };
        if sees {
            player.connection.sink.send_shared(packet);
        }
    }
}

/// Runs an entity command.
pub(crate) fn run(
    id: CommandId,
    args: &Args,
    source: &CommandSource,
    env: &mut Env<'_>,
) -> Result<i32, CommandError> {
    match id {
        CommandId::Summon => summon(args, source, env),
        CommandId::RideMount => {
            let target = select::single(env, source, selector(args, "target")?)?;
            let vehicle = select::single(env, source, selector(args, "vehicle")?)?;
            ride_mount(&target, &vehicle)
        }
        CommandId::RideDismount => {
            let target = select::single(env, source, selector(args, "target")?)?;
            // Nothing rides yet: `getVehicle()` is always null.
            Err(CommandError::new(
                "commands.ride.not_riding",
                vec![component(target.display())],
            ))
        }
        CommandId::Rotate => rotate(args, source, env),
        CommandId::Damage => damage(args, source, env),
        CommandId::Swing { offhand } => swing(offhand, args, source, env),
        CommandId::EffectClear | CommandId::EffectGive { .. } => {
            // The targets resolve like Vanilla's; mob effects do not exist yet.
            match args.selector("targets") {
                Some(s) => drop(select::many(env, source, s)?),
                None => drop(own_entity(source, env)?),
            }
            Err(CommandError::new("command.failed", vec![]))
        }
        CommandId::Bossbar(_) => {
            // Boss bars do not exist yet.
            if let Some(s) = args.selector("targets") {
                select::many(env, source, s)?;
            }
            Err(CommandError::new("command.failed", vec![]))
        }
        _ => Err(CommandError::new("command.unknown.command", vec![])),
    }
}

/// `SummonCommand.spawnEntity`.
fn summon(args: &Args, source: &CommandSource, env: &mut Env<'_>) -> Result<i32, CommandError> {
    let id = args.identifier("entity").ok_or_else(|| missing("entity"))?;
    let pos = args
        .coordinates("pos")
        .map_or(source.pos, |c| resolve_position(source, c));
    let empty = Compound::new();
    let nbt = args.compound("nbt").unwrap_or(&empty);
    let entity = super::execute::summon(env, source, id, pos, nbt)?;
    source.send_success(
        env.global,
        Component::translatable("commands.summon.success", vec![entity.display()]),
        true,
    );
    Ok(1)
}

/// `RideCommand.mount`. Nothing rides yet, so no target already rides and no mount succeeds.
fn ride_mount(target: &Target, vehicle: &Target) -> Result<i32, CommandError> {
    if vehicle.is_player() {
        return Err(CommandError::new(
            "commands.ride.mount.failure.cant_ride_players",
            vec![],
        ));
    }
    if target.uuid == vehicle.uuid {
        return Err(CommandError::new(
            "commands.ride.mount.failure.loop",
            vec![],
        ));
    }
    if target.world != vehicle.world {
        return Err(CommandError::new(
            "commands.ride.mount.failure.wrong_dimension",
            vec![],
        ));
    }
    Err(CommandError::new(
        "commands.ride.mount.failure.generic",
        vec![component(target.display()), component(vehicle.display())],
    ))
}

/// How `rotate` turns an entity.
#[derive(Clone, Copy, Debug)]
enum Turn {
    /// `forceSetRotation` with the rotation from the source, each axis maybe relative.
    Angles {
        y_rot: f32,
        relative_y: bool,
        x_rot: f32,
        relative_x: bool,
    },
    /// `lookAt(anchor, pos)`; players also get `player_look_at` (towards an entity when
    /// given: its network id and anchor).
    Face {
        from: Anchor,
        at: Vec3,
        entity: Option<LookAtEntity>,
    },
}

/// `Mth.wrapDegrees(float)`.
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

fn anchor_id(anchor: Anchor) -> i32 {
    match anchor {
        Anchor::Feet => 0,
        Anchor::Eyes => 1,
    }
}

/// `Entity.forceSetRotation` / `Entity.lookAt`, with `ServerPlayer`'s packets.
#[allow(clippy::cast_possible_truncation, clippy::as_conversions)]
fn turn(entities: &mut Entities, id: EntityId, turn: Turn) {
    let Some(entity) = entities.store.get(id) else {
        return;
    };
    let current = entity.rot();
    let pos = entity.pos();
    let eye_height = f64::from(entity.kind().info().eye_height);
    let (y_rot, x_rot, packet) = match turn {
        Turn::Angles {
            y_rot,
            relative_y,
            x_rot,
            relative_x,
        } => {
            let y = if relative_y {
                y_rot - current.y_rot
            } else {
                y_rot
            };
            let x = if relative_x {
                x_rot - current.x_rot
            } else {
                x_rot
            };
            let abs_y = if relative_y { current.y_rot + y } else { y };
            let abs_x = (if relative_x { current.x_rot + x } else { x }).clamp(-90.0, 90.0);
            let packet = SharedPacket::of(&PlayerRotation {
                y_rot: y,
                relative_y,
                x_rot: x,
                relative_x,
            });
            (abs_y, abs_x, packet)
        }
        Turn::Face { from, at, entity } => {
            let origin = match from {
                Anchor::Eyes => Vec3::new(pos.x, pos.y + eye_height, pos.z),
                Anchor::Feet => pos,
            };
            let d = at - origin;
            let horizontal = (d.x * d.x + d.z * d.z).sqrt();
            let x = wrap_degrees((-(d.y.atan2(horizontal) * 180.0 / std::f64::consts::PI)) as f32);
            let y = wrap_degrees((d.z.atan2(d.x) * 180.0 / std::f64::consts::PI) as f32 - 90.0);
            let packet = SharedPacket::of(&PlayerLookAt {
                from_anchor: anchor_id(from),
                x: at.x,
                y: at.y,
                z: at.z,
                entity,
            });
            (y, x, packet)
        }
    };
    if !(y_rot.is_finite() && x_rot.is_finite()) {
        return;
    }
    if let Some(mut e) = entities.store.get_mut(id) {
        e.set_rot(Rotation {
            y_rot,
            // `setXRot`.
            x_rot: (x_rot % 360.0).clamp(-90.0, 90.0),
            y_head_rot: y_rot,
        });
    }
    if let Some(player) = entities.player(id) {
        player.connection.sink.send_shared(&packet);
    }
}

/// `RotateCommand.rotate`.
fn rotate(args: &Args, source: &CommandSource, env: &mut Env<'_>) -> Result<i32, CommandError> {
    let target = select::single(env, source, selector(args, "target")?)?;
    let how = if let Some(rotation) = args.coordinates("rotation") {
        let (y_rot, x_rot) = resolve_rotation(source, rotation);
        let (relative_x, relative_y) = match rotation {
            Coordinates::World([x, y, _]) => (x.relative, y.relative),
            Coordinates::Local(_) => (false, false),
        };
        Turn::Angles {
            y_rot,
            relative_y,
            x_rot,
            relative_x,
        }
    } else if let Some(location) = args.coordinates("facingLocation") {
        Turn::Face {
            from: source.anchor,
            at: resolve_position(source, location),
            entity: None,
        }
    } else {
        let facing = select::single(env, source, selector(args, "facingEntity")?)?;
        let anchor = args.anchor("facingAnchor").copied().unwrap_or(Anchor::Feet);
        let (pos, _, _) = facing
            .place
            .ok_or_else(|| CommandError::new("argument.entity.notfound.entity", vec![]))?;
        let at = match anchor {
            Anchor::Eyes => Vec3::new(
                pos.x,
                pos.y + f64::from(facing.kind.info().eye_height),
                pos.z,
            ),
            Anchor::Feet => pos,
        };
        let entity = facing
            .entity
            .and_then(|id| env.level.entities.store.get(id))
            .map(|e| LookAtEntity {
                entity_id: e.network_id().0,
                anchor: anchor_id(anchor),
            });
        Turn::Face {
            from: source.anchor,
            at,
            entity,
        }
    };
    on_entity(env, &target, move |entities, id, _, _| {
        turn(entities, id, how)
    });
    source.send_success(
        env.global,
        Component::translatable("commands.rotate.success", vec![target.display()]),
        true,
    );
    Ok(1)
}

/// `DamageCommand.damage`: `hurtServer` of players; other entities have no health yet and
/// take no damage. Players of other worlds are hurt on their world's thread and count as
/// hurt.
fn damage(args: &Args, source: &CommandSource, env: &mut Env<'_>) -> Result<i32, CommandError> {
    let target = select::single(env, source, selector(args, "target")?)?;
    let amount = *args.float("amount").ok_or_else(|| missing("amount"))?;
    let types = &env.global.level.damage_types;
    let kind = match args.identifier("damageType") {
        Some(id) => types.kind(id).ok_or_else(|| {
            CommandError::new(
                "argument.resource.not_found",
                vec![
                    ErrArg::from(id.as_str()),
                    ErrArg::from("minecraft:damage_type"),
                ],
            )
        })?,
        None => types.generic,
    };
    let entity = match args.selector("entity") {
        Some(s) => Some(select::single(env, source, s)?),
        None => None,
    };
    let cause = match args.selector("cause") {
        Some(s) => Some(select::single(env, source, s)?),
        None => None,
    };
    let local = |t: &Option<Target>| t.as_ref().and_then(|t| t.entity);
    let damage_source = DamageSource {
        kind,
        // `new DamageSource(type, direct)` makes the direct entity the cause too.
        cause: local(&cause).or(local(&entity)),
        direct: local(&entity),
        position: args
            .coordinates("location")
            .map(|c| resolve_position(source, c)),
        cause_is_mob: cause
            .as_ref()
            .or(entity.as_ref())
            .is_some_and(|t| vanillite_game::mob::Kind::by_name(t.kind.name()).is_some()),
        cause_name: None,
        disable_shield_seconds: 0.0,
    };
    if !target.is_player() {
        return Err(CommandError::new("commands.damage.invulnerable", vec![]));
    }
    let hurt = match target.entity {
        Some(id) => {
            let dimension = DIMENSIONS
                .get(target.world.index())
                .copied()
                .unwrap_or("minecraft:overworld");
            WorldSurvival::hurt(
                &mut env.level.entities,
                None,
                env.global,
                dimension,
                id,
                damage_source,
                amount,
            )
        }
        None => {
            // Entity ids of this world mean nothing in the target's world.
            let remote = DamageSource {
                cause: None,
                direct: None,
                ..damage_source
            };
            on_entity(env, &target, move |entities, id, global, dimension| {
                WorldSurvival::hurt(entities, None, global, dimension, id, remote, amount);
            })
        }
    };
    if !hurt {
        return Err(CommandError::new("commands.damage.invulnerable", vec![]));
    }
    source.send_success(
        env.global,
        Component::translatable_args(
            "commands.damage.success",
            vec![Arg::Float(amount), Arg::from(target.display())],
        ),
        true,
    );
    Ok(1)
}

/// `SwingCommand.swing`: living entities swing (`LivingEntity.swing(hand, animation, true)`).
fn swing(
    offhand: bool,
    args: &Args,
    source: &CommandSource,
    env: &mut Env<'_>,
) -> Result<i32, CommandError> {
    let targets = match args.selector("targets") {
        Some(s) => select::many(env, source, s)?,
        None => vec![own_entity(source, env)?],
    };
    // `SwingAnimation.DEFAULT`: whack for 6 ticks.
    let kind = match args.string("animation").map(String::as_str) {
        Some("none") => 0,
        Some("stab") => 2,
        _ => 1,
    };
    let duration = args.time("duration").copied().unwrap_or(6);
    let hand = if offhand {
        Hand::OffHand
    } else {
        Hand::MainHand
    };
    let mut tracker = Tracker::new();
    for target in targets {
        // Living entities are the kinds with attributes.
        if target.kind.info().attributes.is_empty() {
            continue;
        }
        on_entity(env, &target, move |entities, id, _, _| {
            let Some(entity_id) = entities.store.get(id).map(|e| e.network_id().0) else {
                return;
            };
            let packet = SharedPacket::of(&SwingAnimation {
                entity_id,
                hand,
                kind,
                duration,
            });
            send_to_viewers(entities, id, true, &packet);
        });
        tracker.track(target, true);
    }
    tracker.feedback(
        source,
        env.global,
        || CommandError::new("commands.swing.failed.notliving", vec![]),
        |entity| Component::translatable("commands.swing.success.single", vec![entity.display()]),
        |count| {
            Component::translatable_args("commands.swing.success.multiple", vec![Arg::Int(count)])
        },
    )
}

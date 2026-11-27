//! The host functions added with ABI 1.7: extra worlds (create, load, unload, list) and moving
//! players between worlds from any thread.

use vanillite_abi::{self as abi, Status, status};

use super::{arg, done, valid_pos, with, write, write_str};
use crate::{Location, WorldCreator};

macro_rules! server {
    ($inner:expr) => {
        match $inner.server() {
            Some(server) => server,
            None => return status::WRONG_PHASE,
        }
    };
}

pub(crate) unsafe extern "C" fn world_create(
    id: abi::PluginId,
    spec: *const abi::WorldCreate,
    out: *mut u32,
) -> Status {
    with(id, |inner, _| {
        // SAFETY: the plugin passes a valid record or null.
        let Some(spec) = (unsafe { spec.as_ref() }) else {
            return status::INVALID_ARGUMENT;
        };
        if (spec.size as usize) < size_of::<abi::WorldCreate>()
            || spec.environment > abi::environment::THE_END
        {
            return status::INVALID_ARGUMENT;
        }
        let (Ok(name), Ok(generator)) = (arg(spec.name), arg(spec.generator)) else {
            return status::INVALID_ARGUMENT;
        };
        let creator = WorldCreator {
            name,
            environment: spec.environment,
            generator,
            seed: (spec.flags & abi::world_create_flag::SEED != 0).then_some(spec.seed),
            structures: spec.flags & abi::world_create_flag::NO_STRUCTURES == 0,
        };
        match server!(inner).create_world(&creator) {
            Ok(world) => write(out, world),
            Err(s) => s,
        }
    })
}

pub(crate) unsafe extern "C" fn world_load(
    id: abi::PluginId,
    name: abi::Str,
    out: *mut u32,
) -> Status {
    with(id, |inner, _| {
        let Ok(name) = arg(name) else {
            return status::INVALID_ARGUMENT;
        };
        match server!(inner).load_world(name) {
            Ok(world) => write(out, world),
            Err(s) => s,
        }
    })
}

pub(crate) unsafe extern "C" fn world_unload(id: abi::PluginId, world: u32, save: u32) -> Status {
    with(id, |inner, _| {
        done(server!(inner).unload_world(world, save != 0))
    })
}

pub(crate) unsafe extern "C" fn world_count(id: abi::PluginId, out: *mut u32) -> Status {
    with(id, |inner, _| write(out, server!(inner).world_count()))
}

pub(crate) unsafe extern "C" fn world_info(
    id: abi::PluginId,
    world: u32,
    out: *mut abi::WorldDetails,
    folder: *mut abi::StrBuf,
) -> Status {
    with(id, |inner, _| {
        let Some(info) = server!(inner).world_info(world) else {
            return status::NOT_FOUND;
        };
        let written = write(
            out,
            abi::WorldDetails {
                size: abi::size_of::<abi::WorldDetails>(),
                environment: info.environment,
                flags: info.flags,
                seed: info.seed,
            },
        );
        if written != status::OK || folder.is_null() {
            return written;
        }
        write_str(folder, &info.folder)
    })
}

pub(crate) unsafe extern "C" fn player_move(
    id: abi::PluginId,
    player: abi::Uuid,
    to: *const abi::Location,
) -> Status {
    with(id, |inner, _| {
        // SAFETY: the plugin passes a valid record or null.
        let Some(to) = (unsafe { to.as_ref() }) else {
            return status::INVALID_ARGUMENT;
        };
        let pos = [to.pos.x, to.pos.y, to.pos.z];
        if !valid_pos(pos) || !to.yaw.is_finite() || !to.pitch.is_finite() {
            return status::INVALID_ARGUMENT;
        }
        done(server!(inner).move_player(
            player.to_u128(),
            Location {
                world: to.world,
                pos,
                yaw: to.yaw,
                pitch: to.pitch,
            },
        ))
    })
}

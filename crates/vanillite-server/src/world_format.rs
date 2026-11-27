//! The world format at startup (#25) and the offline `convert`, `export` and `upgrade-world`
//! commands.
//!
//! New worlds get `storage.world-format` (the own format by default). An existing world keeps
//! the format it was found in, Anvil for every imported Vanilla, Paper or Spigot world, unless
//! `storage.convert-world` converts it before it loads.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use vanillite_chunk::slots::WorldInfo;
use vanillite_config::{Config, RegionFileCompression};
use vanillite_storage::format::{self, ConvertSettings, ConvertStats};
use vanillite_storage::{Compression, WorldFormat, WorldLayout};

use crate::dimensions::Dimensions;
use crate::server::StartError;

/// The configured format of new worlds.
pub(crate) fn configured(config: &Config) -> WorldFormat {
    match config.server.storage.world_format {
        vanillite_config::WorldFormat::Vanillite => WorldFormat::Vanillite,
        vanillite_config::WorldFormat::Anvil => WorldFormat::Anvil,
    }
}

/// The folder layout of the configuration.
pub(crate) fn layout(config: &Config) -> WorldLayout {
    match config.server.storage.layout {
        vanillite_config::StorageLayout::Vanilla => WorldLayout::Vanilla,
        vanillite_config::StorageLayout::Spigot => WorldLayout::Spigot,
    }
}

/// The compression of chunks written to Anvil (`region-file-compression`).
pub(crate) fn anvil_compression(config: &Config) -> Compression {
    match config.properties.region_file_compression {
        RegionFileCompression::Deflate => Compression::Deflate,
        RegionFileCompression::Lz4 => Compression::Lz4,
        RegionFileCompression::None => Compression::None,
    }
}

/// The format a world is opened in, decided before anything is written into it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FormatPlan {
    /// The format the world is in now.
    pub(crate) format: WorldFormat,
    /// The format it is converted into before it loads.
    pub(crate) convert_to: Option<WorldFormat>,
}

/// Detects the format of the world; marks a new world with the configured one.
pub(crate) fn plan(
    config: &Config,
    dir: &Path,
    level_name: &str,
    dimensions: &Dimensions,
) -> Result<FormatPlan, StartError> {
    let level_dir = dir.join(level_name);
    let layout = layout(config);
    let dimension_dirs: Vec<PathBuf> = dimensions
        .names()
        .map(|d| layout.dimension_dir(dir, level_name, d))
        .collect();
    let wanted = configured(config);
    let error = |e: std::io::Error| StartError::Io(format!("world \"{level_name}\": {e}"));
    let backup = convert_settings(config, dir, 1).backup;
    if let format::Interrupted::Pending(to) =
        format::finish_interrupted(dir, &level_dir, &dimension_dirs, backup.as_deref())
            .map_err(error)?
    {
        log::warn!(
            "An interrupted conversion of world \"{level_name}\" to the {} format is \
             unfinished; the world stays in its format until `vanillite convert --to {}` (or \
             storage.convert-world) runs it again",
            to.name(),
            to.name()
        );
    }
    let found = format::detect(&level_dir, &dimension_dirs).map_err(error)?;
    if found == Some(WorldFormat::Vanillite) {
        // An own-format world of an older version: records are upgraded as they are saved.
        format::stamp_current(&level_dir).map_err(error)?;
    }
    let plan = match found {
        None => {
            if wanted == WorldFormat::Vanillite {
                format::mark_own_format(&level_dir).map_err(error)?;
            }
            log::info!("New world \"{level_name}\" in the {} format", wanted.name());
            FormatPlan {
                format: wanted,
                convert_to: None,
            }
        }
        Some(found) if found == wanted => FormatPlan {
            format: found,
            convert_to: None,
        },
        Some(found) if config.server.storage.convert_world => FormatPlan {
            format: found,
            convert_to: Some(wanted),
        },
        Some(found) => {
            log::info!(
                "World \"{level_name}\" is in the {} format and stays so; set \
                 storage.convert-world = true or run `vanillite convert` to convert it to {}",
                found.name(),
                wanted.name()
            );
            FormatPlan {
                format: found,
                convert_to: None,
            }
        }
    };
    if plan.format == WorldFormat::Vanillite {
        let stray: usize = dimension_dirs
            .iter()
            .map(|d| format::count_files(d, WorldFormat::Anvil))
            .sum();
        if stray > 0 {
            log::warn!(
                "World \"{level_name}\" is in the own format but has {stray} Anvil region files \
                 (opened by Vanilla?); they are ignored"
            );
        }
    }
    Ok(plan)
}

/// The conversion settings of the configuration.
pub(crate) fn convert_settings(config: &Config, dir: &Path, threads: usize) -> ConvertSettings {
    let backup = config.server.storage.keep_converted_files.then(|| {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        dir.join("world-format-backup").join(now.to_string())
    });
    ConvertSettings {
        threads,
        anvil_compression: anvil_compression(config),
        level: i32::from(config.server.storage.compression_level),
        backup,
    }
}

/// Converts the world before it loads; on failure it stays in its format, untouched.
pub(crate) fn convert_on_start(
    plan: FormatPlan,
    config: &Config,
    dir: &Path,
    level_name: &str,
    infos: &[WorldInfo],
    threads: usize,
) -> WorldFormat {
    let Some(to) = plan.convert_to else {
        return plan.format;
    };
    let settings = convert_settings(config, dir, threads);
    match format::convert_world(dir, &dir.join(level_name), infos, to, &settings) {
        Ok(_) => to,
        Err(e) => {
            log::error!(
                "Couldn't convert world \"{level_name}\" to the {} format: {e}; it stays in the \
                 {} format",
                to.name(),
                plan.format.name()
            );
            plan.format
        }
    }
}

/// What the offline commands need: the configuration, the locked world and the dimensions.
struct Offline {
    config: Config,
    level_name: String,
    infos: Vec<WorldInfo>,
    _lock: std::fs::File,
}

fn open_offline(dir: &Path) -> Result<Offline, StartError> {
    let config = Config::load(dir)?;
    let level_name = config.properties.level_name.clone();
    let level_dir = dir.join(&level_name);
    if !level_dir.join("level.dat").exists() && !format::marker_path(&level_dir).exists() {
        return Err(StartError::Io(format!(
            "no world in {}",
            level_dir.display()
        )));
    }
    let lock = crate::server::lock_world(&level_dir)?;
    let properties = &config.properties;
    let packs = crate::net::load_world_packs(
        &crate::net::VanillaPack::Download,
        dir,
        &level_dir,
        (
            &properties.initial_enabled_packs,
            &properties.initial_disabled_packs,
        ),
    )?;
    crate::world_setup::install_facts(packs.as_ref())?;
    // The dimensions of the data packs and `world_gen_settings.dat`, like a start.
    let dimensions = crate::world_setup::WorldGen::load(&level_dir)?
        .unwrap_or_else(crate::world_setup::WorldGen::empty)
        .bake(packs.as_ref())
        .dimensions();
    let layout = layout(&config);
    let dimension_dirs: Vec<PathBuf> = dimensions
        .names()
        .map(|d| layout.dimension_dir(dir, &level_name, d))
        .collect();
    let backup = convert_settings(&config, dir, 1).backup;
    format::finish_interrupted(dir, &level_dir, &dimension_dirs, backup.as_deref())
        .map_err(|e| StartError::Io(format!("world \"{level_name}\": {e}")))?;
    let infos = dimensions
        .iter()
        .map(|(_, d, kind)| {
            crate::world_setup::world_info(
                packs.as_ref(),
                layout.dimension_dir(dir, &level_name, d),
                d,
                kind,
                0,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Offline {
        config,
        level_name,
        infos,
        _lock: lock,
    })
}

fn threads() -> usize {
    std::thread::available_parallelism().map_or(1, usize::from)
}

/// `vanillite convert`: converts the world of the server folder `dir` into `to` (default:
/// `storage.world-format`). The server must not run.
pub fn convert_world(dir: &Path, to: Option<WorldFormat>) -> Result<ConvertStats, StartError> {
    let offline = open_offline(dir)?;
    let level_dir = dir.join(&offline.level_name);
    let to = to.unwrap_or_else(|| configured(&offline.config));
    let dirs: Vec<PathBuf> = offline.infos.iter().map(|i| i.dir.clone()).collect();
    let error =
        |e: std::io::Error| StartError::Io(format!("world \"{}\": {e}", offline.level_name));
    if format::detect(&level_dir, &dirs).map_err(error)? == Some(to) {
        return Err(StartError::Io(format!(
            "world \"{}\" is in the {} format already",
            offline.level_name,
            to.name()
        )));
    }
    let settings = convert_settings(&offline.config, dir, threads());
    format::convert_world(dir, &level_dir, &offline.infos, to, &settings).map_err(error)
}

/// `vanillite export <target> [--to <format>]`: writes a copy of the world in `to` (default
/// Anvil, Vanilla's format) into `target`. The server must not run.
pub fn export_world(
    dir: &Path,
    target: &Path,
    to: WorldFormat,
) -> Result<ConvertStats, StartError> {
    let offline = open_offline(dir)?;
    let level_dir = dir.join(&offline.level_name);
    let dirs: Vec<PathBuf> = offline.infos.iter().map(|i| i.dir.clone()).collect();
    let error =
        |e: std::io::Error| StartError::Io(format!("world \"{}\": {e}", offline.level_name));
    if format::detect(&level_dir, &dirs).map_err(error)? == Some(to) {
        return Err(StartError::Io(format!(
            "world \"{}\" is in the {} format already; copy its folder",
            offline.level_name,
            to.name()
        )));
    }
    let mut levels = vec![level_dir];
    for info in &offline.infos {
        // Spigot's layout keeps the nether and the end in their own level folders.
        if let Some(level) = info
            .dir
            .strip_prefix(dir)
            .ok()
            .and_then(|r| r.components().next())
            .map(|c| dir.join(c))
            && !levels.contains(&level)
        {
            levels.push(level);
        }
    }
    let mut settings = convert_settings(&offline.config, dir, threads());
    settings.anvil_compression = Compression::Deflate;
    settings.backup = None;
    format::export_world(dir, &levels, &offline.infos, target, to, &settings).map_err(error)
}

/// `vanillite upgrade-world`: rewrites every chunk of an own-format world written by an older
/// Minecraft version in this server's version. The server must not run.
pub fn upgrade_world(dir: &Path) -> Result<ConvertStats, StartError> {
    let offline = open_offline(dir)?;
    let level_dir = dir.join(&offline.level_name);
    let dirs: Vec<PathBuf> = offline.infos.iter().map(|i| i.dir.clone()).collect();
    let error =
        |e: std::io::Error| StartError::Io(format!("world \"{}\": {e}", offline.level_name));
    if format::detect(&level_dir, &dirs).map_err(error)? != Some(WorldFormat::Vanillite) {
        return Err(StartError::Io(format!(
            "world \"{}\" is an Anvil world: it is read by name and needs no upgrade",
            offline.level_name
        )));
    }
    let settings = convert_settings(&offline.config, dir, threads());
    format::upgrade_world(dir, &level_dir, &offline.infos, &settings).map_err(error)
}

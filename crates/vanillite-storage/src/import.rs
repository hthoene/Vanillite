//! Import of world layouts from before 26.1 and of Spigot/Paper's split worlds.
//!
//! 26.x keeps every dimension in `<world>/dimensions/<namespace>/<path>/` and players in
//! `<world>/players/`. Older Vanilla worlds keep the overworld at the root (`region/`,
//! `entities/`, `poi/`, `data/`), the nether in `DIM-1/` and the end in `DIM1/`; Spigot and Paper
//! before 26.1 put the nether into `<world>_nether/DIM-1/` and the end into
//! `<world>_the_end/DIM1/`. Import only moves folders; chunk data is read as it is (D7).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Moves older Vanilla and Spigot layouts of the world `level_name` in `server_dir` into the
/// 26.x layout. Returns one line per moved folder; folders whose target already exists are left
/// in place and reported.
pub fn import_legacy_layout(server_dir: &Path, level_name: &str) -> io::Result<Vec<String>> {
    let world = server_dir.join(level_name);
    let dims = |path: &str| world.join("dimensions").join("minecraft").join(path);
    let parts = ["region", "entities", "poi", "data"];
    let mut moves: Vec<(PathBuf, PathBuf)> = Vec::new();
    for part in parts {
        moves.push((world.join(part), dims("overworld").join(part)));
        moves.push((
            world.join("DIM-1").join(part),
            dims("the_nether").join(part),
        ));
        moves.push((world.join("DIM1").join(part), dims("the_end").join(part)));
        moves.push((
            server_dir
                .join(format!("{level_name}_nether"))
                .join("DIM-1")
                .join(part),
            dims("the_nether").join(part),
        ));
        moves.push((
            server_dir
                .join(format!("{level_name}_the_end"))
                .join("DIM1")
                .join(part),
            dims("the_end").join(part),
        ));
    }
    for (old, new) in [
        ("playerdata", "data"),
        ("advancements", "advancements"),
        ("stats", "stats"),
    ] {
        moves.push((world.join(old), world.join("players").join(new)));
    }
    let mut report = Vec::new();
    for (from, to) in moves {
        // The overworld's `data/` stays where it is in 26.x too (world-wide saved data).
        if from == world.join("data") {
            continue;
        }
        if !from.is_dir() {
            continue;
        }
        if to.exists() {
            report.push(format!("kept {} ({} exists)", from.display(), to.display()));
            continue;
        }
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(&from, &to)?;
        report.push(format!("moved {} to {}", from.display(), to.display()));
    }
    Ok(report)
}

//! Session recordings of a server (`debug.session-recording`, #152) as parity scenarios.
//!
//! A recording (format in `vanillite-server/src/session_recording.rs`) becomes a scenario on
//! its world snapshot (`world.snapshot`): every recorded player is a client player that joins
//! at its tick with its recorded state and sends its recorded packets; a player that left
//! stays where it was and sends nothing more. The observed region is the box around the
//! players' ways (their positions and moves) with the reach of a player around it. Ticks
//! count from 1 at the snapshot (recording tick + 1); the scenario ends
//! [`TAIL`] ticks after the last packet, or after `max_ticks`.

use std::path::Path;

use serde_json::{Value, json};

/// Ticks a replay runs on after the last packet.
pub const TAIL: u32 = 20;
/// Blocks around the players' ways that the region observes horizontally (reach and more).
const MARGIN_XZ: i32 = 6;
/// Blocks below and above the players' feet that the region observes.
const MARGIN_DOWN: i32 = 4;
const MARGIN_UP: i32 = 6;
/// Overworld build limits.
const MIN_Y: i32 = -64;
const MAX_Y: i32 = 319;
/// Region size above which a replay gets slow (blocks per frame).
pub const LARGE_REGION: i64 = 2_000_000;

/// Packets whose body starts with the position of the player or its vehicle (three doubles).
const MOVES: &[&str] = &["move_player_pos", "move_player_pos_rot", "move_vehicle"];

/// The scenario of the recording in `dir`, at most `max_ticks` long.
pub fn scenario(dir: &Path, max_ticks: Option<u32>) -> Result<Value, String> {
    let read = |name: &str| {
        std::fs::read_to_string(dir.join(name)).map_err(|e| format!("{}: {e}", dir.display()))
    };
    let meta: Value = serde_json::from_str(&read("meta.json")?)
        .map_err(|e| format!("{}/meta.json: {e}", dir.display()))?;
    let world = dir.join("world");
    let world = std::fs::canonicalize(&world).map_err(|e| format!("{}: {e}", world.display()))?;
    let mut entities = Vec::new();
    let mut actions = Vec::new();
    let mut last = 0u32;
    let (mut min, mut max) = ([i32::MAX; 3], [i32::MIN; 3]);
    let mut include = |p: [f64; 3]| {
        if p.iter().all(|c| c.is_finite() && c.abs() < 3.0e7) {
            for i in 0..3 {
                let c = p[i].floor() as i32;
                min[i] = min[i].min(c);
                max[i] = max[i].max(c);
            }
        }
    };
    for (n, line) in read("packets.jsonl")?.lines().enumerate() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            // The last line of a session that is still running may be incomplete.
            continue;
        };
        let bad = || format!("packets.jsonl line {}: {line}", n + 1);
        let tick = entry["tick"]
            .as_u64()
            .and_then(|t| u32::try_from(t + 1).ok())
            .ok_or_else(bad)?;
        if max_ticks.is_some_and(|m| tick > m) {
            break;
        }
        if let Some(name) = entry["join"].as_str() {
            let pos = vec3(&entry["pos"]).ok_or_else(bad)?;
            include(pos);
            let mut spec = json!({
                "name": name,
                "type": "minecraft:player",
                "client": true,
                "pos": entry["pos"],
                "rot": entry["rot"],
                "game_mode": entry["game_mode"],
                "food": entry["food"],
                "inventory": entry["inventory"],
            });
            if tick > 1 {
                spec["join"] = json!(tick);
            }
            entities.push(spec);
        } else if let Some(player) = entry["player"].as_str() {
            let packet = entry["packet"].as_str().ok_or_else(bad)?;
            let data = entry["data"].as_str().ok_or_else(bad)?;
            if MOVES.contains(&packet)
                && let Some(p) = leading_position(data)
            {
                include(p);
            }
            let mut action = json!({
                "tick": tick,
                "op": "packet",
                "entity": player,
                "packet": packet,
                "data": data,
            });
            if let Some(t) = entry["target"].as_str() {
                action["target"] = json!(t);
            }
            actions.push(action);
            last = last.max(tick);
        }
    }
    if entities.is_empty() {
        return Err(format!("{}: no player joined", dir.display()));
    }
    let ticks = last
        .saturating_add(TAIL)
        .min(max_ticks.unwrap_or(u32::MAX))
        .max(1);
    let region = json!([
        [
            min[0] - MARGIN_XZ,
            (min[1] - MARGIN_DOWN).clamp(MIN_Y, MAX_Y),
            min[2] - MARGIN_XZ
        ],
        [
            max[0] + MARGIN_XZ,
            (max[1] + MARGIN_UP).clamp(MIN_Y, MAX_Y),
            max[2] + MARGIN_XZ
        ],
    ]);
    let name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("session");
    Ok(json!({
        "description": format!("Session replay of the recording {name} (#152)."),
        "world": {
            "seed": meta["seed"].as_i64().unwrap_or(0),
            "snapshot": world.to_string_lossy(),
        },
        // The day count matters for the moon phase only (8 days).
        "time": meta["day_time"].as_i64().unwrap_or(0).rem_euclid(24_000 * 8),
        "weather": meta["weather"].as_str().unwrap_or("clear"),
        "difficulty": meta["difficulty"].as_str().unwrap_or("normal"),
        "region": region,
        "ticks": ticks,
        "client_tick_end": false,
        "entities": entities,
        "actions": actions,
    }))
}

/// The number of blocks of a scenario's region.
pub fn region_volume(scenario: &Value) -> i64 {
    let c = |i: usize, j: usize| scenario["region"][i][j].as_i64().unwrap_or(0);
    (0..3).map(|j| c(1, j) - c(0, j) + 1).product()
}

fn vec3(v: &Value) -> Option<[f64; 3]> {
    Some([v[0].as_f64()?, v[1].as_f64()?, v[2].as_f64()?])
}

/// The three big-endian doubles a move packet's body starts with.
fn leading_position(hex: &str) -> Option<[f64; 3]> {
    let byte = |i: usize| u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok();
    let mut out = [0.0; 3];
    for (k, c) in out.iter_mut().enumerate() {
        let mut bytes = [0u8; 8];
        for (j, b) in bytes.iter_mut().enumerate() {
            *b = byte(k * 8 + j)?;
        }
        *c = f64::from_be_bytes(bytes);
    }
    Some(out)
}

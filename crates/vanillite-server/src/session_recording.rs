//! Session recording for parity replays (`debug.session-recording`, #152).
//!
//! A session runs from the first player that joins the overworld until the last one has left
//! (test bots do not start or keep a session, but are recorded while one runs). At its start
//! the world is saved and the overworld's chunk, entity and POI files are copied; then every
//! tick the serverbound play packets each player's connection handled are written, in the
//! format of the parity scenarios' `packet` action. `cargo xtask parity replay` turns a session
//! into a scenario on the copied world and compares Vanilla and Vanillite tick by tick.
//!
//! `<dir>/<start time>/`:
//! - `world/`: `region/`, `entities/` and `poi/` of the overworld at the start (Anvil);
//! - `meta.json`: `format`, `seed`, `day_time`, `game_time`, `weather`, `difficulty`;
//! - `packets.jsonl`, one entry per line, `tick` counting from 0 at the snapshot:
//!   - `{"tick":0,"join":"p0","pos":[x,y,z],"rot":[yaw,pitch],"game_mode":"survival",
//!     "food":20,"inventory":[[slot,"minecraft:stone",64],...]}` (Vanilla `Inventory` slots;
//!     item components are not kept);
//!   - `{"tick":3,"player":"p0","packet":"use_item_on","data":"<hex body>"}`, with
//!     `"target"` for packets that name an entity (`attack`, `interact`, `player_command`,
//!     `pick_item_from_entity`, `spectator_action`): the anonymous id of a recorded player,
//!     `uuid:<hex>` of another entity or `none`; their entity id (or UUID) in `data` is zeroed;
//!   - `{"tick":9,"leave":"p0"}` (left the server or the overworld).
//!
//! Players get anonymous ids in join order (`p0`, `p1`, ..., a new one per join); names,
//! addresses and UUIDs are not written. Left out: chat and commands, signs, books, plugin
//! messages, cookies, and what the replay harness plays itself (keep-alives, pongs, chunk
//! batch acknowledgements, teleport confirmations, `player_loaded`). `client_tick_end` stays: two
//! moves in one server tick are valid only with the client's tick end between them.

use std::fmt::Write as _;
use std::io::{BufWriter, Write as _};
use std::path::{Path, PathBuf};

use vanillite_game::entities::Entities;
use vanillite_game::player::PlayerData;
use vanillite_world::entity::EntityId;

/// What a session snapshot records of the world besides its chunks.
pub(crate) struct SessionStart {
    pub(crate) seed: i64,
    pub(crate) day_time: i64,
    pub(crate) game_time: i64,
    pub(crate) weather: &'static str,
    pub(crate) difficulty: &'static str,
}

/// The session recording of one world.
#[derive(Default)]
pub(crate) struct SessionRecording {
    /// Folder of the sessions; `None` records nothing.
    dir: Option<PathBuf>,
    session: Option<Session>,
}

struct Session {
    dir: PathBuf,
    out: BufWriter<std::fs::File>,
    /// Ticks since the snapshot.
    tick: u64,
    /// Recorded players: entity, network id, anonymous id.
    players: Vec<(EntityId, i32, String)>,
    joins: u32,
}

/// Packets the replay leaves out: private text, plugin data, and the client's part of the
/// protocol the harness plays itself.
const LEFT_OUT: &[&str] = &[
    "chat",
    "chat_ack",
    "chat_command",
    "chat_command_signed",
    "chat_session_update",
    "command_suggestion",
    "custom_payload",
    "custom_click_action",
    "cookie_response",
    "sign_update",
    "edit_book",
    "teleport_to_entity",
    "block_entity_tag_query",
    "entity_tag_query",
    "keep_alive",
    "pong",
    "ping_request",
    "chunk_batch_received",
    "accept_teleportation",
    "player_loaded",
    "configuration_acknowledged",
    "resource_pack",
    "seen_advancements",
    "unhandled",
];

/// Packets whose body starts with the VarInt id of an entity.
const ENTITY_TARGET: &[&str] = &[
    "attack",
    "interact",
    "player_command",
    "pick_item_from_entity",
];

/// Whether a player keeps a session running (test bots do not).
fn counts(player: &PlayerData) -> bool {
    !player.connection.chunks.is_memory_connection()
}

impl SessionRecording {
    /// Records sessions into `dir`, or nothing.
    pub(crate) fn new(dir: Option<PathBuf>) -> SessionRecording {
        SessionRecording { dir, session: None }
    }

    /// Whether a session starts now: recording is on, none runs and a player is in the world.
    pub(crate) fn wants_start(&self, entities: &Entities) -> bool {
        self.dir.is_some()
            && self.session.is_none()
            && entities.players.iter().any(|(_, p)| counts(p))
    }

    /// Starts a session on the saved world in `world_dir` (the caller saved it with flush):
    /// copies its chunk files and writes the metadata. A failure turns recording off.
    pub(crate) fn start(&mut self, world_dir: &Path, start: &SessionStart) {
        let Some(root) = self.dir.clone() else {
            return;
        };
        let started = std::time::Instant::now();
        match Session::create(&root, world_dir, start) {
            Ok(session) => {
                log::info!(
                    "Session recording started in {} (world copied in {:.1} s)",
                    session.dir.display(),
                    started.elapsed().as_secs_f64()
                );
                self.session = Some(session);
            }
            Err(e) => {
                log::error!("Session recording is off: {e}");
                self.dir = None;
            }
        }
    }

    /// Before the packets of a tick: the players that joined or left since the last tick.
    pub(crate) fn begin_tick(&mut self, entities: &mut Entities) {
        let Some(session) = &mut self.session else {
            return;
        };
        session.leaves(entities);
        let Entities { store, players, .. } = entities;
        for (id, player) in players.iter_mut() {
            if session.players.iter().any(|p| p.0 == id) {
                continue;
            }
            let Some(entity) = store.get(id) else {
                continue;
            };
            let anonymous = format!("p{}", session.joins);
            session.joins += 1;
            let (pos, rot) = (entity.pos(), entity.rot());
            let mut inventory = String::new();
            for slot in 0..=vanillite_game::inventory::OFFHAND {
                let stack = player.inventory.get(slot);
                if !stack.is_empty() {
                    let item = vanillite_data::item::ItemId(stack.item).name();
                    let _ = write!(
                        inventory,
                        "{}[{slot},\"{item}\",{}]",
                        if inventory.is_empty() { "" } else { "," },
                        stack.count
                    );
                }
            }
            let line = format!(
                "{{\"tick\":{},\"join\":\"{anonymous}\",\"pos\":[{},{},{}],\"rot\":[{},{}],\
                 \"game_mode\":\"{}\",\"food\":{},\"inventory\":[{inventory}]}}",
                session.tick,
                json_f64(pos.x),
                json_f64(pos.y),
                json_f64(pos.z),
                json_f64(f64::from(rot.y_rot)),
                json_f64(f64::from(rot.x_rot)),
                player.game_mode.name(),
                player.survival.food.food_level,
            );
            session.write(&line);
            session.players.push((id, entity.network_id().0, anonymous));
            player.connection.recorded = Some(Vec::new());
        }
    }

    /// The packets `player` handled this tick.
    pub(crate) fn packets(&mut self, id: EntityId, player: &mut PlayerData, entities: &Entities) {
        let Some(session) = &mut self.session else {
            return;
        };
        let Some(packets) = player.connection.recorded.as_mut() else {
            return;
        };
        let Some(anonymous) = session
            .players
            .iter()
            .find(|p| p.0 == id)
            .map(|p| p.2.clone())
        else {
            return;
        };
        for packet in packets.drain(..) {
            let name = packet.name();
            let name = name.strip_prefix("minecraft:").unwrap_or(name);
            if LEFT_OUT.contains(&name) {
                continue;
            }
            let bytes = packet.encode_body();
            let mut r = vanillite_protocol::codec::Reader::new(&bytes);
            if r.varint().is_err() {
                continue;
            }
            let mut body = r.take_rest().to_vec();
            let mut target = None;
            if ENTITY_TARGET.contains(&name) {
                let mut r = vanillite_protocol::codec::Reader::new(&body);
                let Ok(network_id) = r.varint() else {
                    continue;
                };
                let rest = r.take_rest().to_vec();
                target = Some(session.reference_by_network_id(network_id, entities));
                body = std::iter::once(0).chain(rest).collect();
            } else if name == "spectator_action" {
                let Some(uuid) = body.get(..16).and_then(|b| <[u8; 16]>::try_from(b).ok()) else {
                    continue;
                };
                target = Some(session.reference_by_uuid(u128::from_be_bytes(uuid), entities));
                body = vec![0; 16];
            }
            let mut line = format!(
                "{{\"tick\":{},\"player\":\"{anonymous}\",\"packet\":\"{name}\",\"data\":\"",
                session.tick
            );
            for b in &body {
                let _ = write!(line, "{b:02x}");
            }
            line.push('"');
            if let Some(t) = target {
                let _ = write!(line, ",\"target\":\"{t}\"");
            }
            line.push('}');
            session.write(&line);
        }
    }

    /// After the tick's packets: the players that left; the session ends without players.
    pub(crate) fn end_tick(&mut self, entities: &Entities) {
        let Some(session) = &mut self.session else {
            return;
        };
        session.leaves(entities);
        session.tick += 1;
        let running = entities.players.iter().any(|(_, p)| counts(p));
        if !running {
            let _ = session.out.flush();
            log::info!(
                "Session recording ended after {} ticks: {}",
                session.tick,
                session.dir.display()
            );
            self.session = None;
        } else if session.tick.is_multiple_of(200) {
            let _ = session.out.flush();
        }
    }
}

impl Session {
    fn create(root: &Path, world_dir: &Path, start: &SessionStart) -> Result<Session, String> {
        // A new world has no region files before its first chunks are saved.
        let own_format = std::fs::read_dir(world_dir.join("region"))
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .any(|e| e.path().extension().is_some_and(|x| x == "vwr"));
        if own_format {
            return Err(
                "the world is in the own format; session replays need Anvil \
                        (`storage.world-format = \"anvil\"`)"
                    .to_owned(),
            );
        }
        let dir = root.join(utc_name(std::time::SystemTime::now()));
        let world = dir.join("world");
        for folder in ["region", "entities", "poi"] {
            copy_dir(&world_dir.join(folder), &world.join(folder))
                .map_err(|e| format!("copying the world into {}: {e}", world.display()))?;
        }
        let meta = format!(
            "{{\"format\":1,\"seed\":{},\"day_time\":{},\"game_time\":{},\"weather\":\"{}\",\
             \"difficulty\":\"{}\"}}\n",
            start.seed, start.day_time, start.game_time, start.weather, start.difficulty
        );
        std::fs::write(dir.join("meta.json"), meta).map_err(|e| e.to_string())?;
        let file = std::fs::File::create(dir.join("packets.jsonl")).map_err(|e| e.to_string())?;
        Ok(Session {
            dir,
            out: BufWriter::new(file),
            tick: 0,
            players: Vec::new(),
            joins: 0,
        })
    }

    fn write(&mut self, line: &str) {
        let _ = self
            .out
            .write_all(line.as_bytes())
            .and_then(|()| self.out.write_all(b"\n"));
    }

    /// Writes the leaves of recorded players that are no longer in the world.
    fn leaves(&mut self, entities: &Entities) {
        let tick = self.tick;
        let mut gone = Vec::new();
        self.players.retain(|(id, _, anonymous)| {
            let here = entities.players.iter().any(|(p, _)| p == *id);
            if !here {
                gone.push(format!("{{\"tick\":{tick},\"leave\":\"{anonymous}\"}}"));
            }
            here
        });
        for line in gone {
            self.write(&line);
        }
    }

    /// The stable reference of the entity with `network_id`.
    fn reference_by_network_id(&self, network_id: i32, entities: &Entities) -> String {
        if let Some(p) = self.players.iter().find(|p| p.1 == network_id) {
            return p.2.clone();
        }
        entities
            .by_network_id(network_id)
            .and_then(|id| entities.store.get(id))
            .map_or_else(|| "none".to_owned(), |e| uuid_reference(e.uuid()))
    }

    /// The stable reference of the entity with `uuid`.
    fn reference_by_uuid(&self, uuid: u128, entities: &Entities) -> String {
        for (id, _, anonymous) in &self.players {
            if entities.store.get(*id).is_some_and(|e| e.uuid() == uuid) {
                return anonymous.clone();
            }
        }
        if entities.player_by_uuid(uuid).is_some() {
            // A player that is not recorded (another world): no UUID of a player is written.
            return "none".to_owned();
        }
        uuid_reference(uuid)
    }
}

fn uuid_reference(uuid: u128) -> String {
    format!("uuid:{uuid:032x}")
}

/// A float as JSON (non-finite values as 0).
fn json_f64(v: f64) -> String {
    if v.is_finite() {
        format!("{v:?}")
    } else {
        "0.0".to_owned()
    }
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    let entries = match std::fs::read_dir(from) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            std::fs::copy(entry.path(), to.join(entry.file_name()))?;
        }
    }
    Ok(())
}

/// `YYYY-MM-DD_HH-MM-SS` in UTC.
fn utc_name(time: std::time::SystemTime) -> String {
    let secs = time
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rest = secs % 86_400;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}_{:02}-{:02}-{:02}",
        rest / 3600,
        rest / 60 % 60,
        rest % 60
    )
}

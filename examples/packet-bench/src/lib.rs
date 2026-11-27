//! Measurement plugin for load tests (`cargo xtask loadtest --plugin examples/packet-bench`):
//! observing packet listeners on the hottest clientbound packets, the entity movement the
//! tracker sends every tick, per receiver or per broadcast (`mode` in `config.toml`). The
//! numbers of calls and bytes seen are logged when the server stops.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use vanillite_plugin::packet::PacketFilter;
use vanillite_plugin::prelude::*;

/// What the entity tracker sends for moving entities.
const MOVEMENT: [&str; 5] = [
    "move_entity_pos",
    "move_entity_pos_rot",
    "move_entity_rot",
    "rotate_head",
    "set_entity_motion",
];

/// A packet type that is never in the tracker's tick batches.
const UNRELATED: [&str; 1] = ["set_player_team"];

struct PacketBench {
    calls: Arc<AtomicU64>,
    bytes: Arc<AtomicU64>,
}

impl Plugin for PacketBench {
    fn load(registrar: &mut Registrar<Self>) -> Result<Self, Error> {
        registrar.config(include_str!("../config.toml"))?;
        let mode = registrar.server().config_string("mode").unwrap_or_default();
        let calls = Arc::new(AtomicU64::new(0));
        let bytes = Arc::new(AtomicU64::new(0));
        let names: &[&str] = match mode.as_str() {
            "broadcast" | "receiver" | "rewrite" => &MOVEMENT,
            "unrelated" => &UNRELATED,
            _ => &[],
        };
        for name in names {
            let filter = PacketFilter::clientbound(name);
            let filter = if mode == "broadcast" {
                filter.per_broadcast()
            } else {
                filter
            };
            let (calls, bytes) = (Arc::clone(&calls), Arc::clone(&bytes));
            let rewrite = mode == "rewrite";
            registrar.on_packet(filter, move |event| {
                calls.fetch_add(1, Ordering::Relaxed);
                bytes.fetch_add(event.body().len() as u64, Ordering::Relaxed);
                if rewrite {
                    let body = event.body().to_vec();
                    let _ = event.replace(body);
                }
            });
        }
        vanillite_plugin::info!("packet bench: mode {mode}");
        Ok(PacketBench { calls, bytes })
    }

    fn disable(&mut self, _ctx: &mut Ctx<'_>) {
        vanillite_plugin::info!(
            "packet bench: {} listener calls, {} bytes seen",
            self.calls.load(Ordering::Relaxed),
            self.bytes.load(Ordering::Relaxed)
        );
    }
}

vanillite_plugin::export_plugin!(PacketBench);

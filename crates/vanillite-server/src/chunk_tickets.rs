//! The saved chunk tickets of a dimension (`TicketStorage`, saved data
//! `minecraft:chunk_tickets`): the `forced` tickets of `/forceload` come back as forced chunks;
//! tickets of other types are kept as they were read and written back.

use vanillite_base::ChunkPos;
use vanillite_game::level::Level;
use vanillite_game::level::forced::FORCED_TICKET_LEVEL;
use vanillite_nbt::{Compound, List, Tag};
use vanillite_storage::saved_data::SavedDataStore;

/// The id of the saved data (`TicketStorage.TYPE`).
pub(crate) const SAVED_DATA: &str = "minecraft:chunk_tickets";

const FORCED: &str = "minecraft:forced";

fn chunk_pos(entry: &Compound) -> Option<ChunkPos> {
    match entry.get("chunk_pos") {
        Some(Tag::IntArray(v)) => match v.as_slice() {
            [x, z] => Some(ChunkPos::new(*x, *z)),
            _ => None,
        },
        _ => None,
    }
}

/// Loads the forced tickets into `level`; the other tickets are returned to be kept.
pub(crate) fn load(data: &SavedDataStore, level: &mut Level) -> Vec<Tag> {
    let compound = match data.read(SAVED_DATA) {
        Ok(compound) => compound.unwrap_or_default(),
        Err(e) => {
            log::error!("Cannot read {SAVED_DATA}: {e}");
            return Vec::new();
        }
    };
    let Some(Tag::List(tickets)) = compound.get("tickets") else {
        return Vec::new();
    };
    let mut kept = Vec::new();
    for ticket in &tickets.0 {
        let Tag::Compound(entry) = ticket else {
            continue;
        };
        let forced = matches!(entry.get("type"), Some(Tag::String(t)) if t == FORCED);
        match chunk_pos(entry) {
            Some(pos) if forced => {
                let Level { chunks, forced, .. } = level;
                forced.set(chunks, pos, true);
            }
            Some(_) => kept.push(ticket.clone()),
            None => {}
        }
    }
    // Loading is not a change.
    level.forced.take_dirty();
    kept
}

/// The saved compound: the forced chunks and the kept tickets.
pub(crate) fn save(level: &Level, kept: &[Tag]) -> Compound {
    let mut tickets: Vec<Tag> = level
        .forced
        .sorted()
        .into_iter()
        .map(|pos| {
            Tag::Compound(
                Compound::with_capacity(3)
                    .with("chunk_pos", vec![pos.x, pos.z])
                    .with("type", FORCED)
                    .with("level", i32::from(FORCED_TICKET_LEVEL)),
            )
        })
        .collect();
    tickets.extend(kept.iter().cloned());
    Compound::with_capacity(1).with("tickets", List(tickets))
}

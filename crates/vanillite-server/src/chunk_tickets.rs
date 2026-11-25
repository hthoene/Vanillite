//! The saved chunk tickets of a dimension (`TicketStorage`, saved data
//! `minecraft:chunk_tickets`): the `forced` tickets of `/forceload` come back as forced chunks,
//! `portal` tickets with their remaining ticks; tickets of other types are kept as they were
//! read and written back.

use vanillite_game::level::Level;
use vanillite_game::level::forced::{SAVED_DATA, SavedTickets};
use vanillite_storage::saved_data::SavedDataStore;

/// Loads the forced tickets into `level` and returns the other saved tickets.
pub(crate) fn load(data: &SavedDataStore, level: &mut Level) -> SavedTickets {
    match data.read(SAVED_DATA) {
        Ok(Some(compound)) => {
            let Level { chunks, forced, .. } = level;
            forced.load_tickets(chunks, &compound)
        }
        Ok(None) => SavedTickets::default(),
        Err(e) => {
            log::error!("Cannot read {SAVED_DATA}: {e}");
            SavedTickets::default()
        }
    }
}

/// Writes the forced chunks of `level` and `others`.
pub(crate) fn save(data: &SavedDataStore, level: &Level, others: &SavedTickets) {
    data.write(SAVED_DATA, level.forced.save_tickets(others));
}

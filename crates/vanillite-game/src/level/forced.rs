//! Force-loaded chunks (`TicketStorage`'s `forced` tickets): each keeps its chunk loaded and
//! entity ticking (`ChunkMap.FORCED_TICKET_LEVEL`), and the codec of the dimension's saved
//! chunk tickets (`TicketStorage.CODEC`, saved data `minecraft:chunk_tickets`).

use vanillite_base::ChunkPos;
use vanillite_base::hash::FxHashMap;
use vanillite_nbt::{Compound, List, Tag};
use vanillite_world::chunks::{Chunks, TicketId};

/// `ChunkLevel.byStatus(FullChunkStatus.ENTITY_TICKING)`.
pub const FORCED_TICKET_LEVEL: u8 = 31;

/// `ChunkPos.pack`.
pub fn pack(pos: ChunkPos) -> i64 {
    (i64::from(pos.x) & 0xFFFF_FFFF) | ((i64::from(pos.z) & 0xFFFF_FFFF) << 32)
}

/// The forced chunks of a level with their tickets.
#[derive(Debug, Default)]
pub struct ForcedChunks {
    tickets: FxHashMap<ChunkPos, (TicketId, TicketId)>,
    /// Changed since the last save.
    dirty: bool,
}

impl ForcedChunks {
    /// Whether `pos` is forced.
    pub fn contains(&self, pos: ChunkPos) -> bool {
        self.tickets.contains_key(&pos)
    }

    /// The number of forced chunks.
    pub fn len(&self) -> usize {
        self.tickets.len()
    }

    /// Whether no chunk is forced.
    pub fn is_empty(&self) -> bool {
        self.tickets.is_empty()
    }

    /// The forced chunks sorted like Vanilla's `forcedChunks.longStream().sorted()`.
    pub fn sorted(&self) -> Vec<ChunkPos> {
        let mut out: Vec<ChunkPos> = self.tickets.keys().copied().collect();
        out.sort_by_key(|p| pack(*p));
        out
    }

    /// `updateChunkForced`: adds or removes the ticket; whether anything changed.
    pub fn set(&mut self, chunks: &mut Chunks, pos: ChunkPos, forced: bool) -> bool {
        let changed = if forced {
            match self.tickets.entry(pos) {
                std::collections::hash_map::Entry::Occupied(_) => false,
                std::collections::hash_map::Entry::Vacant(slot) => {
                    let load = chunks.add_ticket(pos, FORCED_TICKET_LEVEL);
                    let simulation = chunks.add_simulation_ticket(pos, FORCED_TICKET_LEVEL);
                    slot.insert((load, simulation));
                    true
                }
            }
        } else if let Some((load, simulation)) = self.tickets.remove(&pos) {
            chunks.remove_ticket(load);
            chunks.remove_simulation_ticket(simulation);
            true
        } else {
            false
        };
        self.dirty |= changed;
        changed
    }

    /// Whether the set changed since the last call (`SavedData.isDirty`).
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }
}

/// The saved data id of the chunk tickets (`TicketStorage.TYPE`).
pub const SAVED_DATA: &str = "minecraft:chunk_tickets";

/// `TicketType.FORCED`.
const FORCED_TYPE: &str = "minecraft:forced";
/// `TicketType.PORTAL`, the other ticket type Vanilla persists.
const PORTAL_TYPE: &str = "minecraft:portal";

/// A saved `portal` ticket: it keeps its chunks loaded for `ticks_left` more ticks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PortalTicket {
    pub pos: ChunkPos,
    pub level: u8,
    pub ticks_left: i64,
}

/// The saved chunk tickets other than the forced ones.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SavedTickets {
    /// The `portal` tickets.
    pub portal: Vec<PortalTicket>,
    /// Tickets of types Vanillite does not know, written back unchanged.
    pub kept: Vec<Tag>,
}

/// `ChunkPos.unpack`.
pub fn unpack(packed: i64) -> ChunkPos {
    #[allow(clippy::cast_possible_truncation)]
    ChunkPos::new(packed as i32, (packed >> 32) as i32)
}

/// `ChunkPos.CODEC`: an int array `[x, z]`; data older than `ChunkTicketUnpackPosFix` has the
/// packed long.
fn chunk_pos(entry: &Compound) -> Option<ChunkPos> {
    match entry.get("chunk_pos") {
        Some(Tag::IntArray(v)) => match v.as_slice() {
            [x, z] => Some(ChunkPos::new(*x, *z)),
            _ => None,
        },
        Some(Tag::Long(packed)) => Some(unpack(*packed)),
        _ => None,
    }
}

fn int(entry: &Compound, key: &str) -> Option<i64> {
    match entry.get(key)? {
        Tag::Byte(v) => Some(i64::from(*v)),
        Tag::Short(v) => Some(i64::from(*v)),
        Tag::Int(v) => Some(i64::from(*v)),
        Tag::Long(v) => Some(*v),
        _ => None,
    }
}

impl ForcedChunks {
    /// Restores the saved chunk tickets (`TicketStorage.fromPacked` and
    /// `activateAllDeactivatedTickets`): the `forced` ones become forced chunks of this set,
    /// which is not dirty afterwards; the others are returned. Entries Vanilla's codec rejects
    /// (no position, a negative level) are dropped.
    pub fn load_tickets(&mut self, chunks: &mut Chunks, data: &Compound) -> SavedTickets {
        let mut saved = SavedTickets::default();
        if let Some(Tag::List(tickets)) = data.get("tickets") {
            for ticket in &tickets.0 {
                let Tag::Compound(entry) = ticket else {
                    continue;
                };
                let (Some(pos), Some(level)) = (chunk_pos(entry), int(entry, "level")) else {
                    continue;
                };
                if level < 0 {
                    continue;
                }
                match entry.get("type") {
                    Some(Tag::String(t)) if t == FORCED_TYPE => {
                        self.set(chunks, pos, true);
                    }
                    Some(Tag::String(t)) if t == PORTAL_TYPE => {
                        saved.portal.push(PortalTicket {
                            pos,
                            level: u8::try_from(level).unwrap_or(u8::MAX),
                            ticks_left: int(entry, "ticks_left").unwrap_or(0),
                        });
                    }
                    _ => saved.kept.push(ticket.clone()),
                }
            }
        }
        // `ForcedChunkToTicketFix`: data of `ForcedChunksSavedData` lists the forced chunks
        // packed.
        if let Some(Tag::LongArray(packed)) = data.get("Forced") {
            for &p in packed {
                self.set(chunks, unpack(p), true);
            }
        }
        // Loading is not a change.
        self.dirty = false;
        saved
    }

    /// The saved chunk tickets (`TicketStorage.packTickets`): the forced chunks, then `others`.
    /// `ticks_left` is left out when it is 0, like Vanilla's optional field.
    pub fn save_tickets(&self, others: &SavedTickets) -> Compound {
        let entry = |pos: ChunkPos, kind: &str, level: u8, ticks_left: i64| {
            let c = Compound::with_capacity(4)
                .with("chunk_pos", vec![pos.x, pos.z])
                .with("type", kind)
                .with("level", i32::from(level));
            Tag::Compound(if ticks_left == 0 {
                c
            } else {
                c.with("ticks_left", ticks_left)
            })
        };
        let mut tickets: Vec<Tag> = self
            .sorted()
            .into_iter()
            .map(|pos| entry(pos, FORCED_TYPE, FORCED_TICKET_LEVEL, 0))
            .collect();
        tickets.extend(
            others
                .portal
                .iter()
                .map(|t| entry(t.pos, PORTAL_TYPE, t.level, t.ticks_left)),
        );
        tickets.extend(others.kept.iter().cloned());
        Compound::with_capacity(1).with("tickets", List(tickets))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::level::Level;

    fn ticket(pos: Tag, kind: &str, level: i32) -> Compound {
        Compound::with_capacity(3)
            .with("chunk_pos", pos)
            .with("type", kind)
            .with("level", level)
    }

    /// Forced and portal tickets come back, unknown ones are kept, and saving writes what was
    /// read (forced chunks in packed order); packed positions and the `Forced` list of older
    /// data are read too.
    #[test]
    fn chunk_tickets_round_trip() {
        let mut level = crate::level::tests::flat_level();
        let unknown = Tag::Compound(ticket(vec![7, 8].into(), "mod:keep", 20).with("extra", 1i8));
        let data = Compound::with_capacity(2)
            .with(
                "tickets",
                List(vec![
                    Tag::Compound(ticket(vec![3, -4].into(), FORCED_TYPE, 31)),
                    Tag::Compound(ticket(pack(ChunkPos::new(-1, 2)).into(), FORCED_TYPE, 31)),
                    Tag::Compound(
                        ticket(vec![5, 6].into(), PORTAL_TYPE, 30).with("ticks_left", 120i64),
                    ),
                    unknown.clone(),
                    Tag::Compound(ticket(vec![1, 1].into(), FORCED_TYPE, -1)),
                ]),
            )
            .with("Forced", vec![pack(ChunkPos::new(9, 9))]);
        let Level { chunks, forced, .. } = &mut level;
        let others = forced.load_tickets(chunks, &data);
        assert!(!forced.take_dirty());
        assert_eq!(
            forced.sorted(),
            vec![
                ChunkPos::new(3, -4),
                ChunkPos::new(-1, 2),
                ChunkPos::new(9, 9)
            ]
        );
        assert_eq!(
            others.portal,
            vec![PortalTicket {
                pos: ChunkPos::new(5, 6),
                level: 30,
                ticks_left: 120
            }]
        );
        assert_eq!(others.kept, vec![unknown]);

        let saved = forced.save_tickets(&others);
        let mut again = crate::level::tests::flat_level();
        let Level { chunks, forced, .. } = &mut again;
        assert_eq!(forced.load_tickets(chunks, &saved), others);
        assert_eq!(forced.save_tickets(&others), saved);
        let Some(Tag::List(list)) = saved.get("tickets") else {
            panic!("no tickets");
        };
        assert_eq!(
            list.0[0],
            Tag::Compound(ticket(vec![3, -4].into(), FORCED_TYPE, 31))
        );
    }
}

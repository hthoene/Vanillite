//! Id tables generated from the data of two adjacent versions.

use crate::generated::{Cb, Sb};

/// Marks an id without counterpart in the other version.
pub const NONE: u16 = u16::MAX;

/// Packet ids of one state and direction: `to_old` maps the newer version's ids (clientbound
/// packets leave the server in the newer version), `to_new` the older version's ids.
#[derive(Debug)]
pub struct PacketTable {
    pub to_old: &'static [u16],
    pub to_new: &'static [u16],
}

impl PacketTable {
    /// The older id of a newer packet id.
    pub fn older(&self, new: i32) -> Option<i32> {
        lookup(self.to_old, new)
    }

    /// The newer id of an older packet id.
    pub fn newer(&self, old: i32) -> Option<i32> {
        lookup(self.to_new, old)
    }
}

/// Ids of a static registry in both versions, matched by name. A newer entry the older version
/// lacks maps to the most similar older entry where that is safe (blocks, items, map
/// decorations, entity types of the same class), else to nothing.
#[derive(Debug)]
pub struct Remap {
    pub to_old: &'static [u16],
    pub to_new: &'static [u16],
}

impl Remap {
    /// The older id of a newer id.
    pub fn older(&self, new: i32) -> Option<i32> {
        lookup(self.to_old, new)
    }

    /// The newer id of an older id.
    pub fn newer(&self, old: i32) -> Option<i32> {
        lookup(self.to_new, old)
    }
}

fn lookup(table: &[u16], id: i32) -> Option<i32> {
    let v = *table.get(usize::try_from(id).ok()?)?;
    (v != NONE).then_some(i32::from(v))
}

/// The closest name in `candidates` for a name they lack: the most trailing `_`-separated words
/// in common (`poplar_planks` → `acacia_planks`, `dappled_forest` → `forest`), then the shortest
/// and alphabetically first name. The same algorithm as `xtask/src/multiversion.rs`.
pub fn similar<'a>(name: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    fn words(s: &str) -> impl DoubleEndedIterator<Item = &str> {
        s.rsplit(':').next().unwrap_or(s).split(['_', '/'])
    }
    let mut best: Option<(usize, &str)> = None;
    for c in candidates {
        let common = words(name)
            .rev()
            .zip(words(c).rev())
            .take_while(|(a, b)| a == b)
            .count();
        if common == 0 {
            continue;
        }
        let better = best
            .is_none_or(|(bc, bn)| common > bc || (common == bc && (c.len(), c) < (bn.len(), bn)));
        if better {
            best = Some((common, c));
        }
    }
    best.map(|b| b.1)
}

/// Everything codegen knows about one step (see `generated/<version>.rs`), for the step code
/// shared by all steps.
#[derive(Debug)]
pub struct Tables {
    pub id: &'static str,
    pub protocol: i32,
    pub next_id: &'static str,
    pub next_protocol: i32,
    /// Login, configuration and play; clientbound, serverbound.
    pub packets: [&'static PacketTable; 6],
    /// Packets whose shape changed, in the same order (newer ids clientbound, older serverbound).
    pub changed: [&'static [i32]; 6],
    /// Packet kinds by newer id (clientbound) or older id (serverbound).
    pub configuration_clientbound: &'static [Cb],
    pub old_configuration_clientbound: &'static [i16],
    pub play_clientbound: &'static [Cb],
    pub old_play_clientbound: &'static [i16],
    pub configuration_serverbound: &'static [Sb],
    pub new_configuration_serverbound: &'static [i16],
    pub play_serverbound: &'static [Sb],
    pub new_play_serverbound: &'static [i16],
    /// The table of a static registry whose ids differ.
    pub remap: fn(&str) -> Option<&'static Remap>,
    pub block_state: &'static Remap,
    pub synchronized: &'static [(&'static str, &'static [&'static str])],
    pub static_registries: &'static [&'static str],
    pub new_particle_types: &'static [&'static str],
    pub new_command_argument_types: &'static [&'static str],
    pub new_stat_types: &'static [&'static str],
    pub new_entity_types: &'static [&'static str],
    pub new_data_component_types: &'static [&'static str],
    pub sound_added: &'static [(u16, &'static str)],
    pub old_serializers: &'static [&'static str],
    pub new_serializers: &'static [&'static str],
    pub entity_data: &'static [&'static [u16]],
    /// The older version's static enchantment registry (before 1.21).
    pub old_enchantments: &'static [&'static str],
    pub old_string_parser: i32,
    pub old_intentionally_empty: i32,
}

impl Tables {
    /// The older id of a newer id of a static registry (the same id if it did not change).
    pub fn older(&self, registry: &str, id: i32) -> Option<i32> {
        match (self.remap)(registry) {
            Some(r) => r.older(id),
            None => Some(id),
        }
    }

    /// The newer id of an older id of a static registry.
    pub fn newer(&self, registry: &str, id: i32) -> Option<i32> {
        match (self.remap)(registry) {
            Some(r) => r.newer(id),
            None => Some(id),
        }
    }

    /// The kind of a newer clientbound packet id.
    pub fn clientbound(&self, state: crate::State, id: i32) -> Option<Cb> {
        let list = match state {
            crate::State::Configuration => self.configuration_clientbound,
            crate::State::Play => self.play_clientbound,
            _ => return None,
        };
        list.get(usize::try_from(id).ok()?).copied()
    }

    /// The older id of a clientbound packet kind.
    pub fn old_clientbound(&self, state: crate::State, kind: Cb) -> Option<i32> {
        let list = match state {
            crate::State::Configuration => self.old_configuration_clientbound,
            crate::State::Play => self.old_play_clientbound,
            _ => return None,
        };
        #[allow(clippy::as_conversions)] // A fieldless enum's discriminant.
        list.get(kind as usize)
            .copied()
            .filter(|&i| i >= 0)
            .map(i32::from)
    }

    /// The kind of an older serverbound packet id.
    pub fn serverbound(&self, state: crate::State, id: i32) -> Option<Sb> {
        let list = match state {
            crate::State::Configuration => self.configuration_serverbound,
            crate::State::Play => self.play_serverbound,
            _ => return None,
        };
        list.get(usize::try_from(id).ok()?).copied()
    }

    /// The newer id of a serverbound packet kind.
    pub fn new_serverbound(&self, state: crate::State, kind: Sb) -> Option<i32> {
        let list = match state {
            crate::State::Configuration => self.new_configuration_serverbound,
            crate::State::Play => self.new_play_serverbound,
            _ => return None,
        };
        #[allow(clippy::as_conversions)] // A fieldless enum's discriminant.
        list.get(kind as usize)
            .copied()
            .filter(|&i| i >= 0)
            .map(i32::from)
    }
}

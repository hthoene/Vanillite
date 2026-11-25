//! Id tables generated from the data of two adjacent versions.

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

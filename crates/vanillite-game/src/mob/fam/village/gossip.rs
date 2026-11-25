//! What villagers tell each other about players and mobs (`GossipContainer`, `GossipType`).
//!
//! Targets are visited in the order of Vanilla's `HashMap<UUID, …>` (bucket of the spread
//! `UUID.hashCode`, then insertion), so the weighted choice of `transferFrom` draws the same
//! entries; the types of a target are visited in declaration order.

use vanillite_base::random::RandomSource;
use vanillite_nbt::{Compound, List, Tag};

/// `GossipType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GossipType {
    MajorNegative,
    MinorNegative,
    MinorPositive,
    MajorPositive,
    Trading,
}

impl GossipType {
    pub const ALL: [GossipType; 5] = [
        GossipType::MajorNegative,
        GossipType::MinorNegative,
        GossipType::MinorPositive,
        GossipType::MajorPositive,
        GossipType::Trading,
    ];

    /// `(id, weight, max, decayPerDay, decayPerTransfer)`.
    const fn params(self) -> (&'static str, i32, i32, i32, i32) {
        match self {
            GossipType::MajorNegative => ("major_negative", -5, 100, 10, 10),
            GossipType::MinorNegative => ("minor_negative", -1, 200, 20, 20),
            GossipType::MinorPositive => ("minor_positive", 1, 25, 1, 5),
            GossipType::MajorPositive => ("major_positive", 5, 20, 0, 20),
            GossipType::Trading => ("trading", 1, 25, 2, 20),
        }
    }

    pub const fn id(self) -> &'static str {
        self.params().0
    }
    pub const fn weight(self) -> i32 {
        self.params().1
    }
    pub const fn max(self) -> i32 {
        self.params().2
    }
    pub const fn decay_per_day(self) -> i32 {
        self.params().3
    }
    pub const fn decay_per_transfer(self) -> i32 {
        self.params().4
    }

    pub fn by_id(id: &str) -> Option<GossipType> {
        GossipType::ALL.into_iter().find(|t| t.id() == id)
    }
}

/// The gossip about one entity (`EntityGossips`).
#[derive(Clone, Debug, PartialEq)]
struct Target {
    uuid: u128,
    values: [Option<i32>; 5],
    /// Insertion order in the map.
    seq: u64,
}

impl Target {
    fn is_empty(&self) -> bool {
        self.values.iter().all(Option::is_none)
    }

    fn weighted(&self, types: impl Fn(GossipType) -> bool) -> i32 {
        GossipType::ALL
            .into_iter()
            .zip(self.values)
            .filter(|(t, _)| types(*t))
            .filter_map(|(t, v)| v.map(|v| v * t.weight()))
            .sum()
    }
}

/// `GossipContainer`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Gossips {
    targets: Vec<Target>,
    /// `HashMap` table length (0 before the first insertion; never shrinks).
    capacity: usize,
    seq: u64,
}

/// `UUID.hashCode` spread like `HashMap.hash`.
fn uuid_hash(u: u128) -> u32 {
    #[allow(clippy::cast_possible_truncation)]
    let hilo = (u >> 64) as u64 ^ u as u64;
    #[allow(clippy::cast_possible_truncation)]
    let h = ((hilo >> 32) as u32) ^ (hilo as u32);
    h ^ (h >> 16)
}

/// One gossip (`GossipEntry`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub target: u128,
    pub kind: GossipType,
    pub value: i32,
}

impl Gossips {
    /// The targets in `HashMap` iteration order.
    fn ordered(&self) -> Vec<usize> {
        let mask = self.capacity.saturating_sub(1) as u32;
        let mut idx: Vec<usize> = (0..self.targets.len()).collect();
        idx.sort_by_key(|&i| (uuid_hash(self.targets[i].uuid) & mask, self.targets[i].seq));
        idx
    }

    fn index(&self, uuid: u128) -> Option<usize> {
        self.targets.iter().position(|t| t.uuid == uuid)
    }

    fn get_or_create(&mut self, uuid: u128) -> usize {
        if let Some(i) = self.index(uuid) {
            return i;
        }
        if self.capacity == 0 {
            self.capacity = 16;
        }
        self.seq += 1;
        self.targets.push(Target {
            uuid,
            values: [None; 5],
            seq: self.seq,
        });
        if self.targets.len() > self.capacity / 4 * 3 {
            self.capacity *= 2;
        }
        self.targets.len() - 1
    }

    /// `unpack`: every gossip in map order.
    pub fn entries(&self) -> Vec<Entry> {
        let mut out = Vec::new();
        for i in self.ordered() {
            let t = &self.targets[i];
            for (kind, v) in GossipType::ALL.into_iter().zip(t.values) {
                if let Some(value) = v {
                    out.push(Entry {
                        target: t.uuid,
                        kind,
                        value,
                    });
                }
            }
        }
        out
    }

    /// Whether there is no gossip.
    pub fn is_empty(&self) -> bool {
        self.targets.is_empty()
    }

    /// `decay`.
    pub fn decay(&mut self) {
        for t in &mut self.targets {
            for (kind, v) in GossipType::ALL.into_iter().zip(t.values.iter_mut()) {
                if let Some(value) = *v {
                    let n = value - kind.decay_per_day();
                    *v = (n >= 2).then_some(n);
                }
            }
        }
        self.targets.retain(|t| !t.is_empty());
    }

    /// `transferFrom(source, random, maxCount)`: copies weighted random gossip of `source`;
    /// returns how many distinct gossips were picked.
    pub fn transfer_from(
        &mut self,
        source: &Gossips,
        random: &mut impl RandomSource,
        max_count: i32,
    ) -> usize {
        let entries = source.entries();
        if entries.is_empty() {
            return 0;
        }
        let mut ranges = Vec::with_capacity(entries.len());
        let mut end = 0;
        for e in &entries {
            end += (e.value * e.kind.weight()).abs();
            ranges.push(end - 1);
        }
        let mut picked: Vec<usize> = Vec::new();
        for _ in 0..max_count {
            let choice = random.next_int_bounded(end);
            let i = ranges.partition_point(|&r| r < choice);
            if !picked.contains(&i) {
                picked.push(i);
            }
        }
        for &i in &picked {
            let e = entries[i];
            let decayed = e.value - e.kind.decay_per_transfer();
            if decayed >= 2 {
                let t = self.get_or_create(e.target);
                let slot = &mut self.targets[t].values[e.kind as usize];
                *slot = Some(slot.map_or(decayed, |old| old.max(decayed)));
            }
        }
        picked.len()
    }

    /// `getReputation(entity, types)`.
    pub fn reputation(&self, uuid: u128, types: impl Fn(GossipType) -> bool) -> i32 {
        self.index(uuid)
            .map_or(0, |i| self.targets[i].weighted(types))
    }

    /// `add(target, type, amount)`.
    pub fn add(&mut self, uuid: u128, kind: GossipType, amount: i32) {
        let i = self.get_or_create(uuid);
        let t = &mut self.targets[i];
        let slot = &mut t.values[kind as usize];
        let value = match *slot {
            None => amount,
            Some(old) => {
                let sum = old + amount;
                if sum > kind.max() {
                    kind.max().max(old)
                } else {
                    sum
                }
            }
        };
        // `makeSureValueIsntTooLowOrTooHigh`.
        *slot = (value >= 2).then_some(value.min(kind.max()));
        if t.is_empty() {
            self.targets.remove(i);
        }
    }

    /// `remove(target, type)`.
    pub fn remove(&mut self, uuid: u128, kind: GossipType) {
        if let Some(i) = self.index(uuid) {
            self.targets[i].values[kind as usize] = None;
            if self.targets[i].is_empty() {
                self.targets.remove(i);
            }
        }
    }

    /// `clear`.
    pub fn clear(&mut self) {
        self.targets.clear();
    }

    /// `putAll(container)`.
    pub fn put_all(&mut self, other: &Gossips) {
        for i in other.ordered() {
            let o = &other.targets[i];
            let t = self.get_or_create(o.uuid);
            for (k, v) in o.values.iter().enumerate() {
                if v.is_some() {
                    self.targets[t].values[k] = *v;
                }
            }
        }
    }

    /// The `Gossips` list tag.
    pub fn save(&self) -> Tag {
        let list = self
            .entries()
            .into_iter()
            .map(|e| {
                #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
                let uuid = vec![
                    (e.target >> 96) as i32,
                    (e.target >> 64) as i32,
                    (e.target >> 32) as i32,
                    e.target as i32,
                ];
                Tag::Compound(
                    Compound::new()
                        .with("Target", Tag::IntArray(uuid))
                        .with("Type", e.kind.id())
                        .with("Value", e.value),
                )
            })
            .collect();
        Tag::List(List(list))
    }

    /// Reads the `Gossips` list tag.
    pub fn load(tag: Option<&Tag>) -> Gossips {
        let mut g = Gossips::default();
        let Some(Tag::List(list)) = tag else {
            return g;
        };
        for c in list.0.iter().filter_map(Tag::as_compound) {
            let (Some(Tag::IntArray(u)), Some(kind), Some(Tag::Int(value))) = (
                c.get("Target"),
                c.get("Type")
                    .and_then(Tag::as_str)
                    .and_then(GossipType::by_id),
                c.get("Value"),
            ) else {
                continue;
            };
            if u.len() != 4 || *value <= 0 {
                continue;
            }
            #[allow(clippy::cast_sign_loss)]
            let uuid = u
                .iter()
                .fold(0u128, |a, &x| (a << 32) | u128::from(x as u32));
            let i = g.get_or_create(uuid);
            g.targets[i].values[kind as usize] = Some(*value);
        }
        g
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vanillite_base::random::LegacyRandom;

    #[test]
    fn add_caps_and_drops_small_values() {
        let mut g = Gossips::default();
        g.add(7, GossipType::Trading, 2);
        g.add(7, GossipType::Trading, 30);
        assert_eq!(g.reputation(7, |_| true), 25);
        g.add(7, GossipType::MinorNegative, 25);
        assert_eq!(g.reputation(7, |_| true), 0);
        g.add(8, GossipType::Trading, 1);
        assert!(g.reputation(8, |_| true) == 0);
    }

    #[test]
    fn transfer_and_decay() {
        let mut a = Gossips::default();
        a.add(1, GossipType::MajorPositive, 20);
        let mut b = Gossips::default();
        let mut r = LegacyRandom::new(5);
        assert_eq!(b.transfer_from(&a, &mut r, 10), 1);
        // 20 - 20 per transfer: nothing worth keeping.
        assert!(b.is_empty());
        a.add(1, GossipType::MinorNegative, 25);
        a.decay();
        assert_eq!(a.reputation(1, |t| t == GossipType::MinorNegative), -5);
        let saved = a.save();
        assert_eq!(Gossips::load(Some(&saved)).entries(), a.entries());
    }
}

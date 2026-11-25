//! `RandomSequences`: the server-wide named randoms loot tables (`random_sequence` of a table,
//! e.g. `minecraft:blocks/oak_leaves`) and `/random` draw from, so a table's rolls do not
//! depend on what else used the level random. Saved as `data/minecraft/random_sequences.dat`
//! ([`SAVED_DATA`]), so the sequences go on where they were after a restart.

use std::collections::BTreeMap;

use vanillite_base::random::{Seed128, XoroshiroRandom};
use vanillite_nbt::{Compound, Tag};

/// The saved data id (`RandomSequences.TYPE`).
pub const SAVED_DATA: &str = "minecraft:random_sequences";

/// The seed options of a sequence: salt, include the world seed, include the sequence id.
pub type SeedOptions = (i32, bool, bool);

/// The named random sequences of a server (`MinecraftServer.randomSequences`).
#[derive(Debug)]
pub struct RandomSequences {
    world_seed: i64,
    salt: i32,
    include_world_seed: bool,
    include_sequence_id: bool,
    sequences: BTreeMap<Box<str>, XoroshiroRandom>,
    /// `SavedData.isDirty`: every draw marks it (`DirtyMarkingRandomSource`), so any `get`.
    dirty: bool,
}

/// The full id of a sequence key (`Identifier.toString`).
fn full_id(key: &str) -> std::borrow::Cow<'_, str> {
    if key.contains(':') {
        key.into()
    } else {
        format!("minecraft:{key}").into()
    }
}

impl RandomSequences {
    /// Fresh sequences of a world seed with Vanilla's defaults (salt 0, world seed and
    /// sequence id included).
    pub fn new(world_seed: i64) -> Self {
        Self {
            world_seed,
            salt: 0,
            include_world_seed: true,
            include_sequence_id: true,
            sequences: BTreeMap::new(),
            dirty: false,
        }
    }

    /// The world seed new sequences start from.
    pub fn world_seed(&self) -> i64 {
        self.world_seed
    }

    /// `RandomSequences.createSequence` with `RandomSequence.createSequence`.
    fn create(&self, id: &str, (salt, world, with_id): SeedOptions) -> XoroshiroRandom {
        let seed = (if world { self.world_seed } else { 0 }) ^ i64::from(salt);
        let mut seed128 = Seed128::upgrade_unmixed(seed);
        if with_id {
            let hash = Seed128::from_hash_of(id);
            seed128 = seed128.xor(hash.lo, hash.hi);
        }
        XoroshiroRandom::from_seed128(seed128.mixed())
    }

    fn defaults(&self) -> SeedOptions {
        (self.salt, self.include_world_seed, self.include_sequence_id)
    }

    /// `get(key, worldSeed)`: the sequence `key`, created on first use.
    pub fn get(&mut self, key: &str) -> &mut XoroshiroRandom {
        self.dirty = true;
        let id = full_id(key);
        if !self.sequences.contains_key(&*id) {
            let random = self.create(&id, self.defaults());
            self.sequences.insert(id.as_ref().into(), random);
        }
        self.sequences.get_mut(&*id).expect("inserted above")
    }

    /// `reset`: a new sequence with the defaults or the given options.
    pub fn reset(&mut self, key: &str, options: Option<SeedOptions>) {
        let id = full_id(key);
        let random = self.create(&id, options.unwrap_or_else(|| self.defaults()));
        self.sequences.insert(id.as_ref().into(), random);
        self.dirty = true;
    }

    /// `setSeedDefaults`.
    pub fn set_defaults(&mut self, (salt, world, with_id): SeedOptions) {
        self.salt = salt;
        self.include_world_seed = world;
        self.include_sequence_id = with_id;
        self.dirty = true;
    }

    /// `clear`: the number of sequences dropped.
    pub fn clear(&mut self) -> usize {
        let count = self.sequences.len();
        self.sequences.clear();
        self.dirty = true;
        count
    }

    /// The ids of the sequences (`forAllSequences`).
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.sequences.keys().map(AsRef::as_ref)
    }

    /// Whether something changed since the last call (`isDirty`, then `setDirty(false)`).
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    /// `RandomSequences.CODEC` decoding the saved `data` compound. Sequences Vanilla's codec
    /// rejects (no two-long `source`) are dropped.
    pub fn load(world_seed: i64, data: &Compound) -> Self {
        let mut out = Self::new(world_seed);
        let number = |key: &str| match data.get(key) {
            Some(Tag::Byte(v)) => Some(i64::from(*v)),
            Some(Tag::Short(v)) => Some(i64::from(*v)),
            Some(Tag::Int(v)) => Some(i64::from(*v)),
            Some(Tag::Long(v)) => Some(*v),
            _ => None,
        };
        #[allow(clippy::cast_possible_truncation)]
        if let Some(salt) = number("salt") {
            out.salt = salt as i32;
        }
        out.include_world_seed = number("include_world_seed").is_none_or(|v| v != 0);
        out.include_sequence_id = number("include_sequence_id").is_none_or(|v| v != 0);
        if let Some(Tag::Compound(sequences)) = data.get("sequences") {
            for (id, entry) in sequences.iter() {
                let Tag::Compound(entry) = entry else {
                    continue;
                };
                let longs: Option<Vec<i64>> = match entry.get("source") {
                    Some(Tag::LongArray(v)) => Some(v.clone()),
                    Some(Tag::IntArray(v)) => Some(v.iter().map(|&x| i64::from(x)).collect()),
                    Some(Tag::List(list)) => list
                        .0
                        .iter()
                        .map(|t| match t {
                            Tag::Long(v) => Some(*v),
                            Tag::Int(v) => Some(i64::from(*v)),
                            _ => None,
                        })
                        .collect(),
                    _ => None,
                };
                if let Some([lo, hi]) = longs.as_deref() {
                    out.sequences.insert(
                        full_id(id).as_ref().into(),
                        XoroshiroRandom::from_longs(*lo, *hi),
                    );
                }
            }
        }
        out
    }

    /// The `data` compound to save (`RandomSequences.CODEC`).
    pub fn to_nbt(&self) -> Compound {
        let mut sequences = Compound::with_capacity(self.sequences.len());
        for (id, random) in &self.sequences {
            let (lo, hi) = random.state();
            sequences.insert(
                id.as_ref(),
                Compound::with_capacity(1).with("source", vec![lo, hi]),
            );
        }
        Compound::with_capacity(4)
            .with("salt", self.salt)
            .with("include_world_seed", self.include_world_seed)
            .with("include_sequence_id", self.include_sequence_id)
            .with("sequences", sequences)
    }
}

#[cfg(test)]
mod tests {
    use vanillite_base::random::RandomSource;

    use super::*;

    #[test]
    fn sequences_go_on_after_a_save() {
        let mut a = RandomSequences::new(42);
        a.reset("minecraft:other", Some((3, false, true)));
        let _ = a.get("blocks/wheat").next_int();
        assert!(a.take_dirty());
        let mut b = RandomSequences::load(42, &a.to_nbt());
        assert_eq!(b.to_nbt(), a.to_nbt());
        assert_eq!(
            a.get("minecraft:blocks/wheat").next_int(),
            b.get("blocks/wheat").next_int()
        );
        assert_eq!(a.get("other").next_long(), b.get("other").next_long());
    }
}

//! `RandomSequences`: the named randoms loot tables roll with (`random_sequence` of a table,
//! e.g. `minecraft:blocks/dirt`), seeded from the world seed and the sequence's name. Vanilla
//! keeps one set for the whole server (the overworld's, shared by every dimension).

use std::sync::{Arc, Mutex, PoisonError};

use vanillite_base::hash::FxHashMap;
use vanillite_base::random::{Seed128, XoroshiroRandom};

/// The sequences of a server and how new ones are seeded (`setSeedDefaults`).
#[derive(Debug)]
pub struct RandomSequences {
    world_seed: i64,
    salt: i32,
    include_world_seed: bool,
    include_sequence_id: bool,
    sequences: FxHashMap<Box<str>, XoroshiroRandom>,
}

impl RandomSequences {
    pub fn new(world_seed: i64) -> RandomSequences {
        RandomSequences {
            world_seed,
            salt: 0,
            include_world_seed: true,
            include_sequence_id: true,
            sequences: FxHashMap::default(),
        }
    }

    /// `get(key, worldSeed)`: the sequence named `key`, created on first use.
    pub fn get(&mut self, key: &str) -> &mut XoroshiroRandom {
        if !self.sequences.contains_key(key) {
            let seed = (if self.include_world_seed {
                self.world_seed
            } else {
                0
            }) ^ i64::from(self.salt);
            let random = create(seed, self.include_sequence_id.then_some(key));
            self.sequences.insert(key.into(), random);
        }
        self.sequences
            .get_mut(key)
            .unwrap_or_else(|| unreachable!("inserted above"))
    }
}

/// `RandomSequence.createSequence(seed, key)`.
fn create(seed: i64, key: Option<&str>) -> XoroshiroRandom {
    let mut seed = Seed128::upgrade_unmixed(seed);
    if let Some(key) = key {
        let hash = Seed128::from_hash_of(&qualified(key));
        seed = seed.xor(hash.lo, hash.hi);
    }
    XoroshiroRandom::from_seed128(seed.mixed())
}

/// `Identifier.toString()`: with the `minecraft` namespace when it has none.
fn qualified(key: &str) -> String {
    if key.contains(':') {
        key.to_owned()
    } else {
        format!("minecraft:{key}")
    }
}

/// The sequences one or more levels share.
pub type SharedRandomSequences = Arc<Mutex<RandomSequences>>;

/// Runs `f` with the sequence `key` of `shared`.
pub fn with_sequence<R>(
    shared: &SharedRandomSequences,
    key: &str,
    f: impl FnOnce(&mut XoroshiroRandom) -> R,
) -> R {
    let mut sequences = shared.lock().unwrap_or_else(PoisonError::into_inner);
    f(sequences.get(key))
}

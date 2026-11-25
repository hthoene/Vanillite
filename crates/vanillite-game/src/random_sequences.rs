//! `RandomSequences`: the server-wide named randoms loot tables draw from (`random_sequence`
//! of a table, e.g. `minecraft:blocks/oak_leaves`), so a table's rolls do not depend on what
//! else used the level random.

use std::collections::HashMap;

use vanillite_base::random::{Seed128, XoroshiroRandom};

/// The named random sequences of a server (`MinecraftServer.randomSequences`).
#[derive(Debug)]
pub struct RandomSequences {
    world_seed: i64,
    salt: i32,
    include_world_seed: bool,
    include_sequence_id: bool,
    sequences: HashMap<Box<str>, XoroshiroRandom>,
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
            sequences: HashMap::new(),
        }
    }

    /// `get(key, worldSeed)`: the sequence `key`, created on first use.
    pub fn get(&mut self, key: &str) -> &mut XoroshiroRandom {
        if !self.sequences.contains_key(key) {
            let seed = if self.include_world_seed {
                self.world_seed
            } else {
                0
            } ^ i64::from(self.salt);
            // `RandomSequence.createSequence`.
            let mut seed128 = Seed128::upgrade_unmixed(seed);
            if self.include_sequence_id {
                let id = if key.contains(':') {
                    key.to_owned()
                } else {
                    format!("minecraft:{key}")
                };
                let hash = Seed128::from_hash_of(&id);
                seed128 = seed128.xor(hash.lo, hash.hi);
            }
            self.sequences
                .insert(key.into(), XoroshiroRandom::from_seed128(seed128.mixed()));
        }
        self.sequences.get_mut(key).expect("inserted above")
    }
}

//! The Vanilla-specific parts of `vanillite-base` against Java: RNG streams, 128-bit seeding,
//! `Mth.sin`/`cos` and `Mth.getSeed`.

mod common;

use serde_json::Value;
use vanillite_base::math;
use vanillite_base::random::{
    LegacyRandom, PositionalRandomFactory, RandomSource, Seed128, XoroshiroRandom,
};
use vanillite_worldgen::random::WorldgenRandom;

fn i64_of(v: &Value) -> i64 {
    v.as_i64().expect("long")
}

fn check_stream<R: RandomSource>(golden: &Value, mut random: R, name: &str) {
    let seed = i64_of(&golden["seed"]);
    let ctx = format!("{name} seed {seed}");
    for (i, expected) in golden["long"].as_array().unwrap().iter().enumerate() {
        assert_eq!(random.next_long(), i64_of(expected), "{ctx}: nextLong #{i}");
    }
    assert_eq!(
        i64::from(random.next_int()),
        i64_of(&golden["int"]),
        "{ctx}: nextInt"
    );
    let bounds = [1, 2, 3, 7, 16, 100, 1_000_000_007, i32::MAX];
    for (bound, expected) in bounds.iter().zip(golden["int_bounded"].as_array().unwrap()) {
        assert_eq!(
            i64::from(random.next_int_bounded(*bound)),
            i64_of(expected),
            "{ctx}: nextInt({bound})"
        );
    }
    assert_eq!(
        random.next_float(),
        common::f32_bits(&golden["float"]),
        "{ctx}: nextFloat"
    );
    assert_eq!(
        random.next_double().to_bits(),
        common::f64_bits(&golden["double"]).to_bits(),
        "{ctx}: nextDouble"
    );
    assert_eq!(
        random.next_gaussian().to_bits(),
        common::f64_bits(&golden["gaussian"]).to_bits(),
        "{ctx}: nextGaussian"
    );
    assert_eq!(
        random.next_boolean(),
        golden["boolean"].as_bool().unwrap(),
        "{ctx}: nextBoolean"
    );
    let positional = random.fork_positional();
    assert_eq!(
        positional.at(1, -2, 3).next_long(),
        i64_of(&golden["at"]),
        "{ctx}: at"
    );
    assert_eq!(
        positional.with_hash_of("minecraft:test").next_long(),
        i64_of(&golden["from_hash_of"]),
        "{ctx}: fromHashOf"
    );
    assert_eq!(
        positional.with_seed(77).next_long(),
        i64_of(&golden["from_seed"]),
        "{ctx}: fromSeed"
    );
    assert_eq!(
        random.fork().next_long(),
        i64_of(&golden["fork"]),
        "{ctx}: fork"
    );
}

#[test]
fn xoroshiro_streams() {
    for golden in common::golden("base")["xoroshiro"].as_array().unwrap() {
        check_stream(
            golden,
            XoroshiroRandom::new(i64_of(&golden["seed"])),
            "xoroshiro",
        );
    }
}

#[test]
fn legacy_streams() {
    for golden in common::golden("base")["legacy"].as_array().unwrap() {
        check_stream(golden, LegacyRandom::new(i64_of(&golden["seed"])), "legacy");
    }
}

#[test]
fn seed_upgrade_and_hash() {
    let golden = common::golden("base");
    for e in golden["upgrade_seed"].as_array().unwrap() {
        let seed = Seed128::upgrade(i64_of(&e[0]));
        assert_eq!(
            (seed.lo, seed.hi),
            (i64_of(&e[1]), i64_of(&e[2])),
            "upgrade {}",
            e[0]
        );
    }
    for e in golden["seed_from_hash_of"].as_array().unwrap() {
        let seed = Seed128::from_hash_of(e[0].as_str().unwrap());
        assert_eq!(
            (seed.lo, seed.hi),
            (i64_of(&e[1]), i64_of(&e[2])),
            "hash of {}",
            e[0]
        );
    }
}

#[test]
fn get_seed() {
    for e in common::golden("base")["get_seed"].as_array().unwrap() {
        let [x, y, z] = [0, 1, 2].map(|i| i64_of(&e[i]) as i32);
        assert_eq!(
            math::get_seed(x, y, z),
            i64_of(&e[3]),
            "getSeed({x}, {y}, {z})"
        );
    }
}

#[test]
fn sin_table() {
    let golden = common::golden("base");
    let mut hash = 0_i64;
    for i in 0..65536 {
        hash = hash
            .wrapping_mul(31)
            .wrapping_add(i64::from(math::sin_table_entry(i).to_bits() as i32));
    }
    assert_eq!(hash, i64_of(&golden["sin_table_hash"]), "Mth.SIN table");
    for e in golden["sin"].as_array().unwrap() {
        let angle = common::f64_bits(&e[0]);
        assert_eq!(math::sin(angle), common::f32_bits(&e[1]), "sin({angle})");
        assert_eq!(math::cos(angle), common::f32_bits(&e[2]), "cos({angle})");
    }
}

#[test]
fn worldgen_random_seeding() {
    for e in common::golden("base")["worldgen_random"]
        .as_array()
        .unwrap()
    {
        let seed = i64_of(&e[0]);
        let mut random = WorldgenRandom::new(0);
        random.set_large_feature_seed(seed, -3, 7);
        assert_eq!(
            random.random.next_long(),
            i64_of(&e[1]),
            "large feature seed {seed}"
        );
        assert_eq!(
            random.set_decoration_seed(seed, 123, -456),
            i64_of(&e[2]),
            "decoration seed {seed}"
        );
        assert_eq!(
            i64::from(random.random.next_int_bounded(1000)),
            i64_of(&e[3])
        );
    }
}

//! Biome goldens recorded from Vanilla 26.3 (`BiomeGoldens.java`).

mod common;

use serde_json::Value;
use vanillite_base::random::{RandomSource, XoroshiroRandom};
use vanillite_chunk::BiomeId;
use vanillite_worldgen::biome::{
    BiomeManager, BiomeRegistry, ParameterList, Preset, SearchCache, TargetPoint, obfuscate_seed,
    quantize_coord, target, unquantize_coord,
};

fn int(v: &Value) -> i64 {
    v.as_i64().expect("integer")
}

#[test]
fn quantize() {
    let golden = common::golden("biome");
    for e in golden["quantize"].as_array().unwrap() {
        let f = common::f32_bits(&e[0]);
        let q = quantize_coord(f);
        assert_eq!(q, int(&e[1]), "quantize {f}");
        assert_eq!(
            unquantize_coord(q).to_bits(),
            common::f32_bits(&e[2]).to_bits()
        );
    }
    for e in golden["target"].as_array().unwrap() {
        let f: Vec<f32> = e[0]
            .as_array()
            .unwrap()
            .iter()
            .map(common::f32_bits)
            .collect();
        let t = target(f[0], f[1], f[2], f[3], f[4], f[5]);
        let expected: Vec<i64> = e[1].as_array().unwrap().iter().map(int).collect();
        assert_eq!(&t.to_parameter_array()[..6], &expected[..]);
    }
}

/// Java's `String.hashCode`.
fn java_hash(s: &str) -> i64 {
    s.encode_utf16()
        .fold(0i32, |h, c| h.wrapping_mul(31).wrapping_add(i32::from(c))) as i64
}

fn check_search(preset: Preset, golden: &Value, seed: i64) {
    let entries = preset.entries();
    assert_eq!(
        entries.len() as i64,
        int(&golden["count"]),
        "{preset:?} size"
    );
    let mut hash = 0i64;
    let mut bounds: [Vec<i64>; 6] = Default::default();
    for (point, name) in entries {
        hash = hash.wrapping_mul(31).wrapping_add(java_hash(name));
        let space = point.parameter_space();
        for d in 0..6 {
            for v in [space[d].min, space[d].max] {
                hash = hash.wrapping_mul(31).wrapping_add(v);
                bounds[d].push(v);
            }
        }
        hash = hash.wrapping_mul(31).wrapping_add(point.offset);
    }
    assert_eq!(hash, int(&golden["hash"]), "{preset:?} parameter list hash");

    let mut random = XoroshiroRandom::new(seed);
    let mut target_hash = 0i64;
    let targets: Vec<TargetPoint> = (0..2000)
        .map(|_| {
            let mut t = [0i64; 6];
            for d in 0..6 {
                let len = bounds[d].len() as i32;
                t[d] = match random.next_int_bounded(4) {
                    0 => bounds[d][random.next_int_bounded(len) as usize],
                    1 => {
                        bounds[d][random.next_int_bounded(len) as usize]
                            + i64::from(random.next_int_bounded(3))
                            - 1
                    }
                    _ => i64::from(random.next_int_bounded(24001) - 12000),
                };
                target_hash = target_hash.wrapping_mul(31).wrapping_add(t[d]);
            }
            TargetPoint {
                temperature: t[0],
                humidity: t[1],
                continentalness: t[2],
                erosion: t[3],
                depth: t[4],
                weirdness: t[5],
            }
        })
        .collect();
    assert_eq!(target_hash, int(&golden["target_hash"]), "targets");

    let names: Vec<&str> = golden["names"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    let list =
        ParameterList::new(entries.iter().map(|(p, n)| (*p, n.as_str())).collect()).expect("list");
    let expected = |key: &str| -> Vec<&str> {
        golden[key]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| names[int(i) as usize])
            .collect()
    };

    let mut cache = SearchCache::default();
    let cached: Vec<&str> = targets
        .iter()
        .map(|t| *list.find_value(t, &mut cache))
        .collect();
    let uncached: Vec<&str> = targets
        .iter()
        .map(|t| *list.find_value(t, &mut SearchCache::default()))
        .collect();
    let brute: Vec<&str> = targets
        .iter()
        .map(|t| *list.find_value_brute_force(t))
        .collect();
    for (what, got) in [
        ("cached", &cached),
        ("uncached", &uncached),
        ("brute_force", &brute),
    ] {
        let want = expected(what);
        let wrong: Vec<usize> = (0..want.len()).filter(|&i| got[i] != want[i]).collect();
        assert!(
            wrong.is_empty(),
            "{preset:?} {what}: {} of {} differ, first at {:?}: {} != {}",
            wrong.len(),
            want.len(),
            targets[wrong[0]],
            got[wrong[0]],
            want[wrong[0]]
        );
    }
}

#[test]
fn overworld_search() {
    check_search(Preset::Overworld, &common::golden("biome")["overworld"], 7);
}

#[test]
fn nether_search() {
    check_search(Preset::Nether, &common::golden("biome")["nether"], 8);
}

#[test]
fn obfuscate_seed_matches() {
    for e in common::golden("biome")["obfuscate_seed"]
        .as_array()
        .unwrap()
    {
        assert_eq!(obfuscate_seed(int(&e[0])), int(&e[1]), "seed {}", e[0]);
    }
}

#[test]
fn zoom() {
    for s in common::golden("biome")["zoom"].as_array().unwrap() {
        let manager = BiomeManager::for_world_seed(int(&s["seed"]));
        for e in s["samples"].as_array().unwrap() {
            let v: Vec<i32> = e
                .as_array()
                .unwrap()
                .iter()
                .map(|v| int(v) as i32)
                .collect();
            assert_eq!(manager.zoom(v[0], v[1], v[2]), (v[3], v[4], v[5]), "{e}");
            // A synthetic noise biome source: a hash of the quart coordinates.
            let mut source = |qx: i32, qy: i32, qz: i32| -> BiomeId {
                (qx.wrapping_mul(31)
                    .wrapping_add(qy)
                    .wrapping_mul(31)
                    .wrapping_add(qz)
                    & 63) as BiomeId
            };
            let expected = source(v[3], v[4], v[5]);
            assert_eq!(manager.get_biome(v[0], v[1], v[2], &mut source), expected);
        }
    }
}

#[test]
fn registry() {
    let registry = BiomeRegistry::load(common::packs()).expect("biome registry");
    let plains = registry.get(registry.id_of("minecraft:plains").unwrap());
    assert_eq!(plains.temperature, 0.8);
    assert_eq!(plains.downfall, 0.4);
    assert!(plains.has_precipitation);
    assert!(!plains.frozen_temperature_modifier);
    assert_eq!(
        plains.carvers,
        [
            "minecraft:cave",
            "minecraft:cave_extra_underground",
            "minecraft:canyon"
        ]
    );
    let frozen = registry.get(registry.id_of("minecraft:frozen_ocean").unwrap());
    assert!(frozen.frozen_temperature_modifier);
    let nether = registry.get(registry.id_of("minecraft:nether_wastes").unwrap());
    assert_eq!(nether.carvers, ["minecraft:nether_cave"]);
    for (i, b) in registry.all().iter().enumerate() {
        assert_eq!(usize::from(b.id), i);
    }
    for preset in [Preset::Overworld, Preset::Nether] {
        for (_, name) in preset.entries() {
            assert!(registry.id_of(name).is_some(), "{name}");
        }
    }
}

#[test]
fn temperature() {
    let registry = BiomeRegistry::load(common::packs()).expect("biome registry");
    for b in common::golden("biome")["temperature"].as_array().unwrap() {
        let biome = registry.get(registry.id_of(b["biome"].as_str().unwrap()).unwrap());
        let sea_level = int(&b["sea_level"]) as i32;
        for e in b["samples"].as_array().unwrap() {
            let (x, y, z) = (int(&e[0]) as i32, int(&e[1]) as i32, int(&e[2]) as i32);
            let t = biome.temperature_at(x, y, z, sea_level);
            assert_eq!(
                t.to_bits(),
                common::f32_bits(&e[3]).to_bits(),
                "{} {e}",
                biome.name
            );
            assert_eq!(
                biome.cold_enough_to_snow(x, y, z, sea_level),
                e[4].as_bool().unwrap()
            );
            assert_eq!(
                biome.should_melt_frozen_ocean_iceberg_slightly(x, y, z, sea_level),
                e[5].as_bool().unwrap()
            );
        }
    }
}

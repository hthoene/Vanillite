//! Light of freshly generated chunks against Vanilla: `LightGoldens.java` lets Vanilla generate
//! and light the 3 × 3 area around a few chunks; the golden holds the blocks of the area and the
//! light of the center. The center is lit from those blocks, as in generation and as after a
//! block change, and must equal Vanilla's light in every section Vanilla keeps light for.
//!
//! The golden is part of the worldgen set: `cargo xtask golden regen LightGoldens`.

mod common;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::Value;
use vanillite_base::ChunkPos;
use vanillite_nbt::Budget;
use vanillite_storage::{Codec, Compression};

fn base64(text: &str) -> Vec<u8> {
    let value = |c: u8| match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => panic!("bad Base64 character {c}"),
    };
    let digits: Vec<u8> = text.bytes().filter(|&c| c != b'=').map(value).collect();
    let mut out = Vec::with_capacity(digits.len() * 3 / 4);
    for group in digits.chunks(4) {
        let mut bits = 0u32;
        for (i, &d) in group.iter().enumerate() {
            bits |= u32::from(d) << (18 - 6 * i);
        }
        for i in 0..group.len() - 1 {
            out.push((bits >> (16 - 8 * i)) as u8);
        }
    }
    out
}

fn golden() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../vanillite-worldgen/tests/goldens/light.json");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "{}: {e} (record it with `cargo xtask golden regen LightGoldens`)",
            path.display()
        )
    });
    serde_json::from_str(&text).expect("golden JSON")
}

#[test]
fn generated_light_matches_vanilla() {
    common::install();
    let codec = Codec::new(&common::world_info(Path::new("")));
    let golden = golden();
    let mut failures = Vec::new();
    for area in golden["areas"].as_array().expect("areas") {
        let center = ChunkPos::new(
            area["center"][0].as_i64().unwrap() as i32,
            area["center"][1].as_i64().unwrap() as i32,
        );
        let mut chunks = HashMap::new();
        for encoded in area["chunks"].as_array().expect("chunks") {
            let bytes = Compression::Gzip
                .decompress(&base64(encoded.as_str().unwrap()))
                .expect("gzip");
            let root = vanillite_nbt::read::read_named(&mut bytes.as_slice(), &mut Budget::file())
                .expect("chunk NBT");
            let pos = ChunkPos::new(
                root.get("xPos").and_then(|t| t.as_i64()).unwrap() as i32,
                root.get("zPos").and_then(|t| t.as_i64()).unwrap() as i32,
            );
            chunks.insert(pos, codec.read(pos, root).expect("chunk"));
        }
        let world = common::World::from_fn(move |pos| chunks.get(&pos).cloned());
        for mode in [common::Mode::Generation, common::Mode::Relight] {
            let mut diffs = Vec::new();
            let stats = world.compare(center, mode, |diff| {
                if diffs.len() < 10 {
                    diffs.push(diff.to_string());
                }
            });
            if stats.differ() > 0 {
                failures.push(format!(
                    "{center:?} {mode:?}: {stats}\n  {}",
                    diffs.join("\n  ")
                ));
            }
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

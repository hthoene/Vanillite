//! Shared test support: the Vanilla data pack, chunk facts and the Java goldens.
//!
//! The Vanilla pack is extracted from the pinned server jar into `$VANILLITE_CACHE` (default
//! `<workspace>/target/vanilla-cache`); `$VANILLITE_SERVER_JAR` points at a local copy of the
//! jar instead of downloading it. Goldens are recorded by `cargo xtask golden regen`.

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::OnceLock;

use serde_json::Value;
use vanillite_chunk::facts;
use vanillite_data::block::{BlockState, STATE_COUNT};
use vanillite_datapack::LoadedPacks;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

/// The loaded Vanilla data pack.
pub fn packs() -> &'static LoadedPacks {
    static PACKS: OnceLock<LoadedPacks> = OnceLock::new();
    PACKS.get_or_init(|| {
        let cache = std::env::var_os("VANILLITE_CACHE").map_or_else(
            || workspace_root().join("target/vanilla-cache"),
            PathBuf::from,
        );
        let jar = std::env::var_os("VANILLITE_SERVER_JAR").map(PathBuf::from);
        let pack = vanillite_datapack::vanilla::ensure_vanilla_pack(&cache, jar.as_deref())
            .expect("Vanilla data pack (set VANILLITE_SERVER_JAR for an offline copy)");
        let packs = vanillite_datapack::load(&[pack.as_path()]).expect("load Vanilla data pack");
        install_facts(&packs);
        packs
    })
}

/// Installs the per-state chunk facts from the generated block data and the pack's tags.
fn install_facts(packs: &LoadedPacks) {
    let motion = vanillite_worldgen::blocks::BlockSet::from_tag(
        packs,
        "minecraft:blocks_motion_in_heightmap",
    );
    let motion_no_leaves = vanillite_worldgen::blocks::BlockSet::from_tag(
        packs,
        "minecraft:blocks_motion_in_heightmap_no_leaves",
    );
    let flags: Vec<u8> = (0..STATE_COUNT)
        .map(|id| {
            let state = BlockState(id as u16);
            let mut f = 0;
            if state.is_air() {
                f |= facts::AIR;
            }
            if state.random_ticks() {
                f |= facts::RANDOM_TICKING;
            }
            if !state.fluid_state().is_empty() {
                f |= facts::FLUID;
            }
            if motion.contains(state) {
                f |= facts::BLOCKS_MOTION;
            }
            if motion_no_leaves.contains(state) {
                f |= facts::BLOCKS_MOTION_NO_LEAVES;
            }
            f
        })
        .collect();
    let biome_count = packs
        .registry("minecraft:worldgen/biome")
        .map_or(0, |r| r.entries().len() as u32);
    facts::install(facts::Tables {
        block_flags: Box::leak(flags.into_boxed_slice()),
        biome_count,
    });
}

/// A golden file from `tests/goldens/`.
pub fn golden(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/goldens")
        .join(format!("{name}.json"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "{}: {e} (record it with `cargo xtask golden regen`)",
            path.display()
        )
    });
    serde_json::from_str(&text).expect("golden JSON")
}

/// A float stored as its bit pattern in a golden.
pub fn f32_bits(v: &Value) -> f32 {
    f32::from_bits(v.as_i64().expect("float bits") as u32)
}

/// A double stored as its bit pattern in a golden.
pub fn f64_bits(v: &Value) -> f64 {
    f64::from_bits(v.as_i64().expect("double bits") as u64)
}

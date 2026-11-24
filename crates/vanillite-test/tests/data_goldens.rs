//! Data parity with Vanilla: the generated tables and the loaded Vanilla data pack against what
//! Vanilla holds in memory (recorded by `tools/extractor/.../golden/data/*Goldens.java`).

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::OnceLock;

use serde_json::Value;
use vanillite_data::block::{BLOCKS, BlockId, BlockState};
use vanillite_data::item::{ITEMS, ItemId, Rarity};
use vanillite_data::packets;
use vanillite_data::registry::{ALL, Registry};
use vanillite_datapack::LoadedPacks;
use vanillite_test::golden::{data, lines_hash, workspace_root};
use vanillite_test::world::state_name;

fn golden(name: &str) -> serde_json::Map<String, Value> {
    match data(name) {
        Ok(Value::Object(m)) => m,
        Ok(_) => panic!("golden {name} is not an object"),
        Err(e) => panic!("{e}"),
    }
}

fn assert_none(errors: &[String]) {
    assert!(
        errors.is_empty(),
        "{} differences:\n  {}",
        errors.len(),
        errors.join("\n  ")
    );
}

/// Compares the key sets of a golden and of Vanillite.
fn same_keys<'a>(
    what: &str,
    golden: impl Iterator<Item = &'a String>,
    ours: impl Iterator<Item = &'a str>,
    errors: &mut Vec<String>,
) {
    let g: BTreeSet<&str> = golden.map(String::as_str).collect();
    let o: BTreeSet<&str> = ours.collect();
    for missing in g.difference(&o) {
        errors.push(format!("{what} {missing}: missing in Vanillite"));
    }
    for extra in o.difference(&g) {
        errors.push(format!("{what} {extra}: not in Vanilla"));
    }
}

#[test]
fn registries() {
    let golden = golden("registries");
    let mut errors = Vec::new();
    same_keys(
        "registry",
        golden.keys(),
        ALL.iter().map(|r| r.name),
        &mut errors,
    );
    for (name, g) in &golden {
        let Some(r) = Registry::by_name(name) else {
            continue;
        };
        if g["id"].as_u64() != Some(u64::from(r.protocol_id)) {
            errors.push(format!(
                "{name}: id {}, expected {}",
                r.protocol_id, g["id"]
            ));
        }
        if g["default"].as_str() != r.default {
            errors.push(format!(
                "{name}: default {:?}, expected {}",
                r.default, g["default"]
            ));
        }
        let hash = lines_hash(r.entries.iter().copied());
        if g["count"].as_u64() != Some(r.len() as u64) || g["hash"].as_str() != Some(&hash) {
            errors.push(format!(
                "{name}: {} entries (hash {hash}), expected {} ({})",
                r.len(),
                g["count"],
                g["hash"]
            ));
        }
    }
    assert_none(&errors);
}

#[test]
fn block_states() {
    let golden = golden("block_states");
    let mut errors = Vec::new();
    same_keys(
        "block",
        golden.keys(),
        BLOCKS.iter().map(|b| b.name),
        &mut errors,
    );
    for (name, g) in &golden {
        let Some(block) = BlockId::by_name(name) else {
            continue;
        };
        let info = block.info();
        let names: Vec<String> = (info.first_state..info.first_state + info.state_count)
            .map(|id| state_name(BlockState(id)))
            .collect();
        let hash = lines_hash(names.iter().map(String::as_str));
        let ours = (
            u64::from(info.first_state),
            u64::from(info.state_count),
            u64::from(info.default_state),
        );
        let theirs = (
            g["first"].as_u64().unwrap_or(u64::MAX),
            g["count"].as_u64().unwrap_or(u64::MAX),
            g["default"].as_u64().unwrap_or(u64::MAX),
        );
        if ours != theirs || g["hash"].as_str() != Some(&hash) {
            errors.push(format!(
                "{name}: (first, count, default) {ours:?} hash {hash}, expected {theirs:?} {}",
                g["hash"]
            ));
        }
    }
    assert_none(&errors);
}

#[test]
fn items() {
    let golden = golden("items");
    let mut errors = Vec::new();
    same_keys(
        "item",
        golden.keys(),
        ITEMS.iter().map(|i| i.name),
        &mut errors,
    );
    for (name, g) in &golden {
        let Some(item) = ItemId::by_name(name) else {
            continue;
        };
        let info = item.info();
        let rarity = match info.rarity {
            Rarity::Common => "common",
            Rarity::Uncommon => "uncommon",
            Rarity::Rare => "rare",
            Rarity::Epic => "epic",
        };
        let block = item.block().map_or("", BlockId::name);
        let ours = serde_json::json!([info.max_stack_size, info.max_damage, rarity, block]);
        if &ours != g {
            errors.push(format!("{name}: {ours}, expected {g}"));
        }
    }
    assert_none(&errors);
}

#[test]
fn packet_ids() {
    let golden = golden("packets");
    let ours: [(&str, &str, &[&str]); 9] = [
        (
            "handshake",
            "serverbound",
            &packets::handshake::serverbound::NAMES,
        ),
        (
            "status",
            "clientbound",
            &packets::status::clientbound::NAMES,
        ),
        (
            "status",
            "serverbound",
            &packets::status::serverbound::NAMES,
        ),
        ("login", "clientbound", &packets::login::clientbound::NAMES),
        ("login", "serverbound", &packets::login::serverbound::NAMES),
        (
            "configuration",
            "clientbound",
            &packets::configuration::clientbound::NAMES,
        ),
        (
            "configuration",
            "serverbound",
            &packets::configuration::serverbound::NAMES,
        ),
        ("play", "clientbound", &packets::play::clientbound::NAMES),
        ("play", "serverbound", &packets::play::serverbound::NAMES),
    ];
    let mut errors = Vec::new();
    let golden_count: usize = golden
        .values()
        .filter_map(Value::as_object)
        .map(serde_json::Map::len)
        .sum();
    if golden_count != ours.len() {
        errors.push(format!(
            "Vanilla has {golden_count} state/direction pairs, Vanillite {}",
            ours.len()
        ));
    }
    for (state, dir, names) in ours {
        let expected: Vec<&str> = golden
            .get(state)
            .and_then(|s| s.get(dir))
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        if expected != names {
            for i in 0..expected.len().max(names.len()) {
                let (e, o) = (expected.get(i), names.get(i));
                if e != o {
                    errors.push(format!("{state} {dir} id {i}: {o:?}, expected {e:?}"));
                }
            }
        }
    }
    assert_none(&errors);
}

/// The Vanilla data pack, extracted from the pinned jar into `$VANILLITE_CACHE` (default
/// `<workspace>/target/vanilla-cache`); `$VANILLITE_SERVER_JAR` points at a local jar.
fn packs() -> &'static LoadedPacks {
    static PACKS: OnceLock<LoadedPacks> = OnceLock::new();
    PACKS.get_or_init(|| {
        let cache = std::env::var_os("VANILLITE_CACHE").map_or_else(
            || workspace_root().join("target/vanilla-cache"),
            PathBuf::from,
        );
        let jar = std::env::var_os("VANILLITE_SERVER_JAR").map(PathBuf::from);
        let pack = vanillite_datapack::vanilla::ensure_vanilla_pack(&cache, jar.as_deref())
            .expect("Vanilla data pack (set VANILLITE_SERVER_JAR for an offline copy)");
        vanillite_datapack::load(&[pack.as_path()]).expect("load Vanilla data pack")
    })
}

#[test]
fn data_pack() {
    let golden = golden("data_pack");
    let packs = packs();
    let mut errors = Vec::new();

    let registries = golden["registries"].as_object().expect("registries");
    for (name, g) in registries {
        let Some(r) = packs.registry(name) else {
            errors.push(format!("registry {name}: not loaded"));
            continue;
        };
        let hash = lines_hash(r.entries().iter().map(|e| e.id.as_str()));
        if g["count"].as_u64() != Some(r.entries().len() as u64)
            || g["hash"].as_str() != Some(&hash)
        {
            errors.push(format!(
                "registry {name}: {} entries (hash {hash}), expected {} ({})",
                r.entries().len(),
                g["count"],
                g["hash"]
            ));
        }
    }

    let tags = golden["tags"].as_object().expect("tags");
    same_keys(
        "tagged registry",
        tags.keys(),
        packs.tagged_registries(),
        &mut errors,
    );
    for (registry, g) in tags {
        let Some(ours) = packs.tags(registry) else {
            continue;
        };
        let lines: Vec<String> = ours
            .iter()
            .map(|(tag, entries)| {
                let mut line = tag.clone();
                for e in entries {
                    line.push(' ');
                    line.push_str(e);
                }
                line
            })
            .collect();
        let hash = lines_hash(lines.iter().map(String::as_str));
        if g["count"].as_u64() != Some(ours.len() as u64) || g["hash"].as_str() != Some(&hash) {
            errors.push(format!(
                "tags of {registry}: {} tags (hash {hash}), expected {} ({})",
                ours.len(),
                g["count"],
                g["hash"]
            ));
        }
    }
    assert_none(&errors);
}

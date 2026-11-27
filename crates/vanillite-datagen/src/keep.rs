//! The facts of an older version that the multi-version layer needs (`vanillite-multiversion`,
//! architecture 14.3), from its extracted server jar:
//!
//! - `version.toml`, `packets.json`, `registries.json`: as for the current version;
//! - `blocks.json`: every block with its first and default state and its properties;
//! - `entities.json`: every entity type with its category and size;
//! - `synchronized.json`: the entries of every synchronised data-driven registry in the
//!   version's built-in data pack (what a client of that version knows by name).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::{Map, Value};

use crate::vanilla::{self, Extracted};
use crate::{Result, normalize, read_json, write};

/// Writes the multi-version data of an extracted version to `out`: from the data generator's
/// reports and the extractor's entity types, synchronised registries and packet shapes (the
/// multi-version or the full extractor; both write these files the same way).
pub fn keep(x: &Extracted, out: &Path) -> Result<()> {
    let reports = x.generated.join("reports");
    vanilla::remove_dir(out)?;
    write(&out.join("version.toml"), &x.info.to_toml())?;
    let registries = normalize::registries(&read_json(&reports.join("registries.json"))?)?;
    write(&out.join("registries.json"), &pretty(&registries)?)?;
    // Versions before 1.21 have no packet report; the extractor reads the same ids.
    let packet_report = if reports.join("packets.json").exists() {
        reports.join("packets.json")
    } else {
        x.extracted.join("packets.json")
    };
    write(
        &out.join("packets.json"),
        &pretty(&normalize::packets(&read_json(&packet_report)?)?)?,
    )?;
    write(
        &out.join("blocks.json"),
        &compact_blocks(&read_json(&reports.join("blocks.json"))?, &registries)?,
    )?;
    write(
        &out.join("entities.json"),
        &compact_entities(&read_json(&x.extracted.join("entities.json"))?["entity_types"])?,
    )?;
    write(
        &out.join("packet_fields.json"),
        &compact_object(&read_json(&x.extracted.join("packet_fields.json"))?)?,
    )?;
    write(
        &out.join("entity_data.json"),
        &compact_entity_data(&read_json(&x.extracted.join("entity_data.json"))?)?,
    )?;
    let dynamic = read_json(&x.extracted.join("registries.json"))?;
    write(
        &out.join("synchronized.json"),
        &synchronized(&x.inner_jar, &dynamic)?,
    )?;
    Ok(())
}

fn pretty(v: &Value) -> Result<String> {
    let mut s = serde_json::to_string_pretty(v).map_err(|e| e.to_string())?;
    s.push('\n');
    Ok(s)
}

/// Entity data layouts: the serializers, then one line per entity type.
pub fn compact_entity_data(v: &Value) -> Result<String> {
    let types = compact_object(&v["types"])?;
    Ok(format!(
        "{{\n\"serializers\":{},\n\"types\":{}}}\n",
        v["serializers"],
        types.trim_end()
    ))
}

/// One line per key.
pub fn compact_object(v: &Value) -> Result<String> {
    let o = v.as_object().ok_or("expected an object")?;
    let lines: Vec<String> = o
        .iter()
        .map(|(k, v)| format!("{}:{v}", Value::from(k.as_str())))
        .collect();
    Ok(format!("{{\n{}\n}}\n", lines.join(",\n")))
}

/// One line per block: name, first and default state, properties (by name; the last varies
/// fastest in the state ids), from the data generator's block report.
fn compact_blocks(report: &Value, registries: &Value) -> Result<String> {
    let report = report
        .as_object()
        .ok_or("blocks.json: expected an object")?;
    let mut lines = Vec::new();
    for name in registries["minecraft:block"]["entries"]
        .as_array()
        .ok_or("registries: no blocks")?
    {
        let name = name.as_str().ok_or("block name")?;
        let b = report
            .get(name)
            .ok_or_else(|| format!("{name} not in blocks.json"))?;
        let mut props: Vec<(&String, &Value)> = b["properties"]
            .as_object()
            .map(|p| p.iter().collect())
            .unwrap_or_default();
        props.sort_by_key(|p| p.0);
        let states = b["states"].as_array().ok_or("block states")?;
        let first = states
            .iter()
            .filter_map(|s| s["id"].as_u64())
            .min()
            .ok_or("block states")?;
        let default = states
            .iter()
            .find(|s| s["default"].as_bool() == Some(true))
            .and_then(|s| s["id"].as_u64())
            .ok_or_else(|| format!("{name}: no default state"))?;
        let mut o = Map::new();
        o.insert("name".to_owned(), name.into());
        o.insert("first_state".to_owned(), first.into());
        o.insert("default_state".to_owned(), default.into());
        o.insert(
            "properties".to_owned(),
            props
                .iter()
                .map(|(n, v)| {
                    let mut p = Map::new();
                    p.insert("name".to_owned(), n.as_str().into());
                    p.insert("values".to_owned(), (*v).clone());
                    Value::Object(p)
                })
                .collect(),
        );
        lines.push(Value::Object(o).to_string());
    }
    Ok(format!("[\n{}\n]\n", lines.join(",\n")))
}

/// One line per entity type: name, category, size.
fn compact_entities(entities: &Value) -> Result<String> {
    let mut lines = Vec::new();
    for e in entities
        .as_array()
        .ok_or("entities.json: expected an array")?
    {
        let mut o = Map::new();
        for key in ENTITY_FACTS {
            o.insert((*key).to_owned(), e[*key].clone());
        }
        lines.push(Value::Object(o).to_string());
    }
    Ok(format!("[\n{}\n]\n", lines.join(",\n")))
}

/// Entity type facts that must match for a fallback type.
pub(crate) const ENTITY_FACTS: &[&str] = &["name", "category", "width", "height", "eye_height"];

/// Entry names of the synchronised registries in the jar's built-in data pack.
fn synchronized(inner_jar: &Path, dynamic: &Value) -> Result<String> {
    let registries: Vec<&str> = dynamic["synchronized"]
        .as_array()
        .ok_or("dynamic_registries.json: no synchronized list")?
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let file =
        std::fs::File::open(inner_jar).map_err(|e| format!("{}: {e}", inner_jar.display()))?;
    let zip = zip::ZipArchive::new(file).map_err(|e| format!("{}: {e}", inner_jar.display()))?;
    let mut out: BTreeMap<&str, BTreeSet<String>> =
        registries.iter().map(|r| (*r, BTreeSet::new())).collect();
    for name in zip.file_names() {
        let Ok(name) = name else { continue };
        let Some(rest) = name
            .strip_prefix("data/minecraft/")
            .and_then(|r| r.strip_suffix(".json"))
        else {
            continue;
        };
        for r in &registries {
            let path = r.trim_start_matches("minecraft:");
            if let Some(entry) = rest.strip_prefix(path).and_then(|e| e.strip_prefix('/'))
                && let Some(set) = out.get_mut(r)
            {
                set.insert(format!("minecraft:{entry}"));
            }
        }
    }
    let mut o = Map::new();
    for (r, entries) in out {
        o.insert(
            r.to_owned(),
            Value::Array(entries.into_iter().map(Value::from).collect()),
        );
    }
    let mut s = serde_json::to_string_pretty(&Value::Object(o)).map_err(|e| e.to_string())?;
    s.push('\n');
    Ok(s)
}

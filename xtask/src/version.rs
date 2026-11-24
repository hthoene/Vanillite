//! `cargo xtask version check|update`: switching Vanillite to a new Minecraft release.
//!
//! `check` compares the pinned version (`data/vanilla/version.toml`) with the latest release in
//! Mojang's version manifest. `update <id>` runs the whole data pipeline for another version
//! (release or pre-release) in a separate git worktree `target/version/<id>/tree/` (data,
//! generated code, goldens, test run) and writes a change report that lists what changed and
//! which code areas need work. `--final` does the same in this checkout, for the real switch.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Map, Value};

use crate::normalize::{self, Snapshot};
use crate::vanilla::{self, Extracted, str_at};
use crate::{Result, read, read_json, write};

/// Repository golden directories, relative to the root, and the code they check.
const GOLDEN_DIRS: &[(&str, &str)] = &[
    (
        "crates/vanillite-worldgen/tests/goldens",
        "vanillite-worldgen (world generation)",
    ),
    (
        "crates/vanillite-test/goldens/data",
        "vanillite-data / vanillite-datapack (tables, registries, data pack)",
    ),
    (
        "crates/vanillite-test/goldens/scenarios",
        "vanillite-game / vanillite-world (gameplay scenarios)",
    ),
];

/// The longest name list printed in full.
const MAX_NAMES: usize = 60;

fn pinned(root: &Path) -> Result<String> {
    let version: toml::Table = read(&root.join("data/vanilla/version.toml"))?
        .parse()
        .map_err(|e| format!("version.toml: {e}"))?;
    version
        .get("id")
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .ok_or_else(|| "version.toml: `id` is missing".to_owned())
}

fn release_time<'a>(manifest: &'a Value, id: &str) -> Option<&'a str> {
    manifest["versions"]
        .as_array()?
        .iter()
        .find(|v| v["id"] == id)?["releaseTime"]
        .as_str()
}

/// `cargo xtask version check`: fails when Mojang has released a newer version than the pinned
/// one. In GitHub Actions it also sets the step output `newer=<id>`.
pub fn check(root: &Path) -> Result<()> {
    let pinned = pinned(root)?;
    let manifest = vanilla::manifest()?;
    let latest = str_at(&manifest, &["latest", "release"])?;
    let snapshot = str_at(&manifest, &["latest", "snapshot"])?;
    let pinned_time = release_time(&manifest, &pinned).unwrap_or_default();
    let latest_time = release_time(&manifest, latest).unwrap_or_default();
    if latest != pinned && latest_time > pinned_time {
        if let Some(out) = std::env::var_os("GITHUB_OUTPUT") {
            use std::io::Write as _;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(&out)
                .map_err(|e| format!("$GITHUB_OUTPUT: {e}"))?;
            writeln!(f, "newer={latest}").map_err(|e| format!("$GITHUB_OUTPUT: {e}"))?;
        }
        return Err(format!(
            "Minecraft {latest} was released {latest_time}; Vanillite is pinned to {pinned}.\n\
             Prepare the switch with `cargo xtask version update {latest}` (change report) and \
             switch with `cargo xtask version update {latest} --final`."
        ));
    }
    println!("version: pinned {pinned} is the latest release");
    if snapshot != pinned && release_time(&manifest, snapshot).unwrap_or_default() > pinned_time {
        println!(
            "version: newest snapshot/pre-release is {snapshot} \
             (preview with `cargo xtask version update {snapshot}`)"
        );
    }
    Ok(())
}

/// `cargo xtask version update <id> [--final] [--bulk] [--no-test]`.
pub fn update(root: &Path, args: &[&str]) -> Result<()> {
    let mut id = None;
    let (mut finalise, mut bulk, mut test) = (false, false, true);
    for a in args {
        match *a {
            "--final" => finalise = true,
            "--bulk" => bulk = true,
            "--no-test" => test = false,
            v if !v.starts_with('-') && id.is_none() => id = Some(v.to_owned()),
            _ => return Err(crate::USAGE.to_owned()),
        }
    }
    let id = id.ok_or("name the version: `cargo xtask version update <id>`")?;
    let pinned = pinned(root)?;
    let manifest = vanilla::manifest()?;
    let out_dir = root.join("target/version").join(&id);
    std::fs::create_dir_all(&out_dir).map_err(|e| format!("{}: {e}", out_dir.display()))?;

    // Both versions through the same (current) extractor, so their raw outputs compare.
    println!("version: extracting the pinned {pinned}");
    let old = vanilla::extract(root, &manifest, Some(&pinned), None)?;
    println!("version: extracting {id}");
    let new = vanilla::extract(root, &manifest, Some(&id), None)?;
    if finalise && new.kind != "release" {
        return Err(format!(
            "{id} is a {}; only a release can become the pinned version",
            new.kind
        ));
    }

    let tree = if finalise {
        root.to_path_buf()
    } else {
        worktree(root, &out_dir.join("tree"))?
    };
    let mut steps = Steps::default();

    // Data and generated code.
    let data = tree.join("data/vanilla");
    let before = Snapshot::load(&root.join("data/vanilla"));
    let data_ok = steps.run("normalise data/vanilla", || {
        normalize::run(
            &new.generated.join("reports"),
            &new.extracted,
            &new.info,
            &data,
        )
    }) && steps.run("codegen", || crate::codegen::run(&tree, false));
    let after = Snapshot::load(&data);

    // Goldens recorded with the new version, then the test suite against them.
    let goldens_ok = data_ok
        && steps.run("record goldens", || {
            let mut args = vec!["--all"];
            if bulk {
                args.push("--bulk");
            }
            crate::golden::regen(&tree, &args)
        });
    let tests = if data_ok && test {
        Some(run_tests(&tree, &out_dir.join("test.log"), bulk)?)
    } else {
        None
    };

    let mut r = String::new();
    let _ = writeln!(
        r,
        "# Minecraft {pinned} → {id}: change report\n\n\
         Generated by `cargo xtask version update {id}{}` from the server jars of both versions \
         (data generator reports, tools/extractor output, built-in data pack). Target: {} \
         ({}, released {}).{}\n",
        if finalise { " --final" } else { "" },
        id,
        new.kind,
        new.release_time,
        if finalise {
            String::from(" This checkout was switched to the new version.")
        } else {
            format!(
                " The pinned version was not changed; the new data, code and goldens are in `{}`.",
                rel(root, &tree)
            )
        }
    );
    r.push_str("## Pipeline steps\n\n");
    for (step, result) in &steps.0 {
        match result {
            Ok(()) => {
                let _ = writeln!(r, "- {step}: ok");
            }
            Err(e) => {
                let _ = writeln!(
                    r,
                    "- {step}: **failed**\n\n```text\n{}\n```\n",
                    head(e, 120)
                );
            }
        }
    }
    let _ = writeln!(
        r,
        "- mappings: {}",
        if new.mappings {
            "downloaded"
        } else {
            "none published (unobfuscated jar)"
        }
    );

    let mut areas = Areas::default();
    r.push_str("\n## Protocol and packet fields\n\n");
    packet_report(root, &old, &new, &mut r, &mut areas)?;
    r.push_str("\n## Data pack (built-in, `data/` in the server jar)\n\n");
    datapack_report(&old, &new, &mut r, &mut areas)?;
    r.push('\n');
    let data_report = normalize::change_report(&before, &after);
    data_areas(&data_report, &before, &after, &mut areas);
    // The data report's own headings go one level down.
    for line in data_report.lines().skip(1) {
        if line.starts_with('#') {
            r.push('#');
        }
        r.push_str(line);
        r.push('\n');
    }
    removed_names(root, &before, &after, &mut r, &mut areas)?;

    r.push_str("\n## Goldens\n\n");
    if data_ok {
        if !goldens_ok {
            r.push_str(
                "Recording failed in part (see the pipeline steps): goldens that were not \
                 recorded keep the pinned version's content, and the tests below compare against \
                 that.\n\n",
            );
        }
        golden_report(root, &tree, finalise, &mut r, &mut areas)?;
    } else {
        r.push_str("not recorded (see the pipeline steps)\n");
    }
    for (step, result) in &steps.0 {
        if let Err(e) = result {
            for line in e.lines().take(2) {
                areas.add(
                    "tools/extractor (golden harness), xtask",
                    format!("step `{step}`: {line}"),
                );
            }
        }
    }
    r.push_str("\n## Build and tests against the new data\n\n");
    match &tests {
        Some(t) => t.report(root, &tree, &mut r, &mut areas),
        None => r.push_str("not run\n"),
    }

    r.push_str("\n## Affected code areas\n\n");
    areas.report(&mut r);

    let report = out_dir.join("change-report.md");
    write(&report, &r)?;
    println!("{r}");
    println!("change report: {}", report.display());
    match steps.0.iter().find(|s| s.1.is_err()) {
        Some((step, _)) => Err(format!(
            "step `{step}` failed; the report says what is missing"
        )),
        None => Ok(()),
    }
}

#[derive(Default)]
struct Steps(Vec<(&'static str, Result<()>)>);

impl Steps {
    fn run(&mut self, name: &'static str, f: impl FnOnce() -> Result<()>) -> bool {
        println!("version: {name}");
        let result = f();
        let ok = result.is_ok();
        self.0.push((name, result));
        ok
    }
}

/// Code areas that need work, with the reasons.
#[derive(Default)]
struct Areas(BTreeMap<String, BTreeSet<String>>);

impl Areas {
    fn add(&mut self, area: &str, why: impl Into<String>) {
        self.0
            .entry(area.to_owned())
            .or_default()
            .insert(why.into());
    }

    fn report(&self, r: &mut String) {
        if self.0.is_empty() {
            r.push_str("none found\n");
            return;
        }
        r.push_str("| Area | Why |\n|---|---|\n");
        for (area, why) in &self.0 {
            let mut list: Vec<String> = why
                .iter()
                .take(12)
                .map(|w| {
                    let short: String = w.chars().take(160).collect();
                    if short.len() < w.len() {
                        format!("{short}…")
                    } else {
                        short
                    }
                })
                .collect();
            if why.len() > 12 {
                list.push(format!("… and {} more", why.len() - 12));
            }
            let _ = writeln!(r, "| {area} | {} |", list.join("<br>").replace('|', "\\|"));
        }
    }
}

/// A clean detached worktree of `HEAD` at `dir`; an existing one is reset (its `target/` stays
/// as build cache). Its `target/vanilla` points to ours, so jars are not fetched twice.
fn worktree(root: &Path, dir: &Path) -> Result<PathBuf> {
    let git =
        |args: &[&str], cwd: &Path| vanilla::run(Command::new("git").current_dir(cwd).args(args));
    if dir.join(".git").exists() {
        git(&["checkout", "--quiet", "--force", "--detach", "HEAD"], dir)?;
        let head = String::from_utf8(
            Command::new("git")
                .current_dir(root)
                .args(["rev-parse", "HEAD"])
                .output()
                .map_err(|e| format!("git: {e}"))?
                .stdout,
        )
        .map_err(|e| e.to_string())?;
        git(
            &["checkout", "--quiet", "--force", "--detach", head.trim()],
            dir,
        )?;
        git(&["clean", "--quiet", "-fd"], dir)?;
    } else {
        let _ = Command::new("git")
            .current_dir(root)
            .args(["worktree", "prune"])
            .status();
        let d = dir.to_str().ok_or("worktree path is not UTF-8")?;
        git(
            &[
                "worktree", "add", "--quiet", "--detach", "--force", d, "HEAD",
            ],
            root,
        )?;
    }
    let link = dir.join("target/vanilla");
    if !link.exists() {
        std::fs::create_dir_all(dir.join("target")).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("target/vanilla"), &link)
            .map_err(|e| format!("{}: {e}", link.display()))?;
    }
    Ok(dir.to_path_buf())
}

fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root).unwrap_or(p).display().to_string()
}

fn head(s: &str, lines: usize) -> String {
    let mut out: Vec<&str> = s.lines().take(lines).collect();
    let more = s.lines().count().saturating_sub(lines);
    let note = format!("… {more} more lines");
    if more > 0 {
        out.push(&note);
    }
    out.join("\n")
}

fn names_list(names: &[String]) -> String {
    let mut shown: Vec<String> = names
        .iter()
        .take(MAX_NAMES)
        .map(|n| format!("`{n}`"))
        .collect();
    if names.len() > MAX_NAMES {
        shown.push(format!("… and {} more", names.len() - MAX_NAMES));
    }
    shown.join(", ")
}

// ---------------------------------------------------------------------------------------------
// Packets

fn packet_report(
    root: &Path,
    old: &Extracted,
    new: &Extracted,
    r: &mut String,
    areas: &mut Areas,
) -> Result<()> {
    let proto = |x: &Extracted| x.info.protocol;
    let _ = writeln!(r, "- protocol version: {} → {}", proto(old), proto(new));
    if proto(old) != proto(new) {
        areas.add(
            "vanillite-protocol, vanillite-data (generated version)",
            "protocol version number changed: clients of the old version are refused",
        );
        areas.add(
            "vanillite-proxy, tools/loadbot",
            "protocol version number changed (forwarding/handshake, test bots)",
        );
    }
    let old_p = read_json(&old.extracted.join("packet_fields.json"))?;
    let new_p = read_json(&new.extracted.join("packet_fields.json"))?;
    let empty = Map::new();
    let (o, n) = (
        old_p.as_object().unwrap_or(&empty),
        new_p.as_object().unwrap_or(&empty),
    );
    let structs = packet_structs(root)?;
    let added: Vec<String> = n.keys().filter(|k| !o.contains_key(*k)).cloned().collect();
    let removed: Vec<String> = o.keys().filter(|k| !n.contains_key(*k)).cloned().collect();
    let _ = writeln!(
        r,
        "- packet types added ({}): {}\n- packet types removed ({}): {}",
        added.len(),
        names_list(&added),
        removed.len(),
        names_list(&removed)
    );
    for k in &removed {
        if let Some(at) = structs.get(packet_name(k)) {
            areas.add(
                "vanillite-protocol (packets)",
                format!("packet `{k}` removed, still implemented at {at}"),
            );
        }
    }
    if !added.is_empty() {
        areas.add(
            "vanillite-protocol (packets)",
            format!("{} new packet type(s) to implement", added.len()),
        );
    }
    r.push_str(
        "\nChanged packet fields (Java record/field shape of the packet class; a changed field \
         almost always means a changed codec):\n\n",
    );
    let mut any = false;
    for (k, nv) in n {
        let Some(ov) = o.get(k) else { continue };
        let fields = |v: &Value| -> Vec<String> {
            v["fields"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|f| f.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default()
        };
        let (of, nf) = (fields(ov), fields(nv));
        if of == nf && ov["class"] == nv["class"] {
            continue;
        }
        any = true;
        let rust = structs.get(packet_name(k));
        let _ = writeln!(
            r,
            "- `{k}` ({}){}\n\n  ```diff",
            nv["class"].as_str().unwrap_or_default(),
            rust.map_or(String::from(", not implemented in Vanillite"), |at| {
                format!(", Vanillite: {at}")
            })
        );
        if ov["class"] != nv["class"] {
            let _ = writeln!(
                r,
                "  - class {}\n  + class {}",
                ov["class"].as_str().unwrap_or_default(),
                nv["class"].as_str().unwrap_or_default()
            );
        }
        for f in &of {
            let _ = writeln!(r, "  {} {f}", if nf.contains(f) { " " } else { "-" });
        }
        for f in nf.iter().filter(|f| !of.contains(f)) {
            let _ = writeln!(r, "  + {f}");
        }
        r.push_str("  ```\n");
        if let Some(at) = rust {
            areas.add(
                "vanillite-protocol (packets)",
                format!("fields of `{k}` changed ({at})"),
            );
        }
    }
    if !any {
        r.push_str("none\n");
    }
    Ok(())
}

/// `clientbound/minecraft:add_entity` → `add_entity`.
fn packet_name(key: &str) -> &str {
    key.rsplit(':').next().unwrap_or(key)
}

/// Packet names (`#[packet(... name = "x")]`) of vanillite-protocol → `file:line`. A name used in
/// both directions maps to the first one; good enough to point at the code.
fn packet_structs(root: &Path) -> Result<BTreeMap<String, String>> {
    let mut files = Vec::new();
    vanilla::collect_files(
        &root.join("crates/vanillite-protocol/src"),
        "rs",
        &mut files,
    )?;
    files.sort();
    let mut out = BTreeMap::new();
    for f in files {
        for (i, line) in read(&f)?.lines().enumerate() {
            if let Some(rest) = line.trim().strip_prefix("#[packet(")
                && let Some(name) = rest.split("name = \"").nth(1)
                && let Some(name) = name.split('"').next()
            {
                out.entry(name.to_owned())
                    .or_insert_with(|| format!("`{}:{}`", rel(root, &f), i + 1));
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Built-in data pack

/// `data/**` and `assets/**` entries of a jar → CRC-32.
fn jar_data(jar: &Path) -> Result<BTreeMap<String, u32>> {
    let file = std::fs::File::open(jar).map_err(|e| format!("{}: {e}", jar.display()))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("{}: {e}", jar.display()))?;
    let mut out = BTreeMap::new();
    for i in 0..zip.len() {
        let e = zip
            .by_index_raw(i)
            .map_err(|e| format!("{}: {e}", jar.display()))?;
        let name = e
            .name()
            .map_err(|e| format!("{}: {e}", jar.display()))?
            .into_owned();
        if (name.starts_with("data/") || name.starts_with("assets/")) && !name.ends_with('/') {
            out.insert(name, e.crc32());
        }
    }
    Ok(out)
}

/// `data/minecraft/worldgen/noise_settings/overworld.json` → (`worldgen/noise_settings`,
/// `overworld`).
fn datapack_group(path: &str) -> (String, String) {
    let parts: Vec<&str> = path.split('/').collect();
    let (body, ns) = match parts.as_slice() {
        ["data", ns, rest @ ..] => (rest.to_vec(), *ns),
        ["assets", ns, rest @ ..] => (rest.to_vec(), *ns),
        _ => (parts.clone(), ""),
    };
    let depth = match body.first() {
        Some(&("worldgen" | "tags" | "loot_table" | "structure" | "datapacks" | "advancement")) => {
            2
        }
        _ => 1,
    }
    .min(body.len().saturating_sub(1));
    let group = body[..depth].join("/");
    let name = body[depth..].join("/");
    let name = name.strip_suffix(".json").unwrap_or(&name).to_owned();
    let group = if ns == "minecraft" || ns.is_empty() {
        group
    } else {
        format!("{ns}:{group}")
    };
    if path.starts_with("assets/") {
        (format!("assets/{group}"), name)
    } else {
        (group, name)
    }
}

fn datapack_report(
    old: &Extracted,
    new: &Extracted,
    r: &mut String,
    areas: &mut Areas,
) -> Result<()> {
    let (o, n) = (jar_data(&old.inner_jar)?, jar_data(&new.inner_jar)?);
    #[derive(Default)]
    struct Diff {
        added: Vec<String>,
        removed: Vec<String>,
        changed: Vec<String>,
    }
    let mut groups: BTreeMap<String, Diff> = BTreeMap::new();
    for path in o.keys().chain(n.keys()).collect::<BTreeSet<_>>() {
        let (group, name) = datapack_group(path);
        let d = match (o.get(path), n.get(path)) {
            (None, Some(_)) => &mut groups.entry(group).or_default().added,
            (Some(_), None) => &mut groups.entry(group).or_default().removed,
            (Some(a), Some(b)) if a != b => &mut groups.entry(group).or_default().changed,
            _ => continue,
        };
        d.push(name);
    }
    if groups.is_empty() {
        r.push_str("no changes\n");
        return Ok(());
    }
    r.push_str("| Group | Added | Removed | Changed |\n|---|---|---|---|\n");
    for (g, d) in &groups {
        let _ = writeln!(
            r,
            "| `{g}` | {} | {} | {} |",
            d.added.len(),
            d.removed.len(),
            d.changed.len()
        );
    }
    r.push('\n');
    for (g, d) in &groups {
        let _ = writeln!(r, "### `{g}`\n");
        for (what, list) in [
            ("added", &d.added),
            ("removed", &d.removed),
            ("changed", &d.changed),
        ] {
            if !list.is_empty() {
                let _ = writeln!(r, "- {what} ({}): {}", list.len(), names_list(list));
            }
        }
        r.push('\n');
        let area = if g.starts_with("worldgen") || g.starts_with("dimension") {
            "vanillite-worldgen, vanillite-datapack (worldgen settings)"
        } else if g.starts_with("tags") {
            "vanillite-datapack (tags), vanillite-game (tag users)"
        } else if g.starts_with("recipe")
            || g.starts_with("loot_table")
            || g.starts_with("advancement")
        {
            "vanillite-datapack, vanillite-game (recipes, loot, advancements)"
        } else if g.starts_with("structure") {
            "vanillite-worldgen (structure templates)"
        } else if g.starts_with("assets/") {
            "vanillite-protocol (translations / text)"
        } else if g.starts_with("datapacks") {
            "vanillite-datapack (built-in feature packs)"
        } else {
            "vanillite-datapack (data-driven registries, registry sync)"
        };
        areas.add(
            area,
            format!(
                "data pack `{g}`: +{} −{} ~{}",
                d.added.len(),
                d.removed.len(),
                d.changed.len()
            ),
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Data tables

/// Areas from the `data/vanilla` change report sections.
fn data_areas(report: &str, before: &Snapshot, after: &Snapshot, areas: &mut Areas) {
    let mut section = "";
    for line in report.lines() {
        if let Some(h) = line.strip_prefix("## ") {
            section = h;
            continue;
        }
        let changed =
            (line.starts_with("- ") && !line.contains("(0)") && !line.contains("unchanged"))
                || (line.starts_with("- `") && section == "Registries");
        if !changed {
            continue;
        }
        let what = line.trim_start_matches("- ");
        let area = match section {
            "Version" => continue,
            "Registries" => {
                let reg = what.split('`').nth(1).unwrap_or_default();
                match reg {
                    "minecraft:block" => {
                        "vanillite-data, vanillite-chunk, vanillite-game (block behaviours)"
                    }
                    "minecraft:item" | "minecraft:data_component_type" => {
                        "vanillite-game (items), vanillite-protocol (item stack/component codecs)"
                    }
                    "minecraft:entity_type" | "minecraft:attribute" | "minecraft:mob_effect" => {
                        "vanillite-game (entities)"
                    }
                    "minecraft:game_rule" => "vanillite-game (game rules), vanillite-config",
                    r if r.contains("argument") || r.contains("command") => "vanillite-command",
                    r if r.contains("worldgen")
                        || r.contains("feature")
                        || r.contains("carver") =>
                    {
                        "vanillite-worldgen"
                    }
                    "minecraft:menu" | "minecraft:recipe_type" | "minecraft:recipe_serializer" => {
                        "vanillite-game (menus, recipes)"
                    }
                    "minecraft:sound_event" | "minecraft:particle_type" => {
                        "vanillite-game (sounds, particles)"
                    }
                    _ => "vanillite-data (generated registries)",
                }
            }
            "Packets" => "vanillite-protocol (packet ids)",
            s if s.starts_with("Blocks") => {
                "vanillite-data (block tables), vanillite-game (block behaviours), vanillite-light"
            }
            "Items" => "vanillite-game (items, components)",
            "Entity types" | "Attributes" => "vanillite-game (entities)",
            s if s.starts_with("Data-driven") => "vanillite-datapack (registry loading and sync)",
            "Other" => {
                if what.contains("command") {
                    "vanillite-command"
                } else if what.contains("biome") {
                    "vanillite-worldgen (biome source)"
                } else if what.contains("fluid") {
                    "vanillite-game (fluids)"
                } else {
                    "vanillite-data (block tables), vanillite-game (block behaviours)"
                }
            }
            _ => continue,
        };
        let short: String = what.chars().take(140).collect();
        areas.add(area, format!("{section}: {short}"));
    }
    if before.data_version() != after.data_version() {
        areas.add(
            "vanillite-storage",
            format!(
                "data version {} → {}: chunk/player/level.dat format may need upgrading",
                before.data_version(),
                after.data_version()
            ),
        );
    }
}

/// Removed or renamed registry entries that hand-written code still names.
fn removed_names(
    root: &Path,
    before: &Snapshot,
    after: &Snapshot,
    r: &mut String,
    areas: &mut Areas,
) -> Result<()> {
    let removed = before.removed_entries(after);
    r.push_str("\n### Removed entries still named in the code\n\n");
    if removed.is_empty() {
        r.push_str("nothing removed\n");
        return Ok(());
    }
    let mut files = Vec::new();
    vanilla::collect_files(&root.join("crates"), "rs", &mut files)?;
    files.retain(|f| !f.components().any(|c| c.as_os_str() == "generated"));
    files.sort();
    let sources: Vec<(PathBuf, String)> = files
        .into_iter()
        .filter_map(|f| Some((read(&f).ok()?, f)))
        .map(|(text, f)| (f, text))
        .collect();
    let mut any = false;
    for (registry, name) in &removed {
        // String literals only: identifiers from the generated tables fail the build instead.
        let mut forms = vec![format!("\"{name}\"")];
        if let Some(path) = name.strip_prefix("minecraft:") {
            forms.push(format!("\"{path}\""));
        }
        let hits: Vec<String> = sources
            .iter()
            .filter(|(_, text)| forms.iter().any(|f| text.contains(f.as_str())))
            .map(|(f, _)| format!("`{}`", rel(root, f)))
            .collect();
        if hits.is_empty() {
            continue;
        }
        any = true;
        let _ = writeln!(r, "- `{name}` ({registry}): {}", hits.join(", "));
        for h in hits {
            let krate = h
                .trim_matches('`')
                .split('/')
                .nth(1)
                .unwrap_or("crates")
                .to_owned();
            areas.add(&krate, format!("names removed `{name}` ({h})"));
        }
    }
    if !any {
        r.push_str("none\n");
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Goldens

fn golden_report(
    root: &Path,
    tree: &Path,
    finalise: bool,
    r: &mut String,
    areas: &mut Areas,
) -> Result<()> {
    if finalise {
        r.push_str("recorded in place; `git diff --stat` shows which goldens changed\n");
        return Ok(());
    }
    for (dir, area) in GOLDEN_DIRS {
        let list = |base: &Path| -> BTreeMap<String, String> {
            let mut files = Vec::new();
            let _ = vanilla::collect_files(&base.join(dir), "json", &mut files);
            files
                .into_iter()
                .filter_map(|f| {
                    let name = f.strip_prefix(base.join(dir)).ok()?.display().to_string();
                    Some((name, read(&f).ok()?))
                })
                .collect()
        };
        let (o, n) = (list(root), list(tree));
        let changed: Vec<String> = n
            .iter()
            .filter(|(k, v)| o.get(*k) != Some(*v))
            .map(|(k, _)| k.trim_end_matches(".json").to_owned())
            .collect();
        let gone: Vec<String> = o
            .keys()
            .filter(|k| !n.contains_key(*k))
            .map(|k| k.trim_end_matches(".json").to_owned())
            .collect();
        let _ = writeln!(
            r,
            "- `{dir}`: {} of {} changed{}{}",
            changed.len(),
            n.len(),
            if changed.is_empty() {
                String::new()
            } else {
                format!(": {}", names_list(&changed))
            },
            if gone.is_empty() {
                String::new()
            } else {
                format!("; not recorded: {}", names_list(&gone))
            }
        );
        if !changed.is_empty() {
            areas.add(
                area,
                format!(
                    "Vanilla output changed for golden(s) {}",
                    names_list(&changed)
                ),
            );
        }
    }
    let _ = writeln!(
        r,
        "\nThe new goldens are in `{}` (same paths); bulk goldens (with `--bulk`) under its \
         `target/goldens/`.",
        rel(root, tree)
    );
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Tests

struct TestRun {
    log: PathBuf,
    success: bool,
    /// `file:line:col: error...` lines.
    compile_errors: Vec<String>,
    /// Crates that did not compile.
    broken: BTreeSet<String>,
    /// Crates whose tests did not run because a dependency did not compile.
    blocked: BTreeMap<String, String>,
    /// (test binary, test name).
    failed: Vec<(String, String)>,
    passed: usize,
}

fn run_tests(tree: &Path, log: &Path, bulk: bool) -> Result<TestRun> {
    println!("version: cargo test --workspace (log: {})", log.display());
    std::fs::File::create(log).map_err(|e| format!("{}: {e}", log.display()))?;
    let mut run = TestRun {
        log: log.to_path_buf(),
        success: true,
        compile_errors: Vec::new(),
        broken: BTreeSet::new(),
        blocked: BTreeMap::new(),
        failed: Vec::new(),
        passed: 0,
    };
    let broken = run.cargo_test(tree, &["--workspace"], bulk)?;
    if !broken.is_empty() {
        // Cargo stops at the first crate that does not compile; test the others one by one.
        let meta = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned()))
            .current_dir(tree)
            .args(["metadata", "--no-deps", "--format-version", "1"])
            .output()
            .map_err(|e| format!("cargo metadata: {e}"))?;
        let meta: Value =
            serde_json::from_slice(&meta.stdout).map_err(|e| format!("cargo metadata: {e}"))?;
        let members: Vec<String> = meta["packages"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|p| p["name"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        for m in members {
            println!("version: cargo test -p {m}");
            let broken = run.cargo_test(tree, &["-p", &m], bulk)?;
            if let Some(b) = broken.iter().find(|b| **b != m) {
                run.blocked.insert(m, b.clone());
            }
        }
    }
    run.compile_errors.sort();
    run.compile_errors.dedup();
    run.failed.sort();
    run.failed.dedup();
    Ok(run)
}

impl TestRun {
    /// One `cargo test` run, appended to the log; returns the crates that did not compile.
    fn cargo_test(&mut self, tree: &Path, which: &[&str], bulk: bool) -> Result<Vec<String>> {
        let start = read(&self.log)?.len();
        let file = std::fs::OpenOptions::new()
            .append(true)
            .open(&self.log)
            .map_err(|e| format!("{}: {e}", self.log.display()))?;
        let err = file.try_clone().map_err(|e| e.to_string())?;
        let mut cmd = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned()));
        cmd.current_dir(tree)
            .arg("test")
            .args(which)
            .args([
                "--all-features",
                "--no-fail-fast",
                "--message-format",
                "short",
                "--",
            ])
            .stdout(Stdio::from(file))
            .stderr(Stdio::from(err));
        if bulk {
            cmd.arg("--include-ignored");
        }
        let status = cmd.status().map_err(|e| format!("cargo: {e}"))?;
        self.success &= status.success();
        let text = read(&self.log)?;
        let mut broken = Vec::new();
        let mut binary = String::new();
        for line in text[start..].lines() {
            let t = line.trim();
            if let Some(rest) = t.strip_prefix("Running ") {
                // `Running tests/parity.rs (target/debug/deps/parity-0123abcd)`
                let target = rest.split(" (").next().unwrap_or(rest);
                let dep = rest
                    .rsplit('/')
                    .next()
                    .unwrap_or_default()
                    .trim_end_matches(')');
                let dep = dep.rsplit_once('-').map_or(dep, |d| d.0);
                binary = format!("{dep} {target}");
            } else if let Some(rest) = t.strip_prefix("Doc-tests ") {
                binary = format!("{rest} doc-tests");
            } else if let Some(name) = t
                .strip_prefix("test ")
                .and_then(|s| s.strip_suffix(" ... FAILED"))
            {
                self.failed.push((binary.clone(), name.to_owned()));
            } else if let Some(rest) = t.strip_prefix("test result: ") {
                self.passed += rest
                    .split("; ")
                    .find_map(|p| p.strip_suffix(" passed"))
                    .and_then(|n| n.rsplit(' ').next())
                    .and_then(|n| n.parse::<usize>().ok())
                    .unwrap_or(0);
            } else if let Some(rest) = t.strip_prefix("error: could not compile `") {
                let krate = rest.split('`').next().unwrap_or_default().to_owned();
                self.broken.insert(krate.clone());
                broken.push(krate);
            } else if line.contains(": error") && line.contains(".rs:") {
                self.compile_errors.push(line.to_owned());
            }
        }
        Ok(broken)
    }

    fn report(&self, root: &Path, tree: &Path, r: &mut String, areas: &mut Areas) {
        let _ = writeln!(
            r,
            "`cargo test --workspace --all-features --no-fail-fast` in `{}`: {}, {} passed, {} \
             failed, {} crate(s) did not compile. Full log: `{}`.\n",
            rel(root, tree),
            if self.success {
                "success"
            } else {
                "**failed**"
            },
            self.passed,
            self.failed.len(),
            self.broken.len(),
            rel(root, &self.log)
        );
        if !self.compile_errors.is_empty() {
            let _ = writeln!(
                r,
                "### Compile errors ({})\n\n```text",
                self.compile_errors.len()
            );
            for e in self.compile_errors.iter().take(80) {
                let _ = writeln!(r, "{e}");
            }
            if self.compile_errors.len() > 80 {
                let _ = writeln!(r, "… and {} more", self.compile_errors.len() - 80);
            }
            r.push_str("```\n\n");
            for e in &self.compile_errors {
                let krate = e.split('/').nth(1).unwrap_or("crates").to_owned();
                let file = e.split(':').next().unwrap_or_default();
                areas.add(&krate, format!("does not compile (`{file}`)"));
            }
        }
        for b in &self.broken {
            areas.add(b, "does not compile");
        }
        if !self.blocked.is_empty() {
            let _ = writeln!(r, "### Tests not run (a dependency does not compile)\n");
            for (m, b) in &self.blocked {
                let _ = writeln!(r, "- `{m}` (needs `{b}`)");
            }
            r.push('\n');
        }
        if !self.failed.is_empty() {
            let mut by_binary: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
            for (b, t) in &self.failed {
                by_binary.entry(b).or_default().push(t);
            }
            let _ = writeln!(r, "### Failing tests ({})\n", self.failed.len());
            for (b, tests) in &by_binary {
                let _ = writeln!(r, "- `{b}` ({}):", tests.len());
                for t in tests.iter().take(MAX_NAMES) {
                    let _ = writeln!(r, "  - `{t}`");
                }
                if tests.len() > MAX_NAMES {
                    let _ = writeln!(r, "  - … and {} more", tests.len() - MAX_NAMES);
                }
                let krate = b.split(' ').next().unwrap_or(b).replace('_', "-");
                areas.add(&krate, format!("{} failing test(s) in `{b}`", tests.len()));
            }
        }
    }
}

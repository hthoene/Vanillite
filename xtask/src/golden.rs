//! `cargo xtask golden check|regen|bless`: the parity goldens recorded with the Java harness in
//! `tools/extractor/src/dev/vanillite/extractor/golden/`.
//!
//! Golden storage (see `vanillite_test::golden`): every golden is keyed by (Minecraft version,
//! jar SHA-1, harness version, entry hash). Repository goldens live in directories with a
//! `KEYS.toml`; bulk scenario goldens live in `$VANILLITE_GOLDENS` (default `target/goldens`)
//! under a path that contains the whole key.
//!
//! Golden sets:
//! - `worldgen`: `golden/*Goldens.java` → `crates/vanillite-worldgen/tests/goldens/`
//! - `data`: `golden/data/*Goldens.java` → `crates/vanillite-test/goldens/data/`
//! - `scenarios`: `crates/vanillite-test/scenarios/*.json` (tier `fast`) →
//!   `crates/vanillite-test/goldens/scenarios/`; tier `bulk` → the bulk store.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha1::{Digest, Sha1};

use crate::vanilla::{
    collect_files, collect_jars, download, java_tool, remove_dir, run, sha1_file,
};
use crate::{Result, read, write};

const KEYS_FILE: &str = "KEYS.toml";
const GOLDEN_SRC: &str = "tools/extractor/src/dev/vanillite/extractor/golden";

/// The key parts shared by all goldens.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Key {
    minecraft: String,
    jar_sha1: String,
    jar_url: String,
    harness: u32,
}

impl Key {
    fn current(root: &Path) -> Result<Self> {
        let version: toml::Table = read(&root.join("data/vanilla/version.toml"))?
            .parse()
            .map_err(|e| format!("version.toml: {e}"))?;
        let field = |k: &str| {
            version
                .get(k)
                .and_then(|v| v.as_str())
                .map(str::to_owned)
                .ok_or_else(|| format!("version.toml: `{k}` is missing"))
        };
        let harness = read(&root.join("tools/extractor/harness-version"))?
            .trim()
            .parse()
            .map_err(|e| format!("tools/extractor/harness-version: {e}"))?;
        Ok(Self {
            minecraft: field("id")?,
            jar_sha1: field("server_jar_sha1")?,
            jar_url: field("server_jar_url")?,
            harness,
        })
    }

    fn bulk_dir(&self, root: &Path) -> PathBuf {
        let base = std::env::var_os("VANILLITE_GOLDENS")
            .map_or_else(|| root.join("target/goldens"), PathBuf::from);
        base.join(format!(
            "{}-{}-h{}",
            self.minecraft,
            &self.jar_sha1[..self.jar_sha1.len().min(12)],
            self.harness
        ))
    }
}

/// What records a golden.
#[derive(Clone, Debug)]
enum Source {
    /// A `Goldens` part class, relative to the golden package (`data.ItemsGoldens`).
    Part(String),
    /// A scenario file.
    Scenario(PathBuf),
}

/// One golden.
#[derive(Clone, Debug)]
struct Entry {
    name: String,
    hash: String,
    source: Source,
    out: PathBuf,
}

/// A repository golden directory and its entries; `dir` is `None` for the bulk store.
#[derive(Debug)]
struct Set {
    name: &'static str,
    dir: Option<PathBuf>,
    entries: Vec<Entry>,
}

fn text_sha1(path: &Path) -> Result<String> {
    let mut h = Sha1::new();
    h.update(read(path)?.replace('\r', "").as_bytes());
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// `FooBarGoldens` → `foo_bar`.
fn snake(class: &str) -> String {
    let base = class.strip_suffix("Goldens").unwrap_or(class);
    let mut s = String::new();
    for (i, c) in base.chars().enumerate() {
        if c.is_ascii_uppercase() && i > 0 {
            s.push('_');
        }
        s.push(c.to_ascii_lowercase());
    }
    s
}

fn part_set(root: &Path, name: &'static str, sub: &str, out: &str) -> Result<Set> {
    let src = root.join(GOLDEN_SRC).join(sub);
    let dir = root.join(out);
    let mut entries = Vec::new();
    let read_dir = std::fs::read_dir(&src).map_err(|e| format!("{}: {e}", src.display()))?;
    for file in read_dir {
        let path = file.map_err(|e| e.to_string())?.path();
        let Some(class) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if path.extension().is_none_or(|e| e != "java")
            || !class.ends_with("Goldens")
            || class == "Goldens"
            || class == "DataGoldens"
        {
            continue;
        }
        let golden = snake(class);
        entries.push(Entry {
            out: dir.join(format!("{golden}.json")),
            name: golden,
            hash: text_sha1(&path)?,
            source: Source::Part(if sub.is_empty() {
                class.to_owned()
            } else {
                format!("{sub}.{class}")
            }),
        });
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Set {
        name,
        dir: Some(dir),
        entries,
    })
}

fn sets(root: &Path, key: &Key) -> Result<Vec<Set>> {
    let mut fast = Vec::new();
    let mut bulk = Vec::new();
    let scenarios = root.join("crates/vanillite-test/scenarios");
    let fast_dir = root.join("crates/vanillite-test/goldens/scenarios");
    let bulk_dir = key.bulk_dir(root).join("scenarios");
    let mut files = Vec::new();
    collect_files(&scenarios, "json", &mut files)?;
    files.sort();
    for path in files {
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| format!("{}: bad name", path.display()))?
            .to_owned();
        let hash = text_sha1(&path)?;
        let json = crate::read_json(&path)?;
        let tier = json.get("tier").and_then(|t| t.as_str()).unwrap_or("fast");
        let profile = json
            .get("profile")
            .and_then(|t| t.as_str())
            .unwrap_or("vanilla");
        if profile != "vanilla" {
            // Only blessed goldens exist for other profiles.
            continue;
        }
        let entry = |out: PathBuf| Entry {
            name: name.clone(),
            hash: hash.clone(),
            source: Source::Scenario(path.clone()),
            out,
        };
        match tier {
            "fast" => fast.push(entry(fast_dir.join(format!("{name}.json")))),
            "bulk" => bulk.push(entry(bulk_dir.join(format!("{name}-{}.json", &hash[..12])))),
            t => return Err(format!("{}: unknown tier `{t}`", path.display())),
        }
    }
    Ok(vec![
        part_set(
            root,
            "worldgen",
            "",
            "crates/vanillite-worldgen/tests/goldens",
        )?,
        part_set(root, "data", "data", "crates/vanillite-test/goldens/data")?,
        Set {
            name: "scenarios",
            dir: Some(fast_dir),
            entries: fast,
        },
        Set {
            name: "bulk scenarios",
            dir: None,
            entries: bulk,
        },
    ])
}

/// `KEYS.toml` of a directory: the shared key and the entry hashes (empty if there is none).
fn read_keys(dir: &Path) -> Result<(Option<Key>, BTreeMap<String, String>)> {
    let path = dir.join(KEYS_FILE);
    if !path.exists() {
        return Ok((None, BTreeMap::new()));
    }
    let table: toml::Table = read(&path)?
        .parse()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let s = |k: &str| table.get(k).and_then(|v| v.as_str()).map(str::to_owned);
    let key = match (s("minecraft"), s("jar_sha1"), table.get("harness")) {
        (Some(minecraft), Some(jar_sha1), Some(h)) => Some(Key {
            minecraft,
            jar_sha1,
            jar_url: String::new(),
            harness: h
                .as_integer()
                .and_then(|h| u32::try_from(h).ok())
                .unwrap_or(0),
        }),
        _ => None,
    };
    let entries = table
        .get("entries")
        .and_then(|e| e.as_table())
        .map(|t| {
            t.iter()
                .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
                .collect()
        })
        .unwrap_or_default();
    Ok((key, entries))
}

fn write_keys(dir: &Path, key: &Key, entries: &BTreeMap<String, String>) -> Result<()> {
    let mut s =
        String::from("# Golden keys, written by `cargo xtask golden regen`. Do not edit.\n");
    let _ = writeln!(s, "minecraft = \"{}\"", key.minecraft);
    let _ = writeln!(s, "jar_sha1 = \"{}\"", key.jar_sha1);
    let _ = writeln!(s, "harness = {}\n\n[entries]", key.harness);
    for (name, hash) in entries {
        let _ = writeln!(s, "{name} = \"{hash}\"");
    }
    write(&dir.join(KEYS_FILE), &s)
}

fn same_key(a: &Key, b: &Key) -> bool {
    (&a.minecraft, &a.jar_sha1, a.harness) == (&b.minecraft, &b.jar_sha1, b.harness)
}

/// The entries of a set that are missing or stale, and names in `KEYS.toml` without a source.
fn stale(set: &Set, key: &Key) -> Result<(Vec<Entry>, Vec<String>)> {
    let Some(dir) = &set.dir else {
        let missing = set.entries.iter().filter(|e| !e.out.exists()).cloned();
        return Ok((missing.collect(), Vec::new()));
    };
    let (stored, entries) = read_keys(dir)?;
    let current = stored.as_ref().is_some_and(|k| same_key(k, key));
    let stale = set
        .entries
        .iter()
        .filter(|e| !current || entries.get(&e.name) != Some(&e.hash) || !e.out.exists())
        .cloned()
        .collect();
    let orphans = entries
        .keys()
        .filter(|n| !set.entries.iter().any(|e| &e.name == *n))
        .cloned()
        .collect();
    Ok((stale, orphans))
}

/// `cargo xtask golden check`: every repository golden exists and has the current key. No Java.
pub fn check(root: &Path) -> Result<()> {
    let key = Key::current(root)?;
    let mut problems = Vec::new();
    for set in sets(root, &key)? {
        if set.dir.is_none() {
            continue;
        }
        let (stale, orphans) = stale(&set, &key)?;
        for e in stale {
            problems.push(format!(
                "{} golden `{}` is missing or stale",
                set.name, e.name
            ));
        }
        for o in orphans {
            problems.push(format!("{} golden `{o}` has no source any more", set.name));
        }
    }
    if problems.is_empty() {
        println!("goldens: all current");
        Ok(())
    } else {
        Err(format!(
            "{}\nrun `cargo xtask golden regen` (needs Java 25) and commit the result",
            problems.join("\n")
        ))
    }
}

/// `cargo xtask golden regen [--all] [--bulk] [<name>...]`.
pub fn regen(root: &Path, args: &[&str]) -> Result<()> {
    let all = args.contains(&"--all");
    let with_bulk = args.contains(&"--bulk");
    let names: Vec<&str> = args
        .iter()
        .copied()
        .filter(|a| !a.starts_with("--"))
        .collect();
    let named = |e: &Entry| {
        names.iter().any(|n| {
            *n == e.name
                || matches!(&e.source, Source::Part(c) if c == n || c.ends_with(&format!(".{n}")))
        })
    };

    let key = Key::current(root)?;
    let sets = sets(root, &key)?;
    let mut work: Vec<(&Set, Vec<Entry>)> = Vec::new();
    for set in &sets {
        if set.dir.is_none() && !with_bulk && names.is_empty() {
            continue;
        }
        let todo: Vec<Entry> = if !names.is_empty() {
            set.entries.iter().filter(|e| named(e)).cloned().collect()
        } else if all {
            set.entries.clone()
        } else {
            stale(set, &key)?.0
        };
        if !todo.is_empty() {
            work.push((set, todo));
        }
    }
    if work.is_empty() {
        println!("goldens: nothing to record (--all records everything)");
        return Ok(());
    }

    let (classes, cp) = harness(root, &key)?;
    let work_dir = root.join("target/vanilla").join(&key.minecraft);
    let sep = if cfg!(windows) { ";" } else { ":" };
    let java = |main: &str| {
        let mut cmd = Command::new(java_tool("java"));
        cmd.current_dir(&work_dir)
            .arg("-cp")
            .arg(format!("{}{sep}{cp}", classes.display()))
            .arg(main);
        cmd
    };
    for (set, todo) in &work {
        println!("goldens: recording {} {} golden(s)", todo.len(), set.name);
        let parts: Vec<&str> = todo
            .iter()
            .filter_map(|e| match &e.source {
                Source::Part(c) => Some(c.as_str()),
                Source::Scenario(_) => None,
            })
            .collect();
        if !parts.is_empty() {
            let out = set.dir.as_ref().ok_or("parts need a directory")?;
            run(java("dev.vanillite.extractor.golden.Goldens")
                .arg(out)
                .args(&parts))?;
        }
        let scenarios: Vec<&Entry> = todo
            .iter()
            .filter(|e| matches!(e.source, Source::Scenario(_)))
            .collect();
        // Every scenario starts its own Vanilla server; spread them over a few JVMs.
        let jvms = std::thread::available_parallelism()
            .map_or(1, |n| (n.get() / 4).max(1))
            .min(scenarios.len().div_ceil(4).max(1));
        let chunks: Vec<Vec<&Entry>> = (0..jvms)
            .map(|i| scenarios.iter().skip(i).step_by(jvms).copied().collect())
            .collect();
        std::thread::scope(|s| {
            let handles: Vec<_> = chunks
                .iter()
                .filter(|c| !c.is_empty())
                .map(|chunk| {
                    s.spawn(move || {
                        let mut cmd = java("dev.vanillite.extractor.golden.ScenarioRecorder");
                        for e in chunk {
                            if let Source::Scenario(p) = &e.source {
                                cmd.arg(p).arg(&e.out);
                            }
                        }
                        run(&mut cmd)
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| {
                    h.join()
                        .map_err(|_| "recorder thread panicked".to_owned())?
                })
                .collect::<Result<Vec<()>>>()
        })?;
        if let Some(dir) = &set.dir {
            let (stored, mut entries) = read_keys(dir)?;
            if !stored.is_some_and(|k| same_key(&k, &key)) {
                entries.clear();
            }
            entries.retain(|n, _| set.entries.iter().any(|e| &e.name == n));
            for e in todo {
                entries.insert(e.name.clone(), e.hash.clone());
            }
            write_keys(dir, &key, &entries)?;
        }
    }
    check(root)
}

/// Compiles the extractor against the pinned jar (downloaded and unpacked if needed); returns
/// the class directory and the jar class path.
fn harness(root: &Path, key: &Key) -> Result<(PathBuf, String)> {
    let work = root.join("target/vanilla").join(&key.minecraft);
    let inner = work
        .join("versions")
        .join(&key.minecraft)
        .join(format!("server-{}.jar", key.minecraft));
    if !inner.exists() {
        std::fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
        let jar = work.join("server.jar");
        if !jar.exists() || sha1_file(&jar)? != key.jar_sha1 {
            println!("downloading {}", key.jar_url);
            download(&key.jar_url, &jar)?;
        }
        let actual = sha1_file(&jar)?;
        if actual != key.jar_sha1 {
            return Err(format!(
                "server.jar SHA-1 mismatch: expected {}, got {actual}",
                key.jar_sha1
            ));
        }
        // The bundler unpacks the server jar and its libraries before it starts the main class.
        run(Command::new(java_tool("java"))
            .current_dir(&work)
            .arg("-DbundlerMainClass=net.minecraft.data.Main")
            .args(["-jar", "server.jar", "--help"]))?;
    }
    let mut classpath = vec![inner];
    collect_jars(&work.join("libraries"), &mut classpath)?;
    classpath.sort();
    let sep = if cfg!(windows) { ";" } else { ":" };
    let cp = classpath
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(sep);

    let classes = work.join("golden-classes");
    remove_dir(&classes)?;
    let mut sources = Vec::new();
    collect_files(&root.join("tools/extractor/src"), "java", &mut sources)?;
    sources.sort();
    run(Command::new(java_tool("javac"))
        .args(["-encoding", "UTF-8", "-nowarn", "-d"])
        .arg(&classes)
        .arg("-cp")
        .arg(&cp)
        .args(&sources))?;
    Ok((classes, cp))
}

/// `cargo xtask golden bless <scenario>...`: runs the scenarios in Vanillite and stores their
/// traces as the expected ones (for intended differences, e.g. a non-`vanilla` profile).
pub fn bless(root: &Path, names: &[&str]) -> Result<()> {
    if names.is_empty() {
        return Err("name the scenarios to bless (or `all`)".to_owned());
    }
    run(
        Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned()))
            .current_dir(root)
            .env("VANILLITE_BLESS", names.join(","))
            .args([
                "test",
                "-p",
                "vanillite-test",
                "--test",
                "parity",
                "--",
                "--include-ignored",
            ]),
    )
}

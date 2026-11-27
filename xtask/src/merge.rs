//! Conflict-free merges of generated and collective files.
//!
//! `cargo xtask setup [--global]` installs a git merge driver named `vanillite` (a copy of this
//! binary, so it works while the sources are mid-merge) for the paths `.gitattributes` marks
//! with `merge=vanillite`. git runs it as `merge-driver <base> <ours> <theirs> <marker size>
//! <path>`; it writes the result into `<ours>` and fails when conflicts remain:
//!
//! - `Cargo.lock`: three-way merge per package (name, version, source); dependency lists merge
//!   as sets. The output is ordered like Cargo writes it, so `--locked` accepts it.
//! - `KEYS.toml` (golden keys): three-way merge per entry; the shared key must agree.
//! - JSON (the Pterodactyl egg): three-way merge per top-level field and per egg variable
//!   (`env_variable`); variables added on the other side keep their place after their
//!   predecessor.
//! - Everything else: `git merge-file`, then conflicts where both sides only inserted lines at
//!   the same place take both insertions (ours first), if the inserted lines are list items:
//!   any lines in TOML files (the result must parse), `mod`/`use` declarations and attributes
//!   in `lib.rs`, `mod.rs` and `main.rs`, any lines in other Rust files (both then go through
//!   rustfmt, which must accept them).
//!
//! Without the driver (not set up, or a conflict it cannot resolve) git leaves the usual
//! conflict; `cargo xtask regen` then rewrites the generated files from their sources.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Map, Value};

use crate::golden::{parse_keys, render_keys};
use crate::{Result, read, write};

/// Name of the driver in the git configuration and in `.gitattributes`.
const DRIVER: &str = "vanillite";

/// `cargo xtask setup [--global]`: installs the merge driver for this clone (`--global`: for
/// every clone of this user, e.g. the clones that land branches).
pub fn setup(args: &[&str]) -> Result<()> {
    let global = match args {
        [] => false,
        ["--global"] => true,
        _ => return Err(crate::USAGE.to_owned()),
    };
    let dir = if global {
        let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
        PathBuf::from(home).join(".local/share/vanillite")
    } else {
        let out = git(&["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
        PathBuf::from(out.trim()).join("vanillite")
    };
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let target = dir.join("merge-driver");
    // Copy and rename, so a merge running the old copy is not disturbed.
    let tmp = dir.join(format!("merge-driver.{}", std::process::id()));
    std::fs::copy(&exe, &tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &target).map_err(|e| format!("{}: {e}", target.display()))?;
    // Falls back to git's normal merge when the copy is gone.
    let command = format!(
        "sh -c 'd=\"$0\"; if [ -x \"$d\" ]; then exec \"$d\" merge-driver \"$@\"; fi; \
         exec git merge-file --marker-size=\"$4\" \"$2\" \"$1\" \"$3\"' '{}' %O %A %B %L %P",
        target.display()
    );
    let scope = if global { "--global" } else { "--local" };
    git(&[
        "config",
        scope,
        &format!("merge.{DRIVER}.name"),
        "Vanillite generated and collective files",
    ])?;
    git(&["config", scope, &format!("merge.{DRIVER}.driver"), &command])?;
    println!(
        "merge driver `{DRIVER}` installed ({}, git config {scope})",
        target.display()
    );
    Ok(())
}

/// `cargo xtask regen`: rewrites the generated files from their sources after a merge (the
/// generated Rust of `vanillite-data`, the Pterodactyl egg and the configuration reference;
/// cargo itself updates `Cargo.lock` when it builds xtask). Golden keys need Java:
/// `cargo xtask golden check|regen`.
pub fn regen(root: &Path) -> Result<()> {
    crate::codegen::run(root, false)?;
    crate::egg::run(root, &[])
}

/// The driver: merges `ours` with `theirs` into `ours`. Fails when conflicts remain (they are
/// then written with conflict markers).
pub fn driver(base: &str, ours: &str, theirs: &str, marker: &str, path: &str) -> Result<()> {
    let (b, o, t) = (
        read(Path::new(base))?,
        read(Path::new(ours))?,
        read(Path::new(theirs))?,
    );
    let name = Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path);
    let structural = match name {
        "Cargo.lock" => merge_lock(&b, &o, &t),
        "KEYS.toml" => merge_keys(&b, &o, &t),
        n if n.ends_with(".json") => merge_json(&b, &o, &t),
        _ => None,
    };
    if let Some(merged) = structural {
        return write(Path::new(ours), &merged);
    }
    let size: usize = marker
        .parse()
        .map_err(|_| format!("bad marker size {marker}"))?;
    let out = Command::new("git")
        .args(["merge-file", "-p", "--zdiff3"])
        .arg(format!("--marker-size={size}"))
        .args([
            "-L", "ours", "-L", "base", "-L", "theirs", ours, base, theirs,
        ])
        .output()
        .map_err(|e| format!("git merge-file: {e}"))?;
    let text = String::from_utf8(out.stdout).map_err(|_| format!("{path}: not UTF-8"))?;
    match out.status.code() {
        Some(0) => return write(Path::new(ours), &text),
        Some(1..=127) => {}
        _ => return Err(format!("git merge-file failed on {path}")),
    }
    let kind = if name.ends_with(".toml") {
        Kind::Toml
    } else if matches!(name, "lib.rs" | "mod.rs" | "main.rs") {
        Kind::Declarations
    } else if name.ends_with(".rs") {
        Kind::Rust
    } else {
        Kind::Other
    };
    let (merged, left) = take_both_insertions(&text, size, kind);
    let valid = match kind {
        _ if left > 0 => None,
        Kind::Toml => merged.parse::<toml::Table>().ok().map(|_| merged.clone()),
        Kind::Declarations | Kind::Rust => rustfmt(&merged),
        Kind::Other => None,
    };
    match valid {
        Some(merged) => write(Path::new(ours), &merged),
        None => {
            write(Path::new(ours), if left == 0 { &text } else { &merged })?;
            Err(format!("{path}: conflicts left"))
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Toml,
    Declarations,
    Rust,
    Other,
}

impl Kind {
    /// Whether a line inserted on both sides may simply be kept from both.
    fn list_item(self, line: &str) -> bool {
        let l = line.trim();
        match self {
            Kind::Toml | Kind::Rust => true,
            Kind::Other => false,
            Kind::Declarations => {
                l.is_empty()
                    || l.starts_with("//")
                    || (l.starts_with("#[") && l.ends_with(']'))
                    || (l.ends_with(';') && {
                        let rest = l
                            .strip_prefix("pub(crate) ")
                            .or_else(|| l.strip_prefix("pub(super) "))
                            .or_else(|| l.strip_prefix("pub "))
                            .unwrap_or(l);
                        rest.starts_with("mod ") || rest.starts_with("use ")
                    })
            }
        }
    }
}

/// Resolves the conflicts of a `--zdiff3` merge where the base part is empty and both sides
/// insert only list items, by keeping both. Returns the text and the number of conflicts left.
fn take_both_insertions(text: &str, size: usize, kind: Kind) -> (String, usize) {
    let marker = |c: char, line: &str| {
        let body = line.trim_end_matches(['\n', '\r']);
        body.len() >= size
            && body.chars().take(size).all(|x| x == c)
            && body[size..].chars().next().is_none_or(|x| x == ' ')
    };
    let mut out = String::with_capacity(text.len());
    let mut left = 0;
    let mut lines = text.split_inclusive('\n');
    while let Some(line) = lines.next() {
        if !marker('<', line) {
            out.push_str(line);
            continue;
        }
        let mut hunk = vec![line];
        let (mut ours, mut base, mut theirs) = (Vec::new(), Vec::new(), Vec::new());
        let mut part = 0;
        for line in lines.by_ref() {
            hunk.push(line);
            if marker('|', line) {
                part = 1;
            } else if marker('=', line) {
                part = 2;
            } else if marker('>', line) {
                break;
            } else {
                [&mut ours, &mut base, &mut theirs][part].push(line);
            }
        }
        let insertions = base.is_empty() && ours.iter().chain(&theirs).all(|l| kind.list_item(l));
        if insertions {
            out.extend(ours);
            out.extend(theirs);
        } else {
            left += 1;
            out.extend(hunk);
        }
    }
    (out, left)
}

/// The file formatted by rustfmt (the repository's configuration), or `None` if it rejects it.
/// Looks in `$CARGO_HOME/bin` and `~/.cargo/bin` too: git may run the driver without them on
/// `PATH`.
fn rustfmt(text: &str) -> Option<String> {
    let home = |var: &str, sub: &str| std::env::var_os(var).map(|d| PathBuf::from(d).join(sub));
    let program = [
        home("CARGO_HOME", "bin/rustfmt"),
        home("HOME", ".cargo/bin/rustfmt"),
    ]
    .into_iter()
    .flatten()
    .find(|p| p.is_file())
    .unwrap_or_else(|| PathBuf::from("rustfmt"));
    let mut child = Command::new(program)
        .args(["--edition", "2024"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    child.stdin.take()?.write_all(text.as_bytes()).ok()?;
    let out = child.wait_with_output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

/// Three-way merge of one value: the side that changed wins; both changed alike is fine.
fn merge3<T: PartialEq + Clone>(
    base: Option<&T>,
    ours: Option<&T>,
    theirs: Option<&T>,
) -> Option<Option<T>> {
    if ours == theirs || theirs == base {
        Some(ours.cloned())
    } else if ours == base {
        Some(theirs.cloned())
    } else {
        None
    }
}

/// Three-way merge of a set.
fn merge_set(
    base: &BTreeSet<String>,
    ours: &BTreeSet<String>,
    theirs: &BTreeSet<String>,
) -> BTreeSet<String> {
    ours.union(theirs)
        .filter(|x| (ours.contains(*x) && theirs.contains(*x)) || !base.contains(*x))
        .cloned()
        .collect()
}

/// A `[[package]]` of a `Cargo.lock`: its lines without the dependencies, and the dependencies.
#[derive(Clone, PartialEq)]
struct Package {
    lines: Vec<String>,
    deps: BTreeSet<String>,
}

/// Name, version and source of a package.
type PackageId = (String, String, String);

/// Header and packages of a `Cargo.lock`, or `None` if it holds anything else.
fn parse_lock(text: &str) -> Option<(String, BTreeMap<PackageId, Package>)> {
    let mut blocks = text.split("\n[[package]]\n");
    let header = blocks.next()?.to_owned();
    let mut packages = BTreeMap::new();
    for block in blocks {
        let mut lines = Vec::new();
        let mut deps = BTreeSet::new();
        let mut in_deps = false;
        let (mut name, mut version, mut source) = (None, None, String::new());
        for line in block.lines().filter(|l| !l.is_empty()) {
            if in_deps {
                if line == "]" {
                    in_deps = false;
                } else {
                    deps.insert(line.trim().trim_end_matches(',').to_owned());
                }
                continue;
            }
            if line == "dependencies = [" {
                in_deps = true;
                continue;
            }
            if line.starts_with('[') || !deps.is_empty() {
                return None; // another table, or fields after the dependencies
            }
            let field = |k: &str| {
                line.strip_prefix(k)?
                    .strip_prefix(" = \"")?
                    .strip_suffix('"')
                    .map(str::to_owned)
            };
            name = name.or_else(|| field("name"));
            version = version.or_else(|| field("version"));
            if let Some(s) = field("source") {
                source = s;
            }
            lines.push(line.to_owned());
        }
        if in_deps {
            return None;
        }
        packages.insert((name?, version?, source), Package { lines, deps });
    }
    Some((header, packages))
}

/// Semantic version order (`0.9.0` < `0.10.0`, `1.0.0-beta` < `1.0.0`), as Cargo sorts packages.
fn semver_cmp(a: &str, b: &str) -> Ordering {
    let split = |v: &str| {
        let v = v.split('+').next().unwrap_or(v).to_owned();
        match v.split_once('-') {
            Some((core, pre)) => (core.to_owned(), Some(pre.to_owned())),
            None => (v, None),
        }
    };
    let ident = |a: &str, b: &str| match (a.parse::<u64>(), b.parse::<u64>()) {
        (Ok(x), Ok(y)) => x.cmp(&y),
        (Ok(_), Err(_)) => Ordering::Less,
        (Err(_), Ok(_)) => Ordering::Greater,
        _ => a.cmp(b),
    };
    let ((ca, pa), (cb, pb)) = (split(a), split(b));
    let dotted = |x: &str, y: &str| {
        let (mut x, mut y) = (x.split('.'), y.split('.'));
        loop {
            match (x.next(), y.next()) {
                (None, None) => return Ordering::Equal,
                (None, Some(_)) => return Ordering::Less,
                (Some(_), None) => return Ordering::Greater,
                (Some(p), Some(q)) => match ident(p, q) {
                    Ordering::Equal => {}
                    o => return o,
                },
            }
        }
    };
    dotted(&ca, &cb).then_with(|| match (pa, pb) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(p), Some(q)) => dotted(&p, &q),
    })
}

fn merge_lock(base: &str, ours: &str, theirs: &str) -> Option<String> {
    let (bh, b) = parse_lock(base)?;
    let (oh, o) = parse_lock(ours)?;
    let (th, t) = parse_lock(theirs)?;
    let header = merge3(Some(&bh), Some(&oh), Some(&th))??;
    let mut ids: Vec<&PackageId> = b.keys().chain(o.keys()).chain(t.keys()).collect();
    ids.sort_by(|x, y| {
        x.0.cmp(&y.0)
            .then_with(|| semver_cmp(&x.1, &y.1))
            .then_with(|| x.2.cmp(&y.2))
    });
    ids.dedup();
    let mut out = header;
    for id in ids {
        let (pb, po, pt) = (b.get(id), o.get(id), t.get(id));
        let package = match merge3(pb, po, pt) {
            Some(p) => p,
            None => {
                // Both changed it: the fields must agree, the dependencies merge as sets.
                let (po, pt) = (po?, pt?);
                let lines = merge3(pb.map(|p| &p.lines), Some(&po.lines), Some(&pt.lines))??;
                let empty = BTreeSet::new();
                let deps = merge_set(pb.map_or(&empty, |p| &p.deps), &po.deps, &pt.deps);
                Some(Package { lines, deps })
            }
        };
        let Some(package) = package else { continue };
        out.push_str("\n[[package]]\n");
        for line in &package.lines {
            out.push_str(line);
            out.push('\n');
        }
        if !package.deps.is_empty() {
            out.push_str("dependencies = [\n");
            for dep in &package.deps {
                out.push_str(&format!(" {dep},\n"));
            }
            out.push_str("]\n");
        }
    }
    Some(out)
}

fn merge_keys(base: &str, ours: &str, theirs: &str) -> Option<String> {
    let (bk, b) = parse_keys(base).ok()?;
    let (ok, o) = parse_keys(ours).ok()?;
    let (tk, t) = parse_keys(theirs).ok()?;
    let key = merge3(bk.as_ref(), ok.as_ref(), tk.as_ref())??;
    let names: BTreeSet<&String> = b.keys().chain(o.keys()).chain(t.keys()).collect();
    let mut entries = BTreeMap::new();
    for name in names {
        if let Some(hash) = merge3(b.get(name), o.get(name), t.get(name))? {
            entries.insert(name.clone(), hash);
        }
    }
    Some(render_keys(&key, &entries))
}

/// Field of the array items that identifies them (the egg's variables).
const ITEM_KEY: &str = "env_variable";

fn merge_json(base: &str, ours: &str, theirs: &str) -> Option<String> {
    let parse = |s: &str| serde_json::from_str::<Value>(s).ok();
    let (Value::Object(b), Value::Object(o), Value::Object(t)) =
        (parse(base)?, parse(ours)?, parse(theirs)?)
    else {
        return None;
    };
    let mut out = Map::new();
    let keys: Vec<&String> = o
        .keys()
        .chain(t.keys().filter(|k| !o.contains_key(*k)))
        .collect();
    for k in keys {
        let (vb, vo, vt) = (b.get(k), o.get(k), t.get(k));
        let merged = match merge3(vb, vo, vt) {
            Some(v) => v,
            None => Some(merge_items(vb?, vo?, vt?)?),
        };
        if let Some(v) = merged {
            out.insert(k.clone(), v);
        }
    }
    let mut text = serde_json::to_string_pretty(&Value::Object(out)).ok()?;
    text.push('\n');
    Some(text)
}

/// Three-way merge of an array of objects identified by [`ITEM_KEY`].
fn merge_items(base: &Value, ours: &Value, theirs: &Value) -> Option<Value> {
    let items = |v: &Value| -> Option<Vec<(String, Value)>> {
        v.as_array()?
            .iter()
            .map(|i| Some((i.get(ITEM_KEY)?.as_str()?.to_owned(), i.clone())))
            .collect()
    };
    let (b, o, t) = (items(base)?, items(ours)?, items(theirs)?);
    let find = |list: &[(String, Value)], k: &str| {
        list.iter().find(|(x, _)| x == k).map(|(_, v)| v.clone())
    };
    // Ours in order, then theirs' new items after their predecessor in theirs.
    let mut order: Vec<String> = o.iter().map(|(k, _)| k.clone()).collect();
    for (i, (k, _)) in t.iter().enumerate() {
        if order.contains(k) {
            continue;
        }
        let at = t[..i]
            .iter()
            .rev()
            .find_map(|(p, _)| order.iter().position(|x| x == p))
            .map_or(0, |p| p + 1);
        order.insert(at, k.clone());
    }
    let mut out = Vec::new();
    for k in order {
        let (vb, vo, vt) = (find(&b, &k), find(&o, &k), find(&t, &k));
        if let Some(v) = merge3(vb.as_ref(), vo.as_ref(), vt.as_ref())? {
            out.push(v);
        }
    }
    Some(Value::Array(out))
}

fn git(args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .args(args)
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

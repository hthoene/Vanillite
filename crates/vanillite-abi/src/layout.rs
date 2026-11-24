//! The ABI as data: every type alias, constant and record with its field offsets, and the
//! compatibility rules between two versions of it.
//!
//! The recorded snapshot (`abi.snapshot` next to this crate's manifest) is the ABI of the
//! last release of the current major version. [`check`] compares the current declarations with
//! it:
//!
//! - removing or changing a declaration, a field, its type or its offset is a **breaking**
//!   change and needs a new [`ABI_MAJOR`](crate::ABI_MAJOR);
//! - appending fields at the end of a record and adding declarations is an **extension** and
//!   needs a new [`ABI_MINOR`](crate::ABI_MINOR);
//! - either way the snapshot is then recorded again (`VANILLITE_BLESS=1 cargo test -p
//!   vanillite-abi`).

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;

/// A type alias of the ABI.
#[derive(Debug, Clone, Copy)]
pub struct TypeLayout {
    /// Name.
    pub name: &'static str,
    /// The aliased type as written.
    pub definition: &'static str,
}

/// A constant of the ABI.
#[derive(Debug, Clone, Copy)]
pub struct ConstLayout {
    /// `group::NAME`.
    pub name: &'static str,
    /// Its type.
    pub ty: &'static str,
    /// Its value.
    pub value: i64,
}

/// A field of a record.
#[derive(Debug, Clone, Copy)]
pub struct FieldLayout {
    /// Name.
    pub name: &'static str,
    /// Type as written.
    pub ty: &'static str,
    /// Offset in bytes.
    pub offset: usize,
}

/// A `repr(C)` record of the ABI.
#[derive(Debug, Clone, Copy)]
pub struct StructLayout {
    /// Name.
    pub name: &'static str,
    /// Size in bytes.
    pub size: usize,
    /// Alignment in bytes.
    pub align: usize,
    /// Fields in declaration order.
    pub fields: &'static [FieldLayout],
}

/// The type aliases.
pub const TYPES: &[TypeLayout] = crate::declared::TYPES;
/// The constants.
pub const CONSTS: &[ConstLayout] = crate::declared::CONSTS;
/// The records.
pub const STRUCTS: &[StructLayout] = crate::declared::STRUCTS;

/// Types as written by `stringify!`, without whitespace (its spacing differs between
/// compiler versions).
fn normalize(ty: &str) -> String {
    ty.chars().filter(|c| !c.is_whitespace()).collect()
}

/// The current ABI in the snapshot's text form. Sizes and offsets are those of the target
/// the check runs on (64-bit).
pub fn render() -> String {
    let mut out = String::new();
    let _ = writeln!(out, "abi {}.{}", crate::ABI_MAJOR, crate::ABI_MINOR);
    for t in TYPES {
        let _ = writeln!(out, "type {} = {}", t.name, normalize(t.definition));
    }
    for c in CONSTS {
        let _ = writeln!(out, "const {}: {} = {}", c.name, normalize(c.ty), c.value);
    }
    for s in STRUCTS {
        let _ = writeln!(out, "struct {} size={} align={}", s.name, s.size, s.align);
        for f in s.fields {
            let _ = writeln!(out, "  {}: {} @{}", f.name, normalize(f.ty), f.offset);
        }
    }
    out
}

/// One parsed snapshot: the version and the declarations by kind and name.
#[derive(Debug, Default, PartialEq)]
struct Parsed {
    major: u16,
    minor: u16,
    /// `type X` and `const X` lines (key: the part before `=`/`:`, value: the whole line).
    items: Vec<(String, String)>,
    /// Records: name and field lines.
    structs: Vec<(String, Vec<String>)>,
}

fn parse(text: &str) -> Result<Parsed, String> {
    let mut parsed = Parsed::default();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        if let Some(version) = line.strip_prefix("abi ") {
            let (major, minor) = version
                .split_once('.')
                .ok_or_else(|| format!("bad version line {line:?}"))?;
            parsed.major = major
                .parse()
                .map_err(|_| format!("bad major in {line:?}"))?;
            parsed.minor = minor
                .parse()
                .map_err(|_| format!("bad minor in {line:?}"))?;
        } else if let Some(rest) = line.strip_prefix("struct ") {
            let name = rest.split(' ').next().unwrap_or_default();
            parsed.structs.push((name.into(), Vec::new()));
        } else if let Some(field) = line.strip_prefix("  ") {
            let (_, fields) = parsed
                .structs
                .last_mut()
                .ok_or_else(|| format!("field outside a struct: {line:?}"))?;
            fields.push(field.into());
        } else {
            let key = line
                .split([':', '='])
                .next()
                .unwrap_or_default()
                .trim()
                .into();
            parsed.items.push((key, line.into()));
        }
    }
    Ok(parsed)
}

/// How the current ABI relates to the snapshot.
#[derive(Debug, PartialEq, Eq)]
pub enum Change {
    /// Identical.
    None,
    /// Only additions: declarations or fields appended at the end of records.
    Extension(Vec<String>),
    /// Something was removed or changed.
    Breaking(Vec<String>),
}

/// Compares the snapshot `old` with the current ABI text `new`.
pub fn compare(old: &str, new: &str) -> Result<Change, String> {
    let old = parse(old)?;
    let new = parse(new)?;
    let mut breaking = Vec::new();
    let mut added = Vec::new();
    for (key, line) in &old.items {
        match new.items.iter().find(|(k, _)| k == key) {
            Some((_, current)) if current == line => {}
            Some((_, current)) => breaking.push(format!("changed: {line} -> {current}")),
            None => breaking.push(format!("removed: {line}")),
        }
    }
    for (key, line) in &new.items {
        if !old.items.iter().any(|(k, _)| k == key) {
            added.push(format!("added: {line}"));
        }
    }
    for (name, fields) in &old.structs {
        let Some((_, current)) = new.structs.iter().find(|(n, _)| n == name) else {
            breaking.push(format!("removed: struct {name}"));
            continue;
        };
        let common = fields.len().min(current.len());
        for (a, b) in fields.iter().zip(current).take(common) {
            if a != b {
                breaking.push(format!("changed: {name}.{a} -> {b}"));
            }
        }
        if current.len() < fields.len() {
            breaking.push(format!(
                "removed fields of {name}: {:?}",
                &fields[current.len()..]
            ));
        }
        for f in current.iter().skip(fields.len()) {
            added.push(format!("appended: {name}.{f}"));
        }
    }
    for (name, _) in &new.structs {
        if !old.structs.iter().any(|(n, _)| n == name) {
            added.push(format!("added: struct {name}"));
        }
    }
    Ok(if !breaking.is_empty() {
        Change::Breaking(breaking)
    } else if !added.is_empty() {
        Change::Extension(added)
    } else {
        Change::None
    })
}

/// Checks the current ABI against the recorded snapshot. `Ok(true)` when the snapshot must be
/// recorded again (a correctly versioned change), `Ok(false)` when nothing changed; `Err` with
/// the reason when the version does not match the change.
pub fn check(snapshot: &str) -> Result<bool, String> {
    let current = render();
    let old = parse(snapshot)?;
    let (major, minor) = (crate::ABI_MAJOR, crate::ABI_MINOR);
    match compare(snapshot, &current)? {
        Change::None if (old.major, old.minor) == (major, minor) => Ok(false),
        Change::None => Err(format!(
            "the version changed to {major}.{minor} without an ABI change (snapshot: {}.{})",
            old.major, old.minor
        )),
        Change::Breaking(changes) if major <= old.major => Err(format!(
            "breaking ABI change without a new ABI_MAJOR (still {major}):\n  {}",
            changes.join("\n  ")
        )),
        Change::Breaking(_) => Ok(true),
        Change::Extension(changes) if major == old.major && minor <= old.minor => Err(format!(
            "ABI extension without a new ABI_MINOR (still {major}.{minor}):\n  {}",
            changes.join("\n  ")
        )),
        Change::Extension(_) => Ok(true),
    }
}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    extern crate std;

    use super::*;

    const SNAPSHOT: &str = "abi.snapshot";

    /// The ABI check of CI: fails on a breaking change without a major version bump and on any
    /// change that is not recorded in the snapshot.
    #[test]
    fn abi_snapshot() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(SNAPSHOT);
        let bless = std::env::var_os("VANILLITE_BLESS").is_some();
        let snapshot = std::fs::read_to_string(&path).unwrap_or_default();
        if snapshot.is_empty() && bless {
            std::fs::write(&path, render()).expect("write the ABI snapshot");
            return;
        }
        match check(&snapshot) {
            Ok(false) => {}
            Ok(true) if bless => std::fs::write(&path, render()).expect("write the ABI snapshot"),
            Ok(true) => panic!(
                "the ABI changed with a correct version bump; record it with \
                 VANILLITE_BLESS=1 cargo test -p vanillite-abi"
            ),
            Err(e) => panic!("{e}"),
        }
    }

    #[test]
    fn appending_is_an_extension_and_changing_is_breaking() {
        let old = "abi 1.0\nconst a::X: u32 = 1\nstruct S size=8 align=4\n  a: u32 @0\n";
        let appended = "abi 1.1\nconst a::X: u32 = 1\nconst a::Y: u32 = 2\n\
                        struct S size=8 align=4\n  a: u32 @0\n  b: u32 @4\n";
        assert!(matches!(compare(old, appended), Ok(Change::Extension(_))));
        let changed = "abi 1.1\nconst a::X: u32 = 2\nstruct S size=8 align=4\n  a: u32 @0\n";
        assert!(matches!(compare(old, changed), Ok(Change::Breaking(_))));
        let reordered =
            "abi 1.1\nconst a::X: u32 = 1\nstruct S size=8 align=4\n  b: u32 @0\n  a: u32 @4\n";
        assert!(matches!(compare(old, reordered), Ok(Change::Breaking(_))));
    }
}

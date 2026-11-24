//! `cargo xtask golden regen`: records the world generation parity goldens with the Java harness
//! in `tools/extractor/src/dev/vanillite/extractor/golden/`.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::vanilla::{collect_files, collect_jars, java_tool, remove_dir, run};
use crate::{Result, read};

/// Compiles the extractor against the unpacked server jar of the pinned version and runs the
/// golden parts (`parts` empty: all `*Goldens` classes) into
/// `crates/vanillite-worldgen/tests/goldens/`.
pub fn regen(root: &Path, parts: &[&str]) -> Result<()> {
    let version: toml::Table = read(&root.join("data/vanilla/version.toml"))?
        .parse()
        .map_err(|e| format!("version.toml: {e}"))?;
    let id = version
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or("version.toml: id")?;
    let work = root.join("target").join("vanilla").join(id);
    let inner = work
        .join("versions")
        .join(id)
        .join(format!("server-{id}.jar"));
    if !inner.exists() {
        return Err(format!(
            "{} is missing: run `cargo xtask vanilla update {id}` first",
            inner.display()
        ));
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

    let golden_src = root.join("tools/extractor/src/dev/vanillite/extractor/golden");
    let parts: Vec<String> = if parts.is_empty() {
        let mut files = Vec::new();
        collect_files(&golden_src, "java", &mut files)?;
        let mut names: Vec<String> = files
            .iter()
            .filter_map(|f| f.file_stem()?.to_str().map(str::to_owned))
            .filter(|n| n.ends_with("Goldens") && n != "Goldens")
            .collect();
        names.sort();
        names
    } else {
        parts.iter().map(|p| (*p).to_owned()).collect()
    };

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
    let out: PathBuf = root.join("crates/vanillite-worldgen/tests/goldens");
    run(Command::new(java_tool("java"))
        .current_dir(&work)
        .arg("-cp")
        .arg(format!("{}{sep}{cp}", classes.display()))
        .arg("dev.vanillite.extractor.golden.Goldens")
        .arg(&out)
        .args(&parts))
}

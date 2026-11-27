//! `cargo xtask parity fuzz`: random parity scenarios against Vanilla (#132); `cargo xtask
//! parity soak`: long scenarios against Vanilla (#166).
//!
//! Compiles the Java recorder on the pinned jar (like `golden regen`) and runs the
//! `parity_fuzz` binary of `vanillite-test` with its class path; that binary generates,
//! records, compares and shrinks (see `crates/vanillite-test/src/bin/parity_fuzz.rs`).
//! Results go to `target/parity-fuzz/` (`found/*.json`, `summary.md`).

use std::path::Path;
use std::process::Command;

use crate::Result;
use crate::golden;
use crate::vanilla::{java_tool, remove_dir, run};

/// Harness sources the scenario recorder needs.
const RECORDER: &[&str] = &[
    "ScenarioRecorder",
    "ScenarioEntities",
    "ParityServer",
    "ParityHooks",
    "ParityAgent",
];

/// The compiled recorder: its classes with the class path, the agent jar and the work dir.
fn recorder(root: &Path) -> Result<(String, std::path::PathBuf, std::path::PathBuf)> {
    let key = golden::Key::current(root)?;
    let (classes, cp, broken) = golden::harness(root, &key)?;
    let recorder_broken = broken.files.iter().any(|f| {
        f.file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(|s| RECORDER.contains(&s))
    });
    if recorder_broken {
        return Err(format!(
            "the scenario recorder does not compile against {}:\n{}",
            key.minecraft, broken.errors
        ));
    }
    let work_dir = root.join("target/vanilla").join(&key.minecraft);
    let agent = golden::agent_jar(&classes, &work_dir)?;
    let sep = if cfg!(windows) { ";" } else { ":" };
    Ok((format!("{}{sep}{cp}", classes.display()), agent, work_dir))
}

/// `cargo xtask parity soak`: the soak scenarios against Vanilla (#166), see
/// `crates/vanillite-test/src/soak.rs`. Results go to `target/parity-soak/`.
pub fn soak(root: &Path, args: &[&str]) -> Result<()> {
    let (classpath, agent, work_dir) = recorder(root)?;
    let out = root.join("target/parity-soak");
    remove_dir(&out)?;
    run(
        Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned()))
            .current_dir(root)
            .args([
                "run",
                "-q",
                "--profile",
                "release-fast",
                "-p",
                "vanillite-test",
                "--bin",
                "parity_soak",
                "--",
            ])
            .arg("--java")
            .arg(java_tool("java"))
            .arg("--agent")
            .arg(&agent)
            .arg("--classpath")
            .arg(classpath)
            .arg("--work-dir")
            .arg(&work_dir)
            .arg("--out")
            .arg(&out)
            .args(args),
    )
}

pub fn fuzz(root: &Path, args: &[&str]) -> Result<()> {
    let adopt = args.contains(&"--adopt");
    let pass: Vec<&str> = args.iter().copied().filter(|a| *a != "--adopt").collect();
    let (classpath, agent, work_dir) = recorder(root)?;
    let out = root.join("target/parity-fuzz");
    remove_dir(&out)?;
    run(
        Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned()))
            .current_dir(root)
            .args([
                "run",
                "-q",
                "-p",
                "vanillite-test",
                "--bin",
                "parity_fuzz",
                "--",
            ])
            .arg("--java")
            .arg(java_tool("java"))
            .arg("--agent")
            .arg(&agent)
            .arg("--classpath")
            .arg(classpath)
            .arg("--work-dir")
            .arg(&work_dir)
            .arg("--out")
            .arg(&out)
            .args(&pass),
    )?;
    if !adopt {
        return Ok(());
    }
    // The found scenarios become fixed ones with their Vanilla goldens.
    let scenarios = root.join("crates/vanillite-test/scenarios");
    let mut names = Vec::new();
    let found = out.join("found");
    let mut files: Vec<_> = std::fs::read_dir(&found)
        .map_err(|e| format!("{}: {e}", found.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    files.sort();
    for f in files {
        let name = f
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or("bad file name")?
            .to_owned();
        std::fs::copy(&f, scenarios.join(format!("{name}.json")))
            .map_err(|e| format!("{}: {e}", f.display()))?;
        names.push(name);
    }
    if names.is_empty() {
        return Ok(());
    }
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    golden::regen(root, &refs)?;
    println!(
        "adopted {} scenario(s): add scenarios/divergences/<name>.toml with tick, cause and \
         owner for each: {}",
        names.len(),
        names.join(", ")
    );
    Ok(())
}

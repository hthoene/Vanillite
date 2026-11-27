//! `cargo xtask parity fuzz`: random parity scenarios against Vanilla (#132); `cargo xtask
//! parity replay`: recorded server sessions against Vanilla (#152); `cargo xtask parity
//! soak`: long scenarios against Vanilla (#166).
//!
//! Compiles the Java recorder on the pinned jar (like `golden regen`) and runs the
//! `parity_fuzz` binary of `vanillite-test` with its class path; that binary generates (or
//! converts a session recording), records, compares and shrinks (see
//! `crates/vanillite-test/src/bin/parity_fuzz.rs`). Results go to `target/parity-fuzz/`
//! (`found/*.json`, `summary.md`).

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

/// `cargo xtask parity replay <recording>... [args]`: local recording folders, or
/// `<port>:<name>` / `<port>:latest` of the test server, fetched into `target/recordings/`.
pub fn replay(root: &Path, args: &[&str]) -> Result<()> {
    let split = args
        .iter()
        .position(|a| a.starts_with("--"))
        .unwrap_or(args.len());
    let (recordings, args) = args.split_at(split);
    if recordings.is_empty() {
        return Err("usage: cargo xtask parity replay <recording>... [--ticks N]".to_owned());
    }
    let mut sessions = Vec::new();
    for recording in recordings {
        let dir = match recording.split_once(':') {
            Some((port, name)) if port.parse::<u16>().is_ok() && !Path::new(recording).exists() => {
                fetch_recording(root, port, name)?
            }
            _ => std::path::PathBuf::from(recording),
        };
        if !dir.join("packets.jsonl").is_file() {
            return Err(format!("{} is no session recording", dir.display()));
        }
        let dir = std::fs::canonicalize(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        sessions.push(dir.to_string_lossy().into_owned());
    }
    let mut pass = Vec::new();
    for session in &sessions {
        pass.extend(["--session", session.as_str()]);
    }
    pass.extend_from_slice(args);
    driver(root, &pass)
}

/// Copies a recording of the test server instance on `port` into `target/recordings/`.
fn fetch_recording(root: &Path, port: &str, name: &str) -> Result<std::path::PathBuf> {
    let host = crate::devenv::host();
    let remote = format!("/opt/vanillite/{port}/recordings");
    let name = if name == "latest" {
        let out = Command::new("ssh")
            .args([host.as_str(), &format!("ls -1 {remote} | sort | tail -n 1")])
            .output()
            .map_err(|e| format!("ssh: {e}"))?;
        let name = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        if name.is_empty() {
            return Err(format!("no recordings in {host}:{remote}"));
        }
        name
    } else {
        name.to_owned()
    };
    let local = root
        .join("target/recordings")
        .join(format!("{port}-{name}"));
    if !local.exists() {
        std::fs::create_dir_all(root.join("target/recordings")).map_err(|e| e.to_string())?;
        println!("fetching {host}:{remote}/{name}");
        run(Command::new("scp").args([
            "-r",
            "-q",
            &format!("{host}:{remote}/{name}"),
            &local.to_string_lossy(),
        ]))?;
    } else {
        // A session that was still running when fetched has more packets now.
        run(Command::new("scp").args([
            "-q",
            &format!("{host}:{remote}/{name}/packets.jsonl"),
            &local.join("packets.jsonl").to_string_lossy(),
        ]))?;
    }
    Ok(local)
}

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
    driver(root, &pass)?;
    if !adopt {
        return Ok(());
    }
    adopt_found(root)
}

/// Runs the `parity_fuzz` driver with the recorder's class path; built with the cargo profile
/// `VANILLITE_PARITY_PROFILE` (default `dev`; the nightly replays with `release-fast`).
fn driver(root: &Path, pass: &[&str]) -> Result<()> {
    let (classpath, agent, work_dir) = recorder(root)?;
    let out = root.join("target/parity-fuzz");
    remove_dir(&out)?;
    let profile = std::env::var("VANILLITE_PARITY_PROFILE").unwrap_or_else(|_| "dev".to_owned());
    run(
        Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned()))
            .current_dir(root)
            .args([
                "run",
                "-q",
                "--profile",
                &profile,
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
            .args(pass),
    )
}

/// The found scenarios become fixed ones with their Vanilla goldens.
fn adopt_found(root: &Path) -> Result<()> {
    let out = root.join("target/parity-fuzz");
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

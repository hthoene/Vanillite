//! `cargo xtask parity fuzz`: random parity scenarios against Vanilla (#132); `cargo xtask
//! parity replay`: recorded server sessions against Vanilla (#152); `cargo xtask parity
//! soak`: long scenarios against Vanilla (#166); `cargo xtask parity quick`: the quick tier of
//! the parity scenarios for the quick check (#257).
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

/// `cargo xtask parity replay <recording> [args]`: a local recording folder, or
/// `<port>:<name>` / `<port>:latest` of the test server, fetched into `target/recordings/`.
pub fn replay(root: &Path, recording: &str, args: &[&str]) -> Result<()> {
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
    let session = dir.to_string_lossy().into_owned();
    let mut pass = vec!["--session", session.as_str()];
    pass.extend_from_slice(args);
    driver(root, &pass, None)
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
pub(crate) fn recorder(root: &Path) -> Result<(String, std::path::PathBuf, std::path::PathBuf)> {
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

/// `cargo xtask parity quick`: the fast scenarios without the slow ones of
/// `crates/vanillite-test/scenarios/quick-skip.txt` against their goldens (no Java needed), with
/// the world crate at opt-level 1 like CI's parity jobs.
pub fn quick(root: &Path) -> Result<()> {
    run(
        Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned()))
            .current_dir(root)
            .args([
                "test",
                "-q",
                "-p",
                "vanillite-test",
                "--locked",
                "--config",
                "profile.dev.package.vanillite-world.opt-level=1",
                "--test",
                "parity",
                "--",
                "--ignored",
                "--exact",
                "quick_scenarios",
            ]),
    )
}

pub fn fuzz(root: &Path, args: &[&str]) -> Result<()> {
    let adopt = args.contains(&"--adopt");
    let mut pass: Vec<&str> = args
        .iter()
        .copied()
        .filter(|a| *a != "--adopt" && *a != "--no-coverage")
        .collect();
    // The coverage of Vanilla's code by the scenarios (`parity coverage`, #257) steers the
    // generated scenarios towards what is least exercised.
    let coverage = crate::coverage::out_dir(root).join("coverage.json");
    let coverage = coverage.to_string_lossy();
    if !args.contains(&"--no-coverage") && Path::new(coverage.as_ref()).is_file() {
        println!("fuzz: weighting the scenarios by {coverage}");
        pass.extend(["--coverage", coverage.as_ref()]);
    }
    driver(root, &pass, None)?;
    if !adopt {
        return Ok(());
    }
    adopt_found(root)
}

/// Runs the `parity_fuzz` driver with the recorder's class path, with an extra environment
/// variable (the coverage agent of `parity coverage`) if given.
pub(crate) fn driver(root: &Path, pass: &[&str], env: Option<(&str, String)>) -> Result<()> {
    let (classpath, agent, work_dir) = recorder(root)?;
    let out = root.join("target/parity-fuzz");
    remove_dir(&out)?;
    let mut cmd = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned()));
    if let Some((k, v)) = env {
        cmd.env(k, v);
    }
    run(cmd
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
        .args(pass))
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

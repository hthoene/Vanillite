//! Repository tasks: Vanilla data update, code generation, golden tests and releases.
//!
//! ```text
//! cargo xtask vanilla update [<version>] [--jar <server.jar>]
//! cargo xtask codegen [--check]
//! cargo xtask golden check
//! cargo xtask golden regen [--all] [--bulk] [<name>...]
//! cargo xtask golden bless <scenario>...
//! cargo xtask dev [--release] [-- <server args>]
//! cargo xtask deploy [--port <port>] [--release]
//! cargo xtask testserver provision|status|logs|start|stop|restart [<port>]
//! cargo xtask docker [--push] [--in-docker] [--tag <tag>]...
//! cargo xtask egg [--check] | egg smoke [--image <ref>]
//! cargo xtask loadtest [--server vanillite|paper|both] [--steps <n,...>] [--hold <s>] ...
//! ```

mod codegen;
mod devenv;
mod docker;
mod egg;
mod golden;
mod loadtest;
mod normalize;
mod vanilla;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Errors are reported as text; xtask is a developer tool.
pub type Result<T> = std::result::Result<T, String>;

pub const USAGE: &str = "usage:
  cargo xtask vanilla update [<version>] [--jar <server.jar>]
      Download (or take) the server jar, verify its SHA-1, run Mojang's data generator and
      tools/extractor, normalise into data/vanilla/, run codegen and print a change report.
      <version> defaults to the latest release. Needs a JDK (JAVA_HOME or
      VANILLITE_JAVA_HOME, else java/javac on PATH) of the version the jar asks for.
  cargo xtask codegen [--check]
      Generate crates/vanillite-data/src/generated/ from data/vanilla/.
      --check fails if the committed files are not current.
  cargo xtask golden check
      Fail if a repository golden (worldgen, data, fast scenarios) is missing or stale: its key
      (Minecraft version, jar SHA-1, tools/extractor/harness-version, source or scenario hash)
      differs from the current one. No Java needed.
  cargo xtask golden regen [--all] [--bulk] [<name>...]
      Record the missing and stale goldens with the Java harness on the pinned jar (downloaded
      and unpacked if needed; needs a JDK 25). --all records every repository golden, --bulk
      also the bulk scenario goldens into $VANILLITE_GOLDENS (default target/goldens); names
      (part like ChunkGoldens or chunk, or scenario) record just those.
  cargo xtask golden bless <scenario>...|all
      Run the scenarios in Vanillite and store their traces as the expected goldens in
      crates/vanillite-test/goldens/blessed/ (intended differences, non-vanilla profiles).
  cargo xtask dev [--release] [-- <server args>]
      Build and run the server in ./run (debug build; --release uses the release-fast profile).
  cargo xtask deploy [--port <port>] [--release]
      Build the server (release-fast; --release for the full release profile) and install it on
      the test server as vanillite@<port>.service. <port> is 30001-30999, default 30001.
      The SSH host is vanillite-test, or VANILLITE_TEST_HOST.
  cargo xtask testserver provision|status|logs|start|stop|restart [<port>]
      provision runs scripts/testserver/provision.sh on the test server (idempotent);
      status lists the services and ports; the others act on vanillite@<port> (default 30001).
  cargo xtask docker [--push] [--in-docker] [--tag <tag>]...
      Build the server image (Dockerfile) for linux/amd64 and linux/arm64, tagged dev and the
      short commit hash, as $VANILLITE_IMAGE (default docker.hthoene.de/vanillite/vanillite).
      The static binaries are cross-compiled here with cargo-zigbuild (--in-docker builds them
      in the image instead). Builds on $DOCKER_HOST, the local engine, or else the test server
      over SSH. --push logs in as $VANILLITE_REGISTRY_USER (default deploy) with the password
      from $PRIVATE_DOCKER_REPO_KEY and pushes; without it the image is loaded into the engine.
  cargo xtask egg [--check]
      Generate the Pterodactyl egg deploy/pterodactyl/egg-vanillite.json from the configuration
      schema (server.properties keys and vanillite.toml rules) and check it against the egg
      format. --check fails if the committed egg is not current.
  cargo xtask egg smoke [--image <ref>]
      Start the image (default $VANILLITE_IMAGE:dev, see docker) like Pterodactyl's Wings does,
      with the egg's startup command and default variables (EULA accepted), on the Docker host
      of `docker`, and check that the server starts and answers a status ping on port 30950.
  cargo xtask loadtest [--server vanillite|paper|both] [--steps 50,100,...] [--hold <s>]
                       [--join-rate <per s>] [--chat <s>] [--view-distance <n>] [--release]
                       [--no-deploy] [--no-gate]
      Scaling benchmark: tools/loadbot bots (here) against vanillite@30901 (deployed from this
      checkout) and the Paper reference paper-loadtest on 30900, both offline mode on the test
      server and only running during the run. Steps default to 50,100,250,500,1000,2000,5000,
      each held 60 s; chat is off by default (--chat sets seconds between messages per bot).
      Writes target/loadtest/<time>/{vanillite,paper}.json, report.md and chart.svg, keeps
      results of clean checkouts under /opt/vanillite/bench/results on the test server and
      fails when p95 MSPT or RSS per player of a step is more than 10 % above the median of
      the last 5 runs (--no-gate skips the check).
  cargo xtask loadtest report <run.json>...
      Writes report.md and chart.svg for earlier runs next to the first file.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args.as_slice() {
        ["vanilla", "update", rest @ ..] => vanilla_update(rest),
        ["codegen"] => codegen::run(&repo_root(), false),
        ["codegen", "--check"] => codegen::run(&repo_root(), true),
        ["golden", "check"] => golden::check(&repo_root()),
        ["golden", "regen", rest @ ..] => golden::regen(&repo_root(), rest),
        ["golden", "bless", names @ ..] => golden::bless(&repo_root(), names),
        ["dev", rest @ ..] => devenv::dev(&repo_root(), rest),
        ["deploy", rest @ ..] => devenv::deploy(&repo_root(), rest),
        ["testserver", rest @ ..] => devenv::testserver(&repo_root(), rest),
        ["docker", rest @ ..] => docker::run_docker(&repo_root(), rest),
        ["egg", rest @ ..] => egg::run(&repo_root(), rest),
        ["loadtest", rest @ ..] => loadtest::run(&repo_root(), rest),
        _ => Err(USAGE.to_owned()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn vanilla_update(args: &[&str]) -> Result<()> {
    let mut version = None;
    let mut jar = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match *arg {
            "--jar" => jar = Some(PathBuf::from(it.next().ok_or("--jar needs a path")?)),
            v if !v.starts_with('-') && version.is_none() => version = Some(v.to_owned()),
            _ => return Err(USAGE.to_owned()),
        }
    }
    vanilla::update(&repo_root(), version.as_deref(), jar.as_deref())
}

/// The workspace root (the parent of the xtask crate).
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Reads a file to a string with the path in the error.
pub fn read(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Parses a JSON file.
pub fn read_json(path: &Path) -> Result<serde_json::Value> {
    serde_json::from_str(&read(path)?).map_err(|e| format!("{}: {e}", path.display()))
}

/// Writes a file, creating parent directories.
pub fn write(path: &Path, contents: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(path, contents).map_err(|e| format!("{}: {e}", path.display()))
}

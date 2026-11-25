//! Repository tasks: Vanilla data update, code generation, golden tests and releases.
//!
//! ```text
//! cargo xtask vanilla update [<version>] [--jar <server.jar>]
//! cargo xtask version check
//! cargo xtask version update <version> [--final] [--bulk] [--no-test]
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
mod version;

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
  cargo xtask version check
      Compare the pinned version (data/vanilla/version.toml) with the latest release in Mojang's
      version manifest; fails if a newer release exists (in GitHub Actions also sets the step
      output `newer`).
  cargo xtask version update <version> [--final] [--bulk] [--no-test]
      Prepare the switch to another version (release, pre-release or snapshot): extract the
      pinned and the new jar, regenerate data/vanilla and the generated code, record all
      repository goldens (--bulk also the bulk goldens and runs the ignored tests) and run the
      tests, all in the worktree target/version/<version>/tree, and write the change report
      target/version/<version>/change-report.md (protocol, packet fields, registries, blocks,
      items, entities, data pack, goldens, failing tests, affected code areas). --final does it
      in this checkout instead (only for releases): the real switch. --no-test skips the tests.
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
      the test server as vanillite@<port>.service. <port> is 30003-30999, default 30003
      (30001, 30002 and the proxy follow dev automatically).
      The SSH host is vanillite-test, or VANILLITE_TEST_HOST.
  cargo xtask testserver provision|status|logs|start|stop|restart [<port>]
      provision runs scripts/testserver/provision.sh on the test server (idempotent);
      status lists the services, ports and the last auto-deploys; the others act on
      vanillite@<port> (default 30001).
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
  cargo xtask loadtest [--server vanillite|paper|both] [--steps 50,100@<movement>,...]
                       [--movement <movement>] [--hold <s>] [--join-rate <per s>] [--chat <s>]
                       [--view-distance <n>] [--release] [--no-deploy] [--no-gate]
      Scaling benchmark: tools/loadbot bots (here) against vanillite@30901 (deployed from this
      checkout) and the Paper reference paper-loadtest on 30900, both offline mode on the test
      server and only running during the run. Steps default to 50,100,250,500,1000,2000,5000,
      each held 60 s; chat is off by default (--chat sets seconds between messages per bot).
      Movement (default for all steps: --movement, per step: <players>@<movement>; counts
      must not decrease): random (walk around the spawn) or straight:<speed>[:<turn s>],
      straight flight at y 300 in a random direction per bot at <speed> blocks/s or a preset
      (walk 4.317, sprint 5.612, elytra 33.5, boat 72.7), optionally turning every <turn s>.
      E.g. --steps 50,50@straight:boat:30. Steps also report chunks/s received by the bots,
      teleports (rejected moves) and the server's disk reads and read calls per player.
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
        ["version", "check"] => version::check(&repo_root()),
        ["version", "update", rest @ ..] => version::update(&repo_root(), rest),
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

//! Repository tasks: Vanilla data update, code generation, golden tests and releases.
//!
//! ```text
//! cargo xtask vanilla update [<version>] [--jar <server.jar>]
//! cargo xtask version check
//! cargo xtask version update <version> [--final] [--bulk] [--no-test]
//! cargo xtask multiversion add <version>
//! cargo xtask codegen [--check]
//! cargo xtask golden check
//! cargo xtask golden regen [--all] [--bulk] [<name>...]
//! cargo xtask golden bless <scenario>...
//! cargo xtask parity fuzz [--seed N] [--count N] [--minutes N] [--adopt]
//! cargo xtask dev [--release] [-- <server args>]
//! cargo xtask deploy [--host <host>] [--port <port>] [--release]
//! cargo xtask testserver [--host <host>] provision|status|logs|start|stop|restart [<port>]
//! cargo xtask testserver [--host <host>] run [--fetch <path>]... -- <xtask args>
//! cargo xtask testserver [--host <host>] nightly [status | fetch [<run>]]
//! cargo xtask testserver incidents [list [<count>] | show <id> | fetch [<id>...]]
//! cargo xtask docker [--push] [--in-docker] [--tag <tag>]...
//! cargo xtask egg [--check] | egg smoke [--image <ref>]
//! cargo xtask config-docs [--check]
//! cargo xtask loadtest [--server vanillite|paper|both] [--steps <n,...>] [--hold <s>] ...
//! cargo xtask bench all|<scenario>... [--servers <a,b>] [--publish] [--no-deploy]
//! cargo xtask bench list | report | publish <time|latest|results.json> | nightly [--no-push]
//! cargo xtask setup [--global] | regen
//! ```

mod bench;
mod codegen;
mod configdoc;
mod devenv;
mod docker;
mod egg;
mod golden;
mod loadtest;
mod merge;
mod multiversion;
mod normalize;
mod parity;
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
  cargo xtask multiversion add <version>...
      Extract older versions (releases or snapshots, 1.20.5 and later; obfuscated jars are
      remapped with Mojang's mappings) and keep what the multi-version layer needs to translate
      between each and its successor in data/multiversion/<version>/ (packets, packet shapes,
      registries, blocks, entity types, synchronised registry entries), then run codegen.
      `version update --final` keeps the outgoing version the same way.
  cargo xtask codegen [--check]
      Generate crates/vanillite-data/src/generated/ from data/vanilla/ and the adapter tables
      crates/vanillite-multiversion/src/generated/ from data/multiversion/.
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
  cargo xtask parity fuzz [--seed N] [--count N] [--minutes N] [--jobs N] [--adopt]
      Random parity scenarios (#132): generate scenarios with a packet-level player from
      consecutive seeds (default: 20 from seed 1), record them in Vanilla and run them in
      Vanillite, shrink every divergence to a small scenario. Writes target/parity-fuzz/found/
      and summary.md. --adopt copies the found scenarios into crates/vanillite-test/scenarios/
      and records their goldens (then add scenarios/divergences/<name>.toml with the cause).
      Also: --replay <scenario.json> runs given scenarios without generating.
  cargo xtask golden bless <scenario>...|all
      Run the scenarios in Vanillite and store their traces as the expected goldens in
      crates/vanillite-test/goldens/blessed/ (intended differences, non-vanilla profiles).
  cargo xtask dev [--release] [-- <server args>]
      Build and run the server in ./run (debug build; --release uses the release-fast profile).
  cargo xtask deploy [--host <host>] [--port <port>] [--release]
      Build the server (release-fast; --release for the full release profile) and install it on
      the test server as vanillite@<port>.service. <port> is 30003-30999, default 30003
      (30001, 30002 and the proxy follow dev automatically).
      The SSH host is --host, else VANILLITE_TEST_HOST, else vanillite-test (vanillite-test2:
      the internal server for experiments, offline mode).
  cargo xtask testserver [--host <host>] provision|status|logs|start|stop|restart [<port>]
      provision runs scripts/testserver/provision.sh on the test server (idempotent; on
      vanillite-test2 scripts/testserver/provision-test2.sh); status lists the services, ports,
      the last auto-deploys and incidents (vanillite-test2: lock, disk, last nightly run); the
      others act on vanillite@<port> (default 30001). Host as for deploy.
  cargo xtask testserver [--host <host>] run [--fetch <path>]... -- <xtask args>
      Heavy tests and experiments on vanillite-test2 (default host): wait for its lock, copy
      this checkout (without target/ and .git/) to /opt/vanillite-work/src there, run
      `cargo xtask <xtask args>` in it, then copy the --fetch paths (relative to the checkout,
      e.g. target/parity-fuzz) back. Example: testserver run --fetch target/parity-fuzz --
      parity fuzz --minutes 30.
  cargo xtask testserver [--host <host>] nightly [status | fetch [<run>]]
      The nightly heavy tests on vanillite-test2 (scripts/testserver/nightly.sh, 00:30
      Europe/Berlin): summaries of the newest runs, or download one run (default the newest)
      into target/nightly/.
  cargo xtask testserver incidents [list [<count>] | show <id> | fetch [<id>...]]
      Crashes and suspicious disconnects that vanillite-incidents saved on the test server:
      list the newest (default 20), show one (summary and backtrace), or download bundles into
      target/incidents/<id>/ (without ids: all not downloaded yet).
  cargo xtask docker [--push] [--in-docker] [--tag <tag>]...
      Build the server image (Dockerfile) for linux/amd64 and linux/arm64, tagged dev and the
      short commit hash, as $VANILLITE_IMAGE (default docker.hthoene.de/vanillite/vanillite).
      The static binaries are cross-compiled here with cargo-zigbuild (--in-docker builds them
      in the image instead). Builds on $DOCKER_HOST, the local engine, or else the test server
      over SSH. --push logs in as $VANILLITE_REGISTRY_USER (default deploy) with the password
      from $PRIVATE_DOCKER_REPO_KEY and pushes; without it the image is loaded into the engine.
  cargo xtask egg [--check]
      Generate the Pterodactyl eggs deploy/pterodactyl/egg-vanillite.json (every setting) and
      egg-vanillite-simple.json (basic settings only) from the configuration schema
      (server.properties keys and vanillite.toml rules) and check them against the egg format,
      and the configuration reference (config-docs). --check fails if a committed file is not
      current or the eggs differ in what they share.
  cargo xtask config-docs [--check]
      Generate the configuration reference docs/configuration.md from the configuration schema
      (server.properties keys and vanillite.toml rules with their value in every profile).
      --check fails if the committed file is not current.
  cargo xtask egg smoke [--image <ref>]
      Start the image (default $VANILLITE_IMAGE:dev, see docker) like Pterodactyl's Wings does,
      with the egg's startup command and default variables (EULA accepted), on the Docker host
      of `docker`, and check that the server starts and answers a status ping on port 30950.
  cargo xtask loadtest [--server vanillite|paper|both] [--steps 50,100@<movement>,...]
                       [--movement <movement>] [--hold <s>] [--settle <s>]
                       [--join-rate <per s>] [--chat <s>]
                       [--view-distance <n>] [--release] [--no-deploy] [--no-gate]
                       [--rule <key>=<value>]...
      Scaling benchmark: tools/loadbot bots (here) against vanillite@30901 (deployed from this
      checkout) and the Paper reference paper-loadtest on 30900, both offline mode on the test
      server and only running during the run. Steps default to 50,100,250,500,1000,2000,5000,
      each held 60 s after --settle seconds (default 0); chat is off by default (--chat sets
      seconds between messages per bot).
      Movement (default for all steps: --movement, per step: <players>@<movement>; counts
      must not decrease): random (walk around the spawn) or straight:<speed>[:<turn s>],
      straight flight at y 300 in a random direction per bot at <speed> blocks/s or a preset
      (walk 4.317, sprint 5.612, elytra 33.5, boat 72.7), optionally turning every <turn s>;
      spread:<radius> (fly to a random point within the radius and stay) or still.
      E.g. --steps 50,50@straight:boat:30. Steps also report chunks/s received by the bots,
      holes (missing chunks within 2 chunks of a bot), teleports (rejected moves), the
      server's disk reads and read calls per player, and RSS per player above the idle RSS.
      Writes target/loadtest/<time>/{vanillite,paper}.json, report.md and chart.svg, keeps
      results of clean checkouts under /opt/vanillite/bench/results on the test server and
      fails when p95 MSPT or RSS per player of a step is more than 10 % above the median of
      the last 5 runs (--no-gate skips the check). --rule sets a rule of vanillite.toml for
      the Vanillite instance of this run (e.g. performance.region-threading.enabled=true);
      such runs are experiments: no regression check, not kept as a baseline.
  cargo xtask loadtest report <run.json>...
      Writes report.md and chart.svg for earlier runs next to the first file.
  cargo xtask bench all|<scenario>... [--servers <server,...>] [--publish] [--no-deploy]
      Benchmark suite (`bench list` shows the scenarios): every scenario against Vanillite
      (vanillite@30901, deployed from this checkout; also with the paper rule profile), Paper,
      Vanilla and Pumpkin (taking turns on 30900), or the proxies Velocity and vanillite-proxy,
      with the same settings and a new world each (`vanillite-bench` on the test server). Player
      load from tools/loadbot here; command-built scenarios (redstone, mobs, items, hoppers,
      TNT, fluids, light) use console commands and one observer bot. A server that lacks a
      command is reported as n/a. Writes target/bench/<time>/results.json and keeps the raw
      results under /opt/vanillite/bench/suite on the test server; --publish also updates
      benchmarks/results/ (latest.json, history.json, runs/<time>.json) and regenerates
      benchmarks/README.md with regressions and improvements against the previous run.
      --servers limits the run to these servers; --no-deploy uses the binaries installed there.
  cargo xtask bench publish <time|latest|results.json>
      Publishes a finished run like --publish: a local results.json or the suite directory
      /opt/vanillite/bench/suite/<time>/ on the test server (latest: the newest).
  cargo xtask bench report
      Regenerates benchmarks/README.md from benchmarks/results/.
  cargo xtask bench nightly [--no-push]
      When origin/dev has code newer than the published results: runs `bench all --publish` in
      the worktree target/bench/tree and commits the results on the branch
      bench/results-<date> (pushed unless --no-push; then hand it to the normal landing).
  cargo xtask setup [--global]
      Install the git merge driver `vanillite` for the files .gitattributes marks (Cargo.lock,
      golden KEYS.toml, the egg, Cargo.toml, module lists, the rule catalogue): parallel
      additions there merge without conflicts. --global: for all clones of this user.
  cargo xtask regen
      After a merge: rewrite the generated files (vanillite-data codegen, the egg, the
      configuration reference) from their sources; cargo updates Cargo.lock on the way.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args.as_slice() {
        ["vanilla", "update", rest @ ..] => vanilla_update(rest),
        ["version", "check"] => version::check(&repo_root()),
        ["version", "update", rest @ ..] => version::update(&repo_root(), rest),
        ["multiversion", "add", rest @ ..] => multiversion::add(&repo_root(), rest),
        ["codegen"] => codegen::run(&repo_root(), false),
        ["codegen", "--check"] => codegen::run(&repo_root(), true),
        ["golden", "check"] => golden::check(&repo_root()),
        ["golden", "regen", rest @ ..] => golden::regen(&repo_root(), rest),
        ["golden", "bless", names @ ..] => golden::bless(&repo_root(), names),
        ["parity", "fuzz", rest @ ..] => parity::fuzz(&repo_root(), rest),
        ["dev", rest @ ..] => devenv::dev(&repo_root(), rest),
        ["deploy", rest @ ..] => devenv::deploy(&repo_root(), rest),
        ["testserver", rest @ ..] => devenv::testserver(&repo_root(), rest),
        ["docker", rest @ ..] => docker::run_docker(&repo_root(), rest),
        ["egg", rest @ ..] => egg::run(&repo_root(), rest),
        ["config-docs", rest @ ..] => configdoc::run(&repo_root(), rest),
        ["loadtest", rest @ ..] => loadtest::run(&repo_root(), rest),
        ["bench", rest @ ..] => bench::run(&repo_root(), rest),
        ["setup", rest @ ..] => merge::setup(rest),
        ["regen"] => merge::regen(&repo_root()),
        ["merge-driver", base, ours, theirs, marker, path] => {
            merge::driver(base, ours, theirs, marker, path)
        }
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

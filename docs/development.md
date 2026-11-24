# Development

## Setup

- Rust is pinned in `rust-toolchain.toml`; rustup installs it on the first `cargo` call.
- `cargo install --locked cargo-nextest cargo-deny` (or the prebuilt binaries).
- Optional: [sccache](https://github.com/mozilla/sccache) as `RUSTC_WRAPPER` shares compiled
  dependencies between checkouts.
- A JDK 25 is needed only for `cargo xtask vanilla update` and golden regeneration.

## Everyday commands

| Command | What |
|---|---|
| `cargo fmt --all` | Format. |
| `cargo clippy --workspace --all-targets -- -D warnings` | Lints; CI fails on any warning. |
| `cargo nextest run` | Tests (`.config/nextest.toml`; no fail-fast). |
| `cargo nextest run --run-ignored all` | Including the bulk tests that CI runs nightly. |
| `cargo test --doc` | Doc tests (nextest does not run them). |
| `cargo deny check` | Licences, advisories, duplicate and wildcard dependencies (`deny.toml`). |
| `cargo xtask codegen --check` | Generated tables match `data/vanilla/`. |
| `cargo xtask egg --check` | The Pterodactyl egg matches the configuration schema (`cargo xtask egg` regenerates it). |
| `cargo xtask dev [--release] [-- <args>]` | Build and run the server in `./run`. |

## New Minecraft versions

Vanillite targets only the newest Minecraft release, pinned in `data/vanilla/version.toml`. The
nightly workflow runs `cargo xtask version check` and opens an issue when Mojang releases a newer
version.

| Command | What |
|---|---|
| `cargo xtask version check` | Fails if a newer release than the pinned one exists. |
| `cargo xtask version update <id>` | Preview (any release, pre-release or snapshot): data, generated code, goldens and a test run in the worktree `target/version/<id>/tree/`, and the change report `target/version/<id>/change-report.md` (protocol, packet fields, registries, blocks, items, entities, data pack, goldens, failing tests, affected code areas). The pinned version stays. |
| `cargo xtask version update <id> --final` | The switch itself (releases only): the same in this checkout; commit data, generated code and goldens. |

Golden harness sources that do not compile against the new jar are left out (with what needs
them) and named in the report; porting them is part of the switch.

## Build profiles

| Profile | Use |
|---|---|
| `dev` | Daily work. Line tables only; dependencies optimised. |
| `release` | Shipped binary: fat LTO, one codegen unit, stripped. Slow to build. |
| `release-fast` | Close to release speed, quick to rebuild; test server deploys. |
| `profiling` | Release code with symbols for perf, flamegraphs and heaptrack. |

## Test server

`ssh vanillite-test` (host overridable with `VANILLITE_TEST_HOST`). Players may only use ports
30000-30999: Paper (reference) runs on 30000, Vanillite on 30001 and up. The machine is
provisioned by `scripts/testserver/provision.sh`; every Vanillite server is a systemd unit
`vanillite@<port>` in `/opt/vanillite/<port>`, with Paper's whitelist.

| Command | What |
|---|---|
| `cargo xtask testserver provision` | Install or update users, units and the deploy helper (idempotent). |
| `cargo xtask deploy [--port 30001] [--release]` | Build (`release-fast`, or `release`) and (re)start `vanillite@<port>`. |
| `cargo xtask testserver status` | Services and listening ports. |
| `cargo xtask testserver logs [<port>]` | Follow the log. |
| `cargo xtask testserver start\|stop\|restart [<port>]` | Control a server; stop also disables it. |

## Docker image

`Dockerfile` builds a static (musl, mimalloc) binary on an empty base for linux/amd64 and
linux/arm64. `cargo xtask docker [--push]` cross-compiles both binaries here (fast, sccache) and
builds the image on `$DOCKER_HOST`, the local engine, or else the test server over SSH (dev
containers cannot run an engine); `--push` pushes `docker.hthoene.de/vanillite/vanillite` with
the tags `dev` and the short commit hash (needs `PRIVATE_DOCKER_REPO_KEY`). Plain
`docker build .` builds the binary inside the image instead.

Host tools for the cross build:

```sh
rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl
cargo install --locked cargo-zigbuild
mkdir -p ~/.local/zig && curl -fsSL https://ziglang.org/download/0.14.1/zig-x86_64-linux-0.14.1.tar.xz \
    | tar -xJ -C ~/.local/zig --strip-components=1   # then put ~/.local/zig on PATH
scripts/docker/install-docker.sh --client   # Docker CLI and buildx
```

### Pterodactyl egg

`deploy/pterodactyl/egg-vanillite.json` is generated from the typed schema in `vanillite-config`:
every `server.properties` key (`ServerProperties::INFO`, except those declared `panel = false`)
and every `vanillite.toml` rule (except the `debug` group) becomes a variable named like its
`VANILLITE_<KEY>` override, with the documentation, default, range or allowed values. After a
change of the schema run `cargo xtask egg`; CI and the `egg` test of xtask fail while the
committed egg is stale. `cargo xtask egg smoke [--image <ref>]` starts an image like Wings does
(STARTUP, `/home/container`, UID 988, read-only root) and pings it.

## Fuzzing

`fuzz/` holds the cargo-fuzz targets of the input decoders (crate `vanillite-fuzz`, its own
workspace, built with nightly): binary NBT, SNBT, inbound framing and decompression, the framing
round trip, and one target per connection state for the serverbound packet decoders. Committed
seeds are in `fuzz/seeds/<target>`; the growing corpus (`fuzz/corpus`) and crashes
(`fuzz/artifacts`) are not committed.

| Command | What |
|---|---|
| `scripts/fuzz.sh [seconds] [jobs]` | Build all targets once and run them in parallel (default 300 s each). |
| `cargo +nightly fuzz run -O <target> fuzz/corpus/<target> fuzz/seeds/<target>` | Run one target. |
| `cargo +nightly fuzz run -O <target> fuzz/artifacts/<target>/<file>` | Reproduce a crash. |

A crash becomes a regular regression test next to the exploit tests in
`crates/<crate>/tests/exploits.rs`, which name the SEC rule of the security research they cover.

## Parity tests

Vanillite is compared with the pinned Vanilla jar (architecture section 13). The Java harness in
`tools/extractor/src/dev/vanillite/extractor/golden/` runs Vanilla in-process and records
goldens; the Rust tests compare against them exactly.

- **Data goldens** (`crates/vanillite-test/tests/data_goldens.rs`): registries, block state ids,
  items, packet ids and the loaded Vanilla data pack (dynamic registries, tags).
- **Scenario traces** (`crates/vanillite-test/scenarios/*.json`, format in
  `crates/vanillite-test/src/scenario.rs`): a scenario runs tick by tick on Vanilla and in
  Vanillite; blocks of a region, pending block and fluid ticks and entities must match in every
  frame. Every scenario also runs serially and in parallel (verify mode). Tier `fast` goldens
  are in the repository and run on every push; tier `bulk` goldens live in
  `$VANILLITE_GOLDENS` (default `target/goldens`) and run nightly.
- **World generation goldens** (`crates/vanillite-worldgen/tests/`).

Every golden is keyed by Minecraft version, jar SHA-1, harness version
(`tools/extractor/harness-version`, bump it when shared recording code changes) and the hash of
its scenario or recorder source. A missing or stale golden fails; it is never skipped.

| Command | What |
|---|---|
| `cargo xtask golden check` | Repository goldens are current (CI, no Java). |
| `cargo xtask golden regen [--all] [--bulk] [<name>...]` | Record missing or stale goldens (JDK 25). |
| `cargo xtask golden bless <scenario>...` | Accept Vanillite's trace for intended differences. |

## CI

`.github/workflows/ci.yml` runs on every push and pull request: format, clippy, tests with codegen
and golden checks and doc tests, and cargo-deny, as parallel jobs with a shared Rust cache written by `dev`.
`.github/workflows/nightly.yml` records missing bulk goldens and runs the ignored bulk tests (with
Java 25 and a goldens cache), a
release build and the fuzz targets (`scripts/fuzz.sh`, corpus cached between nights) once a
night.

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
| `cargo xtask dev [--release] [-- <args>]` | Build and run the server in `./run`. |

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

## CI

`.github/workflows/ci.yml` runs on every push and pull request: format, clippy, tests with codegen
check and doc tests, and cargo-deny, as parallel jobs with a shared Rust cache written by `dev`.
`.github/workflows/nightly.yml` runs the ignored bulk tests (with Java 25 and a goldens cache), a
release build and the fuzz targets once a night.

//! Test harness: headless world builder, scenario runner, trace format, golden storage and in-memory clients.
//!
//! The Vanilla parity framework (architecture section 13):
//!
//! - [`scenario`]: scenarios (`scenarios/*.json`), shared by the Java recorder and the runner.
//! - [`trace`]: the tick-by-tick trace format both sides write, and its exact comparison.
//! - [`world`]: the observation API a scenario world implements ([`world::ScenarioWorld`]) and
//!   the flat stand-in world used until `World::headless` exists.
//! - [`runner`]: runs a scenario serially and in parallel (verify mode) and compares the trace
//!   with its golden.
//! - [`golden`]: golden storage keyed by (Minecraft version, jar SHA-1, harness version,
//!   scenario hash): small goldens in `goldens/`, bulk goldens in a cache directory.
//!
//! Goldens are recorded on the pinned Vanilla jar by `cargo xtask golden regen`; intended
//! differences are accepted with `cargo xtask golden bless <scenario>`.

pub mod golden;
pub mod runner;
pub mod scenario;
pub mod trace;
pub mod world;

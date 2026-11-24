//! Repository tasks: Vanilla data update, code generation, golden tests and releases.
//!
//! ```text
//! cargo xtask vanilla update [<version>] [--jar <server.jar>]
//! cargo xtask codegen [--check]
//! cargo xtask golden regen [<PartGoldens>...]
//! cargo xtask dev [--release] [-- <server args>]
//! cargo xtask deploy [--port <port>] [--release]
//! cargo xtask testserver provision|status|logs|start|stop|restart [<port>]
//! ```

mod codegen;
mod devenv;
mod golden;
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
  cargo xtask golden regen [<PartGoldens>...]
      Record the world generation parity goldens (all parts, or the named ones) into
      crates/vanillite-worldgen/tests/goldens/ with the Java harness. Needs the unpacked jar of
      `cargo xtask vanilla update` and a JDK.
  cargo xtask dev [--release] [-- <server args>]
      Build and run the server in ./run (debug build; --release uses the release-fast profile).
  cargo xtask deploy [--port <port>] [--release]
      Build the server (release-fast; --release for the full release profile) and install it on
      the test server as vanillite@<port>.service. <port> is 30001-30999, default 30001.
      The SSH host is vanillite-test, or VANILLITE_TEST_HOST.
  cargo xtask testserver provision|status|logs|start|stop|restart [<port>]
      provision runs scripts/testserver/provision.sh on the test server (idempotent);
      status lists the services and ports; the others act on vanillite@<port> (default 30001).";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args.as_slice() {
        ["vanilla", "update", rest @ ..] => vanilla_update(rest),
        ["codegen"] => codegen::run(&repo_root(), false),
        ["codegen", "--check"] => codegen::run(&repo_root(), true),
        ["golden", "regen", parts @ ..] => golden::regen(&repo_root(), parts),
        ["dev", rest @ ..] => devenv::dev(&repo_root(), rest),
        ["deploy", rest @ ..] => devenv::deploy(&repo_root(), rest),
        ["testserver", rest @ ..] => devenv::testserver(&repo_root(), rest),
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

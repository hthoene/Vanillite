//! Embeds the short commit hash as `VANILLITE_COMMIT`: from the environment variable of that
//! name (builds without `.git`, e.g. Docker), else from git, else `unknown`.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=VANILLITE_COMMIT");
    let commit = std::env::var("VANILLITE_COMMIT")
        .ok()
        .filter(|commit| !commit.is_empty())
        .map(|commit| commit.chars().take(7).collect())
        .or_else(git_commit)
        .unwrap_or_else(|| "unknown".to_owned());
    println!("cargo:rustc-env=VANILLITE_COMMIT={commit}");
}

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    let text = String::from_utf8(output.stdout).ok()?;
    (output.status.success() && !text.trim().is_empty()).then(|| text.trim().to_owned())
}

fn git_commit() -> Option<String> {
    // Rebuild when HEAD moves: a commit, checkout or reset writes HEAD or its reflog.
    if let Some(dir) = git(&["rev-parse", "--absolute-git-dir"]) {
        println!("cargo:rerun-if-changed={dir}/HEAD");
        println!("cargo:rerun-if-changed={dir}/logs/HEAD");
    }
    git(&["rev-parse", "--short=7", "HEAD"])
}

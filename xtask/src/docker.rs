//! `cargo xtask docker`: builds the multi-arch server image from `Dockerfile` and pushes it.

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::Result;
use crate::devenv::run;

pub const DEFAULT_IMAGE: &str = "docker.hthoene.de/vanillite/vanillite";
const DEFAULT_USER: &str = "deploy";
const PASSWORD_ENV: &str = "PRIVATE_DOCKER_REPO_KEY";
/// zig's clang turns off `evex512` for the x86-64 baseline, which breaks libdeflate's AVX-512
/// functions; they are still only used when the CPU has AVX-512.
const ZIG_X86_CFLAGS: (&str, &str) = ("CFLAGS_x86_64_unknown_linux_musl", "-mevex512");
/// File name of the x86-64-v3 build (as in crates/vanillite/src/cpu.rs).
const V3_BINARY: &str = "vanillite-x86-64-v3";
/// File name of the glibc build that loads plugins (as in crates/vanillite/src/runtime.rs).
const GNU_BINARY: &str = "vanillite-gnu";
/// Docker platform architecture, Rust target of each image platform and its glibc target.
const ARCHES: &[(&str, &str, &str)] = &[
    (
        "amd64",
        "x86_64-unknown-linux-musl",
        "x86_64-unknown-linux-gnu",
    ),
    (
        "arm64",
        "aarch64-unknown-linux-musl",
        "aarch64-unknown-linux-gnu",
    ),
];
/// The glibc version the glibc build links against (zigbuild's target suffix).
const GLIBC: &str = "2.28";

/// `cargo xtask docker [--push] [--in-docker] [--tag <tag>]...`
pub fn run_docker(root: &Path, args: &[&str]) -> Result<()> {
    let mut push = false;
    let mut in_docker = false;
    let mut extra_tags = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match *arg {
            "--push" => push = true,
            "--in-docker" => in_docker = true,
            "--tag" => extra_tags.push(*it.next().ok_or("--tag needs a value")?),
            _ => return Err(crate::USAGE.to_owned()),
        }
    }
    let image = std::env::var("VANILLITE_IMAGE").unwrap_or_else(|_| DEFAULT_IMAGE.to_owned());
    let mut tags = vec!["dev".to_owned(), commit_tag(root)?];
    tags.extend(extra_tags.iter().map(|t| (*t).to_owned()));

    let docker_host = docker_host();
    if !in_docker {
        build_binaries(root)?;
    }

    let mut build = docker(docker_host.as_deref());
    build.current_dir(root).args([
        "buildx",
        "build",
        "--platform",
        &ARCHES
            .iter()
            .map(|(arch, _, _)| format!("linux/{arch}"))
            .collect::<Vec<_>>()
            .join(","),
        "--build-arg",
        if in_docker {
            "BINARY=build"
        } else {
            "BINARY=prebuilt"
        },
    ]);
    for tag in &tags {
        build.args(["--tag", &format!("{image}:{tag}")]);
    }
    build.arg(if push { "--push" } else { "--load" }).arg(".");

    if !push {
        return run(&mut build);
    }
    let registry = image.split('/').next().unwrap_or_default().to_owned();
    login(docker_host.as_deref(), &registry)?;
    let result = run(&mut build);
    let _ = docker(docker_host.as_deref())
        .args(["logout", &registry])
        .stdout(Stdio::null())
        .status();
    result?;
    for tag in &tags {
        println!("pushed {image}:{tag}");
    }
    Ok(())
}

/// The short commit hash, with `-dirty` for uncommitted changes.
fn commit_tag(root: &Path) -> Result<String> {
    let git = |args: &[&str]| {
        Command::new("git")
            .current_dir(root)
            .args(args)
            .output()
            .map_err(|e| format!("git: {e}"))
    };
    let hash = git(&["rev-parse", "--short=10", "HEAD"])?;
    let mut tag = String::from_utf8_lossy(&hash.stdout).trim().to_owned();
    if tag.is_empty() {
        return Err("git rev-parse failed".to_owned());
    }
    if !git(&["status", "--porcelain"])?.stdout.is_empty() {
        tag.push_str("-dirty");
    }
    Ok(tag)
}

/// The Docker engine to build on: `DOCKER_HOST` if set, the local engine if it runs, else the
/// test server over SSH (dev containers cannot run an engine themselves).
pub fn docker_host() -> Option<String> {
    if std::env::var_os("DOCKER_HOST").is_some() {
        return None;
    }
    let local = Command::new("docker")
        .arg("info")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if local {
        return None;
    }
    let host = format!("ssh://{}", crate::devenv::host());
    println!("no local Docker engine, building on {host}");
    Some(host)
}

pub fn docker(host: Option<&str>) -> Command {
    let mut cmd = Command::new("docker");
    if let Some(host) = host {
        cmd.env("DOCKER_HOST", host);
    }
    cmd
}

/// Cross-compiles the static release binaries into `target/docker/<arch>/vanillite`.
fn build_binaries(root: &Path) -> Result<()> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let mut cmd = Command::new(&cargo);
    cmd.current_dir(root)
        .args(["zigbuild", "--release", "--locked", "-p", "vanillite"])
        .env(ZIG_X86_CFLAGS.0, ZIG_X86_CFLAGS.1);
    for (_, triple, _) in ARCHES {
        cmd.args(["--target", triple]);
    }
    run(&mut cmd).map_err(|e| {
        format!("{e}\n(needs cargo-zigbuild, zig and the musl targets: see docs/development.md)")
    })?;
    // The glibc builds that load plugins (crates/vanillite/src/runtime.rs); a cargo run of
    // their own, so no more than two fat-LTO links run at once.
    let mut gnu = Command::new(&cargo);
    gnu.current_dir(root)
        .args(["zigbuild", "--release", "--locked", "-p", "vanillite"])
        .args(["--features", "mimalloc"])
        .env("CFLAGS_x86_64_unknown_linux_gnu", ZIG_X86_CFLAGS.1);
    for (_, _, target) in ARCHES {
        gnu.arg("--target").arg(format!("{target}.{GLIBC}"));
    }
    run(&mut gnu).map_err(|e| {
        format!("{e}\n(needs the targets x86_64- and aarch64-unknown-linux-gnu: see docs/development.md)")
    })?;
    // amd64 also gets the x86-64-v3 build, which the baseline binary starts where the CPU has
    // it (crates/vanillite/src/cpu.rs).
    let mut v3 = Command::new(&cargo);
    v3.current_dir(root)
        .args(["zigbuild", "--release", "--locked", "-p", "vanillite"])
        .args(["--target", ARCHES[0].1, "--target-dir", "target/v3"])
        .env(ZIG_X86_CFLAGS.0, ZIG_X86_CFLAGS.1)
        .env("RUSTFLAGS", "-C target-cpu=x86-64-v3");
    run(&mut v3)?;
    let copy = |from: std::path::PathBuf, arch: &str, name: &str| -> Result<()> {
        let dir = root.join("target/docker").join(arch);
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        std::fs::copy(&from, dir.join(name)).map_err(|e| format!("{}: {e}", from.display()))?;
        Ok(())
    };
    for (arch, triple, gnu) in ARCHES {
        copy(
            root.join("target").join(triple).join("release/vanillite"),
            arch,
            "vanillite",
        )?;
        copy(
            root.join("target").join(gnu).join("release/vanillite"),
            arch,
            GNU_BINARY,
        )?;
    }
    copy(
        root.join("target/v3")
            .join(ARCHES[0].1)
            .join("release/vanillite"),
        ARCHES[0].0,
        V3_BINARY,
    )
}

/// `docker login` with the password from the environment, never on the command line.
fn login(host: Option<&str>, registry: &str) -> Result<()> {
    let password = std::env::var(PASSWORD_ENV).map_err(|_| format!("{PASSWORD_ENV} is not set"))?;
    let user = std::env::var("VANILLITE_REGISTRY_USER").unwrap_or_else(|_| DEFAULT_USER.to_owned());
    let mut child = docker(host)
        .args(["login", registry, "--username", &user, "--password-stdin"])
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| format!("docker login: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(password.as_bytes())
            .map_err(|e| format!("docker login: {e}"))?;
    }
    let status = child.wait().map_err(|e| format!("docker login: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("docker login {registry} failed: {status}"))
    }
}

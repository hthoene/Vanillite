//! Development environment tasks: run the server locally and deploy it to the test server.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::Result;

/// SSH host of the test server; override with `VANILLITE_TEST_HOST`.
const DEFAULT_HOST: &str = "vanillite-test";
/// SSH host of the internal test server for heavy tests and experiments (#179): the default of
/// `testserver run` and `testserver nightly`.
const HEAVY_HOST: &str = "vanillite-test2";
/// The checkout `testserver run` copies the local one to (scripts/testserver/provision-test2.sh).
const WORK_SRC: &str = "/opt/vanillite-work/src";
/// Port of the main Vanillite dev server. Paper runs on 30000.
// 30001, 30002 and the proxy on 30010 follow dev automatically (vanillite-autodeploy on the
// test server); manual deploys of a branch go to 30003-30099.
const DEFAULT_PORT: u16 = 30003;
const PORTS: std::ops::RangeInclusive<u16> = 30001..=30999;

pub fn host() -> String {
    std::env::var("VANILLITE_TEST_HOST").unwrap_or_else(|_| DEFAULT_HOST.to_owned())
}

/// Takes `--host <host>` out of `args` (only before a `--`).
fn split_host<'a>(args: &[&'a str]) -> Result<(Option<String>, Vec<&'a str>)> {
    let end = args.iter().position(|a| *a == "--").unwrap_or(args.len());
    let Some(i) = args.iter().take(end).position(|a| *a == "--host") else {
        return Ok((None, args.to_vec()));
    };
    let host = args.get(i + 1).ok_or("--host needs a value")?;
    let rest = args
        .iter()
        .enumerate()
        .filter(|(j, _)| *j != i && *j != i + 1)
        .map(|(_, a)| *a)
        .collect();
    Ok((Some((*host).to_owned()), rest))
}

/// Runs a command and fails if it does not succeed.
pub fn run(cmd: &mut Command) -> Result<()> {
    let status = cmd.status().map_err(|e| format!("{cmd:?}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{cmd:?} failed: {status}"))
    }
}

/// Builds the `vanillite` binary with `profile` and returns its path.
fn build(root: &Path, profile: &str) -> Result<PathBuf> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    run(Command::new(cargo).current_dir(root).args([
        "build",
        "--locked",
        "-p",
        "vanillite",
        "--profile",
        profile,
    ]))?;
    let dir = if profile == "dev" { "debug" } else { profile };
    Ok(root.join("target").join(dir).join("vanillite"))
}

/// `cargo xtask dev [--release] [-- <server args>]`: builds and runs the server in `./run`.
pub fn dev(root: &Path, args: &[&str]) -> Result<()> {
    let (own, server_args) = match args.iter().position(|a| *a == "--") {
        Some(i) => (&args[..i], &args[i + 1..]),
        None => (args, &[][..]),
    };
    let profile = match own {
        [] => "dev",
        ["--release"] => "release-fast",
        _ => return Err(crate::USAGE.to_owned()),
    };
    let binary = build(root, profile)?;
    let run_dir = root.join("run");
    std::fs::create_dir_all(&run_dir).map_err(|e| format!("{}: {e}", run_dir.display()))?;
    run(Command::new(binary).current_dir(run_dir).args(server_args))
}

/// `cargo xtask deploy [--host <host>] [--port <port>] [--release]`: builds the server and
/// installs it as `vanillite@<port>.service` on the test server.
pub fn deploy(root: &Path, args: &[&str]) -> Result<()> {
    let (host_arg, args) = split_host(args)?;
    let mut port = DEFAULT_PORT;
    let mut profile = "release-fast";
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match *arg {
            "--port" => port = parse_port(it.next().copied())?,
            "--release" => profile = "release",
            _ => return Err(crate::USAGE.to_owned()),
        }
    }
    let binary = build(root, profile)?;
    let file = std::fs::File::open(&binary).map_err(|e| format!("{}: {e}", binary.display()))?;
    let host = host_arg.unwrap_or_else(host);
    println!("deploying {} to {host}, port {port}", binary.display());
    run(Command::new("ssh")
        .args([host.as_str(), "vanillite-deploy", &port.to_string()])
        .stdin(Stdio::from(file)))
}

/// `cargo xtask testserver [--host <host>] provision|status|logs|start|stop|restart [<port>]`,
/// `cargo xtask testserver incidents [list [<count>] | show <id> | fetch [<id>...]]`,
/// `cargo xtask testserver [--host <host>] run [--fetch <path>]... -- <xtask args>` and
/// `cargo xtask testserver [--host <host>] nightly [status | fetch [<run>]]`.
pub fn testserver(root: &Path, args: &[&str]) -> Result<()> {
    let (host_arg, args) = split_host(args)?;
    let args = args.as_slice();
    if let ["run", rest @ ..] = args {
        return remote_run(
            root,
            &host_arg.unwrap_or_else(|| HEAVY_HOST.to_owned()),
            rest,
        );
    }
    if let ["nightly", rest @ ..] = args {
        return nightly(
            root,
            &host_arg.unwrap_or_else(|| HEAVY_HOST.to_owned()),
            rest,
        );
    }
    let host = host_arg.unwrap_or_else(host);
    let unit = |port: Option<&str>| -> Result<String> {
        Ok(format!(
            "vanillite@{}.service",
            parse_port(port.or(Some("30001")))?
        ))
    };
    match args {
        ["provision"] => {
            let script = root.join(if host == HEAVY_HOST {
                "scripts/testserver/provision-test2.sh"
            } else {
                "scripts/testserver/provision.sh"
            });
            let file =
                std::fs::File::open(&script).map_err(|e| format!("{}: {e}", script.display()))?;
            run(Command::new("ssh")
                .args([host.as_str(), "bash", "-s"])
                .stdin(Stdio::from(file)))
        }
        ["status"] if host == HEAVY_HOST => ssh(
            &host,
            "systemctl --no-pager --all list-units paper.service vanilla.service \
             vanillite-nightly.service 'vanillite@*'; echo; ss -ltnp | grep -E ':30[0-9]{3} ' || true; \
             echo; echo \"lock: $(vanillite-lock status)\"; df -h /; echo; vanillite-nightly status 1",
        ),
        ["status"] => ssh(
            &host,
            "systemctl --no-pager --all list-units paper.service 'vanillite@*'; \
             echo; ss -ltnp | grep -E ':30[0-9]{3} ' || true; echo; vanillite-autodeploy status; \
             echo; echo 'incidents (cargo xtask testserver incidents):'; vanillite-incidents list 5",
        ),
        ["incidents"] => ssh(&host, "vanillite-incidents list"),
        ["incidents", "list"] => ssh(&host, "vanillite-incidents list"),
        ["incidents", "list", count] => {
            let count: u32 = count.parse().map_err(|e| format!("count: {e}"))?;
            ssh(&host, &format!("vanillite-incidents list {count}"))
        }
        ["incidents", "show", id] => ssh(
            &host,
            &format!("vanillite-incidents show {}", incident_id(id)?),
        ),
        ["incidents", "fetch", ids @ ..] => fetch_incidents(root, &host, ids),
        ["logs", port @ ..] => run(Command::new("ssh").args([
            "-t",
            host.as_str(),
            "journalctl",
            "-f",
            "-n",
            "200",
            "-u",
            &unit(port.first().copied())?,
        ])),
        ["start", port @ ..] => ssh(
            &host,
            &format!("systemctl enable --now {}", unit(port.first().copied())?),
        ),
        ["stop", port @ ..] => ssh(
            &host,
            &format!("systemctl disable --now {}", unit(port.first().copied())?),
        ),
        ["restart", port @ ..] => ssh(
            &host,
            &format!("systemctl restart {}", unit(port.first().copied())?),
        ),
        _ => Err(crate::USAGE.to_owned()),
    }
}

/// An incident id as `vanillite-incidents` writes it (`20261125-102719-30001-crash`).
fn incident_id(id: &str) -> Result<&str> {
    if !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        Ok(id)
    } else {
        Err(format!("not an incident id: {id}"))
    }
}

/// Downloads incident bundles into `target/incidents/<id>/`: the given ones, or all that are
/// not there yet.
fn fetch_incidents(root: &Path, host: &str, ids: &[&str]) -> Result<()> {
    let dest = root.join("target/incidents");
    std::fs::create_dir_all(&dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    let wanted: Vec<String> = if ids.is_empty() {
        let out = Command::new("ssh")
            .args([host, "vanillite-incidents list 1000"])
            .output()
            .map_err(|e| format!("ssh {host}: {e}"))?;
        if !out.status.success() {
            return Err(format!("vanillite-incidents list failed: {}", out.status));
        }
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|line| line.split_whitespace().next())
            .filter(|id| !dest.join(id).exists())
            .map(str::to_owned)
            .collect()
    } else {
        ids.iter()
            .map(|id| incident_id(id).map(str::to_owned))
            .collect::<Result<_>>()?
    };
    if wanted.is_empty() {
        println!("no new incidents");
        return Ok(());
    }
    let mut remote = Command::new("ssh")
        .args([
            host,
            "tar",
            "-C",
            "/var/lib/vanillite-incidents",
            "-cf",
            "-",
        ])
        .args(&wanted)
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("ssh {host}: {e}"))?;
    let tar = remote.stdout.take().ok_or("no output from ssh")?;
    run(Command::new("tar")
        .args(["-xf", "-", "-C"])
        .arg(&dest)
        .stdin(Stdio::from(tar)))?;
    let status = remote.wait().map_err(|e| format!("ssh {host}: {e}"))?;
    if !status.success() {
        return Err(format!("downloading the incidents failed: {status}"));
    }
    for id in &wanted {
        println!("{}", dest.join(id).join("summary.txt").display());
    }
    Ok(())
}

/// `cargo xtask testserver run [--fetch <path>]... -- <xtask args>`: copies this checkout
/// (without `target/` and `.git/`) to the heavy test server and runs `cargo xtask <args>` there
/// while holding its lock, then copies the `--fetch` paths (relative to the checkout) back.
fn remote_run(root: &Path, host: &str, args: &[&str]) -> Result<()> {
    let split = args
        .iter()
        .position(|a| *a == "--")
        .ok_or("testserver run: give the xtask command after --")?;
    let (own, xargs) = args.split_at(split);
    let xargs = xargs.get(1..).unwrap_or_default();
    if xargs.is_empty() {
        return Err(crate::USAGE.to_owned());
    }
    let mut fetch = Vec::new();
    let mut it = own.iter();
    while let Some(arg) = it.next() {
        match *arg {
            "--fetch" => fetch.push(repo_path(it.next().copied())?),
            _ => return Err(crate::USAGE.to_owned()),
        }
    }
    let who = root
        .parent()
        .and_then(Path::file_name)
        .map_or_else(|| "?".into(), |n| n.to_string_lossy().into_owned());
    let owner = format!("xtask {} from {who}", xargs.join(" "));
    let _lock = crate::loadtest::Lock::take_with(host, "vanillite-lock", &owner)?;
    println!("copying {} to {host}:{WORK_SRC}", root.display());
    run(Command::new("rsync")
        .current_dir(root)
        .args([
            "-a",
            "--delete",
            "--exclude=/target",
            "--exclude=/.git",
            "--filter=:- .gitignore",
            "--chown=vanillite-work:vanillite-work",
            "./",
        ])
        .arg(format!("{host}:{WORK_SRC}/")))?;
    let command = std::iter::once("vanillite-work run".to_owned())
        .chain(
            xargs
                .iter()
                .map(|a| format!("'{}'", a.replace('\'', "'\\''"))),
        )
        .collect::<Vec<_>>()
        .join(" ");
    let result = ssh(host, &command);
    for path in &fetch {
        let dest = root.join(path);
        let parent = dest.parent().unwrap_or(root);
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        run(Command::new("rsync")
            .arg("-a")
            .arg(format!("{host}:{WORK_SRC}/{path}"))
            .arg(parent))?;
        println!("fetched {}", dest.display());
    }
    result
}

/// A relative path inside the checkout (`--fetch`).
fn repo_path(arg: Option<&str>) -> Result<String> {
    let path = arg.ok_or("--fetch needs a path")?.trim_end_matches('/');
    let ok = !path.is_empty()
        && Path::new(path)
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)));
    if ok {
        Ok(path.to_owned())
    } else {
        Err(format!(
            "--fetch: not a relative path inside the checkout: {path}"
        ))
    }
}

/// `cargo xtask testserver nightly [status | fetch [<run>]]`: the summaries of the newest
/// nightly heavy test runs, or download one run (default the newest) into `target/nightly/`.
fn nightly(root: &Path, host: &str, args: &[&str]) -> Result<()> {
    let run_name = match args {
        [] | ["status"] => return ssh(host, "vanillite-nightly status"),
        ["fetch"] => "$(ls -1 | sort | tail -n 1)".to_owned(),
        ["fetch", name] => incident_id(name)?.to_owned(),
        _ => return Err(crate::USAGE.to_owned()),
    };
    let dest = root.join("target/nightly");
    std::fs::create_dir_all(&dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    let mut remote = Command::new("ssh")
        .args([
            host,
            &format!("cd /var/lib/vanillite-nightly && tar -cf - \"{run_name}\""),
        ])
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("ssh {host}: {e}"))?;
    let tar = remote.stdout.take().ok_or("no output from ssh")?;
    run(Command::new("tar")
        .args(["-xvf", "-", "-C"])
        .arg(&dest)
        .stdin(Stdio::from(tar)))?;
    let status = remote.wait().map_err(|e| format!("ssh {host}: {e}"))?;
    if !status.success() {
        return Err(format!("downloading the nightly run failed: {status}"));
    }
    println!("in {}", dest.display());
    Ok(())
}

fn ssh(host: &str, command: &str) -> Result<()> {
    run(Command::new("ssh").args([host, command]))
}

fn parse_port(arg: Option<&str>) -> Result<u16> {
    let port: u16 = arg
        .ok_or("--port needs a value")?
        .parse()
        .map_err(|e| format!("port: {e}"))?;
    if PORTS.contains(&port) {
        Ok(port)
    } else {
        Err(format!(
            "port {port} is outside {}-{} (30000 is the Paper reference server)",
            PORTS.start(),
            PORTS.end()
        ))
    }
}

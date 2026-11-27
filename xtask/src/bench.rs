//! `cargo xtask bench`: the benchmark suite. Every scenario runs against Vanillite and the
//! reference servers (Paper, Vanilla, Pumpkin; Velocity for the proxy) on the test server with
//! the same settings (`vanillite-bench props`: seed, normal world, peaceful, view and
//! simulation distance 10, offline mode) and a fresh world per scenario. The load bots
//! (`tools/loadbot`) run here, as in `cargo xtask loadtest`.
//!
//! Results: `target/bench/<time>/results.json` (full), raw per-server files under
//! /opt/vanillite/bench/suite/<time>/ on the test server, and with `--publish` the summaries in
//! `benchmarks/results/latest.json` and `history.json` plus the generated
//! `benchmarks/README.md` that the documentation reads. `bench nightly` runs the whole suite
//! for a new dev commit in a worktree and pushes the results on a branch.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::Result;
use crate::loadtest::{self, Options, Session, Target, round, show};

const RESULTS: &str = "benchmarks/results";
const REMOTE_RESULTS: &str = "/opt/vanillite/bench/suite";
/// History entries kept in `history.json` (one per run).
const HISTORY_MAX: usize = 120;
/// Corner of the build area of the command-built scenarios (x, y, z): high above the terrain.
const AREA: (i32, i32, i32) = (512, 280, 512);
/// The observer bot of the command-built scenarios (the first bot of `tools/loadbot`).
const OBSERVER: &str = "bot00000";
/// Every metric is the result of one run; this is the usual spread between nightly runs.
const NOISE: &str = "±10 %";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Better {
    Lower,
    Higher,
}

type Build = fn(i32, i32, i32) -> Vec<String>;
/// Players, bot movement, and join rate with concurrent logins (`None`: 20 per s, 4).
type Step = (u64, &'static str, Option<(f64, usize)>);
type Cycle = fn(i32, i32, i32, usize) -> Vec<String>;

enum Kind {
    /// Cold start (new world), warm start, idle RSS and CPU.
    Startup,
    /// Load bots in steps of (players, movement, join rate and concurrent logins).
    Players {
        steps: &'static [Step],
        settle: u64,
        hold: u64,
    },
    /// One observer bot; console commands build a contraption, optional commands repeat
    /// every `period` seconds during the measurement.
    Built {
        setup: Build,
        cycle: Option<(u64, Cycle)>,
    },
    /// World pre-generation of 101 × 101 chunks.
    Pregen,
    /// `save-all flush` after bots flew through new terrain.
    Save,
    /// Small container: a CPU quota in percent of one core and a memory limit in MiB; idle
    /// RSS, then one bot through the steps with the movement checks on.
    Weak {
        cpu_percent: u32,
        memory_mib: u32,
        steps: &'static [Step],
        settle: u64,
        hold: u64,
    },
}

struct Scenario {
    name: &'static str,
    title: &'static str,
    what: &'static str,
    servers: &'static [&'static str],
    /// Label, unit and direction of the one number per server in the summary.
    headline: (&'static str, &'static str, Better),
    kind: Kind,
}

const WORLD_SERVERS: &[&str] = &["vanillite", "paper", "vanilla", "pumpkin"];
const PLAYER_SERVERS: &[&str] = &[
    "vanillite",
    "vanillite-paper",
    "paper",
    "vanilla",
    "pumpkin",
];

const SCENARIOS: &[Scenario] = &[
    Scenario {
        name: "startup",
        title: "Startup time and idle memory",
        what: "Start with a new world until the server answers a status ping (cold), restart \
               with that world (warm), then RSS and CPU of the empty, ticking server over 30 s.",
        servers: WORLD_SERVERS,
        headline: ("idle RSS", "MiB", Better::Lower),
        kind: Kind::Startup,
    },
    Scenario {
        name: "redstone",
        title: "Redstone: clocks, dust lines and pistons",
        what: "93 observer clocks, each driving a 15-block redstone dust line into a sticky \
               piston that moves a block (1 395 dust, 93 pistons), on a platform at y 280.",
        servers: WORLD_SERVERS,
        headline: ("mean MSPT", "ms", Better::Lower),
        kind: Kind::Built {
            setup: redstone,
            cycle: None,
        },
    },
    Scenario {
        name: "mobs",
        title: "Entity farm: 500 cows in a pen",
        what: "500 cows summoned on a grid in a 64 × 64 glass pen (mob AI, pathfinding, \
               collisions, tracking for one player).",
        servers: WORLD_SERVERS,
        headline: ("mean MSPT", "ms", Better::Lower),
        kind: Kind::Built {
            setup: mobs,
            cycle: None,
        },
    },
    Scenario {
        name: "items",
        title: "Dropped items: 2 000 item entities",
        what: "2 000 cobblestone item entities 1.25 blocks apart (no merging, no despawn, no \
               pickup) on a platform.",
        servers: WORLD_SERVERS,
        headline: ("mean MSPT", "ms", Better::Lower),
        kind: Kind::Built {
            setup: items,
            cycle: None,
        },
    },
    Scenario {
        name: "hoppers",
        title: "Hoppers: 1 024 hoppers moving items in rings",
        what: "256 rings of 4 hoppers that pass a stack of 64 items around forever.",
        servers: WORLD_SERVERS,
        headline: ("mean MSPT", "ms", Better::Lower),
        kind: Kind::Built {
            setup: hoppers,
            cycle: None,
        },
    },
    Scenario {
        name: "tnt",
        title: "TNT: chain explosions",
        what: "A 16 × 10 × 16 block of TNT (2 560) ignited in the air every 20 s, three times.",
        servers: WORLD_SERVERS,
        headline: ("mean MSPT", "ms", Better::Lower),
        kind: Kind::Built {
            setup: no_setup,
            cycle: Some((20, tnt)),
        },
    },
    Scenario {
        name: "fluids",
        title: "Flowing water and lava",
        what: "32 water and 32 lava sources placed on a walled 64 × 64 platform, removed again \
               10 s later, in turns (spreading and drying up).",
        servers: WORLD_SERVERS,
        headline: ("mean MSPT", "ms", Better::Lower),
        kind: Kind::Built {
            setup: fluids_setup,
            cycle: Some((10, fluids)),
        },
    },
    Scenario {
        name: "light",
        title: "Light updates",
        what: "A 32 × 32 × 32 cube filled with glowstone and emptied again every 2 s \
               (32 768 light sources appearing and disappearing, with block updates).",
        servers: WORLD_SERVERS,
        headline: ("mean MSPT", "ms", Better::Lower),
        kind: Kind::Built {
            setup: no_setup,
            cycle: Some((2, light)),
        },
    },
    Scenario {
        name: "save",
        title: "World save",
        what: "20 bots fly at boat speed (72.7 blocks/s) through new terrain for 60 s, then \
               `save-all flush` until the server reports that everything is on disk.",
        servers: &["vanillite", "paper", "vanilla"],
        headline: ("save-all flush", "s", Better::Lower),
        kind: Kind::Save,
    },
    Scenario {
        name: "pregen",
        title: "World pre-generation",
        what: "Generate the 101 × 101 chunks (10 201) around 0, 0 on 4 cores (Vanillite also on \
               1 and 2 threads), median of 3 runs. \
               Vanillite: the chunk pipeline benchmark (`chunk_bench`: generation, light, \
               storage, then loading the same chunks from disk). Paper: the Chunky plugin \
               (radius 800, square) in the running server.",
        servers: &["vanillite", "paper"],
        headline: ("chunks generated per second", "chunks/s", Better::Higher),
        kind: Kind::Pregen,
    },
    Scenario {
        name: "weak",
        title: "Weak hardware: 1 core, 1 GiB",
        what: "Vanillite in a cgroup with a CPU quota of one core and a memory limit of \
               1 GiB, like a small Pterodactyl server: idle RSS, then one player walks \
               (sprint speed) and flies (10 blocks/s) straight through new terrain with the \
               movement checks on. Rubber-banding: server teleports of the bot.",
        servers: &["vanillite"],
        headline: ("MSPT max with one moving player", "ms", Better::Lower),
        kind: Kind::Weak {
            cpu_percent: 100,
            memory_mib: 1024,
            steps: &[(1, "straight:sprint", None), (1, "straight:10", None)],
            settle: 5,
            hold: 60,
        },
    },
    Scenario {
        name: "proxy",
        title: "Proxy relay",
        what: "Players join through a proxy on 30900 into Vanillite on 30901 (modern \
               forwarding) and walk randomly: cost of the proxy process.",
        servers: &["vanillite-proxy", "velocity"],
        headline: ("proxy CPU at 500 players", "%", Better::Lower),
        kind: Kind::Players {
            steps: &[(100, "random", None), (500, "random", None)],
            settle: 10,
            hold: 50,
        },
    },
    Scenario {
        name: "flight",
        title: "Flying players: chunk delivery",
        what: "Bots fly straight at y 300 in random directions, first 50 with elytra speed \
               (33.5 blocks/s), then 100 at boat speed (72.7 blocks/s): new terrain is \
               generated and sent all the time; the movement check game rule is off on every \
               server. Holes: chunks within 2 chunks of a bot that it does not have, sampled \
               every 5 s. Full view: from connecting until every chunk within the view distance \
               (circle) is there. Server CPU per chunk: CPU time of the server process divided \
               by the chunks the bots received.",
        servers: WORLD_SERVERS,
        headline: ("holes near players at 100 boats", "%", Better::Lower),
        kind: Kind::Players {
            steps: &[(50, "straight:elytra", None), (100, "straight:boat", None)],
            settle: 10,
            hold: 50,
        },
    },
    Scenario {
        name: "spread",
        title: "Players spread out",
        what: "Bots fly to random points within 1 500 blocks of the spawn and stay there: \
               every player has its own chunks.",
        servers: PLAYER_SERVERS,
        headline: ("MSPT p95 at 500 players", "ms", Better::Lower),
        kind: Kind::Players {
            steps: &[
                (100, "spread:1500", None),
                (250, "spread:1500", None),
                (500, "spread:1500", None),
            ],
            settle: 30,
            hold: 50,
        },
    },
    Scenario {
        name: "join-storm",
        title: "Join storm",
        what: "1 000 bots join at 20 per second; then 1 000 more connect at once (up to 256 \
               logins at the same time).",
        servers: PLAYER_SERVERS,
        headline: ("time until the 1 000 more are in", "s", Better::Lower),
        kind: Kind::Players {
            steps: &[
                (1000, "random", None),
                (2000, "random", Some((1000.0, 256))),
            ],
            settle: 0,
            hold: 10,
        },
    },
    Scenario {
        name: "dense",
        title: "Many players close together",
        what: "Bots walk randomly within 48 blocks of the spawn, 60 s per step (measured over \
               the last 50 s), up to 5 000. A server that stops answering or drops the bots \
               ends its ramp there.",
        servers: PLAYER_SERVERS,
        headline: (
            "most players with MSPT p95 under 50 ms",
            "players",
            Better::Higher,
        ),
        kind: Kind::Players {
            steps: &[
                (100, "random", None),
                (500, "random", None),
                (1000, "random", None),
                (2000, "random", None),
                (5000, "random", None),
            ],
            settle: 10,
            hold: 50,
        },
    },
];

/// Column order and display names of the servers.
const SERVERS: &[(&str, &str)] = &[
    ("vanillite", "Vanillite"),
    ("vanillite-paper", "Vanillite (paper profile)"),
    ("paper", "Paper"),
    ("vanilla", "Vanilla"),
    ("pumpkin", "Pumpkin"),
    ("vanillite-proxy", "vanillite-proxy"),
    ("velocity", "Velocity"),
];

fn display(server: &str) -> &str {
    SERVERS
        .iter()
        .find(|(id, _)| *id == server)
        .map_or(server, |(_, name)| name)
}

/// The test server target and the Vanillite rule profile of a server id.
fn target_of(server: &str) -> Result<(Target, Option<&'static str>)> {
    Ok(match server {
        "vanillite" => (Target::Vanillite, None),
        "vanillite-paper" => (Target::Vanillite, Some("paper")),
        "paper" => (Target::Paper, None),
        "vanilla" => (Target::Vanilla, None),
        "pumpkin" => (Target::Pumpkin, None),
        "velocity" => (Target::Velocity, None),
        "vanillite-proxy" => (Target::VanilliteProxy, None),
        s => return Err(format!("unknown server {s:?}")),
    })
}

struct Ctx<'a> {
    host: String,
    root: &'a Path,
    bin_dir: std::path::PathBuf,
    /// Version strings from the status pings.
    versions: BTreeMap<String, String>,
}

/// `cargo xtask bench ...`.
pub fn run(root: &Path, args: &[&str]) -> Result<()> {
    match args {
        ["list"] => {
            for s in SCENARIOS {
                println!("{:<11} {} ({})", s.name, s.title, s.servers.join(", "));
            }
            Ok(())
        }
        ["report"] => report(root),
        ["nightly", rest @ ..] => nightly(root, rest),
        _ => suite(root, args),
    }
}

fn suite(root: &Path, args: &[&str]) -> Result<()> {
    let mut names = Vec::new();
    let mut only_servers: Option<Vec<String>> = None;
    let mut publish = false;
    let mut deploy = true;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match *arg {
            "--servers" => {
                only_servers = Some(
                    it.next()
                        .ok_or("--servers needs a value")?
                        .split(',')
                        .map(str::to_owned)
                        .collect(),
                );
            }
            "--publish" => publish = true,
            "--no-deploy" => deploy = false,
            "all" => names.extend(SCENARIOS.iter().map(|s| s.name)),
            name if SCENARIOS.iter().any(|s| s.name == name) => names.push(name),
            _ => return Err(crate::USAGE.to_owned()),
        }
    }
    if names.is_empty() {
        return Err(crate::USAGE.to_owned());
    }
    let scenarios: Vec<&Scenario> = SCENARIOS
        .iter()
        .filter(|s| names.contains(&s.name))
        .collect();

    let commit = loadtest::git(root, &["rev-parse", "--short=12", "HEAD"])?;
    let code_commit = loadtest::git(
        root,
        &[
            "log",
            "-1",
            "--format=%h",
            "--abbrev=12",
            "HEAD",
            "--",
            ".",
            ":(exclude)benchmarks",
        ],
    )?;
    let dirty = !loadtest::git(
        root,
        &[
            "status",
            "--porcelain",
            "--untracked-files=no",
            "--",
            ".",
            ":(exclude)benchmarks",
        ],
    )?
    .is_empty();
    if publish && dirty {
        return Err("--publish needs a checkout without local changes".into());
    }
    let stamp = loadtest::command_output(Command::new("date").args(["-u", "+%Y%m%dT%H%M%SZ"]))?;
    let mut ctx = Ctx {
        host: std::env::var("VANILLITE_TEST_HOST").unwrap_or_else(|_| "vanillite-test".into()),
        root,
        bin_dir: root.join("target/release-fast"),
        versions: BTreeMap::new(),
    };
    let _lock = loadtest::Lock::take(&ctx.host, &format!("xtask bench from {}", root.display()))?;
    prepare(&ctx, &scenarios, deploy)?;

    let out_dir = root.join("target/bench").join(&stamp);
    let mut results = Map::new();
    for scenario in &scenarios {
        let mut per_server = Map::new();
        for &server in scenario.servers {
            if only_servers
                .as_ref()
                .is_some_and(|o| !o.iter().any(|s| s == server))
            {
                continue;
            }
            println!("== {} on {}", scenario.name, server);
            let mut result = match run_one(&mut ctx, scenario, server) {
                Ok(r) => r,
                Err(e) => match e.strip_prefix("unsupported: ") {
                    Some(why) => json!({ "status": "unsupported", "note": first_line(why) }),
                    None => json!({ "status": "error", "note": first_line(&e) }),
                },
            };
            if result["status"].is_null() {
                result["status"] = json!("ok");
            }
            result["headline"] = headline(scenario, &result);
            println!(
                "   {}: {} {} ({})",
                scenario.headline.0,
                show(&result["headline"]),
                scenario.headline.1,
                result["status"].as_str().unwrap_or("?"),
            );
            // Never leave a benchmark instance running.
            if let Ok((target, _)) = target_of(server) {
                let _ = bench_cmd(&ctx.host, &["stop", target.name()]);
                if matches!(target, Target::Velocity | Target::VanilliteProxy) {
                    let _ = bench_cmd(&ctx.host, &["stop", "vanillite"]);
                }
            }
            if !dirty {
                let _ = loadtest::store(
                    &ctx.host,
                    &result,
                    &format!("{REMOTE_RESULTS}/{stamp}/{}-{server}.json", scenario.name),
                );
            }
            per_server.insert(server.to_owned(), result);
        }
        results.insert(
            scenario.name.to_owned(),
            json!({
                "title": scenario.title,
                "what": scenario.what,
                "headline": {
                    "label": scenario.headline.0,
                    "unit": scenario.headline.1,
                    "better": match scenario.headline.2 { Better::Lower => "lower", Better::Higher => "higher" },
                },
                "date": stamp,
                "commit": code_commit,
                "results": per_server,
            }),
        );
    }
    let run = json!({
        "suite": 1,
        "date": stamp,
        "commit": commit,
        "code_commit": code_commit,
        "dirty": dirty,
        "environment": environment(&ctx),
        "scenarios": results,
    });
    let text = serde_json::to_string_pretty(&run).map_err(|e| e.to_string())?;
    crate::write(&out_dir.join("results.json"), &text)?;
    println!("wrote {}", out_dir.join("results.json").display());
    if publish {
        publish_results(root, &run)?;
    }
    Ok(())
}

/// Builds and installs what the scenarios need.
fn prepare(ctx: &Ctx, scenarios: &[&Scenario], deploy: bool) -> Result<()> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let mut build = vec![
        "build",
        "--locked",
        "--profile",
        "release-fast",
        "-p",
        "loadbot",
    ];
    if deploy {
        build.extend(["-p", "vanillite", "-p", "vanillite-proxy"]);
    }
    crate::devenv::run(Command::new(&cargo).current_dir(ctx.root).args(&build))?;
    if deploy && scenarios.iter().any(|s| matches!(s.kind, Kind::Pregen)) {
        crate::devenv::run(Command::new(&cargo).current_dir(ctx.root).args([
            "build",
            "--locked",
            "--profile",
            "release-fast",
            "-p",
            "vanillite-server",
            "--example",
            "chunk_bench",
        ]))?;
        install(
            &ctx.host,
            "chunk_bench",
            &ctx.bin_dir.join("examples/chunk_bench"),
        )?;
    }
    if !deploy {
        return Ok(());
    }
    install(
        &ctx.host,
        "vanillite-proxy",
        &ctx.bin_dir.join("vanillite-proxy"),
    )?;
    let binary = ctx.bin_dir.join("vanillite");
    let file = std::fs::File::open(&binary).map_err(|e| format!("{}: {e}", binary.display()))?;
    crate::devenv::run(
        Command::new("ssh")
            .args([&ctx.host, "vanillite-deploy", "30901"])
            .stdin(Stdio::from(file)),
    )?;
    bench_cmd(&ctx.host, &["stop", "vanillite"]).map(drop)
}

fn install(host: &str, name: &str, file: &Path) -> Result<()> {
    let f = std::fs::File::open(file).map_err(|e| format!("{}: {e}", file.display()))?;
    crate::devenv::run(
        Command::new("ssh")
            .args([host, "vanillite-bench", "install", name])
            .stdin(Stdio::from(f)),
    )
}

fn run_one(ctx: &mut Ctx, scenario: &Scenario, server: &str) -> Result<Value> {
    let (target, profile) = target_of(server)?;
    match &scenario.kind {
        Kind::Startup => startup(ctx, server, target, profile),
        Kind::Players {
            steps,
            settle,
            hold,
        } => players(ctx, server, target, profile, steps, *settle, *hold),
        Kind::Built { setup, cycle } => built(ctx, server, target, *setup, *cycle),
        Kind::Pregen => pregen(ctx, target),
        Kind::Save => save(ctx, server, target),
        Kind::Weak {
            cpu_percent,
            memory_mib,
            steps,
            settle,
            hold,
        } => weak(
            ctx,
            server,
            target,
            (*cpu_percent, *memory_mib),
            steps,
            *settle,
            *hold,
        ),
    }
}

/// [`Kind::Weak`]: the limits apply from the start; `reset` drops them again.
fn weak(
    ctx: &mut Ctx,
    server: &str,
    target: Target,
    (cpu_percent, memory_mib): (u32, u32),
    steps: &[Step],
    settle: u64,
    hold: u64,
) -> Result<Value> {
    bench_cmd(&ctx.host, &["reset", target.name()])?;
    bench_cmd(
        &ctx.host,
        &[
            "limit",
            target.name(),
            &cpu_percent.to_string(),
            &memory_mib.to_string(),
        ],
    )?;
    let result = (|| {
        let started = bench_json(&ctx.host, &["start", target.name()])?;
        if let Some(v) = started["version"].as_str() {
            ctx.versions.insert(server.to_owned(), v.to_owned());
        }
        std::thread::sleep(Duration::from_secs(15));
        let idle = bench_json(&ctx.host, &["idle", target.name(), "30"])?;
        let mut o = options(steps, settle, hold);
        o.join_rate = 1.0;
        let mut session = Session::open(&ctx.host, target, &ctx.bin_dir, &o, &[])?;
        let measured = loadtest::ramp(&o, &mut session);
        session.close();
        let measured = measured?;
        let max_mspt = measured
            .iter()
            .filter_map(|s| s["mspt"]["max"].as_f64())
            .fold(None, |m: Option<f64>, v| Some(m.map_or(v, |m| m.max(v))));
        let teleports: u64 = measured
            .iter()
            .filter_map(|s| s["bots"]["teleports"].as_u64())
            .sum();
        let mut result = json!({
            "cpu_quota_percent": cpu_percent,
            "memory_limit_mib": memory_mib,
            "start_s": started["ready_s"],
            "idle_rss_mib": idle["rss_mib"],
            "idle_cpu_percent": idle["cpu_percent"],
            "max_mspt": round(max_mspt),
            "teleports": teleports,
            "steps": measured.iter().map(compact_step).collect::<Vec<_>>(),
        });
        if measured.len() < o.steps.len() {
            result["status"] = json!("failed");
            result["note"] = json!("the bot did not stay online");
        }
        Ok(result)
    })();
    let _ = bench_cmd(&ctx.host, &["reset", target.name()]);
    result
}

/// Fresh world, then start; records the version. With a proxy target the backend is
/// Vanillite on 30901.
fn fresh_start(
    ctx: &mut Ctx,
    server: &str,
    target: Target,
    profile: Option<&str>,
) -> Result<Value> {
    let proxy = matches!(target, Target::Velocity | Target::VanilliteProxy);
    if proxy {
        bench_cmd(&ctx.host, &["reset", "vanillite"])?;
        bench_json(&ctx.host, &["start", "vanillite"])?;
    } else {
        bench_cmd(&ctx.host, &["reset", target.name()])?;
    }
    let mut args = vec!["start", target.name()];
    args.extend(profile);
    let started = bench_json(&ctx.host, &args)?;
    if let Some(v) = started["version"].as_str() {
        ctx.versions.insert(server.to_owned(), v.to_owned());
    }
    let expected = protocol(ctx.root)?;
    if !proxy && started["protocol"].as_i64() != Some(expected) {
        return Err(format!(
            "unsupported: speaks protocol {} ({}), the bots speak {expected}",
            started["protocol"],
            started["version"].as_str().unwrap_or("?"),
        ));
    }
    Ok(started)
}

fn startup(ctx: &mut Ctx, server: &str, target: Target, profile: Option<&str>) -> Result<Value> {
    let cold = fresh_start(ctx, server, target, profile)?;
    std::thread::sleep(Duration::from_secs(20));
    bench_cmd(&ctx.host, &["stop", target.name()])?;
    let mut args = vec!["start", target.name()];
    args.extend(profile);
    let warm = bench_json(&ctx.host, &args)?;
    std::thread::sleep(Duration::from_secs(30));
    let idle = bench_json(&ctx.host, &["idle", target.name(), "30"])?;
    Ok(json!({
        "version": cold["version"],
        "protocol": cold["protocol"],
        "cold_start_s": cold["ready_s"],
        "warm_start_s": warm["ready_s"],
        "idle_rss_mib": idle["rss_mib"],
        "idle_cpu_percent": idle["cpu_percent"],
    }))
}

fn options(steps: &[Step], settle: u64, hold: u64) -> Options {
    Options {
        targets: Vec::new(),
        steps: steps.iter().map(|s| s.0).collect(),
        movements: steps.iter().map(|s| s.1.to_owned()).collect(),
        rates: steps.iter().map(|s| s.2).collect(),
        hold: Duration::from_secs(hold),
        settle: Duration::from_secs(settle),
        join_rate: 20.0,
        chat_interval: 0.0,
        view_distance: 10,
        profile: "release-fast",
        deploy: false,
        gate: false,
    }
}

fn players(
    ctx: &mut Ctx,
    server: &str,
    target: Target,
    profile: Option<&str>,
    steps: &[Step],
    settle: u64,
    hold: u64,
) -> Result<Value> {
    fresh_start(ctx, server, target, profile)?;
    if steps
        .iter()
        .any(|s| s.1.starts_with("straight") || s.1.starts_with("spread"))
        && !matches!(target, Target::Velocity | Target::VanilliteProxy)
    {
        // Fast flight is not a cheat here: no server teleports the bots back. A server
        // without the rule flies them anyway or rejects; the teleports column shows it.
        let _ = commands(
            &ctx.host,
            target,
            &["gamerule player_movement_check false".to_owned()],
        )?;
    }
    let o = options(steps, settle, hold);
    let mut session = Session::open(&ctx.host, target, &ctx.bin_dir, &o, &[])?;
    let measured = loadtest::ramp(&o, &mut session);
    session.close();
    let measured = measured?;
    let last = measured.last().cloned().unwrap_or_default();
    let wanted = *o.steps.last().unwrap_or(&0);
    let reached = last["players"].as_u64().unwrap_or(0);
    let mut result = json!({
        "max_players_under_50ms": loadtest::max_players_under(&measured, 50.0),
        "steps": measured.iter().map(compact_step).collect::<Vec<_>>(),
    });
    if measured.len() < o.steps.len() || reached * 100 < wanted * 95 {
        let failed_at = o
            .steps
            .get(measured.len().saturating_sub(1))
            .copied()
            .unwrap_or(0);
        result["status"] = json!("failed");
        result["note"] = json!(format!("failed at {failed_at} players ({reached} online)"));
    }
    Ok(result)
}

/// The step metrics kept in the published results.
fn compact_step(step: &Value) -> Value {
    let pick = |path: &[&str]| {
        let mut v = step;
        for p in path {
            v = &v[*p];
        }
        v.clone()
    };
    json!({
        "target": step["target"],
        "players": step["players"],
        "movement": step["movement"],
        "ramp_s": step["ramp_s"],
        "ramp_mspt_max": step["ramp_mspt_max"],
        "mspt": {
            "mean": pick(&["mspt", "mean"]),
            "p50": pick(&["mspt", "p50"]),
            "p95": pick(&["mspt", "p95"]),
            "p99": pick(&["mspt", "p99"]),
            "max": pick(&["mspt", "max"]),
        },
        "rss_mib": pick(&["server", "rss_mib"]),
        "rss_net_per_player_kib": pick(&["server", "rss_net_per_player_kib"]),
        "cpu_percent": pick(&["server", "cpu_percent"]),
        "net_tx_mbps": pick(&["server", "net_tx_mbps"]),
        "in_kbps_per_bot": pick(&["bots", "in_kbps_per_bot"]),
        "join_ms_p95": pick(&["bots", "join_ms_p95"]),
        "rtt_ms_p95": pick(&["bots", "rtt_ms_p95"]),
        "chunks_per_s_per_bot": pick(&["bots", "chunks_per_s_per_bot"]),
        "near_missing_percent": pick(&["bots", "near_missing_percent"]),
        "full_view_ms_p50": pick(&["bots", "full_view_ms_p50"]),
        "full_view_ms_p95": pick(&["bots", "full_view_ms_p95"]),
        "chunk_ms_p50": pick(&["bots", "chunk_ms_p50"]),
        "chunk_ms_p95": pick(&["bots", "chunk_ms_p95"]),
        "teleports": pick(&["bots", "teleports"]),
        // Server CPU per chunk the bots received: steadier than wall time on a busy machine.
        "cpu_ms_per_chunk": match (step["server"]["cpu_percent"].as_f64(), step["bots"]["chunks_per_s"].as_f64()) {
            (Some(cpu), Some(chunks)) if chunks > 0.0 => round(Some(cpu * 10.0 / chunks)),
            _ => Value::Null,
        },
        "failed": pick(&["bots", "failed"]),
        "disconnected": pick(&["bots", "disconnected"]),
    })
}

fn built(
    ctx: &mut Ctx,
    server: &str,
    target: Target,
    setup: Build,
    cycle: Option<(u64, Cycle)>,
) -> Result<Value> {
    const HOLD: u64 = 60;
    let (x, y, z) = AREA;
    fresh_start(ctx, server, target, None)?;
    let o = options(&[(1, "still", None)], 0, 0);
    let mut session = Session::open(
        &ctx.host,
        target,
        &ctx.bin_dir,
        &o,
        &["--movement", "still"],
    )?;
    let result = (|| -> Result<Value> {
        session.bot("target 1")?;
        if !session.wait_online(1)? {
            return Err("the observer bot could not join".into());
        }
        let tp = commands(
            &ctx.host,
            target,
            &[format!("tp {OBSERVER} {} {} {}", x + 32, y + 40, z + 32)],
        )?;
        if tp["failed"].as_u64() != Some(0) {
            return Ok(unsupported(&tp));
        }
        // Terrain below the area is generated for the observer.
        std::thread::sleep(Duration::from_secs(20));
        let build = setup(x, y, z);
        let mut sent = build.len();
        if !build.is_empty() {
            let r = commands(&ctx.host, target, &build)?;
            if r["failed"].as_u64() != Some(0) {
                return Ok(unsupported(&r));
            }
        }
        std::thread::sleep(Duration::from_secs(10));
        let from = session.now();
        match cycle {
            Some((period, cycle)) => {
                for i in 0..(HOLD / period) as usize {
                    let lines = cycle(x, y, z, i);
                    sent += lines.len();
                    let r = commands(&ctx.host, target, &lines)?;
                    if r["failed"].as_u64() != Some(0) {
                        return Ok(unsupported(&r));
                    }
                    let next = from + ((i + 1) as u64 * period) as f64;
                    std::thread::sleep(Duration::from_secs_f64((next - session.now()).max(0.0)));
                }
            }
            None => std::thread::sleep(Duration::from_secs(HOLD)),
        }
        let to = session.now();
        let w = session.window(1, from, from, to)?;
        Ok(json!({
            "commands": sent,
            "mspt": w["mspt"],
            "rss_mib": w["server"]["rss_mib"],
            "cpu_percent": w["server"]["cpu_percent"],
            "observer_online": w["players"],
        }))
    })();
    session.close();
    let mut result = result?;
    if result["observer_online"].as_u64() == Some(0) {
        result["status"] = json!("failed");
        result["note"] = json!("the observer bot was disconnected");
    }
    Ok(result)
}

fn unsupported(reply: &Value) -> Value {
    let error = reply["errors"][0].as_str().unwrap_or("command failed");
    json!({
        "status": "unsupported",
        "note": format!("command not supported: {}", first_line(error)),
    })
}

fn save(ctx: &mut Ctx, server: &str, target: Target) -> Result<Value> {
    fresh_start(ctx, server, target, None)?;
    let o = options(&[(20, "straight:boat", None)], 0, 0);
    let mut session = Session::open(
        &ctx.host,
        target,
        &ctx.bin_dir,
        &o,
        &["--movement", "straight:boat"],
    )?;
    let flown = (|| -> Result<Value> {
        session.bot("target 20")?;
        session.wait_online(20)?;
        let from = session.now();
        std::thread::sleep(Duration::from_secs(60));
        session.window(20, from, from, session.now())
    })();
    session.close();
    let flown = flown?;
    let regex = match target {
        Target::Paper | Target::Vanilla => "-",
        // Vanillite logs the translation key of console messages.
        _ => "Saved the game|commands.save.success",
    };
    let saved = bench_json(
        &ctx.host,
        &["timed", target.name(), "600", regex, "save-all", "flush"],
    )?;
    let chunks = bench_json(&ctx.host, &["chunks", target.name()])?;
    let per_s = match (chunks["chunks"].as_f64(), saved["seconds"].as_f64()) {
        (Some(c), Some(s)) if s > 0.0 => round(Some(c / s)),
        _ => Value::Null,
    };
    Ok(json!({
        "save_s": saved["seconds"],
        "chunks_on_disk": chunks["chunks"],
        "chunks_on_disk_per_save_s": per_s,
        "flight_chunks_per_s": flown["bots"]["chunks_per_s"],
        "flight_mspt_p95": flown["mspt"]["p95"],
    }))
}

fn pregen(ctx: &mut Ctx, target: Target) -> Result<Value> {
    const RADIUS: u32 = 50;
    const REPS: usize = 3;
    let chunks = f64::from((2 * RADIUS + 1).pow(2));
    match target {
        Target::Vanillite => {
            let mut scaling = Map::new();
            let mut lines: Vec<Value> = Vec::new();
            for threads in ["1", "2", "4"] {
                let out = bench_cmd(
                    &ctx.host,
                    &["pregen", &RADIUS.to_string(), threads, &REPS.to_string()],
                )?;
                lines = out
                    .lines()
                    .filter_map(|l| serde_json::from_str(l).ok())
                    .collect();
                let rate = median(
                    lines
                        .iter()
                        .filter(|l| l["phase"] == "generate")
                        .filter_map(|l| l["full_per_s"].as_f64())
                        .collect(),
                );
                scaling.insert(threads.to_owned(), round(rate));
            }
            // The 4-thread runs (the last ones) give the headline.
            let phase = |name: &str, key: &str| -> Vec<f64> {
                lines
                    .iter()
                    .filter(|l| l["phase"] == name)
                    .filter_map(|l| l[key].as_f64())
                    .collect()
            };
            let cpu: Vec<f64> = phase("generate", "process_cpu_ms")
                .iter()
                .map(|ms| ms / chunks)
                .collect();
            Ok(json!({
                "method": "chunk_bench --radius 50 --threads 4 --pump --load",
                "chunks_per_s_by_threads": scaling,
                "runs": phase("generate", "full_per_s").len(),
                "chunks_per_s": round(median(phase("generate", "full_per_s"))),
                "first_chunk_ms": round(median(phase("generate", "first_ms"))),
                "cpu_ms_per_chunk": round(median(cpu)),
                "peak_rss_mib": round(median(phase("generate", "peak_rss_mib"))),
                "load_chunks_per_s": round(median(phase("load", "full_per_s"))),
            }))
        }
        Target::Paper => {
            let mut rates = Vec::new();
            let mut cpu = Vec::new();
            let mut last_error = None;
            for _ in 0..REPS {
                bench_cmd(&ctx.host, &["reset", "paper"])?;
                bench_cmd(&ctx.host, &["chunky", "on"])?;
                let run = (|| -> Result<(f64, f64)> {
                    let started = bench_json(&ctx.host, &["start", "paper"])?;
                    if let Some(v) = started["version"].as_str() {
                        ctx.versions.insert("paper".into(), v.to_owned());
                    }
                    let r = commands(
                        &ctx.host,
                        target,
                        &[
                            "chunky center 0 0".to_owned(),
                            format!("chunky radius {}", RADIUS * 16),
                            "chunky shape square".to_owned(),
                        ],
                    )?;
                    if r["failed"].as_u64() != Some(0) {
                        return Err(format!("chunky: {}", r["errors"]));
                    }
                    let cpu0 = bench_json(&ctx.host, &["cpu", "paper"])?["cpu_s"]
                        .as_f64()
                        .unwrap_or(0.0);
                    let t = bench_json(
                        &ctx.host,
                        &["timed", "paper", "1800", "Task finished", "chunky", "start"],
                    )?;
                    let cpu1 = bench_json(&ctx.host, &["cpu", "paper"])?["cpu_s"]
                        .as_f64()
                        .unwrap_or(0.0);
                    let seconds = t["seconds"].as_f64().ok_or("no time")?;
                    Ok((chunks / seconds, (cpu1 - cpu0) * 1000.0 / chunks))
                })();
                let _ = bench_cmd(&ctx.host, &["stop", "paper"]);
                bench_cmd(&ctx.host, &["chunky", "off"])?;
                match run {
                    Ok((rate, ms)) => {
                        rates.push(rate);
                        cpu.push(ms);
                    }
                    Err(e) => last_error = Some(e),
                }
            }
            if rates.is_empty() {
                return Err(last_error.unwrap_or_default());
            }
            Ok(json!({
                "method": "Chunky 1.5.66: chunky radius 800, shape square, start",
                "runs": rates.len(),
                "chunks_per_s": round(median(rates)),
                "cpu_ms_per_chunk": round(median(cpu)),
            }))
        }
        _ => Ok(json!({
            "status": "unsupported",
            "note": "no pre-generation tool",
        })),
    }
}

fn median(mut v: Vec<f64>) -> Option<f64> {
    v.sort_by(f64::total_cmp);
    v.get(v.len() / 2).copied()
}

/// The summary number of a scenario result.
fn headline(scenario: &Scenario, r: &Value) -> Value {
    if r["status"] == "unsupported" || r["status"] == "error" {
        return Value::Null;
    }
    let step = |players: u64| -> Option<&Value> {
        r["steps"]
            .as_array()?
            .iter()
            .find(|s| s["target"].as_u64() == Some(players))
            .filter(|s| s["players"].as_u64().unwrap_or(0) * 100 >= players * 95)
    };
    match scenario.name {
        "startup" => r["idle_rss_mib"].clone(),
        "save" => r["save_s"].clone(),
        "weak" => r["max_mspt"].clone(),
        "pregen" => r["chunks_per_s"].clone(),
        "proxy" => step(500).map_or(Value::Null, |s| s["cpu_percent"].clone()),
        "flight" => step(100).map_or(Value::Null, |s| s["near_missing_percent"].clone()),
        "spread" => step(500).map_or(Value::Null, |s| s["mspt"]["p95"].clone()),
        "join-storm" => step(2000).map_or(Value::Null, |s| s["ramp_s"].clone()),
        "dense" => {
            let has_mspt = r["steps"]
                .as_array()
                .is_some_and(|s| s.iter().any(|s| s["mspt"]["p95"].is_number()));
            if has_mspt {
                r["max_players_under_50ms"].clone()
            } else {
                Value::Null
            }
        }
        _ => r["mspt"]["mean"].clone(),
    }
}

fn environment(ctx: &Ctx) -> Value {
    json!({
        "host": "test server: 4 cores of an AMD Ryzen 9 5950X, 10 GiB RAM, Linux container (Ubuntu 26.04); other test instances on it stay mostly idle during runs",
        "bots": "tools/loadbot on a separate machine in the same LAN (offline mode, no encryption)",
        "settings": "seed -578941029922802438, normal world, peaceful, view distance 10, simulation distance 10, offline mode, pause-when-empty off, a new world per scenario",
        "servers": {
            "vanillite": format!("Vanillite dev {}, release-fast build, rule profile vanilla (paper profile as extra rows); memory limit 6 GiB", ctx.versions.get("vanillite").map_or("", String::as_str)),
            "paper": format!("{} build 262, default configuration (Paper's own optimisations on), Java 25, -Xms1G -Xmx6G", ctx.versions.get("paper").map_or("Paper 26.3", String::as_str)),
            "vanilla": "Mojang's server.jar 26.3, Java 25, -Xms1G -Xmx6G",
            "pumpkin": "Pumpkin 0.2.2+26.3-26.60 release binary, defaults (no tick time report, so no MSPT)",
            "velocity": "Velocity 4.3.1 build 45, Java 25, -Xms512M -Xmx2G, modern forwarding",
            "vanillite-proxy": "vanillite-proxy from the same commit, modern forwarding",
        },
        "mspt": "Vanillite: every tick (tick log). Paper and Vanilla: `/tick query` every 5 s (P50/P95/P99/mean of the last 100 ticks), averaged over the window; max is the highest P99 window.",
        "noise": NOISE,
    })
}

/// Sends console commands; returns `{sent, failed, errors}`.
fn commands(host: &str, target: Target, lines: &[String]) -> Result<Value> {
    let mut child = Command::new("ssh")
        .args([host, "vanillite-bench", "cmds", target.name()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("ssh: {e}"))?;
    {
        let mut stdin = child.stdin.take().ok_or("no ssh input")?;
        for line in lines {
            writeln!(stdin, "{line}").map_err(|e| format!("ssh: {e}"))?;
        }
    }
    let out = child.wait_with_output().map_err(|e| format!("ssh: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let v: Value = serde_json::from_str(text.trim()).map_err(|e| format!("cmds: {e}: {text}"))?;
    if out.status.code() == Some(3) {
        return Ok(json!({ "sent": 0, "failed": 1, "errors": ["the server has no console"] }));
    }
    Ok(v)
}

fn bench_cmd(host: &str, args: &[&str]) -> Result<String> {
    let quoted: Vec<String> = args.iter().map(|a| sh_quote(a)).collect();
    loadtest::command_output(
        Command::new("ssh").args([host, &format!("vanillite-bench {}", quoted.join(" "))]),
    )
}

fn bench_json(host: &str, args: &[&str]) -> Result<Value> {
    let out = bench_cmd(host, args)?;
    let line = out.lines().last().unwrap_or_default();
    serde_json::from_str(line)
        .map_err(|e| format!("vanillite-bench {}: {e}: {out}", args.join(" ")))
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn first_line(s: &str) -> String {
    let line = s.lines().find(|l| !l.trim().is_empty()).unwrap_or(s);
    line.chars().take(200).collect()
}

fn protocol(root: &Path) -> Result<i64> {
    let text = crate::read(&root.join("data/vanilla/version.toml"))?;
    text.lines()
        .find_map(|l| l.strip_prefix("protocol = "))
        .and_then(|v| v.trim().parse().ok())
        .ok_or_else(|| "no protocol in data/vanilla/version.toml".to_owned())
}

// --- Scenario builds (Vanilla command syntax; the area corner is x, y, z) ---

fn platform(x: i32, y: i32, z: i32) -> String {
    format!(
        "fill {} {} {} {} {} {} minecraft:smooth_stone",
        x - 1,
        y - 1,
        z - 1,
        x + 64,
        y - 1,
        z + 64
    )
}

fn no_setup(_: i32, _: i32, _: i32) -> Vec<String> {
    Vec::new()
}

fn redstone(x0: i32, y: i32, z0: i32) -> Vec<String> {
    let mut c = vec![platform(x0, y, z0)];
    let units: Vec<(i32, i32)> = (0..31)
        .flat_map(|row| (0..3).map(move |u| (x0 + 2 + 20 * u, z0 + 1 + 2 * row)))
        .collect();
    for &(x, z) in &units {
        c.push(format!(
            "fill {} {y} {z} {} {y} {z} minecraft:redstone_wire",
            x + 2,
            x + 16
        ));
        c.push(format!(
            "setblock {} {y} {z} minecraft:sticky_piston[facing=up]",
            x + 17
        ));
        c.push(format!("setblock {} {} {z} minecraft:stone", x + 17, y + 1));
    }
    // Two observers facing each other start clocking when the second one is placed.
    for &(x, z) in &units {
        c.push(format!(
            "setblock {x} {y} {z} minecraft:observer[facing=east]"
        ));
        c.push(format!(
            "setblock {} {y} {z} minecraft:observer[facing=west]",
            x + 1
        ));
    }
    c
}

fn pen(x: i32, y: i32, z: i32) -> Vec<String> {
    let wall = |a: (i32, i32), b: (i32, i32)| {
        format!(
            "fill {} {y} {} {} {} {} minecraft:glass",
            a.0,
            a.1,
            b.0,
            y + 1,
            b.1
        )
    };
    vec![
        platform(x, y, z),
        wall((x - 1, z - 1), (x + 64, z - 1)),
        wall((x - 1, z + 64), (x + 64, z + 64)),
        wall((x - 1, z), (x - 1, z + 63)),
        wall((x + 64, z), (x + 64, z + 63)),
    ]
}

fn mobs(x: i32, y: i32, z: i32) -> Vec<String> {
    let mut c = pen(x, y, z);
    for i in 0..500 {
        let cx = f64::from(x) + 2.0 + f64::from(i % 25) * 2.5;
        let cz = f64::from(z) + 2.0 + f64::from(i / 25) * 3.0;
        c.push(format!("summon minecraft:cow {cx} {y} {cz}"));
    }
    c
}

fn items(x: i32, y: i32, z: i32) -> Vec<String> {
    let mut c = vec![platform(x, y, z)];
    for i in 0..2000 {
        let ix = f64::from(x) + 1.0 + f64::from(i % 50) * 1.25;
        let iz = f64::from(z) + 1.0 + f64::from(i / 50) * 1.25;
        c.push(format!(
            "summon minecraft:item {ix} {y} {iz} {{Item:{{id:\"minecraft:cobblestone\",count:1}},PickupDelay:32767s,Age:-32768s}}"
        ));
    }
    c
}

fn hoppers(x0: i32, y: i32, z0: i32) -> Vec<String> {
    let mut c = vec![platform(x0, y, z0)];
    for i in 0..16 {
        for j in 0..16 {
            let (x, z) = (x0 + 1 + 3 * i, z0 + 1 + 3 * j);
            c.push(format!(
                "setblock {x} {y} {z} minecraft:hopper[facing=east]{{Items:[{{Slot:0b,id:\"minecraft:cobblestone\",count:64}}]}}"
            ));
            c.push(format!(
                "setblock {} {y} {z} minecraft:hopper[facing=south]",
                x + 1
            ));
            c.push(format!(
                "setblock {} {y} {} minecraft:hopper[facing=west]",
                x + 1,
                z + 1
            ));
            c.push(format!(
                "setblock {x} {y} {} minecraft:hopper[facing=north]",
                z + 1
            ));
        }
    }
    c
}

fn tnt(x: i32, y: i32, z: i32, _: usize) -> Vec<String> {
    vec![
        format!(
            "fill {x} {y} {z} {} {} {} minecraft:tnt",
            x + 15,
            y + 9,
            z + 15
        ),
        format!(
            "summon minecraft:tnt {} {} {} {{fuse:0}}",
            x + 8,
            y + 5,
            z + 8
        ),
    ]
}

fn fluids_setup(x: i32, y: i32, z: i32) -> Vec<String> {
    let mut c = pen(x, y, z);
    // Water in the west half, lava in the east half.
    c.push(format!(
        "fill {} {y} {z} {} {} {} minecraft:glass",
        x + 32,
        x + 32,
        y + 1,
        z + 63
    ));
    c
}

fn fluids(x: i32, y: i32, z: i32, i: usize) -> Vec<String> {
    if !i.is_multiple_of(2) {
        return ["water", "lava"]
            .iter()
            .map(|f| {
                format!(
                    "fill {x} {y} {z} {} {y} {} minecraft:air replace minecraft:{f}",
                    x + 63,
                    z + 63
                )
            })
            .collect();
    }
    let mut c = Vec::new();
    for gx in 0..4 {
        for gz in 0..8 {
            let (wx, wz) = (x + 4 + 8 * gx, z + 4 + 8 * gz);
            c.push(format!("setblock {wx} {y} {wz} minecraft:water"));
            c.push(format!("setblock {} {y} {wz} minecraft:lava", wx + 32));
        }
    }
    c
}

fn light(x: i32, y: i32, z: i32, i: usize) -> Vec<String> {
    let block = if i.is_multiple_of(2) {
        "minecraft:glowstone"
    } else {
        "minecraft:air"
    };
    vec![format!(
        "fill {x} {y} {z} {} {} {} {block}",
        x + 31,
        y + 31,
        z + 31
    )]
}

// --- Publishing: latest.json, history.json and README.md ---

fn publish_results(root: &Path, run: &Value) -> Result<()> {
    let dir = root.join(RESULTS);
    let latest_path = dir.join("latest.json");
    let mut latest = crate::read_json(&latest_path).unwrap_or_else(|_| json!({ "scenarios": {} }));
    for key in ["suite", "date", "commit", "code_commit", "environment"] {
        latest[key] = run[key].clone();
    }
    for (name, scenario) in run["scenarios"].as_object().into_iter().flatten() {
        latest["scenarios"][name] = scenario.clone();
    }
    let history_path = dir.join("history.json");
    let mut history = crate::read_json(&history_path)
        .ok()
        .and_then(|h| h.as_array().cloned())
        .unwrap_or_default();
    let mut headlines = Map::new();
    for (name, scenario) in run["scenarios"].as_object().into_iter().flatten() {
        let per_server: Map<String, Value> = scenario["results"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(server, r)| (server.clone(), r["headline"].clone()))
            .collect();
        headlines.insert(name.clone(), Value::Object(per_server));
    }
    history.push(json!({
        "date": run["date"],
        "commit": run["code_commit"],
        "headline": headlines,
    }));
    let skip = history.len().saturating_sub(HISTORY_MAX);
    let history: Vec<Value> = history.into_iter().skip(skip).collect();
    let pretty = |v: &Value| serde_json::to_string_pretty(v).map_err(|e| e.to_string());
    crate::write(&latest_path, &(pretty(&latest)? + "\n"))?;
    // One line per run keeps the history diff small.
    let lines: Vec<String> = history
        .iter()
        .map(|h| serde_json::to_string(h).map_err(|e| e.to_string()))
        .collect::<Result<_>>()?;
    crate::write(&history_path, &format!("[\n{}\n]\n", lines.join(",\n")))?;
    report(root)
}

/// `cargo xtask bench report`: writes benchmarks/README.md from the results.
fn report(root: &Path) -> Result<()> {
    let latest = crate::read_json(&root.join(RESULTS).join("latest.json"))?;
    let history = crate::read_json(&root.join(RESULTS).join("history.json"))
        .ok()
        .and_then(|h| h.as_array().cloned())
        .unwrap_or_default();
    let md = markdown(&latest, &history);
    let path = root.join("benchmarks/README.md");
    crate::write(&path, &md)?;
    println!("wrote {}", path.display());
    Ok(())
}

fn num(v: &Value) -> String {
    match v.as_f64() {
        Some(f) if f.abs() >= 100.0 => format!("{f:.0}"),
        Some(f) => format!("{}", (f * 100.0).round() / 100.0),
        None => "–".to_owned(),
    }
}

/// The cell of a server: the value, or why there is none.
fn cell(r: &Value, v: &Value) -> String {
    match r["status"].as_str() {
        Some("unsupported") => "n/a¹".to_owned(),
        Some("error") => "error²".to_owned(),
        Some("failed") if v.is_null() => "failed³".to_owned(),
        _ => num(v),
    }
}

fn servers_of(scenario: &Value) -> Vec<&'static str> {
    SERVERS
        .iter()
        .map(|(id, _)| *id)
        .filter(|id| scenario["results"].get(*id).is_some())
        .collect()
}

fn markdown(latest: &Value, history: &[Value]) -> String {
    let env = &latest["environment"];
    let mut s = String::new();
    s.push_str(&format!(
        "# Benchmarks: Vanillite vs Paper, Vanilla and Pumpkin\n\n\
         <!-- Generated by `cargo xtask bench` from results/latest.json. Do not edit. -->\n\n\
         Measured automatically every night on the newest dev commit with new code (last run \
         {}, dev `{}`). Machine-readable: [`results/latest.json`](results/latest.json) (all \
         numbers of the last run) and [`results/history.json`](results/history.json) (the \
         summary numbers of every run).\n\n",
        pretty_date(latest["date"].as_str().unwrap_or("?")),
        latest["code_commit"].as_str().unwrap_or("?"),
    ));
    s.push_str(
        "## How to read these numbers\n\n\
         - **Bots are not players.** The load comes from headless bots (`tools/loadbot`): they \
         log in, walk or fly by simple rules, answer keep-alives and count what they receive. \
         They do not render, click, fight or build, and they send fewer packets than real \
         clients. All servers get exactly the same bots.\n",
    );
    s.push_str(&format!(
        "- **Hardware:** {}. Bots: {}.\n- **Same conditions for all:** {}.\n",
        env["host"].as_str().unwrap_or("?"),
        env["bots"].as_str().unwrap_or("?"),
        env["settings"].as_str().unwrap_or("?"),
    ));
    s.push_str("- **Versions and configuration:**\n");
    for (id, name) in SERVERS {
        if let Some(v) = env["servers"][id].as_str() {
            s.push_str(&format!("  - {name}: {v}.\n"));
        }
    }
    s.push_str(&format!(
        "- **Tick time (MSPT):** {}\n\
         - **One run per night:** expect about {} between runs; look at the history before \
         reading much into one change.\n\
         - **What each server can do:** n/a¹ = the server cannot run this scenario (the \
         command or feature is missing; the note says which). error² = the run broke (see the \
         note). failed³ = the server did not reach the load (players dropped, crashed or \
         stopped answering); the ramp stops there. Vanillite is under development: missing \
         commands or features show up here as n/a until they land.\n\n",
        env["mspt"].as_str().unwrap_or("?"),
        env["noise"].as_str().unwrap_or(NOISE),
    ));

    // Summary.
    let scenarios = latest["scenarios"].as_object().cloned().unwrap_or_default();
    let ordered: Vec<(&str, &Value)> = SCENARIOS
        .iter()
        .filter_map(|sc| scenarios.get(sc.name).map(|v| (sc.name, v)))
        .collect();
    let columns: Vec<&str> = SERVERS
        .iter()
        .map(|(id, _)| *id)
        .filter(|id| {
            ordered
                .iter()
                .any(|(_, sc)| sc["results"].get(*id).is_some())
        })
        .collect();
    s.push_str("## Summary\n\nOne number per scenario and server; the arrow says what is better. In brackets: change against the previous run.\n\n| Scenario | Metric |");
    for c in &columns {
        s.push_str(&format!(" {} |", display(c)));
    }
    s.push_str("\n|---|---|");
    for _ in &columns {
        s.push_str("---:|");
    }
    s.push('\n');
    let previous = history.iter().rev().nth(1);
    for (name, sc) in &ordered {
        let better = if sc["headline"]["better"] == "higher" {
            "↑"
        } else {
            "↓"
        };
        s.push_str(&format!(
            "| [{}](#{}) | {} ({}) {better} |",
            sc["title"].as_str().unwrap_or(name),
            anchor(sc["title"].as_str().unwrap_or(name)),
            sc["headline"]["label"].as_str().unwrap_or("?"),
            sc["headline"]["unit"].as_str().unwrap_or(""),
        ));
        for c in &columns {
            let r = &sc["results"][*c];
            if r.is_null() {
                s.push_str(" |");
                continue;
            }
            let mut text = cell(r, &r["headline"]);
            if let (Some(now), Some(before)) = (
                r["headline"].as_f64(),
                previous.and_then(|p| p["headline"][*name][*c].as_f64()),
            ) && before != 0.0
            {
                text.push_str(&format!(" ({:+.0} %)", (now - before) / before * 100.0));
            }
            s.push_str(&format!(" {text} |"));
        }
        s.push('\n');
    }
    s.push('\n');

    for (name, sc) in &ordered {
        s.push_str(&format!(
            "## {}\n\n{} Run {} on dev `{}`.\n\n",
            sc["title"].as_str().unwrap_or(name),
            sc["what"].as_str().unwrap_or(""),
            pretty_date(sc["date"].as_str().unwrap_or("?")),
            sc["commit"].as_str().unwrap_or("?"),
        ));
        s.push_str(&details(name, sc));
        s.push_str(&bar_chart(sc));
        s.push_str(&history_chart(name, sc, history));
        let notes: Vec<String> = servers_of(sc)
            .iter()
            .filter_map(|id| {
                sc["results"][*id]["note"]
                    .as_str()
                    .map(|n| format!("- {}: {n}", display(id)))
            })
            .collect();
        if !notes.is_empty() {
            s.push_str(&format!("Notes:\n\n{}\n\n", notes.join("\n")));
        }
    }
    s
}

fn details(name: &str, sc: &Value) -> String {
    let servers = servers_of(sc);
    let mut s = String::new();
    let mut table = |head: &[&str], rows: Vec<Vec<String>>| {
        s.push_str(&format!("| {} |\n|", head.join(" | ")));
        for (i, _) in head.iter().enumerate() {
            s.push_str(if i == 0 { "---|" } else { "---:|" });
        }
        s.push('\n');
        for row in rows {
            s.push_str(&format!("| {} |\n", row.join(" | ")));
        }
        s.push('\n');
    };
    let status = |r: &Value| match r["status"].as_str() {
        Some("ok") | None => String::new(),
        Some(st) => format!(" ({st})"),
    };
    match name {
        "startup" => table(
            &[
                "Server",
                "Version",
                "Cold start s",
                "Warm start s",
                "Idle RSS MiB",
                "Idle CPU %",
            ],
            servers
                .iter()
                .map(|id| {
                    let r = &sc["results"][*id];
                    vec![
                        format!("{}{}", display(id), status(r)),
                        r["version"].as_str().unwrap_or("–").to_owned(),
                        num(&r["cold_start_s"]),
                        num(&r["warm_start_s"]),
                        num(&r["idle_rss_mib"]),
                        num(&r["idle_cpu_percent"]),
                    ]
                })
                .collect(),
        ),
        "pregen" => table(
            &[
                "Server",
                "Chunks/s",
                "Chunks/s with 1 / 2 threads",
                "CPU ms per chunk",
                "First chunk ms",
                "Peak RSS MiB",
                "Load from disk chunks/s",
                "Method",
            ],
            servers
                .iter()
                .map(|id| {
                    let r = &sc["results"][*id];
                    vec![
                        format!("{}{}", display(id), status(r)),
                        num(&r["chunks_per_s"]),
                        format!(
                            "{} / {}",
                            num(&r["chunks_per_s_by_threads"]["1"]),
                            num(&r["chunks_per_s_by_threads"]["2"])
                        ),
                        num(&r["cpu_ms_per_chunk"]),
                        num(&r["first_chunk_ms"]),
                        num(&r["peak_rss_mib"]),
                        num(&r["load_chunks_per_s"]),
                        r["method"]
                            .as_str()
                            .map_or("–".to_owned(), |m| format!("`{m}`")),
                    ]
                })
                .collect(),
        ),
        "weak" => table(
            &[
                "Server",
                "Limits",
                "Start s",
                "Idle RSS MiB",
                "Movement",
                "MSPT p50",
                "MSPT p99",
                "MSPT max",
                "RSS MiB",
                "Teleports",
            ],
            servers
                .iter()
                .flat_map(|id| {
                    let r = &sc["results"][*id];
                    let head = vec![
                        format!("{}{}", display(id), status(r)),
                        format!(
                            "{} % CPU, {} MiB",
                            num(&r["cpu_quota_percent"]),
                            num(&r["memory_limit_mib"])
                        ),
                        num(&r["start_s"]),
                        num(&r["idle_rss_mib"]),
                    ];
                    let steps = r["steps"].as_array().cloned().unwrap_or_default();
                    steps
                        .iter()
                        .map(|st| {
                            let mut row = head.clone();
                            row.extend([
                                st["movement"].as_str().unwrap_or("–").to_owned(),
                                num(&st["mspt"]["p50"]),
                                num(&st["mspt"]["p99"]),
                                num(&st["mspt"]["max"]),
                                num(&st["rss_mib"]),
                                num(&st["teleports"]),
                            ]);
                            row
                        })
                        .collect::<Vec<_>>()
                })
                .collect(),
        ),
        "save" => table(
            &[
                "Server",
                "save-all flush s",
                "Chunks on disk after",
                "Chunks on disk / save s",
                "Chunks/s to the bots while flying",
                "MSPT p95 while flying",
            ],
            servers
                .iter()
                .map(|id| {
                    let r = &sc["results"][*id];
                    vec![
                        format!("{}{}", display(id), status(r)),
                        num(&r["save_s"]),
                        num(&r["chunks_on_disk"]),
                        num(&r["chunks_on_disk_per_save_s"]),
                        num(&r["flight_chunks_per_s"]),
                        num(&r["flight_mspt_p95"]),
                    ]
                })
                .collect(),
        ),
        _ if sc["results"]
            .as_object()
            .is_some_and(|o| o.values().any(|r| r["steps"].is_array())) =>
        {
            let rows = servers
                .iter()
                .flat_map(|id| {
                    let r = &sc["results"][*id];
                    let steps = r["steps"].as_array().cloned().unwrap_or_default();
                    if steps.is_empty() {
                        let mut row = vec![format!("{}{}", display(id), status(r))];
                        row.extend(std::iter::repeat_n("–".to_owned(), 16));
                        return vec![row];
                    }
                    steps
                        .iter()
                        .map(|st| {
                            let per_player_out = st["net_tx_mbps"]
                                .as_f64()
                                .zip(st["players"].as_f64().filter(|p| *p > 0.0))
                                .map(|(mbps, p)| json!((mbps * 1000.0 / p * 100.0).round() / 100.0))
                                .unwrap_or(Value::Null);
                            vec![
                                format!("{}{}", display(id), status(r)),
                                format!("{} / {}", st["players"], st["target"]),
                                num(&st["mspt"]["p50"]),
                                num(&st["mspt"]["p95"]),
                                num(&st["mspt"]["p99"]),
                                num(&st["mspt"]["max"]),
                                num(&st["rss_mib"]),
                                num(&st["rss_net_per_player_kib"]),
                                num(&st["cpu_percent"]),
                                num(&per_player_out),
                                format!("{} / {}", num(&st["ramp_s"]), num(&st["join_ms_p95"])),
                                num(&st["rtt_ms_p95"]),
                                num(&st["chunks_per_s_per_bot"]),
                                num(&st["cpu_ms_per_chunk"]),
                                num(&st["full_view_ms_p95"]),
                                num(&st["near_missing_percent"]),
                                num(&st["teleports"]),
                            ]
                        })
                        .collect()
                })
                .collect();
            table(
                &[
                    "Server",
                    "Players",
                    "MSPT p50",
                    "p95",
                    "p99",
                    "max",
                    "RSS MiB",
                    "RSS per player KiB",
                    "CPU %",
                    "Out KB/s per player",
                    "Join: ramp s / p95 ms",
                    "RTT p95 ms",
                    "Chunks/s per bot",
                    "Server CPU ms per chunk",
                    "Full view p95 ms",
                    "Holes near players %",
                    "Teleports",
                ],
                rows,
            );
        }
        _ => table(
            &[
                "Server",
                "MSPT mean",
                "p50",
                "p95",
                "p99",
                "max",
                "CPU %",
                "RSS MiB",
            ],
            servers
                .iter()
                .map(|id| {
                    let r = &sc["results"][*id];
                    vec![
                        format!("{}{}", display(id), status(r)),
                        cell(r, &r["mspt"]["mean"]),
                        num(&r["mspt"]["p50"]),
                        num(&r["mspt"]["p95"]),
                        num(&r["mspt"]["p99"]),
                        num(&r["mspt"]["max"]),
                        num(&r["cpu_percent"]),
                        num(&r["rss_mib"]),
                    ]
                })
                .collect(),
        ),
    }
    s
}

/// A bar per server with the summary number (servers without one are left out).
fn bar_chart(sc: &Value) -> String {
    let bars: Vec<(&str, f64)> = servers_of(sc)
        .into_iter()
        .filter_map(|id| sc["results"][id]["headline"].as_f64().map(|v| (id, v)))
        .collect();
    if bars.len() < 2 {
        return String::new();
    }
    let max = bars.iter().map(|b| b.1).fold(0.0, f64::max);
    format!(
        "```mermaid\nxychart-beta\n    title \"{} ({}, {} is better)\"\n    x-axis [{}]\n    y-axis \"{}\" 0 --> {}\n    bar [{}]\n```\n\n",
        sc["headline"]["label"].as_str().unwrap_or(""),
        sc["headline"]["unit"].as_str().unwrap_or(""),
        sc["headline"]["better"].as_str().unwrap_or("lower"),
        bars.iter()
            .map(|b| format!("\"{}\"", display(b.0)))
            .collect::<Vec<_>>()
            .join(", "),
        sc["headline"]["unit"].as_str().unwrap_or(""),
        nice(max),
        bars.iter()
            .map(|b| num(&json!(b.1)))
            .collect::<Vec<_>>()
            .join(", "),
    )
}

/// The summary number of Vanillite and Paper over the last 30 runs.
fn history_chart(name: &str, sc: &Value, history: &[Value]) -> String {
    let runs: Vec<&Value> = history
        .iter()
        .filter(|h| h["headline"][name].is_object())
        .collect();
    let runs = &runs[runs.len().saturating_sub(30)..];
    if runs.len() < 2 {
        return String::new();
    }
    let mut lines = Vec::new();
    let mut names = Vec::new();
    for id in ["vanillite", "paper"] {
        let values: Vec<Option<f64>> = runs
            .iter()
            .map(|h| h["headline"][name][id].as_f64())
            .collect();
        if values.iter().all(Option::is_some) {
            names.push(display(id));
            lines.push(
                values
                    .iter()
                    .map(|v| num(&json!(v.unwrap_or(0.0))))
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        }
    }
    if lines.is_empty() {
        return String::new();
    }
    let mut s = format!(
        "```mermaid\nxychart-beta\n    title \"History: {} ({}; lines: {})\"\n    x-axis [{}]\n    y-axis \"{}\"\n",
        sc["headline"]["label"].as_str().unwrap_or(""),
        sc["headline"]["unit"].as_str().unwrap_or(""),
        names.join(", "),
        runs.iter()
            .map(|h| format!(
                "\"{}\"",
                h["date"].as_str().unwrap_or("?").get(4..8).unwrap_or("?")
            ))
            .collect::<Vec<_>>()
            .join(", "),
        sc["headline"]["unit"].as_str().unwrap_or(""),
    );
    for l in lines {
        s.push_str(&format!("    line [{l}]\n"));
    }
    s.push_str("```\n\n");
    s
}

fn nice(v: f64) -> String {
    if v <= 0.0 {
        return "1".into();
    }
    let magnitude = 10f64.powf(v.log10().floor());
    let top = [1.0, 2.0, 2.5, 5.0, 10.0]
        .iter()
        .map(|m| m * magnitude)
        .find(|c| *c >= v * 1.05)
        .unwrap_or(10.0 * magnitude);
    num(&json!(top))
}

fn anchor(title: &str) -> String {
    title
        .to_lowercase()
        .chars()
        .filter_map(|c| match c {
            'a'..='z' | '0'..='9' | '-' | '_' => Some(c),
            ' ' => Some('-'),
            _ => None,
        })
        .collect()
}

/// 20261125T013000Z → 2026-11-25 01:30 UTC.
fn pretty_date(stamp: &str) -> String {
    match (
        stamp.get(0..4),
        stamp.get(4..6),
        stamp.get(6..8),
        stamp.get(9..11),
        stamp.get(11..13),
    ) {
        (Some(y), Some(m), Some(d), Some(h), Some(min)) => format!("{y}-{m}-{d} {h}:{min} UTC"),
        _ => stamp.to_owned(),
    }
}

// --- Nightly ---

/// `cargo xtask bench nightly [--no-push]`: when dev has new code since the published results,
/// runs the whole suite on it in the worktree target/bench/tree and commits the results there
/// on the branch `bench/results-<date>` (pushed unless --no-push). Prints the branch.
fn nightly(root: &Path, args: &[&str]) -> Result<()> {
    let push = match args {
        [] => true,
        ["--no-push"] => false,
        _ => return Err(crate::USAGE.to_owned()),
    };
    loadtest::git(root, &["fetch", "--quiet", "origin", "dev"])?;
    let code = loadtest::git(
        root,
        &[
            "log",
            "-1",
            "--format=%h",
            "--abbrev=12",
            "origin/dev",
            "--",
            ".",
            ":(exclude)benchmarks",
        ],
    )?;
    let published = loadtest::git(
        root,
        &["show", &format!("origin/dev:{RESULTS}/latest.json")],
    )
    .ok()
    .and_then(|t| serde_json::from_str::<Value>(&t).ok());
    let last = published
        .as_ref()
        .and_then(|p| p["code_commit"].as_str())
        .unwrap_or("");
    if !last.is_empty() && (last.starts_with(&code) || code.starts_with(last)) {
        println!("dev has no new code since the last run ({code}): nothing to do");
        return Ok(());
    }
    let tree = root.join("target/bench/tree");
    if tree.exists() {
        crate::devenv::run(Command::new("git").current_dir(&tree).args([
            "checkout",
            "--quiet",
            "--force",
            "--detach",
            "origin/dev",
        ]))?;
    } else {
        crate::devenv::run(Command::new("git").current_dir(root).args([
            "worktree",
            "add",
            "--detach",
            &tree.to_string_lossy(),
            "origin/dev",
        ]))?;
    }
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    crate::devenv::run(
        Command::new(&cargo)
            .current_dir(&tree)
            .env("CARGO_TARGET_DIR", root.join("target"))
            .args([
                "run",
                "--locked",
                "-q",
                "-p",
                "xtask",
                "--",
                "bench",
                "all",
                "--publish",
            ]),
    )?;
    let date = loadtest::command_output(Command::new("date").args(["-u", "+%Y-%m-%d"]))?;
    let branch = format!("bench/results-{date}");
    let git = |args: &[&str]| crate::devenv::run(Command::new("git").current_dir(&tree).args(args));
    git(&["checkout", "--quiet", "-B", &branch])?;
    git(&["add", "benchmarks"])?;
    git(&[
        "commit",
        "--quiet",
        "-m",
        &format!(
            "Benchmarks: nightly suite results for dev {code}\n\n\
             benchmarks/results/latest.json, history.json and benchmarks/README.md from \
             `cargo xtask bench all --publish` (#71).\n\n\
             Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
        ),
    ])?;
    if push {
        git(&["push", "--quiet", "--force", "origin", &branch])?;
    }
    println!(
        "results committed on branch {branch}{}",
        if push { " (pushed)" } else { "" }
    );
    Ok(())
}

//! Random parity scenarios against Vanilla (#132); started by `cargo xtask parity fuzz`, which
//! compiles the Java recorder and passes its class path.
//!
//! Each round generates scenarios from consecutive seeds ([`vanillite_test::fuzz::generate`]),
//! records them in Vanilla (the recorder on the pinned jar, several JVMs in parallel), runs
//! them in Vanillite and compares the traces tick by tick. A divergence is shrunk: smaller
//! variants are recorded and run again, and the smallest one that still shows the same
//! divergence is written to `<out>/found/<name>.json`, ready to go into
//! `crates/vanillite-test/scenarios/`. `<out>/summary.md` lists what was found.
//!
//! ```text
//! parity-fuzz --java <java> --agent <agent.jar> --classpath <cp> --work-dir <dir> --out <dir>
//!             [--seed N] [--count N] [--minutes N] [--jobs N] [--shrink-rounds N]
//!             [--replay <scenario.json>...] [--session <recording dir>... [--ticks N]]
//!             [--coverage <coverage.json>]
//! ```
//!
//! `--coverage` weights the generated scenarios towards the behaviours, mobs, items and blocks
//! whose Vanilla code the parity scenarios exercise least (`cargo xtask parity coverage`, #257).
//!
//! `--session` replays a recorded server session (`cargo xtask parity replay`, #152): the
//! recording becomes a scenario on its world snapshot ([`vanillite_test::session`]), written
//! to `<out>/sessions/<name>.json`, then compared and shrunk like a random one. `--ticks`
//! replays only the first ticks of a long session.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::Value;
use vanillite_test::fuzz;
use vanillite_test::game_world::GameWorld;
use vanillite_test::runner;
use vanillite_test::scenario::Scenario;
use vanillite_test::trace::Trace;
use vanillite_test::world::Execution;

/// Differences kept per result.
const DIFF_LIMIT: usize = 12;

struct Args {
    java: String,
    agent: String,
    classpath: String,
    work_dir: PathBuf,
    out: PathBuf,
    seed: u64,
    count: Option<u64>,
    minutes: Option<u64>,
    jobs: usize,
    shrink_rounds: usize,
    replay: Vec<PathBuf>,
    sessions: Vec<PathBuf>,
    ticks: Option<u32>,
    /// Weights from the coverage of Vanilla's code (`--coverage <coverage.json>`, #257).
    bias: Option<fuzz::Bias>,
}

fn args() -> Result<Args, String> {
    let mut a = Args {
        java: "java".to_owned(),
        agent: String::new(),
        classpath: String::new(),
        work_dir: PathBuf::from("."),
        out: PathBuf::from("target/parity-fuzz"),
        seed: 1,
        count: None,
        minutes: None,
        jobs: std::thread::available_parallelism().map_or(2, |n| (n.get() / 2).clamp(1, 8)),
        shrink_rounds: 40,
        replay: Vec::new(),
        sessions: Vec::new(),
        ticks: None,
        bias: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let mut v = || it.next().ok_or_else(|| format!("{k} needs a value"));
        match k.as_str() {
            "--java" => a.java = v()?,
            "--agent" => a.agent = v()?,
            "--classpath" => a.classpath = v()?,
            "--work-dir" => a.work_dir = PathBuf::from(v()?),
            "--out" => a.out = PathBuf::from(v()?),
            "--seed" => a.seed = v()?.parse().map_err(|e| format!("--seed: {e}"))?,
            "--count" => a.count = Some(v()?.parse().map_err(|e| format!("--count: {e}"))?),
            "--minutes" => a.minutes = Some(v()?.parse().map_err(|e| format!("--minutes: {e}"))?),
            "--jobs" => a.jobs = v()?.parse().map_err(|e| format!("--jobs: {e}"))?,
            "--shrink-rounds" => {
                a.shrink_rounds = v()?.parse().map_err(|e| format!("--shrink-rounds: {e}"))?;
            }
            "--replay" => a.replay.push(PathBuf::from(v()?)),
            "--session" => a.sessions.push(PathBuf::from(v()?)),
            "--ticks" => a.ticks = Some(v()?.parse().map_err(|e| format!("--ticks: {e}"))?),
            "--coverage" => {
                let path = v()?;
                let text = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
                let json: Value =
                    serde_json::from_str(&text).map_err(|e| format!("{path}: {e}"))?;
                a.bias = Some(fuzz::Bias::from_coverage(&json));
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if a.count.is_none() && a.minutes.is_none() && a.replay.is_empty() && a.sessions.is_empty() {
        a.count = Some(20);
    }
    Ok(a)
}

/// What a scenario did on both sides.
#[derive(Clone, Debug)]
enum Outcome {
    Same,
    /// First divergent tick, signature, differences.
    Diverges(u32, String, Vec<String>),
    /// Vanilla rejected the scenario (an invalid input, not a Vanillite bug).
    VanillaError(String),
    /// Vanillite failed where Vanilla ran.
    VanilliteError(String),
}

impl Outcome {
    fn signature(&self) -> Option<String> {
        match self {
            Outcome::Diverges(_, s, _) => Some(s.clone()),
            Outcome::VanilliteError(e) => Some(format!("error: {}", first_words(e))),
            _ => None,
        }
    }
}

fn first_words(s: &str) -> String {
    s.split_whitespace()
        .take_while(|w| !w.chars().any(|c| c.is_ascii_digit()))
        .take(6)
        .collect::<Vec<_>>()
        .join(" ")
}

/// A recorder JVM that stays up (`ScenarioRecorder --serve`): one Vanilla bootstrap for all
/// scenarios it records.
struct Recorder {
    child: std::process::Child,
    input: Option<std::process::ChildStdin>,
    output: std::io::BufReader<std::process::ChildStdout>,
}

impl Recorder {
    fn start(args: &Args) -> Result<Self, String> {
        let mut child = Command::new(&args.java)
            .current_dir(&args.work_dir)
            .arg(format!("-javaagent:{}", args.agent))
            .arg("-cp")
            .arg(&args.classpath)
            .arg("dev.vanillite.extractor.golden.ScenarioRecorder")
            .arg("--serve")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("java: {e}"))?;
        let input = child.stdin.take().ok_or("no recorder stdin")?;
        let output = std::io::BufReader::new(child.stdout.take().ok_or("no recorder stdout")?);
        Ok(Self {
            child,
            input: Some(input),
            output,
        })
    }

    /// Records one scenario; `Err` if the JVM is gone.
    fn record(&mut self, scenario: &Path, trace: &Path) -> Result<(), String> {
        use std::io::{BufRead, Write};
        let input = self.input.as_mut().ok_or("recorder: no stdin")?;
        writeln!(input, "{}\t{}", scenario.display(), trace.display())
            .and_then(|()| input.flush())
            .map_err(|e| format!("recorder: {e}"))?;
        let mut line = String::new();
        loop {
            line.clear();
            let n = self
                .output
                .read_line(&mut line)
                .map_err(|e| format!("recorder: {e}"))?;
            if n == 0 {
                return Err("the recorder JVM ended".to_owned());
            }
            if line.starts_with("@@done ") {
                return Ok(());
            }
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        // The end of its input ends the recorder normally, so a Java agent's shutdown hook
        // (the coverage of `cargo xtask parity coverage`) still writes its data.
        drop(self.input.take());
        let end = Instant::now() + Duration::from_secs(20);
        while Instant::now() < end {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The recorder JVMs, one per job; a JVM that died is started again.
struct Recorders(Vec<std::sync::Mutex<Option<Recorder>>>);

impl Recorders {
    fn new(jobs: usize) -> Self {
        Self(
            (0..jobs.max(1))
                .map(|_| std::sync::Mutex::new(None))
                .collect(),
        )
    }
}

/// Records the scenarios in Vanilla on the recorder JVMs; returns per scenario the trace text
/// or the error.
fn record_vanilla(
    args: &Args,
    recorders: &Recorders,
    dir: &Path,
    names: &[String],
) -> Vec<Result<String, String>> {
    let next = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|s| {
        for slot in &recorders.0 {
            let next = &next;
            s.spawn(move || {
                let mut guard = slot
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(n) = names.get(i) else {
                        return;
                    };
                    let scenario = dir.join(format!("{n}.json"));
                    let trace = dir.join(format!("{n}.trace.json"));
                    for _attempt in 0..2 {
                        if guard.is_none() {
                            match Recorder::start(args) {
                                Ok(r) => *guard = Some(r),
                                Err(e) => {
                                    let _ = std::fs::write(
                                        dir.join(format!("{n}.trace.json.error")),
                                        e,
                                    );
                                    break;
                                }
                            }
                        }
                        let Some(r) = guard.as_mut() else {
                            break;
                        };
                        match r.record(&scenario, &trace) {
                            Ok(()) => break,
                            Err(_) => *guard = None,
                        }
                    }
                }
            });
        }
    });
    names
        .iter()
        .map(|n| {
            let trace = dir.join(format!("{n}.trace.json"));
            let error = dir.join(format!("{n}.trace.json.error"));
            if let Ok(e) = std::fs::read_to_string(&error) {
                Err(e)
            } else {
                std::fs::read_to_string(&trace).map_err(|e| format!("no Vanilla trace: {e}"))
            }
        })
        .collect()
}

/// Runs a scenario in Vanillite.
fn run_vanillite(name: &str, text: &str) -> Result<Trace, String> {
    let scenario = Scenario::parse(name, text)?;
    let run = std::panic::catch_unwind(|| {
        runner::record(&scenario, GameWorld::factory, Execution::Serial)
    });
    match run {
        Ok(r) => r,
        Err(p) => Err(p
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| p.downcast_ref::<&str>().map(|s| (*s).to_owned()))
            .unwrap_or_else(|| "panic".to_owned())),
    }
}

/// Writes, records and runs a batch of scenarios; returns their outcomes in order.
fn evaluate(
    args: &Args,
    recorders: &Recorders,
    dir: &Path,
    batch: &[(String, Value)],
) -> Vec<Outcome> {
    let _ = std::fs::create_dir_all(dir);
    let mut texts = Vec::new();
    for (name, json) in batch {
        let text = serde_json::to_string_pretty(json).unwrap_or_default() + "\n";
        let _ = std::fs::write(dir.join(format!("{name}.json")), &text);
        let _ = std::fs::remove_file(dir.join(format!("{name}.trace.json")));
        let _ = std::fs::remove_file(dir.join(format!("{name}.trace.json.error")));
        texts.push(text);
    }
    let names: Vec<String> = batch.iter().map(|b| b.0.clone()).collect();
    let started = Instant::now();
    let vanilla = record_vanilla(args, recorders, dir, &names);
    let vanilla_time = started.elapsed();
    // Vanillite runs on all cores; scenarios are independent. Session replays are large: as
    // many at once as recorders, so their traces fit into memory.
    let next = std::sync::atomic::AtomicUsize::new(0);
    let threads = if names.iter().any(|n| n.starts_with("session_")) {
        args.jobs.max(1)
    } else {
        std::thread::available_parallelism().map_or(1, std::num::NonZero::get)
    };
    let mut outcomes: Vec<(usize, Outcome)> = std::thread::scope(|s| {
        let workers: Vec<_> = (0..threads.min(batch.len()))
            .map(|_| {
                s.spawn(|| {
                    let mut done = Vec::new();
                    loop {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(name) = names.get(i) else {
                            return done;
                        };
                        let outcome = match &vanilla[i] {
                            Err(e) => Outcome::VanillaError(e.clone()),
                            Ok(expected) => match Trace::parse(expected) {
                                Err(e) => Outcome::VanillaError(format!("bad trace: {e}")),
                                Ok(expected) => match run_vanillite(name, &texts[i]) {
                                    Err(e) => Outcome::VanilliteError(e),
                                    Ok(actual) => {
                                        let diff = expected.diff(&actual, DIFF_LIMIT);
                                        match expected.first_divergence(&actual) {
                                            None => Outcome::Same,
                                            Some(t) => {
                                                Outcome::Diverges(t, fuzz::signature(&diff), diff)
                                            }
                                        }
                                    }
                                },
                            },
                        };
                        done.push((i, outcome));
                    }
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|w| w.join().unwrap_or_default())
            .collect()
    });
    outcomes.sort_by_key(|o| o.0);
    eprintln!(
        "parity-fuzz: {} scenario(s): Vanilla {:.1} s, Vanillite {:.1} s",
        batch.len(),
        vanilla_time.as_secs_f64(),
        (started.elapsed() - vanilla_time).as_secs_f64()
    );
    outcomes.into_iter().map(|o| o.1).collect()
}

/// Size of a scenario for shrinking: actions, setup steps, entities, ticks.
fn size(s: &Value) -> usize {
    let n = |k: &str| s[k].as_array().map_or(0, Vec::len);
    n("actions") + n("setup") + n("entities") + s["ticks"].as_u64().unwrap_or(0) as usize
}

/// Shrinks a diverging scenario: returns the smallest variant with the same signature and its
/// outcome.
fn shrink(
    deadline: Option<Instant>,
    args: &Args,
    recorders: &Recorders,
    dir: &Path,
    name: &str,
    start: Value,
    outcome: Outcome,
) -> (Value, Outcome) {
    let Some(signature) = outcome.signature() else {
        return (start, outcome);
    };
    let (mut best, mut best_outcome) = (start, outcome);
    let mut granularity = 2;
    for round in 0..args.shrink_rounds {
        if deadline.is_some_and(|d| Instant::now() >= d) {
            break;
        }
        let first = match &best_outcome {
            Outcome::Diverges(t, ..) => *t,
            _ => best["ticks"].as_u64().unwrap_or(0) as u32,
        };
        let candidates = fuzz::shrink_candidates(&best, first, granularity);
        if candidates.is_empty() {
            break;
        }
        let batch: Vec<(String, Value)> = candidates
            .into_iter()
            .enumerate()
            .map(|(i, c)| (format!("{name}_r{round}_{i}"), c))
            .collect();
        let outcomes = evaluate(args, recorders, dir, &batch);
        let hit = batch
            .into_iter()
            .zip(outcomes)
            .find(|(_, o)| o.signature().as_deref() == Some(signature.as_str()));
        match hit {
            Some(((_, s), o)) if size(&s) < size(&best) => {
                best = s;
                best_outcome = o;
                granularity = 2.max(granularity - 1);
            }
            _ => {
                let longest = ["actions", "setup", "entities"]
                    .iter()
                    .map(|k| best[*k].as_array().map_or(0, Vec::len))
                    .max()
                    .unwrap_or(0);
                if granularity >= longest {
                    break;
                }
                granularity = (granularity * 2).min(longest);
            }
        }
    }
    (best, best_outcome)
}

fn main() -> std::process::ExitCode {
    let args = match args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("parity-fuzz: {e}");
            return std::process::ExitCode::from(2);
        }
    };
    let started = Instant::now();
    let deadline = args.minutes.map(|m| started + Duration::from_secs(m * 60));
    let work = args.out.join("work");
    let found_dir = args.out.join("found");
    let _ = std::fs::create_dir_all(&found_dir);
    let mut seed = args.seed;
    let mut ran = 0u64;
    let mut same = 0u64;
    let mut invalid = 0u64;
    // Smallest scenario per signature.
    let mut found: BTreeMap<String, (String, Value, Outcome, u64)> = BTreeMap::new();
    let batch_size = (args.jobs * 3).max(4) as u64;
    let recorders = Recorders::new(args.jobs);
    // One line per replayed session: name and result.
    let mut sessions: Vec<(String, String)> = Vec::new();
    loop {
        let mut batch = Vec::new();
        if !args.replay.is_empty() || !args.sessions.is_empty() {
            if ran > 0 {
                break;
            }
            for dir in &args.sessions {
                let stem = dir
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("session");
                let json = match vanillite_test::session::scenario(dir, args.ticks, None) {
                    Ok(j) => j,
                    Err(e) => {
                        eprintln!("parity-fuzz: {e}");
                        sessions.push((dir.display().to_string(), format!("not replayed: {e}")));
                        continue;
                    }
                };
                let mut variants = Vec::new();
                if vanillite_test::session::is_cut(&json) {
                    // Players far apart: each one alone in a region of its own as well.
                    for p in vanillite_test::session::players(dir) {
                        match vanillite_test::session::scenario(dir, args.ticks, Some(&p)) {
                            Ok(j) => variants.push((format!("session_{}_{p}", slug(stem)), j)),
                            Err(e) => eprintln!("parity-fuzz: {e}"),
                        }
                    }
                }
                if json["ticks"].as_u64().unwrap_or(0) > 1 || variants.is_empty() {
                    variants.insert(0, (format!("session_{}", slug(stem)), json));
                }
                for (name, json) in variants {
                    let volume = vanillite_test::session::region_volume(&json);
                    println!(
                        "{name}: {} players, {} actions, {} ticks, region {} blocks",
                        json["entities"].as_array().map_or(0, Vec::len),
                        json["actions"].as_array().map_or(0, Vec::len),
                        json["ticks"],
                        volume
                    );
                    if vanillite_test::session::is_cut(&json) {
                        println!("{name}: {}", json["description"].as_str().unwrap_or(""));
                    }
                    let sessions = args.out.join("sessions");
                    let _ = std::fs::create_dir_all(&sessions);
                    let text = serde_json::to_string_pretty(&json).unwrap_or_default() + "\n";
                    let _ = std::fs::write(sessions.join(format!("{name}.json")), text);
                    batch.push((name, json, 0));
                }
            }
            for p in &args.replay {
                let Ok(text) = std::fs::read_to_string(p) else {
                    eprintln!("parity-fuzz: cannot read {}", p.display());
                    continue;
                };
                let Ok(json) = serde_json::from_str::<Value>(&text) else {
                    eprintln!("parity-fuzz: {} is no JSON", p.display());
                    continue;
                };
                let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("replay");
                batch.push((stem.to_owned(), json, 0));
            }
        } else {
            let left = args
                .count
                .map_or(batch_size, |c| c.saturating_sub(ran).min(batch_size));
            if left == 0 || deadline.is_some_and(|d| Instant::now() >= d) {
                break;
            }
            for _ in 0..left {
                batch.push((
                    format!("fuzz_{seed}"),
                    fuzz::generate_biased(seed, args.bias.as_ref()),
                    seed,
                ));
                seed += 1;
            }
        }
        if batch.is_empty() {
            break;
        }
        let inputs: Vec<(String, Value)> =
            batch.iter().map(|b| (b.0.clone(), b.1.clone())).collect();
        let outcomes = evaluate(&args, &recorders, &work, &inputs);
        ran += batch.len() as u64;
        for ((name, json, seed), outcome) in batch.into_iter().zip(outcomes) {
            if name.starts_with("session_") {
                let result = match &outcome {
                    Outcome::Same => "replays equal".to_owned(),
                    Outcome::VanillaError(e) => {
                        format!("Vanilla rejects it: {}", e.lines().next().unwrap_or(""))
                    }
                    Outcome::Diverges(t, sig, _) => format!("diverges at tick {t}: {sig}"),
                    Outcome::VanilliteError(e) => {
                        format!("Vanillite fails: {}", e.lines().next().unwrap_or(""))
                    }
                };
                sessions.push((name.clone(), result));
            }
            match &outcome {
                Outcome::Same => same += 1,
                Outcome::VanillaError(e) => {
                    invalid += 1;
                    println!(
                        "{name}: invalid (Vanilla: {})",
                        e.lines().next().unwrap_or("")
                    );
                }
                Outcome::Diverges(t, sig, _) => println!("{name}: diverges at tick {t}: {sig}"),
                Outcome::VanilliteError(e) => {
                    println!(
                        "{name}: Vanillite fails: {}",
                        e.lines().next().unwrap_or("")
                    );
                }
            }
            let Some(sig) = outcome.signature() else {
                continue;
            };
            if let Some(known) = found.get(&sig)
                && size(&known.1) <= size(&json)
            {
                continue;
            }
            let (small, small_outcome) =
                shrink(deadline, &args, &recorders, &work, &name, json, outcome);
            println!(
                "{name}: shrunk to {} actions, {} setup steps, {} entities, {} ticks",
                small["actions"].as_array().map_or(0, Vec::len),
                small["setup"].as_array().map_or(0, Vec::len),
                small["entities"].as_array().map_or(0, Vec::len),
                small["ticks"]
            );
            found.insert(sig, (name, small, small_outcome, seed));
        }
    }
    // The shrunk scenarios and the summary.
    let mut summary = format!(
        "# Parity fuzz\n\n{ran} scenarios (seeds from {}), {same} equal, {invalid} invalid \
         (Vanilla rejected them), {} distinct divergences, {:.0} s.\n\n",
        args.seed,
        found.len(),
        started.elapsed().as_secs_f64()
    );
    if !sessions.is_empty() {
        summary.push_str("| session | result |\n|---|---|\n");
        for (name, result) in &sessions {
            let result = result.replace('|', "\\|");
            summary.push_str(&format!("| {name} | {result} |\n"));
        }
        summary.push('\n');
    }
    for (sig, (name, mut json, outcome, seed)) in found {
        let prefix = if name.starts_with("session_") {
            "session"
        } else {
            "fuzz"
        };
        let file_name = format!("{prefix}_{}", slug(&sig));
        if !name.starts_with("session_") {
            json["description"] = Value::String(format!(
                "Random parity scenario (#132), seed {seed}, shrunk: {sig}."
            ));
        } else {
            json["description"] = Value::String(format!(
                "Session replay {name} (#152), shrunk: {sig}. Needs the recording's world \
                 snapshot (`world.snapshot`)."
            ));
        }
        let text = serde_json::to_string_pretty(&json).unwrap_or_default() + "\n";
        let path = found_dir.join(format!("{file_name}.json"));
        let _ = std::fs::write(&path, text);
        let _ = writeln_summary(&mut summary, &file_name, &name, &sig, &outcome);
    }
    let _ = std::fs::write(args.out.join("summary.md"), &summary);
    println!("\n{summary}");
    std::process::ExitCode::SUCCESS
}

fn writeln_summary(
    out: &mut String,
    file: &str,
    from: &str,
    sig: &str,
    outcome: &Outcome,
) -> std::fmt::Result {
    use std::fmt::Write as _;
    writeln!(out, "## {file}\n\nFrom `{from}`: {sig}\n")?;
    match outcome {
        Outcome::Diverges(t, _, diff) => {
            writeln!(out, "First divergence at tick {t}:\n\n```")?;
            for d in diff {
                writeln!(out, "{d}")?;
            }
            writeln!(out, "```\n")
        }
        Outcome::VanilliteError(e) => writeln!(out, "Vanillite fails:\n\n```\n{e}\n```\n"),
        _ => Ok(()),
    }
}

/// A file name part of a signature.
fn slug(sig: &str) -> String {
    let mut s: String = sig
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    while s.contains("__") {
        s = s.replace("__", "_");
    }
    s.trim_matches('_').chars().take(48).collect()
}

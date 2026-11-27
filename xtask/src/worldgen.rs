//! `cargo xtask worldgen sweep`: world generation against Vanilla over many seeds, all
//! dimensions and world types (#223); `cargo xtask light sweep`: light after generation and
//! after scripted block changes the same way (#237).
//!
//! Compiles the Java harness on the pinned jar (like `golden regen`), lets
//! `golden/WorldSweep.java` (`LightSweep.java`) generate the areas in Vanilla, then runs the
//! ignored test `vanillite-worldgen/tests/vanilla_sweep.rs`
//! (`vanillite-light/tests/vanilla_light_sweep.rs`), which does the same with Vanillite and
//! compares chunk by chunk. Results go to `target/worldgen-sweep/` (`target/light-sweep/`),
//! `summary.md`.

use std::path::Path;
use std::process::Command;

use crate::Result;
use crate::golden;
use crate::vanilla::{java_tool, remove_dir, run};

/// Every world type and dimension the sweep covers.
const KINDS: [&str; 6] = [
    "overworld",
    "nether",
    "end",
    "amplified",
    "large_biomes",
    "flat",
];

/// SplitMix64: the seeds and far areas from the start seed.
fn mix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// One kind of sweep.
struct Sweep {
    /// Name in messages, e.g. `worldgen sweep`.
    name: &'static str,
    /// Java class under `dev.vanillite.extractor.golden`.
    class: &'static str,
    /// Output under `target/`.
    out: &'static str,
    /// Crate and test that compare.
    package: &'static str,
    test: &'static str,
    /// Chunks per side of the origin area.
    size: i32,
}

/// `cargo xtask worldgen sweep [--seed N] [--seeds N] [--world-seed N]... [--size N]
/// [--kinds a,b] [--keep]`.
pub fn sweep(root: &Path, args: &[&str]) -> Result<()> {
    run_sweep(
        root,
        args,
        &Sweep {
            name: "worldgen sweep",
            class: "WorldSweep",
            out: "worldgen-sweep",
            package: "vanillite-worldgen",
            test: "vanilla_sweep",
            size: 16,
        },
    )
}

/// `cargo xtask light sweep [--seed N] [--seeds N] [--world-seed N]... [--size N] [--kinds a,b]
/// [--keep]`.
pub fn light_sweep(root: &Path, args: &[&str]) -> Result<()> {
    run_sweep(
        root,
        args,
        &Sweep {
            name: "light sweep",
            class: "LightSweep",
            out: "light-sweep",
            package: "vanillite-light",
            test: "vanilla_light_sweep",
            size: 6,
        },
    )
}

fn run_sweep(root: &Path, args: &[&str], sweep: &Sweep) -> Result<()> {
    let name = sweep.name;
    let mut start: Option<u64> = None;
    let mut seeds = 3_usize;
    let mut size = sweep.size;
    let mut kinds: Vec<String> = KINDS.iter().map(|k| (*k).to_owned()).collect();
    let mut explicit: Vec<i64> = Vec::new();
    let mut keep = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = |name: &str| it.next().copied().ok_or(format!("{name} needs a value"));
        match *arg {
            "--seed" => {
                start = Some(
                    value("--seed")?
                        .parse()
                        .map_err(|e| format!("--seed: {e}"))?,
                )
            }
            "--seeds" => {
                seeds = value("--seeds")?
                    .parse()
                    .map_err(|e| format!("--seeds: {e}"))?
            }
            "--size" => {
                size = value("--size")?
                    .parse()
                    .map_err(|e| format!("--size: {e}"))?
            }
            "--world-seed" => explicit.push(
                value("--world-seed")?
                    .parse()
                    .map_err(|e| format!("--world-seed: {e}"))?,
            ),
            "--kinds" => {
                kinds = value("--kinds")?.split(',').map(str::to_owned).collect();
                if let Some(k) = kinds.iter().find(|k| !KINDS.contains(&k.as_str())) {
                    return Err(format!("unknown kind {k} (kinds: {})", KINDS.join(", ")));
                }
            }
            "--keep" => keep = true,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if size < 1 {
        return Err("--size must be at least 1".to_owned());
    }
    let mut state = start.unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() / 86_400)
    });
    let start_seed = state;
    println!("{name}: start seed {start_seed}");
    let mut world_seeds = explicit;
    if world_seeds.is_empty() {
        world_seeds = (0..seeds).map(|_| mix(&mut state) as i64).collect();
    }
    // Per seed: the area around the origin and a far one (half the size) up to 20000 chunks
    // out, in every kind.
    let mut jobs = Vec::new();
    for &seed in &world_seeds {
        let far = |state: &mut u64| (mix(state) % 40_001) as i32 - 20_000;
        let (fx, fz) = (far(&mut state), far(&mut state));
        for kind in &kinds {
            jobs.push(format!("{kind}:{seed}:{}:{}:{size}", -size / 2, -size / 2));
            jobs.push(format!("{kind}:{seed}:{fx}:{fz}:{}", (size / 2).max(1)));
        }
    }
    let seeds_text: Vec<String> = world_seeds.iter().map(i64::to_string).collect();
    println!(
        "{name}: seeds {}, kinds {}, {} areas",
        seeds_text.join(" "),
        kinds.join(","),
        jobs.len()
    );

    let key = golden::Key::current(root)?;
    let (classes, cp, broken) = golden::harness(root, &key)?;
    if broken
        .files
        .iter()
        .any(|f| f.file_stem().and_then(|s| s.to_str()) == Some(sweep.class))
    {
        return Err(format!(
            "{} does not compile against {}:\n{}",
            sweep.class, key.minecraft, broken.errors
        ));
    }
    let work_dir = root.join("target/vanilla").join(&key.minecraft);
    let out = root.join("target").join(sweep.out);
    let vanilla = out.join("vanilla");
    remove_dir(&out)?;
    std::fs::create_dir_all(&vanilla).map_err(|e| format!("{}: {e}", vanilla.display()))?;
    let sep = if cfg!(windows) { ";" } else { ":" };
    let classpath = format!("{}{sep}{cp}", classes.display());

    // A few JVMs, each with its share of the areas.
    let started = std::time::Instant::now();
    let jvms = std::thread::available_parallelism()
        .map_or(1, |n| (n.get() / 4).max(1))
        .min(3)
        .min(jobs.len());
    std::thread::scope(|s| {
        let handles: Vec<_> = (0..jvms)
            .map(|i| {
                let part: Vec<&String> = jobs.iter().skip(i).step_by(jvms).collect();
                let (classpath, vanilla, work_dir) = (&classpath, &vanilla, &work_dir);
                s.spawn(move || {
                    run(Command::new(java_tool("java"))
                        .current_dir(work_dir)
                        .arg("-Xmx3G")
                        .arg("-cp")
                        .arg(classpath)
                        .arg(format!("dev.vanillite.extractor.golden.{}", sweep.class))
                        .arg(vanilla)
                        .args(part))
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().map_err(|_| "sweep thread panicked".to_owned())?)
            .collect::<Result<Vec<()>>>()
    })?;
    println!(
        "{name}: Vanilla generated {} areas in {:.0} s",
        jobs.len(),
        started.elapsed().as_secs_f64()
    );

    let summary = out.join("summary.md");
    let result = run(
        Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned()))
            .current_dir(root)
            .args([
                "test",
                "-q",
                "--release",
                "-p",
                sweep.package,
                "--test",
                sweep.test,
                "--",
                "--ignored",
                "--nocapture",
            ])
            .env("VANILLITE_SWEEP_DIR", &vanilla)
            .env("VANILLITE_SWEEP_REPORT", &summary),
    );
    if let Ok(text) = std::fs::read_to_string(&summary) {
        let header = format!(
            "Seeds: {} (`--seed {start_seed}`). Kinds: {}.\n\n",
            seeds_text.join(", "),
            kinds.join(", ")
        );
        let text = text.replacen("\n\n", &format!("\n\n{header}"), 1);
        std::fs::write(&summary, text).map_err(|e| format!("{}: {e}", summary.display()))?;
    }
    if !keep {
        remove_dir(&vanilla)?;
    }
    println!("{name}: {}", summary.display());
    result
}

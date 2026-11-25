//! Runs scenarios and compares their traces with the goldens.
//!
//! [`check`] runs a scenario twice, with the world's parallel phases run serially and in
//! parallel (verify mode): both traces must be identical. Then the trace must equal the golden
//! exactly: the blessed golden if the scenario has one, else the Vanilla recording.
//!
//! `VANILLITE_BLESS=<scenario>,...` (or `all`; set by `cargo xtask golden bless`) writes the
//! traces of those scenarios as their blessed goldens instead of comparing them.
//! `VANILLITE_SCENARIO=<scenario>,...` runs only those scenarios.

use std::path::PathBuf;

use crate::golden::{self, Keys};
use crate::scenario::{OpKind, Scenario, Tier};
use crate::trace::{Frame, Trace};
use crate::world::{Execution, ScenarioWorld, WorldFactory, parse_state, state_name};

/// Differences listed per failing scenario.
const DIFF_LIMIT: usize = 30;

/// Runs `scenario` in a world from `factory` and records its trace.
pub fn record(
    scenario: &Scenario,
    factory: WorldFactory,
    execution: Execution,
) -> Result<Trace, String> {
    let mut world = factory(&scenario.world, execution);
    for op in &scenario.setup {
        apply(world.as_mut(), op, false)?;
    }
    world.add_entities(&scenario.entities, scenario.entity_seed)?;
    let mut trace = Trace::new(scenario.min, scenario.max);
    observe(world.as_mut(), &mut trace, 0, scenario.world.observe_light);
    for tick in 1..=scenario.ticks {
        for op in scenario.actions.iter().filter(|a| a.tick == tick) {
            apply(world.as_mut(), op, true)?;
        }
        world.step();
        observe(
            world.as_mut(),
            &mut trace,
            tick,
            scenario.world.observe_light,
        );
    }
    Ok(trace)
}

fn apply(
    world: &mut dyn ScenarioWorld,
    op: &crate::scenario::Op,
    action: bool,
) -> Result<(), String> {
    if let Some(hurt) = &op.hurt {
        return world.hurt(hurt);
    }
    if let Some(effect) = &op.effect {
        return world.effect(effect);
    }
    let updates = match op.kind {
        OpKind::Set => false,
        OpKind::Place => true,
        OpKind::Fill => action,
        OpKind::Hurt => return Err("hurt without its damage".to_owned()),
        OpKind::Effect => return Err("effect without its effect".to_owned()),
        OpKind::Force | OpKind::Unforce => {
            return world.set_forced(&op.chunks, op.kind == OpKind::Force);
        }
        OpKind::Restart => return world.restart(),
    };
    let state = parse_state(&op.block)?;
    let (a, b) = (op.from, op.to);
    // `BlockPos.betweenClosed` order: x fastest, then y, then z.
    for z in a.z.min(b.z)..=a.z.max(b.z) {
        for y in a.y.min(b.y)..=a.y.max(b.y) {
            for x in a.x.min(b.x)..=a.x.max(b.x) {
                let pos = vanillite_base::BlockPos::new(x, y, z);
                if updates {
                    world.place_block(pos, state);
                } else {
                    world.set_block_raw(pos, state);
                }
            }
        }
    }
    Ok(())
}

fn observe(world: &mut dyn ScenarioWorld, trace: &mut Trace, tick: u32, light: bool) {
    let volume: usize = trace.size().iter().product();
    let mut frame = Frame {
        tick,
        blocks: Vec::with_capacity(volume),
        ..Frame::default()
    };
    for i in 0..volume {
        let pos = trace.pos_of(i);
        let name = if world.is_loaded(pos) {
            state_name(world.block(pos))
        } else {
            crate::scenario::UNLOADED.to_owned()
        };
        frame.blocks.push(trace.intern(&name));
    }
    world.block_ticks(trace.min, trace.max, &mut frame.block_ticks);
    world.fluid_ticks(trace.min, trace.max, &mut frame.fluid_ticks);
    world.entities(trace.min, trace.max, &mut frame.entities);
    if light {
        world.light(trace.min, trace.max, &mut frame.light);
    }
    frame.level_rng = world.level_rng();
    trace.frames.push(frame);
}

/// Path of the Vanilla golden of a scenario (repository or bulk tier).
pub fn golden_path(scenario: &Scenario) -> PathBuf {
    match scenario.tier {
        Tier::Fast => golden::crate_dir()
            .join("goldens/scenarios")
            .join(format!("{}.json", scenario.name)),
        Tier::Bulk => golden::bulk_dir().join("scenarios").join(format!(
            "{}-{}.json",
            scenario.name,
            &scenario.sha1[..12]
        )),
    }
}

/// Path of the blessed golden of a scenario.
pub fn blessed_path(scenario: &Scenario) -> PathBuf {
    golden::crate_dir()
        .join("goldens/blessed")
        .join(format!("{}.json", scenario.name))
}

fn bless_requested(name: &str) -> bool {
    std::env::var("VANILLITE_BLESS")
        .is_ok_and(|v| v.split(',').any(|n| n.trim() == name || n.trim() == "all"))
}

/// The expected trace of a scenario: blessed, else Vanilla's, after checking its key.
fn expected(scenario: &Scenario) -> Result<Trace, String> {
    let blessed = blessed_path(scenario);
    if blessed.exists() {
        let trace = read_trace(&blessed)?;
        if trace.blessed_for.as_deref() != Some(scenario.sha1.as_str()) {
            return Err(format!(
                "{} was blessed for another version of the scenario; bless it again with \
                 `cargo xtask golden bless {}` or delete it",
                blessed.display(),
                scenario.name
            ));
        }
        return Ok(trace);
    }
    if scenario.world.profile != "vanilla" {
        return Err(format!(
            "profile `{}` cannot be recorded on Vanilla: bless the scenario with \
             `cargo xtask golden bless {}`",
            scenario.world.profile, scenario.name
        ));
    }
    let path = golden_path(scenario);
    match scenario.tier {
        Tier::Fast => Keys::check(
            path.parent().expect("golden directory"),
            &scenario.name,
            &scenario.sha1,
        )?,
        Tier::Bulk if !path.exists() => {
            return Err(format!(
                "bulk golden {} is missing; run `cargo xtask golden regen --bulk` (needs Java 25)",
                path.display()
            ));
        }
        Tier::Bulk => {}
    }
    read_trace(&path)
}

fn read_trace(path: &std::path::Path) -> Result<Trace, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Trace::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Runs a scenario in verify mode and compares it with its golden (or blesses it).
pub fn check(scenario: &Scenario, factory: WorldFactory) -> Result<(), String> {
    let serial = record(scenario, factory, Execution::Serial)?;
    let parallel = record(scenario, factory, Execution::Parallel)?;
    let diff = serial.diff(&parallel, DIFF_LIMIT);
    if !diff.is_empty() {
        return Err(format!(
            "verify mode: the parallel run differs from the serial run:\n  {}",
            diff.join("\n  ")
        ));
    }
    if bless_requested(&scenario.name) {
        let path = blessed_path(scenario);
        let mut trace = serial;
        trace.blessed_for = Some(scenario.sha1.clone());
        std::fs::create_dir_all(path.parent().expect("blessed directory"))
            .map_err(|e| e.to_string())?;
        std::fs::write(&path, trace.to_json()).map_err(|e| format!("{}: {e}", path.display()))?;
        eprintln!("blessed {}", path.display());
        return Ok(());
    }
    let expected = expected(scenario)?;
    let first = expected.first_divergence(&serial);
    let known = scenario.diverges.as_ref();
    if first == known.map(|d| d.tick) {
        return Ok(());
    }
    let diff = expected.diff(&serial, DIFF_LIMIT);
    match (known, first) {
        (Some(d), None) => Err(format!(
            "matches the golden now: remove its entry (tick {}: {}) from \
             scenarios/divergences.toml",
            d.tick, d.cause
        )),
        (Some(d), Some(t)) => Err(format!(
            "the first divergence moved from tick {} to tick {t} ({}; {}, owner {}): {} the \
             tick in scenarios/divergences.toml if intended:\n  {}",
            d.tick,
            scenario.description,
            d.cause,
            d.owner,
            if t > d.tick {
                "progress, raise"
            } else {
                "regression, or lower"
            },
            diff.join("\n  ")
        )),
        _ => Err(format!(
            "differs from the golden ({}):\n  {}",
            scenario.description,
            diff.join("\n  ")
        )),
    }
}

/// Checks every scenario of a tier; returns one message per failing scenario.
pub fn check_tier(tier: Tier, factory: WorldFactory) -> Vec<String> {
    let scenarios = match Scenario::all() {
        Ok(s) => s,
        Err(e) => return vec![e],
    };
    let only = std::env::var("VANILLITE_SCENARIO").ok();
    let todo: Vec<&Scenario> = scenarios
        .iter()
        .filter(|s| s.tier == tier)
        .filter(|s| {
            only.as_deref()
                .is_none_or(|o| o.split(',').any(|n| n.trim() == s.name))
        })
        .collect();
    // Scenarios are independent: run them on all cores, report in name order.
    let threads = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let mut results: Vec<(usize, String)> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads.min(todo.len()))
            .map(|_| {
                scope.spawn(|| {
                    let mut failed = Vec::new();
                    loop {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(s) = todo.get(i) else {
                            return failed;
                        };
                        if let Err(e) = check(s, factory) {
                            failed.push((i, format!("{}: {e}", s.name)));
                        }
                    }
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|w| w.join().unwrap_or_else(|p| std::panic::resume_unwind(p)))
            .collect()
    });
    results.sort_by_key(|r| r.0);
    results.into_iter().map(|r| r.1).collect()
}

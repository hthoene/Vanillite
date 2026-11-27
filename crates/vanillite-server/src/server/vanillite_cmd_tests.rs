//! `/vanillite` on a running server (#250): every subcommand answers, also those that ask
//! every world between the ticks, the profiler writes its report, and the commands only
//! exist where they should. No Vanilla data: the texts are the English fallbacks.

use super::*;

fn start(name: &str, toml: &str) -> (PathBuf, Server) {
    let dir = std::env::temp_dir().join(format!("vanillite-cmd-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("eula.txt"), "eula=true\n").unwrap();
    fs::write(
        dir.join("server.properties"),
        "view-distance=2\nsimulation-distance=2\n",
    )
    .unwrap();
    fs::write(dir.join(VANILLITE_FILE), toml).unwrap();
    let mut builder = ServerBuilder::new(&dir);
    builder.vanilla_pack(VanillaPack::None).network(false);
    builder.harness = true;
    (dir, builder.start().expect("server starts"))
}

fn step(server: &mut Server) {
    server.wait_until_next_tick(0, true);
    server.tick_server();
    server.wait_until_next_tick(0, true);
}

/// Runs `line` as an RCON-like source on the overworld and steps until the feedback holds
/// `until` (answers that need every world come between the ticks), at most 20 s.
fn run(server: &mut Server, line: &str, until: &str) -> String {
    let buffer = Arc::new(Mutex::new(String::new()));
    let global = Arc::clone(&server.global);
    let world = match &mut server.worlds {
        Worlds::Sequential(worlds) => worlds[0].as_mut().unwrap(),
        Worlds::Parallel(_) => panic!("sequential worlds"),
    };
    crate::commands::run_collected(world, &global, line, &buffer);
    let deadline = Instant::now() + Duration::from_secs(20);
    while !buffer.lock().unwrap().contains(until) && Instant::now() < deadline {
        step(server);
    }
    buffer.lock().unwrap().clone()
}

#[test]
fn vanillite_commands_answer() {
    let (dir, mut server) = start("answer", "profile = \"vanilla\"\n");
    for _ in 0..5 {
        step(&mut server);
    }
    let help = run(&mut server, "vanillite", "debug");
    assert!(help.contains("Vanillite commands:"), "{help}");
    assert!(help.contains("/vanillite profile [<seconds>]"), "{help}");

    let version = run(&mut server, "vl version", "Apache");
    assert!(
        version.contains("Hannes Thöne · https://vanillite.org · Apache-2.0"),
        "{version}"
    );

    let tps = run(&mut server, "vanillite:tps", "TPS");
    assert!(tps.starts_with("TPS from last 5s, 1m, 5m: "), "{tps}");
    let mspt = run(&mut server, "vl mspt", "◴");
    assert!(mspt.contains("Server tick times (avg/min/max)"), "{mspt}");
    // Plain `/tps` only with the Paper commands.
    let plain = run(&mut server, "tps", "");
    assert!(!plain.starts_with("TPS"), "{plain}");

    let status = run(&mut server, "vl status", "Total");
    assert!(status.contains("Vanillite status"), "{status}");
    assert!(status.contains("minecraft:overworld: "), "{status}");
    assert!(status.contains("Memory: "), "{status}");
    assert!(status.contains("Total: "), "{status}");

    let memory = run(&mut server, "vl memory", "Other");
    assert!(
        memory.contains("Chunks (blocks, light, heightmaps): "),
        "{memory}"
    );
    assert!(memory.contains("Other (code, caches"), "{memory}");

    let chunks = run(&mut server, "vl chunks minecraft:overworld", "Top");
    assert!(
        chunks.contains("Top chunks in minecraft:overworld"),
        "{chunks}"
    );
    let entities = run(
        &mut server,
        "vl entities minecraft:overworld minecraft:zombie",
        "most zombie",
    );
    assert!(
        entities.contains("Entities in minecraft:overworld: "),
        "{entities}"
    );
    assert!(
        entities.contains("Chunks with the most zombie:"),
        "{entities}"
    );

    fs::write(
        dir.join(VANILLITE_FILE),
        "profile = \"vanilla\"\n[performance.threads]\ncompute = 3\n[performance.survival]\ntime-sync-interval = 40\n",
    )
    .unwrap();
    let reload = run(&mut server, "vl reload config", "Reloaded");
    assert!(
        reload.contains("Reloaded vanillite.toml: 1 changes apply now, 1 need a restart"),
        "{reload}"
    );
    assert!(reload.contains("performance.threads.compute"), "{reload}");

    let debug = run(&mut server, "vl debug nobody", "");
    assert!(!debug.contains("Debug of"), "{debug}");

    let profile = run(&mut server, "vl profile 1", "Report");
    assert!(profile.contains("Profiling for 1 seconds..."), "{profile}");
    assert!(profile.contains("Profile of "), "{profile}");
    assert!(profile.contains("overworld: "), "{profile}");
    let reports: Vec<_> = fs::read_dir(dir.join("profiles"))
        .expect("profiles folder")
        .flatten()
        .collect();
    assert_eq!(reports.len(), 1);
    let text = fs::read_to_string(reports[0].path()).unwrap();
    assert!(text.contains("Tick phases:"), "{text}");
    assert!(!server.global.diagnostics.profiler.is_active());

    server.stop();
    drop(server);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn paper_profile_has_plain_tps_and_mspt() {
    let (dir, mut server) = start(
        "paper",
        "profile = \"paper\"\n[performance.tick]\nparallel-dimensions = false\n",
    );
    step(&mut server);
    let tps = run(&mut server, "tps", "TPS");
    assert!(tps.starts_with("TPS from last 5s, 1m, 5m: "), "{tps}");
    let mspt = run(&mut server, "mspt", "◴");
    assert!(mspt.contains("Server tick times"), "{mspt}");
    server.stop();
    drop(server);
    let _ = fs::remove_dir_all(&dir);
}

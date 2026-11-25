//! Chunk pipeline benchmark: generates (or loads) every chunk within a radius through the real
//! chunk runtime with the built-in slots (Vanilla generator, BFS light, Anvil storage) on the
//! compute pool, and reports wall time, throughput, CPU time per status and memory.
//!
//! ```text
//! cargo run --profile release-fast -p vanillite-server --example chunk_bench -- \
//!     --radius 16 --threads 4 [--tick-ms 50] [--pump] [--seed N] [--dir /tmp/bench] [--load]
//! ```
//!
//! `--tick-ms` is the world tick interval: the runtime applies results and starts jobs once per
//! tick, as on a server. `--load` saves the generated chunks and then measures loading them
//! from disk into a fresh runtime. Output is one JSON object per phase on stdout.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use vanillite_base::ChunkPos;
use vanillite_chunk::slots::{
    ChunkGenerator, ChunkStatus, ChunkStorage, LightEngine, Neighbours, ProtoChunk, WorldInfo,
};
use vanillite_server::{Compute, ComputePool};
use vanillite_world::chunks::{ChunkSettings, ChunkSlots, Chunks, Executor, FULL_LEVEL};
use vanillite_worldgen::{DimensionSpec, TerrainGenerator, VanillaChunkGenerator};

/// CPU nanoseconds and calls per status (index = `ChunkStatus::index`), plus load and save.
#[derive(Default)]
struct Timings {
    nanos: [AtomicU64; 12],
    calls: [AtomicU64; 12],
}

const LOAD: usize = 10;
const SAVE: usize = 11;

#[repr(C)]
struct Timespec {
    sec: i64,
    nsec: i64,
}

unsafe extern "C" {
    fn clock_gettime(clock: i32, ts: *mut Timespec) -> i32;
}

/// CPU time of the calling thread in nanoseconds (`CLOCK_THREAD_CPUTIME_ID`), so the numbers
/// hold on a busy machine.
#[allow(unsafe_code)]
fn thread_cpu_ns() -> u64 {
    let mut ts = Timespec { sec: 0, nsec: 0 };
    // SAFETY: `ts` is a valid, writable timespec; clock 3 is CLOCK_THREAD_CPUTIME_ID on Linux.
    unsafe { clock_gettime(3, &mut ts) };
    ts.sec as u64 * 1_000_000_000 + ts.nsec as u64
}

impl Timings {
    fn record(&self, slot: usize, since: u64) {
        self.nanos[slot].fetch_add(thread_cpu_ns() - since, Ordering::Relaxed);
        self.calls[slot].fetch_add(1, Ordering::Relaxed);
    }

    fn take(&self) -> Vec<(String, f64, u64)> {
        let name = |i: usize| match i {
            LOAD => "load".to_owned(),
            SAVE => "save".to_owned(),
            i => ChunkStatus::ALL[i].name().to_owned(),
        };
        (0..12)
            .map(|i| {
                (
                    name(i),
                    self.nanos[i].swap(0, Ordering::Relaxed) as f64 / 1e6,
                    self.calls[i].swap(0, Ordering::Relaxed),
                )
            })
            .filter(|(_, _, calls)| *calls > 0)
            .collect()
    }
}

struct TimedGenerator(Arc<dyn ChunkGenerator>, Arc<Timings>);

impl ChunkGenerator for TimedGenerator {
    fn new_chunk(&self, pos: ChunkPos) -> ProtoChunk {
        self.0.new_chunk(pos)
    }

    fn prepare(&self) {
        self.0.prepare();
    }

    fn generate(
        &self,
        status: ChunkStatus,
        targets: &mut [ProtoChunk],
        neighbours: &mut Neighbours<'_>,
    ) {
        let start = thread_cpu_ns();
        self.0.generate(status, targets, neighbours);
        self.1.record(status.index(), start);
    }
}

struct TimedLight(Arc<dyn LightEngine>, Arc<Timings>);

impl LightEngine for TimedLight {
    fn initialize_light(&self, targets: &mut [ProtoChunk]) {
        let start = thread_cpu_ns();
        self.0.initialize_light(targets);
        self.1.record(ChunkStatus::InitializeLight.index(), start);
    }

    fn light_chunks(&self, targets: &mut [ProtoChunk], neighbours: &mut Neighbours<'_>) {
        let start = thread_cpu_ns();
        self.0.light_chunks(targets, neighbours);
        self.1.record(ChunkStatus::Light.index(), start);
    }

    fn relight(&self, changed: &[ChunkPos], area: &mut Neighbours<'_>) {
        self.0.relight(changed, area);
    }
}

struct TimedStorage(Arc<dyn ChunkStorage>, Arc<Timings>);

impl ChunkStorage for TimedStorage {
    fn load(&self, positions: &[ChunkPos], out: &mut Vec<std::io::Result<Option<ProtoChunk>>>) {
        let start = thread_cpu_ns();
        self.0.load(positions, out);
        self.1.record(LOAD, start);
    }

    fn save(&self, chunks: &[Arc<ProtoChunk>], out: &mut Vec<std::io::Result<()>>) {
        let start = thread_cpu_ns();
        self.0.save(chunks, out);
        self.1.record(SAVE, start);
    }

    fn flush(&self) -> std::io::Result<()> {
        self.0.flush()
    }
}

struct PoolExecutor(Compute);

impl Executor for PoolExecutor {
    fn spawn(&self, priority: u32, job: Box<dyn FnOnce() + Send>) {
        self.0.spawn_background(priority, job);
    }
}

struct Args {
    radius: i32,
    threads: usize,
    tick: Duration,
    seed: i64,
    dir: PathBuf,
    load: bool,
    max_jobs: usize,
    pump: bool,
}

fn args() -> Args {
    let mut a = Args {
        radius: 16,
        threads: std::thread::available_parallelism().map_or(4, |n| n.get()),
        tick: Duration::from_millis(50),
        seed: -578_941_029_922_802_438,
        dir: std::env::temp_dir().join("vanillite-chunk-bench"),
        load: false,
        pump: false,
        max_jobs: ChunkSettings::default().max_jobs,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || it.next().expect("flag value");
        match flag.as_str() {
            "--radius" => a.radius = value().parse().expect("radius"),
            "--threads" => a.threads = value().parse().expect("threads"),
            "--tick-ms" => a.tick = Duration::from_millis(value().parse().expect("tick")),
            "--seed" => a.seed = value().parse().expect("seed"),
            "--dir" => a.dir = value().into(),
            "--max-jobs" => a.max_jobs = value().parse().expect("max jobs"),
            "--load" => a.load = true,
            "--pump" => a.pump = true,
            other => panic!("unknown flag {other}"),
        }
    }
    a
}

fn rss_kib(key: &str) -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with(key))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|v| v.parse().ok())
        })
        .unwrap_or(0)
}

fn cpu_ms() -> f64 {
    // Sum of the run time of every thread of the process (`schedstat`, nanoseconds).
    let mut total = 0;
    if let Ok(tasks) = std::fs::read_dir("/proc/self/task") {
        for task in tasks.flatten() {
            total += std::fs::read_to_string(task.path().join("schedstat"))
                .ok()
                .and_then(|s| {
                    s.split_whitespace()
                        .next()
                        .and_then(|v| v.parse::<u64>().ok())
                })
                .unwrap_or(0);
        }
    }
    total as f64 / 1e6
}

/// Ticks the runtime until every chunk within `radius` is full; returns the time to the first
/// full centre chunk and the total time. With `wake`, finished jobs are pumped between ticks
/// as the server does.
fn run(
    chunks: &mut Chunks,
    radius: i32,
    tick: Duration,
    wake: Option<&mpsc::Receiver<()>>,
) -> (Duration, Duration) {
    let start = Instant::now();
    let _ticket = chunks.add_ticket(ChunkPos::new(0, 0), FULL_LEVEL - radius as u8);
    let mut first = None;
    let total = (2 * radius + 1).pow(2) as usize;
    loop {
        let tick_start = Instant::now();
        chunks.tick();
        if first.is_none() && chunks.chunk(ChunkPos::new(0, 0)).is_some() {
            first = Some(start.elapsed());
        }
        let mut full = 0;
        for z in -radius..=radius {
            for x in -radius..=radius {
                full += usize::from(chunks.chunk(ChunkPos::new(x, z)).is_some());
            }
        }
        if full == total {
            return (first.unwrap_or_default(), start.elapsed());
        }
        let deadline = tick_start + tick;
        match wake {
            Some(wake) => {
                while let Some(rest) = deadline.checked_duration_since(Instant::now()) {
                    if wake.recv_timeout(rest).is_ok() {
                        chunks.pump();
                    }
                }
            }
            None => {
                if let Some(rest) = deadline.checked_duration_since(Instant::now()) {
                    std::thread::sleep(rest);
                }
            }
        }
    }
}

fn report(
    phase: &str,
    a: &Args,
    chunks: &Chunks,
    timings: &Timings,
    (first, total): (Duration, Duration),
    cpu: f64,
) {
    let full = (2 * a.radius + 1).pow(2) as f64;
    let stages: Vec<String> = timings
        .take()
        .into_iter()
        .map(|(name, ms, calls)| {
            format!(
                "\"{name}\":{{\"cpu_ms\":{ms:.1},\"calls\":{calls},\"us_per_call\":{:.1}}}",
                ms * 1000.0 / calls as f64
            )
        })
        .collect();
    println!(
        "{{\"phase\":\"{phase}\",\"radius\":{},\"threads\":{},\"tick_ms\":{},\"full_chunks\":{full},\
         \"loaded_chunks\":{},\"first_ms\":{:.0},\"wall_ms\":{:.0},\"full_per_s\":{:.1},\
         \"process_cpu_ms\":{cpu:.0},\"rss_mib\":{},\"peak_rss_mib\":{},\"stages\":{{{}}}}}",
        a.radius,
        a.threads,
        a.tick.as_millis(),
        chunks.loaded(),
        first.as_secs_f64() * 1000.0,
        total.as_secs_f64() * 1000.0,
        full / total.as_secs_f64(),
        rss_kib("VmRSS:") / 1024,
        rss_kib("VmHWM:") / 1024,
        stages.join(",")
    );
}

fn main() {
    let a = args();
    let cache = std::env::var_os("VANILLITE_CACHE").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/vanilla-cache"),
        PathBuf::from,
    );
    let pack = vanillite_datapack::vanilla::ensure_vanilla_pack(&cache, None).expect("pack");
    let packs = vanillite_datapack::load(&[pack.as_path()]).expect("packs");
    let tag = |name: &str| packs.tag_ids("minecraft:block", name).expect("tag");
    let biome_registry = packs.registry("minecraft:worldgen/biome").expect("biomes");
    vanillite_chunk::facts::install_vanilla(
        &tag("minecraft:blocks_motion_in_heightmap"),
        &tag("minecraft:blocks_motion_in_heightmap_no_leaves"),
        biome_registry.entries().len() as u32,
    );
    let biomes: Arc<[String]> = biome_registry
        .entries()
        .iter()
        .map(|e| e.id.clone())
        .collect();
    let default_biome = biomes
        .iter()
        .position(|b| b == "minecraft:plains")
        .unwrap_or(0) as u16;
    let _ = std::fs::remove_dir_all(&a.dir);
    let info = WorldInfo {
        dimension: "minecraft:overworld".to_owned(),
        dir: a.dir.clone(),
        min_y: -64,
        height: 384,
        seed: a.seed,
        has_sky_light: true,
        biomes,
        default_biome,
        air: vanillite_data::block::BlockState::AIR.0,
    };
    let biome_source =
        serde_json::json!({"type": "minecraft:multi_noise", "preset": "minecraft:overworld"});
    let spec = DimensionSpec {
        noise_settings: "minecraft:overworld",
        biome_source: &biome_source,
        min_y: -64,
        height: 384,
        has_sky_light: true,
    };
    let setup = Instant::now();
    let terrain = TerrainGenerator::new(&packs, spec, a.seed).expect("generator");
    eprintln!("generator setup {:?}", setup.elapsed());
    let timings = Arc::new(Timings::default());
    let pool = ComputePool::new(a.threads).expect("pool");
    let executor: Arc<dyn Executor> = Arc::new(PoolExecutor(Compute::new(Arc::new(pool))));
    let generator: Arc<dyn ChunkGenerator> = Arc::new(TimedGenerator(
        Arc::new(VanillaChunkGenerator::new(terrain, info.clone())),
        Arc::clone(&timings),
    ));
    let prepare = Instant::now();
    generator.prepare();
    eprintln!("generator prepare {:?}", prepare.elapsed());
    let light: Arc<dyn LightEngine> = Arc::new(TimedLight(
        Arc::new(vanillite_light::BfsLightEngine::new(&info)),
        Arc::clone(&timings),
    ));
    let storage = || -> Arc<dyn ChunkStorage> {
        Arc::new(TimedStorage(
            Arc::new(vanillite_storage::AnvilStorage::new(
                &info,
                vanillite_storage::Compression::Deflate,
            )),
            Arc::clone(&timings),
        ))
    };
    let settings = ChunkSettings {
        view_distance: a.radius.clamp(2, 32),
        max_jobs: a.max_jobs,
        ..ChunkSettings::default()
    };
    let slots = ChunkSlots {
        storage: storage(),
        generator: Arc::clone(&generator),
        light: Arc::clone(&light),
        executor: Arc::clone(&executor),
    };
    let mut chunks = Chunks::new(info.clone(), settings, slots);
    let (wake_tx, wake_rx) = mpsc::sync_channel::<()>(1);
    let waker = move || {
        let _ = wake_tx.try_send(());
    };
    if a.pump {
        chunks.set_waker(Arc::new(waker.clone()));
    }
    let wake = a.pump.then_some(&wake_rx);
    let cpu = cpu_ms();
    let times = run(&mut chunks, a.radius, a.tick, wake);
    report("generate", &a, &chunks, &timings, times, cpu_ms() - cpu);
    if a.load {
        let save = Instant::now();
        chunks.save_all(true);
        // Saves run as jobs; tick until they are applied.
        for _ in 0..200 {
            std::thread::sleep(Duration::from_millis(20));
            chunks.tick();
        }
        eprintln!("save {:?}", save.elapsed());
        drop(chunks);
        timings.take();
        let slots = ChunkSlots {
            storage: storage(),
            generator,
            light,
            executor,
        };
        let mut chunks = Chunks::new(info, settings, slots);
        if a.pump {
            chunks.set_waker(Arc::new(waker));
        }
        let cpu = cpu_ms();
        let times = run(&mut chunks, a.radius, a.tick, wake);
        report("load", &a, &chunks, &timings, times, cpu_ms() - cpu);
    }
}

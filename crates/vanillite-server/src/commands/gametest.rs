//! The game test framework of `/test` and the test instance blocks (Vanilla's
//! `net.minecraft.gametest.framework`): the `minecraft:test_instance` and
//! `minecraft:test_environment` registries of the data packs, the runner with its batches,
//! environments, retries and reports, and the ticker.
//!
//! The ticker of a server is one (`GameTestTicker.SINGLETON`); its tests tick at the end of the
//! tick of the world they are in, so a run never needs a main thread. A batch whose world is
//! not the one starting it is spawned by its own world on its next tick.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use serde_json::Value;
use vanillite_base::{Aabb, BlockPos, Vec3};
use vanillite_command::Arg as ErrArg;
use vanillite_command::error::CommandError;
use vanillite_data::block::ids;
use vanillite_game::block::technical::test_block;
use vanillite_game::block::technical::test_instance::{
    self, Action, BlockBox, Data, Status, TestInstance, TestShape, action,
};
use vanillite_game::level::{BlockEntity, Level, update};
use vanillite_game::survival::rules::{Difficulty, Rule};
use vanillite_protocol::packets::play::clientbound::{
    GameTestHighlightPos, SystemChat, TestInstanceBlockStatus,
};
use vanillite_protocol::text::{Arg, ClickEvent, Color, Component, HoverEvent, NamedColor};
use vanillite_worldgen::template::transform::Rotation;

use super::Env;
use super::source::{CommandSource, SourceKind};
use crate::global::Global;
use crate::player_list::Uuid;
use crate::world::WorldId;

/// `TEST_NEARBY_SEARCH_RADIUS`.
const NEARBY_RADIUS: i32 = 15;
/// `TEST_FULL_SEARCH_RADIUS`.
const FULL_RADIUS: i32 = 250;
/// `GameTestBatchFactory.MAX_TESTS_PER_BATCH`.
const MAX_TESTS_PER_BATCH: usize = 50;
/// `GameTestRunner.DEFAULT_TESTS_PER_ROW`.
pub(crate) const DEFAULT_TESTS_PER_ROW: i32 = 8;

// --- Registries ------------------------------------------------------------------------------

/// A `Holder<TestEnvironmentDefinition>`: a registry entry or an inline definition.
#[derive(Clone, Debug, PartialEq)]
enum EnvRef {
    Id(String),
    Inline(Box<EnvDef>),
}

impl EnvRef {
    /// `getRegisteredName`.
    fn registered_name(&self) -> String {
        match self {
            EnvRef::Id(id) => id.clone(),
            EnvRef::Inline(_) => "[unregistered]".to_owned(),
        }
    }
}

/// `TestEnvironmentDefinition.Weather.Type`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WeatherType {
    Clear,
    Rain,
    Thunder,
}

impl WeatherType {
    fn apply(self, global: &Global) {
        let mut data = global.level.weather();
        match self {
            WeatherType::Clear => data.set(100_000, 0, false, false),
            WeatherType::Rain => data.set(0, 100_000, true, false),
            WeatherType::Thunder => data.set(0, 100_000, true, true),
        }
    }
}

/// `TestEnvironmentDefinition`.
#[derive(Clone, Debug, PartialEq)]
enum EnvDef {
    AllOf(Vec<EnvRef>),
    ClockTime {
        clock: String,
        time: i32,
    },
    Difficulty(Difficulty),
    Function {
        setup: Option<String>,
        teardown: Option<String>,
    },
    GameRules(Vec<(String, i32)>),
    Timelines,
    Weather(WeatherType),
}

/// A `GameTestInstance` of the registry.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TestDef {
    /// The registry id.
    pub(crate) id: String,
    /// `function` tests: the test function; `None` for `block_based` tests.
    function: Option<String>,
    environment: EnvRef,
    dimension: String,
    structure: String,
    max_ticks: i32,
    setup_ticks: i32,
    required: bool,
    rotation: Rotation,
    max_attempts: i32,
    required_successes: i32,
    sky_access: bool,
    padding: i32,
}

impl TestDef {
    fn shape(&self) -> TestShape {
        TestShape {
            rotation: self.rotation,
            padding: self.padding,
            sky_access: self.sky_access,
        }
    }

    fn world(&self, global: &Global) -> Option<WorldId> {
        global.dimensions.world(&self.dimension)
    }

    /// `describe`: the rows of the test instance screen.
    fn describe(&self) -> Component {
        let row = |key: &str, value: Component| {
            Component::translatable(key, vec![value.color(Color::Named(NamedColor::Blue))])
                .append(Component::text("\n"))
        };
        let kind = if self.function.is_some() {
            "test_instance.type.function"
        } else {
            "test_instance.type.block_based"
        };
        let mut out = row(
            "test_instance.description.type",
            Component::translatable(kind, vec![]),
        );
        if let Some(function) = &self.function {
            out = out.append(row(
                "test_instance.description.function",
                Component::text(function.clone()),
            ));
        }
        out.append(row(
            "test_instance.description.structure",
            Component::text(self.structure.clone()),
        ))
        .append(row(
            "test_instance.description.batch",
            Component::text(self.environment.registered_name()),
        ))
        .append(row(
            "test_instance.description.dimension",
            Component::text(self.dimension.clone()),
        ))
    }
}

/// The test registries of the data packs.
#[derive(Debug, Default)]
pub(crate) struct Registry {
    tests: Vec<Arc<TestDef>>,
    environments: BTreeMap<String, EnvDef>,
}

impl Registry {
    fn get(&self, id: &str) -> Option<&Arc<TestDef>> {
        self.tests.iter().find(|t| t.id == id)
    }

    fn environment<'r>(&'r self, env: &'r EnvRef) -> Option<&'r EnvDef> {
        match env {
            EnvRef::Id(id) => self.environments.get(id),
            EnvRef::Inline(def) => Some(def),
        }
    }

    fn load(global: &Global) -> Registry {
        let mut out = Registry::default();
        let Some(packs) = global.packs.loaded() else {
            return out;
        };
        if let Some(reg) = packs.registry("minecraft:test_environment") {
            for e in reg.entries() {
                match serde_json::from_str::<Value>(e.json.get())
                    .ok()
                    .and_then(|v| parse_env(&v))
                {
                    Some(def) => {
                        out.environments.insert(e.id.clone(), def);
                    }
                    None => log::error!("Couldn't parse test environment {}", e.id),
                }
            }
        }
        if let Some(reg) = packs.registry("minecraft:test_instance") {
            for e in reg.entries() {
                match serde_json::from_str::<Value>(e.json.get())
                    .ok()
                    .and_then(|v| parse_test(&e.id, &v))
                {
                    Some(def) => out.tests.push(Arc::new(def)),
                    None => log::error!("Couldn't parse test instance {}", e.id),
                }
            }
        }
        out
    }
}

fn qualify(id: &str) -> String {
    if id.contains(':') {
        id.to_owned()
    } else {
        format!("minecraft:{id}")
    }
}

fn parse_env_ref(v: &Value) -> Option<EnvRef> {
    match v {
        Value::String(id) => Some(EnvRef::Id(qualify(id))),
        Value::Object(_) => parse_env(v).map(|d| EnvRef::Inline(Box::new(d))),
        _ => None,
    }
}

fn parse_env(v: &Value) -> Option<EnvDef> {
    let kind = qualify(v.get("type")?.as_str()?);
    let string = |name: &str| v.get(name).and_then(Value::as_str).map(qualify);
    Some(match kind.as_str() {
        "minecraft:all_of" => EnvDef::AllOf(
            v.get("definitions")?
                .as_array()?
                .iter()
                .map(parse_env_ref)
                .collect::<Option<Vec<_>>>()?,
        ),
        "minecraft:clock_time" => EnvDef::ClockTime {
            clock: string("clock")?,
            time: i32::try_from(v.get("time")?.as_i64()?)
                .ok()
                .filter(|t| *t >= 0)?,
        },
        "minecraft:difficulty" => EnvDef::Difficulty(match v.get("difficulty")?.as_str()? {
            "peaceful" => Difficulty::Peaceful,
            "easy" => Difficulty::Easy,
            "normal" => Difficulty::Normal,
            "hard" => Difficulty::Hard,
            _ => return None,
        }),
        "minecraft:function" => EnvDef::Function {
            setup: string("setup"),
            teardown: string("teardown"),
        },
        "minecraft:game_rules" => EnvDef::GameRules(
            v.get("rules")?
                .as_object()?
                .iter()
                .map(|(k, value)| {
                    let value = match value {
                        Value::Bool(b) => i32::from(*b),
                        Value::Number(n) => i32::try_from(n.as_i64()?).ok()?,
                        _ => return None,
                    };
                    Some((qualify(k), value))
                })
                .collect::<Option<Vec<_>>>()?,
        ),
        "minecraft:timeline_attributes" => EnvDef::Timelines,
        "minecraft:weather" => EnvDef::Weather(match v.get("weather")?.as_str()? {
            "clear" => WeatherType::Clear,
            "rain" => WeatherType::Rain,
            "thunder" => WeatherType::Thunder,
            _ => return None,
        }),
        _ => return None,
    })
}

fn parse_test(id: &str, v: &Value) -> Option<TestDef> {
    let kind = qualify(v.get("type")?.as_str()?);
    let function = match kind.as_str() {
        "minecraft:function" => Some(qualify(v.get("function")?.as_str()?)),
        "minecraft:block_based" => None,
        _ => return None,
    };
    let int = |name: &str, default: Option<i64>| -> Option<i32> {
        match v.get(name) {
            Some(n) => i32::try_from(n.as_i64()?).ok(),
            None => default.and_then(|d| i32::try_from(d).ok()),
        }
    };
    let bool = |name: &str, default: bool| v.get(name).and_then(Value::as_bool).unwrap_or(default);
    let positive = |n: i32| (n > 0).then_some(n);
    Some(TestDef {
        id: id.to_owned(),
        function,
        environment: parse_env_ref(v.get("environment")?)?,
        dimension: v
            .get("dimension")
            .and_then(Value::as_str)
            .map_or_else(|| "minecraft:overworld".to_owned(), qualify),
        structure: qualify(v.get("structure")?.as_str()?),
        max_ticks: positive(int("max_ticks", None)?)?,
        setup_ticks: int("setup_ticks", Some(0)).filter(|n| *n >= 0)?,
        required: bool("required", true),
        rotation: match v.get("rotation") {
            Some(r) => Rotation::ALL
                .into_iter()
                .find(|x| test_instance::rotation_name(*x) == r.as_str().unwrap_or(""))?,
            None => Rotation::None,
        },
        max_attempts: positive(int("max_attempts", Some(1))?)?,
        required_successes: positive(int("required_successes", Some(1))?)?,
        sky_access: bool("sky_access", false),
        padding: int("padding", Some(0)).filter(|n| (0..=128).contains(n))?,
    })
}

// --- Test runs -------------------------------------------------------------------------------

/// `RetryOptions`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Retry {
    tries: i32,
    halt_on_failure: bool,
}

impl Retry {
    /// `noRetries`.
    pub(crate) const NONE: Retry = Retry {
        tries: 1,
        halt_on_failure: true,
    };

    pub(crate) fn new(tries: i32, halt_on_failure: bool) -> Retry {
        Retry {
            tries,
            halt_on_failure,
        }
    }

    fn unlimited(self) -> bool {
        self.tries < 1
    }

    fn has_tries_left(self, attempts: i32, successes: i32) -> bool {
        let failures = attempts != successes;
        let more = self.unlimited() || attempts < self.tries;
        more && (!failures || !self.halt_on_failure)
    }

    fn has_retries(self) -> bool {
        self.tries != 1
    }
}

/// `GameTestException` and its kinds.
#[derive(Clone, Debug, PartialEq)]
enum TestError {
    /// `GameTestAssertException`.
    Assert { message: Component, tick: i32 },
    /// `GameTestAssertPosException`.
    Pos {
        message: Component,
        absolute: BlockPos,
        relative: BlockPos,
        tick: i32,
    },
    /// `GameTestTimeoutException`.
    Timeout(Component),
    /// `UnknownGameTestException`: the message of what went wrong.
    Unknown(String),
}

impl TestError {
    /// `getDescription`.
    fn description(&self) -> Component {
        match self {
            TestError::Assert { message, tick } => Component::translatable_args(
                "test.error.tick",
                vec![Arg::Component(Box::new(message.clone())), Arg::Int(*tick)],
            ),
            TestError::Pos {
                message,
                absolute,
                relative,
                tick,
            } => Component::translatable_args(
                "test.error.position",
                vec![
                    Arg::Component(Box::new(message.clone())),
                    Arg::Int(absolute.x),
                    Arg::Int(absolute.y),
                    Arg::Int(absolute.z),
                    Arg::Int(relative.x),
                    Arg::Int(relative.y),
                    Arg::Int(relative.z),
                    Arg::Int(*tick),
                ],
            ),
            TestError::Timeout(message) => message.clone(),
            TestError::Unknown(reason) => Component::translatable_args(
                "test.error.unknown",
                vec![Arg::String(reason.clone())],
            ),
        }
    }

    /// `getMessage`.
    fn message(&self, global: &Global) -> String {
        match self {
            TestError::Unknown(reason) => reason.clone(),
            _ => global.english(&self.description()),
        }
    }
}

/// `ReportGameListener`: shared by a test and its reruns.
#[derive(Clone, Copy, Debug, Default)]
struct Report {
    attempts: i32,
    successes: i32,
}

/// `GameTestInfo`.
#[derive(Debug)]
struct Info {
    test: Arc<TestDef>,
    world: WorldId,
    pos: Option<BlockPos>,
    extra_rotation: Rotation,
    retry: Retry,
    placed: bool,
    chunks_loaded: bool,
    tick: i32,
    started: bool,
    done: bool,
    error: Option<TestError>,
    started_at: Option<Instant>,
    run_ms: u128,
    /// The `onEachTick` checks of a block based test: the next tick to check and the end.
    checks: Option<(i32, i32)>,
    report: usize,
    /// In the run's summary tracker.
    tracked: bool,
    /// In the current batch's tracker.
    in_batch: bool,
}

impl Info {
    fn new(test: Arc<TestDef>, world: WorldId, extra_rotation: Rotation, retry: Retry) -> Info {
        Info {
            test,
            world,
            pos: None,
            extra_rotation,
            retry,
            placed: false,
            chunks_loaded: false,
            tick: 0,
            started: false,
            done: false,
            error: None,
            started_at: None,
            run_ms: 0,
            checks: None,
            report: 0,
            tracked: false,
            in_batch: false,
        }
    }

    /// `getRotation`.
    fn rotation(&self) -> Rotation {
        self.test.rotation.rotated(self.extra_rotation)
    }

    fn fail(&mut self, message: Component) {
        self.error = Some(TestError::Assert {
            message,
            tick: self.tick,
        });
    }

    fn finish(&mut self) {
        if !self.done {
            self.done = true;
            if let Some(at) = self.started_at.take() {
                self.run_ms = at.elapsed().as_millis();
            }
        }
    }

    fn run_time(&self) -> u128 {
        self.started_at
            .map_or(self.run_ms, |at| at.elapsed().as_millis())
    }
}

/// A batch (`GameTestBatch`).
#[derive(Debug)]
struct Batch {
    index: usize,
    tests: Vec<usize>,
    environment: EnvRef,
    world: WorldId,
}

/// `TestEnvironmentDefinition.Activation`.
#[derive(Debug)]
enum Activation {
    AllOf(Vec<Activation>),
    Clock { clock: usize, previous: i64 },
    Difficulty(Difficulty),
    Function(Option<String>),
    Rules(Vec<(Rule, i32)>),
    None,
    Weather(WeatherType),
}

/// Where new tests are built (`StructureSpawner`).
#[derive(Debug)]
enum Spawner {
    /// `IN_PLACE`.
    InPlace,
    /// `NOT_SET`.
    NotSet,
    /// The same spawner as the one for new tests (`/test verify`).
    SameAsNew,
    /// `StructureGridSpawner`, one grid per world.
    Grid(Grid),
}

#[derive(Debug)]
struct Grid {
    tests_per_row: i32,
    clear_on_batch: bool,
    /// `firstTestNorthWestCorner` per world.
    first: BTreeMap<u16, BlockPos>,
    grids: BTreeMap<u16, GridState>,
}

#[derive(Debug)]
struct GridState {
    first: BlockPos,
    next: BlockPos,
    row: Aabb,
    row_count: i32,
    last_batch: Vec<usize>,
}

/// `new AABB(pos)`.
fn block_aabb(p: BlockPos) -> Aabb {
    BlockBox { min: p, max: p }.aabb()
}

fn minmax(a: Aabb, b: Aabb) -> Aabb {
    Aabb::new(
        a.min_x.min(b.min_x),
        a.min_y.min(b.min_y),
        a.min_z.min(b.min_z),
        a.max_x.max(b.max_x),
        a.max_y.max(b.max_y),
        a.max_z.max(b.max_z),
    )
}

#[allow(clippy::cast_possible_truncation, clippy::as_conversions)]
fn size_i(v: f64) -> i32 {
    v as i32
}

/// `GameTestRunner`.
struct Runner {
    infos: Vec<Info>,
    reports: Vec<Report>,
    batches: Vec<Batch>,
    /// The batch that was started last.
    current: Option<usize>,
    /// A batch that waits for its world to tick.
    pending: Option<usize>,
    rerun: Vec<usize>,
    stopped: bool,
    environment: Option<Activation>,
    existing: Spawner,
    new: Spawner,
    halt_on_error: bool,
    clear_between_batches: bool,
    max_per_batch: usize,
    /// `trackAndStartRunner`'s source (batch and summary messages).
    source: CommandSource,
    /// The dimensions of `TestSummaryDisplayer`.
    dimensions: Vec<WorldId>,
}

/// `GameTestTicker.State`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum TickerState {
    #[default]
    Idle,
    Running,
    Halting,
}

/// The game tests of the server (`GameTestTicker.SINGLETON`, `FailedTestTracker`).
#[derive(Default)]
pub(crate) struct GameTests {
    registry: Option<Arc<Registry>>,
    runner: Option<Runner>,
    ticking: Vec<usize>,
    state: TickerState,
    /// `LAST_FAILED_TESTS`.
    failed: Vec<Arc<TestDef>>,
}

/// What a step leaves to do once the game tests are unlocked: functions of test environments
/// (they may run `/test` themselves).
#[derive(Debug, Default)]
struct Deferred {
    functions: Vec<String>,
}

impl Deferred {
    fn run(self, env: &mut Env<'_>) {
        for id in self.functions {
            let library = env.global.systems.functions();
            let Some(function) = library.get(&id).cloned() else {
                log::error!("Test Batch failed for non-existent function {id}");
                continue;
            };
            let Some(arc) = env.global.arc() else {
                return;
            };
            let mut source = CommandSource::new(
                arc,
                SourceKind::Console,
                vanillite_game::commands::level::GAMEMASTERS,
                env.world,
                env.global.systems.spawn(),
                (0.0, 0.0),
                None,
            );
            source.silent = true;
            super::functions::run_function(&function, &source, env);
        }
    }
}

/// The step a world takes with the game tests locked.
struct Step<'e, 'a> {
    env: &'e mut Env<'a>,
    deferred: Deferred,
}

impl GameTests {
    fn registry(&mut self, global: &Global) -> Arc<Registry> {
        Arc::clone(
            self.registry
                .get_or_insert_with(|| Arc::new(Registry::load(global))),
        )
    }

    /// `GameTestTicker.clear`.
    fn clear(&mut self, step: &mut Step<'_, '_>) {
        if self.state != TickerState::Idle {
            self.state = TickerState::Halting;
            return;
        }
        self.ticking.clear();
        if let Some(mut runner) = self.runner.take() {
            runner.stopped = true;
            end_environment(&mut runner, step);
        }
    }
}

/// Runs `f` with the game tests locked, then what it deferred.
fn with_tests<R>(env: &mut Env<'_>, f: impl FnOnce(&mut GameTests, &mut Step<'_, '_>) -> R) -> R {
    let global: &Global = env.global;
    let (result, deferred) = {
        let mut tests = global.systems.game_tests();
        let mut step = Step {
            env,
            deferred: Deferred::default(),
        };
        let result = f(&mut tests, &mut step);
        (result, step.deferred)
    };
    deferred.run(env);
    result
}

/// Sends `message` to an online player that accepts system messages.
fn tell_player(global: &Global, uuid: u128, message: Component) {
    if let Some((sink, inbox)) = global.players.connection(Uuid(uuid)) {
        let accepts = inbox
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .accepts_system(false);
        if accepts {
            sink.send(&SystemChat {
                content: message,
                overlay: false,
            });
        }
    }
}

/// `ReportGameListener.say`: to every player.
fn say(global: &Global, color: NamedColor, text: String) {
    global.players.broadcast_system(
        Component::text(text).color(Color::Named(color)),
        false,
        None,
    );
}

/// The server as a source (`createCommandSourceStack`).
fn server_source(global: &Global) -> Option<CommandSource> {
    Some(CommandSource::new(
        global.arc()?,
        SourceKind::Console,
        4,
        WorldId(0),
        global.systems.spawn(),
        (0.0, 0.0),
        None,
    ))
}

// --- Environments ----------------------------------------------------------------------------

fn activate(def: &EnvDef, registry: &Registry, step: &mut Step<'_, '_>) -> Activation {
    let global = step.env.global;
    match def {
        EnvDef::AllOf(list) => Activation::AllOf(
            list.iter()
                .map(|r| match registry.environment(r) {
                    Some(d) => activate(d, registry, step),
                    None => Activation::None,
                })
                .collect(),
        ),
        EnvDef::ClockTime { clock, time } => {
            let mut clocks = global.level.clocks();
            let Some(index) = clocks.index(clock) else {
                return Activation::None;
            };
            let previous = clocks.clocks.get(index).map_or(0, |c| c.total_ticks);
            clocks.set_total_ticks(index, i64::from(*time));
            drop(clocks);
            clock_changed(global, index);
            Activation::Clock {
                clock: index,
                previous,
            }
        }
        EnvDef::Difficulty(d) => {
            let previous = global.level.difficulty();
            set_difficulty(global, *d);
            Activation::Difficulty(previous)
        }
        EnvDef::Function { setup, teardown } => {
            if let Some(f) = setup {
                step.deferred.functions.push(f.clone());
            }
            Activation::Function(teardown.clone())
        }
        EnvDef::GameRules(rules) => {
            let rules: Vec<(Rule, i32)> = rules
                .iter()
                .filter_map(|(name, v)| {
                    Rule::by_name(name.strip_prefix("minecraft:").unwrap_or(name)).map(|r| (r, *v))
                })
                .collect();
            let previous = {
                let current = global.level.rules();
                rules.iter().map(|(r, _)| (*r, current.get(*r))).collect()
            };
            set_rules(global, &rules);
            Activation::Rules(previous)
        }
        // Environment attribute timelines are not modelled.
        EnvDef::Timelines => Activation::None,
        EnvDef::Weather(w) => {
            let previous = {
                let data = global.level.weather();
                if data.thundering {
                    WeatherType::Thunder
                } else if data.raining {
                    WeatherType::Rain
                } else {
                    WeatherType::Clear
                }
            };
            w.apply(global);
            Activation::Weather(previous)
        }
    }
}

fn teardown(activation: Activation, step: &mut Step<'_, '_>) {
    let global = step.env.global;
    match activation {
        Activation::AllOf(list) => {
            for a in list.into_iter().rev() {
                teardown(a, step);
            }
        }
        Activation::Clock { clock, previous } => {
            global.level.clocks().set_total_ticks(clock, previous);
            clock_changed(global, clock);
        }
        Activation::Difficulty(d) => set_difficulty(global, d),
        Activation::Function(f) => {
            if let Some(f) = f {
                step.deferred.functions.push(f);
            }
        }
        Activation::Rules(rules) => set_rules(global, &rules),
        Activation::None => {}
        Activation::Weather(w) => w.apply(global),
    }
}

fn clock_changed(global: &Global, clock: usize) {
    crate::survival::broadcast_all(global, &crate::survival::clock_update(&global.level, clock));
}

/// `MinecraftServer.setDifficulty(difficulty, true)`.
fn set_difficulty(global: &Global, difficulty: Difficulty) {
    global.level.set_difficulty(difficulty);
    global
        .players
        .broadcast(&crate::survival::change_difficulty(&global.level), None);
}

/// `GameRules.setAll(map, server)` with the rules' change callbacks.
fn set_rules(global: &Global, rules: &[(Rule, i32)]) {
    for &(rule, value) in rules {
        let changed = {
            let mut current = global.level.rules_mut();
            let changed = current.get(rule) != value;
            current.set(rule, value);
            changed
        };
        if !changed {
            continue;
        }
        super::survival::on_game_rule_changed(global, rule, value);
    }
}

fn end_environment(runner: &mut Runner, step: &mut Step<'_, '_>) {
    if let Some(a) = runner.environment.take() {
        teardown(a, step);
    }
}

// --- Test instance blocks --------------------------------------------------------------------

fn shape_of(registry: &Registry, level: &Level, pos: BlockPos) -> TestShape {
    test_instance::get(level, pos)
        .and_then(|t| t.data.test.as_deref())
        .and_then(|id| registry.get(id))
        .map(|t| t.shape())
        .unwrap_or_default()
}

/// `getStructureTemplate(level, test)`.
fn template_of(
    registry: &Registry,
    level: &Level,
    test: &str,
) -> Option<Arc<vanillite_worldgen::template::structure::StructureTemplate>> {
    level.structure_template(&registry.get(test)?.structure)
}

/// `TestInstanceBlockEntity.placeStructure()`: whether the test had a structure.
fn place_test_structure(registry: &Registry, level: &mut Level, pos: BlockPos) -> bool {
    let Some(test) = test_instance::get(level, pos).and_then(|t| t.data.test.clone()) else {
        return false;
    };
    let Some(template) = template_of(registry, level, &test) else {
        return false;
    };
    let shape = shape_of(registry, level, pos);
    test_instance::place_structure(level, pos, shape, &template);
    true
}

fn coords(pos: BlockPos) -> Vec<Arg> {
    vec![Arg::Int(pos.x), Arg::Int(pos.y), Arg::Int(pos.z)]
}

/// `TestInstanceBlockEntity.resetTest`.
fn reset_test(
    registry: &Registry,
    level: &mut Level,
    pos: BlockPos,
    feedback: &mut dyn FnMut(Component),
) {
    let shape = shape_of(registry, level, pos);
    let template = test_instance::get(level, pos)
        .and_then(|t| t.data.test.clone())
        .and_then(|test| template_of(registry, level, &test));
    test_instance::reset_test(level, pos, shape, template.as_ref(), feedback);
}

/// `TestInstanceBlockEntity.saveTest`.
fn save_test(
    registry: &Registry,
    level: &mut Level,
    pos: BlockPos,
    feedback: &mut dyn FnMut(Component),
) -> Option<String> {
    let shape = shape_of(registry, level, pos);
    let structure = test_instance::get(level, pos)
        .and_then(|t| t.data.test.as_deref())
        .and_then(|id| registry.get(id))
        .map(|t| t.structure.clone());
    test_instance::save_test(level, pos, shape, structure, feedback)
}

/// `TestInstanceBlockEntity.exportTest`: a dedicated server has no test structure directory.
fn export_test(
    registry: &Registry,
    level: &mut Level,
    pos: BlockPos,
    feedback: &mut dyn FnMut(Component),
) -> bool {
    if save_test(registry, level, pos, feedback).is_none() {
        return false;
    }
    feedback(
        Component::text("Test structure exporting is disabled")
            .color(Color::Named(NamedColor::Red)),
    );
    true
}

/// `handleTestInstanceBlockAction` of the queued packets of the tick.
pub(crate) fn handle_actions(env: &mut Env<'_>) {
    if env.level.test_instance_actions.is_empty() {
        return;
    }
    let actions = std::mem::take(&mut env.level.test_instance_actions);
    for a in actions {
        handle_action(env, a);
    }
}

fn handle_action(env: &mut Env<'_>, a: Action) {
    let global = env.global;
    if test_instance::get(env.level, a.pos).is_none() {
        return;
    }
    let player = a.player;
    let mut feedback = |m: Component| tell_player(global, player, m);
    if a.action == action::QUERY || a.action == action::INIT {
        let registry = with_tests(env, |tests, step| tests.registry(step.env.global));
        let test = a.data.test.as_deref().and_then(|id| registry.get(id));
        let status = match test {
            Some(t) => t.describe(),
            None => Component::translatable("test_instance.description.no_test", vec![])
                .color(Color::Named(NamedColor::Red)),
        };
        let size = if a.action == action::QUERY {
            a.data
                .test
                .as_deref()
                .and_then(|t| template_of(&registry, env.level, t))
                .map(|t| {
                    let s = t.size();
                    [s.x, s.y, s.z]
                })
        } else {
            None
        };
        if let Some((sink, _)) = global.players.connection(Uuid(player)) {
            sink.send(&TestInstanceBlockStatus { status, size });
        }
        return;
    }
    test_instance::set(env.level, a.pos, a.data);
    let registry = with_tests(env, |tests, step| tests.registry(step.env.global));
    match a.action {
        action::RESET => reset_test(&registry, env.level, a.pos, &mut feedback),
        action::SAVE => {
            save_test(&registry, env.level, a.pos, &mut feedback);
        }
        action::EXPORT => {
            export_test(&registry, env.level, a.pos, &mut feedback);
        }
        action::RUN => run_test_block(env, a.pos, &mut feedback),
        _ => {}
    }
    env.level.send_block_entity_updated(a.pos);
}

/// `TestInstanceBlockEntity.runTest`.
fn run_test_block(env: &mut Env<'_>, pos: BlockPos, feedback: &mut dyn FnMut(Component)) {
    with_tests(env, |tests, step| {
        let registry = tests.registry(step.env.global);
        let level = &mut *step.env.level;
        let Some(block) = test_instance::get(level, pos) else {
            return;
        };
        let test = block
            .data
            .test
            .as_deref()
            .and_then(|id| registry.get(id))
            .cloned();
        let rotation = block.data.rotation;
        let Some(test) = test else {
            feedback(
                Component::translatable_args("test_instance_block.error.no_test", coords(pos))
                    .color(Color::Named(NamedColor::Red)),
            );
            return;
        };
        if !place_test_structure(&registry, level, pos) {
            feedback(
                Component::translatable_args(
                    "test_instance_block.error.no_test_structure",
                    coords(pos),
                )
                .color(Color::Named(NamedColor::Red)),
            );
            return;
        }
        test_instance::clear_error_markers(level, pos);
        tests.clear(step);
        tests.failed.clear();
        feedback(Component::translatable_args(
            "test_instance_block.starting",
            vec![Arg::String(test.id.clone())],
        ));
        let world = step.env.world;
        let mut info = Info::new(Arc::clone(&test), world, rotation, Retry::NONE);
        info.pos = Some(pos);
        let Some(source) = server_source(step.env.global) else {
            return;
        };
        let runner = Runner::from_infos(
            vec![info],
            MAX_TESTS_PER_BATCH,
            Spawner::InPlace,
            Spawner::NotSet,
            false,
            false,
            source,
        );
        track_and_start(tests, step, runner);
    });
}

// --- The runner ------------------------------------------------------------------------------

impl Runner {
    fn from_infos(
        mut infos: Vec<Info>,
        max_per_batch: usize,
        existing: Spawner,
        new: Spawner,
        halt_on_error: bool,
        clear_between_batches: bool,
        source: CommandSource,
    ) -> Runner {
        let mut reports = Vec::new();
        for (i, info) in infos.iter_mut().enumerate() {
            info.report = i;
            reports.push(Report::default());
        }
        let all: Vec<usize> = (0..infos.len()).collect();
        let batches = batch(&infos, &all, max_per_batch);
        Runner {
            infos,
            reports,
            batches,
            current: None,
            pending: None,
            rerun: Vec::new(),
            stopped: true,
            environment: None,
            existing,
            new,
            halt_on_error,
            clear_between_batches,
            max_per_batch,
            source,
            dimensions: Vec::new(),
        }
    }
}

/// `GameTestBatchFactory.fromGameTestInfo(max)`: grouped by environment and dimension in
/// order of first appearance, split into batches of at most `max`.
fn batch(infos: &[Info], tests: &[usize], max: usize) -> Vec<Batch> {
    let mut groups: Vec<(EnvRef, WorldId, Vec<usize>)> = Vec::new();
    for &i in tests {
        let info = &infos[i];
        let key = (&info.test.environment, info.world);
        match groups.iter_mut().find(|(e, w, _)| (e, *w) == key) {
            Some((_, _, list)) => list.push(i),
            None => groups.push((info.test.environment.clone(), info.world, vec![i])),
        }
    }
    let mut out = Vec::new();
    for (environment, world, list) in groups {
        for (index, chunk) in list.chunks(max.max(1)).enumerate() {
            out.push(Batch {
                index,
                tests: chunk.to_vec(),
                environment: environment.clone(),
                world,
            });
        }
    }
    out
}

/// `TestCommand.trackAndStartRunner`.
fn track_and_start(tests: &mut GameTests, step: &mut Step<'_, '_>, mut runner: Runner) -> i32 {
    for info in &mut runner.infos {
        info.tracked = true;
    }
    runner.stopped = false;
    tests.ticking.clear();
    tests.runner = Some(runner);
    run_batch(tests, step, 0);
    1
}

/// `GameTestRunner.runBatch`.
fn run_batch(tests: &mut GameTests, step: &mut Step<'_, '_>, index: usize) {
    let Some(runner) = tests.runner.as_mut() else {
        return;
    };
    if index >= runner.batches.len() {
        end_environment(runner, step);
        run_scheduled_reruns(tests, step);
        return;
    }
    if runner.batches[index].world != step.env.world {
        runner.pending = Some(index);
        return;
    }
    runner.pending = None;
    let registry = tests.registry(step.env.global);
    let Some(runner) = tests.runner.as_mut() else {
        return;
    };
    let level = &mut *step.env.level;
    if index > 0 && runner.clear_between_batches {
        let last: Vec<usize> = runner.batches[index - 1].tests.clone();
        for i in last {
            if let Some(pos) = runner.infos[i].pos {
                let shape = shape_of(&registry, level, pos);
                if let Some(t) = test_instance::get(level, pos) {
                    let area = t.test_box(pos, shape);
                    test_instance::clear_space(level, area);
                }
                level.destroy_block(pos, false, None);
            }
        }
    }
    let world = step.env.world;
    {
        let Runner {
            infos,
            existing,
            new,
            ..
        } = &mut *runner;
        existing.on_batch_start(&registry, level, world, infos);
        new.on_batch_start(&registry, level, world, infos);
    }
    let batch_tests = runner.batches[index].tests.clone();
    let mut spawned = Vec::new();
    for i in batch_tests {
        let in_place = runner.infos[i].pos.is_some();
        let ok = {
            let Runner {
                infos,
                reports,
                existing,
                new,
                ..
            } = &mut *runner;
            let spawner = if in_place && !matches!(existing, Spawner::SameAsNew) {
                existing
            } else {
                new
            };
            spawner.spawn(&registry, level, infos, reports, i)
        };
        if ok {
            spawned.push(i);
        }
    }
    let environment = runner.batches[index].environment.clone();
    log::info!(
        "Running test environment '{}' batch {} ({} tests)...",
        environment.registered_name(),
        runner.batches[index].index,
        spawned.len()
    );
    end_environment(runner, step);
    let activation = registry
        .environment(&environment)
        .map(|d| activate(d, &registry, step));
    let Some(runner) = tests.runner.as_mut() else {
        return;
    };
    runner.environment = activation;
    runner.current = Some(index);
    // `TestBatchSummaryDisplayer.testBatchStarting`.
    runner.source.send_success(
        step.env.global,
        Component::translatable_args(
            "commands.test.batch.starting",
            vec![
                Arg::String(environment.registered_name()),
                Arg::Int(i32::try_from(runner.batches[index].index).unwrap_or(i32::MAX)),
            ],
        ),
        true,
    );
    for &i in &spawned {
        runner.infos[i].in_batch = true;
    }
    runner.batches[index].tests = spawned.clone();
    tests.ticking.extend(spawned);
}

/// `GameTestRunner.runScheduledRerunTests`.
fn run_scheduled_reruns(tests: &mut GameTests, step: &mut Step<'_, '_>) {
    let Some(runner) = tests.runner.as_mut() else {
        return;
    };
    if runner.rerun.is_empty() {
        runner.batches.clear();
        runner.stopped = true;
        return;
    }
    let names: Vec<String> = runner
        .rerun
        .iter()
        .map(|&i| runner.infos[i].test.id.clone())
        .collect();
    log::info!("Starting re-run of tests: {}", names.join(", "));
    let rerun = std::mem::take(&mut runner.rerun);
    runner.batches = batch(&runner.infos, &rerun, runner.max_per_batch);
    runner.stopped = false;
    run_batch(tests, step, 0);
}

/// `GameTestRunner.rerunTest`.
fn rerun_test(runner: &mut Runner, i: usize) -> usize {
    let old = &runner.infos[i];
    let mut copy = Info::new(
        Arc::clone(&old.test),
        old.world,
        old.extra_rotation,
        old.retry,
    );
    copy.pos = old.pos;
    copy.report = old.report;
    copy.tracked = old.tracked;
    runner.infos.push(copy);
    let index = runner.infos.len() - 1;
    runner.rerun.push(index);
    index
}

impl Spawner {
    /// `onBatchStart`: a grid that clears on batches clears the last batch's tests and starts
    /// at its first corner again.
    fn on_batch_start(
        &mut self,
        registry: &Registry,
        level: &mut Level,
        world: WorldId,
        infos: &[Info],
    ) {
        let Spawner::Grid(grid) = self else {
            return;
        };
        if !grid.clear_on_batch {
            return;
        }
        let Some(state) = grid.grids.get_mut(&world.0) else {
            return;
        };
        for &i in &state.last_batch {
            let Some(pos) = infos.get(i).and_then(|t| t.pos) else {
                continue;
            };
            let shape = shape_of(registry, level, pos);
            if let Some(t) = test_instance::get(level, pos) {
                let area = t.test_box(pos, shape);
                test_instance::clear_space(level, area);
            }
        }
        state.last_batch.clear();
        state.next = state.first;
        state.row = block_aabb(state.first);
        state.row_count = 0;
    }

    /// `spawnStructure`: whether the test got its structure.
    fn spawn(
        &mut self,
        registry: &Registry,
        level: &mut Level,
        infos: &mut [Info],
        reports: &mut [Report],
        i: usize,
    ) -> bool {
        match self {
            Spawner::NotSet | Spawner::SameAsNew => false,
            Spawner::InPlace => {
                if prepare_structure(registry, level, &mut infos[i], reports) {
                    start_execution(&mut infos[i], 1);
                    true
                } else {
                    false
                }
            }
            Spawner::Grid(grid) => {
                let world = infos[i].world;
                let first = grid.first.get(&world.0).copied().unwrap_or_default();
                let state = grid.grids.entry(world.0).or_insert_with(|| GridState {
                    first,
                    next: first,
                    row: block_aabb(first),
                    row_count: 0,
                    last_batch: Vec::new(),
                });
                let corner = state.next;
                infos[i].pos = Some(corner);
                if !prepare_structure(registry, level, &mut infos[i], reports) {
                    return false;
                }
                start_execution(&mut infos[i], 1);
                let shape = infos[i].test.shape();
                let Some(t) = test_instance::get(level, corner) else {
                    return true;
                };
                let bounds = t.test_box(corner, shape).aabb();
                state.row = minmax(state.row, bounds);
                state.next.x += size_i(bounds.max_x - bounds.min_x) + 5;
                state.row_count += 1;
                if state.row_count >= grid.tests_per_row {
                    state.row_count = 0;
                    state.next.z += size_i(state.row.max_z - state.row.min_z) + 6;
                    state.next.x = state.first.x;
                    state.row = block_aabb(state.next);
                }
                state.last_batch.push(i);
                true
            }
        }
    }
}

/// `GameTestInfo.startExecution(delay)`.
fn start_execution(info: &mut Info, delay: i32) {
    info.tick = -(info.test.setup_ticks + delay + 1);
}

/// `GameTestInfo.prepareTestStructure`: a test instance block at the test's position with its
/// structure placed.
fn prepare_structure(
    registry: &Registry,
    level: &mut Level,
    info: &mut Info,
    reports: &mut [Report],
) -> bool {
    let Some(pos) = info.pos else {
        return false;
    };
    level.set_block(pos, ids::TEST_INSTANCE_BLOCK.default_state(), update::ALL);
    if test_instance::get(level, pos).is_none() {
        return false;
    }
    let size = level
        .structure_template(&info.test.structure)
        .map_or([1, 1, 1], |t| {
            let s = t.size();
            [s.x, s.y, s.z]
        });
    test_instance::set(
        level,
        pos,
        Data {
            test: Some(info.test.id.clone()),
            size,
            rotation: info.extra_rotation,
            ignore_entities: false,
            status: Status::Cleared,
            error_message: None,
        },
    );
    place_structure(registry, level, info, reports);
    true
}

/// `GameTestInfo.placeStructure`.
fn place_structure(
    registry: &Registry,
    level: &mut Level,
    info: &mut Info,
    reports: &mut [Report],
) {
    if info.placed {
        return;
    }
    let Some(pos) = info.pos else {
        return;
    };
    if !place_test_structure(registry, level, pos) {
        let name = test_instance::get(level, pos).map_or_else(
            || Component::translatable("test_instance_block.invalid_test", vec![]),
            TestInstance::test_name,
        );
        let name = level_english(&name);
        info.fail(Component::translatable_args(
            "test.error.structure.failure",
            vec![Arg::String(name)],
        ));
    }
    info.placed = true;
    let shape = info.test.shape();
    test_instance::encase_structure(level, pos, shape);
    if let Some(t) = test_instance::get(level, pos) {
        let area = t.test_box(pos, shape);
        level.chunks.clear_block_ticks(area.min, area.max);
        level.clear_block_events(area.min, area.max);
    }
    // `ReportGameListener.testStructureLoaded`.
    if let Some(r) = reports.get_mut(info.report) {
        r.attempts += 1;
    }
}

/// `Component.getString` of a test name (a literal id or the invalid test key).
fn level_english(name: &Component) -> String {
    name.plain()
}

/// `GameTestTicker.tick` for the tests of `world`, and a batch that waits for this world.
pub(crate) fn tick(env: &mut Env<'_>) {
    with_tests(env, |tests, step| {
        let Some(runner) = tests.runner.as_ref() else {
            return;
        };
        let pending = runner
            .pending
            .filter(|&p| runner.batches.get(p).map(|b| b.world) == Some(step.env.world));
        if let Some(pending) = pending {
            run_batch(tests, step, pending);
        }
        if tests.runner.is_none() {
            return;
        }
        tests.state = TickerState::Running;
        let snapshot = tests.ticking.clone();
        let registry = tests.registry(step.env.global);
        for i in snapshot {
            let Some(runner) = tests.runner.as_ref() else {
                break;
            };
            if runner
                .infos
                .get(i)
                .is_none_or(|t| t.world != step.env.world)
            {
                continue;
            }
            tick_info(tests, step, &registry, i);
        }
        if let Some(runner) = tests.runner.as_ref() {
            tests
                .ticking
                .retain(|&i| runner.infos.get(i).is_some_and(|t| !t.done));
        }
        let finishing = tests.state;
        tests.state = TickerState::Idle;
        if finishing == TickerState::Halting {
            tests.clear(step);
        }
    });
}

/// `GameTestInfo.tick(runner)`.
fn tick_info(tests: &mut GameTests, step: &mut Step<'_, '_>, registry: &Registry, i: usize) {
    let Some(runner) = tests.runner.as_mut() else {
        return;
    };
    let level = &mut *step.env.level;
    let info = &mut runner.infos[i];
    if info.done {
        return;
    }
    if !info.placed {
        info.fail(Component::translatable(
            "test.error.ticking_without_structure",
            vec![],
        ));
    }
    let block = info.pos.and_then(|p| test_instance::get(level, p).cloned());
    if block.is_none() {
        info.fail(Component::translatable(
            "test.error.missing_block_entity",
            vec![],
        ));
    }
    if info.error.is_some() {
        info.finish();
    } else if !info.chunks_loaded {
        let (Some(pos), Some(block)) = (info.pos, block.as_ref()) else {
            return;
        };
        let area = block.structure_box(pos, info.test.shape());
        if !area
            .chunks()
            .all(|c| level.entities.store.is_chunk_ticking(c.x, c.z))
        {
            return;
        }
        info.chunks_loaded = true;
    }
    tick_internal(info, level, registry);
    if !info.done {
        return;
    }
    if info.error.is_some() {
        test_failed(tests, step, i);
    } else {
        test_passed(tests, step, i);
    }
}

/// The block based test of a test (`GameTestHelper` over its structure).
fn block_based(level: &Level, info: &Info) -> Option<test_block::BlockBasedTest> {
    let pos = info.pos?;
    let block = test_instance::get(level, pos)?;
    let shape = info.test.shape();
    let b = block.structure_box(pos, shape);
    let mut size = [
        b.max.x - b.min.x + 1,
        b.max.y - b.min.y + 1,
        b.max.z - b.min.z + 1,
    ];
    if matches!(
        info.rotation(),
        Rotation::Clockwise90 | Rotation::Counterclockwise90
    ) {
        size.swap(0, 2);
    }
    Some(test_block::BlockBasedTest {
        origin: block.start_corner(pos, shape),
        rotation: info.rotation(),
        size,
    })
}

/// The exception of a failed assertion at `tick`.
fn assertion(failure: test_block::Failure, tick: i32) -> TestError {
    match failure.at {
        Some((absolute, relative)) => TestError::Pos {
            message: *failure.message,
            absolute,
            relative,
            tick,
        },
        None => TestError::Assert {
            message: *failure.message,
            tick,
        },
    }
}

/// `GameTestInfo.tickInternal`.
fn tick_internal(info: &mut Info, level: &mut Level, registry: &Registry) {
    info.tick += 1;
    if info.tick < 0 {
        return;
    }
    if !info.started {
        start_test(info, level);
    }
    // `runAtTickTimeMap`: one check per tick from the start until the timeout.
    while let Some((next, until)) = info.checks {
        if next > info.tick || next >= until {
            break;
        }
        info.checks = Some((next + 1, until));
        let Some(test) = block_based(level, info) else {
            break;
        };
        match test.check(level) {
            Ok(true) => succeed(info, level),
            Ok(false) => {}
            Err(e) => info.error = Some(assertion(e, info.tick)),
        }
    }
    let _ = registry;
    if info.tick > info.test.max_ticks {
        info.error = Some(TestError::Timeout(Component::translatable_args(
            "test.error.timeout.no_result",
            vec![Arg::Int(info.test.max_ticks)],
        )));
    }
}

/// `GameTestInfo.startTest`.
fn start_test(info: &mut Info, level: &mut Level) {
    info.started = true;
    info.started_at = Some(Instant::now());
    if let Some(pos) = info.pos {
        test_instance::set_status(level, pos, Status::Running);
    }
    match info.test.function.clone() {
        // `BuiltinTestFunctions.ALWAYS_PASS`.
        Some(f) if f == "minecraft:always_pass" => succeed(info, level),
        Some(f) => {
            info.error = Some(TestError::Unknown(format!(
                "Trying to access missing test function: {f}"
            )));
        }
        None => {
            let Some(test) = block_based(level, info) else {
                return;
            };
            match test.run(level) {
                Ok(()) => info.checks = Some((info.tick, info.test.max_ticks)),
                Err(e) => info.error = Some(assertion(e, info.tick)),
            }
        }
    }
}

/// `GameTestInfo.succeed`: entities other than players in the structure go.
fn succeed(info: &mut Info, level: &mut Level) {
    if info.error.is_some() {
        return;
    }
    info.finish();
    let Some(pos) = info.pos else {
        return;
    };
    let Some(block) = test_instance::get(level, pos) else {
        return;
    };
    let b = block.structure_box(pos, info.test.shape()).aabb();
    let area = Aabb::new(
        b.min_x - 1.0,
        b.min_y - 1.0,
        b.min_z - 1.0,
        b.max_x + 1.0,
        b.max_y + 1.0,
        b.max_z + 1.0,
    );
    let mut found = Vec::new();
    level
        .entities
        .store
        .for_each_non_player_in(&area, |id| found.push(id));
    for id in found {
        if level.entities.player(id).is_none() {
            level.entities.despawn(id);
        }
    }
}

/// `ReportGameListener.reportPassed`.
fn report_passed(runner: &Runner, level: &mut Level, global: &Global, i: usize, text: String) {
    if let Some(pos) = runner.infos[i].pos
        && test_instance::get(level, pos).is_some()
    {
        test_instance::set_status(level, pos, Status::Finished);
    }
    say(global, NamedColor::Green, text);
    log::info!(
        "{} passed! ({}ms)",
        runner.infos[i].test.id,
        runner.infos[i].run_time()
    );
}

/// `ReportGameListener.reportFailure` with the error message of `visualizeFailedTest`.
fn report_failure(
    runner: &Runner,
    level: &mut Level,
    global: &Global,
    i: usize,
    description: Component,
    message: String,
    root: Option<&TestError>,
) {
    let info = &runner.infos[i];
    if let Some(pos) = info.pos
        && test_instance::get(level, pos).is_some()
    {
        test_instance::set_error_message(level, pos, description);
    }
    let text = format!(
        "{}{} failed! {}",
        if info.test.required {
            ""
        } else {
            "(optional) "
        },
        info.test.id,
        message
    );
    let color = if info.test.required {
        NamedColor::Red
    } else {
        NamedColor::Yellow
    };
    say(global, color, text.clone());
    if let Some(TestError::Pos {
        absolute, message, ..
    }) = root
        && let Some(pos) = info.pos
    {
        test_instance::mark_error(level, pos, *absolute, message.clone());
    }
    if info.test.required {
        log::error!("{text}");
    } else {
        log::warn!("{text}");
    }
}

/// The description and the message `reportFailure` shows for a test's own error.
fn failure_text(global: &Global, error: &TestError) -> (Component, String) {
    let description = match error {
        TestError::Assert { .. } | TestError::Pos { .. } => error.description(),
        _ => Component::text(error.message(global)),
    };
    (description, error.message(global))
}

/// `ReportGameListener.handleRetry`.
fn handle_retry(tests: &mut GameTests, step: &mut Step<'_, '_>, i: usize, passed: bool) {
    let global = step.env.global;
    let Some(runner) = tests.runner.as_mut() else {
        return;
    };
    let info = &runner.infos[i];
    let report = runner.reports[info.report];
    let retry = info.retry;
    let mut report_as = format!(
        "[Run: {:4}, Ok: {:4}, Fail: {:4}",
        report.attempts,
        report.successes,
        report.attempts - report.successes
    );
    if !retry.unlimited() {
        report_as.push_str(&format!(", Left: {:4}", retry.tries - report.attempts));
    }
    report_as.push(']');
    let name = format!(
        "{} {}! {}ms",
        info.test.id,
        if passed { "passed" } else { "failed" },
        info.run_time()
    );
    let text = format!("{report_as:<53}{name}");
    if passed {
        report_passed(runner, step.env.level, global, i, text);
    } else {
        say(global, NamedColor::Red, text);
    }
    if retry.has_tries_left(report.attempts, report.successes) {
        rerun(tests, step, i);
    }
}

/// `GameTestRunner.rerunTest` with the summary tracker taking the copy.
fn rerun(tests: &mut GameTests, step: &mut Step<'_, '_>, i: usize) {
    let Some(runner) = tests.runner.as_mut() else {
        return;
    };
    rerun_test(runner, i);
    if runner.stopped {
        run_scheduled_reruns(tests, step);
    }
}

/// The listeners of a passed test in Vanilla's order: the report, the summary, the batch.
fn test_passed(tests: &mut GameTests, step: &mut Step<'_, '_>, i: usize) {
    let global = step.env.global;
    // `ReportGameListener.testPassed`.
    {
        let Some(runner) = tests.runner.as_mut() else {
            return;
        };
        let report = runner.infos[i].report;
        runner.reports[report].successes += 1;
    }
    let (retry, flaky, required, attempts, successes, tick) = {
        let Some(runner) = tests.runner.as_ref() else {
            return;
        };
        let info = &runner.infos[i];
        let r = runner.reports[info.report];
        (
            info.retry,
            info.test.max_attempts > 1,
            info.test.required_successes,
            r.attempts,
            r.successes,
            info.tick,
        )
    };
    if retry.has_retries() {
        handle_retry(tests, step, i, true);
    } else if !flaky {
        if let Some(runner) = tests.runner.as_ref() {
            let info = &runner.infos[i];
            let text = format!(
                "{} passed! ({}ms / {}gameticks)",
                info.test.id,
                info.run_time(),
                tick
            );
            report_passed(runner, step.env.level, global, i, text);
        }
    } else if successes >= required {
        if let Some(runner) = tests.runner.as_ref() {
            let text = format!(
                "{} passed {} times of {} attempts.",
                runner.infos[i].test.id, successes, attempts
            );
            report_passed(runner, step.env.level, global, i, text);
        }
    } else {
        if let Some(runner) = tests.runner.as_ref() {
            say(
                global,
                NamedColor::Green,
                format!(
                    "Flaky test {} succeeded, attempt: {} successes: {}",
                    runner.infos[i].test.id, attempts, successes
                ),
            );
        }
        rerun(tests, step, i);
    }
    summary_listener(tests, step, i);
    batch_listener(tests, step, i, true);
}

/// The listeners of a failed test: the report, the summary, the failed tests, the batch.
fn test_failed(tests: &mut GameTests, step: &mut Step<'_, '_>, i: usize) {
    let global = step.env.global;
    let Some(runner) = tests.runner.as_ref() else {
        return;
    };
    let info = &runner.infos[i];
    let Some(error) = info.error.clone() else {
        return;
    };
    let report = runner.reports[info.report];
    let test = Arc::clone(&info.test);
    let has_retries = info.retry.has_retries();
    if test.max_attempts <= 1 {
        let (description, message) = failure_text(global, &error);
        report_failure(
            runner,
            step.env.level,
            global,
            i,
            description,
            message,
            Some(&error),
        );
        if has_retries {
            handle_retry(tests, step, i, false);
        }
    } else {
        let mut text = format!(
            "Flaky test {} failed, attempt: {}/{}",
            test.id, report.attempts, test.max_attempts
        );
        if test.required_successes > 1 {
            text.push_str(&format!(
                ", successes: {} ({} required)",
                report.successes, test.required_successes
            ));
        }
        say(global, NamedColor::Yellow, text);
        if test.max_attempts - report.attempts + report.successes >= test.required_successes {
            rerun(tests, step, i);
        } else if let Some(runner) = tests.runner.as_ref() {
            // `ExhaustedAttemptsException` with the test's error as its cause.
            let cause = error.message(global);
            let message = format!(
                "Not enough successes: {} out of {} attempts. Required successes: {}. max attempts: {}. cause: {}",
                report.successes,
                report.attempts,
                test.required_successes,
                test.max_attempts,
                cause
            );
            report_failure(
                runner,
                step.env.level,
                global,
                i,
                Component::text(cause),
                message,
                Some(&error),
            );
        }
    }
    summary_listener(tests, step, i);
    // `FailedTestTracker.rememberFailedTest`.
    if let Some(runner) = tests.runner.as_ref()
        && runner.infos[i].tracked
    {
        let test = Arc::clone(&runner.infos[i].test);
        if !tests.failed.iter().any(|t| t.id == test.id) {
            tests.failed.push(test);
        }
    }
    batch_listener(tests, step, i, false);
}

/// `TestCommand.TestSummaryDisplayer`.
fn summary_listener(tests: &mut GameTests, step: &mut Step<'_, '_>, i: usize) {
    let global = step.env.global;
    let Some(runner) = tests.runner.as_mut() else {
        return;
    };
    if !runner.infos[i].tracked {
        return;
    }
    let world = runner.infos[i].world;
    if !runner.dimensions.contains(&world) {
        runner.dimensions.push(world);
    }
    let tracked: Vec<&Info> = runner.infos.iter().filter(|t| t.tracked).collect();
    if !tracked.iter().all(|t| t.done) {
        return;
    }
    let total = tracked.len();
    let failed_required = tracked
        .iter()
        .filter(|t| t.error.is_some() && t.test.required)
        .count();
    let failed_optional = tracked
        .iter()
        .filter(|t| t.error.is_some() && !t.test.required)
        .count();
    let source = runner.source.clone();
    let dimensions = runner.dimensions.clone();
    let int = |n: usize| Arg::Int(i32::try_from(n).unwrap_or(i32::MAX));
    source.send_success(
        global,
        Component::translatable_args("commands.test.summary", vec![int(total)])
            .color(Color::Named(NamedColor::White)),
        true,
    );
    if failed_required > 0 {
        source.send_failure(
            global,
            Component::translatable_args(
                "commands.test.summary.failed",
                vec![int(failed_required)],
            ),
        );
    } else {
        source.send_success(
            global,
            Component::translatable("commands.test.summary.all_required_passed", vec![])
                .color(Color::Named(NamedColor::Green)),
            true,
        );
    }
    if failed_optional > 0 {
        source.send_system(
            global,
            Component::translatable_args(
                "commands.test.summary.optional_failed",
                vec![int(failed_optional)],
            ),
        );
    }
    output_player_coordinates(&source, step.env, &dimensions);
    output_test_coordinates(&source, step.env, &dimensions);
}

/// The batch tracker's listener (`GameTestRunner.runBatch`).
fn batch_listener(tests: &mut GameTests, step: &mut Step<'_, '_>, i: usize, passed: bool) {
    let registry = tests.registry(step.env.global);
    let Some(runner) = tests.runner.as_mut() else {
        return;
    };
    if !runner.infos[i].in_batch {
        return;
    }
    if passed && let Some(pos) = runner.infos[i].pos {
        let shape = shape_of(&registry, step.env.level, pos);
        test_instance::remove_barriers(step.env.level, pos, shape);
    }
    if !passed && runner.halt_on_error {
        end_environment(runner, step);
        unforce_all(step.env.level);
        tests.clear(step);
        return;
    }
    let Some(current) = runner.current else {
        return;
    };
    let done = runner.batches[current]
        .tests
        .iter()
        .all(|&t| runner.infos[t].done);
    if done {
        unforce_all(step.env.level);
        run_batch(tests, step, current + 1);
    }
}

/// Every forced chunk of the level stops being forced.
fn unforce_all(level: &mut Level) {
    let Level { chunks, forced, .. } = level;
    for pos in forced.sorted() {
        forced.set(chunks, pos, false);
    }
}

// --- `/test` ---------------------------------------------------------------------------------

/// `TestCommand.Info`: the source player's position and the surface there.
fn player_and_surface(
    env: &Env<'_>,
    source: &CommandSource,
) -> (Option<(Vec3, f32, f32)>, BlockPos, i32) {
    let player = source
        .uuid()
        .and_then(|u| {
            super::select::online_players(&env.level.entities, env.global, env.world)
                .into_iter()
                .find(|t| t.uuid == u)
        })
        .and_then(|t| t.place);
    let at = player.map_or(source.pos, |(p, _, _)| p);
    let pos = BlockPos::containing(at.x, at.y, at.z);
    let surface = env.level.heightmap_y(
        vanillite_chunk::HeightmapKind::WorldSurface,
        BlockPos::new(pos.x, 384, pos.z),
    );
    (player, pos, surface)
}

/// `createTestPositionAround`.
fn test_position_around(env: &Env<'_>, source: &CommandSource) -> BlockPos {
    let (_, pos, surface) = player_and_surface(env, source);
    BlockPos::new(pos.x, surface, pos.z + 3)
}

fn dimension_name(global: &Global, world: WorldId) -> &str {
    global.dimensions.name_or_overworld(world)
}

/// `isPlayerInCurrentTestDimension`.
fn player_in_test_dimension(source: &CommandSource, dimensions: &[WorldId]) -> bool {
    source.uuid().is_some()
        && dimensions.len() <= 1
        && dimensions.contains(&source.world)
        && dimensions.contains(&WorldId(0))
}

/// `outputPlayerCoordinates`.
fn output_player_coordinates(source: &CommandSource, env: &Env<'_>, dimensions: &[WorldId]) {
    if player_in_test_dimension(source, dimensions) {
        return;
    }
    let (player, pos, _) = player_and_surface(env, source);
    let Some((_, y_rot, x_rot)) = player else {
        return;
    };
    let dimension = dimension_name(env.global, source.world);
    let short = dimension.split_once(':').map_or(dimension, |(_, p)| p);
    #[allow(clippy::cast_possible_truncation, clippy::as_conversions)]
    let (yr, xr) = (y_rot.floor() as i32, x_rot.floor() as i32);
    let text = Component::text("[")
        .append(Component::translatable_args(
            "test.player.coordinates",
            vec![
                Arg::Int(pos.x),
                Arg::Int(pos.y),
                Arg::Int(pos.z),
                Arg::String(short.to_owned()),
            ],
        ))
        .append(Component::text("]"))
        .color(Color::Named(NamedColor::Gold))
        .click(ClickEvent::SuggestCommand(format!(
            "/execute in {dimension} run tp @s {} {} {} {yr} {xr}",
            pos.x, pos.y, pos.z
        )))
        .hover(HoverEvent::ShowText(Component::translatable(
            "chat.coordinates.tooltip",
            vec![],
        )));
    source.send_success(env.global, text, false);
}

/// `outputTestCoordinates`: only tests of the source's world are located here.
fn output_test_coordinates(source: &CommandSource, env: &Env<'_>, dimensions: &[WorldId]) {
    for &world in dimensions {
        if player_in_test_dimension(source, dimensions) {
            return;
        }
        let (_, pos, surface) = player_and_surface(env, source);
        let dimension = dimension_name(env.global, world);
        let short = dimension.split_once(':').map_or(dimension, |(_, p)| p);
        let text = Component::text("[")
            .append(Component::translatable_args(
                "test.run.coordinates",
                vec![
                    Arg::Int(pos.x),
                    Arg::Int(surface),
                    Arg::Int(pos.z),
                    Arg::String(short.to_owned()),
                ],
            ))
            .append(Component::text("]"))
            .color(Color::Named(NamedColor::Yellow))
            .click(ClickEvent::SuggestCommand(format!(
                "/execute in {dimension} run tp @s {} {surface} {} 0 0",
                pos.x, pos.z
            )))
            .hover(HoverEvent::ShowText(Component::translatable(
                "chat.coordinates.tooltip",
                vec![],
            )));
        source.send_success(env.global, text, false);
    }
}

/// `StructureUtils.findTestBlocks`: test instance blocks of the live chunks within `radius`,
/// in chunk order (`ChunkPos.rangeClosed`), then by height.
fn find_test_blocks(level: &Level, center: BlockPos, radius: i32) -> Vec<BlockPos> {
    let r = radius.div_euclid(16) + 1;
    let (cx, cz) = (center.x >> 4, center.z >> 4);
    let r2 = i64::from(radius) * i64::from(radius);
    let mut out = Vec::new();
    for z in cz - r..=cz + r {
        for x in cx - r..=cx + r {
            let mut found: Vec<BlockPos> = level
                .block_entities
                .in_chunk(vanillite_base::ChunkPos::new(x, z))
                .filter(|(_, e)| matches!(e, BlockEntity::TestInstance(_)))
                .map(|(p, _)| p)
                .filter(|p| {
                    (p.x - center.x).abs() <= radius
                        && (p.z - center.z).abs() <= radius
                        && dist_sqr(*p, center) <= r2
                })
                .collect();
            found.sort_by_key(|p| (p.y, p.z, p.x));
            out.extend(found);
        }
    }
    out
}

fn dist_sqr(a: BlockPos, b: BlockPos) -> i64 {
    let d = |a: i32, b: i32| i64::from(a - b) * i64::from(a - b);
    d(a.x, b.x) + d(a.y, b.y) + d(a.z, b.z)
}

/// `findNearestTest`.
fn find_nearest(level: &Level, center: BlockPos, radius: i32) -> Option<BlockPos> {
    let manhattan =
        |p: &BlockPos| (p.x - center.x).abs() + (p.y - center.y).abs() + (p.z - center.z).abs();
    find_test_blocks(level, center, radius)
        .into_iter()
        .min_by_key(manhattan)
}

/// `AABB.clip(from, to)`: whether the segment hits the box.
fn clips(b: &Aabb, from: Vec3, to: Vec3) -> bool {
    let d = [to.x - from.x, to.y - from.y, to.z - from.z];
    let o = [from.x, from.y, from.z];
    let lo = [b.min_x, b.min_y, b.min_z];
    let hi = [b.max_x, b.max_y, b.max_z];
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for axis in 0..3 {
        if d[axis].abs() < 1.0e-12 {
            if o[axis] < lo[axis] || o[axis] > hi[axis] {
                return false;
            }
            continue;
        }
        let a = (lo[axis] - o[axis]) / d[axis];
        let c = (hi[axis] - o[axis]) / d[axis];
        t0 = t0.max(a.min(c));
        t1 = t1.min(a.max(c));
        if t0 > t1 {
            return false;
        }
    }
    true
}

/// The eye position and look end `distance` away of the source player.
fn eye_ray(env: &Env<'_>, source: &CommandSource, distance: f64) -> Option<(Vec3, Vec3)> {
    let (player, _, _) = player_and_surface(env, source);
    let (pos, y_rot, x_rot) = player?;
    let eye = Vec3::new(pos.x, pos.y + 1.62, pos.z);
    let f = f64::from(x_rot).to_radians();
    let g = -f64::from(y_rot).to_radians();
    let look = Vec3::new(g.sin() * f.cos(), -f.sin(), g.cos() * f.cos());
    let end = Vec3::new(
        eye.x + look.x * distance,
        eye.y + look.y * distance,
        eye.z + look.z * distance,
    );
    Some((eye, end))
}

/// `lookedAtTestPos`.
fn looked_at(
    registry: &Registry,
    env: &Env<'_>,
    source: &CommandSource,
) -> Result<Vec<BlockPos>, CommandError> {
    let (start, end) = eye_ray(env, source, 250.0)
        .ok_or_else(|| CommandError::new("permissions.requires.player", vec![]))?;
    let center = BlockPos::containing(source.pos.x, source.pos.y, source.pos.z);
    let mut hits: Vec<BlockPos> = find_test_blocks(env.level, center, 250)
        .into_iter()
        .filter(|&p| {
            test_instance::get(env.level, p).is_some_and(|t| {
                let shape = shape_of(registry, env.level, p);
                clips(&t.structure_box(p, shape).aabb(), start, end)
            })
        })
        .collect();
    hits.sort_by_key(|p| dist_sqr(*p, center));
    hits.truncate(1);
    Ok(hits)
}

/// How `/test` finds its tests (`TestFinder`).
pub(crate) enum Finder {
    /// `byResourceSelection` of a pattern, `amount` copies.
    Selection { pattern: String, copies: usize },
    /// `allNearby`.
    AllNearby,
    /// `nearest`.
    Nearest,
    /// `lookedAt`.
    LookedAt,
    /// `radius`.
    Radius(i32),
    /// `failedTests(onlyRequired)`.
    Failed { only_required: bool },
}

/// `ResourceSelectorArgument`: `*` any run, `?` one character.
fn wildcard(text: &str, pattern: &str) -> bool {
    let t: Vec<char> = text.chars().collect();
    let p: Vec<char> = pattern.chars().collect();
    let (mut ti, mut pi) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            ti += 1;
            pi += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

impl Finder {
    /// `findTests`.
    fn tests(&self, tests: &GameTests, registry: &Registry) -> Vec<Arc<TestDef>> {
        match self {
            Finder::Selection { pattern, copies } => {
                let mut ids: Vec<&Arc<TestDef>> = registry
                    .tests
                    .iter()
                    .filter(|t| wildcard(&t.id, pattern))
                    .collect();
                ids.sort_by(|a, b| a.id.cmp(&b.id));
                let one: Vec<Arc<TestDef>> = ids.into_iter().cloned().collect();
                let mut out = Vec::new();
                for _ in 0..*copies {
                    out.extend(one.iter().cloned());
                }
                out
            }
            Finder::Failed { only_required } => tests
                .failed
                .iter()
                .filter(|t| !only_required || t.required)
                .cloned()
                .collect(),
            _ => Vec::new(),
        }
    }

    /// `findTestPos` in the source's world.
    fn positions(
        &self,
        registry: &Registry,
        env: &Env<'_>,
        source: &CommandSource,
    ) -> Result<Vec<BlockPos>, CommandError> {
        let center = BlockPos::containing(source.pos.x, source.pos.y, source.pos.z);
        Ok(match self {
            Finder::AllNearby => find_test_blocks(env.level, center, FULL_RADIUS),
            Finder::Nearest => find_nearest(env.level, center, NEARBY_RADIUS)
                .into_iter()
                .collect(),
            Finder::LookedAt => looked_at(registry, env, source)?,
            Finder::Radius(r) => find_test_blocks(env.level, center, *r),
            Finder::Selection { .. } | Finder::Failed { .. } => Vec::new(),
        })
    }
}

/// `verifyStructureExists`.
fn structure_exists(source: &CommandSource, env: &Env<'_>, structure: &str) -> bool {
    if env.level.structure_template(structure).is_some() {
        return true;
    }
    source.send_failure(
        env.global,
        Component::translatable_args(
            "commands.test.error.structure_not_found",
            vec![Arg::String(structure.to_owned())],
        ),
    );
    false
}

/// `toGameTestInfos`: the tests of test instance blocks.
fn infos_at(
    registry: &Registry,
    env: &Env<'_>,
    source: &CommandSource,
    positions: &[BlockPos],
    retry: Retry,
) -> Vec<Info> {
    let mut out = Vec::new();
    for &pos in positions {
        let Some(block) = test_instance::get(env.level, pos) else {
            source.send_failure(
                env.global,
                Component::translatable_args(
                    "commands.test.error.test_instance_not_found.position",
                    coords(pos),
                ),
            );
            continue;
        };
        let Some(test) = block.data.test.as_deref().and_then(|id| registry.get(id)) else {
            source.send_failure(
                env.global,
                Component::translatable_args(
                    "commands.test.error.non_existant_test",
                    vec![Arg::Component(Box::new(block.test_name()))],
                ),
            );
            continue;
        };
        let rotation = block.rotation(test.shape());
        let mut info = Info::new(Arc::clone(test), env.world, rotation, retry);
        info.pos = Some(pos);
        if structure_exists(source, env, &test.structure) {
            out.push(info);
        }
    }
    out
}

/// `toGameTestInfo`: new tests of the registry.
fn infos_of(
    env: &Env<'_>,
    source: &CommandSource,
    tests: Vec<Arc<TestDef>>,
    retry: Retry,
    rotation: Rotation,
) -> Vec<Info> {
    let mut out = Vec::new();
    for test in tests {
        if !structure_exists(source, env, &test.structure) {
            continue;
        }
        let Some(world) = test.world(env.global) else {
            source.send_failure(
                env.global,
                Component::text(format!(
                    "Could not resolve level for dimension: {}",
                    test.dimension
                )),
            );
            continue;
        };
        out.push(Info::new(test, world, rotation, retry));
    }
    out
}

/// `StructureUtils.getRotationForRotationSteps`.
fn rotation_steps(steps: i32) -> Result<Rotation, CommandError> {
    usize::try_from(steps)
        .ok()
        .and_then(|s| Rotation::ALL.get(s).copied())
        .ok_or_else(|| {
            CommandError::new(
                "command.failed",
                vec![ErrArg::Str(format!(
                    "rotationSteps must be a value from 0-3. Got value {steps}"
                ))],
            )
        })
}

fn grid(env: &Env<'_>, source: &CommandSource, tests_per_row: i32, clear_on_batch: bool) -> Grid {
    let mut first = BTreeMap::new();
    first.insert(env.world.0, test_position_around(env, source));
    Grid {
        tests_per_row,
        clear_on_batch,
        first,
        grids: BTreeMap::new(),
    }
}

/// `TestCommand.run`.
pub(crate) fn run(
    env: &mut Env<'_>,
    source: &CommandSource,
    finder: &Finder,
    retry: Retry,
    rotation_steps_n: i32,
    tests_per_row: i32,
) -> Result<i32, CommandError> {
    with_tests(env, |tests, step| {
        tests.clear(step);
        let registry = tests.registry(step.env.global);
        let positions = finder.positions(&registry, step.env, source)?;
        let mut infos = infos_at(&registry, step.env, source, &positions, retry);
        let selected = finder.tests(tests, &registry);
        let rotation = if selected.is_empty() {
            Rotation::None
        } else {
            rotation_steps(rotation_steps_n)?
        };
        infos.extend(infos_of(step.env, source, selected, retry, rotation));
        let global = step.env.global;
        if infos.is_empty() {
            source.send_success(
                global,
                Component::translatable("commands.test.no_tests", vec![]),
                false,
            );
            return Ok(0);
        }
        tests.failed.clear();
        source.send_success(
            global,
            Component::translatable_args(
                "commands.test.run.running",
                vec![Arg::Int(i32::try_from(infos.len()).unwrap_or(i32::MAX))],
            ),
            false,
        );
        let runner = Runner::from_infos(
            infos,
            MAX_TESTS_PER_BATCH,
            Spawner::InPlace,
            Spawner::Grid(grid(step.env, source, tests_per_row, false)),
            false,
            false,
            source.clone(),
        );
        Ok(track_and_start(tests, step, runner))
    })
}

/// `TestCommand.verify`: every test 100 times in each rotation.
pub(crate) fn verify(
    env: &mut Env<'_>,
    source: &CommandSource,
    finder: &Finder,
) -> Result<i32, CommandError> {
    with_tests(env, |tests, step| {
        tests.clear(step);
        let registry = tests.registry(step.env.global);
        let positions = finder.positions(&registry, step.env, source)?;
        let mut base = infos_at(&registry, step.env, source, &positions, Retry::NONE);
        let selected = finder.tests(tests, &registry);
        base.extend(infos_of(
            step.env,
            source,
            selected,
            Retry::NONE,
            Rotation::None,
        ));
        tests.failed.clear();
        let mut infos = Vec::new();
        let mut batches = Vec::new();
        for info in &base {
            for (index, rotation) in Rotation::ALL.into_iter().enumerate() {
                let mut list = Vec::new();
                for _ in 0..100 {
                    let mut copy = Info::new(
                        Arc::clone(&info.test),
                        info.world,
                        rotation,
                        Retry::new(1, true),
                    );
                    copy.pos = info.pos;
                    infos.push(copy);
                    list.push(infos.len() - 1);
                }
                batches.push(Batch {
                    index,
                    tests: list,
                    environment: info.test.environment.clone(),
                    world: info.world,
                });
            }
        }
        let mut runner = Runner::from_infos(
            infos,
            100,
            Spawner::SameAsNew,
            Spawner::Grid(grid(step.env, source, 10, true)),
            true,
            true,
            source.clone(),
        );
        runner.batches = batches;
        Ok(track_and_start(tests, step, runner))
    })
}

/// `TestCommand.locate`.
pub(crate) fn locate(
    env: &mut Env<'_>,
    source: &CommandSource,
    finder: &Finder,
) -> Result<i32, CommandError> {
    let global = env.global;
    source.send_system(
        global,
        Component::translatable("commands.test.locate.started", vec![]),
    );
    let registry = with_tests(env, |tests, step| tests.registry(step.env.global));
    let selected: Vec<String> = with_tests(env, |tests, _| {
        finder
            .tests(tests, &registry)
            .into_iter()
            .map(|t| t.id.clone())
            .collect()
    });
    let mut found = finder.positions(&registry, env, source)?;
    if !selected.is_empty() {
        let center = BlockPos::containing(source.pos.x, source.pos.y, source.pos.z);
        for p in find_test_blocks(env.level, center, FULL_RADIUS) {
            if test_instance::get(env.level, p)
                .and_then(|t| t.data.test.as_deref())
                .is_some_and(|id| selected.iter().any(|s| s == id))
                && !found.contains(&p)
            {
                found.push(p);
            }
        }
    }
    let source_pos = BlockPos::containing(source.pos.x, source.pos.y, source.pos.z);
    let dimension = dimension_name(env.global, env.world);
    let mut count = 0;
    for p in found {
        let Some(block) = test_instance::get(env.level, p) else {
            continue;
        };
        let shape = shape_of(&registry, env.level, p);
        let facing = block
            .rotation(shape)
            .rotate(vanillite_base::Direction::North);
        let tp = p.relative_by(facing, 3);
        #[allow(clippy::cast_possible_truncation, clippy::as_conversions)]
        let yaw = facing_yaw(facing.opposite()) as i32;
        let command = format!(
            "/execute in {dimension} run tp @s {} {} {} {yaw} 0",
            tp.x, tp.y, tp.z
        );
        let dx = i64::from(source_pos.x - p.x);
        let dz = i64::from(source_pos.z - p.z);
        #[allow(
            clippy::cast_possible_truncation,
            clippy::as_conversions,
            clippy::cast_precision_loss
        )]
        let distance = ((dx * dx + dz * dz) as f32).sqrt().floor() as i32;
        let path = dimension.split_once(':').map_or(dimension, |(_, p)| p);
        let coordinates = Component::text("[")
            .append(
                Component::translatable_args("chat.coordinates", coords(p))
                    .append(Component::text(format!(" ->[{path}]"))),
            )
            .append(Component::text("]"))
            .color(Color::Named(NamedColor::Green))
            .click(ClickEvent::SuggestCommand(command))
            .hover(HoverEvent::ShowText(Component::translatable(
                "chat.coordinates.tooltip",
                vec![],
            )));
        source.send_success(
            global,
            Component::translatable_args(
                "commands.test.locate.found",
                vec![
                    Arg::Component(Box::new(coordinates)),
                    Arg::String(distance.to_string()),
                ],
            ),
            false,
        );
        count += 1;
    }
    if count == 0 {
        return Err(CommandError::new(
            "commands.test.error.no_test_instances",
            vec![],
        ));
    }
    output_player_coordinates(source, env, &[env.world]);
    source.send_success(
        global,
        Component::translatable_args("commands.test.locate.done", vec![Arg::Int(count)]),
        true,
    );
    Ok(count)
}

/// `Direction.toYRot`.
fn facing_yaw(d: vanillite_base::Direction) -> f32 {
    match d {
        vanillite_base::Direction::South => 0.0,
        vanillite_base::Direction::West => 90.0,
        vanillite_base::Direction::North => 180.0,
        vanillite_base::Direction::East => 270.0,
        _ => 0.0,
    }
}

trait RelativeBy {
    fn relative_by(self, d: vanillite_base::Direction, n: i32) -> BlockPos;
}

impl RelativeBy for BlockPos {
    fn relative_by(self, d: vanillite_base::Direction, n: i32) -> BlockPos {
        let mut p = self;
        for _ in 0..n {
            p = p.relative(d);
        }
        p
    }
}

/// `TestCommand.reset`.
pub(crate) fn reset(
    env: &mut Env<'_>,
    source: &CommandSource,
    finder: &Finder,
) -> Result<i32, CommandError> {
    with_tests(env, |tests, step| {
        tests.clear(step);
        let registry = tests.registry(step.env.global);
        let positions = finder.positions(&registry, step.env, source)?;
        let infos = infos_at(&registry, step.env, source, &positions, Retry::NONE);
        let global = step.env.global;
        let mut feedback = |m: Component| source.send_system(global, m);
        for info in &infos {
            if let Some(pos) = info.pos {
                reset_test(&registry, step.env.level, pos, &mut feedback);
            }
        }
        if infos.is_empty() {
            return Err(CommandError::new(
                "commands.test.clear.error.no_tests",
                vec![],
            ));
        }
        let n = i32::try_from(infos.len()).unwrap_or(i32::MAX);
        source.send_success(
            global,
            Component::translatable_args("commands.test.reset.success", vec![Arg::Int(n)]),
            true,
        );
        Ok(n)
    })
}

/// `TestCommand.clear`.
pub(crate) fn clear(
    env: &mut Env<'_>,
    source: &CommandSource,
    finder: &Finder,
) -> Result<i32, CommandError> {
    with_tests(env, |tests, step| {
        tests.clear(step);
        let registry = tests.registry(step.env.global);
        let positions = finder.positions(&registry, step.env, source)?;
        let level = &mut *step.env.level;
        let mut n = 0;
        for pos in positions {
            let shape = shape_of(&registry, level, pos);
            let Some(block) = test_instance::get(level, pos) else {
                continue;
            };
            let area = block.test_box(pos, shape);
            test_instance::clear_space(level, area);
            test_instance::remove_barriers(level, pos, shape);
            level.destroy_block(pos, false, None);
            n += 1;
        }
        if n == 0 {
            return Err(CommandError::new(
                "commands.test.clear.error.no_tests",
                vec![],
            ));
        }
        source.send_success(
            step.env.global,
            Component::translatable_args("commands.test.clear.success", vec![Arg::Int(n)]),
            true,
        );
        Ok(n)
    })
}

/// `TestCommand.stopTests`.
pub(crate) fn stop(env: &mut Env<'_>) -> i32 {
    with_tests(env, |tests, step| tests.clear(step));
    1
}

/// `TestCommand.createNewStructure`.
pub(crate) fn create(
    env: &mut Env<'_>,
    source: &CommandSource,
    id: &str,
    size: [i32; 3],
) -> Result<i32, CommandError> {
    if size.iter().any(|s| *s > 48) {
        return Err(CommandError::new(
            "commands.test.error.too_large",
            vec![ErrArg::Int(48)],
        ));
    }
    let pos = test_position_around(env, source);
    let level = &mut *env.level;
    // `StructureUtils.createNewEmptyTest`.
    let mut probe = TestInstance::default();
    probe.data.size = size;
    let area = probe.structure_box(pos, TestShape::default());
    test_instance::clear_space(level, area);
    level.set_block(pos, ids::TEST_INSTANCE_BLOCK.default_state(), update::ALL);
    test_instance::set(
        level,
        pos,
        Data {
            test: Some(id.to_owned()),
            size,
            rotation: Rotation::None,
            ignore_entities: false,
            status: Status::Cleared,
            error_message: None,
        },
    );
    let low = probe.structure_pos(pos, TestShape::default());
    for x in 0..size[0] {
        for z in 0..size[2] {
            level.set_block(
                BlockPos::new(low.x + x, low.y, low.z + z),
                ids::BEDROCK.default_state(),
                update::ALL,
            );
        }
    }
    source.send_success(
        env.global,
        Component::translatable_args(
            "commands.test.create.success",
            vec![Arg::String(id.to_owned())],
        ),
        true,
    );
    Ok(1)
}

/// `TestCommand.showPos`.
pub(crate) fn pos(
    env: &mut Env<'_>,
    source: &CommandSource,
    var: &str,
) -> Result<i32, CommandError> {
    let (start, end) = eye_ray(env, source, 10.0)
        .ok_or_else(|| CommandError::new("permissions.requires.player", vec![]))?;
    let target = vanillite_game::mob::physics::clip(&*env.level, start, end)
        .map_or_else(|| BlockPos::containing(end.x, end.y, end.z), |(p, _)| p);
    let registry = with_tests(env, |tests, step| tests.registry(step.env.global));
    let containing = |radius: i32| {
        find_test_blocks(env.level, target, radius)
            .into_iter()
            .find(|&p| {
                test_instance::get(env.level, p).is_some_and(|t| {
                    t.structure_box(p, shape_of(&registry, env.level, p))
                        .contains(target)
                })
            })
    };
    let Some(test_pos) = containing(NEARBY_RADIUS).or_else(|| containing(FULL_RADIUS)) else {
        return Err(CommandError::new(
            "commands.test.error.no_test_containing_pos",
            vec![
                ErrArg::Int(target.x),
                ErrArg::Int(target.y),
                ErrArg::Int(target.z),
            ],
        ));
    };
    let Some(block) = test_instance::get(env.level, test_pos) else {
        return Err(CommandError::new(
            "commands.test.error.test_instance_not_found",
            vec![],
        ));
    };
    let origin = block.structure_pos(test_pos, shape_of(&registry, env.level, test_pos));
    let rel = BlockPos::new(
        target.x - origin.x,
        target.y - origin.y,
        target.z - origin.z,
    );
    let description = format!("{}, {}, {}", rel.x, rel.y, rel.z);
    let name = block.test_name().plain();
    let coordinates = Component::translatable_args(
        "commands.test.coordinates",
        vec![Arg::Int(rel.x), Arg::Int(rel.y), Arg::Int(rel.z)],
    )
    .bold(true)
    .color(Color::Named(NamedColor::Green))
    .hover(HoverEvent::ShowText(Component::translatable(
        "commands.test.coordinates.copy",
        vec![],
    )))
    .click(ClickEvent::CopyToClipboard(format!(
        "final BlockPos {var} = new BlockPos({description});"
    )));
    source.send_success(
        env.global,
        Component::translatable_args(
            "commands.test.relative_position",
            vec![Arg::String(name), Arg::Component(Box::new(coordinates))],
        ),
        false,
    );
    if let Some(uuid) = source.uuid()
        && let Some((sink, _)) = env.global.players.connection(Uuid(uuid))
    {
        let wire = |p: BlockPos| vanillite_protocol::types::BlockPos {
            x: p.x,
            y: p.y,
            z: p.z,
        };
        sink.send(&GameTestHighlightPos {
            absolute: wire(target),
            relative: wire(rel),
        });
    }
    Ok(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_instance_registry_entries_parse() {
        let v: Value = serde_json::from_str(
            r#"{"type":"minecraft:function","environment":"minecraft:default",
            "function":"minecraft:always_pass","max_ticks":1,"required":false,
            "setup_ticks":1,"structure":"minecraft:empty"}"#,
        )
        .expect("json");
        let t = parse_test("minecraft:always_pass", &v).expect("parses");
        assert_eq!(t.function.as_deref(), Some("minecraft:always_pass"));
        assert_eq!(t.environment, EnvRef::Id("minecraft:default".into()));
        assert_eq!((t.max_ticks, t.setup_ticks, t.required), (1, 1, false));
        assert_eq!(t.dimension, "minecraft:overworld");
        let v: Value =
            serde_json::from_str(r#"{"type":"minecraft:all_of","definitions":[]}"#).expect("json");
        assert_eq!(parse_env(&v), Some(EnvDef::AllOf(Vec::new())));
        let v: Value = serde_json::from_str(
            r#"{"type":"block_based","environment":{"type":"weather","weather":"rain"},
            "structure":"a:b","max_ticks":0}"#,
        )
        .expect("json");
        assert_eq!(parse_test("a:b", &v), None);
    }

    #[test]
    fn retry_options_like_vanilla() {
        assert!(!Retry::NONE.has_retries());
        assert!(!Retry::NONE.has_tries_left(1, 1));
        let until_failed = Retry::new(0, true);
        assert!(until_failed.has_tries_left(5, 5));
        assert!(!until_failed.has_tries_left(5, 4));
        assert!(Retry::new(3, false).has_tries_left(2, 0));
        assert!(wildcard("minecraft:a/two", "minecraft:a/*"));
        assert!(wildcard("minecraft:a/two", "*:a/t?o"));
        assert!(!wildcard("minecraft:b/two", "minecraft:a/*"));
    }
}

//! Vanillite's own commands (#250): `vanillite <sub>` (`vl`, also `vanillite:`): `version`,
//! `status`, `tps`, `mspt`, `profile`, `chunks`, `entities`, `memory`, `reload config` and
//! `debug`. They cost nothing while nobody runs them: everything is read when asked, and what
//! needs every world runs on the worlds between their ticks and answers a moment later.
//!
//! Texts are translatable (`vanillite.command.*`) with their English text as the fallback.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use vanillite_base::ChunkPos;
use vanillite_command::error::CommandError;
use vanillite_command::{Arg as ErrArg, Parsed, Requirement, Source as _};
use vanillite_data::entity::EntityTypeId;
use vanillite_game::commands::{CommandId, VanilliteOp};
use vanillite_protocol::text::{Arg, ClickEvent, Color, Component, HoverEvent, NamedColor};

use super::Env;
use super::plugins_cmd::{AUTHOR, LICENSE, WEBSITE};
use super::source::CommandSource;
use crate::diagnostics::{self, Detail, WorldSnapshot};
use crate::global::Global;
use crate::player_list::Uuid;
use crate::profiler::{Finished, Report, Row, WorldProfile};
use crate::server::Server;
use crate::tick_rate::TickWindow;
use crate::world::WorldId;

/// The windows of `tps`, `mspt` and `status`: label and ticks (at 20 ticks per second).
const WINDOWS: [(&str, usize); 3] = [("5s", 100), ("1m", 1200), ("5m", 6000)];
/// Default length of `profile`.
const PROFILE_SECONDS: i32 = 10;
/// Entity types `entities` lists.
const TYPES_SHOWN: usize = 15;

/// A text of these commands: key `vanillite.command.<key>`, English fallback.
fn t(key: &str, fallback: &str, args: Vec<Arg>) -> Component {
    Component::translatable_args(format!("vanillite.command.{key}"), args).fallback(fallback)
}

fn named(text: impl Into<String>, color: NamedColor) -> Component {
    Component::text(text).color(Color::Named(color))
}

fn arg(c: Component) -> Arg {
    Arg::Component(Box::new(c))
}

fn plain(text: impl Into<String>) -> Arg {
    Arg::String(text.into())
}

fn value(text: impl Into<String>) -> Arg {
    arg(named(text, NamedColor::Aqua))
}

fn header(c: Component) -> Component {
    c.color(Color::Named(NamedColor::Gold))
}

/// A failure with a text of these commands (the client knows no `vanillite.*` key, `%s`
/// passes the translatable text with its fallback on).
fn err(key: &str, fallback: &str) -> CommandError {
    CommandError::new(
        "%s",
        vec![ErrArg::Component(Box::new(t(key, fallback, vec![])))],
    )
}

/// Runs a `vanillite` subcommand.
pub(crate) fn run(
    op: VanilliteOp,
    parsed: &Parsed<CommandId>,
    source: &CommandSource,
    env: &mut Env<'_>,
) -> Result<i32, CommandError> {
    let global = env.global;
    match op {
        VanilliteOp::Help => {
            for line in help_lines(source) {
                source.send_system(global, line);
            }
            Ok(1)
        }
        VanilliteOp::Version => {
            source.send_system(global, version_line(&global.version));
            Ok(1)
        }
        VanilliteOp::Tps => {
            let (windows, rate) = windows(global);
            source.send_system(global, tps_line(&windows, rate));
            Ok(windows.first().map_or(0, |w| w.tps.round() as i32))
        }
        VanilliteOp::Mspt => {
            let (windows, rate) = windows(global);
            for line in mspt_lines(&windows, rate) {
                source.send_system(global, line);
            }
            Ok(windows.first().map_or(0, |w| w.mean.round() as i32))
        }
        VanilliteOp::Status => on_server(global, source, Detail::Counts, None, status),
        VanilliteOp::Memory => on_server(global, source, Detail::Memory, None, memory),
        VanilliteOp::Chunks => {
            let world = world_arg(parsed, global)?;
            on_server(global, source, Detail::Chunks, world, chunks)
        }
        VanilliteOp::Entities => {
            let world = world_arg(parsed, global)?;
            let only = match parsed.args.identifier("type") {
                Some(id) => Some(EntityTypeId::by_name(id).ok_or_else(|| {
                    CommandError::new(
                        "argument.resource.not_found",
                        vec![
                            ErrArg::Str(id.clone()),
                            ErrArg::Str("minecraft:entity_type".into()),
                        ],
                    )
                })?),
                None => None,
            };
            on_server(global, source, Detail::Entities(only), world, entities)
        }
        VanilliteOp::Profile => profile(parsed, source, global),
        VanilliteOp::ReloadConfig => reload(source, global),
        VanilliteOp::Debug => debug(parsed, source, env),
    }
}

/// `vanillite`: the subcommands the source may use, each a click away.
fn help_lines(source: &CommandSource) -> Vec<Component> {
    let mut lines = vec![header(t("help", "Vanillite commands:", vec![]))];
    for (_, name, node, level) in VanilliteOp::SUBCOMMANDS {
        let requirement = Requirement {
            level,
            permission: Some(node),
            node_decides: true,
        };
        if !source.meets(&requirement) {
            continue;
        }
        let usage = match name {
            "profile" => " [<seconds>]",
            "chunks" => " [<world>]",
            "entities" => " [<world> [<type>]]",
            "reload" => " config",
            "debug" => " <player>",
            _ => "",
        };
        let command = format!("/vanillite {name}");
        lines.push(
            named(format!(" {command}{usage}"), NamedColor::Yellow)
                .click(ClickEvent::SuggestCommand(command))
                .hover(HoverEvent::ShowText(Component::text(node))),
        );
    }
    lines
}

/// `vanillite version`: `Vanillite 0.1.0 (Minecraft 26.3, abc1234) by … · … · …`.
fn version_line(version: &str) -> Component {
    let dot = || named(" · ", NamedColor::DarkGray);
    named(version, NamedColor::Green)
        .hover(HoverEvent::ShowText(Component::text(
            "Click to copy to clipboard",
        )))
        .click(ClickEvent::CopyToClipboard(version.to_owned()))
        .append(Component::text(" by "))
        .append(named(AUTHOR, NamedColor::Green))
        .append(dot())
        .append(
            named(WEBSITE, NamedColor::Aqua)
                .underlined(true)
                .click(ClickEvent::OpenUrl(WEBSITE.to_owned())),
        )
        .append(dot())
        .append(named(LICENSE, NamedColor::Green))
}

/// The tick windows and the target tick rate.
fn windows(global: &Global) -> (Vec<TickWindow>, f64) {
    let times = global
        .tick_times
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let windows = WINDOWS
        .iter()
        .map(|(_, ticks)| times.window(*ticks).unwrap_or(EMPTY))
        .collect();
    drop(times);
    (windows, f64::from(global.tick_rate().tick_rate()))
}

const EMPTY: TickWindow = TickWindow {
    ticks: 0,
    tps: 0.0,
    mean: 0.0,
    p50: 0.0,
    p95: 0.0,
    max: 0.0,
    min: 0.0,
};

/// A TPS value like Paper's: at most the tick rate (`*` when above it), coloured.
fn tps_value(tps: f64, rate: f64) -> Component {
    let shown = tps.min(rate);
    let text = format!("{}{shown:.2}", if tps > rate * 1.05 { "*" } else { "" });
    let color = if shown >= rate * 0.9 {
        NamedColor::Green
    } else if shown >= rate * 0.75 {
        NamedColor::Yellow
    } else {
        NamedColor::Red
    };
    named(text, color)
}

/// Milliseconds coloured by the share of the tick budget.
fn ms_value(ms: f64, rate: f64) -> Component {
    let budget = 1000.0 / rate.max(0.1);
    let color = if ms < budget * 0.6 {
        NamedColor::Green
    } else if ms <= budget {
        NamedColor::Yellow
    } else {
        NamedColor::Red
    };
    named(format!("{ms:.1}"), color)
}

fn tps_line(windows: &[TickWindow], rate: f64) -> Component {
    let mut args = Vec::new();
    for w in windows {
        args.push(arg(tps_value(w.tps, rate)));
    }
    t("tps", "TPS from last 5s, 1m, 5m: %s, %s, %s", args)
}

/// Paper's `mspt`: average, minimum and maximum per window.
fn mspt_lines(windows: &[TickWindow], rate: f64) -> Vec<Component> {
    let mut line = Component::text("◴ ").color(Color::Named(NamedColor::Gold));
    for (i, w) in windows.iter().enumerate() {
        if i > 0 {
            line = line.append(named(", ", NamedColor::Gray));
        }
        line = line
            .append(ms_value(w.mean, rate))
            .append(named("/", NamedColor::Gray))
            .append(ms_value(w.min, rate))
            .append(named("/", NamedColor::Gray))
            .append(ms_value(w.max, rate));
    }
    vec![
        header(t(
            "mspt",
            "Server tick times (avg/min/max) from last 5s, 1m, 5m:",
            vec![],
        )),
        line,
    ]
}

/// The world of the optional `world` argument.
fn world_arg(parsed: &Parsed<CommandId>, global: &Global) -> Result<Option<WorldId>, CommandError> {
    if parsed.args.identifier("world").is_none() {
        return Ok(None);
    }
    super::execute::dimension(global, &parsed.args, "world").map(Some)
}

/// What the server adds to the worlds' snapshots.
#[derive(Debug, Clone)]
struct ServerFacts {
    memory_limit: Option<u64>,
    cores: f64,
    plugins: usize,
}

/// Turns the worlds' snapshots into the lines of a command's answer.
type Answer = fn(&Global, &ServerFacts, &[WorldSnapshot], Detail, Option<&str>) -> Vec<Component>;

/// Runs on the server: takes a snapshot of every world (or of `only`), then `answer` sends
/// the lines to `source`.
fn on_server(
    global: &Global,
    source: &CommandSource,
    detail: Detail,
    only: Option<WorldId>,
    answer: Answer,
) -> Result<i32, CommandError> {
    let handle = global.handle.get().ok_or_else(not_running)?;
    let source = source.clone();
    handle.run_global(move |server: &mut Server| {
        let limits = *server.limits();
        let facts = ServerFacts {
            memory_limit: limits.memory,
            cores: limits.cpu,
            plugins: server.global().plugins.list().len(),
        };
        let only_name = only.and_then(|w| server.global().dimensions.name(w).map(str::to_owned));
        server.gather_worlds(
            move |w| (only.is_none_or(|o| o == w.id())).then(|| diagnostics::snapshot(w, detail)),
            move |snapshots| {
                let snapshots: Vec<WorldSnapshot> = snapshots.into_iter().flatten().collect();
                let global = &source.global;
                for line in answer(global, &facts, &snapshots, detail, only_name.as_deref()) {
                    source.send_system(global, line);
                }
            },
        );
    });
    Ok(1)
}

fn not_running() -> CommandError {
    err("not_running", "The server is not running")
}

/// A label and a value: `Label: value`.
fn field(key: &str, label: &str, value: Component) -> Component {
    t(key, label, vec![arg(value)]).color(Color::Named(NamedColor::Gray))
}

/// `vanillite status`.
fn status(
    global: &Global,
    facts: &ServerFacts,
    worlds: &[WorldSnapshot],
    _: Detail,
    _: Option<&str>,
) -> Vec<Component> {
    let (windows, rate) = windows(global);
    let process = diagnostics::process();
    let mut lines = vec![header(t("status", "Vanillite status", vec![]))];
    lines.push(field(
        "status.version",
        "Version: %s",
        named(global.version.clone(), NamedColor::White),
    ));
    lines.push(field(
        "status.uptime",
        "Uptime: %s",
        named(
            diagnostics::uptime(global.diagnostics.started.elapsed()),
            NamedColor::White,
        ),
    ));
    lines.push(tps_line(&windows, rate).color(Color::Named(NamedColor::Gray)));
    for ((label, _), w) in WINDOWS.iter().zip(&windows) {
        lines.push(
            t(
                "status.mspt",
                "MSPT %s: avg %s, p50 %s, p95 %s, max %s (%s ticks)",
                vec![
                    plain(*label),
                    arg(ms_value(w.mean, rate)),
                    arg(ms_value(w.p50, rate)),
                    arg(ms_value(w.p95, rate)),
                    arg(ms_value(w.max, rate)),
                    plain(w.ticks.to_string()),
                ],
            )
            .color(Color::Named(NamedColor::Gray)),
        );
    }
    let rss = process
        .rss
        .map_or_else(|| "?".to_owned(), diagnostics::bytes);
    let limit = facts
        .memory_limit
        .map_or_else(|| "none".to_owned(), diagnostics::bytes);
    lines.push(
        t(
            "status.memory",
            "Memory: %s resident (peak %s), limit %s",
            vec![
                value(rss),
                value(
                    process
                        .peak
                        .map_or_else(|| "?".to_owned(), diagnostics::bytes),
                ),
                value(limit),
            ],
        )
        .color(Color::Named(NamedColor::Gray)),
    );
    if let Some(cpu) = process.cpu_seconds {
        let (percent, over) = global.diagnostics.cpu_percent(cpu);
        lines.push(
            t(
                "status.cpu",
                "CPU: %s%% of one core over the last %s (%s cores available, %s threads)",
                vec![
                    value(format!("{percent:.1}")),
                    plain(diagnostics::uptime(over)),
                    plain(format!("{:.1}", facts.cores)),
                    plain(
                        process
                            .threads
                            .map_or_else(|| "?".to_owned(), |t| t.to_string()),
                    ),
                ],
            )
            .color(Color::Named(NamedColor::Gray)),
        );
    }
    lines.push(field(
        "status.players",
        "Players: %s",
        named(
            format!(
                "{} / {}",
                global.players.count(),
                global.players.max_players()
            ),
            NamedColor::Aqua,
        ),
    ));
    let mut totals = (0, 0, 0);
    for w in worlds {
        totals.0 += w.chunks;
        totals.1 += w.entities;
        totals.2 += w.block_entities;
        lines.push(
            t(
                "status.world",
                " %s: %s chunks (%s full), %s entities, %s block entities (%s ticking), %s players",
                vec![
                    arg(named(w.dimension.clone(), NamedColor::White)),
                    value(w.chunks.to_string()),
                    plain(w.full_chunks.to_string()),
                    value(w.entities.to_string()),
                    value(w.block_entities.to_string()),
                    plain(w.ticking_block_entities.to_string()),
                    plain(w.players.to_string()),
                ],
            )
            .color(Color::Named(NamedColor::Gray))
            .hover(HoverEvent::ShowText(Component::text(
                "Click for the top chunks",
            )))
            .click(ClickEvent::RunCommand(format!(
                "/vanillite chunks {}",
                w.dimension
            ))),
        );
    }
    lines.push(
        t(
            "status.total",
            "Total: %s chunks, %s entities, %s block entities, %s plugins",
            vec![
                value(totals.0.to_string()),
                value(totals.1.to_string()),
                value(totals.2.to_string()),
                value(facts.plugins.to_string()),
            ],
        )
        .color(Color::Named(NamedColor::Gray)),
    );
    lines
}

/// `vanillite memory`.
fn memory(
    _: &Global,
    facts: &ServerFacts,
    worlds: &[WorldSnapshot],
    _: Detail,
    _: Option<&str>,
) -> Vec<Component> {
    memory_impl(facts, worlds, diagnostics::process())
}

fn memory_impl(
    facts: &ServerFacts,
    worlds: &[WorldSnapshot],
    process: diagnostics::ProcessStats,
) -> Vec<Component> {
    let mut lines = vec![header(t("memory", "Vanillite memory", vec![]))];
    let rss = process.rss.unwrap_or(0);
    lines.push(
        t(
            "memory.process",
            "Process: %s resident (peak %s), limit %s",
            vec![
                value(diagnostics::bytes(rss)),
                value(
                    process
                        .peak
                        .map_or_else(|| "?".to_owned(), diagnostics::bytes),
                ),
                value(
                    facts
                        .memory_limit
                        .map_or_else(|| "none".to_owned(), diagnostics::bytes),
                ),
            ],
        )
        .color(Color::Named(NamedColor::Gray)),
    );
    let per_world = |f: fn(&diagnostics::WorldMemory) -> usize| -> Vec<(String, u64)> {
        worlds
            .iter()
            .map(|w| {
                (
                    w.dimension.clone(),
                    w.memory.as_ref().map_or(0, |m| f(m) as u64),
                )
            })
            .collect()
    };
    let parts = [
        (
            "memory.chunks",
            " Chunks (blocks, light, heightmaps): %s",
            per_world(|m| m.chunks),
        ),
        (
            "memory.entities",
            " Entities: %s",
            per_world(|m| m.entities),
        ),
        (
            "memory.block_entities",
            " Block entities: %s",
            per_world(|m| m.block_entities),
        ),
        (
            "memory.tick_history",
            " Tick history: %s",
            vec![(String::new(), (crate::tick_rate::HISTORY * 8) as u64)],
        ),
    ];
    let mut known = 0u64;
    for (key, label, per) in parts {
        let bytes: u64 = per.iter().map(|(_, b)| b).sum();
        known += bytes;
        let hover = per
            .iter()
            .filter(|(w, _)| !w.is_empty())
            .map(|(w, b)| format!("{w}: {}", diagnostics::bytes(*b)))
            .collect::<Vec<_>>()
            .join("\n");
        let mut line = t(key, label, vec![value(diagnostics::bytes(bytes))])
            .color(Color::Named(NamedColor::Gray));
        if !hover.is_empty() {
            line = line.hover(HoverEvent::ShowText(Component::text(hover)));
        }
        lines.push(line);
    }
    lines.push(
        t(
            "memory.other",
            " Other (code, caches, network buffers, allocator, %s plugins): %s",
            vec![
                plain(facts.plugins.to_string()),
                value(diagnostics::bytes(rss.saturating_sub(known))),
            ],
        )
        .color(Color::Named(NamedColor::Gray)),
    );
    lines
}

/// A chunk of a top list as a line: teleports there on click.
fn chunk_line(dimension: &str, pos: ChunkPos, text: Component, hover: String) -> Component {
    let (x, z) = (pos.x * 16 + 8, pos.z * 16 + 8);
    let short = dimension.strip_prefix("minecraft:").unwrap_or(dimension);
    let mut hover = hover;
    if !hover.is_empty() {
        hover.push('\n');
    }
    hover.push_str(&format!("{short}, blocks {x} {z}\nClick to teleport"));
    Component::text(format!(" {short} {} {}: ", pos.x, pos.z))
        .color(Color::Named(NamedColor::Yellow))
        .append(text)
        .hover(HoverEvent::ShowText(Component::text(hover)))
        .click(ClickEvent::RunCommand(format!(
            "/execute in {dimension} run tp @s {x} ~ {z}"
        )))
}

fn type_name(kind: EntityTypeId) -> &'static str {
    let name = kind.name();
    name.strip_prefix("minecraft:").unwrap_or(name)
}

fn where_text(only: Option<&str>) -> String {
    only.map_or_else(|| "all worlds".to_owned(), str::to_owned)
}

/// The top chunks of all snapshots, most first.
fn top_chunks(worlds: &[WorldSnapshot]) -> Vec<(&str, &diagnostics::ChunkCount)> {
    let mut all: Vec<(&str, &diagnostics::ChunkCount)> = worlds
        .iter()
        .flat_map(|w| w.top_chunks.iter().map(move |c| (w.dimension.as_str(), c)))
        .collect();
    all.sort_by_key(|(_, c)| std::cmp::Reverse(c.entities + c.block_entities));
    all.truncate(diagnostics::TOP);
    all
}

/// `vanillite chunks [<world>]`.
fn chunks(
    _: &Global,
    _: &ServerFacts,
    worlds: &[WorldSnapshot],
    _: Detail,
    only: Option<&str>,
) -> Vec<Component> {
    chunks_impl(worlds, only)
}

fn chunks_impl(worlds: &[WorldSnapshot], only: Option<&str>) -> Vec<Component> {
    let mut lines = vec![header(t(
        "chunks",
        "Top chunks in %s (entities, block entities):",
        vec![plain(where_text(only))],
    ))];
    for (dimension, c) in top_chunks(worlds) {
        let hover = c
            .types
            .iter()
            .map(|(k, n)| format!("{}: {n}", type_name(*k)))
            .collect::<Vec<_>>()
            .join("\n");
        lines.push(chunk_line(
            dimension,
            c.pos,
            t(
                "chunks.entry",
                "%s entities, %s block entities",
                vec![
                    value(c.entities.to_string()),
                    value(c.block_entities.to_string()),
                ],
            ),
            hover,
        ));
    }
    if lines.len() == 1 {
        lines.push(t("chunks.none", " No loaded chunks", vec![]));
    }
    lines
}

/// `vanillite entities [<world> [<type>]]`.
fn entities(
    _: &Global,
    _: &ServerFacts,
    worlds: &[WorldSnapshot],
    detail: Detail,
    only: Option<&str>,
) -> Vec<Component> {
    entities_impl(worlds, detail, only)
}

fn entities_impl(worlds: &[WorldSnapshot], detail: Detail, only: Option<&str>) -> Vec<Component> {
    let total: usize = worlds.iter().map(|w| w.entities).sum();
    let mut types: Vec<(EntityTypeId, usize)> = Vec::new();
    for w in worlds {
        for (kind, n) in &w.types {
            match types.iter_mut().find(|(k, _)| k == kind) {
                Some((_, count)) => *count += n,
                None => types.push((*kind, *n)),
            }
        }
    }
    types.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut lines = vec![header(t(
        "entities",
        "Entities in %s: %s",
        vec![plain(where_text(only)), plain(total.to_string())],
    ))];
    let mut line = Component::text(" ");
    for (i, (kind, n)) in types.iter().take(TYPES_SHOWN).enumerate() {
        if i > 0 {
            line = line.append(named(", ", NamedColor::Gray));
        }
        let world = only.unwrap_or("minecraft:overworld");
        line = line.append(
            named(format!("{} ", type_name(*kind)), NamedColor::Yellow)
                .append(named(n.to_string(), NamedColor::Aqua))
                .hover(HoverEvent::ShowText(Component::text(
                    "Click for the chunks with the most of them",
                )))
                .click(ClickEvent::RunCommand(format!(
                    "/vanillite entities {world} {}",
                    kind.name()
                ))),
        );
    }
    lines.push(line);
    if let Detail::Entities(Some(kind)) = detail {
        lines.push(header(t(
            "entities.chunks",
            "Chunks with the most %s:",
            vec![plain(type_name(kind))],
        )));
        for (dimension, c) in top_chunks(worlds) {
            lines.push(chunk_line(
                dimension,
                c.pos,
                named(c.entities.to_string(), NamedColor::Aqua),
                String::new(),
            ));
        }
    }
    lines
}

/// `vanillite profile [<seconds>]`.
fn profile(
    parsed: &Parsed<CommandId>,
    source: &CommandSource,
    global: &Global,
) -> Result<i32, CommandError> {
    let seconds = parsed
        .args
        .int("seconds")
        .copied()
        .unwrap_or(PROFILE_SECONDS);
    let seconds = u32::try_from(seconds).unwrap_or(1).max(1);
    if !global.diagnostics.profiler.start(source.clone(), seconds) {
        return Err(err("profile.running", "A profile is already running"));
    }
    source.send_system(
        global,
        t(
            "profile.started",
            "Profiling for %s seconds...",
            vec![value(seconds.to_string())],
        ),
    );
    Ok(i32::try_from(seconds).unwrap_or(i32::MAX))
}

/// The end of a profile: writes the report under `dir` and sends its summary.
pub(crate) fn profile_finished(
    global: &Global,
    dir: &Path,
    finished: &Finished,
    worlds: &[(String, WorldProfile)],
) {
    let report = crate::profiler::report(finished, worlds);
    let text = crate::profiler::report_text(&report, &global.version);
    let name = format!(
        "profile-{}.txt",
        crate::session_recording::utc_name(SystemTime::now())
    );
    let path = dir.join(&name);
    let written = std::fs::create_dir_all(dir).and_then(|()| std::fs::write(&path, text));
    if let Err(e) = &written {
        log::warn!("Cannot write the profile {}: {e}", path.display());
    }
    for line in profile_lines(&report, written.is_ok().then_some(path.as_path())) {
        finished.source.send_system(global, line);
    }
}

fn row_line(r: &Row) -> Component {
    let text = Component::text(format!(" {:>6.2} ms ", r.millis_per_tick))
        .color(Color::Named(NamedColor::Aqua))
        .append(named(format!("{:>5.1}% ", r.percent), NamedColor::Gray))
        .append(named(r.name.clone(), NamedColor::White));
    text.hover(HoverEvent::ShowText(Component::text(format!(
        "{:.3} ms per tick, {:.2}% of the tick time\n{} {}",
        r.millis_per_tick,
        r.percent,
        r.count,
        if r.chunk.is_some() {
            "entity ticks"
        } else {
            "ticks"
        },
    ))))
}

/// The chat summary of a profile.
fn profile_lines(report: &Report, file: Option<&Path>) -> Vec<Component> {
    let tps = if report.seconds > 0.0 {
        report.ticks as f64 / report.seconds
    } else {
        0.0
    };
    let mut lines = vec![header(t(
        "profile.done",
        "Profile of %s s: %s ticks, %s ms per tick, TPS %s",
        vec![
            plain(format!("{:.1}", report.seconds)),
            plain(report.ticks.to_string()),
            value(format!("{:.2}", report.mspt)),
            value(format!("{tps:.2}")),
        ],
    ))];
    lines.push(t("profile.phases", "Tick phases:", vec![]).color(Color::Named(NamedColor::Gold)));
    lines.extend(report.phases.iter().take(8).map(row_line));
    if !report.entity_types.is_empty() {
        lines.push(
            t("profile.entities", "Entities by type:", vec![])
                .color(Color::Named(NamedColor::Gold)),
        );
        lines.extend(report.entity_types.iter().take(5).map(row_line));
    }
    if !report.chunks.is_empty() {
        lines.push(
            t("profile.chunks", "Chunks by entity time:", vec![])
                .color(Color::Named(NamedColor::Gold)),
        );
        for r in report.chunks.iter().take(5) {
            match &r.chunk {
                Some((dimension, pos)) => lines.push(chunk_line(
                    dimension,
                    *pos,
                    named(
                        format!("{:.2} ms, {} entity ticks", r.millis_per_tick, r.count),
                        NamedColor::Aqua,
                    ),
                    String::new(),
                )),
                None => lines.push(row_line(r)),
            }
        }
    }
    if report.sync_loads.0 > 0 {
        lines.push(t(
            "profile.sync_loads",
            "Synchronous chunk loads blocked the tick %s times (%s ms)",
            vec![
                value(report.sync_loads.0.to_string()),
                value(report.sync_loads.1.as_millis().to_string()),
            ],
        ));
    }
    if let Some(path) = file {
        let shown = path.display().to_string();
        lines.push(
            t("profile.file", "Report: %s", vec![plain(shown.clone())])
                .color(Color::Named(NamedColor::Gray))
                .click(ClickEvent::CopyToClipboard(shown)),
        );
    }
    lines
}

/// `vanillite reload config`.
fn reload(source: &CommandSource, global: &Global) -> Result<i32, CommandError> {
    let handle = global.handle.get().ok_or_else(not_running)?;
    let source = source.clone();
    handle.run_global(move |server: &mut Server| {
        let lines = reload_lines(&server.reload_config_report());
        for line in lines {
            source.send_system(&source.global, line);
        }
    });
    Ok(1)
}

fn reload_lines(result: &Result<crate::server::ReloadReport, String>) -> Vec<Component> {
    let report = match result {
        Ok(report) => report,
        Err(e) => {
            return vec![
                t(
                    "reload.failed",
                    "Reloading vanillite.toml failed, the old values stay: %s",
                    vec![plain(e.clone())],
                )
                .color(Color::Named(NamedColor::Red)),
            ];
        }
    };
    let mut lines = vec![
        t(
            "reload.done",
            "Reloaded vanillite.toml: %s changes apply now, %s need a restart",
            vec![
                value(report.applied.len().to_string()),
                value(report.restart.len().to_string()),
            ],
        )
        .color(Color::Named(NamedColor::Green)),
    ];
    if !report.applied.is_empty() {
        lines.push(
            t(
                "reload.applied",
                " Applied: %s",
                vec![plain(report.applied.join(", "))],
            )
            .color(Color::Named(NamedColor::Gray)),
        );
    }
    if !report.restart.is_empty() {
        lines.push(
            t(
                "reload.restart",
                " After a restart: %s",
                vec![plain(report.restart.join(", "))],
            )
            .color(Color::Named(NamedColor::Yellow)),
        );
    }
    lines
}

/// What `debug` reads of the player in its world.
#[derive(Debug, Clone, Default)]
struct PlayerView {
    client_view: u8,
    view: i32,
    server_view: i32,
    language: String,
}

/// `vanillite debug <player>`: the facts at once, then the packet rates over one second.
fn debug(
    parsed: &Parsed<CommandId>,
    source: &CommandSource,
    env: &mut Env<'_>,
) -> Result<i32, CommandError> {
    let selector = parsed.args.selector("player").ok_or_else(|| {
        CommandError::new(
            "command.exception",
            vec![ErrArg::Str("missing argument player".into())],
        )
    })?;
    let target = super::select::single(env, source, selector)?;
    let global = env.global;
    let entry = global
        .players
        .snapshot()
        .into_iter()
        .find(|p| p.uuid == Uuid(target.uuid))
        .ok_or_else(|| CommandError::new("argument.entity.notfound.player", vec![]))?;
    let source = source.clone();
    let world = entry.world;
    global.run_in_world(world, move |w| {
        let view = w
            .level()
            .entities
            .player_by_uuid(entry.uuid.0)
            .and_then(|id| w.level().entities.player(id))
            .map(|p| PlayerView {
                client_view: p.client.view_distance,
                view: p.chunk_view.view_distance,
                server_view: w.players.view_distance(),
                language: p.client.language.clone(),
            })
            .unwrap_or_default();
        let dimension = w.dimension().to_owned();
        let global = Arc::clone(&source.global);
        for line in debug_lines(&entry, &view, &dimension) {
            source.send_system(&global, line);
        }
        let Some(details) = entry.details.clone() else {
            return;
        };
        let before = (details.packets_received(), details.packets_sent());
        // The rates over the next second, from the connection's counters.
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(1));
            let after = (details.packets_received(), details.packets_sent());
            source.send_system(
                &global,
                t(
                    "debug.packets",
                    "Packets: %s/s in, %s/s out (%s in, %s out since joining)",
                    vec![
                        value(after.0.saturating_sub(before.0).to_string()),
                        value(after.1.saturating_sub(before.1).to_string()),
                        plain(after.0.to_string()),
                        plain(after.1.to_string()),
                    ],
                )
                .color(Color::Named(NamedColor::Gray)),
            );
        });
    });
    Ok(1)
}

/// The edition and client version of a player.
fn client_text(bedrock: bool, details: Option<&vanillite_net::ClientDetails>) -> String {
    if bedrock {
        return "Bedrock (through the Bedrock frontend)".to_owned();
    }
    match details {
        Some(details) => java_text(details.protocol()),
        None => "Java (in-process connection)".to_owned(),
    }
}

/// A Java client of `protocol`.
fn java_text(protocol: i32) -> String {
    if protocol == vanillite_protocol::PROTOCOL_VERSION {
        return format!(
            "Java {} (protocol {protocol})",
            vanillite_protocol::VERSION_ID
        );
    }
    let name = vanillite_multiversion::Accepted::all()
        .version(protocol)
        .map_or_else(|| "?".to_owned(), |v| v.names.join("/"));
    format!("Java {name} (protocol {protocol}, through the multi-version layer)")
}

fn debug_lines(
    entry: &crate::player_list::PlayerEntry,
    view: &PlayerView,
    dimension: &str,
) -> Vec<Component> {
    let gray = |c: Component| c.color(Color::Named(NamedColor::Gray));
    vec![
        header(t(
            "debug",
            "Debug of %s (%s)",
            vec![plain(entry.name.clone()), plain(entry.uuid.to_string())],
        )),
        gray(t(
            "debug.client",
            "Client: %s, brand %s, language %s",
            vec![
                value(client_text(entry.bedrock, entry.details.as_deref())),
                value(
                    entry
                        .details
                        .as_ref()
                        .and_then(|d| d.brand().map(str::to_owned))
                        .unwrap_or_else(|| "unknown".to_owned()),
                ),
                plain(view.language.clone()),
            ],
        )),
        gray(t(
            "debug.connection",
            "Ping: %s ms, address %s",
            vec![
                value(entry.sink.latency_ms().to_string()),
                plain(entry.ip.clone()),
            ],
        )),
        gray(t(
            "debug.view",
            "World %s, view distance %s (client %s, server %s)",
            vec![
                plain(dimension.to_owned()),
                value(view.view.to_string()),
                plain(view.client_view.to_string()),
                plain(view.server_view.to_string()),
            ],
        )),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn english(c: &Component) -> String {
        c.to_string_in(&|_| None)
    }

    fn window(tps: f64, mean: f64) -> TickWindow {
        TickWindow {
            ticks: 100,
            tps,
            mean,
            p50: mean,
            p95: mean * 2.0,
            max: mean * 3.0,
            min: mean / 2.0,
        }
    }

    #[test]
    fn version_names_author_website_and_licence() {
        let line = version_line("Vanillite 0.1.0 (Minecraft 26.3, abc1234)");
        assert_eq!(
            english(&line),
            "Vanillite 0.1.0 (Minecraft 26.3, abc1234) by Hannes Thöne · https://vanillite.org · Apache-2.0"
        );
        assert!(line.to_json_string().contains("open_url"));
    }

    #[test]
    fn tps_is_capped_coloured_and_starred_like_paper() {
        let line = tps_line(
            &[window(19.97, 2.0), window(25.0, 2.0), window(12.0, 2.0)],
            20.0,
        );
        assert_eq!(
            english(&line),
            "TPS from last 5s, 1m, 5m: 19.97, *20.00, 12.00"
        );
        let json = line.to_json_string();
        assert!(json.contains("green") && json.contains("red"), "{json}");
        assert!(line.to_json_string().contains("vanillite.command.tps"));
    }

    #[test]
    fn mspt_shows_avg_min_max_per_window() {
        let lines = mspt_lines(
            &[window(20.0, 2.0), window(20.0, 40.0), window(20.0, 80.0)],
            20.0,
        );
        assert_eq!(
            english(&lines[0]),
            "Server tick times (avg/min/max) from last 5s, 1m, 5m:"
        );
        assert_eq!(
            english(&lines[1]),
            "◴ 2.0/1.0/6.0, 40.0/20.0/120.0, 80.0/40.0/240.0"
        );
    }

    fn snapshot(dimension: &str) -> WorldSnapshot {
        let zombie = EntityTypeId::by_name("minecraft:zombie").unwrap();
        WorldSnapshot {
            dimension: dimension.into(),
            chunks: 10,
            full_chunks: 8,
            entities: 5,
            block_entities: 2,
            ticking_block_entities: 1,
            players: 1,
            memory: Some(diagnostics::WorldMemory {
                chunks: 1 << 20,
                entities: 1024,
                block_entities: 512,
            }),
            top_chunks: vec![diagnostics::ChunkCount {
                pos: ChunkPos::new(2, -1),
                entities: 4,
                block_entities: 2,
                types: vec![(zombie, 4)],
            }],
            types: vec![(zombie, 5)],
        }
    }

    fn facts() -> ServerFacts {
        ServerFacts {
            memory_limit: Some(1 << 30),
            cores: 2.0,
            plugins: 3,
        }
    }

    #[test]
    fn chunks_lists_top_chunks_with_teleport() {
        let worlds = [
            snapshot("minecraft:overworld"),
            snapshot("minecraft:the_nether"),
        ];
        let lines = chunks_impl(&worlds, None);
        assert_eq!(
            english(&lines[0]),
            "Top chunks in all worlds (entities, block entities):"
        );
        assert_eq!(
            english(&lines[1]),
            " overworld 2 -1: 4 entities, 2 block entities"
        );
        let json = lines[1].to_json_string();
        assert!(
            json.contains("/execute in minecraft:overworld run tp @s 40 ~ -8"),
            "{json}"
        );
        assert!(json.contains("zombie: 4"), "{json}");
        assert_eq!(lines.len(), 3);
        assert_eq!(english(&chunks_impl(&[], None)[1]), " No loaded chunks");
    }

    #[test]
    fn memory_and_entities_sum_the_worlds() {
        let worlds = [
            snapshot("minecraft:overworld"),
            snapshot("minecraft:the_nether"),
        ];
        let texts = |lines: Vec<Component>| lines.iter().map(english).collect::<Vec<_>>();
        let zombie = EntityTypeId::by_name("minecraft:zombie").unwrap();
        let entity_lines = texts(entities_impl(&worlds, Detail::Entities(Some(zombie)), None));
        assert_eq!(entity_lines[0], "Entities in all worlds: 10");
        assert_eq!(entity_lines[1], " zombie 10");
        assert_eq!(entity_lines[2], "Chunks with the most zombie:");
        let process = diagnostics::ProcessStats {
            rss: Some(64 << 20),
            peak: Some(80 << 20),
            threads: Some(8),
            cpu_seconds: Some(1.0),
        };
        let memory = texts(memory_impl(&facts(), &worlds, process));
        assert_eq!(
            memory[1],
            "Process: 64.0 MiB resident (peak 80.0 MiB), limit 1.0 GiB"
        );
        assert_eq!(memory[2], " Chunks (blocks, light, heightmaps): 2.0 MiB");
        assert_eq!(memory[3], " Entities: 2.0 KiB");
        assert_eq!(memory[4], " Block entities: 1.0 KiB");
        assert!(memory[6].contains("3 plugins"), "{memory:?}");
    }

    #[test]
    fn debug_names_edition_and_version() {
        assert_eq!(
            client_text(true, None),
            "Bedrock (through the Bedrock frontend)"
        );
        assert_eq!(client_text(false, None), "Java (in-process connection)");
        assert_eq!(
            java_text(vanillite_protocol::PROTOCOL_VERSION),
            format!(
                "Java {} (protocol {})",
                vanillite_protocol::VERSION_ID,
                vanillite_protocol::PROTOCOL_VERSION
            )
        );
        let older = vanillite_multiversion::Accepted::all().versions()[0];
        let text = java_text(older.protocol);
        assert!(text.contains(older.names[0]), "{text}");
        assert!(text.ends_with("through the multi-version layer)"), "{text}");
    }

    #[test]
    fn reload_names_applied_and_restart_keys() {
        let report = crate::server::ReloadReport {
            applied: vec!["performance.tick.max-relights".into()],
            restart: vec!["network.threads".into()],
        };
        let lines: Vec<String> = reload_lines(&Ok(report)).iter().map(english).collect();
        assert_eq!(
            lines,
            [
                "Reloaded vanillite.toml: 1 changes apply now, 1 need a restart",
                " Applied: performance.tick.max-relights",
                " After a restart: network.threads",
            ]
        );
        let failed = reload_lines(&Err("bad value".into()));
        assert_eq!(
            english(&failed[0]),
            "Reloading vanillite.toml failed, the old values stay: bad value"
        );
    }

    #[test]
    fn profile_summary_lists_phases_entities_and_the_file() {
        let report = Report {
            seconds: 10.0,
            ticks: 200,
            mspt: 4.0,
            phases: vec![Row {
                name: "overworld: entities".into(),
                millis_per_tick: 2.0,
                percent: 50.0,
                count: 200,
                chunk: None,
            }],
            entity_types: vec![Row {
                name: "zombie".into(),
                millis_per_tick: 1.0,
                percent: 25.0,
                count: 4000,
                chunk: None,
            }],
            chunks: vec![Row {
                name: "overworld 1 2".into(),
                millis_per_tick: 0.5,
                percent: 12.5,
                count: 2000,
                chunk: Some(("minecraft:overworld".into(), ChunkPos::new(1, 2))),
            }],
            sync_loads: (0, Duration::ZERO),
        };
        let lines: Vec<String> = profile_lines(&report, Some(Path::new("profiles/p.txt")))
            .iter()
            .map(english)
            .collect();
        assert_eq!(
            lines[0],
            "Profile of 10.0 s: 200 ticks, 4.00 ms per tick, TPS 20.00"
        );
        assert_eq!(lines[1], "Tick phases:");
        assert!(lines[2].contains("overworld: entities"), "{lines:?}");
        assert!(lines[4].contains("zombie"), "{lines:?}");
        assert_eq!(lines[6], " overworld 1 2: 0.50 ms, 2000 entity ticks");
        assert_eq!(lines[7], "Report: profiles/p.txt");
    }
}

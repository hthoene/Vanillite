# Server, configuration and tick loop

Crates: `vanillite-server`, `vanillite-config`, binary `vanillite`.

## Configuration (`vanillite-config`)

- `server.properties` holds Vanilla's keys with Vanilla's meaning and defaults. Like Vanilla,
  the file is written back on start with every known key; unknown keys are kept.
- `vanillite.toml` holds everything Vanillite-specific: `profile`, rule overrides and
  per-world sections `[worlds."minecraft:the_nether"]`. It is created on first start with every
  rule commented out, documented, with its range.
- Every deviation from Vanilla and every setting is one declaration in the `rules!` catalogue
  (`catalogue.rs`): field, TOML key, `Kind`, optional `range = [min, max]`, optional
  `restart = true`, and per-profile values (`vanilla`, `technical`, `paper`, `spigot`; missing
  profiles take the `vanilla` value). The macro generates the typed structs, TOML access by
  dotted key and the `RuleInfo` table used for docs and validation.
- Two rule sets: `ServerRules` (server scope) and `WorldRules` (overridable per world). Both
  read from the top level of `vanillite.toml`; group keys must be unique across both.
- Performance knobs live under `performance.*` (`performance.threads`, `performance.tick`,
  `performance.autosave`, `performance.distances`, …). New systems add their knobs there.
- Layers, lowest first: profile → global keys → world section → environment variables
  `VANILLITE_<KEY>` (dots become `__`, dashes `_`; `VANILLITE_PROFILE` selects the profile).
- Rules without `restart = true` are hot-reloadable: `vanillite reload config` builds a new
  `Config` and swaps the world rules between ticks. Changed `restart` rules log a warning.
- `Kind::Setting` was added to the kinds of the architecture draft for operational settings
  that do not change game behaviour (thread counts, watchdog).
- Decision D1: exploit rules (`exploits.*`) keep Vanilla behaviour in the `vanilla` profile;
  `paper` fixes them. Decision D2: `performance.tick.parallel-dimensions` is on by default.

## Slots (`slot.rs`)

`Slot<P>` holds one implementation of a slot trait and its `Origin` (built-in, embedded,
plugin). It is filled at most once; a second fill is a startup error naming both. At the end
of startup the server takes the filled implementation or installs the built-in one. Slot
contracts live in the crate that owns them (`Scheduler` here; world-scoped slots in
`vanillite_chunk::slots`); `ServerBuilder` gets one field and one method per slot.

## Threads and the tick loop

- **Compute pool** (`compute.rs`, the `scheduler` slot): one pool, workers = cores minus one
  (the tick threads; dimensions without players sleep between ticks; at least 1) unless `performance.threads.compute` is set. An urgent FIFO for
  fork-join jobs of a running tick phase (`Compute::scope`; the waiting thread helps) and a
  background priority heap (lower number first, FIFO within a priority). `InlineScheduler`
  runs everything inline for deterministic tests.
- **Server thread** runs a port of Vanilla's `runServer`: 50 ms deadline per tick from the tick
  rate manager (rate, freeze, step, sprint), catch-up and "Can't keep up!" after 1 s + 20 ticks,
  pause when empty (`pause-when-empty-seconds`), and between ticks it handles messages until
  the next deadline (Vanilla's `waitUntilNextTick`).
- **Server tick**: G1 (console commands and global tasks, `&mut Server`) → world phase →
  cross-world hand-offs in world order → G2 (player list, autosave).
- **Sequential mode**: the server thread ticks the worlds in Vanilla order (overworld, nether,
  end). `TickContext::in_world` runs on the other world immediately.
- **Parallel mode** (default): every world has a thread that owns it, ticks on command and runs
  the world's tasks between ticks. `in_world` is queued and runs after the world phase,
  before the target's next tick. A panicking world thread stops the server.
- `ServerHandle` (any thread): `stop`, `console`, `run_global`, `run_in_world`.

## Autosave, shutdown, watchdog

- Autosave every `max(100, tick rate × 300)` ticks like Vanilla, or
  `performance.autosave.interval`; worlds save between their ticks.
- Shutdown (Ctrl-C, SIGTERM, `stop`): stops ticking, saves every world with flush on its
  owner thread, joins the world threads, drains the compute pool, releases `session.lock`
  (held with an OS file lock so a second server cannot open the same world).
- Watchdog: compares the clock with the current tick deadline. After
  `watchdog.early-warning-after` seconds it warns every `watchdog.warning-interval` seconds
  and names the busy threads; after `max-tick-time` (server.properties, ≤ 0 disables) it logs
  and exits the process. Thread backtraces are not printed yet.

## Not yet wired

Network (joins in G1, bounded by `security.max-joins-per-tick`), the world runtime inside
`ServerWorld`, players and their saving, the command system (the console knows `stop`,
`help` and `vanillite reload config`).

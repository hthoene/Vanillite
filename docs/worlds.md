# Worlds: formats, layouts, saving and backups

Crate: `vanillite-storage`. All keys below are in `vanillite.toml`; their defaults and ranges
are in the [configuration reference](configuration.md).

## Formats

| Format | Files | Used for |
|---|---|---|
| Own format (`vanillite`) | `region/r.<x>.<z>.vwr` (also for `entities/` and `poi/`), marker `<level>/vanillite/format` | New worlds (`storage.world-format = "vanillite"`, the default). |
| Anvil (`anvil`) | `region/r.<x>.<z>.mca`, as Vanilla | Existing Vanilla, Paper and Spigot worlds; new worlds with `storage.world-format = "anvil"`. |

`level.dat`, player data (`players/`) and the saved data in `data/` are Vanilla files in both
formats. A world keeps the format it was found in until it is converted.

The own format keeps Anvil's 32 × 32 chunk grid per file, so conversion is 1:1 per file. Its
records are zstd-compressed (`storage.compression-level`), and every record carries a
checksum that is checked on every read. Each file has two copies of its index: a save writes
new records next to the old ones and then the older index copy, so a crash at any point leaves
the file as of its last completed save.

### Converting and exporting

| How | What |
|---|---|
| `vanillite convert [--to vanillite\|anvil]` | Converts a stopped world into the given format (default: `storage.world-format`). |
| `storage.convert-world = true` | The same at startup, before the world loads. |
| `vanillite export <folder> [--to anvil\|vanillite]` | Writes a copy of the world in the other format into `<folder>`: by default a Vanilla (Anvil) copy of an own-format world, with `--to vanillite` an own-format copy of an Anvil world. Every file is copied, every record verified; the world itself stays unchanged. |
| `vanillite upgrade-world` | Rewrites every chunk of an own-format world that an older Minecraft version wrote (otherwise chunks are upgraded when they are saved). |

A conversion never risks the original: the new files are written next to the old ones, read
back and compared record by record before the switch. Then the originals are moved into
`world-format-backup/<time>/` (`storage.keep-converted-files`, on by default) or deleted. If the
check fails, the world stays as it was and the server starts with it.

A conversion that stopped (killed, disk full, an unreadable chunk) is resumed by running it
again: files that were already verified and whose original did not change since are kept. One
that stopped after every file was verified is finished at the next start. Dimensions of the
world's enabled data packs are converted and exported with the others; a world with dimension
folders the server does not load (those of a removed data pack) is not converted.

An own-format world written by an older Minecraft version is read by newer servers (block and
entity ids are mapped by name); a world of a newer version is refused.

## Folder layouts

`storage.layout` decides where the nether and the end live:

| Layout | Overworld | Nether | End |
|---|---|---|---|
| `vanilla` (Vanilla and Paper) | `<level>/dimensions/minecraft/overworld/` | `<level>/dimensions/minecraft/the_nether/` | `<level>/dimensions/minecraft/the_end/` |
| `spigot` (Spigot and CraftBukkit) | `<level>/dimensions/minecraft/overworld/` | `<level>_nether/dimensions/minecraft/the_nether/` | `<level>_the_end/dimensions/minecraft/the_end/` |

Dimensions of data packs (`data/<namespace>/dimension/<path>.json`, loaded at startup like
Vanilla, with the dimensions of `world_gen_settings.dat`) live in
`<level>/dimensions/<namespace>/<path>/`, with `spigot` in
`<level>_<namespace>_<path>/dimensions/<namespace>/<path>/`.

Players, `level.dat` and the world-wide saved data stay in `<level>/` in both layouts. A world
found in the other layout is moved into the configured one once at startup, so a world can go
between Vanillite and Paper or Spigot without manual moving (Anvil format, or after
`vanillite export`).

## Extra worlds

Besides the default world (`level-name` with its Overworld, Nether and End) a server can run
more independent worlds at once, like Multiverse on Paper. Each one is a single dimension with
its own folder, as Bukkit keeps a world:

| File | What |
|---|---|
| `<name>/level.dat` | Its name and spawn. |
| `<name>/data/minecraft/world_gen_settings.dat` | Its seed and generator (Vanilla's file). |
| `<name>/dimensions/minecraft/<name>/` | Its chunks, entities and POI, in the format it was found in (new worlds in `storage.world-format`). |
| `<name>/session.lock` | Held while it is loaded. |

Its dimension key is `minecraft:<name in lower case>` (for `/execute in`, `/tp` and data
packs), and its rules come from `[worlds."minecraft:<name>"]` like those of any dimension.
Time, weather, game rules, difficulty and the player data are the server's, as Vanilla shares
them between its dimensions: one player data file per player, as Bukkit keeps it. Without
extra worlds nothing changes.

Extra worlds are declared in `vanillite.toml` and loaded at startup:

```toml
[extra-worlds.lobby]
environment = "normal"     # normal, nether or the_end: its dimension type
generator = "flat"         # normal, flat, void, amplified, large_biomes, or a dimension key
seed = ""                  # empty: random; a number or a text like level-seed
generate-structures = true
```

`generator`, `seed` and `generate-structures` only matter when the world is created; an
existing world keeps what it was created with. A dimension key as generator (e.g.
`minecraft:overworld` or a data pack's dimension) takes that dimension's generator.

Plugins create, load and unload extra worlds while the server runs (`Server::create_world`,
`load_world`, `unload_world`; events `on_world_load` and `on_world_unload`) and move players
between worlds (`Server::move_player`). A world loads in the next global phase (the server
thread waits for its spawn area, like Bukkit's `createWorld`); unloading saves it, releases its
folder and frees its memory, and is refused while players are in it. Each world ticks on its
own thread with `performance.tick.parallel-dimensions`, like every dimension.

**Portals and respawns** work as on Spigot: a nether portal in an extra world leads to the
default world's Nether (from a world of the `nether` environment to the Overworld), an end
portal to the default world's End (from a `the_end` world to the world spawn). Plugins can
point a portal elsewhere, also into another world (`on_entity_portal`, Multiverse-NetherPortals
style). Beds and respawn anchors in an extra world make it the player's respawn world;
without a respawn point players respawn at the default world's spawn, which is also where new
players join.

## Saving

- **Autosave** runs at Vanilla's interval (every 5 minutes of game time, or
  `performance.autosave.interval`). A save round captures the changed chunks after the world
  tick from the time left in the tick (`storage.autosave.tick-budget`), spread over as many
  ticks as needed, so saving causes no tick spikes.
- **Disk work** (reading and writing files) runs on its own IO threads (`storage.io-threads`),
  compression on the shared worker pool. `storage.max-pending-writes` bounds the memory of
  saves waiting for a slow disk.
- **Small files** (`level.dat`, player data, `data/`) are written to a temporary file, synced
  and renamed over the old one; `level.dat_old` and `<uuid>.dat_old` keep the previous version
  as in Vanilla.
- `save-all flush`, `stop`, Ctrl+C and SIGTERM save every world completely before the server
  exits. `session.lock` keeps a second server from opening the same world.
- **Full disk or read-only file system:** chunks that cannot be written stay in memory, the
  server keeps running and logs the error, and every save tries them again until the disk takes
  them. Records written before stay intact.
- **Damaged files** are never saved over without a copy: an unreadable chunk, entity or POI
  record goes to `<dimension>/unreadable/` before the chunk is generated again, a damaged
  own-format region file is kept as `<file>.damaged-<time>`, and a damaged `level.dat`, player,
  statistics, advancements or saved data file as `<file>.corrupted-<hash>`.

## Backups

`backup.interval` (minutes, off by default) makes zip archives of the world in
`backup.directory` while the server runs: the server saves, then a background thread copies the
files, so ticks never wait. Region files are copied while no chunk is written into them, so
every chunk in an archive is complete. `backup.keep` limits the number of archives.

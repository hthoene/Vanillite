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
that stopped after every file was verified is finished at the next start. Worlds with
dimensions this server does not load (custom dimensions of data packs) are not converted.

An own-format world written by an older Minecraft version is read by newer servers (block and
entity ids are mapped by name); a world of a newer version is refused.

## Folder layouts

`storage.layout` decides where the nether and the end live:

| Layout | Overworld | Nether | End |
|---|---|---|---|
| `vanilla` (Vanilla and Paper) | `<level>/dimensions/minecraft/overworld/` | `<level>/dimensions/minecraft/the_nether/` | `<level>/dimensions/minecraft/the_end/` |
| `spigot` (Spigot and CraftBukkit) | `<level>/dimensions/minecraft/overworld/` | `<level>_nether/dimensions/minecraft/the_nether/` | `<level>_the_end/dimensions/minecraft/the_end/` |

Players, `level.dat` and the world-wide saved data stay in `<level>/` in both layouts. A world
found in the other layout is moved into the configured one once at startup, so a world can go
between Vanillite and Paper or Spigot without manual moving (Anvil format, or after
`vanillite export`).

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

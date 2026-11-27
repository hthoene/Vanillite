# Vanillite commands

Besides Vanilla's commands (and Paper's `/plugins`, `/version` with `commands.bukkit-commands`),
Vanillite has its own admin commands under `/vanillite <subcommand>`, alias `/vl`, also with
the namespace: `/vanillite:vanillite`, `/vanillite:vl`. They cost nothing while nobody uses
them: the figures are read when a command asks, and the profiler measures only while a
profile runs. `/vanillite` alone lists the subcommands the sender may use.

Each subcommand has its own permission node `vanillite.command.<subcommand>`. A permission
plugin (LuckPerms through the Paper layer, or a native plugin's permission provider) can grant
or deny it; without its answer the default operator level below applies. The command tree a
client gets shows only what the player may use. Texts are translatable
(`vanillite.command.*`) with English as the fallback.

| Command | Default | What |
|---|---|---|
| `/vanillite version` | everyone | Version, Minecraft version and commit, author, website and licence. Works in every profile; plain `/version` stays Vanilla's (or Bukkit's with `commands.bukkit-commands`). |
| `/vanillite tps` | level 2 | Ticks per second over the last 5 s, 1 min and 5 min, like Paper's `/tps`. Also `/vanillite:tps`, and `/tps` when `commands.bukkit-commands` is on (`paper` and `spigot` profiles). |
| `/vanillite mspt` | level 2 | Tick times (average, minimum, maximum in ms) over the last 5 s, 1 min and 5 min, like Paper's `/mspt`. Also `/vanillite:mspt`, and `/mspt` with `commands.bukkit-commands`. |
| `/vanillite status` | level 3 | TPS and tick times (average, p50, p95, max per window), resident memory of the process with its peak and limit, CPU use since the last `status`, players, per world the loaded chunks, entities and block entities, uptime and version. |
| `/vanillite memory` | level 3 | Resident memory of the process and what the worlds' chunks (blocks, light, heightmaps), entities and block entities take (per world on hover); the rest is code, caches, network buffers, the allocator and plugins. |
| `/vanillite chunks [<world>]` | level 3 | The chunks with the most entities and block entities, in one world or all; hover shows the entity types, a click teleports there. |
| `/vanillite entities [<world> [<type>]]` | level 3 | Entities by type; with a type the chunks with the most of it (click to teleport). |
| `/vanillite profile [<seconds>]` | level 4 | Profiles the server for 1 to 600 seconds (default 10): time per tick phase and world (network, plugins including the Paper layer, block ticks and redstone, chunk system, spawning, random ticks, entities, block entities, tracking, chunk sending, saving), entities by type, the chunks whose entities take the most time, and synchronous chunk loads. The summary goes to the sender (with hover and teleport clicks), the full report to `profiles/profile-<date>.txt` in the server folder. One profile at a time. |
| `/vanillite reload config` | level 4 | Reloads `vanillite.toml` and names the changed keys that apply now and those that need a restart. Also from the console. |
| `/vanillite debug <player>` | level 4 | Client edition and version (Java, Java through the multi-version layer, Bedrock), brand, language, ping, address, world, view distance (effective, client, server) and the packet rates over the next second. |

The console also has `vanillite plugins` and, with `plugins.development`, `vanillite plugin
reload <id>`.

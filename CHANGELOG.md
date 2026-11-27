# Changelog

The changes of each Vanillite release for players and server owners; the GitHub release of a
version shows its section.

## 0.1.0-beta.2

Minecraft Java Edition 26.3.

### Gameplay

- Death and respawn like Vanilla: kill credit, death messages, sleeping, beds, respawn anchors
  and spawn points.
- Dispensers build golems and withers, use spawn eggs, buckets, fire charges and more;
  droppers and crafters reach every container; jukeboxes and shelves are containers.
- Many more items and blocks behave like Vanilla: tools, bone meal, wax, buckets, bottles,
  throwables, elytra, totems, tridents, maces, fishing, fireworks, shields, pots and sherds,
  hanging signs, suspicious blocks, waterlogging, sponges, dripleaf, turtle eggs, kelp, coral.
- Block contact for every entity (pressure plates, tripwires, cactus, cobwebs, lily pads) and
  falling block damage; step and fall sounds.
- Mobs: many more mob mechanics tick-exact with Vanilla (egg laying, sheep dyeing, lightning
  conversions, leashes on fences, piglins guarding containers, ominous trial spawners, animal
  sound variants).
- Vehicles carry their passengers through portals; the End dragon fight starts and ends
  correctly; `/kill` and `/summon` work on the dragon and all mobs like Vanilla.
- All advancement triggers and statistics of Minecraft 26.3, with rewards like Vanilla.
- Loot tables: better compatibility, `set_components` and `/loot spawn`.
- Container locks on block entities; custom dimensions from data packs; several independent
  worlds on one server.

### Performance

- World generation about 1.6x faster per core, still bit-exact with Vanilla.
- Faster light updates after block changes and faster chunk delivery while all workers are busy.
- Mob activation range for far-away mobs (configurable), cheaper
  despawn checks and path finding; dense crowds cost less network time.
- Lower memory use: memory is given back while the server is empty, world generation data
  and structure templates load on demand.
- Runs on very weak hardware: respects CPU quotas, one core plays smoothly.
- Linux archives and the Docker image include an x86-64-v3 (AVX2) build that is picked
  automatically on CPUs that support it.
- Optional tile region mode: natural spawning and random ticks on several cores.

### Server and plugins

- Plugin ABI 1.10: many new events (player, entity, block, chunk, world, inventory), entity,
  world, menu, item, scoreboard, boss bar and registry API, packet listeners, custom goals and
  world populators.
- New anti-cheat rule `anticheat.digging.fast-break-check`.
- Proxy: BungeeCord forwarding, `send`/`alert`/`reload` and a console; it tries each server
  once and shows the server's kick reason.
- Bedrock: clients can also join over NetherNet (WebRTC); containers, crafting, block
  entities, sidebar and item names; every connection attempt is logged.
- Older clients: recipe book, stonecutter, advancements and book and container items.
- Safer world storage: full disks and damaged files no longer lose data, world conversion
  can resume.
- Pterodactyl and Pelican: a simple and an advanced egg, one image per Minecraft version.

### Building from source

- The Vanilla data is no longer part of the source: the build downloads Mojang's official
  server jar and extracts the data from it (a JDK is downloaded too if none is installed).

## 0.1.0-beta.1

The first public release, for Minecraft Java Edition 26.3.

# World access: reads, writes and chunk loads

Crates: `vanillite-world` (`chunks`), `vanillite-game` (`level`, `mob`).

## Two read paths

A world has two ways to read blocks. Which one a piece of code uses is part of its Vanilla
parity, so pick it by the Vanilla call it models.

| Path | API | Vanilla | Side effect | Threads |
|---|---|---|---|---|
| Tick view | `Level::block`, `Level::fluid`, `Chunks::block`, `impl Blocks for Level` | `Level.getBlockState` (`getChunk(x, z, FULL, true)`) | Goes through the chunk cache and keeps the `unknown` ticket of a synchronously loaded chunk alive | World thread only |
| Read view | `Level::view()` → `LevelView`, `Chunks::view()` → `ChunkView` | `getChunkForCollisions`, `getChunkNow`, `PathNavigationRegion` | None | `Copy + Send + Sync`, shareable while the world thread does not write |

- Collision (`mob::physics::{collide, collide_for, block_boxes, no_collision}`,
  `block::falling::collide`) and path finding (`PathFinder::find_path`) read through the view.
- Code that models `getBlockState` on the level (ray casts, fluid checks, block behaviours)
  reads through the tick view.
- New code that only reads blocks and runs, or may later run, outside the world thread
  (tracking, path finding, AI proposals, spawn scans) takes a `LevelView` or `&dyn Blocks`,
  never `&Level`.
- Known difference: Vanilla's collision reads put the chunk into the chunk cache, so a
  following `getBlockState` of that chunk does not reset its `unknown` ticket. The view does
  not model the cache. This only matters for chunks held by nothing but a synchronous load's
  ticket.

## Rule 1: an entity's world changes are applied after its tick

The entity loop (`vanillite-server/src/mobs.rs`, `WorldMobs::tick_entities`) walks the tick list
once, in Vanilla's order. An entity tick gets the world read-only (`Tick { level: &Level, .. }`)
and `&mut Entities`; every change of blocks or the level (`setBlock`, `destroyBlock`, explosions,
game events, level events, block entity changes) is queued as a `mob::WorldAction` and applied
right after that entity's tick, before the next entity ticks (`apply_actions`). In Vanilla those
changes are immediate.

- This is exact as long as an entity does not read back its own change within the same tick.
  Code that may read a position it changed in the same tick reads through `Tick::block`, which
  sees the entity's own queued `setBlock` and `destroyBlock` (example: the snow golem's trail,
  whose four positions can be the same block).
- Effects that Vanilla produces in the same call keep their order in the queue (a `setBlock`
  followed by its `gameEvent`).
- Entities of every kind tick in the one list, items included (`ItemEntity.tick`, with the
  portal handled in `baseTick` before the item moves).
- Counter: `WorldMobs::deferred_actions`, logged per world once a minute at debug level.
- Scenarios: `mob_snow_golem_trail` (read after own write, game event), `mob_sheep_eat_grass`,
  `mob_creeper_explode`, `mob_enderman_teleport`.

This shape (read-only world plus a queue of changes) is what read-only parallel phases and
region mode need. Do not hand an entity tick `&mut Level`.

## Rule 2: synchronous chunk loads block the tick

`Level::load_chunk`, `Level::block_loading` and `Chunks::load_now` model Vanilla's
`getChunk(x, z, FULL, true)` on the world thread: a chunk that is not accessible gets an
`unknown` ticket (level 33, one tick) and is loaded or generated while the world thread waits.

- Use them only where Vanilla loads (`getBlockState`/`setBlock` through `getChunkAt`), never
  where Vanilla checks `hasChunkAt`, `isLoaded` or `getChunkNow` first. Everything else reads
  without loading: `Level::block` (air when the chunk is not live) or the read view.
- A guard may only be added where Vanilla has one; removing a load that Vanilla does changes
  the result of the read.
- Making the wait cheaper (priority for the chunk's jobs, the waiting thread running them) is
  allowed and changes nothing in the game.
- Counter: `Chunks::sync_loads` (count and time waited); every load is logged at debug level
  with its chunk and wait, and a world with loads in the last minute logs one info line.
- Scenarios: `redstone_border_chunk_load` (the `unknown` ticket keeps the chunk for one tick
  and a second read resets it), `falling_block_lazy_chunk`.

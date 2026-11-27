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

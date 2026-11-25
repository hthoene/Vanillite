# Structure goldens

Vanilla 26.3 parity goldens for structures, recorded by
`tools/extractor/src/dev/vanillite/extractor/golden/StructureGoldens.java` with the real
`NoiseBasedChunkGenerator`, `ChunkGeneratorStructureState` and `StructureManager` (structures on,
no blending). Each file has a `"format"` member with the same description in short; the class
comment of `StructureGoldens.java` is the reference.

Regenerate (from the repository root; needs the unpacked 26.3 jar in
`target/vanilla/26.3/{versions,libraries}`):

```sh
VANILLITE_JAVA_HOME=$HOME/jdks/temurin-25-jdk cargo xtask golden regen StructureGoldens
```

The output is deterministic (two runs give identical files). Layout of every file: one top-level
member per line, and one array element per line for top-level arrays (each element compact JSON;
nulls are kept).

The Rust side is `tests/structure_goldens.rs` (one test per file below). It prints, per
structure id, how many items match and the first difference of the others, lists structures
whose type is still a stub (`src/structure/kinds/unported.rs`) apart, and fails if anything
differs. A start (or a reference to it) of a ported structure where Vanilla started a stubbed
structure of the same structure set (e.g. a bastion remnant where Vanilla has a fortress: the
stub never generates, so the set's selection goes on) is listed with the stubs too. `VANILLITE_STRUCTURE=minecraft:igloo` checks one structure id (with all details);
`VANILLITE_STRUCTURE_DIFFS=100` lists more differences per item.

## Worlds

| index | dimension | seed | area (chunks, min x/z, size) | search |
| --- | --- | --- | --- | --- |
| 0 | overworld | 0 | -32, -32, 64 × 64 | yes |
| 1 | overworld | -7302402383914416 | -16, -16, 32 × 32 | no |
| 2 | nether | 42 | -16, -16, 32 × 32 | yes |
| 3 | end | 1 | 80, -16, 32 × 32 | yes |

Generator, random state and registries are the ones of `FeatureGoldens` (registries loaded from the
server jar's data pack like a server loads them, so structure sets are in registry order). The
structure state is `generator.createState(registries.lookupOrThrow(STRUCTURE_SET), randomState,
seed)` followed by `ensureStructuresGenerated()`, as `ChunkMap`/`ServerLevel` do.

Every chunk in the goldens goes through the Vanilla steps in order, each with Vanilla's
`WorldGenRegion` access rule of its step (`ChunkPyramid.GENERATION_PYRAMID` direct dependencies):

1. `createStructures(registryAccess, state, structureManager, chunk, templateManager, dimension)` on
   a fresh `ProtoChunk` (the manager only reads and writes the chunk's own starts).
2. `createReferences(region, structureManager, chunk)` (starts of the chunks within 8).
3. `createBiomes` (no blending).
4. Terrain: `Beardifier.forStructuresInChunk(structureManager, pos)` with a manager over the region
   of the `terrain` step, `new NoiseChunk(randomState, beardifier, settings, fluidPicker,
   Blender.empty(), volume)`, then `doFill`, `buildSurface`, `generateCarvers` (as `chunk.json`),
   then the final heightmaps are primed.
5. `applyBiomeDecoration(region, chunk, structureManager)` with `generateStructures` on.

## structure_starts.json

- `worlds[i]`: `dimension`, `seed`, `min_y`, `height`, `origin`
  (`ChunkGeneratorStructureState.getDimensionOrigin`, chunk x/z), `area` `[min_x, min_z, size_x,
  size_z]`, `stronghold_rings` (all positions of `getRingPositionsFor` of the stronghold placement,
  or `null` in dimensions without strongholds), `chunks` (targeted chunks).
- Step 1 ran for every chunk of the area (z outer, x inner) and then for each targeted chunk (list
  order). Targeted chunks: the first stronghold ring (3 positions) and, for worlds with search, for
  each structure the area lacks, the first chunk where its start generates (found by a spiral over
  the grid of its random spread set; only the resulting chunks matter). Every chunk that is not
  listed in `starts` has no start.
- `starts`, one per line, in that chunk order, per chunk sorted by structure id:
  `{"w": world index, "structure": id, "chunk": [x, z], "references": n, "bb": box, "pieces": [...]}`
  where `bb` is `StructureStart.getBoundingBox()` as `[min_x, min_y, min_z, max_x, max_y, max_z]`.
- Each piece (in `StructureStart.getPieces()` order) is
  `[type id, bb, orientation, gen_depth, rotation, mirror, data]`:
  - `orientation`: direction name (`north`, …) or `null`;
  - `rotation`, `mirror`: `StructurePiece.getRotation()/getMirror()` as enum constant names
    (`NONE`, `CLOCKWISE_90`, `CLOCKWISE_180`, `COUNTERCLOCKWISE_90`; `NONE`, `LEFT_RIGHT`,
    `FRONT_BACK`) or `null`. For template pieces this is the placement settings' rotation/mirror,
    for jigsaw pieces the jigsaw rotation, otherwise derived from the orientation;
  - `data`: the piece's NBT (`StructurePiece.createTag`) without `id`, `BB`, `O`, `GD`, keys sorted,
    numbers as JSON numbers (bytes/booleans as 0/1). Template pieces carry `Template` and
    `TPX/TPY/TPZ` (plus their own keys, e.g. `Rot`, `Integrity`, `BiomeType`); jigsaw pieces carry
    `pool_element` (`element_type`, `location`, `processors`, `projection`), `PosX/PosY/PosZ`,
    `rotation`, `ground_level_delta` and `junctions` (`source_x`, `source_ground_y`, `source_z`,
    `delta_y`, `dest_proj`).

Content (88 starts, 6212 pieces, 44 of the 48 piece types; the ocean monument's sub pieces and
`nestart` never appear in a start's piece list): every structure of the 26.3 data pack at least
once — abandoned camps (all 18 variants, forest 2), ancient city 2, bastion remnant 1, buried
treasure 2, desert pyramid 1, end city 1, fortress 2, igloo 1, jungle pyramid 1, mansion 1,
mineshaft 16, mineshaft mesa 5, monument 1, nether fossil 2, ocean ruins cold 1 / warm 1, pillager
outpost 1, ruined portals (standard 2, desert 1, jungle 1, mountain 1, nether 1, ocean 3, swamp 1),
shipwreck 2, beached shipwreck 1, stronghold 6, swamp hut 1, trail ruins 1, trial chambers 3,
villages (desert 2, plains 1, savanna 1, snowy 1, taiga 1).

## structure_references.json

- `areas[i]`: `dimension`, `seed`, `min_y`, `height`, `structure` (the area is centred on the chunk of
  that structure's first start in `structure_starts`), `area` `[min_x, min_z, 16, 16]`.
- `chunks`, one per line, every chunk of every area (z outer, x inner):
  `{"a": area index, "chunk": [x, z], "references": {structure id: [[start x, z], ...]}}` after
  steps 1–2. Structure ids are sorted; the start chunks are in the iteration order of Vanilla's
  `LongOpenHashSet` (default capacity, `ChunkPos.pack` keys inserted by `createReferences` with
  source x outer, z inner), which is the order `StructureManager.startsForStructure` yields their
  starts in (and so the order in which overlapping starts of one structure are placed).

## structure_terrain.json

`chunks`, one per line: `dimension`, `seed`, `min_y`, `height`, `structure`, `start` (first start of
that structure), `x`, `z`, `references` (as above), `beardifier`, `noise`, `surface`, `carvers`.

- Targets: villages (all 5), pillager outpost, abandoned camp (forest) (`beard_thin`), ancient city
  (`beard_box`), trial chambers (`encapsulate`), trail ruins and stronghold (`bury`), nether fossil
  (`beard_thin`, nether). Per structure, all at the z of the centre of its first piece: the chunk of
  that centre, the chunk west of it and the chunk of the block west of the start's bounding box
  (without repeats).
- Chunks get steps 1–3 for the 3×3 area (references radius 1, starts radius 9) and step 4 for the
  target chunk.
- `beardifier`: `starts` `[[structure id, start x, z], ...]` that
  `startsForStructure(x, z, s -> s.terrainAdaptation() != NONE)` returned (sorted), `rigids` and
  `junctions` (entry counts) and `affected_box` (or `null`). The Beardifier sums the float
  contributions of its rigids, then its junctions, in the order of its starts. Across structures
  that order is Vanilla's `HashMap` order of `Structure` objects (identity hash codes), which
  changes from one JVM run to the next, so Vanilla itself is not deterministic there. Only the
  `ancient_city` target at chunk 42, 79 has two adapted structures (with trial chambers); runs with
  both orders recorded the same blocks.
- `noise`, `surface`, `carvers`: snapshots after each terrain step in the format of `chunk.json`
  (per-section block id hashes, `world_surface_wg`/`ocean_floor_wg` heightmap hashes,
  post-processing marks).

## structure_decoration.json

`areas`, one per line, one per structure id (its first start in `structure_starts`; 52 areas):
`dimension`, `seed`, `min_y`, `height`, `structure`, `start`, `x`, `z` (centre chunk), `chunks`,
`entities`.

- Centre: of the chunks intersecting a piece of the start, the one whose 3×3 chunk area intersects
  the most distinct piece types (ties: nearest to the start chunk by chessboard distance, then
  smaller z, then smaller x).
- On fresh chunks: steps 1–4 for the 5×5 chunks around the centre (starts radius 10, references
  and terrain radius 2, biomes radius 3), then step 5 for the 3×3 chunks around the centre in
  row-major order (z outer, x inner). The decoration region writes within one chunk and reads starts
  within 8, like Vanilla's `features` step.
- `chunks`: the 5×5 chunks (z outer) in the format of `feature.json` decorations — `sections`,
  `heightmaps` (`WORLD_SURFACE`, `OCEAN_FLOOR`, `MOTION_BLOCKING`, `MOTION_BLOCKING_NO_LEAVES`
  hashes), `post_processing`, `block_ticks`, `fluid_ticks` — and `block_entities`
  `[x, y, z, type id, nbt]` sorted by y, z, x. `nbt` is `BlockEntity.saveWithoutMetadata` as JSON
  (keys sorted; containers carry `LootTable` and `LootTableSeed`, spawners `SpawnData`, …), or `null`
  with type `DUMMY` for a block entity that was never created (the pending placeholder tag of
  `WorldGenRegion.setBlock`).
- `entities`: `[decorated chunk index 0–8 (row-major), entity type id, x, y, z]` in the order the
  region's `addFreshEntity` got them (Vanilla stores them in the proto chunk).

Harness details that matter for parity: the fake server level has difficulty `easy` (default of a
fresh dedicated server; some pieces only replace their data marker when their mob was created),
time 0 (full moon), item components bound, and its `structureManager()` reads any chunk of the
area (entity variant conditions use it, e.g. the swamp hut cat).

## structure_large.json

Whole starts of the large structures, checked by `tests/large_structure_goldens.rs`
(`VANILLITE_STRUCTURE` selects one structure id there too).

- `starts`, one per line: for each target (world 0: 3 monuments, 2 mansions; world 2: one bastion
  remnant per start pool variant — units, hoglin stable, treasure, bridge; world 3: 4 end cities)
  its first starts (for a variant: whose first piece comes from that start pool element), found by a spiral over the grid cells of its random spread set around the
  origin (as the search of `structure_starts`, but collecting several hits): `dimension`, `seed`,
  `min_y`, `height`, `w`, `structure`, `chunk`, `references`, `bb`, `pieces` (as in
  `structure_starts`) and `decorated`: every chunk that a piece's box intersects, z outer then x.
- Per start, on fresh chunks, `applyBiomeDecoration` with structures runs for the decorated
  chunks in that order (each chunk's dependencies first, as in `structure_decoration`).
- `chunks`, one per line per decorated chunk, recorded right after its own decoration: `s` (start
  index), `chunk`, the `structure_decoration` chunk format (`sections`, `heightmaps`,
  `post_processing`, `block_entities`, `block_ticks`, `fluid_ticks`) and `entities`
  `[[type id, x, y, z], ...]` that its decoration added.

Content: monuments at chunks [86, -12], [-44, -87], [-21, -79]; mansions at [-221, -52] (531
pieces) and [-305, 434] (580 pieces); end cities at [-15, -78] (105 pieces), [-73, -15] (57, with
a ship), [86, -15] and [106, -98] (74, with a ship). 16 chunks per monument, 29–30 per mansion,
4–28 per end city; bastion remnants at [-21, 47] (units), [-25, -43] (hoglin stable), [37, 45]
(treasure) and [59, -49] (bridge), 15–21 chunks each; entities: shulkers, the ships' elytra item frames, elder guardians, evokers,
vindicators (and mineshaft chest minecarts).

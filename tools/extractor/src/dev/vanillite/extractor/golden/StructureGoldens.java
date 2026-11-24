// Copyright Hannes Thöne. Licensed under the Apache License, Version 2.0.
package dev.vanillite.extractor.golden;

import com.google.gson.JsonArray;
import com.google.gson.JsonElement;
import com.google.gson.JsonNull;
import com.google.gson.JsonObject;
import com.google.gson.JsonPrimitive;
import dev.vanillite.extractor.golden.FeatureGoldens.Dimension;
import dev.vanillite.extractor.golden.FeatureGoldens.Region;
import dev.vanillite.extractor.golden.FeatureGoldens.Snapshot;
import it.unimi.dsi.fastutil.longs.LongSet;
import java.lang.reflect.Field;
import java.lang.reflect.Method;
import java.util.ArrayList;
import java.util.Comparator;
import java.util.HashMap;
import java.util.HashSet;
import java.util.LinkedHashMap;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.TreeMap;
import java.util.TreeSet;
import java.util.function.Supplier;
import net.minecraft.core.BlockPos;
import net.minecraft.core.Direction;
import net.minecraft.core.Holder;
import net.minecraft.core.QuartPos;
import net.minecraft.core.Registry;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.core.registries.Registries;
import net.minecraft.nbt.ByteArrayTag;
import net.minecraft.nbt.ByteTag;
import net.minecraft.nbt.CompoundTag;
import net.minecraft.nbt.DoubleTag;
import net.minecraft.nbt.FloatTag;
import net.minecraft.nbt.IntArrayTag;
import net.minecraft.nbt.IntTag;
import net.minecraft.nbt.ListTag;
import net.minecraft.nbt.LongArrayTag;
import net.minecraft.nbt.LongTag;
import net.minecraft.nbt.ShortTag;
import net.minecraft.nbt.StringTag;
import net.minecraft.nbt.Tag;
import net.minecraft.resources.ResourceKey;
import net.minecraft.world.entity.Entity;
import net.minecraft.world.level.ChunkPos;
import net.minecraft.world.level.Level;
import net.minecraft.world.level.LevelHeightAccessor;
import net.minecraft.world.level.StructureManager;
import net.minecraft.world.level.biome.Biome;
import net.minecraft.world.level.biome.BiomeManager;
import net.minecraft.world.level.block.entity.BlockEntity;
import net.minecraft.world.level.chunk.ChunkAccess;
import net.minecraft.world.level.chunk.ChunkGeneratorStructureState;
import net.minecraft.world.level.chunk.ProtoChunk;
import net.minecraft.world.level.chunk.UpgradeData;
import net.minecraft.world.level.chunk.status.ChunkPyramid;
import net.minecraft.world.level.chunk.status.ChunkStatus;
import net.minecraft.world.level.levelgen.Aquifer;
import net.minecraft.world.level.levelgen.Beardifier;
import net.minecraft.world.level.levelgen.Heightmap;
import net.minecraft.world.level.levelgen.NoiseBasedChunkGenerator;
import net.minecraft.world.level.levelgen.NoiseChunk;
import net.minecraft.world.level.levelgen.RandomState;
import net.minecraft.world.level.levelgen.WorldOptions;
import net.minecraft.world.level.levelgen.blending.Blender;
import net.minecraft.world.level.levelgen.densityfunction.DensityVolume;
import net.minecraft.world.level.levelgen.material.rule.MaterialRule;
import net.minecraft.world.level.levelgen.structure.BoundingBox;
import net.minecraft.world.level.levelgen.structure.Structure;
import net.minecraft.world.level.levelgen.structure.StructurePiece;
import net.minecraft.world.level.levelgen.structure.StructureSet;
import net.minecraft.world.level.levelgen.structure.StructureStart;
import net.minecraft.world.level.levelgen.structure.TerrainAdjustment;
import net.minecraft.world.level.levelgen.structure.pieces.StructurePieceSerializationContext;
import net.minecraft.world.level.levelgen.structure.placement.ConcentricRingsStructurePlacement;
import net.minecraft.world.level.levelgen.structure.placement.RandomSpreadStructurePlacement;
import net.minecraft.world.level.levelgen.structure.placement.StructurePlacement;

/**
 * Structure goldens: Vanilla 26.3 structure starts, references, terrain with the Beardifier and
 * decoration with structures, run with the real {@code NoiseBasedChunkGenerator}, {@code
 * ChunkGeneratorStructureState} and {@code StructureManager} (structures on, no blending).
 *
 * <p>Worlds ({@link #WORLDS}): overworld seed 0 and seed -7302402383914416, nether seed 42, end
 * seed 1. Every world uses the generator, random state and registries of {@link FeatureGoldens}
 * (registries loaded from the server jar's data pack like a server loads them; the possible
 * structure sets are in registry order). The structure state is {@code
 * generator.createState(registries.lookupOrThrow(STRUCTURE_SET), randomState, seed)} followed by
 * {@code ensureStructuresGenerated()}, as {@code ChunkMap} and {@code ServerLevel} do.
 *
 * <p><b>structure_starts.json</b>: {@code createStructures(registryAccess, state,
 * structureManager, chunk, templateManager, dimension)} on a fresh {@code ProtoChunk} (status
 * empty) for every chunk of the world's area (row-major, z outer) and then for the world's
 * targeted chunks (list order); the {@code StructureManager} only reads and writes the chunk's
 * starts. Every start of each chunk is recorded (sorted by structure id), one per line. The
 * targeted chunks are the first ring of stronghold positions ({@code getRingPositionsFor}) and,
 * where the world has {@code search}, the chunks found by a spiral over the placement grid of
 * each random spread structure set ({@code getPotentialStructureChunk} + {@code
 * isStructureChunk}) for every structure the area lacks (the first chunk where its start
 * generates). The search itself is not part of the golden: only its result chunks are.
 *
 * <p><b>structure_references.json</b>: {@code createReferences} (real {@code
 * WorldGenRegion} access rules of the {@code structure_references} step: starts within 8 chunks)
 * for every chunk of a 16×16 area, recorded as structure id → start chunks in Vanilla's {@code
 * LongOpenHashSet} iteration order (the order {@code startsForStructure} returns starts in).
 *
 * <p><b>structure_terrain.json</b>: the terrain stage of chunks next to starts with terrain
 * adaptation: chunks get starts (radius 9), references (radius 1) and biomes (radius 1) first,
 * then {@code Beardifier.forStructuresInChunk(structureManager, pos)} (manager over the region
 * of the {@code terrain} step) feeds {@code new NoiseChunk(randomState, beardifier, ...)} and the
 * three steps run as in {@link ChunkGoldens} (same snapshot format). The Beardifier sums its
 * contributions in the order of the starts it gets, and that order follows a {@code HashMap} of
 * {@code Structure} objects (identity hash codes) that changes between runs; the starts are
 * recorded sorted (the one target with two adapted structures gave the same blocks either way).
 *
 * <p><b>structure_decoration.json</b>: per area, on fresh chunks: starts (radius 10),
 * references (radius 2), biomes (radius 3), terrain with Beardifier (radius 2), then {@code
 * applyBiomeDecoration(region, chunk, structureManager)} for the 3×3 chunks around the centre in
 * row-major order (z outer), the manager over the {@code features} step region with {@code
 * generateStructures} on. Records all 25 chunks like {@link FeatureGoldens} decorations, block
 * entities with their saved NBT (loot table and seed of chests) and the entities added.
 *
 * <p><b>structure_large.json</b>: whole starts of the large structures ({@link #LARGE_TARGETS}): the
 * first starts of each found by a spiral over its placement grid, each with its pieces (as in
 * structure_starts) and the decoration of every chunk one of its pieces intersects (row-major, on
 * fresh chunks, as in structure_decoration), with the entities each chunk's decoration added.
 *
 * <p>Harness: the {@code WorldGenLevel} is {@link FeatureGoldens}' proxy region with Vanilla's
 * chunk access rule of the step ({@code ChunkPyramid.GENERATION_PYRAMID} direct dependencies by
 * distance). Its fake server level has difficulty {@code easy} (a fresh dedicated server's
 * default; under peaceful monsters would not be created, and some pieces only set the marker
 * block when their mob was created), time 0, the dimension's constant environment attributes,
 * item components bound, and a {@code structureManager()} that reads any chunk of the area (entity
 * variant conditions such as the swamp hut cat use it). Entities are recorded, not stored.
 */
public final class StructureGoldens {
  private record WorldSpec(
      Dimension dimension, long seed, ResourceKey<Level> level, int minX, int minZ, int sizeX, int sizeZ,
      boolean search) {}

  private static final WorldSpec[] WORLDS = {
    new WorldSpec(FeatureGoldens.OVERWORLD, 0L, Level.OVERWORLD, -32, -32, 64, 64, true),
    new WorldSpec(FeatureGoldens.OVERWORLD, -7302402383914416L, Level.OVERWORLD, -16, -16, 32, 32, false),
    new WorldSpec(FeatureGoldens.NETHER, 42L, Level.NETHER, -16, -16, 32, 32, true),
    new WorldSpec(FeatureGoldens.END, 1L, Level.END, 80, -16, 32, 32, true),
  };

  /** Search limits: chunk radius and number of grid cells per structure set. */
  private static final int SEARCH_CHUNK_RADIUS = 4000;
  private static final long SEARCH_MAX_CELLS = 4_000_000L;

  /** References areas: world index and a structure whose first start is the area's centre. */
  private record ReferenceArea(int world, String structure) {}

  private static final ReferenceArea[] REFERENCE_AREAS = {
    new ReferenceArea(0, "minecraft:village_plains"),
    new ReferenceArea(2, "minecraft:fortress"),
  };
  private static final int REFERENCE_RADIUS = 8;

  /** Structures with terrain adaptation whose first start gets terrain goldens. */
  private static final String[] TERRAIN_TARGETS = {
    "minecraft:village_plains", "minecraft:village_desert", "minecraft:village_savanna", "minecraft:village_snowy",
    "minecraft:village_taiga", "minecraft:pillager_outpost", "minecraft:abandoned_camp_forest",
    "minecraft:ancient_city", "minecraft:trial_chambers", "minecraft:trail_ruins", "minecraft:stronghold",
    "minecraft:nether_fossil",
  };

  private StructureGoldens() {}

  // ---------------------------------------------------------------------------------------------
  // Worlds and the chunk pipeline

  private static final ChunkStatus[] REFERENCES_DEPS = dependencies(ChunkStatus.STRUCTURE_REFERENCES);
  private static final ChunkStatus[] TERRAIN_DEPS = dependencies(ChunkStatus.TERRAIN);
  private static final ChunkStatus[] FEATURES_DEPS = dependencies(ChunkStatus.FEATURES);

  /** Chunk access of the server level itself: every chunk the pipeline has, at any status. */
  private static final ChunkStatus[] SERVER_DEPS = new ChunkStatus[64];

  static {
    java.util.Arrays.fill(SERVER_DEPS, ChunkStatus.FULL);
  }

  private static ChunkStatus[] dependencies(ChunkStatus status) {
    return ChunkPyramid.GENERATION_PYRAMID.getStepTo(status).directDependencies().asList().toArray(new ChunkStatus[0]);
  }

  /** A world with its structure state. */
  private static final class SWorld {
    final int index;
    final WorldSpec spec;
    final FeatureGoldens.World world;
    final ChunkGeneratorStructureState state;
    final LevelHeightAccessor height;
    final WorldOptions options;
    /** For {@code createStructures}: only {@code get/setStartForStructure} on the chunk. */
    final StructureManager starter;
    final Registry<Structure> structures;

    SWorld(int index, WorldSpec spec) throws Exception {
      this.index = index;
      this.spec = spec;
      this.world = FeatureGoldens.world(spec.dimension(), spec.seed());
      this.structures = FeatureGoldens.registryAccess.lookupOrThrow(Registries.STRUCTURE);
      this.state = world.generator.createState(
          FeatureGoldens.registryAccess.lookupOrThrow(Registries.STRUCTURE_SET), world.randomState, spec.seed());
      this.state.ensureStructuresGenerated();
      this.height = LevelHeightAccessor.create(spec.dimension().minY(), spec.dimension().height());
      this.options = new WorldOptions(spec.seed(), true, false);
      this.starter = new StructureManager(null, options, null);
    }

    String id(Structure structure) {
      return structures.getKey(structure).toString();
    }

    /** A fresh proto chunk after {@code createStructures}. */
    ProtoChunk starts(ChunkPos pos) {
      ProtoChunk chunk = new ProtoChunk(pos, UpgradeData.EMPTY, height, FeatureGoldens.factory, null);
      world.generator.createStructures(FeatureGoldens.registryAccess, state, starter, chunk, FeatureGoldens.templates,
          spec.level());
      chunk.setPersistedStatus(ChunkStatus.STRUCTURE_STARTS);
      return chunk;
    }
  }

  /** Chunks of one world taken through the generation steps on demand. */
  private static final class Pipeline {
    final SWorld w;
    final Map<Long, ProtoChunk> chunks = new HashMap<>();
    final Method doFill;
    final Method buildSurface;
    final Method generateCarvers;
    final Aquifer.FluidPicker picker;

    Pipeline(SWorld w) throws Exception {
      this.w = w;
      doFill = FeatureGoldens.generatorMethod("doFill", NoiseChunk.class, ChunkAccess.class);
      buildSurface = FeatureGoldens.generatorMethod("buildSurface", ChunkAccess.class, NoiseChunk.class,
          RandomState.class, BiomeManager.class, Set.class, MaterialRule.class);
      generateCarvers = FeatureGoldens.generatorMethod("generateCarvers", ChunkAccess.class, Blender.class,
          NoiseChunk.class, RandomState.class, BiomeManager.class, net.minecraft.server.level.WorldGenRegion.class,
          MaterialRule.class);
      Field pickerField = NoiseBasedChunkGenerator.class.getDeclaredField("globalFluidPicker");
      pickerField.setAccessible(true);
      @SuppressWarnings("unchecked")
      Supplier<Aquifer.FluidPicker> supplier = (Supplier<Aquifer.FluidPicker>) pickerField.get(w.world.generator);
      picker = supplier.get();
    }

    ProtoChunk get(int x, int z) {
      return chunks.get(ChunkPos.pack(x, z));
    }

    boolean reached(ProtoChunk chunk, ChunkStatus status) {
      return chunk != null && chunk.getPersistedStatus().isOrAfter(status);
    }

    ProtoChunk starts(int x, int z) {
      ProtoChunk chunk = get(x, z);
      if (chunk == null) {
        chunk = w.starts(new ChunkPos(x, z));
        chunks.put(ChunkPos.pack(x, z), chunk);
      }
      return chunk;
    }

    ProtoChunk references(int x, int z) {
      ProtoChunk chunk = starts(x, z);
      if (reached(chunk, ChunkStatus.STRUCTURE_REFERENCES)) {
        return chunk;
      }
      for (int dz = -8; dz <= 8; dz++) {
        for (int dx = -8; dx <= 8; dx++) {
          starts(x + dx, z + dz);
        }
      }
      Region region = new Region(w.world, chunks, chunk.getPos(), REFERENCES_DEPS);
      w.world.generator.createReferences(region.proxy, new StructureManager(region.proxy, w.options, null), chunk);
      chunk.setPersistedStatus(ChunkStatus.STRUCTURE_REFERENCES);
      return chunk;
    }

    ProtoChunk biomes(int x, int z) {
      ProtoChunk chunk = references(x, z);
      if (!reached(chunk, ChunkStatus.BIOMES)) {
        w.world.generator.createBiomes(w.world.randomState, Blender.empty(), null, chunk).join();
        chunk.setPersistedStatus(ChunkStatus.BIOMES);
      }
      return chunk;
    }

    /** The terrain step with the Beardifier; snapshots after each step when {@code out} is set. */
    ProtoChunk terrain(int x, int z, JsonObject out) throws Exception {
      ProtoChunk chunk = biomes(x, z);
      if (reached(chunk, ChunkStatus.TERRAIN)) {
        return chunk;
      }
      Set<Holder<Biome>> possibleBiomes = new HashSet<>();
      for (int dz = -1; dz <= 1; dz++) {
        for (int dx = -1; dx <= 1; dx++) {
          biomes(x + dx, z + dz).collectBiomesInPalette(possibleBiomes);
        }
      }
      BiomeManager biomeManager = new BiomeManager(
          (qx, qy, qz) -> chunks.get(ChunkPos.pack(qx >> 2, qz >> 2)).getNoiseBiome(qx, qy, qz),
          BiomeManager.obfuscateSeed(w.spec.seed()));
      ChunkPos pos = chunk.getPos();
      Region region = new Region(w.world, chunks, pos, TERRAIN_DEPS);
      StructureManager structureManager = new StructureManager(region.proxy, w.options, null);
      Beardifier beardifier = Beardifier.forStructuresInChunk(structureManager, pos);
      if (out != null) {
        JsonObject json = new JsonObject();
        TreeMap<String, JsonArray> sorted = new TreeMap<>();
        for (StructureStart start : structureManager.startsForStructure(
            pos.x(), pos.z(), s -> s.terrainAdaptation() != TerrainAdjustment.NONE)) {
          JsonArray a = new JsonArray();
          a.add(w.id(start.getStructure()));
          a.add(start.getChunkPos().x());
          a.add(start.getChunkPos().z());
          sorted.put(w.id(start.getStructure()) + " " + start.getChunkPos().x() + " " + start.getChunkPos().z(), a);
        }
        JsonArray starts = new JsonArray();
        sorted.values().forEach(starts::add);
        json.add("starts", starts);
        beardifierJson(beardifier, json);
        out.add("beardifier", json);
      }
      DensityVolume volume = new DensityVolume(16, w.world.settings.noiseSettings().height(), 16,
          pos.getMinBlockX(), w.world.settings.noiseSettings().minY(), pos.getMinBlockZ());
      MaterialRule materialRule = w.world.settings.materialRule().value();
      try (NoiseChunk noiseChunk =
          new NoiseChunk(w.world.randomState, beardifier, w.world.settings, picker, Blender.empty(), volume)) {
        doFill.invoke(w.world.generator, noiseChunk, chunk);
        if (out != null) {
          out.add("noise", ChunkGoldens.snapshot(chunk));
        }
        buildSurface.invoke(w.world.generator, chunk, noiseChunk, w.world.randomState, biomeManager, possibleBiomes,
            materialRule);
        if (out != null) {
          out.add("surface", ChunkGoldens.snapshot(chunk));
        }
        generateCarvers.invoke(w.world.generator, chunk, Blender.empty(), noiseChunk, w.world.randomState,
            biomeManager, null, materialRule);
        if (out != null) {
          out.add("carvers", ChunkGoldens.snapshot(chunk));
        }
      }
      chunk.setPersistedStatus(ChunkStatus.TERRAIN);
      Heightmap.primeHeightmaps(chunk, FeatureGoldens.FINAL_HEIGHTMAPS);
      return chunk;
    }

    /** {@code applyBiomeDecoration} with structures; returns the entities it added. */
    List<Entity> features(int x, int z) throws Exception {
      for (int dz = -1; dz <= 1; dz++) {
        for (int dx = -1; dx <= 1; dx++) {
          terrain(x + dx, z + dz, null);
        }
      }
      ProtoChunk chunk = get(x, z);
      Region region = new Region(w.world, chunks, chunk.getPos(), FEATURES_DEPS);
      // The server level's own manager (entity variant conditions use it) reads any loaded chunk.
      Region server = new Region(w.world, chunks, chunk.getPos(), SERVER_DEPS);
      w.world.level.structures = new StructureManager(server.proxy, w.options, null);
      try {
        w.world.generator.applyBiomeDecoration(region.proxy, chunk, new StructureManager(region.proxy, w.options, null));
      } finally {
        w.world.level.structures = null;
      }
      chunk.setPersistedStatus(ChunkStatus.FEATURES);
      return region.entities;
    }
  }

  // ---------------------------------------------------------------------------------------------
  // Entry

  /** A start found in the starts part: world, chunk and structure. */
  private record Found(SWorld world, ChunkPos chunk, String structure, StructureStart start) {}

  public static Map<String, JsonObject> extractFiles() throws Exception {
    FeatureGoldens.init();
    StructurePieceSerializationContext context = new StructurePieceSerializationContext(
        FeatureGoldens.resources, FeatureGoldens.registryAccess, FeatureGoldens.templates);
    List<SWorld> worlds = new ArrayList<>();
    for (int i = 0; i < WORLDS.length; i++) {
      worlds.add(new SWorld(i, WORLDS[i]));
    }
    Map<String, JsonObject> files = new LinkedHashMap<>();
    List<Found> found = new ArrayList<>();

    long start = System.nanoTime();
    files.put("structure_starts", starts(worlds, context, found));
    System.out.printf("structure goldens: starts in %.1f s%n", (System.nanoTime() - start) / 1e9);
    Map<String, Integer> counts = new TreeMap<>();
    for (Found f : found) {
      counts.merge(f.structure(), 1, Integer::sum);
    }
    System.out.println("structure goldens: starts per structure " + counts);

    start = System.nanoTime();
    files.put("structure_references", references(worlds, found));
    System.out.printf("structure goldens: references in %.1f s%n", (System.nanoTime() - start) / 1e9);

    start = System.nanoTime();
    files.put("structure_terrain", terrain(worlds, found));
    System.out.printf("structure goldens: terrain in %.1f s%n", (System.nanoTime() - start) / 1e9);

    start = System.nanoTime();
    files.put("structure_decoration", decoration(found));
    System.out.printf("structure goldens: decoration in %.1f s%n", (System.nanoTime() - start) / 1e9);

    start = System.nanoTime();
    files.put("structure_large", large(worlds, context));
    System.out.printf("structure goldens: large structures in %.1f s%n", (System.nanoTime() - start) / 1e9);
    return files;
  }

  private static Found first(List<Found> found, String structure) {
    return found.stream().filter(f -> f.structure().equals(structure)).findFirst()
        .orElseThrow(() -> new IllegalStateException("no start of " + structure + " found"));
  }

  private static JsonObject worldJson(SWorld w) {
    JsonObject o = new JsonObject();
    o.addProperty("dimension", w.spec.dimension().name());
    o.addProperty("seed", w.spec.seed());
    o.addProperty("min_y", w.spec.dimension().minY());
    o.addProperty("height", w.spec.dimension().height());
    return o;
  }

  // ---------------------------------------------------------------------------------------------
  // Part 1: starts

  private static JsonObject starts(List<SWorld> worlds, StructurePieceSerializationContext context, List<Found> found)
      throws Exception {
    JsonArray worldsJson = new JsonArray();
    JsonArray starts = new JsonArray();
    for (SWorld w : worlds) {
      WorldSpec spec = w.spec;
      JsonObject o = worldJson(w);
      ChunkPos origin = w.state.getDimensionOrigin();
      o.add("origin", FeatureGoldens.array(origin.x(), origin.z()));
      o.add("area", FeatureGoldens.array(spec.minX(), spec.minZ(), spec.sizeX(), spec.sizeZ()));
      Set<String> seen = new HashSet<>();
      for (int z = spec.minZ(); z < spec.minZ() + spec.sizeZ(); z++) {
        for (int x = spec.minX(); x < spec.minX() + spec.sizeX(); x++) {
          record(w, w.starts(new ChunkPos(x, z)), context, starts, found, seen);
        }
      }
      System.out.println("structure goldens: " + spec.dimension().name() + " seed " + spec.seed() + " area done");

      List<ChunkPos> targeted = new ArrayList<>();
      JsonElement rings = JsonNull.INSTANCE;
      for (Holder<StructureSet> set : w.state.possibleStructureSets()) {
        if (set.value().placement() instanceof ConcentricRingsStructurePlacement ringsPlacement) {
          List<ChunkPos> positions = w.state.getRingPositionsFor(ringsPlacement);
          JsonArray r = new JsonArray();
          for (ChunkPos p : positions) {
            r.add(FeatureGoldens.array(p.x(), p.z()));
          }
          rings = r;
          for (int i = 0; i < Math.min(ringsPlacement.spread(), positions.size()); i++) {
            ChunkPos p = positions.get(i);
            if (!inArea(spec, p) && !targeted.contains(p)) {
              targeted.add(p);
              record(w, w.starts(p), context, starts, found, seen);
            }
          }
        }
      }
      o.add("stronghold_rings", rings);
      if (spec.search()) {
        search(w, context, starts, found, seen, targeted);
      }
      JsonArray t = new JsonArray();
      for (ChunkPos p : targeted) {
        t.add(FeatureGoldens.array(p.x(), p.z()));
      }
      o.add("chunks", t);
      Set<String> missing = new TreeSet<>(possibleStructures(w));
      missing.removeAll(seen);
      if (!missing.isEmpty()) {
        System.out.println("structure goldens: no start found in " + spec.dimension().name() + " seed " + spec.seed()
            + " for " + missing);
      }
      worldsJson.add(o);
    }
    JsonObject root = new JsonObject();
    root.addProperty("format", STARTS_FORMAT);
    root.add("worlds", worldsJson);
    root.add("starts", starts);
    return root;
  }

  private static final String STARTS_FORMAT = "Vanilla 26.3 structure starts. worlds[i]: dimension, seed, min_y, "
      + "height, origin (ChunkGeneratorStructureState.getDimensionOrigin), area [min_chunk_x, min_chunk_z, size_x, "
      + "size_z], stronghold_rings (getRingPositionsFor of the stronghold placement, all positions, null if none), "
      + "chunks (targeted chunks). For every chunk of the area (z outer, x inner) and then every targeted chunk, "
      + "ChunkGenerator.createStructures ran on a fresh ProtoChunk (state = generator.createState(structure sets "
      + "in registry order, randomState, seed) after ensureStructuresGenerated). starts: every start of those chunks "
      + "in that chunk order, per chunk sorted by structure id; one per line: {w: world index, structure, chunk: "
      + "[x, z], references, bb: [min_x, min_y, min_z, max_x, max_y, max_z] (StructureStart.getBoundingBox), "
      + "pieces: [[piece type id, bb, orientation (direction name or null), gen_depth, rotation, mirror "
      + "(StructurePiece.getRotation/getMirror, enum constant names such as CLOCKWISE_90 and LEFT_RIGHT: the placement settings of template pieces, the "
      + "jigsaw rotation of pool element pieces, else derived from the orientation; null without orientation), "
      + "data]]} in StructureStart.getPieces order. data is "
      + "the piece's NBT (StructurePiece.createTag) without id, BB, O and GD, keys sorted, numbers as JSON numbers "
      + "(e.g. Template/TPX/TPY/TPZ of template pieces; pool_element {element_type, location, processors, "
      + "projection}, PosX/PosY/PosZ, rotation, ground_level_delta, junctions of jigsaw pieces). A chunk of the "
      + "area or targeted list without entry has no start.";

  private static boolean inArea(WorldSpec spec, ChunkPos p) {
    return p.x() >= spec.minX() && p.x() < spec.minX() + spec.sizeX() && p.z() >= spec.minZ()
        && p.z() < spec.minZ() + spec.sizeZ();
  }

  private static void record(
      SWorld w, ProtoChunk chunk, StructurePieceSerializationContext context, JsonArray out, List<Found> found,
      Set<String> seen) {
    TreeMap<String, StructureStart> sorted = new TreeMap<>();
    for (Map.Entry<Structure, StructureStart> e : chunk.getAllStarts().entrySet()) {
      sorted.put(w.id(e.getKey()), e.getValue());
    }
    for (Map.Entry<String, StructureStart> e : sorted.entrySet()) {
      StructureStart start = e.getValue();
      JsonObject o = new JsonObject();
      o.addProperty("w", w.index);
      o.addProperty("structure", e.getKey());
      o.add("chunk", FeatureGoldens.array(start.getChunkPos().x(), start.getChunkPos().z()));
      o.addProperty("references", start.getReferences());
      o.add("bb", box(start.getBoundingBox()));
      JsonArray pieces = new JsonArray();
      for (StructurePiece piece : start.getPieces()) {
        pieces.add(piece(piece, context));
      }
      o.add("pieces", pieces);
      out.add(o);
      found.add(new Found(w, chunk.getPos(), e.getKey(), start));
      seen.add(e.getKey());
    }
  }

  private static JsonArray piece(StructurePiece piece, StructurePieceSerializationContext context) {
    JsonArray p = new JsonArray();
    p.add(BuiltInRegistries.STRUCTURE_PIECE.getKey(piece.getType()).toString());
    p.add(box(piece.getBoundingBox()));
    Direction orientation = piece.getOrientation();
    p.add(orientation == null ? JsonNull.INSTANCE : new JsonPrimitive(orientation.getSerializedName()));
    p.add(piece.getGenDepth());
    p.add(piece.getRotation() == null ? JsonNull.INSTANCE : new JsonPrimitive(piece.getRotation().name()));
    p.add(piece.getMirror() == null ? JsonNull.INSTANCE : new JsonPrimitive(piece.getMirror().name()));
    CompoundTag tag = piece.createTag(context);
    for (String key : new String[] {"id", "BB", "O", "GD"}) {
      tag.remove(key);
    }
    p.add(nbt(tag));
    return p;
  }

  static JsonArray box(BoundingBox box) {
    return FeatureGoldens.array(box.minX(), box.minY(), box.minZ(), box.maxX(), box.maxY(), box.maxZ());
  }

  /** NBT as JSON: compounds with sorted keys, numbers and arrays as JSON numbers. */
  static JsonElement nbt(Tag tag) {
    return switch (tag) {
      case CompoundTag c -> {
        JsonObject o = new JsonObject();
        for (String key : new TreeSet<>(c.keySet())) {
          o.add(key, nbt(c.get(key)));
        }
        yield o;
      }
      case ListTag l -> {
        JsonArray a = new JsonArray();
        for (Tag t : l) {
          a.add(nbt(t));
        }
        yield a;
      }
      case ByteTag b -> new JsonPrimitive(b.value());
      case ShortTag s -> new JsonPrimitive(s.value());
      case IntTag i -> new JsonPrimitive(i.value());
      case LongTag l -> new JsonPrimitive(l.value());
      case FloatTag f -> new JsonPrimitive(f.value());
      case DoubleTag d -> new JsonPrimitive(d.value());
      case StringTag s -> new JsonPrimitive(s.value());
      case ByteArrayTag a -> {
        JsonArray out = new JsonArray();
        for (byte v : a.getAsByteArray()) {
          out.add(v);
        }
        yield out;
      }
      case IntArrayTag a -> {
        JsonArray out = new JsonArray();
        for (int v : a.getAsIntArray()) {
          out.add(v);
        }
        yield out;
      }
      case LongArrayTag a -> {
        JsonArray out = new JsonArray();
        for (long v : a.getAsLongArray()) {
          out.add(v);
        }
        yield out;
      }
      default -> throw new IllegalArgumentException("unknown tag " + tag);
    };
  }

  /** Structures of the world's possible structure sets that one of its biomes allows. */
  private static Set<String> possibleStructures(SWorld w) {
    Set<Holder<Biome>> biomes = w.world.generator.getBiomeSource().possibleBiomes();
    Set<String> out = new TreeSet<>();
    for (Holder<StructureSet> set : w.state.possibleStructureSets()) {
      for (StructureSet.StructureSelectionEntry entry : set.value().structures()) {
        if (entry.structure().value().biomes().stream().anyMatch(biomes::contains)) {
          out.add(w.id(entry.structure().value()));
        }
      }
    }
    return out;
  }

  /**
   * Adds targeted chunks for the structures the area lacks: per random spread set with such a
   * structure, a spiral over its grid cells around the origin; a cell's potential chunk that
   * passes {@code isStructureChunk} (and, to save time, a biome pre-check at its centre column)
   * runs {@code createStructures}; the first chunk with a start of a missing structure is kept.
   */
  private static void search(
      SWorld w, StructurePieceSerializationContext context, JsonArray starts, List<Found> found, Set<String> seen,
      List<ChunkPos> targeted) {
    Set<String> possible = possibleStructures(w);
    Set<Long> tried = new HashSet<>();
    for (int z = w.spec.minZ(); z < w.spec.minZ() + w.spec.sizeZ(); z++) {
      for (int x = w.spec.minX(); x < w.spec.minX() + w.spec.sizeX(); x++) {
        tried.add(ChunkPos.pack(x, z));
      }
    }
    for (ChunkPos p : targeted) {
      tried.add(p.pack());
    }
    for (Holder<StructureSet> set : w.state.possibleStructureSets()) {
      if (!(set.value().placement() instanceof RandomSpreadStructurePlacement placement)) {
        continue;
      }
      List<Structure> wanted = new ArrayList<>();
      for (StructureSet.StructureSelectionEntry entry : set.value().structures()) {
        String id = w.id(entry.structure().value());
        if (possible.contains(id) && !seen.contains(id)) {
          wanted.add(entry.structure().value());
        }
      }
      if (wanted.isEmpty()) {
        continue;
      }
      long started = System.nanoTime();
      int spacing = placement.spacing();
      int radius = Math.max(1, Math.min(SEARCH_CHUNK_RADIUS / spacing, (int) (Math.sqrt(SEARCH_MAX_CELLS) / 2)));
      int runs = 0;
      search:
      for (int r = 0; r <= radius; r++) {
        for (int gz = -r; gz <= r; gz++) {
          for (int gx = -r; gx <= r; gx++) {
            if (Math.max(Math.abs(gx), Math.abs(gz)) != r) {
              continue;
            }
            ChunkPos p = placement.getPotentialStructureChunk(w.spec.seed(), gx * spacing, gz * spacing);
            if (tried.contains(p.pack()) || !placement.isStructureChunk(w.state, p.x(), p.z())) {
              continue;
            }
            wanted.removeIf(s -> seen.contains(w.id(s)));
            if (wanted.isEmpty()) {
              break search;
            }
            if (!biomeMatch(w, p, wanted)) {
              continue;
            }
            tried.add(p.pack());
            runs++;
            ProtoChunk chunk = w.starts(p);
            boolean hit = chunk.getAllStarts().keySet().stream().anyMatch(s -> wanted.contains(s));
            if (hit) {
              targeted.add(p);
              record(w, chunk, context, starts, found, seen);
            }
          }
        }
      }
      wanted.removeIf(s -> seen.contains(w.id(s)));
      System.out.printf("structure goldens: search %s: %d runs in %.1f s%s%n", set.unwrapKey().orElseThrow()
          .identifier(), runs, (System.nanoTime() - started) / 1e9,
          wanted.isEmpty() ? "" : ", not found: " + wanted.stream().map(w::id).toList());
    }
  }

  /** Whether a biome of one of the structures is at the chunk's centre column (every 8 blocks). */
  private static boolean biomeMatch(SWorld w, ChunkPos p, List<Structure> wanted) {
    int qx = QuartPos.fromBlock(p.getMiddleBlockX());
    int qz = QuartPos.fromBlock(p.getMiddleBlockZ());
    int minY = w.spec.dimension().minY();
    for (int y = minY; y < minY + w.spec.dimension().height(); y += 8) {
      Holder<Biome> biome = w.world.biomes.getNoiseBiome(qx, QuartPos.fromBlock(y), qz);
      for (Structure s : wanted) {
        if (s.biomes().contains(biome)) {
          return true;
        }
      }
    }
    return false;
  }

  // ---------------------------------------------------------------------------------------------
  // Part 2: references

  private static JsonObject references(List<SWorld> worlds, List<Found> found) throws Exception {
    JsonArray areas = new JsonArray();
    JsonArray chunks = new JsonArray();
    for (int a = 0; a < REFERENCE_AREAS.length; a++) {
      ReferenceArea spec = REFERENCE_AREAS[a];
      SWorld w = worlds.get(spec.world());
      ChunkPos center = found.stream().filter(f -> f.world() == w && f.structure().equals(spec.structure()))
          .findFirst().orElseThrow(() -> new IllegalStateException("no " + spec.structure())).chunk();
      JsonObject o = worldJson(w);
      o.addProperty("structure", spec.structure());
      int minX = center.x() - REFERENCE_RADIUS;
      int minZ = center.z() - REFERENCE_RADIUS;
      o.add("area", FeatureGoldens.array(minX, minZ, 2 * REFERENCE_RADIUS, 2 * REFERENCE_RADIUS));
      areas.add(o);
      Pipeline pipeline = new Pipeline(w);
      for (int z = minZ; z < minZ + 2 * REFERENCE_RADIUS; z++) {
        for (int x = minX; x < minX + 2 * REFERENCE_RADIUS; x++) {
          ProtoChunk chunk = pipeline.references(x, z);
          JsonObject c = new JsonObject();
          c.addProperty("a", a);
          c.add("chunk", FeatureGoldens.array(x, z));
          c.add("references", referencesJson(w, chunk));
          chunks.add(c);
        }
      }
    }
    JsonObject root = new JsonObject();
    root.addProperty("format", "Vanilla 26.3 structure references. areas[i]: dimension, seed, min_y, height, "
        + "structure (the area is centred on the chunk of its first start in structure_starts), area [min_chunk_x, "
        + "min_chunk_z, size_x, size_z]. chunks: every chunk of each area (z outer, x inner), one per line: {a: area "
        + "index, chunk: [x, z], references: {structure id (sorted): [[start chunk x, z], ...]}} after "
        + "ChunkGenerator.createReferences on the chunk (starts from createStructures on every chunk within 8). The "
        + "start chunks are in the iteration order of Vanilla's LongOpenHashSet (default capacity, keys "
        + "ChunkPos.pack inserted by createReferences with source x outer, z inner), which is the order "
        + "StructureManager.startsForStructure returns their starts in.");
    root.add("areas", areas);
    root.add("chunks", chunks);
    return root;
  }

  private static JsonObject referencesJson(SWorld w, ChunkAccess chunk) {
    TreeMap<String, List<ChunkPos>> sorted = new TreeMap<>();
    for (Map.Entry<Structure, LongSet> e : chunk.getAllReferences().entrySet()) {
      List<ChunkPos> list = new ArrayList<>();
      e.getValue().forEach(k -> list.add(ChunkPos.unpack(k)));
      sorted.put(w.id(e.getKey()), list);
    }
    JsonObject o = new JsonObject();
    for (Map.Entry<String, List<ChunkPos>> e : sorted.entrySet()) {
      JsonArray a = new JsonArray();
      for (ChunkPos p : e.getValue()) {
        a.add(FeatureGoldens.array(p.x(), p.z()));
      }
      o.add(e.getKey(), a);
    }
    return o;
  }

  // ---------------------------------------------------------------------------------------------
  // Part 3: terrain with Beardifier

  private static JsonObject terrain(List<SWorld> worlds, List<Found> found) throws Exception {
    Map<SWorld, Pipeline> pipelines = new HashMap<>();
    JsonArray chunks = new JsonArray();
    for (String structure : TERRAIN_TARGETS) {
      Found f = first(found, structure);
      Pipeline pipeline = pipelines.get(f.world());
      if (pipeline == null) {
        pipeline = new Pipeline(f.world());
        pipelines.put(f.world(), pipeline);
      }
      StructurePiece main = f.start().getPieces().get(0);
      BlockPos center = main.getBoundingBox().getCenter();
      BoundingBox all = f.start().getBoundingBox();
      Set<ChunkPos> targets = new LinkedHashSet<>();
      targets.add(new ChunkPos(center.getX() >> 4, center.getZ() >> 4));
      targets.add(new ChunkPos((center.getX() >> 4) - 1, center.getZ() >> 4));
      targets.add(new ChunkPos((all.minX() - 1) >> 4, center.getZ() >> 4));
      for (ChunkPos pos : targets) {
        JsonObject o = worldJson(f.world());
        o.addProperty("structure", structure);
        o.add("start", FeatureGoldens.array(f.chunk().x(), f.chunk().z()));
        o.addProperty("x", pos.x());
        o.addProperty("z", pos.z());
        if (pipeline.reached(pipeline.get(pos.x(), pos.z()), ChunkStatus.TERRAIN)) {
          throw new IllegalStateException("terrain target " + pos + " generated twice");
        }
        ProtoChunk chunk = pipeline.biomes(pos.x(), pos.z());
        o.add("references", referencesJson(f.world(), chunk));
        pipeline.terrain(pos.x(), pos.z(), o);
        chunks.add(o);
      }
    }
    JsonObject root = new JsonObject();
    root.addProperty("format", "Vanilla 26.3 terrain with the Beardifier. chunks: one per line: dimension, seed, "
        + "min_y, height, structure and start [chunk x, z] (the first start of that structure in "
        + "structure_starts; the targets, all at the z of the centre of its first piece: the chunk of that centre, the "
        + "chunk west of it and the chunk of the block west of the start's bounding box, without repeats), x, z, "
        + "references (as in structure_references), beardifier {starts: [[structure id, start chunk x, z], ...] "
        + "that Beardifier.forStructuresInChunk got from StructureManager.startsForStructure, sorted (Vanilla's "
        + "order follows a HashMap of Structure objects and changes between JVM runs; the float sum of the "
        + "contributions follows it, but the recorded chunks gave the same blocks in either order), rigids, "
        + "junctions: number of entries, affected_box or null}, then noise/surface/carvers snapshots in the format of chunk.json, taken after doFill, "
        + "buildSurface and generateCarvers of a NoiseChunk built with that Beardifier (no blending). Starts come "
        + "from createStructures on fresh chunks; the references of the chunk from createReferences.");
    root.add("chunks", chunks);
    return root;
  }

  /** Sizes of the Beardifier's rigid pieces and junctions and its affected box. */
  private static void beardifierJson(Beardifier beardifier, JsonObject o) throws Exception {
    for (String name : new String[] {"pieces", "junctions"}) {
      Field f = Beardifier.class.getDeclaredField(name);
      f.setAccessible(true);
      o.addProperty(name.equals("pieces") ? "rigids" : name, ((List<?>) f.get(beardifier)).size());
    }
    Field f = Beardifier.class.getDeclaredField("affectedBox");
    f.setAccessible(true);
    BoundingBox box = (BoundingBox) f.get(beardifier);
    o.add("affected_box", box == null ? JsonNull.INSTANCE : box(box));
  }

  // ---------------------------------------------------------------------------------------------
  // Part 4: decoration with structures

  private static JsonObject decoration(List<Found> found) throws Exception {
    List<Found> targets = new ArrayList<>();
    Set<String> done = new HashSet<>();
    for (Found f : found) {
      if (done.add(f.structure())) {
        targets.add(f);
      }
    }
    JsonArray areas = new JsonArray();
    for (Found f : targets) {
      long started = System.nanoTime();
      ChunkPos center = decorationCenter(f.start());
      Pipeline pipeline = new Pipeline(f.world());
      JsonObject o = worldJson(f.world());
      o.addProperty("structure", f.structure());
      o.add("start", FeatureGoldens.array(f.chunk().x(), f.chunk().z()));
      o.addProperty("x", center.x());
      o.addProperty("z", center.z());
      for (int dz = -2; dz <= 2; dz++) {
        for (int dx = -2; dx <= 2; dx++) {
          pipeline.terrain(center.x() + dx, center.z() + dz, null);
        }
      }
      JsonArray entities = new JsonArray();
      int index = 0;
      for (int dz = -1; dz <= 1; dz++) {
        for (int dx = -1; dx <= 1; dx++) {
          for (Entity e : pipeline.features(center.x() + dx, center.z() + dz)) {
            JsonArray a = new JsonArray();
            a.add(index);
            a.add(BuiltInRegistries.ENTITY_TYPE.getKey(e.getType()).toString());
            a.add(e.getX());
            a.add(e.getY());
            a.add(e.getZ());
            entities.add(a);
          }
          index++;
        }
      }
      JsonArray list = new JsonArray();
      for (ProtoChunk chunk : FeatureGoldens.ordered(pipeline.chunks, center, 2)) {
        JsonObject c = Snapshot.of(chunk).toJson();
        c.add("block_entities", blockEntities(chunk));
        long[] ticks = FeatureGoldens.blockTicks(chunk);
        c.add("block_ticks", FeatureGoldens.array(ticks[0], ticks[1]));
        ticks = FeatureGoldens.fluidTicks(chunk);
        c.add("fluid_ticks", FeatureGoldens.array(ticks[0], ticks[1]));
        list.add(c);
      }
      o.add("chunks", list);
      o.add("entities", entities);
      areas.add(o);
      System.out.printf("structure goldens: decoration %s at %s in %.1f s%n", f.structure(), center,
          (System.nanoTime() - started) / 1e9);
    }
    JsonObject root = new JsonObject();
    root.addProperty("format", "Vanilla 26.3 decoration with structures. areas: one per line, one per structure "
        + "id (its first start in structure_starts): dimension, seed, min_y, height, structure, start [chunk x, z], "
        + "x, z (centre chunk: of the chunks intersecting a piece of the start, the one whose 3x3 area intersects "
        + "the most distinct piece types; ties: nearest to the start chunk, then smaller z, then smaller x). On "
        + "fresh chunks: createStructures (all chunks within 10 of the centre), createReferences (within 2), "
        + "biomes, terrain with the Beardifier (within 2, as structure_terrain), then applyBiomeDecoration with "
        + "generateStructures on for the 3x3 chunks around the centre in row-major order (z outer), each with a "
        + "WorldGenRegion of the features step (writes within 1 chunk, starts read within 8). chunks: the 5x5 "
        + "chunks around the centre (z outer) in the format of feature.json decorations (sections, heightmaps "
        + "WORLD_SURFACE/OCEAN_FLOOR/MOTION_BLOCKING/MOTION_BLOCKING_NO_LEAVES, post_processing, block_ticks, "
        + "fluid_ticks) with block_entities [x, y, z, type id, nbt] sorted by y, z, x where nbt is "
        + "BlockEntity.saveWithoutMetadata as JSON (keys sorted; chests carry LootTable and LootTableSeed) or null "
        + "for a block entity that was never created (pending DUMMY tag, id then 'DUMMY'). entities: [index of the "
        + "decorated chunk (0-8, row-major), entity type id, x, y, z] in the order addFreshEntity got them.");
    root.add("areas", areas);
    return root;
  }

  /**
   * The chunk intersecting a piece of the start whose 3×3 area intersects the most distinct
   * piece types (ties: nearest the start chunk, then smaller z, then smaller x).
   */
  private static ChunkPos decorationCenter(StructureStart start) {
    Set<ChunkPos> candidates = new HashSet<>();
    for (StructurePiece piece : start.getPieces()) {
      BoundingBox b = piece.getBoundingBox();
      for (int z = b.minZ() >> 4; z <= b.maxZ() >> 4; z++) {
        for (int x = b.minX() >> 4; x <= b.maxX() >> 4; x++) {
          candidates.add(new ChunkPos(x, z));
        }
      }
    }
    ChunkPos origin = start.getChunkPos();
    ChunkPos best = null;
    int bestScore = -1;
    List<ChunkPos> ordered = new ArrayList<>(candidates);
    ordered.sort(Comparator.<ChunkPos>comparingInt(p -> p.getChessboardDistance(origin)).thenComparingInt(ChunkPos::z)
        .thenComparingInt(ChunkPos::x));
    for (ChunkPos p : ordered) {
      int minX = (p.x() - 1) << 4;
      int minZ = (p.z() - 1) << 4;
      Set<String> types = new HashSet<>();
      for (StructurePiece piece : start.getPieces()) {
        if (piece.getBoundingBox().intersects(minX, minZ, minX + 47, minZ + 47)) {
          types.add(BuiltInRegistries.STRUCTURE_PIECE.getKey(piece.getType()).toString());
        }
      }
      if (types.size() > bestScore) {
        bestScore = types.size();
        best = p;
      }
    }
    return best;
  }

  // ---------------------------------------------------------------------------------------------
  // Part 5: whole starts of large structures

  /** A large structure: world index, structure id and how many of its starts to record. */
  private record LargeTarget(int world, String structure, int count) {}

  private static final LargeTarget[] LARGE_TARGETS = {
    new LargeTarget(0, "minecraft:monument", 3),
    new LargeTarget(0, "minecraft:mansion", 2),
    new LargeTarget(3, "minecraft:end_city", 4),
  };

  private static JsonObject large(List<SWorld> worlds, StructurePieceSerializationContext context) throws Exception {
    JsonArray starts = new JsonArray();
    JsonArray chunks = new JsonArray();
    for (LargeTarget target : LARGE_TARGETS) {
      SWorld w = worlds.get(target.world());
      for (ChunkPos startChunk : largeStarts(w, target)) {
        long started = System.nanoTime();
        Pipeline pipeline = new Pipeline(w);
        StructureStart start = pipeline.starts(startChunk.x(), startChunk.z()).getAllStarts().entrySet().stream()
            .filter(e -> w.id(e.getKey()).equals(target.structure())).findFirst().orElseThrow().getValue();
        TreeSet<ChunkPos> decorated = new TreeSet<>(
            Comparator.<ChunkPos>comparingInt(ChunkPos::z).thenComparingInt(ChunkPos::x));
        for (StructurePiece piece : start.getPieces()) {
          BoundingBox b = piece.getBoundingBox();
          for (int z = b.minZ() >> 4; z <= b.maxZ() >> 4; z++) {
            for (int x = b.minX() >> 4; x <= b.maxX() >> 4; x++) {
              decorated.add(new ChunkPos(x, z));
            }
          }
        }
        JsonObject o = worldJson(w);
        o.addProperty("w", w.index);
        o.addProperty("structure", target.structure());
        o.add("chunk", FeatureGoldens.array(startChunk.x(), startChunk.z()));
        o.addProperty("references", start.getReferences());
        o.add("bb", box(start.getBoundingBox()));
        JsonArray pieces = new JsonArray();
        for (StructurePiece piece : start.getPieces()) {
          pieces.add(piece(piece, context));
        }
        o.add("pieces", pieces);
        JsonArray decoratedJson = new JsonArray();
        decorated.forEach(p -> decoratedJson.add(FeatureGoldens.array(p.x(), p.z())));
        o.add("decorated", decoratedJson);
        int index = starts.size();
        starts.add(o);
        for (ChunkPos pos : decorated) {
          JsonArray entities = new JsonArray();
          for (Entity e : pipeline.features(pos.x(), pos.z())) {
            JsonArray a = new JsonArray();
            a.add(BuiltInRegistries.ENTITY_TYPE.getKey(e.getType()).toString());
            a.add(e.getX());
            a.add(e.getY());
            a.add(e.getZ());
            entities.add(a);
          }
          ProtoChunk chunk = pipeline.get(pos.x(), pos.z());
          JsonObject c = new JsonObject();
          c.addProperty("s", index);
          c.add("chunk", FeatureGoldens.array(pos.x(), pos.z()));
          Snapshot.of(chunk).toJson().entrySet().forEach(e -> c.add(e.getKey(), e.getValue()));
          c.add("block_entities", blockEntities(chunk));
          long[] ticks = FeatureGoldens.blockTicks(chunk);
          c.add("block_ticks", FeatureGoldens.array(ticks[0], ticks[1]));
          ticks = FeatureGoldens.fluidTicks(chunk);
          c.add("fluid_ticks", FeatureGoldens.array(ticks[0], ticks[1]));
          c.add("entities", entities);
          chunks.add(c);
        }
        System.out.printf("structure goldens: large %s at %s, %d chunks in %.1f s%n", target.structure(), startChunk,
            decorated.size(), (System.nanoTime() - started) / 1e9);
      }
    }
    JsonObject root = new JsonObject();
    root.addProperty("format", "Vanilla 26.3 whole starts of large structures. starts: one per line, for each "
        + "target (world 0 monument x3, mansion x2; world 3 end_city x4) its first starts found by a spiral over the "
        + "grid cells of its random spread structure set around the origin (cells whose potential chunk passes "
        + "isStructureChunk and has a biome of the structure in its middle column, then createStructures): "
        + "dimension, seed, min_y, height, w (world index of structure_starts), structure, chunk, references, bb, "
        + "pieces (as in structure_starts), decorated (every chunk a piece's box intersects, z outer then x). Per "
        + "start on fresh chunks, applyBiomeDecoration with structures ran for the decorated chunks in that order "
        + "(each chunk's dependencies first, as in structure_decoration: starts within 8 of the referenced chunks, "
        + "references, biomes, terrain with the Beardifier). chunks: one per line per decorated chunk: s (start "
        + "index), chunk [x, z], the structure_decoration chunk format (sections, heightmaps, post_processing, "
        + "block_entities, block_ticks, fluid_ticks) and entities [[type id, x, y, z], ...] that its decoration "
        + "added, in addFreshEntity order.");
    root.add("starts", starts);
    root.add("chunks", chunks);
    return root;
  }

  /**
   * The first {@code target.count()} chunks with a start of the target structure: a spiral over
   * the grid cells of its random spread set around the origin (potential chunk, {@code
   * isStructureChunk}, a biome pre-check at the middle column, then {@code createStructures}).
   */
  private static List<ChunkPos> largeStarts(SWorld w, LargeTarget target) {
    for (Holder<StructureSet> set : w.state.possibleStructureSets()) {
      if (!(set.value().placement() instanceof RandomSpreadStructurePlacement placement)) {
        continue;
      }
      List<Structure> wanted = set.value().structures().stream().map(e -> e.structure().value())
          .filter(s -> w.id(s).equals(target.structure())).toList();
      if (wanted.isEmpty()) {
        continue;
      }
      List<ChunkPos> out = new ArrayList<>();
      int spacing = placement.spacing();
      int radius = Math.max(1, Math.min(SEARCH_CHUNK_RADIUS / spacing, (int) (Math.sqrt(SEARCH_MAX_CELLS) / 2)));
      for (int r = 0; r <= radius; r++) {
        for (int gz = -r; gz <= r; gz++) {
          for (int gx = -r; gx <= r; gx++) {
            if (Math.max(Math.abs(gx), Math.abs(gz)) != r) {
              continue;
            }
            ChunkPos p = placement.getPotentialStructureChunk(w.spec.seed(), gx * spacing, gz * spacing);
            if (!placement.isStructureChunk(w.state, p.x(), p.z()) || !biomeMatch(w, p, wanted)) {
              continue;
            }
            if (w.starts(p).getAllStarts().keySet().stream().anyMatch(wanted::contains)) {
              out.add(p);
              if (out.size() == target.count()) {
                return out;
              }
            }
          }
        }
      }
      throw new IllegalStateException("only " + out.size() + " starts of " + target.structure() + " found");
    }
    throw new IllegalStateException("no random spread set with " + target.structure());
  }

  /** Block entities [x, y, z, type id, saved NBT or null] sorted by y, z, x. */
  private static JsonArray blockEntities(ProtoChunk chunk) {
    List<BlockPos> positions = new ArrayList<>(chunk.getBlockEntitiesPos());
    positions.sort(Comparator.<BlockPos>comparingInt(BlockPos::getY).thenComparingInt(BlockPos::getZ)
        .thenComparingInt(BlockPos::getX));
    JsonArray out = new JsonArray();
    for (BlockPos pos : positions) {
      BlockEntity entity = chunk.getBlockEntity(pos);
      JsonArray e = new JsonArray();
      e.add(pos.getX());
      e.add(pos.getY());
      e.add(pos.getZ());
      if (entity != null) {
        e.add(BuiltInRegistries.BLOCK_ENTITY_TYPE.getKey(entity.getType()).toString());
        e.add(nbt(entity.saveWithoutMetadata(FeatureGoldens.registryAccess)));
      } else {
        e.add(chunk.getBlockEntityNbt(pos).getStringOr("id", ""));
        e.add(JsonNull.INSTANCE);
      }
      out.add(e);
    }
    return out;
  }
}

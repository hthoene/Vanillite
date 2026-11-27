// Copyright Hannes Thöne. Licensed under the Apache License, Version 2.0.
package dev.vanillite.extractor.golden;

import com.google.gson.Gson;
import com.google.gson.GsonBuilder;
import com.google.gson.JsonArray;
import com.google.gson.JsonNull;
import com.google.gson.JsonObject;
import com.google.gson.JsonPrimitive;
import dev.vanillite.extractor.golden.FeatureGoldens.Dimension;
import dev.vanillite.extractor.golden.FeatureGoldens.Region;
import dev.vanillite.extractor.golden.FeatureGoldens.World;
import it.unimi.dsi.fastutil.longs.LongSet;
import java.io.BufferedOutputStream;
import java.io.DataOutputStream;
import java.io.OutputStream;
import java.lang.reflect.Field;
import java.lang.reflect.Method;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.Comparator;
import java.util.HashMap;
import java.util.HashSet;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.TreeMap;
import java.util.function.Supplier;
import java.util.zip.GZIPOutputStream;
import net.minecraft.SharedConstants;
import net.minecraft.core.BlockPos;
import net.minecraft.core.Holder;
import net.minecraft.core.Registry;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.core.registries.Registries;
import net.minecraft.resources.ResourceKey;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.ChunkPos;
import net.minecraft.world.level.Level;
import net.minecraft.world.level.LevelHeightAccessor;
import net.minecraft.world.level.StructureManager;
import net.minecraft.world.level.biome.Biome;
import net.minecraft.world.level.biome.BiomeManager;
import net.minecraft.world.level.biome.Climate;
import net.minecraft.world.level.biome.MultiNoiseBiomeSource;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.entity.BlockEntity;
import net.minecraft.world.level.chunk.ChunkAccess;
import net.minecraft.world.level.chunk.ChunkGenerator;
import net.minecraft.world.level.chunk.ChunkGeneratorStructureState;
import net.minecraft.world.level.chunk.LevelChunkSection;
import net.minecraft.world.level.chunk.ProtoChunk;
import net.minecraft.world.level.chunk.UpgradeData;
import net.minecraft.world.level.chunk.status.ChunkPyramid;
import net.minecraft.world.level.chunk.status.ChunkStatus;
import net.minecraft.world.level.dimension.BuiltinDimensionTypes;
import net.minecraft.world.level.levelgen.Aquifer;
import net.minecraft.world.level.levelgen.Beardifier;
import net.minecraft.world.level.levelgen.FlatLevelSource;
import net.minecraft.world.level.levelgen.Heightmap;
import net.minecraft.world.level.levelgen.NoiseBasedChunkGenerator;
import net.minecraft.world.level.levelgen.NoiseChunk;
import net.minecraft.world.level.levelgen.NoiseGeneratorSettings;
import net.minecraft.world.level.levelgen.RandomState;
import net.minecraft.world.level.levelgen.WorldOptions;
import net.minecraft.world.level.levelgen.blending.Blender;
import net.minecraft.world.level.levelgen.densityfunction.DensityVolume;
import net.minecraft.world.level.levelgen.flat.FlatLevelGeneratorSettings;
import net.minecraft.world.level.levelgen.material.rule.MaterialRule;
import net.minecraft.world.level.levelgen.structure.Structure;
import net.minecraft.world.level.levelgen.structure.StructurePiece;
import net.minecraft.world.level.levelgen.structure.StructureStart;

/**
 * World generation sweep (#223): Vanilla 26.3 generates whole areas of a world in-process, and
 * Vanillite's {@code vanilla_sweep} test generates the same areas and compares them chunk by
 * chunk. Not a golden: nothing is committed, the output is written per run.
 *
 * <p>Usage: {@code java -cp <classes>:<server jar>:<libraries> dev.vanillite.extractor.golden.WorldSweep
 * <out dir> <kind>:<seed>:<min chunk x>:<min chunk z>:<size>...}; kinds: {@code overworld},
 * {@code nether}, {@code end}, {@code amplified}, {@code large_biomes} (overworld biome source
 * with the noise settings of that name) and {@code flat} (the default flat preset, {@code
 * FlatLevelGeneratorSettings.getDefault}: classic layers, villages and strongholds). Each job
 * writes {@code <out>/<kind>_<seed>_<x>_<z>_<size>.bin.gz}.
 *
 * <p>Pipeline (structures on, no blending), as {@link StructureGoldens}' decoration areas: fresh
 * proto chunks get starts, references, biomes and terrain on demand, and {@code
 * applyBiomeDecoration} runs for every chunk of the area plus one ring, row-major (z outer, x
 * inner), with the chunk access rules of each step. A fixed decoration order makes the result
 * deterministic (a server decorates neighbours in a run-dependent order), and so does a fresh
 * climate search cache per chunk's biomes (a server's worker threads carry it over from chunk to
 * chunk in a run-dependent order, which matters on exact climate ties). The area chunks are
 * then recorded as decorated (status {@code features}; light, mob spawning and post-processing
 * are not run).
 *
 * <p>File (gzip, big-endian {@code DataOutputStream}): int magic {@code 0x56535750}, int format
 * 1, UTF kind, long seed, int min y, int height, int min chunk x, int min chunk z, int size;
 * then per chunk (z outer, x inner): int x, int z; per section 4096 shorts {@code
 * Block.getId} (y, z, x order); per section 64 biome indices as shorts (quart y, z, x order) where
 * an index equal to the number of names so far is followed by the UTF name of the new biome; 4 ×
 * 256 ints {@code getHeight} of WORLD_SURFACE, OCEAN_FLOOR, MOTION_BLOCKING,
 * MOTION_BLOCKING_NO_LEAVES (z, x order); per section int count and long hash of the
 * post-processing marks; long count and hash of the block ticks and of the fluid ticks (as
 * {@link FeatureGoldens#blockTicks}); int length and UTF-8 bytes of a JSON object {@code
 * {starts: [{structure, chunk: [x, z], bb, pieces: [[type, bb, orientation, gen depth]]}] sorted by
 * structure, references: {structure: [sorted chunk longs]}, block_entities: [[x, y, z, type,
 * NBT (saveWithoutMetadata) or null for a pending tag]] sorted by y, z, x}}.
 */
public final class WorldSweep {
  private static final Gson GSON = new GsonBuilder().disableHtmlEscaping().serializeNulls().create();

  private WorldSweep() {}

  public static void main(String[] args) throws Exception {
    if (args.length < 2) {
      System.err.println("usage: WorldSweep <out dir> <kind>:<seed>:<min chunk x>:<min chunk z>:<size>...");
      System.exit(2);
    }
    Path out = Path.of(args[0]);
    Files.createDirectories(out);
    SharedConstants.tryDetectVersion();
    Bootstrap.bootStrap();
    FeatureGoldens.init();
    for (int i = 1; i < args.length; i++) {
      String[] job = args[i].split(":");
      String kind = job[0];
      long seed = Long.parseLong(job[1]);
      int x0 = Integer.parseInt(job[2]);
      int z0 = Integer.parseInt(job[3]);
      int size = Integer.parseInt(job[4]);
      long start = System.nanoTime();
      Path file = out.resolve(kind + "_" + seed + "_" + x0 + "_" + z0 + "_" + size + ".bin.gz");
      Sweep sweep = new Sweep(kind, seed);
      for (int z = z0 - 1; z <= z0 + size; z++) {
        for (int x = x0 - 1; x <= x0 + size; x++) {
          sweep.features(x, z);
        }
      }
      try (OutputStream raw = Files.newOutputStream(file);
          DataOutputStream o = new DataOutputStream(new BufferedOutputStream(new GZIPOutputStream(raw, 1 << 16)))) {
        o.writeInt(0x56535750);
        o.writeInt(1);
        o.writeUTF(kind);
        o.writeLong(seed);
        o.writeInt(sweep.dimension.minY());
        o.writeInt(sweep.dimension.height());
        o.writeInt(x0);
        o.writeInt(z0);
        o.writeInt(size);
        Map<String, Integer> biomes = new HashMap<>();
        for (int z = z0; z < z0 + size; z++) {
          for (int x = x0; x < x0 + size; x++) {
            sweep.write(sweep.get(x, z), o, biomes);
          }
        }
      }
      System.out.printf("world sweep: %s seed %d area %d %d size %d in %.1f s%n", kind, seed, x0, z0, size,
          (System.nanoTime() - start) / 1e9);
      // Each job has its own world; drop the cached ones.
      clearWorlds();
    }
  }

  @SuppressWarnings("unchecked")
  static void clearWorlds() throws Exception {
    Field f = FeatureGoldens.class.getDeclaredField("WORLDS");
    f.setAccessible(true);
    ((Map<String, World>) f.get(null)).clear();
  }

  /** One world's chunks taken through the generation steps on demand. */
  static final class Sweep {
    // Static state is initialised on first use, after the bootstrap in main.
    private static final Dimension AMPLIFIED = new Dimension("minecraft:amplified", NoiseGeneratorSettings.AMPLIFIED,
        BuiltinDimensionTypes.OVERWORLD, -64, 384, new int[0]);
    private static final Dimension LARGE_BIOMES = new Dimension("minecraft:large_biomes",
        NoiseGeneratorSettings.LARGE_BIOMES, BuiltinDimensionTypes.OVERWORLD, -64, 384, new int[0]);
    /** The flat world: a noise world's level, random state and registries with a flat generator. */
    private static final Dimension FLAT = new Dimension("minecraft:flat", NoiseGeneratorSettings.OVERWORLD,
        BuiltinDimensionTypes.OVERWORLD, -64, 384, new int[0]);

    private static final Heightmap.Types[] HEIGHTMAPS = {
      Heightmap.Types.WORLD_SURFACE, Heightmap.Types.OCEAN_FLOOR,
      Heightmap.Types.MOTION_BLOCKING, Heightmap.Types.MOTION_BLOCKING_NO_LEAVES,
    };

    private static ChunkStatus[] dependencies(ChunkStatus status) {
      return ChunkPyramid.GENERATION_PYRAMID.getStepTo(status).directDependencies().asList().toArray(new ChunkStatus[0]);
    }

    private static final ChunkStatus[] REFERENCES_DEPS = dependencies(ChunkStatus.STRUCTURE_REFERENCES);
    private static final ChunkStatus[] TERRAIN_DEPS = dependencies(ChunkStatus.TERRAIN);
    private static final ChunkStatus[] FEATURES_DEPS = dependencies(ChunkStatus.FEATURES);
    private static final ChunkStatus[] SERVER_DEPS = new ChunkStatus[64];

    static {
      Arrays.fill(SERVER_DEPS, ChunkStatus.FULL);
    }

    final Dimension dimension;
    final World world;
    final ChunkGenerator generator;
    final ResourceKey<Level> level;
    final long seed;
    final ChunkGeneratorStructureState state;
    final LevelHeightAccessor height;
    final WorldOptions options;
    final StructureManager starter;
    final Registry<Structure> structures;
    final Map<Long, ProtoChunk> chunks = new HashMap<>();
    final Method doFill;
    final Method buildSurface;
    final Method generateCarvers;
    final Aquifer.FluidPicker picker;

    Sweep(String kind, long seed) throws Exception {
      this.seed = seed;
      this.dimension = switch (kind) {
        case "overworld" -> FeatureGoldens.OVERWORLD;
        case "nether" -> FeatureGoldens.NETHER;
        case "end" -> FeatureGoldens.END;
        case "amplified" -> AMPLIFIED;
        case "large_biomes" -> LARGE_BIOMES;
        case "flat" -> FLAT;
        default -> throw new IllegalArgumentException("unknown kind " + kind);
      };
      this.level = switch (kind) {
        case "nether" -> Level.NETHER;
        case "end" -> Level.END;
        default -> Level.OVERWORLD;
      };
      this.world = FeatureGoldens.world(dimension, seed);
      if (kind.equals("flat")) {
        generator = new FlatLevelSource(FlatLevelGeneratorSettings.getDefault(
            FeatureGoldens.lookup.lookupOrThrow(Registries.BIOME),
            FeatureGoldens.lookup.lookupOrThrow(Registries.STRUCTURE_SET),
            FeatureGoldens.lookup.lookupOrThrow(Registries.PLACED_FEATURE)));
        Field cacheField = FeatureGoldens.FakeLevel.class.getDeclaredField("chunkCache");
        cacheField.setAccessible(true);
        Object cache = cacheField.get(world.level);
        Field generatorField = cache.getClass().getDeclaredField("generator");
        generatorField.setAccessible(true);
        generatorField.set(cache, generator);
      } else {
        generator = world.generator;
      }
      this.structures = FeatureGoldens.registryAccess.lookupOrThrow(Registries.STRUCTURE);
      this.state = generator.createState(
          FeatureGoldens.registryAccess.lookupOrThrow(Registries.STRUCTURE_SET), world.randomState, seed);
      this.state.ensureStructuresGenerated();
      this.height = LevelHeightAccessor.create(dimension.minY(), dimension.height());
      this.options = new WorldOptions(seed, true, false);
      this.starter = new StructureManager(null, options, null);
      doFill = FeatureGoldens.generatorMethod("doFill", NoiseChunk.class, ChunkAccess.class);
      buildSurface = FeatureGoldens.generatorMethod("buildSurface", ChunkAccess.class, NoiseChunk.class,
          RandomState.class, BiomeManager.class, Set.class, MaterialRule.class);
      generateCarvers = FeatureGoldens.generatorMethod("generateCarvers", ChunkAccess.class, Blender.class,
          NoiseChunk.class, RandomState.class, BiomeManager.class, net.minecraft.server.level.WorldGenRegion.class,
          MaterialRule.class);
      Field pickerField = NoiseBasedChunkGenerator.class.getDeclaredField("globalFluidPicker");
      pickerField.setAccessible(true);
      @SuppressWarnings("unchecked")
      Supplier<Aquifer.FluidPicker> supplier = (Supplier<Aquifer.FluidPicker>) pickerField.get(world.generator);
      picker = supplier.get();
    }

    /** Clears {@code Climate.RTree.lastResult} of this thread for a multi-noise biome source. */
    void clearLastResult() throws Exception {
      if (!(generator.getBiomeSource() instanceof MultiNoiseBiomeSource source)) {
        return;
      }
      Method parameters = MultiNoiseBiomeSource.class.getDeclaredMethod("parameters");
      parameters.setAccessible(true);
      Object list = parameters.invoke(source);
      Field index = Climate.ParameterList.class.getDeclaredField("index");
      index.setAccessible(true);
      Object tree = index.get(list);
      Field lastResult = tree.getClass().getDeclaredField("lastResult");
      lastResult.setAccessible(true);
      ((ThreadLocal<?>) lastResult.get(tree)).remove();
    }

    String id(Structure structure) {
      return structures.getKey(structure).toString();
    }

    ProtoChunk get(int x, int z) {
      return chunks.get(ChunkPos.pack(x, z));
    }

    static boolean reached(ProtoChunk chunk, ChunkStatus status) {
      return chunk != null && chunk.getPersistedStatus().isOrAfter(status);
    }

    ProtoChunk starts(int x, int z) {
      ProtoChunk chunk = get(x, z);
      if (chunk == null) {
        chunk = new ProtoChunk(new ChunkPos(x, z), UpgradeData.EMPTY, height, FeatureGoldens.factory, null);
        generator.createStructures(FeatureGoldens.registryAccess, state, starter, chunk, FeatureGoldens.templates,
            level);
        chunk.setPersistedStatus(ChunkStatus.STRUCTURE_STARTS);
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
      Region region = new Region(world, chunks, chunk.getPos(), REFERENCES_DEPS);
      generator.createReferences(region.proxy, new StructureManager(region.proxy, options, null), chunk);
      chunk.setPersistedStatus(ChunkStatus.STRUCTURE_REFERENCES);
      return chunk;
    }

    ProtoChunk biomes(int x, int z) throws Exception {
      ProtoChunk chunk = references(x, z);
      if (!reached(chunk, ChunkStatus.BIOMES)) {
        // `createBiomes` on this thread, as on a fresh worker thread: the climate search starts
        // without the last result of another chunk (Vanillite keeps that cache per chunk;
        // Vanilla's is per thread, so on exact climate ties its result depends on which chunk
        // the worker thread generated before).
        clearLastResult();
        Method doCreateBiomes = ChunkGenerator.class.getDeclaredMethod(
            "doCreateBiomes", Blender.class, RandomState.class, ChunkAccess.class);
        doCreateBiomes.setAccessible(true);
        doCreateBiomes.invoke(generator, Blender.empty(), world.randomState, chunk);
        chunk.setPersistedStatus(ChunkStatus.BIOMES);
      }
      return chunk;
    }

    ProtoChunk terrain(int x, int z) throws Exception {
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
      ChunkPos pos = chunk.getPos();
      Region region = new Region(world, chunks, pos, TERRAIN_DEPS);
      StructureManager structureManager = new StructureManager(region.proxy, options, null);
      if (generator instanceof NoiseBasedChunkGenerator) {
        BiomeManager biomeManager = new BiomeManager(
            (qx, qy, qz) -> chunks.get(ChunkPos.pack(qx >> 2, qz >> 2)).getNoiseBiome(qx, qy, qz),
            BiomeManager.obfuscateSeed(seed));
        Beardifier beardifier = Beardifier.forStructuresInChunk(structureManager, pos);
        DensityVolume volume = new DensityVolume(16, world.settings.noiseSettings().height(), 16,
            pos.getMinBlockX(), world.settings.noiseSettings().minY(), pos.getMinBlockZ());
        MaterialRule materialRule = world.settings.materialRule().value();
        try (NoiseChunk noiseChunk =
            new NoiseChunk(world.randomState, beardifier, world.settings, picker, Blender.empty(), volume)) {
          doFill.invoke(generator, noiseChunk, chunk);
          buildSurface.invoke(generator, chunk, noiseChunk, world.randomState, biomeManager, possibleBiomes,
              materialRule);
          generateCarvers.invoke(generator, chunk, Blender.empty(), noiseChunk, world.randomState, biomeManager,
              null, materialRule);
        }
      } else {
        // Flat: the layers; no surface and no carvers.
        generator.buildTerrain(chunk, Blender.empty(), world.randomState, structureManager, null, null, possibleBiomes)
            .join();
      }
      chunk.setPersistedStatus(ChunkStatus.TERRAIN);
      Heightmap.primeHeightmaps(chunk, FeatureGoldens.FINAL_HEIGHTMAPS);
      return chunk;
    }

    void features(int x, int z) throws Exception {
      for (int dz = -1; dz <= 1; dz++) {
        for (int dx = -1; dx <= 1; dx++) {
          terrain(x + dx, z + dz);
        }
      }
      ProtoChunk chunk = get(x, z);
      Region region = new Region(world, chunks, chunk.getPos(), FEATURES_DEPS);
      Region server = new Region(world, chunks, chunk.getPos(), SERVER_DEPS);
      world.level.structures = new StructureManager(server.proxy, options, null);
      try {
        generator.applyBiomeDecoration(region.proxy, chunk, new StructureManager(region.proxy, options, null));
      } finally {
        world.level.structures = null;
      }
      chunk.setPersistedStatus(ChunkStatus.FEATURES);
    }

    void write(ProtoChunk chunk, DataOutputStream o, Map<String, Integer> biomes) throws Exception {
      ChunkPos pos = chunk.getPos();
      o.writeInt(pos.x());
      o.writeInt(pos.z());
      LevelChunkSection[] sections = chunk.getSections();
      for (LevelChunkSection section : sections) {
        for (int y = 0; y < 16; y++) {
          for (int z = 0; z < 16; z++) {
            for (int x = 0; x < 16; x++) {
              o.writeShort(Block.getId(section.getBlockState(x, y, z)));
            }
          }
        }
      }
      int minQuartY = dimension.minY() >> 2;
      for (int s = 0; s < sections.length; s++) {
        for (int qy = 0; qy < 4; qy++) {
          for (int qz = 0; qz < 4; qz++) {
            for (int qx = 0; qx < 4; qx++) {
              Holder<Biome> biome = chunk.getNoiseBiome((pos.x() << 2) + qx, minQuartY + s * 4 + qy,
                  (pos.z() << 2) + qz);
              String name = biome.unwrapKey().orElseThrow().identifier().toString();
              Integer index = biomes.get(name);
              if (index == null) {
                index = biomes.size();
                biomes.put(name, index);
                o.writeShort(index);
                o.writeUTF(name);
              } else {
                o.writeShort(index);
              }
            }
          }
        }
      }
      for (Heightmap.Types type : HEIGHTMAPS) {
        for (int z = 0; z < 16; z++) {
          for (int x = 0; x < 16; x++) {
            o.writeInt(chunk.getHeight(type, x, z));
          }
        }
      }
      var post = chunk.getPostProcessing();
      for (int s = 0; s < sections.length; s++) {
        long hash = 0;
        int count = 0;
        if (post[s] != null) {
          for (int j = 0; j < post[s].size(); j++) {
            hash = hash * 31 + post[s].getShort(j);
          }
          count = post[s].size();
        }
        o.writeInt(count);
        o.writeLong(hash);
      }
      for (long v : FeatureGoldens.blockTicks(chunk)) {
        o.writeLong(v);
      }
      for (long v : FeatureGoldens.fluidTicks(chunk)) {
        o.writeLong(v);
      }
      byte[] json = GSON.toJson(details(chunk)).getBytes(StandardCharsets.UTF_8);
      o.writeInt(json.length);
      o.write(json);
    }

    JsonObject details(ProtoChunk chunk) {
      JsonObject out = new JsonObject();
      TreeMap<String, JsonObject> starts = new TreeMap<>();
      for (Map.Entry<Structure, StructureStart> e : chunk.getAllStarts().entrySet()) {
        StructureStart start = e.getValue();
        JsonObject s = new JsonObject();
        s.addProperty("structure", id(e.getKey()));
        s.add("chunk", FeatureGoldens.array(start.getChunkPos().x(), start.getChunkPos().z()));
        s.add("bb", StructureGoldens.box(start.getBoundingBox()));
        JsonArray pieces = new JsonArray();
        for (StructurePiece piece : start.getPieces()) {
          JsonArray p = new JsonArray();
          p.add(BuiltInRegistries.STRUCTURE_PIECE.getKey(piece.getType()).toString());
          p.add(StructureGoldens.box(piece.getBoundingBox()));
          p.add(piece.getOrientation() == null
              ? JsonNull.INSTANCE : new JsonPrimitive(piece.getOrientation().getSerializedName()));
          p.add(piece.getGenDepth());
          pieces.add(p);
        }
        s.add("pieces", pieces);
        starts.put(id(e.getKey()), s);
      }
      JsonArray startsJson = new JsonArray();
      starts.values().forEach(startsJson::add);
      out.add("starts", startsJson);
      TreeMap<String, JsonArray> references = new TreeMap<>();
      for (Map.Entry<Structure, LongSet> e : chunk.getAllReferences().entrySet()) {
        if (e.getValue().isEmpty()) {
          continue;
        }
        long[] sorted = e.getValue().toLongArray();
        Arrays.sort(sorted);
        references.put(id(e.getKey()), FeatureGoldens.array(sorted));
      }
      JsonObject referencesJson = new JsonObject();
      references.forEach(referencesJson::add);
      out.add("references", referencesJson);
      List<BlockPos> positions = new ArrayList<>(chunk.getBlockEntitiesPos());
      positions.sort(Comparator.<BlockPos>comparingInt(BlockPos::getY).thenComparingInt(BlockPos::getZ)
          .thenComparingInt(BlockPos::getX));
      JsonArray blockEntities = new JsonArray();
      for (BlockPos p : positions) {
        BlockEntity entity = chunk.getBlockEntity(p);
        JsonArray e = new JsonArray();
        e.add(p.getX());
        e.add(p.getY());
        e.add(p.getZ());
        if (entity != null) {
          e.add(BuiltInRegistries.BLOCK_ENTITY_TYPE.getKey(entity.getType()).toString());
          e.add(StructureGoldens.nbt(entity.saveWithoutMetadata(FeatureGoldens.registryAccess)));
        } else {
          e.add(chunk.getBlockEntityNbt(p).getStringOr("id", ""));
          e.add(JsonNull.INSTANCE);
        }
        blockEntities.add(e);
      }
      out.add("block_entities", blockEntities);
      return out;
    }
  }
}

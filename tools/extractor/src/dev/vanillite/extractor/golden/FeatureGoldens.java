// Copyright Hannes Thöne. Licensed under the Apache License, Version 2.0.
package dev.vanillite.extractor.golden;

import com.google.gson.JsonArray;
import com.google.gson.JsonObject;
import it.unimi.dsi.fastutil.shorts.ShortArrayList;
import it.unimi.dsi.fastutil.shorts.ShortList;
import java.lang.reflect.Field;
import java.lang.reflect.InvocationHandler;
import java.lang.reflect.Method;
import java.lang.reflect.Proxy;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.Collections;
import java.util.Comparator;
import java.util.EnumSet;
import java.util.HashMap;
import java.util.HashSet;
import java.util.IdentityHashMap;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.atomic.AtomicLong;
import java.util.function.Predicate;
import java.util.function.Supplier;
import java.util.stream.Stream;
import net.minecraft.core.BlockPos;
import net.minecraft.core.Holder;
import net.minecraft.core.HolderGetter;
import net.minecraft.core.HolderLookup;
import net.minecraft.core.HolderSet;
import net.minecraft.core.LayeredRegistryAccess;
import net.minecraft.core.QuartPos;
import net.minecraft.core.Registry;
import net.minecraft.core.RegistryAccess;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.core.registries.Registries;
import net.minecraft.nbt.CompoundTag;
import net.minecraft.resources.FileToIdConverter;
import net.minecraft.resources.Identifier;
import net.minecraft.resources.ResourceKey;
import net.minecraft.server.MinecraftServer;
import net.minecraft.server.dedicated.DedicatedServer;
import net.minecraft.server.level.ServerChunkCache;
import net.minecraft.server.level.ServerLevel;
import net.minecraft.server.packs.PackType;
import net.minecraft.server.packs.repository.ServerPacksSource;
import net.minecraft.server.packs.resources.MultiPackResourceManager;
import net.minecraft.server.packs.resources.ResourceManager;
import net.minecraft.resources.RegistryDataLoader;
import net.minecraft.server.RegistryLayer;
import net.minecraft.tags.TagLoader;
import net.minecraft.util.RandomSource;
import net.minecraft.util.datafix.DataFixers;
import net.minecraft.world.Difficulty;
import net.minecraft.world.DifficultyInstance;
import net.minecraft.world.flag.FeatureFlagSet;
import net.minecraft.world.flag.FeatureFlags;
import net.minecraft.world.level.ChunkPos;
import net.minecraft.world.level.LevelHeightAccessor;
import net.minecraft.world.level.StructureManager;
import net.minecraft.world.level.WorldGenLevel;
import net.minecraft.world.level.biome.Biome;
import net.minecraft.world.level.biome.BiomeManager;
import net.minecraft.world.level.biome.BiomeResolver;
import net.minecraft.world.level.biome.BiomeSource;
import net.minecraft.world.level.biome.MultiNoiseBiomeSource;
import net.minecraft.world.level.biome.MultiNoiseBiomeSourceParameterLists;
import net.minecraft.world.level.biome.TheEndBiomeSource;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.Blocks;
import net.minecraft.world.level.block.EntityBlock;
import net.minecraft.world.level.block.entity.BlockEntity;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.chunk.ChunkAccess;
import net.minecraft.world.level.chunk.ChunkGenerator;
import net.minecraft.world.level.BlockGetter;
import net.minecraft.world.level.chunk.LevelChunkSection;
import net.minecraft.world.level.chunk.LightChunk;
import net.minecraft.world.level.chunk.LightChunkGetter;
import net.minecraft.world.level.lighting.LevelLightEngine;
import net.minecraft.world.level.chunk.PalettedContainerFactory;
import net.minecraft.world.level.chunk.ProtoChunk;
import net.minecraft.world.level.chunk.UpgradeData;
import net.minecraft.world.level.chunk.status.ChunkStatus;
import net.minecraft.world.level.dimension.BuiltinDimensionTypes;
import net.minecraft.world.level.dimension.DimensionType;
import net.minecraft.world.level.levelgen.Aquifer;
import net.minecraft.world.level.levelgen.Heightmap;
import net.minecraft.world.level.levelgen.NoiseBasedChunkGenerator;
import net.minecraft.world.level.levelgen.NoiseChunk;
import net.minecraft.world.level.levelgen.NoiseGeneratorSettings;
import net.minecraft.world.level.levelgen.RandomState;
import net.minecraft.world.level.levelgen.WorldOptions;
import net.minecraft.world.level.levelgen.WorldgenRandom;
import net.minecraft.world.level.levelgen.XoroshiroRandomSource;
import net.minecraft.world.level.levelgen.blending.Blender;
import net.minecraft.world.level.levelgen.densityfunction.DensityVolume;
import net.minecraft.world.level.levelgen.feature.Feature;
import net.minecraft.world.level.levelgen.material.rule.MaterialRule;
import net.minecraft.world.level.levelgen.placement.BiomeFilter;
import net.minecraft.world.level.levelgen.placement.FeaturePlacer;
import net.minecraft.world.level.levelgen.placement.PlacedFeature;
import net.minecraft.world.level.levelgen.structure.templatesystem.StructureTemplateManager;
import net.minecraft.world.level.levelgen.structure.templatesystem.loader.ResourceManagerTemplateSource;
import net.minecraft.world.level.material.Fluid;
import net.minecraft.world.level.material.FluidState;
import net.minecraft.world.level.storage.LevelData;
import net.minecraft.world.ticks.ProtoChunkTicks;
import net.minecraft.world.ticks.SavedTick;
import net.minecraft.world.ticks.WorldGenTickAccess;

/**
 * Feature stage goldens: Vanilla's {@code FeaturePlacer} and {@code applyBiomeDecoration} on
 * terrain generated like {@link ChunkGoldens} (no structures, no blending).
 *
 * <p>Vanilla code gets a {@link WorldGenLevel} without a running server: a {@link Proxy} over
 * the generated {@link ProtoChunk}s with {@code WorldGenRegion}'s semantics (see {@link
 * Region}). {@code getLevel()} is a {@link ServerLevel} subclass allocated without running a
 * constructor, so the few feature code paths that read it (template and fossil features, end
 * crystals, the pale moss tree decorator) find a structure template manager, the generator and
 * the registries.
 *
 * <p>Part {@code features}: every placed feature of the Vanilla registry on the 3×3 terrain
 * chunks around a chunk where one of its biomes was found ({@code areas}). Placed features that
 * biomes list run like {@code applyBiomeDecoration} runs them ({@code placeWithBiomeCheck} at the
 * centre chunk's minimum corner at the bottom of the world); the others (trees, piles and other
 * features that only other features or structures place) run {@code place} at the surface of the
 * centre column. The random is {@code new WorldgenRandom(new XoroshiroRandomSource(0))} with
 * {@code setDecorationSeed(seed, minX, minZ)} and {@code setFeatureSeed(decorationSeed, 0,
 * step)}, where {@code step} is the first decoration step that lists the feature (0 for the
 * others). Recorded: the result and every difference to the area's terrain (section block
 * hashes, final heightmap hashes, post-processing marks) plus block entities and scheduled
 * ticks.
 *
 * <p>Part {@code decorations}: {@code applyBiomeDecoration} for the chunks within {@code radius}
 * of a centre, in row-major order (or its reverse), on terrain chunks one further out; records
 * all of them. Features near chunk borders write into neighbours that are decorated before or
 * after, so the result depends on that order.
 */
public final class FeatureGoldens {
  record Dimension(
      String name, ResourceKey<NoiseGeneratorSettings> settings, ResourceKey<DimensionType> type,
      int minY, int height, int[] sampleYs) {}

  static final Dimension OVERWORLD = new Dimension("minecraft:overworld", NoiseGeneratorSettings.OVERWORLD,
      BuiltinDimensionTypes.OVERWORLD, -64, 384, new int[] {70, 120, 0, -40});
  static final Dimension NETHER = new Dimension("minecraft:nether", NoiseGeneratorSettings.NETHER,
      BuiltinDimensionTypes.NETHER, 0, 256, new int[] {32, 64, 100});
  static final Dimension END = new Dimension("minecraft:end", NoiseGeneratorSettings.END,
      BuiltinDimensionTypes.END, 0, 256, new int[] {64});
  private static final Dimension[] DIMENSIONS = {OVERWORLD, NETHER, END};

  /** World seed of the per placed feature part, in every dimension. */
  private static final long FEATURE_SEED = 0L;

  private record DecorationArea(Dimension dimension, long seed, int x, int z, int radius, boolean reversed) {
    DecorationArea(Dimension dimension, long seed, int x, int z) {
      this(dimension, seed, x, z, 1, false);
    }
  }

  private static final DecorationArea[] DECORATIONS = {
    new DecorationArea(OVERWORLD, 0L, 0, 0),
    new DecorationArea(OVERWORLD, -7302402383914416L, -3, 8),
    new DecorationArea(NETHER, 42L, 0, 0),
    new DecorationArea(END, 1L, 0, 0),
    new DecorationArea(END, 1L, 100, 0),
    // A forest with many overlapping trees and ore blobs across chunk borders, in both orders.
    new DecorationArea(OVERWORLD, -578941029922802438L, -7, -12, 3, false),
    new DecorationArea(OVERWORLD, -578941029922802438L, -7, -12, 3, true),
  };

  /**
   * Attempts per placed feature: a feature that changes no block is run again (on fresh
   * terrain) with the next feature seed index, up to this many times in its first area and half
   * as many in each further area (up to {@link #AREAS} areas where its biomes were found); the
   * last attempt is recorded.
   */
  private static final int ATTEMPTS = 16;
  private static final int AREAS = 5;

  /**
   * Placed features the default search covers badly: a fixed area (chunk x, z) or a biome to
   * search instead of the feature's biomes, and the attempts in the first area.
   */
  private record Special(int[] chunk, String biome, int attempts) {}

  private static final Map<String, Special> SPECIAL = Map.of(
      // A spike centre: spike 0 is at (42, 0) for every seed.
      "minecraft:end_spike", new Special(new int[] {2, 0}, null, ATTEMPTS),
      // Snow and ice need a cold biome; every biome lists the feature.
      "minecraft:freeze_top_layer", new Special(null, "minecraft:snowy_plains", ATTEMPTS),
      // Rarity 1/64.
      "minecraft:fossil_upper", new Special(null, null, 160),
      "minecraft:fossil_lower", new Special(null, null, 160),
      "minecraft:underwater_magma", new Special(null, null, 64));

  /** Grid step and radius (in grid steps) of the biome search for the per feature areas. */
  private static final int SEARCH_STEP = 8;
  private static final int SEARCH_RADIUS = 48;

  /** The heightmaps recorded, in this order. */
  private static final Heightmap.Types[] HEIGHTMAPS = {
    Heightmap.Types.WORLD_SURFACE, Heightmap.Types.OCEAN_FLOOR,
    Heightmap.Types.MOTION_BLOCKING, Heightmap.Types.MOTION_BLOCKING_NO_LEAVES,
  };
  static final EnumSet<Heightmap.Types> FINAL_HEIGHTMAPS = EnumSet.of(
      Heightmap.Types.MOTION_BLOCKING, Heightmap.Types.MOTION_BLOCKING_NO_LEAVES,
      Heightmap.Types.OCEAN_FLOOR, Heightmap.Types.WORLD_SURFACE);

  private FeatureGoldens() {}

  // ---------------------------------------------------------------------------------------------
  // Shared state

  static HolderLookup.Provider lookup;
  static RegistryAccess registryAccess;
  static ResourceManager resources;
  static PalettedContainerFactory factory;
  static StructureTemplateManager templates;
  private static final Map<String, World> WORLDS = new HashMap<>();

  /** A dimension with a seed: generator, random state and the fake server level. */
  static final class World {
    final Dimension dimension;
    final long seed;
    final NoiseGeneratorSettings settings;
    final NoiseBasedChunkGenerator generator;
    final RandomState randomState;
    final DimensionType dimensionType;
    /** {@code ServerLevel.uncachedBiomeResolver()}. */
    final BiomeResolver biomes;
    final FakeLevel level;

    World(Dimension dimension, long seed) throws Exception {
      this.dimension = dimension;
      this.seed = seed;
      Holder<NoiseGeneratorSettings> settings =
          lookup.lookupOrThrow(Registries.NOISE_SETTINGS).getOrThrow(dimension.settings());
      HolderGetter<Biome> biomes = lookup.lookupOrThrow(Registries.BIOME);
      BiomeSource source = switch (dimension.name()) {
        case "minecraft:nether" -> MultiNoiseBiomeSource.createFromPreset(lookup
            .lookupOrThrow(Registries.MULTI_NOISE_BIOME_SOURCE_PARAMETER_LIST)
            .getOrThrow(MultiNoiseBiomeSourceParameterLists.NETHER));
        case "minecraft:end" -> TheEndBiomeSource.create(biomes);
        default -> MultiNoiseBiomeSource.createFromPreset(lookup
            .lookupOrThrow(Registries.MULTI_NOISE_BIOME_SOURCE_PARAMETER_LIST)
            .getOrThrow(MultiNoiseBiomeSourceParameterLists.OVERWORLD));
      };
      this.settings = settings.value();
      this.generator = new NoiseBasedChunkGenerator(source, settings);
      this.randomState = RandomState.create(lookup.lookupOrThrow(Registries.NOISE), seed, settings.value());
      this.dimensionType = lookup.lookupOrThrow(Registries.DIMENSION_TYPE).getOrThrow(dimension.type()).value();
      this.biomes = source.createUncachedResolver(randomState);
      this.level = FakeLevel.create(this);
    }
  }

  static World world(Dimension dimension, long seed) throws Exception {
    String key = dimension.name() + "/" + seed;
    World world = WORLDS.get(key);
    if (world == null) {
      world = new World(dimension, seed);
      WORLDS.put(key, world);
    }
    return world;
  }

  /** Loads the registries, the container factory and the template manager once. */
  static void init() throws Exception {
    if (registryAccess != null) {
      return;
    }
    BlockTags.bind();
    resources = new MultiPackResourceManager(
        PackType.SERVER_DATA, List.of(ServerPacksSource.createVanillaPackSource().fullResources()));
    registryAccess = loadRegistries(resources);
    lookup = registryAccess;
    factory = ChunkGoldens.containerFactory(lookup);
    templates = templateManager(resources);
    // Item components, as ReloadableServerResources binds them (structure block entities such as
    // vaults create item stacks).
    BuiltInRegistries.DATA_COMPONENT_INITIALIZERS.build(registryAccess)
        .forEach(net.minecraft.core.component.DataComponentInitializers.PendingComponents::apply);
  }

  public static JsonObject extract() throws Exception {
    init();

    JsonObject root = new JsonObject();
    long start = System.nanoTime();
    perFeature(root);
    System.out.printf("feature goldens: placed features in %.1f s%n", (System.nanoTime() - start) / 1e9);
    start = System.nanoTime();
    JsonArray decorations = new JsonArray();
    for (DecorationArea area : DECORATIONS) {
      decorations.add(decoration(area));
    }
    root.add("decorations", decorations);
    System.out.printf("feature goldens: decorations in %.1f s%n", (System.nanoTime() - start) / 1e9);
    return root;
  }

  // ---------------------------------------------------------------------------------------------
  // Part 1: every placed feature

  /**
   * Where a placed feature runs: its biomes, its first decoration step and whether it runs as
   * decoration does (with biome check, at the centre chunk's minimum corner).
   */
  private record Usage(Set<Holder<Biome>> biomes, int step, boolean asDecoration) {}

  private static void perFeature(JsonObject root) throws Exception {
    Map<PlacedFeature, Usage> usages = usages();
    List<Holder.Reference<PlacedFeature>> placed =
        new ArrayList<>(lookup.lookupOrThrow(Registries.PLACED_FEATURE).listElements().toList());
    placed.sort(Comparator.comparing(h -> h.key().identifier().toString()));

    Map<Dimension, BiomeSearch> searches = new HashMap<>();
    Map<String, Integer> areaIndex = new HashMap<>();
    Map<String, Map<Long, ProtoChunk>> terrainCache = new HashMap<>();
    JsonArray areas = new JsonArray();
    JsonArray features = new JsonArray();
    for (Holder.Reference<PlacedFeature> holder : placed) {
      String name = holder.key().identifier().toString();
      PlacedFeature feature = holder.value();
      Usage usage = usages.getOrDefault(feature, new Usage(Set.of(), 0, false));

      Dimension dimension = OVERWORLD;
      for (Dimension d : DIMENSIONS) {
        World w = world(d, FEATURE_SEED);
        if (usage.biomes().stream().anyMatch(b -> w.generator.getBiomeSource().possibleBiomes().contains(b))) {
          dimension = d;
          break;
        }
      }
      World world = world(dimension, FEATURE_SEED);
      BiomeSearch search = searches.get(dimension);
      if (search == null) {
        search = new BiomeSearch(world);
        searches.put(dimension, search);
      }
      Special special = SPECIAL.get(name);
      List<ChunkPos> centers;
      if (special != null && special.chunk() != null) {
        centers = List.of(new ChunkPos(special.chunk()[0], special.chunk()[1]));
      } else if (special != null && special.biome() != null) {
        centers = search.find(Set.of(lookup.lookupOrThrow(Registries.BIOME)
            .getOrThrow(ResourceKey.create(Registries.BIOME, Identifier.parse(special.biome())))));
      } else {
        centers = search.find(usage.biomes());
      }

      JsonObject record = null;
      String areaKey = null;
      ChunkPos center = null;
      search:
      for (int a = 0; a < centers.size(); a++) {
        center = centers.get(a);
        areaKey = dimension.name() + "/" + center.x() + "/" + center.z();
        Map<Long, ProtoChunk> chunks = terrainCache.get(areaKey);
        if (chunks == null) {
          chunks = terrain(world, center, 1);
          terrainCache.put(areaKey, chunks);
        }
        int attempts = a > 0 ? ATTEMPTS / 2 : special != null ? special.attempts() : ATTEMPTS;
        for (int attempt = 0; attempt < attempts; attempt++) {
          record = attempt(world, chunks, center, feature, usage, attempt);
          if (record.has("error") || !record.getAsJsonArray("sections").isEmpty()) {
            break search;
          }
        }
      }
      Integer index = areaIndex.get(areaKey);
      if (index == null) {
        index = areaIndex.size();
        areaIndex.put(areaKey, index);
        JsonObject area = new JsonObject();
        area.addProperty("dimension", dimension.name());
        area.addProperty("seed", FEATURE_SEED);
        area.addProperty("min_y", dimension.minY());
        area.addProperty("height", dimension.height());
        area.addProperty("x", center.x());
        area.addProperty("z", center.z());
        JsonArray list = new JsonArray();
        for (ProtoChunk chunk : ordered(terrainCache.get(areaKey), center, 1)) {
          list.add(Snapshot.of(chunk).toJson());
        }
        area.add("chunks", list);
        areas.add(area);
      }
      JsonObject o = new JsonObject();
      o.addProperty("name", name);
      o.addProperty("area", index);
      for (Map.Entry<String, com.google.gson.JsonElement> e : record.entrySet()) {
        o.add(e.getKey(), e.getValue());
      }
      if (o.has("error")) {
        System.out.println("feature goldens: " + name + " cannot run: " + o.get("error").getAsString());
      }
      features.add(o);
    }
    root.add("areas", areas);
    root.add("features", features);
  }

  /**
   * Runs a placed feature once on a copy of the area. Attempt {@code k} uses feature seed index
   * {@code k}; features without biome check also move their surface origin around the centre
   * chunk.
   */
  private static JsonObject attempt(
      World world, Map<Long, ProtoChunk> area, ChunkPos center, PlacedFeature feature, Usage usage, int k) {
    Map<Long, ProtoChunk> chunks = copy(area, world.dimension);
    List<ProtoChunk> order = ordered(chunks, center, 1);
    List<Snapshot> before = order.stream().map(Snapshot::of).toList();
    Region region = new Region(world, chunks, center);
    WorldGenLevel level = region.proxy;
    int minX = center.getMinBlockX();
    int minZ = center.getMinBlockZ();
    BlockPos origin;
    if (usage.asDecoration()) {
      origin = new BlockPos(minX, level.getMinY(), minZ);
    } else {
      int x = minX + ((8 + 5 * k) & 15);
      int z = minZ + ((8 + 11 * k) & 15);
      origin = new BlockPos(x, level.getHeight(Heightmap.Types.WORLD_SURFACE, x, z), z);
    }

    JsonObject o = new JsonObject();
    o.add("origin", array(origin.getX(), origin.getY(), origin.getZ()));
    o.addProperty("biome_check", usage.asDecoration());
    o.add("feature_seed", array(k, usage.step()));

    WorldgenRandom random = new WorldgenRandom(new XoroshiroRandomSource(0L));
    long decorationSeed = random.setDecorationSeed(FEATURE_SEED, minX, minZ);
    random.setFeatureSeed(decorationSeed, k, usage.step());
    FeaturePlacer placer = new FeaturePlacer(level, world.generator);
    boolean result;
    try {
      result = usage.asDecoration()
          ? placer.placeWithBiomeCheck(feature, random, origin)
          : placer.place(feature, random, origin);
    } catch (Throwable e) {
      StackTraceElement[] trace = e.getStackTrace();
      o.addProperty("error", e + (trace.length > 0 ? " at " + trace[0] : ""));
      if (System.getenv("FEATURE_GOLDENS_TRACE") != null) {
        e.printStackTrace(System.out);
      }
      return o;
    }
    o.addProperty("result", result);
    JsonArray sections = new JsonArray();
    JsonArray heightmaps = new JsonArray();
    JsonArray post = new JsonArray();
    JsonArray blockTicks = new JsonArray();
    JsonArray fluidTicks = new JsonArray();
    JsonArray blockEntities = new JsonArray();
    for (int c = 0; c < order.size(); c++) {
      ProtoChunk chunk = order.get(c);
      Snapshot after = Snapshot.of(chunk);
      Snapshot was = before.get(c);
      for (int s = 0; s < after.sections.length; s++) {
        if (after.sections[s] != was.sections[s]) {
          sections.add(array(c, s, after.sections[s]));
        }
        if (after.postCount[s] != was.postCount[s] || after.postHash[s] != was.postHash[s]) {
          post.add(array(c, s, after.postCount[s], after.postHash[s]));
        }
      }
      for (int h = 0; h < HEIGHTMAPS.length; h++) {
        if (after.heights[h] != was.heights[h]) {
          heightmaps.add(array(c, h, after.heights[h]));
        }
      }
      long[] ticks = blockTicks(chunk);
      if (ticks[0] > 0) {
        blockTicks.add(array(c, ticks[0], ticks[1]));
      }
      ticks = fluidTicks(chunk);
      if (ticks[0] > 0) {
        fluidTicks.add(array(c, ticks[0], ticks[1]));
      }
      blockEntities.addAll(blockEntities(chunk));
    }
    o.add("sections", sections);
    o.add("heightmaps", heightmaps);
    o.add("post_processing", post);
    o.add("block_entities", blockEntities);
    o.add("block_ticks", blockTicks);
    o.add("fluid_ticks", fluidTicks);
    return o;
  }

  /**
   * Biomes and first decoration step of every placed feature a biome lists; other placed
   * features inherit them from listed features whose configured feature (or a sub-feature of
   * it) is theirs.
   */
  private static Map<PlacedFeature, Usage> usages() {
    Map<PlacedFeature, Set<Holder<Biome>>> biomes = new IdentityHashMap<>();
    Map<PlacedFeature, Integer> steps = new IdentityHashMap<>();
    for (Holder.Reference<Biome> biome : lookup.lookupOrThrow(Registries.BIOME).listElements().toList()) {
      List<HolderSet<PlacedFeature>> features = biome.value().getGenerationSettings().features();
      for (int step = 0; step < features.size(); step++) {
        for (Holder<PlacedFeature> f : features.get(step)) {
          biomes.computeIfAbsent(f.value(), k -> new HashSet<>()).add(biome);
          steps.merge(f.value(), step, Math::min);
        }
      }
    }
    Map<Feature, Set<Holder<Biome>>> configured = new IdentityHashMap<>();
    Map<Feature, Integer> configuredSteps = new IdentityHashMap<>();
    for (Map.Entry<PlacedFeature, Set<Holder<Biome>>> e : biomes.entrySet()) {
      int step = steps.get(e.getKey());
      e.getKey().getFeatures().forEach(f -> {
        configured.computeIfAbsent(f.value(), k -> new HashSet<>()).addAll(e.getValue());
        configuredSteps.merge(f.value(), step, Math::min);
      });
    }
    // Features that only other features place run without biome check, unless their placement
    // has a biome filter (which throws without a top feature).
    Map<PlacedFeature, Usage> out = new IdentityHashMap<>();
    for (Holder.Reference<PlacedFeature> h : lookup.lookupOrThrow(Registries.PLACED_FEATURE).listElements().toList()) {
      PlacedFeature f = h.value();
      if (biomes.containsKey(f)) {
        out.put(f, new Usage(biomes.get(f), steps.get(f), true));
      } else {
        Feature c = f.feature().value();
        boolean biomeFilter = f.placement().stream().anyMatch(m -> m instanceof BiomeFilter);
        out.put(f, new Usage(configured.getOrDefault(c, Set.of()), configuredSteps.getOrDefault(c, 0), biomeFilter));
      }
    }
    return out;
  }

  /**
   * The first {@link #AREAS} chunks (in a fixed spiral over a coarse grid) where each biome is
   * sampled at the centre column at one of the dimension's sample heights.
   */
  private static final class BiomeSearch {
    private final World world;
    private final List<ChunkPos> candidates = new ArrayList<>();
    private final Map<Holder<Biome>, List<Integer>> found = new HashMap<>();

    BiomeSearch(World world) {
      this.world = world;
      Set<Holder<Biome>> possible = world.generator.getBiomeSource().possibleBiomes();
      int complete = 0;
      for (int r = 0; r <= SEARCH_RADIUS && complete < possible.size(); r++) {
        for (int gz = -r; gz <= r; gz++) {
          for (int gx = -r; gx <= r; gx++) {
            if (Math.max(Math.abs(gx), Math.abs(gz)) != r) {
              continue;
            }
            ChunkPos pos = new ChunkPos(gx * SEARCH_STEP, gz * SEARCH_STEP);
            int index = candidates.size();
            candidates.add(pos);
            for (int y : world.dimension.sampleYs()) {
              Holder<Biome> biome = world.biomes.getNoiseBiome(QuartPos.fromBlock(pos.getMinBlockX() + 8),
                  QuartPos.fromBlock(y), QuartPos.fromBlock(pos.getMinBlockZ() + 8));
              List<Integer> list = found.computeIfAbsent(biome, b -> new ArrayList<>());
              if (list.size() < AREAS && !list.contains(index)) {
                list.add(index);
                if (list.size() == AREAS) {
                  complete++;
                }
              }
            }
          }
        }
      }
    }

    /** Up to {@link #AREAS} candidates with one of the biomes, else the origin chunk. */
    List<ChunkPos> find(Set<Holder<Biome>> biomes) {
      java.util.TreeSet<Integer> indices = new java.util.TreeSet<>();
      for (Holder<Biome> b : biomes) {
        indices.addAll(found.getOrDefault(b, List.of()));
      }
      if (indices.isEmpty()) {
        if (!biomes.isEmpty()) {
          System.out.println("feature goldens: no chunk found for biomes " + biomes.stream()
              .map(b -> b.unwrapKey().orElseThrow().identifier().toString()).sorted().toList()
              + " in " + world.dimension.name());
        }
        return List.of(new ChunkPos(0, 0));
      }
      return indices.stream().limit(AREAS).map(candidates::get).toList();
    }
  }

  // ---------------------------------------------------------------------------------------------
  // Part 2: full decoration

  private static JsonObject decoration(DecorationArea spec) throws Exception {
    World world = world(spec.dimension(), spec.seed());
    ChunkPos center = new ChunkPos(spec.x(), spec.z());
    int radius = spec.radius();
    Map<Long, ProtoChunk> chunks = terrain(world, center, radius + 1);
    int sign = spec.reversed() ? -1 : 1;
    for (int dz = -radius; dz <= radius; dz++) {
      for (int dx = -radius; dx <= radius; dx++) {
        ChunkPos pos = new ChunkPos(center.x() + sign * dx, center.z() + sign * dz);
        ProtoChunk chunk = chunks.get(pos.pack());
        Region region = new Region(world, chunks, pos);
        StructureManager structures = new StructureManager(region.proxy, new WorldOptions(spec.seed(), false, false), null);
        world.generator.applyBiomeDecoration(region.proxy, chunk, structures);
        chunk.setPersistedStatus(ChunkStatus.FEATURES);
      }
    }
    JsonObject o = new JsonObject();
    o.addProperty("dimension", spec.dimension().name());
    o.addProperty("seed", spec.seed());
    o.addProperty("min_y", spec.dimension().minY());
    o.addProperty("height", spec.dimension().height());
    o.addProperty("x", spec.x());
    o.addProperty("z", spec.z());
    o.addProperty("radius", radius);
    o.addProperty("reversed", spec.reversed());
    JsonArray list = new JsonArray();
    for (ProtoChunk chunk : ordered(chunks, center, radius + 1)) {
      JsonObject c = Snapshot.of(chunk).toJson();
      c.add("block_entities", blockEntities(chunk));
      long[] ticks = blockTicks(chunk);
      c.add("block_ticks", array(ticks[0], ticks[1]));
      ticks = fluidTicks(chunk);
      c.add("fluid_ticks", array(ticks[0], ticks[1]));
      list.add(c);
    }
    o.add("chunks", list);
    return o;
  }

  // ---------------------------------------------------------------------------------------------
  // Terrain

  /**
   * Terrain (status {@code terrain}, final heightmaps primed) for the chunks within {@code radius}
   * of {@code center}, with biomes one chunk further out, as {@link ChunkGoldens} generates it.
   */
  private static Map<Long, ProtoChunk> terrain(World world, ChunkPos center, int radius) throws Exception {
    LevelHeightAccessor height = LevelHeightAccessor.create(world.dimension.minY(), world.dimension.height());
    Map<Long, ProtoChunk> all = new HashMap<>();
    for (int dz = -radius - 1; dz <= radius + 1; dz++) {
      for (int dx = -radius - 1; dx <= radius + 1; dx++) {
        ChunkPos pos = new ChunkPos(center.x() + dx, center.z() + dz);
        ProtoChunk chunk = new ProtoChunk(pos, UpgradeData.EMPTY, height, factory, null);
        world.generator.createBiomes(world.randomState, Blender.empty(), null, chunk).join();
        chunk.setPersistedStatus(ChunkStatus.BIOMES);
        all.put(pos.pack(), chunk);
      }
    }
    BiomeManager biomeManager = new BiomeManager(
        (qx, qy, qz) -> all.get(ChunkPos.pack(qx >> 2, qz >> 2)).getNoiseBiome(qx, qy, qz),
        BiomeManager.obfuscateSeed(world.seed));
    Field pickerField = NoiseBasedChunkGenerator.class.getDeclaredField("globalFluidPicker");
    pickerField.setAccessible(true);
    @SuppressWarnings("unchecked")
    Aquifer.FluidPicker picker = ((Supplier<Aquifer.FluidPicker>) pickerField.get(world.generator)).get();
    MaterialRule materialRule = world.settings.materialRule().value();
    Method doFill = generatorMethod("doFill", NoiseChunk.class, ChunkAccess.class);
    Method buildSurface = generatorMethod("buildSurface", ChunkAccess.class, NoiseChunk.class, RandomState.class,
        BiomeManager.class, Set.class, MaterialRule.class);
    Method generateCarvers = generatorMethod("generateCarvers", ChunkAccess.class, Blender.class, NoiseChunk.class,
        RandomState.class, BiomeManager.class, net.minecraft.server.level.WorldGenRegion.class, MaterialRule.class);

    Map<Long, ProtoChunk> out = new HashMap<>();
    for (int dz = -radius; dz <= radius; dz++) {
      for (int dx = -radius; dx <= radius; dx++) {
        ChunkPos pos = new ChunkPos(center.x() + dx, center.z() + dz);
        ProtoChunk chunk = all.get(pos.pack());
        Set<Holder<Biome>> possibleBiomes = new HashSet<>();
        for (int oz = -1; oz <= 1; oz++) {
          for (int ox = -1; ox <= 1; ox++) {
            all.get(ChunkPos.pack(pos.x() + ox, pos.z() + oz)).collectBiomesInPalette(possibleBiomes);
          }
        }
        DensityVolume volume = new DensityVolume(16, world.settings.noiseSettings().height(), 16,
            pos.getMinBlockX(), world.settings.noiseSettings().minY(), pos.getMinBlockZ());
        try (NoiseChunk noiseChunk = new NoiseChunk(world.randomState, null, world.settings, picker, Blender.empty(), volume)) {
          doFill.invoke(world.generator, noiseChunk, chunk);
          buildSurface.invoke(world.generator, chunk, noiseChunk, world.randomState, biomeManager, possibleBiomes, materialRule);
          generateCarvers.invoke(world.generator, chunk, Blender.empty(), noiseChunk, world.randomState, biomeManager, null, materialRule);
        }
        chunk.setPersistedStatus(ChunkStatus.TERRAIN);
        Heightmap.primeHeightmaps(chunk, FINAL_HEIGHTMAPS);
        out.put(pos.pack(), chunk);
      }
    }
    return out;
  }

  static Method generatorMethod(String name, Class<?>... types) throws NoSuchMethodException {
    Method m = NoiseBasedChunkGenerator.class.getDeclaredMethod(name, types);
    m.setAccessible(true);
    return m;
  }

  /** A fresh copy of terrain chunks (blocks, biomes, post-processing marks). */
  private static Map<Long, ProtoChunk> copy(Map<Long, ProtoChunk> chunks, Dimension dimension) {
    LevelHeightAccessor height = LevelHeightAccessor.create(dimension.minY(), dimension.height());
    Map<Long, ProtoChunk> out = new HashMap<>();
    for (Map.Entry<Long, ProtoChunk> e : chunks.entrySet()) {
      ProtoChunk source = e.getValue();
      LevelChunkSection[] sections = Arrays.stream(source.getSections()).map(LevelChunkSection::copy)
          .toArray(LevelChunkSection[]::new);
      ProtoChunk chunk = new ProtoChunk(source.getPos(), UpgradeData.EMPTY, sections, new ProtoChunkTicks<>(),
          new ProtoChunkTicks<>(), height, factory, null);
      ShortList[] post = source.getPostProcessing();
      for (int i = 0; i < post.length; i++) {
        if (post[i] != null) {
          chunk.addPackedPostProcess(new ShortArrayList(post[i]), i);
        }
      }
      chunk.setPersistedStatus(ChunkStatus.TERRAIN);
      Heightmap.primeHeightmaps(chunk, FINAL_HEIGHTMAPS);
      out.put(e.getKey(), chunk);
    }
    return out;
  }

  /** The chunks within {@code radius} of {@code center}, row-major ({@code z} outer). */
  static List<ProtoChunk> ordered(Map<Long, ProtoChunk> chunks, ChunkPos center, int radius) {
    List<ProtoChunk> out = new ArrayList<>();
    for (int dz = -radius; dz <= radius; dz++) {
      for (int dx = -radius; dx <= radius; dx++) {
        out.add(chunks.get(ChunkPos.pack(center.x() + dx, center.z() + dz)));
      }
    }
    return out;
  }

  // ---------------------------------------------------------------------------------------------
  // Recording

  /** Per-section block hashes, final heightmap hashes and post-processing marks of a chunk. */
  static final class Snapshot {
    long[] sections;
    long[] heights;
    int[] postCount;
    long[] postHash;

    static Snapshot of(ProtoChunk chunk) {
      Snapshot s = new Snapshot();
      LevelChunkSection[] sections = chunk.getSections();
      s.sections = new long[sections.length];
      for (int i = 0; i < sections.length; i++) {
        long hash = 0;
        for (int y = 0; y < 16; y++) {
          for (int z = 0; z < 16; z++) {
            for (int x = 0; x < 16; x++) {
              hash = hash * 31 + Block.getId(sections[i].getBlockState(x, y, z));
            }
          }
        }
        s.sections[i] = hash;
      }
      s.heights = new long[HEIGHTMAPS.length];
      for (int h = 0; h < HEIGHTMAPS.length; h++) {
        long hash = 0;
        for (int z = 0; z < 16; z++) {
          for (int x = 0; x < 16; x++) {
            hash = hash * 31 + chunk.getHeight(HEIGHTMAPS[h], x, z);
          }
        }
        s.heights[h] = hash;
      }
      ShortList[] post = chunk.getPostProcessing();
      s.postCount = new int[sections.length];
      s.postHash = new long[sections.length];
      for (int i = 0; i < sections.length; i++) {
        long hash = 0;
        if (post[i] != null) {
          for (int j = 0; j < post[i].size(); j++) {
            hash = hash * 31 + post[i].getShort(j);
          }
          s.postCount[i] = post[i].size();
        }
        s.postHash[i] = hash;
      }
      return s;
    }

    JsonObject toJson() {
      JsonObject o = new JsonObject();
      JsonArray sections = new JsonArray();
      for (long h : this.sections) {
        sections.add(h);
      }
      o.add("sections", sections);
      JsonArray heights = new JsonArray();
      for (long h : this.heights) {
        heights.add(h);
      }
      o.add("heightmaps", heights);
      JsonArray post = new JsonArray();
      for (int i = 0; i < postCount.length; i++) {
        post.add(array(postCount[i], postHash[i]));
      }
      o.add("post_processing", post);
      return o;
    }
  }

  /**
   * Block entities of a chunk as {@code [x, y, z, id]} sorted by y, z, x: the type of a created
   * block entity, else the {@code id} of the pending tag ({@code DUMMY} for placeholders).
   */
  private static JsonArray blockEntities(ProtoChunk chunk) {
    List<BlockPos> positions = new ArrayList<>(chunk.getBlockEntitiesPos());
    positions.sort(Comparator.<BlockPos>comparingInt(BlockPos::getY).thenComparingInt(BlockPos::getZ)
        .thenComparingInt(BlockPos::getX));
    JsonArray out = new JsonArray();
    for (BlockPos pos : positions) {
      BlockEntity entity = chunk.getBlockEntity(pos);
      String id = entity != null
          ? BuiltInRegistries.BLOCK_ENTITY_TYPE.getKey(entity.getType()).toString()
          : chunk.getBlockEntityNbt(pos).getStringOr("id", "");
      JsonArray e = new JsonArray();
      e.add(pos.getX());
      e.add(pos.getY());
      e.add(pos.getZ());
      e.add(id);
      out.add(e);
    }
    return out;
  }

  /** Count and hash (type name, x, y, z, priority, in scheduling order) of the block ticks. */
  @SuppressWarnings("unchecked")
  static long[] blockTicks(ProtoChunk chunk) {
    return tickHash(((ProtoChunkTicks<Block>) chunk.getBlockTicks()).scheduledTicks(),
        t -> BuiltInRegistries.BLOCK.getKey(t).toString());
  }

  /** Count and hash of the fluid ticks, as {@link #blockTicks}. */
  @SuppressWarnings("unchecked")
  static long[] fluidTicks(ProtoChunk chunk) {
    return tickHash(((ProtoChunkTicks<Fluid>) chunk.getFluidTicks()).scheduledTicks(),
        t -> BuiltInRegistries.FLUID.getKey(t).toString());
  }

  private static <T> long[] tickHash(List<SavedTick<T>> ticks, java.util.function.Function<T, String> name) {
    long hash = 0;
    for (SavedTick<T> tick : ticks) {
      hash = hash * 31 + name.apply(tick.type()).hashCode();
      hash = hash * 31 + tick.pos().getX();
      hash = hash * 31 + tick.pos().getY();
      hash = hash * 31 + tick.pos().getZ();
      hash = hash * 31 + tick.priority().getValue();
    }
    return new long[] {ticks.size(), hash};
  }

  static JsonArray array(long... values) {
    JsonArray a = new JsonArray();
    for (long v : values) {
      a.add(v);
    }
    return a;
  }

  // ---------------------------------------------------------------------------------------------
  // WorldGenLevel without a server

  /**
   * {@code WorldGenRegion} of the features step over the 3×3 chunks around {@code center}: reads
   * and writes within one chunk of the centre, {@code VOID_AIR} outside the build height, {@code
   * DUMMY} block entity tags, post-processing marks, ticks into the chunks' {@code
   * ProtoChunkTicks}, the region random of the centre chunk and a biome manager over the chunks'
   * noise biomes, no light data. Entities are dropped ({@code addFreshEntity} keeps nothing). Anything else
   * Vanilla's region supports and the feature code does not use throws.
   */
  static final class Region implements InvocationHandler {
    final World world;
    final Map<Long, ProtoChunk> chunks;
    final ChunkPos center;
    final WorldGenLevel proxy;
    final BiomeManager biomeManager;
    final RandomSource random;
    final AtomicLong subTickCount = new AtomicLong();
    final WorldGenTickAccess<Block> blockTicks;
    final WorldGenTickAccess<Fluid> fluidTicks;
    LevelLightEngine lightEngine;
    /** {@code WorldGenRegion}'s attribute system: the dimension and biome layers. */
    net.minecraft.world.attribute.EnvironmentAttributeSystem environmentAttributes;
    /**
     * Vanilla's chunk access rule, or null for the rule of the feature goldens (any status within
     * one chunk of the centre): the highest status a chunk may be requested at, by chessboard
     * distance from the centre ({@code ChunkStep.directDependencies()} of the step).
     */
    final ChunkStatus[] dependencies;
    /** The entities added with {@code addFreshEntity}, in order (not stored in the chunks). */
    final List<net.minecraft.world.entity.Entity> entities = new ArrayList<>();

    Region(World world, Map<Long, ProtoChunk> chunks, ChunkPos center) {
      this(world, chunks, center, null);
    }

    Region(World world, Map<Long, ProtoChunk> chunks, ChunkPos center, ChunkStatus[] dependencies) {
      this.world = world;
      this.chunks = chunks;
      this.center = center;
      this.dependencies = dependencies;
      this.proxy = (WorldGenLevel) Proxy.newProxyInstance(
          WorldGenLevel.class.getClassLoader(), new Class<?>[] {WorldGenLevel.class}, this);
      this.biomeManager = new BiomeManager(proxy, BiomeManager.obfuscateSeed(world.seed));
      this.random = world.randomState.getOrCreateRandomFactory(Identifier.withDefaultNamespace("worldgen_region_random"))
          .at(center.getWorldPosition());
      this.blockTicks = new WorldGenTickAccess<>(pos -> chunk(pos).getBlockTicks());
      this.fluidTicks = new WorldGenTickAccess<>(pos -> chunk(pos).getFluidTicks());
    }

    /**
     * A light engine without light data: the chunks of a features step are not lit yet, so sky
     * light reads 15 where the dimension has sky light and block light 0, as in Vanilla.
     */
    LevelLightEngine lightEngine() {
      if (lightEngine == null) {
        lightEngine = new LevelLightEngine(new LightChunkGetter() {
          @Override
          public LightChunk getChunkForLighting(int x, int z) {
            return null;
          }

          @Override
          public BlockGetter getLevel() {
            return proxy;
          }
        }, true, world.dimensionType.hasSkyLight());
      }
      return lightEngine;
    }

    /** {@code WorldGenRegion.getChunk(x, z, status, true)}. */
    ChunkAccess chunk(int x, int z, ChunkStatus status) {
      if (dependencies == null) {
        return chunk(x, z);
      }
      int distance = center.getChessboardDistance(x, z);
      ChunkStatus allowed = distance < dependencies.length ? dependencies[distance] : null;
      ChunkAccess chunk = allowed != null ? chunks.get(ChunkPos.pack(x, z)) : null;
      if (chunk != null && status.isOrBefore(allowed)) {
        return chunk;
      }
      throw new IllegalStateException("Requested chunk unavailable during world generation: [" + x + ", " + z
          + "] at " + status + " while generating [" + center.x() + ", " + center.z() + "]");
    }

    ChunkAccess chunk(int x, int z) {
      if (dependencies != null) {
        return chunk(x, z, ChunkStatus.EMPTY);
      }
      if (Math.max(Math.abs(x - center.x()), Math.abs(z - center.z())) > 1) {
        throw new IllegalStateException("Requested chunk unavailable during world generation: [" + x + ", " + z
            + "] while generating [" + center.x() + ", " + center.z() + "]");
      }
      return chunks.get(ChunkPos.pack(x, z));
    }

    ChunkAccess chunk(BlockPos pos) {
      return chunk(pos.getX() >> 4, pos.getZ() >> 4);
    }

    boolean withinWriteZone(BlockPos pos) {
      return Math.abs(center.x() - (pos.getX() >> 4)) <= 1 && Math.abs(center.z() - (pos.getZ() >> 4)) <= 1;
    }

    /** {@code WorldGenRegion.setBlock}. */
    boolean setBlock(BlockPos pos, BlockState state, int flags) {
      if (!withinWriteZone(pos)) {
        return false;
      }
      ChunkAccess chunk = chunk(pos);
      BlockState old = chunk.setBlockState(pos, state, flags);
      if (state.hasBlockEntity()) {
        CompoundTag tag = new CompoundTag();
        tag.putInt("x", pos.getX());
        tag.putInt("y", pos.getY());
        tag.putInt("z", pos.getZ());
        tag.putString("id", "DUMMY");
        chunk.setBlockEntityNbt(tag);
      } else if (old != null && old.hasBlockEntity()) {
        chunk.removeBlockEntity(pos);
      }
      if ((flags & Block.UPDATE_KNOWN_SHAPE) == 0) {
        BlockPos post = state.getPostProcessPos(proxy, pos);
        if (post != null) {
          chunk(post).markPosForPostProcessing(post);
        }
      }
      return true;
    }

    /** {@code WorldGenRegion.getBlockEntity}. */
    BlockEntity blockEntity(BlockPos pos) {
      ChunkAccess chunk = chunk(pos);
      BlockEntity entity = chunk.getBlockEntity(pos);
      if (entity != null) {
        return entity;
      }
      CompoundTag tag = chunk.getBlockEntityNbt(pos);
      BlockState state = chunk.getBlockState(pos);
      if (tag != null) {
        if ("DUMMY".equals(tag.getStringOr("id", ""))) {
          if (!state.hasBlockEntity()) {
            return null;
          }
          entity = ((EntityBlock) state.getBlock()).newBlockEntity(pos, state);
        } else {
          entity = BlockEntity.loadStatic(pos, state, tag, registryAccess);
        }
        if (entity != null) {
          chunk.setBlockEntity(entity);
          return entity;
        }
      }
      return null;
    }

    @Override
    @SuppressWarnings("unchecked")
    public Object invoke(Object self, Method method, Object[] args) throws Throwable {
      int n = args == null ? 0 : args.length;
      switch (method.getName()) {
        case "hashCode":
          if (n == 0) {
            return System.identityHashCode(self);
          }
          break;
        case "equals":
          if (n == 1) {
            return self == args[0];
          }
          break;
        case "toString":
          if (n == 0) {
            return "FeatureRegion[" + center + "]";
          }
          break;
        case "getSeed":
          return world.seed;
        case "setCurrentlyGenerating":
          return null;
        case "getChunk":
          if (n == 2 && args[0] instanceof Integer) {
            return chunk((int) args[0], (int) args[1]);
          }
          if (n == 4) {
            return chunk((int) args[0], (int) args[1], (ChunkStatus) args[2]);
          }
          break;
        case "hasChunk":
          return center.getChessboardDistance((int) args[0], (int) args[1])
              < (dependencies != null ? dependencies.length : 9);
        case "getBlockState":
          return chunk((BlockPos) args[0]).getBlockState((BlockPos) args[0]);
        case "getFluidState":
          return chunk((BlockPos) args[0]).getFluidState((BlockPos) args[0]);
        case "getNearestPlayer":
          if (n == 5) {
            return null;
          }
          break;
        case "getSkyDarken":
          return 0;
        case "getLightEngine":
          return lightEngine();
        case "getBiomeManager":
          return biomeManager;
        case "getUncachedNoiseBiome":
          return world.biomes.getNoiseBiome((int) args[0], (int) args[1], (int) args[2]);
        case "destroyBlock":
          if (n == 4) {
            BlockPos pos = (BlockPos) args[0];
            return !proxy.getBlockState(pos).isAir() && setBlock(pos, Blocks.AIR.defaultBlockState(), 3);
          }
          break;
        case "getBlockEntity":
          if (n == 1) {
            return blockEntity((BlockPos) args[0]);
          }
          break;
        case "ensureCanWrite":
          return withinWriteZone((BlockPos) args[0]);
        case "setBlock":
          if (n == 4) {
            return setBlock((BlockPos) args[0], (BlockState) args[1], (int) args[2]);
          }
          break;
        case "addFreshEntity":
          entities.add((net.minecraft.world.entity.Entity) args[0]);
          return true;
        case "removeBlock":
          return setBlock((BlockPos) args[0], Blocks.AIR.defaultBlockState(), 3);
        case "isClientSide":
          return false;
        case "getLevel":
          return world.level;
        case "getLevelData":
          return world.level.getLevelData();
        case "environmentAttributes":
          if (environmentAttributes == null) {
            environmentAttributes = net.minecraft.world.attribute.EnvironmentAttributeSystem.builder()
                .addStaticLayers(proxy).build();
          }
          return environmentAttributes;
        case "getCurrentDifficultyAt":
          if (n == 1) {
            BlockPos pos = (BlockPos) args[0];
            if (!proxy.hasChunk(pos.getX() >> 4, pos.getZ() >> 4)) {
              throw new RuntimeException("We are asking a region for a chunk out of bound");
            }
            return new DifficultyInstance(world.level.getDifficulty(), world.level.getOverworldClockTime(), 0L,
                world.level.getMoonBrightness(pos));
          }
          break;
        case "registryAccess":
          return registryAccess;
        case "enabledFeatures":
          return FeatureFlags.VANILLA_SET;
        case "getServer":
          return world.level.getServer();
        case "getChunkSource":
          return world.level.getChunkSource();
        case "getBlockTicks":
          return blockTicks;
        case "getFluidTicks":
          return fluidTicks;
        case "getGameTime":
          return 0L;
        case "getSeaLevel":
          return world.settings.seaLevel();
        case "getRandom":
          return random;
        case "getHeight":
          if (n == 0) {
            return world.dimension.height();
          }
          if (n == 3) {
            int x = (int) args[1];
            int z = (int) args[2];
            return chunk(x >> 4, z >> 4).getHeight((Heightmap.Types) args[0], x & 15, z & 15) + 1;
          }
          break;
        case "getMinY":
          return world.dimension.minY();
        case "playSound", "addParticle", "levelEvent", "gameEvent":
          return null;
        case "dimensionType":
          return world.dimensionType;
        case "isStateAtPosition":
          return ((Predicate<BlockState>) args[1]).test(proxy.getBlockState((BlockPos) args[0]));
        case "isFluidAtPosition":
          return ((Predicate<FluidState>) args[1]).test(proxy.getFluidState((BlockPos) args[0]));
        case "getEntities", "players":
          return Collections.emptyList();
        case "nextSubTickCount":
          return subTickCount.getAndIncrement();
        default:
          break;
      }
      if (method.isDefault()) {
        return InvocationHandler.invokeDefault(self, method, args);
      }
      throw new UnsupportedOperationException("WorldGenLevel." + method.getName() + " is not supported by the harness");
    }
  }

  /**
   * {@code getLevel()} of the harness region: a {@link ServerLevel} allocated without its
   * constructor; only the methods overridden here work.
   */
  static final class FakeLevel extends ServerLevel {
    World world;
    /**
     * {@code structureManager()}: the server level's manager, which reads chunks anywhere (set by
     * the structure goldens while they decorate; null otherwise).
     */
    StructureManager structures;
    net.minecraft.world.attribute.EnvironmentAttributeSystem attributes;
    MinecraftServer server;
    FakeChunkCache chunkCache;
    int entityIds;

    @SuppressWarnings("DataFlowIssue")
    private FakeLevel() {
      super(null, null, null, null, null, null, false, 0L, null, false);
    }

    static FakeLevel create(World world) throws Exception {
      FakeLevel level = (FakeLevel) allocate(FakeLevel.class);
      level.world = world;
      DedicatedServer server = (DedicatedServer) allocate(DedicatedServer.class);
      setField(MinecraftServer.class, server, "structureTemplateManager", templates);
      level.server = server;
      FakeChunkCache cache = (FakeChunkCache) allocate(FakeChunkCache.class);
      cache.generator = world.generator;
      cache.randomState = world.randomState;
      level.chunkCache = cache;
      // Entity constructors read the field directly (getSeaLevel).
      setField(ServerLevel.class, level, "chunkSource", cache);
      return level;
    }

    @Override
    public MinecraftServer getServer() {
      return server;
    }

    @Override
    public String toString() {
      return "FakeLevel[" + world.dimension.name() + "]";
    }

    @Override
    public StructureManager structureManager() {
      return structures;
    }

    @Override
    public ServerChunkCache getChunkSource() {
      return chunkCache;
    }

    @Override
    public RegistryAccess registryAccess() {
      return registryAccess;
    }

    @Override
    public FeatureFlagSet enabledFeatures() {
      return FeatureFlags.VANILLA_SET;
    }

    @Override
    public int getNextEntityId() {
      return ++entityIds;
    }

    @Override
    public long getSeed() {
      return world.seed;
    }

    @Override
    public DimensionType dimensionType() {
      return world.dimensionType;
    }

    @Override
    public int getMinY() {
      return world.dimension.minY();
    }

    @Override
    public int getHeight() {
      return world.dimension.height();
    }

    @Override
    public boolean isClientSide() {
      return false;
    }

    /**
     * Level data of a fresh dedicated server: difficulty {@code easy} (the default of {@code
     * server.properties}), time 0.
     */
    @Override
    public LevelData getLevelData() {
      return new LevelData() {
        @Override
        public LevelData.RespawnData getRespawnData() {
          return LevelData.RespawnData.DEFAULT;
        }

        @Override
        public long getGameTime() {
          return 0L;
        }

        @Override
        public boolean isHardcore() {
          return false;
        }

        @Override
        public Difficulty getDifficulty() {
          return Difficulty.EASY;
        }

        @Override
        public boolean isDifficultyLocked() {
          return false;
        }
      };
    }

    @Override
    public Difficulty getDifficulty() {
      return Difficulty.EASY;
    }

    /** The dimension's constant attributes only (entity brains read them while constructed). */
    @Override
    public net.minecraft.world.attribute.EnvironmentAttributeSystem environmentAttributes() {
      if (attributes == null) {
        attributes = net.minecraft.world.attribute.EnvironmentAttributeSystem.builder()
            .addConstantLayer(world.dimensionType.attributes()).build();
      }
      return attributes;
    }

    @Override
    public long getOverworldClockTime() {
      return 0L;
    }

    /** The moon brightness at time 0 (full moon). */
    @Override
    public float getMoonBrightness(BlockPos pos) {
      return DimensionType.MOON_BRIGHTNESS_PER_PHASE[0];
    }

    /**
     * Sounds are not generation: the end city ship's item frame plays one when it gets its
     * elytra ({@code ItemFrame.setItem}); the level's own sound seed random is not set here.
     */
    @Override
    public void playSound(
        net.minecraft.world.entity.Entity except, double x, double y, double z,
        net.minecraft.sounds.SoundEvent sound, net.minecraft.sounds.SoundSource source, float volume, float pitch) {}
  }

  /** {@code getChunkSource()} of {@link FakeLevel}: the generator and random state. */
  private static final class FakeChunkCache extends ServerChunkCache {
    ChunkGenerator generator;
    RandomState randomState;

    @SuppressWarnings("DataFlowIssue")
    private FakeChunkCache() {
      super(null, null, null, null, null, null, 0, 0, false, null);
    }

    @Override
    public ChunkGenerator getGenerator() {
      return generator;
    }

    @Override
    public RandomState randomState() {
      return randomState;
    }
  }

  // ---------------------------------------------------------------------------------------------
  // Registries and templates

  /**
   * The world registries loaded from the server jar's data pack like {@code WorldLoader} loads
   * them (static registry tags applied), together with the static registries.
   *
   * <p>{@code VanillaRegistries.createWorldLookup()} cannot serve features: its tag holder sets
   * are construction placeholders that throw when a feature iterates them (for example the
   * {@code random_block} provider of the coral features).
   */
  private static RegistryAccess loadRegistries(ResourceManager resources) {
    LayeredRegistryAccess<RegistryLayer> layers = RegistryLayer.createRegistryAccess();
    List<Registry.PendingTags<?>> staticTags =
        TagLoader.loadTagsForExistingRegistries(resources, layers.getLayer(RegistryLayer.STATIC));
    RegistryAccess.Frozen context = layers.getAccessForLoading(RegistryLayer.WORLD);
    List<HolderLookup.RegistryLookup<?>> contextLookups = TagLoader.buildUpdatedLookups(context, staticTags);
    RegistryAccess.Frozen world =
        RegistryDataLoader.load(resources, contextLookups, RegistryDataLoader.WORLD_REGISTRIES, Runnable::run).join();
    staticTags.forEach(Registry.PendingTags::apply);
    return new RegistryAccess.ImmutableRegistryAccess(Stream.concat(context.registries(), world.registries())).freeze();
  }

  /**
   * A {@link StructureTemplateManager} that loads templates from the server jar's data pack only
   * (allocated without its constructor, which needs a world directory).
   */
  private static StructureTemplateManager templateManager(ResourceManager resources) throws Exception {
    HolderGetter<Block> blocks = BuiltInRegistries.BLOCK;
    ResourceManagerTemplateSource source = new ResourceManagerTemplateSource(
        DataFixers.getDataFixer(), blocks, resources, new FileToIdConverter("structure", ".nbt"));
    StructureTemplateManager manager = (StructureTemplateManager) allocate(StructureTemplateManager.class);
    setField(StructureTemplateManager.class, manager, "structureRepository", new ConcurrentHashMap<>());
    setField(StructureTemplateManager.class, manager, "resourceManagerSource", source);
    setField(StructureTemplateManager.class, manager, "sources", List.of(source));
    return manager;
  }

  private static Object allocate(Class<?> type) throws Exception {
    Field f = Class.forName("sun.misc.Unsafe").getDeclaredField("theUnsafe");
    f.setAccessible(true);
    Object unsafe = f.get(null);
    return unsafe.getClass().getMethod("allocateInstance", Class.class).invoke(unsafe, type);
  }

  private static void setField(Class<?> owner, Object target, String name, Object value) throws Exception {
    Field f = owner.getDeclaredField(name);
    f.setAccessible(true);
    f.set(target, value);
  }
}

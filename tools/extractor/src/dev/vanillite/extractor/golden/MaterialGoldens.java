// Copyright Hannes Thöne. Licensed under the Apache License, Version 2.0.
package dev.vanillite.extractor.golden;

import com.google.gson.JsonArray;
import com.google.gson.JsonElement;
import com.google.gson.JsonObject;
import com.google.gson.JsonParser;
import com.mojang.serialization.DataResult;
import com.mojang.serialization.JsonOps;
import java.lang.reflect.Field;
import java.lang.reflect.Method;
import net.minecraft.core.Holder;
import net.minecraft.core.HolderGetter;
import net.minecraft.core.HolderLookup;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.core.registries.Registries;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.resources.ResourceKey;
import net.minecraft.world.level.LevelHeightAccessor;
import net.minecraft.world.level.biome.Biomes;
import net.minecraft.world.level.biome.FixedBiomeSource;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.levelgen.NoiseBasedChunkGenerator;
import net.minecraft.world.level.levelgen.NoiseGeneratorSettings;
import net.minecraft.world.level.levelgen.RandomState;
import net.minecraft.world.level.levelgen.VerticalAnchor;
import net.minecraft.world.level.levelgen.WorldGenerationContext;
import net.minecraft.world.level.levelgen.material.MaterialSystem;
import net.minecraft.world.level.levelgen.synth.NormalNoise;

/**
 * Goldens for the material (surface rule) system: clay bands, vertical anchors with the world
 * generation context, and the noise based per-column helpers of {@code MaterialSystem}.
 */
public final class MaterialGoldens {
  private static final long[] SEEDS = {0L, 1L, 42L, -7302402383914416L};

  private static final String[] ANCHORS = {
    "{\"absolute\": 30}",
    "{\"absolute\": -2032}",
    "{\"absolute\": 2031}",
    "{\"absolute\": 2032}",
    "{\"above_bottom\": 0}",
    "{\"above_bottom\": 5}",
    "{\"below_top\": 0}",
    "{\"below_top\": 5}",
    "{\"relative_to_sea_level\": 0}",
    "{\"relative_to_sea_level\": -2}",
    "{}",
    "{\"absolute\": 1, \"below_top\": 2}",
    "{\"above_bottom\": 1, \"below_top\": 2, \"relative_to_sea_level\": 3}",
    "{\"absolute\": 1.5}",
  };

  private static final int[][] COLUMNS = {
    {0, 0}, {1, 0}, {0, 1}, {15, 15}, {-1, -1}, {-17, 33}, {123, -456}, {1000, 1000},
    {-12345, 6789}, {30000, -30000}, {7, 99999}, {-2048, 512}
  };

  private MaterialGoldens() {}

  public static JsonElement extract() throws Exception {
    HolderLookup.Provider lookup = VanillaRegistries.createWorldLookup();
    HolderGetter<NormalNoise> noises = lookup.lookupOrThrow(Registries.NOISE);
    HolderLookup<NoiseGeneratorSettings> allSettings = lookup.lookupOrThrow(Registries.NOISE_SETTINGS);
    JsonObject root = new JsonObject();

    Field clayBandsField = MaterialSystem.class.getDeclaredField("clayBands");
    clayBandsField.setAccessible(true);
    Method surfaceDepth = MaterialSystem.class.getDeclaredMethod("getSurfaceDepth", int.class, int.class);
    surfaceDepth.setAccessible(true);
    Method surfaceSecondary = MaterialSystem.class.getDeclaredMethod("getSurfaceSecondary", int.class, int.class);
    surfaceSecondary.setAccessible(true);
    Method band = MaterialSystem.class.getDeclaredMethod("getBand", int.class, int.class, int.class);
    band.setAccessible(true);

    // Clay bands of the overworld (Xoroshiro) and the nether (legacy random).
    JsonArray clayBands = new JsonArray();
    for (ResourceKey<NoiseGeneratorSettings> key : new ResourceKey[] {NoiseGeneratorSettings.OVERWORLD, NoiseGeneratorSettings.NETHER}) {
      NoiseGeneratorSettings settings = allSettings.getOrThrow(key).value();
      for (long seed : SEEDS) {
        MaterialSystem system = RandomState.create(noises, seed, settings).surfaceSystem();
        JsonObject o = new JsonObject();
        o.addProperty("seed", seed);
        o.addProperty("legacy", settings.useLegacyRandomSource());
        JsonArray bands = new JsonArray();
        for (BlockState state : (BlockState[]) clayBandsField.get(system)) {
          bands.add(BuiltInRegistries.BLOCK.getKey(state.getBlock()).toString());
        }
        o.add("bands", bands);
        clayBands.add(o);
      }
    }
    root.add("clay_bands", clayBands);

    // World generation contexts of noise generators in levels of several heights, and anchors
    // resolved in them.
    Holder<net.minecraft.world.level.biome.Biome> plains = lookup.lookupOrThrow(Registries.BIOME).getOrThrow(Biomes.PLAINS);
    Object[][] contexts = {
      {NoiseGeneratorSettings.OVERWORLD, -64, 384},
      {NoiseGeneratorSettings.NETHER, 0, 256},
      {NoiseGeneratorSettings.END, 0, 256},
      {NoiseGeneratorSettings.OVERWORLD, 0, 256},
      {NoiseGeneratorSettings.CAVES, -64, 512},
    };
    JsonArray contextResults = new JsonArray();
    WorldGenerationContext[] resolved = new WorldGenerationContext[contexts.length];
    for (int i = 0; i < contexts.length; i++) {
      @SuppressWarnings("unchecked")
      Holder<NoiseGeneratorSettings> settings = allSettings.getOrThrow((ResourceKey<NoiseGeneratorSettings>) contexts[i][0]);
      int minY = (Integer) contexts[i][1];
      int height = (Integer) contexts[i][2];
      NoiseBasedChunkGenerator generator = new NoiseBasedChunkGenerator(new FixedBiomeSource(plains), settings);
      WorldGenerationContext context = new WorldGenerationContext(generator, LevelHeightAccessor.create(minY, height));
      resolved[i] = context;
      JsonObject o = new JsonObject();
      o.addProperty("generator_min_y", settings.value().noiseSettings().minY());
      o.addProperty("generator_height", settings.value().noiseSettings().height());
      o.addProperty("sea_level", settings.value().seaLevel());
      o.addProperty("level_min_y", minY);
      o.addProperty("level_height", height);
      o.addProperty("min_gen_y", context.getMinGenY());
      o.addProperty("gen_depth", context.getGenDepth());
      o.addProperty("context_sea_level", context.seaLevel());
      contextResults.add(o);
    }
    root.add("contexts", contextResults);

    JsonArray anchors = new JsonArray();
    for (String text : ANCHORS) {
      JsonObject o = new JsonObject();
      o.add("json", JsonParser.parseString(text));
      DataResult<VerticalAnchor> result = VerticalAnchor.CODEC.parse(JsonOps.INSTANCE, JsonParser.parseString(text));
      if (result.result().isPresent()) {
        VerticalAnchor anchor = result.result().get();
        JsonArray ys = new JsonArray();
        for (WorldGenerationContext context : resolved) {
          ys.add(anchor.resolveY(context));
        }
        o.add("resolved", ys);
      }
      anchors.add(o);
    }
    root.add("anchors", anchors);

    // Per-column helpers that need the noises (surface depth, surface secondary, bands).
    JsonArray columns = new JsonArray();
    NoiseGeneratorSettings overworld = allSettings.getOrThrow(NoiseGeneratorSettings.OVERWORLD).value();
    for (long seed : new long[] {0L, 42L}) {
      MaterialSystem system = RandomState.create(noises, seed, overworld).surfaceSystem();
      for (int[] c : COLUMNS) {
        JsonObject o = new JsonObject();
        o.addProperty("seed", seed);
        o.addProperty("x", c[0]);
        o.addProperty("z", c[1]);
        o.addProperty("surface_depth", (Integer) surfaceDepth.invoke(system, c[0], c[1]));
        o.addProperty("surface_secondary", Goldens.bits((Double) surfaceSecondary.invoke(system, c[0], c[1])));
        JsonArray bands = new JsonArray();
        for (int y : new int[] {-64, 0, 63, 100, 255}) {
          BlockState state = (BlockState) band.invoke(system, c[0], y, c[1]);
          bands.add(BuiltInRegistries.BLOCK.getKey(state.getBlock()).toString());
        }
        o.add("bands_at_y_-64_0_63_100_255", bands);
        columns.add(o);
      }
    }
    root.add("columns", columns);
    return root;
  }
}

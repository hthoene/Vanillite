// Copyright Hannes Thöne. Licensed under the Apache License, Version 2.0.
package dev.vanillite.extractor.golden;

import com.google.gson.JsonArray;
import com.google.gson.JsonElement;
import com.google.gson.JsonObject;
import net.minecraft.core.HolderLookup;
import net.minecraft.core.registries.Registries;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.resources.Identifier;
import net.minecraft.resources.ResourceKey;
import net.minecraft.util.Interval;
import net.minecraft.util.RandomSource;
import net.minecraft.world.level.levelgen.LegacyRandomSource;
import net.minecraft.world.level.levelgen.NoiseGeneratorSettings;
import net.minecraft.world.level.levelgen.RandomState;
import net.minecraft.world.level.levelgen.WorldgenRandom;
import net.minecraft.world.level.levelgen.densityfunction.DensityBuffer;
import net.minecraft.world.level.levelgen.densityfunction.DensityVolume;
import net.minecraft.world.level.levelgen.synth.BlendedNoise;
import net.minecraft.world.level.levelgen.synth.Noise;
import net.minecraft.world.level.levelgen.synth.NormalNoise;
import net.minecraft.world.level.levelgen.synth.SimplexNoise;

/**
 * Goldens for the noise generators: {@code NormalNoise} instances of a {@code RandomState}
 * (Xoroshiro and legacy), the legacy nether biome noises, the end island simplex noise and the
 * {@code BlendedNoise} fbm stacks. Each noise records its range, {@code get(x, y, z)},
 * {@code get(x, y)} and {@code addToVolume} over a few volumes on a pre-filled buffer.
 */
public final class NoiseGoldens {
  private static final String[] NOISES = {
    "temperature", "vegetation", "continentalness", "erosion", "ridge", "offset", "jagged",
    "aquifer_barrier", "surface", "cave_cheese", "spaghetti_3d_1", "noodle", "ore_vein_a"
  };
  private static final String[] LEGACY_NOISES = {"temperature", "netherrack", "surface", "cave_cheese"};
  /** A subset sampled with a second seed. */
  private static final String[] SECOND_SEED_NOISES = {"temperature", "continentalness", "jagged", "cave_cheese", "ore_vein_a"};
  private static final long[] SEEDS = {0L, -7302402383914416L};
  private static final long LEGACY_SEED = 42L;

  /** Sample points: negative, fractional and huge (±3e7) coordinates. */
  static final double[][] POINTS = {
    {0.0, 0.0, 0.0}, {1.0, 2.0, 3.0}, {-1.0, -64.0, -1.0}, {0.5, 0.25, 0.125},
    {-0.5, -0.75, -0.0625}, {123.456, 78.9, -321.654}, {-1000.5, 319.5, 1000.25},
    {15.999, -16.001, 7.5}, {255.0, 256.0, 257.0}, {-255.75, 63.3, -256.2},
    {4096.0, 100.0, -4096.0}, {12345.678, -45.5, 98765.4321}, {-54321.0, 12.0, 11111.11},
    {2.0e5, 0.0, -2.0e5}, {1.6777215e7, 10.0, -1.6777216e7}, {1.6777217e7, -10.0, 1.6777215e7},
    {3.0e7, 320.0, -3.0e7}, {-3.0e7, -64.0, 3.0e7}, {2.9999999e7, 5.5, 2.9999983e7},
    {-2.9999984e7, 100.25, -1.0}, {0.001, 1.0e-9, -0.001}, {1.0e-300, -1.0e-300, 0.0},
    {7.0, 7.0, 7.0}, {-7.0, -7.0, -7.0}, {33554432.0, 0.0, -33554432.0},
    {5.0e8, 50.0, -5.0e8}, {0.25 * 1023, 0.125 * -59, 0.25 * 2047}, {-13.0, -61.0, 27.0},
    {100.0, -200.0, 300.0}, {-0.999999, 0.999999, -0.000001}
  };

  /** Volumes: {sizeX, sizeY, sizeZ, minX, minY, minZ, stepX, stepY, stepZ}. */
  static final int[][] VOLUMES = {
    {4, 5, 3, -13, -61, 27, 1, 1, 1},
    {3, 6, 2, 1001, -64, -2003, 4, 8, 4},
    {3, 4, 2, 29999990, 300, -29999995, 4, 8, 4},
  };

  /** Scales per volume test: {xzScale, yScale}. */
  static final double[][] SCALES = {{0.25, 0.0}, {1.0, 0.5}, {1.0, 1.0}};

  private static final float AMPLITUDE = 4.0F;

  private NoiseGoldens() {}

  public static JsonElement extract() {
    HolderLookup.Provider lookup = VanillaRegistries.createWorldLookup();
    HolderLookup.RegistryLookup<NormalNoise> noises = lookup.lookupOrThrow(Registries.NOISE);
    HolderLookup.RegistryLookup<NoiseGeneratorSettings> settings = lookup.lookupOrThrow(Registries.NOISE_SETTINGS);
    NoiseGeneratorSettings overworld = settings.getOrThrow(NoiseGeneratorSettings.OVERWORLD).value();
    NoiseGeneratorSettings nether = settings.getOrThrow(NoiseGeneratorSettings.NETHER).value();
    JsonObject root = new JsonObject();
    JsonArray points = new JsonArray();
    for (double[] p : POINTS) {
      JsonArray point = new JsonArray();
      for (double c : p) {
        point.add(Goldens.bits(c));
      }
      points.add(point);
    }
    root.add("points", points);
    JsonArray volumes = new JsonArray();
    for (int i = 0; i < VOLUMES.length; i++) {
      JsonObject volume = new JsonObject();
      JsonArray dims = new JsonArray();
      for (int c : VOLUMES[i]) {
        dims.add(c);
      }
      volume.add("volume", dims);
      volume.addProperty("xz_scale", Goldens.bits(SCALES[i][0]));
      volume.addProperty("y_scale", Goldens.bits(SCALES[i][1]));
      volumes.add(volume);
    }
    root.add("volumes", volumes);
    root.addProperty("amplitude", Goldens.bits(AMPLITUDE));

    JsonObject parameters = new JsonObject();
    noises.listElements().sorted(java.util.Comparator.comparing(holder -> holder.key().identifier().toString())).forEach(holder -> parameters.add(holder.key().identifier().toString(), range(holder.value().range())));
    root.add("parameter_ranges", parameters);

    JsonArray states = new JsonArray();
    states.add(state(noises, SEEDS[0], false, overworld, NOISES));
    states.add(state(noises, SEEDS[1], false, overworld, SECOND_SEED_NOISES));
    states.add(state(noises, LEGACY_SEED, true, nether, LEGACY_NOISES));
    root.add("random_states", states);

    JsonArray netherBiome = new JsonArray();
    for (long seed : new long[] {0L, LEGACY_SEED}) {
      JsonObject e = new JsonObject();
      e.addProperty("seed", seed);
      e.add("temperature", sample(noises.getOrThrow(key("nether/temperature")).value().createForLegacyNetherBiome(new LegacyRandomSource(seed))));
      e.add("vegetation", sample(noises.getOrThrow(key("nether/vegetation")).value().createForLegacyNetherBiome(new LegacyRandomSource(seed + 1))));
      netherBiome.add(e);
    }
    root.add("legacy_nether_biome", netherBiome);

    JsonArray simplex = new JsonArray();
    for (long seed : new long[] {0L, LEGACY_SEED, -7302402383914416L}) {
      RandomSource random = new LegacyRandomSource(seed);
      random.consumeCount(17292);
      SimplexNoise noise = new SimplexNoise(random, true);
      JsonObject e = sample(noise);
      e.addProperty("seed", seed);
      JsonArray islands = new JsonArray();
      for (long cx = -30; cx <= 30; cx += 7) {
        for (long cz = -1900000; cz <= 1900000; cz += 475000) {
          islands.add(Goldens.bits(noise.get(cx * 13 + cz, cz - cx * 61)));
        }
      }
      e.add("islands", islands);
      simplex.add(e);
    }
    root.add("end_islands", simplex);

    JsonArray blended = new JsonArray();
    // {seed, legacy (0/1), params index}: overworld (Xoroshiro), nether and end (legacy).
    double[][] blendedParams = {{0.25, 0.125, 80.0, 160.0, 8.0}, {0.25, 0.375, 80.0, 60.0, 8.0}, {0.25, 0.25, 80.0, 160.0, 4.0}};
    long[][] blendedCases = {{SEEDS[0], 0, 0}, {SEEDS[1], 0, 0}, {LEGACY_SEED, 1, 1}, {SEEDS[0], 1, 2}};
    for (long[] c : blendedCases) {
      long seed = c[0];
      boolean legacy = c[1] == 1;
      double[] p = blendedParams[(int) c[2]];
      BlendedNoise noise = new BlendedNoise(p[0], p[1], p[2], p[3], p[4]);
      RandomSource random = legacy
        ? new LegacyRandomSource(seed)
        : WorldgenRandom.Algorithm.XOROSHIRO.newInstance(seed).forkPositional().fromHashOf(BlendedNoise.NOISE_SEED);
      BlendedNoise.FbmSet set = noise.createFbmSet(random);
      JsonObject e = new JsonObject();
      e.addProperty("seed", seed);
      e.addProperty("legacy", legacy);
      JsonArray params = new JsonArray();
      for (double v : p) {
        params.add(Goldens.bits(v));
      }
      e.add("params", params);
      e.add("range", range(noise.range()));
      e.add("min_limit", sample(set.minLimitNoise()));
      e.add("max_limit", sample(set.maxLimitNoise()));
      e.add("main", sample(set.mainNoise()));
      blended.add(e);
    }
    root.add("blended", blended);
    return root;
  }

  private static ResourceKey<NormalNoise> key(String name) {
    return ResourceKey.create(Registries.NOISE, Identifier.withDefaultNamespace(name));
  }

  private static JsonObject state(
      HolderLookup.RegistryLookup<NormalNoise> noises, long seed, boolean legacy, NoiseGeneratorSettings settings, String[] names) {
    if (settings.useLegacyRandomSource() != legacy) {
      throw new IllegalStateException("unexpected random source");
    }
    RandomState state = RandomState.create(noises, seed, settings);
    JsonObject e = new JsonObject();
    e.addProperty("seed", seed);
    e.addProperty("legacy", legacy);
    JsonObject samples = new JsonObject();
    for (String name : names) {
      samples.add("minecraft:" + name, sample(state.getOrCreateNoise(key(name))));
    }
    e.add("noises", samples);
    return e;
  }

  private static JsonArray range(Interval range) {
    JsonArray a = new JsonArray();
    a.add(Goldens.bits(range.min()));
    a.add(Goldens.bits(range.max()));
    return a;
  }

  /**
   * Range, get3 and get2 at {@link #POINTS}, and addToVolume over {@link #VOLUMES} on a buffer
   * pre-filled with {@code (i % 7) * 0.125F - 0.375F}.
   */
  static JsonObject sample(Noise noise) {
    JsonObject e = new JsonObject();
    e.add("range", range(noise.range()));
    JsonArray get3 = new JsonArray();
    JsonArray get2 = new JsonArray();
    for (double[] p : POINTS) {
      get3.add(Goldens.bits(noise.get(p[0], p[1], p[2])));
      get2.add(Goldens.bits(noise.get(p[0], p[2])));
    }
    e.add("get3", get3);
    e.add("get2", get2);
    JsonArray volumes = new JsonArray();
    for (int i = 0; i < VOLUMES.length; i++) {
      int[] v = VOLUMES[i];
      DensityVolume volume = new DensityVolume(v[0], v[1], v[2], v[3], v[4], v[5], v[6], v[7], v[8]);
      int size = v[0] * v[1] * v[2];
      DensityBuffer buffer = DensityBuffer.createUnpooled(size);
      for (int j = 0; j < size; j++) {
        buffer.set(j, (j % 7) * 0.125F - 0.375F);
      }
      noise.addToVolume(buffer, volume, SCALES[i][0], SCALES[i][1], AMPLITUDE);
      JsonArray values = new JsonArray();
      for (int j = 0; j < size; j++) {
        values.add(Goldens.bits(buffer.get(j)));
      }
      volumes.add(values);
    }
    e.add("volumes", volumes);
    return e;
  }
}

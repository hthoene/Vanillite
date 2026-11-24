// Copyright Hannes Thöne. Licensed under the Apache License, Version 2.0.
package dev.vanillite.extractor.golden;

import com.google.gson.JsonArray;
import com.google.gson.JsonElement;
import com.google.gson.JsonObject;
import com.mojang.datafixers.util.Pair;
import java.lang.reflect.Field;
import java.lang.reflect.Method;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import net.minecraft.core.BlockPos;
import net.minecraft.core.HolderGetter;
import net.minecraft.core.HolderLookup;
import net.minecraft.core.registries.Registries;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.resources.Identifier;
import net.minecraft.resources.ResourceKey;
import net.minecraft.world.level.biome.Biome;
import net.minecraft.world.level.biome.BiomeManager;
import net.minecraft.world.level.biome.Climate;
import net.minecraft.world.level.biome.MultiNoiseBiomeSourceParameterList;
import net.minecraft.world.level.levelgen.XoroshiroRandomSource;

/**
 * Goldens for the biome module: climate quantization, the parameter lists of the overworld and
 * nether presets and their R-tree search (with and without the last-result cache), the biome zoom
 * of {@code BiomeManager} and {@code Biome.getTemperature}.
 */
public final class BiomeGoldens {
  private static final int SEARCHES = 2000;
  private static final long[] SEEDS = {0L, 1L, 42L, -1L, 123456789L, Long.MIN_VALUE, -7302402383914416L};

  private BiomeGoldens() {}

  public static JsonElement extract() throws Exception {
    JsonObject root = new JsonObject();
    root.add("quantize", quantize());
    root.add("target", targets());
    Map<MultiNoiseBiomeSourceParameterList.Preset, Climate.ParameterList<ResourceKey<Biome>>> presets =
        MultiNoiseBiomeSourceParameterList.knownPresets();
    root.add("overworld", search(presets.get(MultiNoiseBiomeSourceParameterList.Preset.OVERWORLD), 7L));
    root.add("nether", search(presets.get(MultiNoiseBiomeSourceParameterList.Preset.NETHER), 8L));
    root.add("obfuscate_seed", obfuscateSeed());
    root.add("zoom", zoom());
    root.add("temperature", temperature());
    return root;
  }

  private static JsonArray quantize() {
    JsonArray out = new JsonArray();
    float[] special = {
      0.0F, -0.0F, 1.0F, -1.0F, 2.0F, -2.0F, -0.455F, -0.19F, 0.55F, 1.0E-4F, -1.0E-4F, 0.99999F,
      Float.NaN, Float.POSITIVE_INFINITY, Float.NEGATIVE_INFINITY, 1.0E20F, -1.0E20F, Float.MIN_VALUE
    };
    XoroshiroRandomSource random = new XoroshiroRandomSource(5L);
    List<Float> values = new ArrayList<>();
    for (float f : special) {
      values.add(f);
    }
    for (int i = 0; i < 200; i++) {
      values.add(random.nextFloat() * 4.0F - 2.0F);
    }
    for (float f : values) {
      long q = Climate.quantizeCoord(f);
      JsonArray e = new JsonArray();
      e.add(Goldens.bits(f));
      e.add(q);
      e.add(Goldens.bits(Climate.unquantizeCoord(q)));
      out.add(e);
    }
    return out;
  }

  private static JsonArray targets() {
    JsonArray out = new JsonArray();
    XoroshiroRandomSource random = new XoroshiroRandomSource(6L);
    for (int i = 0; i < 20; i++) {
      float[] f = new float[6];
      JsonArray in = new JsonArray();
      for (int d = 0; d < 6; d++) {
        f[d] = (float) random.nextGaussian();
        in.add(Goldens.bits(f[d]));
      }
      Climate.TargetPoint t = Climate.target(f[0], f[1], f[2], f[3], f[4], f[5]);
      JsonArray q = new JsonArray();
      q.add(t.temperature());
      q.add(t.humidity());
      q.add(t.continentalness());
      q.add(t.erosion());
      q.add(t.depth());
      q.add(t.weirdness());
      JsonArray e = new JsonArray();
      e.add(in);
      e.add(q);
      out.add(e);
    }
    return out;
  }

  private static Climate.Parameter[] space(Climate.ParameterPoint p) {
    return new Climate.Parameter[] {
      p.temperature(), p.humidity(), p.continentalness(), p.erosion(), p.depth(), p.weirdness()
    };
  }

  /**
   * Searches a list for deterministic target points: per dimension a boundary value of the list
   * (often a tie between neighbouring entries), a boundary value +-1, or a uniform value.
   */
  private static JsonObject search(Climate.ParameterList<ResourceKey<Biome>> list, long seed) throws Exception {
    List<Pair<Climate.ParameterPoint, ResourceKey<Biome>>> values = list.values();
    long hash = 0;
    long[][] bounds = new long[6][values.size() * 2];
    for (int i = 0; i < values.size(); i++) {
      Pair<Climate.ParameterPoint, ResourceKey<Biome>> v = values.get(i);
      hash = hash * 31 + v.getSecond().identifier().toString().hashCode();
      Climate.Parameter[] space = space(v.getFirst());
      for (int d = 0; d < 6; d++) {
        hash = hash * 31 + space[d].min();
        hash = hash * 31 + space[d].max();
        bounds[d][i * 2] = space[d].min();
        bounds[d][i * 2 + 1] = space[d].max();
      }
      hash = hash * 31 + v.getFirst().offset();
    }

    XoroshiroRandomSource random = new XoroshiroRandomSource(seed);
    List<Climate.TargetPoint> targets = new ArrayList<>();
    long targetHash = 0;
    for (int i = 0; i < SEARCHES; i++) {
      long[] t = new long[6];
      for (int d = 0; d < 6; d++) {
        int mode = random.nextInt(4);
        if (mode == 0) {
          t[d] = bounds[d][random.nextInt(bounds[d].length)];
        } else if (mode == 1) {
          t[d] = bounds[d][random.nextInt(bounds[d].length)] + random.nextInt(3) - 1;
        } else {
          t[d] = random.nextInt(24001) - 12000;
        }
        targetHash = targetHash * 31 + t[d];
      }
      targets.add(new Climate.TargetPoint(t[0], t[1], t[2], t[3], t[4], t[5]));
    }

    // A fresh tree: its thread-local last result starts empty.
    Climate.ParameterList<ResourceKey<Biome>> fresh = list.rebuildWithChildrenPerNode(19);
    Field indexField = Climate.ParameterList.class.getDeclaredField("index");
    indexField.setAccessible(true);
    Object tree = indexField.get(fresh);
    Field lastField = tree.getClass().getDeclaredField("lastResult");
    lastField.setAccessible(true);
    ThreadLocal<?> last = (ThreadLocal<?>) lastField.get(tree);

    Map<String, Integer> names = new LinkedHashMap<>();
    JsonArray cached = new JsonArray();
    for (Climate.TargetPoint t : targets) {
      cached.add(nameIndex(names, fresh.findValue(t)));
    }
    JsonArray uncached = new JsonArray();
    for (Climate.TargetPoint t : targets) {
      last.remove();
      uncached.add(nameIndex(names, fresh.findValue(t)));
    }
    JsonArray brute = new JsonArray();
    for (Climate.TargetPoint t : targets) {
      brute.add(nameIndex(names, fresh.findValueBruteForce(t)));
    }

    JsonObject out = new JsonObject();
    out.addProperty("count", values.size());
    out.addProperty("hash", hash);
    out.addProperty("target_hash", targetHash);
    JsonArray nameArray = new JsonArray();
    names.keySet().forEach(nameArray::add);
    out.add("names", nameArray);
    out.add("cached", cached);
    out.add("uncached", uncached);
    out.add("brute_force", brute);
    return out;
  }

  private static int nameIndex(Map<String, Integer> names, ResourceKey<Biome> key) {
    return names.computeIfAbsent(key.identifier().toString(), k -> names.size());
  }

  private static JsonArray obfuscateSeed() {
    JsonArray out = new JsonArray();
    for (long seed : SEEDS) {
      JsonArray e = new JsonArray();
      e.add(seed);
      e.add(BiomeManager.obfuscateSeed(seed));
      out.add(e);
    }
    return out;
  }

  /** {@code getBiome(x, y, z)} with a resolver that records which quart it was asked for. */
  private static JsonArray zoom() {
    JsonArray out = new JsonArray();
    long[] seeds = {0L, 42L, -7302402383914416L};
    for (long seed : seeds) {
      int[] asked = new int[3];
      BiomeManager manager =
          new BiomeManager(
              (qx, qy, qz) -> {
                asked[0] = qx;
                asked[1] = qy;
                asked[2] = qz;
                return null;
              },
              BiomeManager.obfuscateSeed(seed));
      XoroshiroRandomSource random = new XoroshiroRandomSource(seed ^ 0x5DEECE66DL);
      JsonArray samples = new JsonArray();
      for (int i = 0; i < 300; i++) {
        int x;
        int y;
        int z;
        if (i < 100) {
          x = random.nextInt(64) - 32;
          y = random.nextInt(64) - 32;
          z = random.nextInt(64) - 32;
        } else {
          x = random.nextInt(60000001) - 30000000;
          y = random.nextInt(448) - 64;
          z = random.nextInt(60000001) - 30000000;
        }
        manager.getBiome(x, y, z);
        JsonArray e = new JsonArray();
        e.add(x);
        e.add(y);
        e.add(z);
        e.add(asked[0]);
        e.add(asked[1]);
        e.add(asked[2]);
        samples.add(e);
      }
      JsonObject s = new JsonObject();
      s.addProperty("seed", seed);
      s.add("samples", samples);
      out.add(s);
    }
    return out;
  }

  private static JsonArray temperature() throws Exception {
    HolderLookup.Provider registries = VanillaRegistries.createWorldLookup();
    HolderGetter<Biome> biomes = registries.lookupOrThrow(Registries.BIOME);
    Method getTemperature = Biome.class.getDeclaredMethod("getTemperature", BlockPos.class, int.class);
    getTemperature.setAccessible(true);
    String[] ids = {
      "plains", "frozen_ocean", "deep_frozen_ocean", "snowy_taiga", "frozen_peaks", "desert",
      "ocean", "stony_shore", "frozen_river"
    };
    int seaLevel = 63;
    JsonArray out = new JsonArray();
    XoroshiroRandomSource random = new XoroshiroRandomSource(9L);
    for (String id : ids) {
      Biome biome =
          biomes.getOrThrow(ResourceKey.create(Registries.BIOME, Identifier.withDefaultNamespace(id))).value();
      JsonArray samples = new JsonArray();
      for (int i = 0; i < 40; i++) {
        int x = random.nextInt(20001) - 10000;
        int y = random.nextInt(200) - 20;
        int z = random.nextInt(20001) - 10000;
        BlockPos pos = new BlockPos(x, y, z);
        float t = (float) getTemperature.invoke(biome, pos, seaLevel);
        JsonArray e = new JsonArray();
        e.add(x);
        e.add(y);
        e.add(z);
        e.add(Goldens.bits(t));
        e.add(biome.coldEnoughToSnow(pos, seaLevel));
        e.add(biome.shouldMeltFrozenOceanIcebergSlightly(pos, seaLevel));
        samples.add(e);
      }
      JsonObject b = new JsonObject();
      b.addProperty("biome", "minecraft:" + id);
      b.addProperty("sea_level", seaLevel);
      b.add("samples", samples);
      out.add(b);
    }
    return out;
  }
}

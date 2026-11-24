// Copyright Hannes Thöne. Licensed under the Apache License, Version 2.0.
package dev.vanillite.extractor.golden;

import com.google.gson.JsonArray;
import com.google.gson.JsonElement;
import com.google.gson.JsonObject;
import com.google.gson.JsonParser;
import com.mojang.serialization.JsonOps;
import net.minecraft.core.HolderGetter;
import net.minecraft.core.HolderLookup;
import net.minecraft.core.registries.Registries;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.resources.Identifier;
import net.minecraft.resources.RegistryOps;
import net.minecraft.resources.ResourceKey;
import net.minecraft.util.Interval;
import net.minecraft.world.level.levelgen.Aquifer;
import net.minecraft.world.level.levelgen.NoiseGeneratorSettings;
import net.minecraft.world.level.levelgen.NoiseRouter;
import net.minecraft.world.level.levelgen.RandomState;
import net.minecraft.world.level.levelgen.densityfunction.DensityBuffer;
import net.minecraft.world.level.levelgen.densityfunction.DensityFunction;
import net.minecraft.world.level.levelgen.densityfunction.DensityFunctions;
import net.minecraft.world.level.levelgen.densityfunction.DensitySampler;
import net.minecraft.world.level.levelgen.densityfunction.DensityVolume;
import net.minecraft.world.level.levelgen.densityfunction.SamplerContext;
import net.minecraft.world.level.levelgen.synth.NormalNoise;

/**
 * Goldens for density functions: the noise router and registry functions of the built-in noise
 * settings, compiled by {@code RandomState}'s {@code DensityFunctionCompiler} and sampled per block
 * without caches, over volumes with fresh caching contexts, and as one cached sequence that mixes
 * volumes and single values like a {@code NoiseChunk}.
 */
public final class DensityGoldens {
  private static final int[][] POINTS = {
    {0, 0, 0}, {1, 2, 3}, {-1, -1, -1}, {15, 64, 15}, {16, -64, 16}, {-17, 100, 33},
    {123, 7, -456}, {-1000, 40, 1000}, {4096, -30, -4097}, {8, 8, 8}, {-8, -56, 4},
    {37, 250, -91}, {-300, 319, 299}, {12345, 70, -54321}, {5, -64, -5}, {1600, 90, -1600}
  };
  private static final int VOLUME_SAMPLES = 4;

  private DensityGoldens() {}

  public static JsonElement extract() {
    HolderLookup.Provider registries = VanillaRegistries.createWorldLookup();
    JsonArray cases = new JsonArray();
    cases.add(
        settingsCase(
            registries,
            "overworld",
            0L,
            "overworld/continents",
            "overworld/caves/entrances",
            "overworld/caves/noodle",
            "overworld/caves/pillars",
            "overworld/caves/spaghetti_2d",
            "overworld/sloped_cheese",
            "overworld/base_3d_noise",
            "overworld/ore_vein/toggle",
            "y",
            "shift_x"));
    cases.add(
        settingsCase(
            registries,
            "overworld",
            -7302402383914416L,
            "overworld/continents",
            "overworld/caves/entrances",
            "overworld/caves/noodle",
            "overworld/sloped_cheese"));
    cases.add(settingsCase(registries, "nether", 42L, "nether/base_3d_noise"));
    cases.add(settingsCase(registries, "end", 1L, "end/base_3d_noise", "end/sloped_cheese", "end/islands"));
    String[] inline = synthetic();
    cases.add(settingsCase(registries, "overworld", 3L, false, inline));
    JsonObject root = new JsonObject();
    JsonArray points = new JsonArray();
    for (int[] p : POINTS) {
      points.add(pos(p[0], p[1], p[2]));
    }
    root.add("points", points);
    JsonArray inlineList = new JsonArray();
    for (String json : inline) {
      inlineList.add(json);
    }
    root.add("inline", inlineList);
    root.add("cases", cases);
    return root;
  }

  private static String n(String noise, double xzScale, double yScale) {
    return "{\"type\":\"minecraft:noise\",\"noise\":\"minecraft:" + noise + "\",\"xz_scale\":" + xzScale + ",\"y_scale\":"
        + yScale + "}";
  }

  private static String f(String type, String... fields) {
    StringBuilder b = new StringBuilder("{\"type\":\"minecraft:" + type + "\"");
    for (int i = 0; i < fields.length; i += 2) {
      b.append(",\"").append(fields[i]).append("\":").append(fields[i + 1]);
    }
    return b.append('}').toString();
  }

  /**
   * Inline functions (JSON, parsed with {@code DensityFunction.CODEC} on both sides) for the
   * samplers Vanilla's own functions do not reach: pow, round, distances, tiled gradients, the
   * remaining unary ops, constant variants, explicit slices, odd interpolation cells and context
   * bound functions.
   */
  private static String[] synthetic() {
    String a = n("continentalness", 1.0, 1.0);
    String b = n("erosion", 0.5, 0.0);
    String c = n("ridge", 1.0, 0.25);
    String d = n("cave_entrance", 0.75, 0.5);
    String positive = f("add", "left", f("abs", "input", c), "right", "0.25");
    String alpha = f("clamp", "input", f("mul", "left", a, "right", "3.0"), "min", "0.0", "max", "1.0");
    String gradientY = f("gradient", "axis", "\"y\"", "from_coordinate", "-10", "to_coordinate", "90", "from_value", "1.0", "to_value", "-1.0");
    java.util.List<String> list = new java.util.ArrayList<>();
    list.add(f("pow", "base", f("add", "left", a, "right", "1.5"), "exponent", b));
    String pows = "0.0";
    for (String e : new String[] {"2.0", "-2.0", "0.5", "-0.5"}) {
      pows = f("add", "left", pows, "right", f("pow", "base", positive, "exponent", e));
    }
    list.add(pows);
    pows = "0.0";
    for (String e : new String[] {"3.0", "1.0", "-1.0", "1.7", "0.0"}) {
      pows = f("add", "left", pows, "right", f("pow", "base", positive, "exponent", e));
    }
    list.add(pows);
    list.add(f("pow", "base", "2.0", "exponent", f("mul", "left", c, "right", "4.0")));
    String scaled = f("mul", "left", d, "right", "10.0");
    list.add(f("add", "left", f("add", "left", f("floor", "input", scaled), "right", f("round", "input", scaled)),
        "right", f("add", "left", f("ceil", "input", scaled), "right", f("truncate", "input", scaled))));
    list.add(f("add", "left", f("round", "input", scaled, "multiple", "0.25"),
        "right", f("add", "left", f("floor", "input", scaled, "multiple", b), "right", f("truncate", "input", scaled, "multiple", "0.0"))));
    String point = "\"point\":[3,-20,17]";
    list.add("{\"type\":\"minecraft:distance_to_point\"," + point + ",\"metric\":\"euclidean\"}");
    list.add(f("add", "left", "{\"type\":\"minecraft:distance_to_point\"," + point + ",\"metric\":\"euclidean_squared\"}",
        "right", f("add", "left", "{\"type\":\"minecraft:distance_to_point\"," + point + ",\"metric\":\"manhattan\"}",
            "right", "{\"type\":\"minecraft:distance_to_point\"," + point + ",\"metric\":\"chebyshev\"}")));
    list.add(f("add",
        "left", f("gradient", "axis", "\"x\"", "tiling", "\"repeat\"", "from_coordinate", "-7", "to_coordinate", "13", "from_value", "0.5", "to_value", "-2.0"),
        "right", f("add",
            "left", f("gradient", "axis", "\"z\"", "tiling", "\"mirrored_repeat\"", "from_coordinate", "20", "to_coordinate", "-11", "from_value", "-1.0", "to_value", "3.0"),
            "right", f("gradient", "axis", "\"y\"", "tiling", "\"mirrored_repeat\"", "from_coordinate", "0", "to_coordinate", "64", "from_value", "0.0", "to_value", "1.0"))));
    list.add(f("log", "input", positive));
    list.add(f("add", "left", f("sign", "input", c), "right", f("reciprocal", "input", f("add", "left", a, "right", "0.5"))));
    list.add(f("add", "left", f("add", "left", f("sqrt", "input", positive), "right", f("cube", "input", c)),
        "right", f("add", "left", f("negate", "input", d), "right", f("add", "left", f("square", "input", a),
            "right", f("add", "left", f("squeeze", "input", f("mul", "left", c, "right", "3.0")),
                "right", f("add", "left", f("half_negative", "input", b), "right", f("quarter_negative", "input", d)))))));
    list.add(f("add", "left", f("div", "left", a, "right", f("add", "left", c, "right", "0.3")),
        "right", f("add", "left", f("div", "left", "2.0", "right", positive), "right", f("div", "left", d, "right", "4.0"))));
    list.add(f("add", "left", f("min", "left", "-0.5", "right", a), "right", f("max", "left", b, "right", "0.25")));
    list.add(f("add", "left", f("min", "left", a, "right", c), "right", f("max", "left", c, "right", d)));
    list.add(f("add", "left", f("min", "left", alpha, "right", "5.0"), "right", f("max", "left", "-3.0", "right", gradientY)));
    list.add(f("add", "left", f("sub", "left", "0.75", "right", a), "right", f("sub", "left", b, "right", "0.125")));
    list.add(f("lerp", "alpha", alpha, "first", "0.5", "second", c));
    list.add(f("lerp", "alpha", alpha, "first", c, "second", "-0.5"));
    list.add(f("lerp", "alpha", alpha, "first", c, "second", d));
    list.add(f("range_choice", "input", a, "min_inclusive", "-0.2", "max_exclusive", "0.3", "when_in_range", c, "when_out_of_range", d));
    list.add(f("range_choice", "input", a, "min_inclusive", "-0.2", "max_exclusive", "0.3", "when_in_range", "1.0", "when_out_of_range", "-1.0"));
    list.add(f("interval_select", "input", a, "thresholds", "[0.1]", "functions", "[" + c + "," + d + "]"));
    list.add(f("add", "left", f("slice", "axis", "\"x\"", "coordinate", "5", "input", d),
        "right", f("slice", "axis", "\"z\"", "coordinate", "-3", "input", f("slice", "axis", "\"x\"", "coordinate", "9", "input", d))));
    list.add(f("add", "left", f("slice", "axis", "\"x\"", "coordinate", "2", "input", f("slice", "axis", "\"z\"", "coordinate", "-6", "input", c)),
        "right", f("add", "left", f("slice", "axis", "\"y\"", "coordinate", "40", "input", d), "right", f("slice", "axis", "\"z\"", "coordinate", "7", "input", a))));
    list.add(f("interpolated", "input", d, "cell_size_xz", "3", "cell_size_y", "5"));
    list.add(f("add", "left", f("interpolated", "input", f("cache", "input", c), "cell_size_xz", "2", "cell_size_y", "4"),
        "right", f("cache", "input", f("add", "left", gradientY, "right", "0.5"))));
    list.add(f("spline", "spline", "0.75"));
    String spline = "{\"coordinate\":" + c + ",\"points\":[{\"location\":-0.5,\"value\":-1.0,\"derivative\":0.5},"
        + "{\"location\":0.0,\"value\":{\"coordinate\":" + a + ",\"points\":[{\"location\":0.0,\"value\":0.25,\"derivative\":1.5}]},\"derivative\":0.0},"
        + "{\"location\":0.4,\"value\":{\"coordinate\":" + c + ",\"points\":[{\"location\":-1.0,\"value\":2.0,\"derivative\":0.0},{\"location\":1.0,\"value\":3.0,\"derivative\":-2.0}]},\"derivative\":-1.0}]}";
    list.add(f("spline", "spline", spline));
    list.add(f("find_top_surface", "density", f("add", "left", d, "right", gradientY), "upper_bound", "120.0", "lower_bound", "-40", "cell_height", "6"));
    list.add(f("add", "left", f("shift", "noise", "\"minecraft:offset\""),
        "right", f("noise", "noise", "\"minecraft:temperature\"", "xz_scale", "0.5", "y_scale", "0.25", "shift_x", a, "shift_y", c, "shift_z", "\"minecraft:shift_z\"")));
    list.add(f("add", "left", f("add", "left", f("blend_alpha"), "right", f("blend_offset")),
        "right", f("add", "left", f("beardifier"), "right", f("blend_density", "input", d))));
    list.add(f("add", "left", "\"minecraft:zero\"", "right", f("mul", "left", "\"minecraft:y\"", "right", "0.01")));
    return list.toArray(new String[0]);
  }

  private static JsonObject settingsCase(HolderLookup.Provider registries, String settingsId, long seed, String... extraFunctions) {
    return settingsCase(registries, settingsId, seed, true, extraFunctions);
  }

  private static JsonObject settingsCase(
      HolderLookup.Provider registries, String settingsId, long seed, boolean includeRouter, String... extraFunctions) {
    HolderGetter<NormalNoise> noises = registries.lookupOrThrow(Registries.NOISE);
    HolderLookup.RegistryLookup<DensityFunction> functions = registries.lookupOrThrow(Registries.DENSITY_FUNCTION);
    NoiseGeneratorSettings settings = registries
        .lookupOrThrow(Registries.NOISE_SETTINGS)
        .getOrThrow(ResourceKey.create(Registries.NOISE_SETTINGS, Identifier.withDefaultNamespace(settingsId)))
        .value();
    RandomState state = RandomState.create(noises, seed, settings);
    int minY = settings.noiseSettings().minY();
    int height = settings.noiseSettings().height();

    java.util.List<String> names = new java.util.ArrayList<>();
    java.util.List<DensityFunction> list = new java.util.ArrayList<>();
    NoiseRouter router = settings.noiseRouter();
    String[] routerNames = {
      "temperature", "vegetation", "continents", "erosion", "depth", "ridges", "chunk_surface_level", "final_density"
    };
    DensityFunction[] routerFunctions = {
      router.temperature(), router.vegetation(), router.continents(), router.erosion(), router.depth(), router.ridges(),
      router.chunkSurfaceLevel(), router.finalDensity()
    };
    for (int i = 0; includeRouter && i < routerNames.length; i++) {
      names.add("router/" + routerNames[i]);
      list.add(routerFunctions[i]);
    }
    if (includeRouter && settings.aquifers().isPresent()) {
      Aquifer.Config aquifers = settings.aquifers().get();
      String[] aquiferNames = {"barrier", "fluid_level_floodedness", "fluid_level_spread", "lava", "exclusion", "surface_level"};
      DensityFunction[] aquiferFunctions = {
        aquifers.barrierNoise(), aquifers.fluidLevelFloodednessNoise(), aquifers.fluidLevelSpreadNoise(), aquifers.lavaNoise(),
        aquifers.exclusion(), aquifers.surfaceLevel()
      };
      for (int i = 0; i < aquiferNames.length; i++) {
        names.add("aquifers/" + aquiferNames[i]);
        list.add(aquiferFunctions[i]);
      }
    }
    RegistryOps<JsonElement> ops = registries.createSerializationContext(JsonOps.INSTANCE);
    for (int e = 0; e < extraFunctions.length; e++) {
      String id = extraFunctions[e];
      if (id.startsWith("{")) {
        names.add("inline/" + e);
        list.add(DensityFunction.CODEC.parse(ops, JsonParser.parseString(id)).getOrThrow());
        continue;
      }
      names.add("minecraft:" + id);
      list.add(
          new DensityFunctions.HolderHolder(
              functions.getOrThrow(ResourceKey.create(Registries.DENSITY_FUNCTION, Identifier.withDefaultNamespace(id)))));
    }

    DensityVolume[] volumes = {
      new DensityVolume(16, height, 16, -37 * 16, minY, 21 * 16),
      new DensityVolume(4, height / 4, 4, 64, minY, -64, 4, 4, 4),
      new DensityVolume(5, height / 8 + 1, 5, -32, minY, 48, 4, 8, 4),
      new DensityVolume(5, 1, 7, -3, 17, 5),
      new DensityVolume(3, 9, 2, 13, minY + 3, -7, 2, 3, 5),
      new DensityVolume(1, height, 1, 7, minY, -9)
    };

    JsonArray results = new JsonArray();
    for (int f = 0; f < list.size(); f++) {
      DensityFunction function = list.get(f);
      DensitySampler sampler = state.getSampler(function);
      JsonObject entry = new JsonObject();
      entry.addProperty("name", names.get(f));
      Interval range = function.range();
      JsonArray rangeBits = new JsonArray();
      rangeBits.add(Goldens.bits(range.min()));
      rangeBits.add(Goldens.bits(range.max()));
      entry.add("range", rangeBits);
      entry.addProperty("axes", function.domainAxes());
      JsonArray values = new JsonArray();
      for (int[] p : POINTS) {
        values.add(Goldens.bits(state.sampleBlockValueUncached(function, p[0], p[1], p[2])));
      }
      entry.add("values", values);
      JsonArray volumeResults = new JsonArray();
      for (DensityVolume volume : volumes) {
        SamplerContext context = SamplerContext.builder().enableCaches().build();
        DensityBuffer buffer = DensityBuffer.createUnpooled(volume.size());
        sampler.sampleVolume(context, buffer, volume);
        volumeResults.add(volumeResult(volume, buffer));
      }
      entry.add("volumes", volumeResults);
      results.add(entry);
    }

    // One shared caching context, like a NoiseChunk: volumes and single values of all functions
    // interleaved, so cache cells see each other's volumes.
    SamplerContext shared = SamplerContext.builder().enableCaches().build();
    DensityVolume chunk = new DensityVolume(16, height, 16, 32, minY, -48);
    int[][] sequencePoints = {
      {32, minY + 5, -48}, {40, minY + 70, -40}, {47, minY + height - 1, -33}, {31, minY + 70, -40}, {100, minY + 10, 100},
      {100, minY + 10, 100}, {33, minY + 64, -47}, {48, minY + 64, -40}, {36, minY + 8, -44}
    };
    JsonArray sequence = new JsonArray();
    for (int f = 0; f < list.size(); f++) {
      DensitySampler sampler = state.getSampler(list.get(f));
      JsonArray values = new JsonArray();
      // Values before the volume of this function, then the volume, then values again.
      values.add(Goldens.bits(sampler.sampleValue(shared, 41, minY + 33, -41)));
      DensityBuffer buffer = DensityBuffer.createUnpooled(chunk.size());
      sampler.sampleVolume(shared, buffer, chunk);
      JsonObject step = new JsonObject();
      step.add("before", values);
      step.add("volume", volumeResult(chunk, buffer));
      JsonArray after = new JsonArray();
      for (int[] p : sequencePoints) {
        after.add(Goldens.bits(sampler.sampleValue(shared, p[0], p[1], p[2])));
      }
      step.add("after", after);
      sequence.add(step);
    }

    JsonObject result = new JsonObject();
    result.addProperty("settings", "minecraft:" + settingsId);
    result.addProperty("seed", seed);
    JsonArray volumeList = new JsonArray();
    for (DensityVolume v : volumes) {
      volumeList.add(volume(v));
    }
    result.add("volumes", volumeList);
    result.add("functions", results);
    result.add("sequence_volume", volume(chunk));
    JsonArray sequencePointList = new JsonArray();
    for (int[] p : sequencePoints) {
      sequencePointList.add(pos(p[0], p[1], p[2]));
    }
    result.add("sequence_points", sequencePointList);
    result.add("sequence", sequence);
    return result;
  }

  private static JsonObject volumeResult(DensityVolume volume, DensityBuffer buffer) {
    long hash = 0;
    for (int i = 0; i < volume.size(); i++) {
      hash = hash * 31 + Float.floatToRawIntBits(buffer.get(i));
    }
    JsonObject o = new JsonObject();
    o.addProperty("hash", hash);
    JsonArray samples = new JsonArray();
    int size = volume.size();
    for (int k = 0; k < VOLUME_SAMPLES; k++) {
      int index = (int) ((long) k * (size - 1) / (VOLUME_SAMPLES - 1));
      JsonArray s = new JsonArray();
      s.add(index);
      s.add(Goldens.bits(buffer.get(index)));
      samples.add(s);
    }
    o.add("samples", samples);
    return o;
  }

  private static JsonArray volume(DensityVolume v) {
    JsonArray a = new JsonArray();
    for (int x : new int[] {v.sizeX(), v.sizeY(), v.sizeZ(), v.minBlockX(), v.minBlockY(), v.minBlockZ(), v.stepBlockX(), v.stepBlockY(), v.stepBlockZ()}) {
      a.add(x);
    }
    return a;
  }

  private static JsonArray pos(int x, int y, int z) {
    JsonArray a = new JsonArray();
    a.add(x);
    a.add(y);
    a.add(z);
    return a;
  }
}

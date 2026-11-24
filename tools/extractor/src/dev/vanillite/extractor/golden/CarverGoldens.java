// Copyright Hannes Thöne. Licensed under the Apache License, Version 2.0.
package dev.vanillite.extractor.golden;

import com.google.gson.JsonArray;
import com.google.gson.JsonElement;
import com.google.gson.JsonObject;
import com.google.gson.JsonParser;
import com.mojang.serialization.JsonOps;
import java.lang.reflect.Field;
import java.util.BitSet;
import java.util.List;
import net.minecraft.core.Holder;
import net.minecraft.core.HolderLookup;
import net.minecraft.core.registries.Registries;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.data.worldgen.Carvers;
import net.minecraft.util.RandomSource;
import net.minecraft.util.valueproviders.FloatProvider;
import net.minecraft.util.valueproviders.FloatProviders;
import net.minecraft.util.valueproviders.IntProvider;
import net.minecraft.util.valueproviders.IntProviders;
import net.minecraft.world.level.ChunkPos;
import net.minecraft.world.level.LevelHeightAccessor;
import net.minecraft.world.level.biome.Biomes;
import net.minecraft.world.level.biome.FixedBiomeSource;
import net.minecraft.world.level.chunk.CarvingMask;
import net.minecraft.world.level.levelgen.LegacyRandomSource;
import net.minecraft.world.level.levelgen.NoiseBasedChunkGenerator;
import net.minecraft.world.level.levelgen.NoiseGeneratorSettings;
import net.minecraft.world.level.levelgen.RandomSupport;
import net.minecraft.world.level.levelgen.WorldGenerationContext;
import net.minecraft.world.level.levelgen.WorldgenRandom;
import net.minecraft.world.level.levelgen.carver.WorldCarver;
import net.minecraft.world.level.levelgen.heightproviders.HeightProvider;

/**
 * Goldens for carvers: Vanilla's carver loop ({@code NoiseBasedChunkGenerator.generateCarvers}
 * without the mask application) with a fixed carver list for every source chunk, and samples of
 * the value and height providers.
 */
public final class CarverGoldens {
  private static final long[] SEEDS = {0L, 42L, -7302402383914416L};
  private static final int[][] CHUNKS = {
    {0, 0}, {1, 0}, {-1, -1}, {3, 3}, {5, -3}, {7, -12}, {-8, 8}, {10, 10}, {-20, 7}, {30, 31},
    {-31, -30}, {64, 0}, {0, -64}, {100, -100}, {200, 201}, {-500, -499}, {-1000, 250},
    {12345, -6789}, {-30000, 29999}, {1875000, -1875000}
  };
  private static final int SAMPLE_SEGMENTS = 6;

  private CarverGoldens() {}

  public static JsonElement extract() throws Exception {
    HolderLookup.Provider lookup = VanillaRegistries.createWorldLookup();
    HolderLookup.RegistryLookup<WorldCarver> carvers = lookup.lookupOrThrow(Registries.CARVER);
    HolderLookup.RegistryLookup<NoiseGeneratorSettings> settings =
        lookup.lookupOrThrow(Registries.NOISE_SETTINGS);
    Holder<net.minecraft.world.level.biome.Biome> plains =
        lookup.lookupOrThrow(Registries.BIOME).getOrThrow(Biomes.PLAINS);

    WorldGenerationContext overworld =
        context(new FixedBiomeSource(plains), settings.getOrThrow(NoiseGeneratorSettings.OVERWORLD), -64, 384);
    WorldGenerationContext nether =
        context(new FixedBiomeSource(plains), settings.getOrThrow(NoiseGeneratorSettings.NETHER), 0, 256);

    JsonObject root = new JsonObject();
    root.add("overworld", contextJson(overworld));
    root.add("nether", contextJson(nether));

    List<String> overworldNames = List.of("minecraft:cave", "minecraft:cave_extra_underground", "minecraft:canyon");
    List<WorldCarver> overworldCarvers =
        List.of(
            carvers.getOrThrow(Carvers.CAVE).value(),
            carvers.getOrThrow(Carvers.CAVE_EXTRA_UNDERGROUND).value(),
            carvers.getOrThrow(Carvers.CANYON).value());
    List<WorldCarver> netherCarvers = List.of(carvers.getOrThrow(Carvers.NETHER_CAVE).value());

    JsonArray runs = new JsonArray();
    for (long seed : SEEDS) {
      for (int[] c : CHUNKS) {
        runs.add(run("overworld", overworldCarvers, seed, c, overworld));
        runs.add(run("nether", netherCarvers, seed, c, nether));
      }
    }
    JsonObject sets = new JsonObject();
    JsonArray ow = new JsonArray();
    overworldNames.forEach(ow::add);
    sets.add("overworld", ow);
    JsonArray ne = new JsonArray();
    ne.add("minecraft:nether_cave");
    sets.add("nether", ne);
    root.add("carver_sets", sets);
    root.add("runs", runs);
    root.add("providers", providers(overworld, nether));
    return root;
  }

  private static WorldGenerationContext context(
      FixedBiomeSource biomes, Holder<NoiseGeneratorSettings> settings, int minY, int height) {
    NoiseBasedChunkGenerator generator = new NoiseBasedChunkGenerator(biomes, settings);
    return new WorldGenerationContext(generator, LevelHeightAccessor.create(minY, height));
  }

  private static JsonObject contextJson(WorldGenerationContext context) {
    JsonObject o = new JsonObject();
    o.addProperty("min_gen_y", context.getMinGenY());
    o.addProperty("gen_depth", context.getGenDepth());
    o.addProperty("sea_level", context.seaLevel());
    return o;
  }

  /** The carver loop of {@code generateCarvers} for one chunk, every source chunk using {@code list}. */
  private static JsonObject run(
      String set, List<WorldCarver> list, long seed, int[] c, WorldGenerationContext context) throws Exception {
    ChunkPos pos = new ChunkPos(c[0], c[1]);
    WorldgenRandom random = new WorldgenRandom(new LegacyRandomSource(RandomSupport.generateUniqueSeed()));
    int maxY = context.getMinGenY() + context.getGenDepth() - 1 - 7;
    CarvingMask mask = new CarvingMask(context.getMinGenY() + 1, maxY);
    for (int dx = -8; dx <= 8; dx++) {
      for (int dz = -8; dz <= 8; dz++) {
        ChunkPos sourcePos = new ChunkPos(pos.x() + dx, pos.z() + dz);
        int index = 0;
        for (WorldCarver carver : list) {
          random.setLargeFeatureSeed(seed + index, sourcePos.x(), sourcePos.z());
          if (carver.isStartChunk(random)) {
            carver.carve(context, random, pos, sourcePos, mask);
          }
          index++;
        }
      }
    }

    Field maskField = CarvingMask.class.getDeclaredField("mask");
    maskField.setAccessible(true);
    BitSet bits = (BitSet) maskField.get(mask);
    long bitHash = 0;
    for (int i = bits.nextSetBit(0); i >= 0; i = bits.nextSetBit(i + 1)) {
      bitHash = bitHash * 31 + i;
    }
    long[] visitHash = {0};
    JsonArray segments = new JsonArray();
    mask.visit(
        (x, z, bottomY, topY) -> {
          visitHash[0] = (((visitHash[0] * 31 + x) * 31 + z) * 31 + bottomY) * 31 + topY;
          if (segments.size() < SAMPLE_SEGMENTS) {
            JsonArray s = new JsonArray();
            s.add(x);
            s.add(z);
            s.add(bottomY);
            s.add(topY);
            segments.add(s);
          }
        });

    JsonObject o = new JsonObject();
    o.addProperty("set", set);
    o.addProperty("seed", seed);
    o.addProperty("x", c[0]);
    o.addProperty("z", c[1]);
    o.addProperty("min_y", mask.minY());
    o.addProperty("max_y", mask.maxY());
    o.addProperty("count", bits.cardinality());
    o.addProperty("bit_hash", bitHash);
    o.addProperty("visit_hash", visitHash[0]);
    o.add("segments", segments);
    return o;
  }

  private static final String[] INT_PROVIDERS = {
    "5",
    "{\"type\":\"minecraft:constant\",\"value\":-3}",
    "{\"type\":\"minecraft:uniform\",\"min_inclusive\":-4,\"max_inclusive\":9}",
    "{\"type\":\"minecraft:biased_to_bottom\",\"min_inclusive\":0,\"max_inclusive\":14}",
    "{\"type\":\"minecraft:very_biased_to_bottom\",\"min_inclusive\":0,\"max_inclusive\":14}",
    "{\"type\":\"minecraft:clamped\",\"source\":{\"type\":\"minecraft:uniform\",\"min_inclusive\":-10,\"max_inclusive\":10},\"min_inclusive\":-2,\"max_inclusive\":3}",
    "{\"type\":\"minecraft:weighted_list\",\"distribution\":[{\"data\":1,\"weight\":3},{\"data\":{\"type\":\"minecraft:uniform\",\"min_inclusive\":5,\"max_inclusive\":8},\"weight\":1}]}",
    "{\"type\":\"minecraft:weighted_list\",\"distribution\":[{\"data\":1,\"weight\":40},{\"data\":2,\"weight\":30}]}",
    "{\"type\":\"minecraft:clamped_normal\",\"mean\":0.5,\"deviation\":2.25,\"min_inclusive\":-3,\"max_inclusive\":4}",
    "{\"type\":\"minecraft:trapezoid\",\"min\":-5,\"max\":5,\"plateau\":0}",
    "{\"type\":\"minecraft:trapezoid\",\"min\":-2,\"max\":7,\"plateau\":3}",
    "{\"type\":\"minecraft:trapezoid\",\"min\":1,\"max\":4,\"plateau\":3}",
  };

  private static final String[] FLOAT_PROVIDERS = {
    "0.7",
    "{\"type\":\"minecraft:constant\",\"value\":1.25}",
    "{\"type\":\"minecraft:uniform\",\"min_inclusive\":-0.125,\"max_exclusive\":0.125}",
    "{\"type\":\"minecraft:uniform\",\"min_inclusive\":0.1,\"max_exclusive\":0.9}",
    "{\"type\":\"minecraft:clamped_normal\",\"mean\":1.0,\"deviation\":0.3,\"min\":0.5,\"max\":1.6}",
    "{\"type\":\"minecraft:trapezoid\",\"min\":0.0,\"max\":6.0,\"plateau\":2.0}",
    "{\"type\":\"minecraft:trapezoid\",\"min\":0.0,\"max\":3.0,\"plateau\":1.0}",
  };

  private static final String[] HEIGHT_PROVIDERS = {
    "{\"absolute\":12}",
    "{\"type\":\"minecraft:constant\",\"value\":{\"below_top\":3}}",
    "{\"type\":\"minecraft:uniform\",\"min_inclusive\":{\"above_bottom\":8},\"max_inclusive\":{\"absolute\":180}}",
    "{\"type\":\"minecraft:uniform\",\"min_inclusive\":{\"absolute\":0},\"max_inclusive\":{\"below_top\":1}}",
    "{\"type\":\"minecraft:uniform\",\"min_inclusive\":{\"relative_to_sea_level\":-10},\"max_inclusive\":{\"relative_to_sea_level\":10}}",
    "{\"type\":\"minecraft:uniform\",\"min_inclusive\":{\"absolute\":50},\"max_inclusive\":{\"absolute\":10}}",
    "{\"type\":\"minecraft:biased_to_bottom\",\"min_inclusive\":{\"above_bottom\":0},\"max_inclusive\":{\"absolute\":40},\"inner\":4}",
    "{\"type\":\"minecraft:very_biased_to_bottom\",\"min_inclusive\":{\"above_bottom\":0},\"max_inclusive\":{\"below_top\":8}}",
    "{\"type\":\"minecraft:very_biased_to_bottom\",\"min_inclusive\":{\"absolute\":5},\"max_inclusive\":{\"absolute\":30},\"inner\":6}",
    "{\"type\":\"minecraft:trapezoid\",\"min_inclusive\":{\"absolute\":-16},\"max_inclusive\":{\"absolute\":112}}",
    "{\"type\":\"minecraft:trapezoid\",\"min_inclusive\":{\"above_bottom\":0},\"max_inclusive\":{\"absolute\":64},\"plateau\":20}",
    "{\"type\":\"minecraft:weighted_list\",\"distribution\":[{\"data\":{\"absolute\":1},\"weight\":1},{\"data\":{\"type\":\"minecraft:uniform\",\"min_inclusive\":{\"absolute\":20},\"max_inclusive\":{\"absolute\":30}},\"weight\":2}]}",
  };

  private static final int SAMPLES = 24;

  private static JsonObject providers(WorldGenerationContext overworld, WorldGenerationContext nether) {
    JsonObject root = new JsonObject();
    JsonArray ints = new JsonArray();
    for (String json : INT_PROVIDERS) {
      IntProvider p = IntProviders.CODEC.parse(JsonOps.INSTANCE, JsonParser.parseString(json)).getOrThrow();
      JsonObject o = new JsonObject();
      o.addProperty("json", json);
      o.addProperty("min", p.minInclusive());
      o.addProperty("max", p.maxInclusive());
      JsonArray values = new JsonArray();
      for (long seed : SEEDS) {
        RandomSource random = new LegacyRandomSource(seed);
        for (int i = 0; i < SAMPLES; i++) {
          values.add(p.sample(random));
        }
      }
      o.add("samples", values);
      ints.add(o);
    }
    root.add("int", ints);

    JsonArray floats = new JsonArray();
    for (String json : FLOAT_PROVIDERS) {
      FloatProvider p = FloatProviders.CODEC.parse(JsonOps.INSTANCE, JsonParser.parseString(json)).getOrThrow();
      JsonObject o = new JsonObject();
      o.addProperty("json", json);
      o.addProperty("min", Goldens.bits(p.min()));
      o.addProperty("max", Goldens.bits(p.max()));
      JsonArray values = new JsonArray();
      for (long seed : SEEDS) {
        RandomSource random = new LegacyRandomSource(seed);
        for (int i = 0; i < SAMPLES; i++) {
          values.add(Goldens.bits(p.sample(random)));
        }
      }
      o.add("samples", values);
      floats.add(o);
    }
    root.add("float", floats);

    JsonArray heights = new JsonArray();
    for (String json : HEIGHT_PROVIDERS) {
      HeightProvider p = HeightProvider.CODEC.parse(JsonOps.INSTANCE, JsonParser.parseString(json)).getOrThrow();
      JsonObject o = new JsonObject();
      o.addProperty("json", json);
      JsonArray values = new JsonArray();
      for (WorldGenerationContext context : new WorldGenerationContext[] {overworld, nether}) {
        for (long seed : SEEDS) {
          RandomSource random = new LegacyRandomSource(seed);
          for (int i = 0; i < SAMPLES; i++) {
            values.add(p.sample(random, context));
          }
        }
      }
      o.add("samples", values);
      heights.add(o);
    }
    root.add("height", heights);

    // WorldgenRandom keeps its own Gaussian cache across setSeed (reseeding only the wrapped
    // source), which a clamped_normal provider in the carver loop observes.
    FloatProvider normal =
        FloatProviders.CODEC
            .parse(JsonOps.INSTANCE, JsonParser.parseString(FLOAT_PROVIDERS[4]))
            .getOrThrow();
    JsonArray gaussian = new JsonArray();
    for (long seed : SEEDS) {
      WorldgenRandom random = new WorldgenRandom(new LegacyRandomSource(RandomSupport.generateUniqueSeed()));
      for (int i = 0; i < 8; i++) {
        random.setLargeFeatureSeed(seed + i, 3, -4);
        gaussian.add(Goldens.bits(normal.sample(random)));
        if (i % 3 == 0) {
          gaussian.add(Goldens.bits(normal.sample(random)));
        }
      }
    }
    root.add("worldgen_random_gaussian", gaussian);
    return root;
  }
}

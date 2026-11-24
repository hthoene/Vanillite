// Copyright Hannes Thöne. Licensed under the Apache License, Version 2.0.
package dev.vanillite.extractor.golden;

import com.google.gson.JsonArray;
import com.google.gson.JsonElement;
import com.google.gson.JsonObject;
import java.lang.reflect.Field;
import net.minecraft.util.Mth;
import net.minecraft.util.RandomSource;
import net.minecraft.world.level.levelgen.LegacyRandomSource;
import net.minecraft.world.level.levelgen.PositionalRandomFactory;
import net.minecraft.world.level.levelgen.RandomSupport;
import net.minecraft.world.level.levelgen.WorldgenRandom;
import net.minecraft.world.level.levelgen.XoroshiroRandomSource;

/** Goldens for the Vanilla parts of vanillite-base: RNGs, seeding, Mth.sin and Mth.getSeed. */
public final class BaseGoldens {
  private static final long[] SEEDS = {0L, 1L, 42L, -1L, 123456789L, Long.MIN_VALUE, -7302402383914416L};

  private BaseGoldens() {}

  public static JsonElement extract() throws Exception {
    JsonObject root = new JsonObject();
    JsonArray xoroshiro = new JsonArray();
    JsonArray legacy = new JsonArray();
    for (long seed : SEEDS) {
      xoroshiro.add(stream(seed, new XoroshiroRandomSource(seed)));
      legacy.add(stream(seed, new LegacyRandomSource(seed)));
    }
    root.add("xoroshiro", xoroshiro);
    root.add("legacy", legacy);

    JsonArray upgrade = new JsonArray();
    for (long seed : SEEDS) {
      RandomSupport.Seed128bit s = RandomSupport.upgradeSeedTo128bit(seed);
      JsonArray e = new JsonArray();
      e.add(seed);
      e.add(s.seedLo());
      e.add(s.seedHi());
      upgrade.add(e);
    }
    root.add("upgrade_seed", upgrade);

    JsonArray hash = new JsonArray();
    for (String name : new String[] {"", "minecraft:terrain", "minecraft:aquifer", "octave_-3", "ä→ß"}) {
      RandomSupport.Seed128bit s = RandomSupport.seedFromHashOf(name);
      JsonArray e = new JsonArray();
      e.add(name);
      e.add(s.seedLo());
      e.add(s.seedHi());
      hash.add(e);
    }
    root.add("seed_from_hash_of", hash);

    JsonArray getSeed = new JsonArray();
    int[][] positions = {{0, 0, 0}, {1, 2, 3}, {-1, -64, -1}, {30000000, 320, -30000000}, {12345, -17, 99}};
    for (int[] p : positions) {
      JsonArray e = new JsonArray();
      e.add(p[0]);
      e.add(p[1]);
      e.add(p[2]);
      e.add(Mth.getSeed(p[0], p[1], p[2]));
      getSeed.add(e);
    }
    root.add("get_seed", getSeed);

    Field sinField = Mth.class.getDeclaredField("SIN");
    sinField.setAccessible(true);
    float[] sin = (float[]) sinField.get(null);
    long sinHash = 0;
    for (float v : sin) {
      sinHash = sinHash * 31 + Float.floatToRawIntBits(v);
    }
    root.addProperty("sin_table_hash", sinHash);
    JsonArray sinSamples = new JsonArray();
    double[] angles = {0.0, 0.5, 1.0, -1.0, 3.141592653589793, 100.25, -12345.678, 1.0e7};
    for (double a : angles) {
      JsonArray e = new JsonArray();
      e.add(Goldens.bits(a));
      e.add(Goldens.bits(Mth.sin(a)));
      e.add(Goldens.bits(Mth.cos(a)));
      sinSamples.add(e);
    }
    root.add("sin", sinSamples);

    JsonArray largeFeature = new JsonArray();
    for (long seed : SEEDS) {
      WorldgenRandom random = new WorldgenRandom(new LegacyRandomSource(0L));
      random.setLargeFeatureSeed(seed, -3, 7);
      JsonArray e = new JsonArray();
      e.add(seed);
      e.add(random.nextLong());
      e.add(random.setDecorationSeed(seed, 123, -456));
      e.add(random.nextInt(1000));
      largeFeature.add(e);
    }
    root.add("worldgen_random", largeFeature);
    return root;
  }

  private static JsonObject stream(long seed, RandomSource random) {
    JsonObject o = new JsonObject();
    o.addProperty("seed", seed);
    JsonArray longs = new JsonArray();
    for (int i = 0; i < 4; i++) {
      longs.add(random.nextLong());
    }
    o.add("long", longs);
    o.addProperty("int", random.nextInt());
    JsonArray bounded = new JsonArray();
    for (int bound : new int[] {1, 2, 3, 7, 16, 100, 1000000007, Integer.MAX_VALUE}) {
      bounded.add(random.nextInt(bound));
    }
    o.add("int_bounded", bounded);
    o.addProperty("float", Goldens.bits(random.nextFloat()));
    o.addProperty("double", Goldens.bits(random.nextDouble()));
    o.addProperty("gaussian", Goldens.bits(random.nextGaussian()));
    o.addProperty("boolean", random.nextBoolean());
    PositionalRandomFactory positional = random.forkPositional();
    o.addProperty("at", positional.at(1, -2, 3).nextLong());
    o.addProperty("from_hash_of", positional.fromHashOf("minecraft:test").nextLong());
    o.addProperty("from_seed", positional.fromSeed(77L).nextLong());
    o.addProperty("fork", random.fork().nextLong());
    return o;
  }
}

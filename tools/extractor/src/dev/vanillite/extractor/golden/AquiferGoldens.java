// Copyright Hannes Thöne. Licensed under the Apache License, Version 2.0.
package dev.vanillite.extractor.golden;

import com.google.gson.JsonArray;
import com.google.gson.JsonElement;
import com.google.gson.JsonObject;
import java.lang.reflect.Method;
import net.minecraft.core.HolderLookup;
import net.minecraft.core.registries.Registries;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.levelgen.Aquifer;
import net.minecraft.world.level.levelgen.NoiseBasedChunkGenerator;
import net.minecraft.world.level.levelgen.NoiseChunk;
import net.minecraft.world.level.levelgen.NoiseGeneratorSettings;
import net.minecraft.world.level.levelgen.RandomState;
import net.minecraft.world.level.levelgen.blending.Blender;
import net.minecraft.world.level.levelgen.densityfunction.DensitySampler;
import net.minecraft.world.level.levelgen.densityfunction.DensityVolume;
import net.minecraft.world.level.levelgen.densityfunction.ScopedDensityBuffer;

/**
 * Goldens for the noise-based aquifer: Vanilla's {@code NoiseChunk} for whole overworld chunks
 * (no structures, no blending), {@code computeSubstance} for every block in {@code doFill}
 * order, then a carver-style pass with density {@code 0.0} on a few columns.
 */
public final class AquiferGoldens {
  private static final long[] SEEDS = {0L, -7302402383914416L};
  private static final int[][] CHUNKS = {{0, 0}, {5, -3}, {-20, 7}, {100, -100}, {-37, 52}, {12, 300}};
  /** Chunk-relative columns of the carver-style pass. */
  private static final int[][] CARVE_COLUMNS = {{0, 0}, {7, 9}, {15, 3}, {4, 15}};
  private static final int CARVE_TOP = 120;
  private static final int CARVE_BOTTOM = -63;
  /** Blocks per chunk recorded explicitly (every n-th in doFill order). */
  private static final int SAMPLE_STRIDE = 997;

  private AquiferGoldens() {}

  public static JsonElement extract() throws Exception {
    HolderLookup.Provider lookup = VanillaRegistries.createWorldLookup();
    NoiseGeneratorSettings settings =
        lookup.lookupOrThrow(Registries.NOISE_SETTINGS).getOrThrow(NoiseGeneratorSettings.OVERWORLD).value();
    Method createFluidPicker =
        NoiseBasedChunkGenerator.class.getDeclaredMethod("createFluidPicker", NoiseGeneratorSettings.class);
    createFluidPicker.setAccessible(true);
    Aquifer.FluidPicker picker = (Aquifer.FluidPicker) createFluidPicker.invoke(null, settings);

    JsonObject root = new JsonObject();
    root.addProperty("settings", "minecraft:overworld");
    root.addProperty("carve_top", CARVE_TOP);
    root.addProperty("carve_bottom", CARVE_BOTTOM);
    root.addProperty("sample_stride", SAMPLE_STRIDE);
    JsonArray columns = new JsonArray();
    for (int[] c : CARVE_COLUMNS) {
      JsonArray e = new JsonArray();
      e.add(c[0]);
      e.add(c[1]);
      columns.add(e);
    }
    root.add("carve_columns", columns);
    JsonArray chunks = new JsonArray();
    for (long seed : SEEDS) {
      RandomState randomState = RandomState.create(lookup.lookupOrThrow(Registries.NOISE), seed, settings);
      for (int[] c : CHUNKS) {
        chunks.add(chunk(settings, randomState, picker, seed, c[0], c[1]));
      }
    }
    root.add("chunks", chunks);
    return root;
  }

  private static int id(BlockState state) {
    return state == null ? -1 : Block.getId(state);
  }

  private static JsonObject chunk(
      NoiseGeneratorSettings settings, RandomState randomState, Aquifer.FluidPicker picker, long seed, int chunkX, int chunkZ) {
    int minY = settings.noiseSettings().minY();
    int height = settings.noiseSettings().height();
    DensityVolume volume = new DensityVolume(16, height, 16, chunkX << 4, minY, chunkZ << 4);
    JsonObject o = new JsonObject();
    o.addProperty("seed", seed);
    o.addProperty("x", chunkX);
    o.addProperty("z", chunkZ);
    try (NoiseChunk noiseChunk = new NoiseChunk(randomState, null, settings, picker, Blender.empty(), volume)) {
      Aquifer aquifer = noiseChunk.aquifer();
      DensitySampler.Bound finalDensity = noiseChunk.cachingSamplers().get(settings.noiseRouter().finalDensity());
      long fillHash = 0;
      int solid = 0;
      int scheduled = 0;
      int index = 0;
      JsonArray samples = new JsonArray();
      try (ScopedDensityBuffer densityBuffer = finalDensity.sampleVolume(volume)) {
        for (int z = 0; z < volume.sizeZ(); z++) {
          int blockZ = volume.blockZ(z);
          for (int x = 0; x < volume.sizeX(); x++) {
            int blockX = volume.blockX(x);
            for (int y = volume.sizeY() - 1; y >= 0; y--) {
              int blockY = volume.blockY(y);
              float density = densityBuffer.get(volume.indexUnchecked(x, y, z));
              BlockState state = aquifer.computeSubstance(blockX, blockY, blockZ, density);
              boolean schedule = aquifer.shouldScheduleFluidUpdate();
              fillHash = (fillHash * 31 + id(state)) * 31 + (schedule ? 1 : 0);
              if (state == null) {
                solid++;
              }
              if (schedule) {
                scheduled++;
              }
              if (index % SAMPLE_STRIDE == 0) {
                JsonArray s = new JsonArray();
                s.add(blockX);
                s.add(blockY);
                s.add(blockZ);
                s.add(Goldens.bits(density));
                s.add(id(state));
                s.add(schedule);
                samples.add(s);
              }
              index++;
            }
          }
        }
      }
      o.addProperty("fill_hash", fillHash);
      o.addProperty("solid", solid);
      o.addProperty("scheduled", scheduled);
      o.add("samples", samples);

      long carveHash = 0;
      for (int[] c : CARVE_COLUMNS) {
        int blockX = (chunkX << 4) + c[0];
        int blockZ = (chunkZ << 4) + c[1];
        for (int blockY = CARVE_TOP; blockY >= CARVE_BOTTOM; blockY--) {
          BlockState state = aquifer.computeSubstance(blockX, blockY, blockZ, 0.0);
          carveHash = (carveHash * 31 + id(state)) * 31 + (aquifer.shouldScheduleFluidUpdate() ? 1 : 0);
        }
      }
      o.addProperty("carve_hash", carveHash);
    }
    return o;
  }
}

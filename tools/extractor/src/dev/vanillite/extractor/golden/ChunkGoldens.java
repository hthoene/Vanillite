// Copyright Hannes Thöne. Licensed under the Apache License, Version 2.0.
package dev.vanillite.extractor.golden;

import com.google.gson.JsonArray;
import com.google.gson.JsonElement;
import com.google.gson.JsonObject;
import it.unimi.dsi.fastutil.shorts.ShortList;
import java.lang.reflect.Field;
import java.lang.reflect.Method;
import java.util.ArrayList;
import java.util.Comparator;
import java.util.HashMap;
import java.util.HashSet;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.function.Supplier;
import net.minecraft.core.Holder;
import net.minecraft.core.HolderGetter;
import net.minecraft.core.HolderLookup;
import net.minecraft.core.IdMapper;
import net.minecraft.core.registries.Registries;
import net.minecraft.data.registries.VanillaRegistries;
import net.minecraft.world.level.ChunkPos;
import net.minecraft.world.level.LevelHeightAccessor;
import net.minecraft.world.level.biome.Biome;
import net.minecraft.world.level.biome.BiomeManager;
import net.minecraft.world.level.biome.BiomeSource;
import net.minecraft.world.level.biome.MultiNoiseBiomeSource;
import net.minecraft.world.level.biome.MultiNoiseBiomeSourceParameterLists;
import net.minecraft.world.level.biome.TheEndBiomeSource;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.Blocks;
import net.minecraft.world.level.chunk.ChunkAccess;
import net.minecraft.world.level.chunk.LevelChunkSection;
import net.minecraft.world.level.chunk.PalettedContainerFactory;
import net.minecraft.world.level.chunk.ProtoChunk;
import net.minecraft.world.level.chunk.Strategy;
import net.minecraft.world.level.chunk.UpgradeData;
import net.minecraft.world.level.levelgen.Aquifer;
import net.minecraft.world.level.levelgen.Heightmap;
import net.minecraft.world.level.levelgen.NoiseBasedChunkGenerator;
import net.minecraft.world.level.levelgen.NoiseChunk;
import net.minecraft.world.level.levelgen.NoiseGeneratorSettings;
import net.minecraft.world.level.levelgen.RandomState;
import net.minecraft.world.level.levelgen.blending.Blender;
import net.minecraft.world.level.levelgen.densityfunction.DensityVolume;
import net.minecraft.world.level.levelgen.material.rule.MaterialRule;
import net.minecraft.resources.ResourceKey;

/**
 * Per-stage chunk goldens: Vanilla's {@code NoiseBasedChunkGenerator} on fresh proto chunks
 * without structures or blending. The biomes stage runs for the 3×3 area around each chunk; the
 * terrain stage runs its three steps (noise fill, material rules, carvers) one after another
 * through the generator's own private methods, recording the chunk after each.
 */
public final class ChunkGoldens {
  private record Dimension(
      String name, ResourceKey<NoiseGeneratorSettings> settings, long seed, int minY, int height, int[][] chunks) {}

  private static final Dimension[] DIMENSIONS = {
    new Dimension("minecraft:overworld", NoiseGeneratorSettings.OVERWORLD, 0L, -64, 384,
        new int[][] {{0, 0}, {1, -1}, {-7, 13}, {37, -52}, {-150, 90}, {400, -1000}}),
    new Dimension("minecraft:overworld", NoiseGeneratorSettings.OVERWORLD, -7302402383914416L, -64, 384,
        new int[][] {{0, 0}, {-3, 8}, {64, 64}, {-500, 250}}),
    new Dimension("minecraft:nether", NoiseGeneratorSettings.NETHER, 42L, 0, 256,
        new int[][] {{0, 0}, {3, -5}, {-20, 11}}),
    new Dimension("minecraft:end", NoiseGeneratorSettings.END, 1L, 0, 256,
        new int[][] {{0, 0}, {100, 0}, {-70, 64}}),
  };

  private ChunkGoldens() {}

  public static JsonElement extract() throws Exception {
    BlockTags.bind();
    HolderLookup.Provider lookup = VanillaRegistries.createWorldLookup();
    HolderGetter<Biome> biomes = lookup.lookupOrThrow(Registries.BIOME);
    PalettedContainerFactory factory = containerFactory(lookup);

    JsonArray out = new JsonArray();
    for (Dimension dimension : DIMENSIONS) {
      Holder<NoiseGeneratorSettings> settings = lookup.lookupOrThrow(Registries.NOISE_SETTINGS).getOrThrow(dimension.settings());
      BiomeSource source = switch (dimension.name()) {
        case "minecraft:nether" -> MultiNoiseBiomeSource.createFromPreset(
            lookup.lookupOrThrow(Registries.MULTI_NOISE_BIOME_SOURCE_PARAMETER_LIST).getOrThrow(MultiNoiseBiomeSourceParameterLists.NETHER));
        case "minecraft:end" -> TheEndBiomeSource.create(biomes);
        default -> MultiNoiseBiomeSource.createFromPreset(
            lookup.lookupOrThrow(Registries.MULTI_NOISE_BIOME_SOURCE_PARAMETER_LIST).getOrThrow(MultiNoiseBiomeSourceParameterLists.OVERWORLD));
      };
      NoiseBasedChunkGenerator generator = new NoiseBasedChunkGenerator(source, settings);
      RandomState randomState = RandomState.create(lookup.lookupOrThrow(Registries.NOISE), dimension.seed(), settings.value());
      for (int[] c : dimension.chunks()) {
        out.add(chunk(dimension, generator, settings.value(), randomState, factory, new ChunkPos(c[0], c[1])));
      }
    }
    JsonObject root = new JsonObject();
    root.add("chunks", out);
    return root;
  }

  /** The biome registry in protocol order (identifier order), as Vanillite numbers biomes. */
  static PalettedContainerFactory containerFactory(HolderLookup.Provider lookup) {
    List<Holder.Reference<Biome>> list = new ArrayList<>(lookup.lookupOrThrow(Registries.BIOME).listElements().toList());
    list.sort(Comparator.comparing((Holder.Reference<Biome> h) -> h.key().identifier().getPath())
        .thenComparing(h -> h.key().identifier().getNamespace()));
    IdMapper<Holder<Biome>> ids = new IdMapper<>(list.size());
    Holder<Biome> plains = null;
    for (int i = 0; i < list.size(); i++) {
      ids.addMapping(list.get(i), i);
      if (list.get(i).key().identifier().getPath().equals("plains")) {
        plains = list.get(i);
      }
    }
    return new PalettedContainerFactory(
        Strategy.createForBlockStates(Block.BLOCK_STATE_REGISTRY), Blocks.AIR.defaultBlockState(), null,
        Strategy.createForBiomes(ids), plains, null);
  }

  private static JsonObject chunk(
      Dimension dimension, NoiseBasedChunkGenerator generator, NoiseGeneratorSettings settings,
      RandomState randomState, PalettedContainerFactory factory, ChunkPos center) throws Exception {
    LevelHeightAccessor height = LevelHeightAccessor.create(dimension.minY(), dimension.height());
    Map<Long, ProtoChunk> area = new HashMap<>();
    for (int dz = -1; dz <= 1; dz++) {
      for (int dx = -1; dx <= 1; dx++) {
        ChunkPos pos = new ChunkPos(center.x() + dx, center.z() + dz);
        ProtoChunk chunk = new ProtoChunk(pos, UpgradeData.EMPTY, height, factory, null);
        generator.createBiomes(randomState, Blender.empty(), null, chunk).join();
        chunk.setPersistedStatus(net.minecraft.world.level.chunk.status.ChunkStatus.BIOMES);
        area.put(pos.pack(), chunk);
      }
    }
    ProtoChunk chunk = area.get(center.pack());
    JsonObject o = new JsonObject();
    o.addProperty("dimension", dimension.name());
    o.addProperty("seed", dimension.seed());
    o.addProperty("min_y", dimension.minY());
    o.addProperty("height", dimension.height());
    o.addProperty("x", center.x());
    o.addProperty("z", center.z());
    o.addProperty("biomes", biomeHash(chunk));

    Set<Holder<Biome>> possibleBiomes = new HashSet<>();
    for (ProtoChunk c : area.values()) {
      c.collectBiomesInPalette(possibleBiomes);
    }
    BiomeManager biomeManager = new BiomeManager(
        (qx, qy, qz) -> area.get(ChunkPos.pack(qx >> 2, qz >> 2)).getNoiseBiome(qx, qy, qz),
        BiomeManager.obfuscateSeed(dimension.seed()));

    Field pickerField = NoiseBasedChunkGenerator.class.getDeclaredField("globalFluidPicker");
    pickerField.setAccessible(true);
    @SuppressWarnings("unchecked")
    Aquifer.FluidPicker picker = ((Supplier<Aquifer.FluidPicker>) pickerField.get(generator)).get();
    DensityVolume volume = new DensityVolume(16, settings.noiseSettings().height(), 16,
        center.getMinBlockX(), settings.noiseSettings().minY(), center.getMinBlockZ());
    MaterialRule materialRule = settings.materialRule().value();

    Method doFill = method("doFill", NoiseChunk.class, ChunkAccess.class);
    Method buildSurface = method("buildSurface", ChunkAccess.class, NoiseChunk.class, RandomState.class,
        BiomeManager.class, Set.class, MaterialRule.class);
    Method generateCarvers = method("generateCarvers", ChunkAccess.class, Blender.class, NoiseChunk.class,
        RandomState.class, BiomeManager.class, net.minecraft.server.level.WorldGenRegion.class, MaterialRule.class);
    try (NoiseChunk noiseChunk = new NoiseChunk(randomState, null, settings, picker, Blender.empty(), volume)) {
      doFill.invoke(generator, noiseChunk, chunk);
      o.add("noise", snapshot(chunk));
      buildSurface.invoke(generator, chunk, noiseChunk, randomState, biomeManager, possibleBiomes, materialRule);
      o.add("surface", snapshot(chunk));
      generateCarvers.invoke(generator, chunk, Blender.empty(), noiseChunk, randomState, biomeManager, null, materialRule);
      o.add("carvers", snapshot(chunk));
    }
    return o;
  }

  private static Method method(String name, Class<?>... types) throws NoSuchMethodException {
    Method m = NoiseBasedChunkGenerator.class.getDeclaredMethod(name, types);
    m.setAccessible(true);
    return m;
  }

  /** Hash of the biome ids (Java string hash of the name) in section, y, z, x order. */
  private static long biomeHash(ProtoChunk chunk) {
    long hash = 0;
    int minQuart = chunk.getMinY() >> 2;
    for (int qy = minQuart; qy < minQuart + (chunk.getHeight() >> 2); qy++) {
      for (int qz = 0; qz < 4; qz++) {
        for (int qx = 0; qx < 4; qx++) {
          String name = chunk.getNoiseBiome((chunk.getPos().x() << 2) + qx, qy, (chunk.getPos().z() << 2) + qz)
              .unwrapKey().orElseThrow().identifier().toString();
          hash = hash * 31 + name.hashCode();
        }
      }
    }
    return hash;
  }

  /** Per-section block hashes, WG heightmaps and post-processing marks. */
  private static JsonObject snapshot(ProtoChunk chunk) {
    JsonObject o = new JsonObject();
    JsonArray sections = new JsonArray();
    for (LevelChunkSection section : chunk.getSections()) {
      long hash = 0;
      for (int y = 0; y < 16; y++) {
        for (int z = 0; z < 16; z++) {
          for (int x = 0; x < 16; x++) {
            hash = hash * 31 + Block.getId(section.getBlockState(x, y, z));
          }
        }
      }
      sections.add(hash);
    }
    o.add("sections", sections);
    for (Heightmap.Types type : new Heightmap.Types[] {Heightmap.Types.WORLD_SURFACE_WG, Heightmap.Types.OCEAN_FLOOR_WG}) {
      long hash = 0;
      for (int z = 0; z < 16; z++) {
        for (int x = 0; x < 16; x++) {
          hash = hash * 31 + chunk.getHeight(type, x, z);
        }
      }
      o.addProperty(type.getSerializationKey().toLowerCase(), hash);
    }
    JsonArray post = new JsonArray();
    for (ShortList list : chunk.getPostProcessing()) {
      long hash = 0;
      int count = 0;
      if (list != null) {
        for (int i = 0; i < list.size(); i++) {
          hash = hash * 31 + list.getShort(i);
          count++;
        }
      }
      JsonArray e = new JsonArray();
      e.add(count);
      e.add(hash);
      post.add(e);
    }
    o.add("post_processing", post);
    return o;
  }
}

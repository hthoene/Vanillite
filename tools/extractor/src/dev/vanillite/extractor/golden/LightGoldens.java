// Copyright Hannes Thöne. Licensed under the Apache License, Version 2.0.
package dev.vanillite.extractor.golden;

import com.google.gson.JsonArray;
import com.google.gson.JsonElement;
import com.google.gson.JsonObject;
import java.io.ByteArrayOutputStream;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Base64;
import java.util.Comparator;
import java.util.List;
import java.util.concurrent.CompletableFuture;
import java.util.stream.Stream;
import net.minecraft.nbt.CompoundTag;
import net.minecraft.nbt.ListTag;
import net.minecraft.nbt.NbtIo;
import net.minecraft.nbt.Tag;
import net.minecraft.server.level.ServerLevel;
import net.minecraft.server.level.ThreadedLevelLightEngine;
import net.minecraft.world.level.chunk.ChunkAccess;
import net.minecraft.world.level.chunk.status.ChunkStatus;
import net.minecraft.world.level.chunk.storage.SerializableChunkData;

/**
 * Light of freshly generated chunks: a {@link ParityServer} with Vanilla's normal overworld
 * generates the 3×3 area around each center chunk and lights it; the golden holds the blocks of
 * the area and the light Vanilla computed for the center chunk.
 *
 * <p>The 5×5 area gets its features first, so nothing writes into the 3×3 area while it is lit
 * (Vanilla's parallel generation can otherwise leave stale sky light sources behind, differently
 * in every run). Each chunk is the chunk NBT reduced to position, status, sections (blocks; light
 * only in the center), gzip-compressed and in Base64.
 */
public final class LightGoldens {
  private static final long SEED = -578941029922802438L;

  /** Center chunks: a coast with forest, snow, caves and lava; a forest over large caves. */
  private static final int[][] CENTERS = {{-67, -16}, {-61, -13}};

  private LightGoldens() {}

  public static JsonElement extract() throws Exception {
    Path dir = Files.createTempDirectory("vanillite-light");
    ParityServer server = ParityServer.start(dir, SEED, null, null);
    try {
      ServerLevel level = server.level();
      JsonObject root = new JsonObject();
      root.addProperty("seed", SEED);
      JsonArray areas = new JsonArray();
      for (int[] center : CENTERS) {
        int cx = center[0];
        int cz = center[1];
        for (int dz = -2; dz <= 2; dz++) {
          for (int dx = -2; dx <= 2; dx++) {
            level.getChunk(cx + dx, cz + dz, ChunkStatus.FEATURES, true);
          }
        }
        List<ChunkAccess> chunks = new ArrayList<>();
        for (int dz = -1; dz <= 1; dz++) {
          for (int dx = -1; dx <= 1; dx++) {
            chunks.add(level.getChunk(cx + dx, cz + dz, ChunkStatus.FULL, true));
          }
        }
        settleLight(level, cx, cz);
        JsonObject area = new JsonObject();
        JsonArray pos = new JsonArray();
        pos.add(cx);
        pos.add(cz);
        area.add("center", pos);
        JsonArray encoded = new JsonArray();
        for (ChunkAccess chunk : chunks) {
          boolean isCenter = chunk.getPos().x() == cx && chunk.getPos().z() == cz;
          encoded.add(encode(SerializableChunkData.copyOf(level, chunk).write(), isCenter));
        }
        area.add("chunks", encoded);
        areas.add(area);
      }
      root.add("areas", areas);
      return root;
    } finally {
      server.shutdown();
      try (Stream<Path> paths = Files.walk(dir)) {
        for (Path p : paths.sorted(Comparator.reverseOrder()).toList()) {
          Files.deleteIfExists(p);
        }
      }
    }
  }

  /** Runs the light engine until no task around the area and no light work is left. */
  private static void settleLight(ServerLevel level, int cx, int cz) throws InterruptedException {
    ThreadedLevelLightEngine light = level.getChunkSource().getLightEngine();
    while (true) {
      List<CompletableFuture<?>> pending = new ArrayList<>();
      for (int dz = -2; dz <= 2; dz++) {
        for (int dx = -2; dx <= 2; dx++) {
          pending.add(light.waitForPendingTasks(cx + dx, cz + dz));
        }
      }
      while (!pending.stream().allMatch(CompletableFuture::isDone)) {
        light.tryScheduleUpdate();
        level.getChunkSource().pollTask();
        Thread.sleep(1);
      }
      if (!light.hasLightWork()) {
        return;
      }
    }
  }

  private static String encode(CompoundTag full, boolean withLight) throws Exception {
    CompoundTag tag = new CompoundTag();
    for (String key : new String[] {"xPos", "yPos", "zPos", "Status", "DataVersion"}) {
      Tag value = full.get(key);
      if (value != null) {
        tag.put(key, value);
      }
    }
    if (withLight) {
      tag.putBoolean("isLightOn", true);
    }
    ListTag sections = new ListTag();
    for (Tag t : full.getListOrEmpty("sections")) {
      CompoundTag section = (CompoundTag) t;
      CompoundTag out = new CompoundTag();
      out.put("Y", section.get("Y"));
      if (section.contains("block_states")) {
        out.put("block_states", section.get("block_states"));
      }
      if (withLight) {
        for (String key : new String[] {"BlockLight", "SkyLight"}) {
          if (section.contains(key)) {
            out.put(key, section.get(key));
          }
        }
      }
      sections.add(out);
    }
    tag.put("sections", sections);
    ByteArrayOutputStream bytes = new ByteArrayOutputStream();
    NbtIo.writeCompressed(tag, bytes);
    return Base64.getEncoder().encodeToString(bytes.toByteArray());
  }
}

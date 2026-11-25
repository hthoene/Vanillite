// Copyright Hannes Thöne. Licensed under the Apache License, Version 2.0.
package dev.vanillite.extractor.golden;

import com.google.gson.JsonArray;
import com.google.gson.JsonElement;
import com.google.gson.JsonObject;
import com.google.gson.JsonParser;
import java.io.IOException;
import java.lang.reflect.Field;
import java.lang.reflect.Method;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Comparator;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import java.util.concurrent.CompletableFuture;
import java.util.stream.Stream;
import net.minecraft.SharedConstants;
import net.minecraft.commands.arguments.blocks.BlockStateParser;
import net.minecraft.core.BlockPos;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.server.Bootstrap;
import net.minecraft.server.level.ChunkLevel;
import net.minecraft.server.level.ChunkMap;
import net.minecraft.server.level.FullChunkStatus;
import net.minecraft.server.level.ServerChunkCache;
import net.minecraft.server.level.ServerLevel;
import net.minecraft.server.level.ThreadedLevelLightEngine;
import net.minecraft.world.entity.Entity;
import net.minecraft.world.entity.player.Player;
import net.minecraft.world.level.ChunkPos;
import net.minecraft.world.level.LightLayer;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.block.state.properties.Property;
import net.minecraft.world.level.entity.EntityTypeTest;
import net.minecraft.world.level.entity.PersistentEntitySectionManager;
import net.minecraft.world.phys.Vec3;
import net.minecraft.world.ticks.LevelChunkTicks;
import net.minecraft.world.ticks.LevelTicks;
import net.minecraft.world.ticks.ScheduledTick;

/**
 * Records parity scenarios on Vanilla, tick by tick.
 *
 * <p>Each scenario (JSON, format in {@code crates/vanillite-test/src/scenario.rs}) runs in a
 * fresh {@link ParityServer}: its chunks are loaded and ticking, the setup is applied without
 * updates, frame 0 is recorded, then every tick applies the actions of that tick, runs one server
 * tick and records a frame: the block states of the region (frame 0 in full, later frames as
 * changes), the pending block and fluid ticks in drain order, the entities in the region and, for
 * scenarios with {@code "light": true}, the settled sky and block light of the region.
 * Positions in chunks that are not accessible are recorded as {@link #UNLOADED}.
 *
 * <p>The actions {@code force} and {@code unforce} add or remove forced tickets of chunks and then
 * run the chunk system and the entity manager without a game tick until every chunk around the
 * scenario is loaded or unloaded, with its entities, as its ticket level says.
 *
 * <p>Usage: {@code java -cp ... dev.vanillite.extractor.golden.ScenarioRecorder (<scenario.json>
 * <trace.json>)...}
 */
public final class ScenarioRecorder {
  /** Trace format version; the Rust reader rejects other versions. */
  private static final int FORMAT = 1;

  /** The traced state of a position in a chunk that is not accessible. */
  static final String UNLOADED = "<unloaded>";

  /** Chunks around the forced ones whose state a settle waits for. */
  private static final int SETTLE_MARGIN = 3;

  /** Game rules every scenario starts with; a scenario's {@code rules} override them. */
  private static final String[][] DEFAULT_RULES = {
    {"random_tick_speed", "0"},
    {"spawn_mobs", "false"},
    {"advance_time", "false"},
    {"advance_weather", "false"},
  };

  private ScenarioRecorder() {}

  public static void main(String[] args) throws Exception {
    if (args.length == 0 || args.length % 2 != 0) {
      System.err.println("usage: ScenarioRecorder (<scenario.json> <trace.json>)...");
      System.exit(2);
    }
    SharedConstants.tryDetectVersion();
    Bootstrap.bootStrap();
    for (int i = 0; i < args.length; i += 2) {
      Path scenario = Path.of(args[i]);
      Path out = Path.of(args[i + 1]);
      long start = System.nanoTime();
      String trace = record(JsonParser.parseString(Files.readString(scenario)).getAsJsonObject());
      Files.createDirectories(out.toAbsolutePath().getParent());
      Files.writeString(out, trace, StandardCharsets.UTF_8);
      System.out.printf(
          "scenario: %s -> %s (%d ms)%n", scenario, out, (System.nanoTime() - start) / 1_000_000);
    }
    // The server leaves worker threads behind; the recording is complete.
    System.exit(0);
  }

  static String record(JsonObject scenario) throws Exception {
    JsonObject world = scenario.has("world") ? scenario.getAsJsonObject("world") : new JsonObject();
    long seed = world.has("seed") ? world.get("seed").getAsLong() : 0;
    String biome = world.has("biome") ? world.get("biome").getAsString() : "minecraft:plains";
    List<ParityServer.Layer> layers = new ArrayList<>();
    if (world.has("layers")) {
      for (JsonElement l : world.getAsJsonArray("layers")) {
        JsonArray a = l.getAsJsonArray();
        layers.add(new ParityServer.Layer(a.get(0).getAsString(), a.get(1).getAsInt()));
      }
    } else {
      layers.add(new ParityServer.Layer("minecraft:bedrock", 1));
      layers.add(new ParityServer.Layer("minecraft:dirt", 2));
      layers.add(new ParityServer.Layer("minecraft:grass_block", 1));
    }

    ParityHooks.reset();
    Path dir = Files.createTempDirectory("vanillite-parity");
    ParityServer server = ParityServer.start(dir, seed, layers, biome);
    ScenarioEntities entities = null;
    try {
      Map<String, String> rules = new java.util.LinkedHashMap<>();
      for (String[] rule : DEFAULT_RULES) {
        rules.put(rule[0], rule[1]);
      }
      if (scenario.has("rules")) {
        for (Map.Entry<String, JsonElement> e : scenario.getAsJsonObject("rules").entrySet()) {
          rules.put(e.getKey(), e.getValue().getAsString());
        }
      }
      rules.forEach((k, v) -> server.command("gamerule " + k + " " + v));

      ServerLevel level = server.level();
      JsonArray region = scenario.getAsJsonArray("region");
      BlockPos min = pos(region.get(0));
      BlockPos max = pos(region.get(1));
      loadRegion(server, level, min, max);
      Area area = new Area(min, max);

      // The setup may draw from the level random (blocks that schedule a random delay when
      // placed): it starts from the world seed, like Vanillite's new level.
      level.getRandom().setSeed(seed);
      if (scenario.has("setup")) {
        for (JsonElement op : scenario.getAsJsonArray("setup")) {
          apply(level, op.getAsJsonObject(), false);
        }
      }

      if (scenario.has("weather")) {
        String weather = scenario.get("weather").getAsString();
        boolean thunder = weather.equals("thunder");
        if (!thunder && !weather.equals("rain") && !weather.equals("clear")) {
          throw new IllegalArgumentException("unknown weather " + weather);
        }
        boolean rain = !weather.equals("clear");
        // Full rain (and thunder) at once instead of the client fade of 0.01 per tick.
        server.setWeatherParameters(0, rain ? 1_000_000 : 0, rain, thunder);
        level.setRainLevel(rain ? 1.0F : 0.0F);
        level.setThunderLevel(thunder ? 1.0F : 0.0F);
      }
      if (scenario.has("time")) {
        server.command("time set " + scenario.get("time").getAsLong());
      }
      server.command(
          "difficulty "
              + (scenario.has("difficulty") ? scenario.get("difficulty").getAsString() : "normal"));
      entities = new ScenarioEntities(server, level);
      JsonArray specs = scenario.has("entities") ? scenario.getAsJsonArray("entities") : new JsonArray();
      // Players join first; then the level random starts from the world seed and the mobs come.
      for (int i = 0; i < specs.size(); i++) {
        JsonObject spec = specs.get(i).getAsJsonObject();
        if (spec.get("type").getAsString().equals("minecraft:player")) {
          entities.player(spec, i);
        }
      }
      level.getRandom().setSeed(seed);
      // The LCG of random tick positions starts from the seed too (Vanilla seeds it from a
      // thread-local random).
      Field randValue = net.minecraft.world.level.Level.class.getDeclaredField("randValue");
      randValue.setAccessible(true);
      randValue.setInt(level, (int) seed);
      for (int i = 0; i < specs.size(); i++) {
        JsonObject spec = specs.get(i).getAsJsonObject();
        if (!spec.get("type").getAsString().equals("minecraft:player")) {
          entities.mob(spec, i);
        }
      }
      ParityHooks.startStream(scenario.has("entity_seed") ? scenario.get("entity_seed").getAsLong() : seed);

      boolean light = scenario.has("light") && scenario.get("light").getAsBoolean();
      boolean levelRng = scenario.has("level_rng") && scenario.get("level_rng").getAsBoolean();
      Trace trace = new Trace(min, max, entities, light, levelRng);
      trace.frame(level, 0, true);
      int ticks = scenario.get("ticks").getAsInt();
      JsonArray actions = scenario.has("actions") ? scenario.getAsJsonArray("actions") : null;
      for (int tick = 1; tick <= ticks; tick++) {
        if (actions != null) {
          for (JsonElement a : actions) {
            JsonObject action = a.getAsJsonObject();
            if (action.get("tick").getAsInt() == tick) {
              String op = action.get("op").getAsString();
              if (op.equals("force") || op.equals("unforce")) {
                for (JsonElement c : action.getAsJsonArray("chunks")) {
                  JsonArray xz = c.getAsJsonArray();
                  area.add(xz.get(0).getAsInt(), xz.get(1).getAsInt());
                  level.setChunkForced(xz.get(0).getAsInt(), xz.get(1).getAsInt(), op.equals("force"));
                }
                settle(server, level, area);
              } else if (op.equals("hurt")) {
                entities.hurt(action);
              } else if (op.equals("use_item")) {
                entities.useItem(action);
              } else if (action.get("op").getAsString().equals("effect")) {
                entities.effect(action);
              } else {
                apply(level, action, true);
              }
            }
          }
        }
        server.step();
        // The connection tick of the players (`ServerGamePacketListenerImpl.tick`), which the
        // server runs after the levels.
        entities.tickPlayers();
        trace.frame(level, tick, false);
      }
      return trace.finish();
    } finally {
      if (entities != null) {
        entities.release();
      }
      server.shutdown();
      deleteTree(dir);
    }
  }

  /** Loads the region's chunks and their neighbours and steps until they all tick. */
  private static void loadRegion(ParityServer server, ServerLevel level, BlockPos min, BlockPos max) {
    List<Long> chunks = new ArrayList<>();
    for (int cx = (min.getX() >> 4) - 1; cx <= (max.getX() >> 4) + 1; cx++) {
      for (int cz = (min.getZ() >> 4) - 1; cz <= (max.getZ() >> 4) + 1; cz++) {
        level.setChunkForced(cx, cz, true);
        level.getChunk(cx, cz);
        chunks.add(ChunkPos.pack(cx, cz));
      }
    }
    // Entities load asynchronously on the IO worker, which can be slow on a busy machine: wait
    // by wall time, not by a number of warm-up ticks.
    long deadline = System.nanoTime() + 60_000_000_000L;
    while (true) {
      if (chunks.stream().allMatch(c -> level.shouldTickBlocksAt(c) && level.areEntitiesLoaded(c))) {
        return;
      }
      if (System.nanoTime() > deadline) {
        throw new IllegalStateException("the scenario region does not become ticking");
      }
      server.step();
      try {
        Thread.sleep(1);
      } catch (InterruptedException e) {
        Thread.currentThread().interrupt();
        throw new IllegalStateException("interrupted while loading the scenario region", e);
      }
    }
  }

  /** The chunks a settle checks: around every chunk a scenario forced or unforced. */
  private static final class Area {
    private int minX;
    private int minZ;
    private int maxX;
    private int maxZ;

    Area(BlockPos min, BlockPos max) {
      minX = (min.getX() >> 4) - 1;
      minZ = (min.getZ() >> 4) - 1;
      maxX = (max.getX() >> 4) + 1;
      maxZ = (max.getZ() >> 4) + 1;
    }

    void add(int x, int z) {
      minX = Math.min(minX, x);
      minZ = Math.min(minZ, z);
      maxX = Math.max(maxX, x);
      maxZ = Math.max(maxZ, z);
    }
  }

  /**
   * Runs the chunk system (distance manager, loads, unloads and saves) and the entity manager
   * without a game tick until every chunk of the area is as its ticket level says: full and with
   * its entities loaded up to full level, otherwise not accessible and without entities, and
   * dropped above the highest loaded level.
   */
  private static void settle(ParityServer server, ServerLevel level, Area area) throws Exception {
    Field managerField = ServerLevel.class.getDeclaredField("entityManager");
    managerField.setAccessible(true);
    PersistentEntitySectionManager<?> entities =
        (PersistentEntitySectionManager<?>) managerField.get(level);
    Method holder = ChunkMap.class.getDeclaredMethod("getVisibleChunkIfPresent", long.class);
    holder.setAccessible(true);
    ServerChunkCache source = level.getChunkSource();
    long deadline = System.nanoTime() + 20_000_000_000L;
    while (true) {
      source.tick(() -> true, false);
      entities.tick();
      server.drainEveryTask();
      String reason = unsettled(level, source, holder, area);
      if (reason == null) {
        return;
      }
      if (System.nanoTime() > deadline) {
        throw new IllegalStateException("the chunk system does not settle: " + reason);
      }
      Thread.sleep(1);
    }
  }

  /** Why the area is not settled yet, {@code null} once it is. */
  private static String unsettled(
      ServerLevel level, ServerChunkCache source, Method holder, Area area) throws Exception {
    int margin = SETTLE_MARGIN;
    for (int x = area.minX - margin; x <= area.maxX + margin; x++) {
      for (int z = area.minZ - margin; z <= area.maxZ + margin; z++) {
        long key = ChunkPos.pack(x, z);
        int ticketLevel = source.chunkMap.getDistanceManager().getChunkLevel(key, false);
        boolean full = ticketLevel <= ChunkLevel.byStatus(FullChunkStatus.FULL);
        boolean accessible = source.getChunkNow(x, z) != null;
        boolean entities = level.areEntitiesLoaded(key);
        boolean dropped = ticketLevel <= ChunkLevel.MAX_LEVEL || holder.invoke(source.chunkMap, key) == null;
        if (accessible != full || entities != full || !dropped) {
          return String.format(
              "chunk %d, %d: level %d, accessible %b, entities %b, dropped %b",
              x, z, ticketLevel, accessible, entities, dropped);
        }
      }
    }
    return null;
  }

  private static void apply(ServerLevel level, JsonObject op, boolean action) throws Exception {
    String kind = op.get("op").getAsString();
    BlockState state =
        BlockStateParser.parseForBlock(
                BuiltInRegistries.BLOCK, op.get("block").getAsString(), false)
            .blockState();
    switch (kind) {
      // Setup and `set`: no neighbour or shape updates, like placing a structure.
      case "set" -> level.setBlock(pos(op.get("pos")), state, Block.UPDATE_CLIENTS | Block.UPDATE_KNOWN_SHAPE);
      case "place" -> level.setBlock(pos(op.get("pos")), state, Block.UPDATE_ALL);
      case "fill" -> {
        BlockPos a = pos(op.get("from"));
        BlockPos b = pos(op.get("to"));
        int flags = action ? Block.UPDATE_ALL : Block.UPDATE_CLIENTS | Block.UPDATE_KNOWN_SHAPE;
        for (BlockPos p : BlockPos.betweenClosed(a, b)) {
          level.setBlock(p, state, flags);
        }
      }
      default -> throw new IllegalArgumentException("unknown scenario op " + kind);
    }
  }

  private static BlockPos pos(JsonElement e) {
    JsonArray a = e.getAsJsonArray();
    return new BlockPos(a.get(0).getAsInt(), a.get(1).getAsInt(), a.get(2).getAsInt());
  }

  /** {@code name[key=value,...]} with the properties sorted by name (Vanillite's state names). */
  public static String stateName(BlockState state) {
    String name = BuiltInRegistries.BLOCK.getKey(state.getBlock()).toString();
    List<Property<?>> props = new ArrayList<>(state.getProperties());
    if (props.isEmpty()) {
      return name;
    }
    props.sort(Comparator.comparing(Property::getName));
    StringBuilder sb = new StringBuilder(name).append('[');
    for (int i = 0; i < props.size(); i++) {
      if (i > 0) {
        sb.append(',');
      }
      sb.append(props.get(i).getName()).append('=').append(valueName(state, props.get(i)));
    }
    return sb.append(']').toString();
  }

  private static <T extends Comparable<T>> String valueName(BlockState state, Property<T> p) {
    return p.getName(state.getValue(p));
  }

  private static void deleteTree(Path dir) throws IOException {
    try (Stream<Path> paths = Files.walk(dir)) {
      for (Path p : paths.sorted(Comparator.reverseOrder()).toList()) {
        Files.deleteIfExists(p);
      }
    }
  }

  /** The trace being written: palette and one JSON line per frame. */
  private static final class Trace {
    private final BlockPos min;
    private final BlockPos max;
    private final Map<String, Integer> palette = new HashMap<>();
    private final List<String> paletteOrder = new ArrayList<>();
    private final List<String> frames = new ArrayList<>();
    private final boolean light;
    private final boolean levelRng;
    private int[] last;
    private final ScenarioEntities names;

    Trace(BlockPos min, BlockPos max, ScenarioEntities names, boolean light, boolean levelRng) {
      this.min = min;
      this.max = max;
      this.names = names;
      this.light = light;
      this.levelRng = levelRng;
    }

    private int index(String state) {
      return palette.computeIfAbsent(
          state,
          s -> {
            paletteOrder.add(s);
            return paletteOrder.size() - 1;
          });
    }

    void frame(ServerLevel level, int tick, boolean full) throws Exception {
      int sx = max.getX() - min.getX() + 1;
      int sy = max.getY() - min.getY() + 1;
      int sz = max.getZ() - min.getZ() + 1;
      int[] blocks = new int[sx * sy * sz];
      BlockPos.MutableBlockPos p = new BlockPos.MutableBlockPos();
      int i = 0;
      for (int y = 0; y < sy; y++) {
        for (int z = 0; z < sz; z++) {
          for (int x = 0; x < sx; x++) {
            p.set(min.getX() + x, min.getY() + y, min.getZ() + z);
            // Reading a position of a chunk that is not accessible would load it.
            boolean loaded = level.getChunkSource().getChunkNow(p.getX() >> 4, p.getZ() >> 4) != null;
            blocks[i++] = index(loaded ? stateName(level.getBlockState(p)) : UNLOADED);
          }
        }
      }
      StringBuilder sb = new StringBuilder("{\"tick\":").append(tick);
      if (full) {
        // Run-length pairs (count, palette index) in y, z, x order.
        sb.append(",\"blocks\":[");
        int run = 0;
        boolean first = true;
        for (int j = 0; j < blocks.length; j++) {
          run++;
          if (j + 1 == blocks.length || blocks[j + 1] != blocks[j]) {
            sb.append(first ? "" : ",").append(run).append(',').append(blocks[j]);
            first = false;
            run = 0;
          }
        }
        sb.append(']');
      } else {
        sb.append(",\"changes\":[");
        boolean first = true;
        for (int j = 0; j < blocks.length; j++) {
          if (blocks[j] != last[j]) {
            int x = j % sx;
            int z = j / sx % sz;
            int y = j / (sx * sz);
            sb.append(first ? "" : ",")
                .append('[')
                .append(min.getX() + x)
                .append(',')
                .append(min.getY() + y)
                .append(',')
                .append(min.getZ() + z)
                .append(',')
                .append(blocks[j])
                .append(']');
            first = false;
          }
        }
        sb.append(']');
      }
      last = blocks;
      sb.append(",\"block_ticks\":");
      ticks(sb, level, level.getBlockTicks(), b -> BuiltInRegistries.BLOCK.getKey(b).toString());
      sb.append(",\"fluid_ticks\":");
      ticks(sb, level, level.getFluidTicks(), f -> BuiltInRegistries.FLUID.getKey(f).toString());
      sb.append(",\"entities\":");
      entities(sb, level);
      if (light) {
        light(sb, level);
      }
      if (names.hasEntities() || levelRng) {
        sb.append(",\"level_rng\":").append(ScenarioEntities.state(level.getRandom()));
      }
      frames.add(sb.append('}').toString());
    }

    /**
     * The settled light of the region as run-length pairs of {@code sky << 4 | block} in y, z, x
     * order: the light engine first runs until no task around the region is left.
     */
    private void light(StringBuilder sb, ServerLevel level) throws InterruptedException {
      ThreadedLevelLightEngine engine = level.getChunkSource().getLightEngine();
      while (true) {
        List<CompletableFuture<?>> pending = new ArrayList<>();
        for (int cx = (min.getX() >> 4) - 1; cx <= (max.getX() >> 4) + 1; cx++) {
          for (int cz = (min.getZ() >> 4) - 1; cz <= (max.getZ() >> 4) + 1; cz++) {
            pending.add(engine.waitForPendingTasks(cx, cz));
          }
        }
        while (!pending.stream().allMatch(CompletableFuture::isDone)) {
          engine.tryScheduleUpdate();
          Thread.sleep(1);
        }
        if (!engine.hasLightWork()) {
          break;
        }
      }
      sb.append(",\"light\":[");
      BlockPos.MutableBlockPos p = new BlockPos.MutableBlockPos();
      int previous = -1;
      int run = 0;
      boolean first = true;
      for (int y = min.getY(); y <= max.getY(); y++) {
        for (int z = min.getZ(); z <= max.getZ(); z++) {
          for (int x = min.getX(); x <= max.getX(); x++) {
            p.set(x, y, z);
            int level16 =
                level.getBrightness(LightLayer.SKY, p) << 4
                    | level.getBrightness(LightLayer.BLOCK, p);
            if (level16 != previous && run > 0) {
              sb.append(first ? "" : ",").append(run).append(',').append(previous);
              first = false;
              run = 0;
            }
            previous = level16;
            run++;
          }
        }
      }
      if (run > 0) {
        sb.append(first ? "" : ",").append(run).append(',').append(previous);
      }
      sb.append(']');
    }

    private boolean inside(BlockPos p) {
      return p.getX() >= min.getX()
          && p.getY() >= min.getY()
          && p.getZ() >= min.getZ()
          && p.getX() <= max.getX()
          && p.getY() <= max.getY()
          && p.getZ() <= max.getZ();
    }

    /** Pending ticks in the region in drain order: [x, y, z, type, delay, priority]. */
    @SuppressWarnings("unchecked")
    private <T> void ticks(
        StringBuilder sb,
        ServerLevel level,
        LevelTicks<T> ticks,
        java.util.function.Function<T, String> name)
        throws Exception {
      Field f = LevelTicks.class.getDeclaredField("allContainers");
      f.setAccessible(true);
      Map<Long, LevelChunkTicks<T>> containers = (Map<Long, LevelChunkTicks<T>>) f.get(ticks);
      List<ScheduledTick<T>> all = new ArrayList<>();
      for (int cx = min.getX() >> 4; cx <= max.getX() >> 4; cx++) {
        for (int cz = min.getZ() >> 4; cz <= max.getZ() >> 4; cz++) {
          LevelChunkTicks<T> c = containers.get(ChunkPos.pack(cx, cz));
          if (c != null) {
            c.getAll().filter(t -> inside(t.pos())).forEach(all::add);
          }
        }
      }
      all.sort(ScheduledTick.DRAIN_ORDER);
      long now = level.getGameTime();
      sb.append('[');
      for (int i = 0; i < all.size(); i++) {
        ScheduledTick<T> t = all.get(i);
        sb.append(i == 0 ? "" : ",")
            .append('[')
            .append(t.pos().getX())
            .append(',')
            .append(t.pos().getY())
            .append(',')
            .append(t.pos().getZ())
            .append(",\"")
            .append(name.apply(t.type()))
            .append("\",")
            .append(t.triggerTick() - now)
            .append(',')
            .append(t.priority().getValue())
            .append(']');
      }
      sb.append(']');
    }

    /** Entities in the region: [type, x, y, z, dx, dy, dz] (doubles as bit patterns). */
    private void entities(StringBuilder sb, ServerLevel level) {
      List<Entity> list = new ArrayList<>();
      level.getEntities(
          EntityTypeTest.forClass(Entity.class), e -> inside(e.blockPosition()), list);
      List<String> rows = new ArrayList<>();
      for (Entity e : list) {
        // A player moves by its client: its server-side motion is not part of the trace.
        Vec3 v = e instanceof Player ? Vec3.ZERO : e.getDeltaMovement();
        rows.add(
            "[\""
                + BuiltInRegistries.ENTITY_TYPE.getKey(e.getType())
                + "\","
                + Double.doubleToRawLongBits(e.getX())
                + ','
                + Double.doubleToRawLongBits(e.getY())
                + ','
                + Double.doubleToRawLongBits(e.getZ())
                + ','
                + Double.doubleToRawLongBits(v.x)
                + ','
                + Double.doubleToRawLongBits(v.y)
                + ','
                + Double.doubleToRawLongBits(v.z)
                + names.detail(e)
                + ']');
      }
      rows.sort(Comparator.naturalOrder());
      sb.append('[').append(String.join(",", rows)).append(']');
    }

    String finish() {
      StringBuilder sb = new StringBuilder("{\"format\":").append(FORMAT);
      sb.append(",\n\"region\":[[")
          .append(min.getX())
          .append(',')
          .append(min.getY())
          .append(',')
          .append(min.getZ())
          .append("],[")
          .append(max.getX())
          .append(',')
          .append(max.getY())
          .append(',')
          .append(max.getZ())
          .append("]],\n\"palette\":[");
      for (int i = 0; i < paletteOrder.size(); i++) {
        sb.append(i == 0 ? "\n" : ",\n").append('"').append(paletteOrder.get(i)).append('"');
      }
      sb.append("],\n\"frames\":[");
      for (int i = 0; i < frames.size(); i++) {
        sb.append(i == 0 ? "\n" : ",\n").append(frames.get(i));
      }
      return sb.append("]}\n").toString();
    }
  }
}

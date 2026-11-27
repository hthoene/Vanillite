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
import net.minecraft.tags.BlockTags;
import net.minecraft.world.DifficultyInstance;
import net.minecraft.world.entity.Entity;
import net.minecraft.world.entity.EntitySpawnReason;
import net.minecraft.world.entity.EntityTypes;
import net.minecraft.world.entity.LightningBolt;
import net.minecraft.world.entity.animal.equine.SkeletonHorse;
import net.minecraft.world.entity.player.Player;
import net.minecraft.world.item.BoneMealItem;
import net.minecraft.world.item.ItemStack;
import net.minecraft.world.item.Items;
import net.minecraft.world.level.ChunkPos;
import net.minecraft.world.level.LightLayer;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.block.state.properties.Property;
import net.minecraft.world.level.entity.EntityTypeTest;
import net.minecraft.world.level.entity.PersistentEntitySectionManager;
import net.minecraft.world.level.gamerules.GameRules;
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
 * <p>The action {@code command} runs a command line as the server at the origin of the
 * overworld.
 *
 * <p>The actions {@code force} and {@code unforce} add or remove forced tickets of chunks and then
 * run the chunk system and the entity manager without a game tick until every chunk around the
 * scenario is loaded or unloaded, with its entities, as its ticket level says. The action {@code
 * restart} stops the server, which saves the world, and starts a new one on the saved world: its
 * saved chunk tickets come back and their chunks load before the next tick ({@code
 * prepareLevels}); the rules, time and difficulty are set again and the level random starts from
 * the seed. The action {@code bonemeal} uses bone meal on a block ({@code
 * BoneMealItem.growCrop}).
 *
 * <p>Scenarios with {@code "dimensions": true} run with all three dimensions (the Nether and the
 * End flat too) and players that load chunks; their region lies in {@code dimension}, the
 * entities of their {@code views} (regions of other dimensions) join the frames with their
 * {@code dim}, and block operations work in their {@code dimension}.
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

  /** View and simulation distance of scenarios across dimensions. */
  private static final int VIEW_DISTANCE = 2;

  private static final String OVERWORLD = "minecraft:overworld";

  private ScenarioRecorder() {}

  /** An extra region of a scenario across dimensions whose entities the trace holds. */
  private record View(String dimension, BlockPos min, BlockPos max) {}

  /** The level a block operation works in: its {@code dimension}, default the overworld. */
  private static ServerLevel levelOf(ParityServer server, JsonObject op) {
    return server.level(op.has("dimension") ? op.get("dimension").getAsString() : OVERWORLD);
  }

  /**
   * The level random (and with {@code randValue} the LCG of random tick positions) of the
   * region's level, or of every level across dimensions, starts from the world seed.
   */
  private static void seedLevels(
      ParityServer server, ServerLevel level, long seed, boolean dimensions, boolean randValue)
      throws Exception {
    Field field = net.minecraft.world.level.Level.class.getDeclaredField("randValue");
    field.setAccessible(true);
    for (ServerLevel l : dimensions ? server.getAllLevels() : List.of(level)) {
      l.getRandom().setSeed(seed);
      if (randValue) {
        field.setInt(l, (int) seed);
      }
    }
  }

  public static void main(String[] args) throws Exception {
    if (args.length == 1 && args[0].equals("--serve")) {
      serve();
      return;
    }
    if (args.length == 0 || args.length % 2 != 0) {
      System.err.println("usage: ScenarioRecorder (<scenario.json> <trace.json>)... | --serve");
      System.exit(2);
    }
    SharedConstants.tryDetectVersion();
    Bootstrap.bootStrap();
    for (int i = 0; i < args.length; i += 2) {
      Path scenario = Path.of(args[i]);
      Path out = Path.of(args[i + 1]);
      long start = System.nanoTime();
      String trace =
          record(
              JsonParser.parseString(Files.readString(scenario)).getAsJsonObject(),
              scenario.toAbsolutePath().getParent());
      Files.createDirectories(out.toAbsolutePath().getParent());
      Files.writeString(out, trace, StandardCharsets.UTF_8);
      System.out.printf(
          "scenario: %s -> %s (%d ms)%n", scenario, out, (System.nanoTime() - start) / 1_000_000);
    }
    // The server leaves worker threads behind; the recording is complete.
    System.exit(0);
  }

  /**
   * {@code --serve} (random scenarios, {@code cargo xtask parity fuzz}): one JVM records many
   * scenarios. Each input line is {@code <scenario.json>\t<trace.json>}; a failure goes to
   * {@code <trace.json>.error}; every scenario is answered with {@code @@done <trace.json>}.
   */
  private static void serve() throws Exception {
    // The bootstrap sends System.out to the log; the replies go to the real output.
    java.io.PrintStream reply = System.out;
    SharedConstants.tryDetectVersion();
    Bootstrap.bootStrap();
    java.io.BufferedReader in =
        new java.io.BufferedReader(new java.io.InputStreamReader(System.in, StandardCharsets.UTF_8));
    String line;
    while ((line = in.readLine()) != null) {
      String[] parts = line.split("\t");
      if (parts.length != 2) {
        continue;
      }
      Path out = Path.of(parts[1]);
      try {
        String trace =
            record(JsonParser.parseString(Files.readString(Path.of(parts[0]))).getAsJsonObject());
        Files.writeString(out, trace, StandardCharsets.UTF_8);
      } catch (Exception | AssertionError e) {
        Files.writeString(Path.of(out + ".error"), String.valueOf(e), StandardCharsets.UTF_8);
      }
      reply.println("@@done " + out);
      reply.flush();
    }
    System.exit(0);
  }

  static String record(JsonObject scenario) throws Exception {
    return record(scenario, null);
  }

  /**
   * Records a scenario; its {@code datapack} is {@code ../datapacks/<name>} of {@code
   * scenarioDir}.
   */
  static String record(JsonObject scenario, Path scenarioDir) throws Exception {
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
    // Across dimensions: the Nether and the End are flat too, and players load chunks.
    boolean dimensions = scenario.has("dimensions") && scenario.get("dimensions").getAsBoolean();
    Path datapack = null;
    if (scenario.has("datapack")) {
      if (!dimensions || scenarioDir == null) {
        throw new IllegalArgumentException("a data pack needs \"dimensions\"");
      }
      datapack =
          scenarioDir.getParent().resolve("datapacks").resolve(scenario.get("datapack").getAsString());
    }
    ParityServer server = ParityServer.start(dir, seed, layers, biome, dimensions, datapack);
    ScenarioEntities entities = null;
    try {
      rules(server, scenario);
      if (dimensions) {
        server.getPlayerList().setViewDistance(VIEW_DISTANCE);
        server.getPlayerList().setSimulationDistance(VIEW_DISTANCE);
      }

      String dimension =
          scenario.has("dimension") ? scenario.get("dimension").getAsString() : OVERWORLD;
      if (!dimensions && !dimension.equals(OVERWORLD)) {
        throw new IllegalArgumentException("a region in another dimension needs \"dimensions\"");
      }
      ServerLevel level = server.level(dimension);
      JsonArray region = scenario.getAsJsonArray("region");
      BlockPos min = pos(region.get(0));
      BlockPos max = pos(region.get(1));
      loadRegion(server, level, min, max);
      List<View> views = new ArrayList<>();
      if (scenario.has("views")) {
        if (!dimensions) {
          throw new IllegalArgumentException("views need \"dimensions\"");
        }
        for (JsonElement v : scenario.getAsJsonArray("views")) {
          JsonObject view = v.getAsJsonObject();
          JsonArray r = view.getAsJsonArray("region");
          View w = new View(view.get("dimension").getAsString(), pos(r.get(0)), pos(r.get(1)));
          loadRegion(server, server.level(w.dimension), w.min, w.max);
          views.add(w);
        }
      }
      // The warm-up takes as many ticks as the chunks need to load: the scenario starts at game
      // time 0 instead, like Vanillite's new level (camels count their pose time from it).
      server.getWorldData().overworldData().setGameTime(0);
      Area area = new Area(min, max);

      // The setup may draw from the level random (blocks that schedule a random delay when
      // placed): it starts from the world seed, like Vanillite's new level.
      seedLevels(server, level, seed, dimensions, false);
      if (scenario.has("setup")) {
        for (JsonElement op : scenario.getAsJsonArray("setup")) {
          apply(levelOf(server, op.getAsJsonObject()), op.getAsJsonObject(), false);
        }
      }
      // Light is settled whenever a tick starts (see `ParityServer.settleLight`): after the
      // setup, the entities, the actions of a tick and the tick. Not across dimensions: chunks
      // generate there meanwhile, and waiting for their light would let more of them load.
      boolean settleLight = !dimensions;
      if (settleLight) {
        server.settleLight();
      }

      timeAndDifficulty(server, level, scenario);
      entities = new ScenarioEntities(server, server.overworld());
      entities.loadingPlayers = dimensions;
      entities.playerState =
          scenario.has("player_state") && scenario.get("player_state").getAsBoolean();
      entities.sleepState =
          scenario.has("sleep_state") && scenario.get("sleep_state").getAsBoolean();
      JsonArray specs = scenario.has("entities") ? scenario.getAsJsonArray("entities") : new JsonArray();
      // Players join first; then the level random starts from the world seed and the mobs come.
      for (int i = 0; i < specs.size(); i++) {
        JsonObject spec = specs.get(i).getAsJsonObject();
        if (spec.get("type").getAsString().equals("minecraft:player")) {
          entities.player(spec, i);
        }
      }
      // The LCG of random tick positions starts from the seed too (Vanilla seeds it from a
      // thread-local random).
      seedLevels(server, level, seed, dimensions, true);
      for (int i = 0; i < specs.size(); i++) {
        JsonObject spec = specs.get(i).getAsJsonObject();
        if (!spec.get("type").getAsString().equals("minecraft:player")) {
          entities.mob(spec, i);
        }
      }
      if (settleLight) {
        server.settleLight();
      }
      ParityHooks.startStream(scenario.has("entity_seed") ? scenario.get("entity_seed").getAsLong() : seed);

      boolean light = scenario.has("light") && scenario.get("light").getAsBoolean();
      boolean levelRng = scenario.has("level_rng") && scenario.get("level_rng").getAsBoolean();
      Trace trace = new Trace(min, max, entities, light, levelRng, dimension, views);
      trace.frame(level, 0, true);
      int ticks = scenario.get("ticks").getAsInt();
      JsonArray actions = scenario.has("actions") ? scenario.getAsJsonArray("actions") : null;
      for (int tick = 1; tick <= ticks; tick++) {
        entities.clientBegin();
        if (actions != null) {
          boolean acted = false;
          for (JsonElement a : actions) {
            JsonObject action = a.getAsJsonObject();
            if (action.get("tick").getAsInt() == tick) {
              acted = true;
              String op = action.get("op").getAsString();
              if (op.equals("force") || op.equals("unforce")) {
                for (JsonElement c : action.getAsJsonArray("chunks")) {
                  JsonArray xz = c.getAsJsonArray();
                  area.add(xz.get(0).getAsInt(), xz.get(1).getAsInt());
                  level.setChunkForced(xz.get(0).getAsInt(), xz.get(1).getAsInt(), op.equals("force"));
                }
                settle(server, level, area);
              } else if (op.equals("restart")) {
                if (dimensions) {
                  throw new IllegalStateException("scenarios across dimensions cannot restart");
                }
                if (!specs.isEmpty()) {
                  throw new IllegalStateException("scenarios with entities cannot restart");
                }
                // `stopServer` saves every level with its chunks and chunk tickets; the new
                // server loads them, and `prepareLevels` loads the chunks of the saved tickets.
                server.shutdown();
                server = ParityServer.start(dir, seed, layers, biome);
                rules(server, scenario);
                level = server.level();
                timeAndDifficulty(server, level, scenario);
                level.getRandom().setSeed(seed);
              } else if (op.equals("bonemeal")) {
                // Bone meal used on the block (`BoneMealItem.growCrop`).
                BoneMealItem.growCrop(new ItemStack(Items.BONE_MEAL), level, pos(action.get("pos")));
              } else if (op.equals("hurt")) {
                entities.hurt(action);
              } else if (op.equals("use_item")
                  || op.equals("interact")
                  || op.equals("release_item")
                  || op.equals("attack_entity")) {
                entities.useItem(action, op);
              } else if (op.equals("lightning")) {
                strike(level, pos(action.get("pos")));
              } else if (op.equals("effect")) {
                entities.effect(action);
              } else if (op.equals("command")) {
                // As the server, at the origin of the overworld.
                server
                    .getCommands()
                    .performPrefixedCommand(
                        server.createCommandSourceStack().withPosition(Vec3.ZERO),
                        action.get("command").getAsString());
              } else if (op.equals("throw_potion")) {
                entities.throwPotion(action);
              } else if (op.equals("use") || op.equals("attack")) {
                entities.use(action, op.equals("attack"));
              } else if (op.equals("packet")) {
                entities.packet(action);
              } else {
                apply(levelOf(server, action), action, true);
              }
            }
          }
          if (acted && settleLight) {
            server.settleLight();
          }
        }
        entities.clientEnd();
        server.step();
        if (settleLight) {
          server.settleLight();
        }
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

  /** The default game rules and the scenario's on top. */
  private static void rules(ParityServer server, JsonObject scenario) {
    Map<String, String> rules = new java.util.LinkedHashMap<>();
    for (String[] rule : DEFAULT_RULES) {
      rules.put(rule[0], rule[1]);
    }
    if (scenario.has("rules")) {
      for (Map.Entry<String, JsonElement> e : scenario.getAsJsonObject("rules").entrySet()) {
        rules.put(e.getKey(), e.getValue().getAsString());
      }
    }
    for (Map.Entry<String, String> rule : rules.entrySet()) {
      server.command("gamerule " + rule.getKey() + " " + rule.getValue());
    }
  }

  /** The scenario's time of day and difficulty. */
  private static void timeAndDifficulty(
      ParityServer server, ServerLevel level, JsonObject scenario) {
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

  /**
   * The {@code lightning} action: the strike of {@code ServerLevel.tickThunder} in the column of
   * {@code column}, without its chance roll and the random column.
   */
  private static void strike(ServerLevel level, BlockPos column) throws Exception {
    Method find = ServerLevel.class.getDeclaredMethod("findLightningTargetAround", BlockPos.class);
    find.setAccessible(true);
    BlockPos pos = (BlockPos) find.invoke(level, column);
    if (!level.isRainingAt(pos)) {
      return;
    }
    DifficultyInstance difficulty = level.getCurrentDifficultyAt(pos);
    boolean isTrap =
        level.getGameRules().get(GameRules.SPAWN_MOBS)
            && level.getRandom().nextDouble() < difficulty.getEffectiveDifficulty() * 0.01
            && !level.getBlockState(pos.below()).is(BlockTags.LIGHTNING_RODS);
    if (isTrap) {
      SkeletonHorse horse = EntityTypes.SKELETON_HORSE.create(level, EntitySpawnReason.EVENT);
      if (horse != null) {
        horse.setTrap(true);
        horse.setAge(0);
        horse.setPos(pos.getX(), pos.getY(), pos.getZ());
        level.addFreshEntity(horse);
      }
    }
    LightningBolt bolt = EntityTypes.LIGHTNING_BOLT.create(level, EntitySpawnReason.EVENT);
    if (bolt != null) {
      bolt.snapTo(Vec3.atBottomCenterOf(pos));
      bolt.setVisualOnly(isTrap);
      level.addFreshEntity(bolt);
    }
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
    /** The dimension of the region. */
    private final String dimension;
    private final List<View> views;

    Trace(
        BlockPos min,
        BlockPos max,
        ScenarioEntities names,
        boolean light,
        boolean levelRng,
        String dimension,
        List<View> views) {
      this.min = min;
      this.max = max;
      this.names = names;
      this.light = light;
      this.levelRng = levelRng;
      this.dimension = dimension;
      this.views = views;
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
      return inside(p, min, max);
    }

    private static boolean inside(BlockPos p, BlockPos min, BlockPos max) {
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

    /**
     * Entities in the region and the views: [type, x, y, z, dx, dy, dz] (doubles as bit
     * patterns); those outside the overworld with their {@code dim}.
     */
    private void entities(StringBuilder sb, ServerLevel level) {
      List<String> rows = new ArrayList<>();
      entities(rows, level, dimension, min, max);
      for (View view : views) {
        entities(rows, level.getServer().getLevel(
            net.minecraft.resources.ResourceKey.create(
                net.minecraft.core.registries.Registries.DIMENSION,
                net.minecraft.resources.Identifier.parse(view.dimension))),
            view.dimension, view.min, view.max);
      }
      rows.sort(Comparator.naturalOrder());
      sb.append('[').append(String.join(",", rows)).append(']');
    }

    private void entities(
        List<String> rows, ServerLevel level, String dim, BlockPos min, BlockPos max) {
      List<Entity> list = new ArrayList<>();
      level.getEntities(
          EntityTypeTest.forClass(Entity.class), e -> inside(e.blockPosition(), min, max), list);
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
                + withDimension(names.detail(e), dim)
                + ']');
      }
    }

    /** The detail {@code ,{...}} with {@code "dim"} outside the overworld. */
    private static String withDimension(String detail, String dim) {
      if (dim.equals(OVERWORLD)) {
        return detail;
      }
      String d = "\"dim\":\"" + dim + '"';
      return detail.isEmpty() ? ",{" + d + "}" : ",{" + d + ',' + detail.substring(2);
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

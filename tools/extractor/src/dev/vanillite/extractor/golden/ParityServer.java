// Copyright Hannes Thöne. Licensed under the Apache License, Version 2.0.
package dev.vanillite.extractor.golden;

import java.io.IOException;
import java.net.Proxy;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;
import java.util.Optional;
import java.util.concurrent.Executor;
import net.minecraft.SystemReport;
import net.minecraft.commands.Commands;
import net.minecraft.core.Holder;
import net.minecraft.core.HolderGetter;
import net.minecraft.core.HolderLookup;
import net.minecraft.core.HolderSet;
import net.minecraft.core.MappedRegistry;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.core.registries.Registries;
import net.minecraft.resources.Identifier;
import net.minecraft.resources.ResourceKey;
import net.minecraft.server.MinecraftServer;
import net.minecraft.server.Services;
import net.minecraft.server.TickTask;
import net.minecraft.server.WorldLoader;
import net.minecraft.server.WorldStem;
import net.minecraft.server.level.ServerLevel;
import net.minecraft.server.level.progress.LoggingLevelLoadListener;
import net.minecraft.server.notifications.EmptyNotificationService;
import net.minecraft.server.notifications.NotificationManager;
import net.minecraft.server.packs.repository.PackRepository;
import net.minecraft.server.packs.repository.ServerPacksSource;
import net.minecraft.server.permissions.LevelBasedPermissionSet;
import net.minecraft.server.permissions.PermissionSet;
import net.minecraft.server.players.NameAndId;
import net.minecraft.server.players.PlayerList;
import net.minecraft.util.Util;
import net.minecraft.util.datafix.DataFixers;
import net.minecraft.util.debugchart.LocalSampleLogger;
import net.minecraft.util.debugchart.SampleLogger;
import net.minecraft.world.level.GameType;
import net.minecraft.world.level.LevelSettings;
import net.minecraft.world.level.WorldDataConfiguration;
import net.minecraft.world.level.biome.Biome;
import net.minecraft.world.level.dimension.LevelStem;
import net.minecraft.world.level.gamerules.GameRules;
import net.minecraft.world.level.levelgen.FlatLevelSource;
import net.minecraft.world.level.levelgen.WorldDimensions;
import net.minecraft.world.level.levelgen.WorldGenSettings;
import net.minecraft.world.level.levelgen.WorldOptions;
import net.minecraft.world.level.levelgen.flat.FlatLayerInfo;
import net.minecraft.world.level.levelgen.flat.FlatLevelGeneratorSettings;
import net.minecraft.world.level.levelgen.presets.WorldPresets;
import net.minecraft.world.level.storage.LevelDataAndDimensions;
import net.minecraft.world.level.storage.LevelStorageSource;
import net.minecraft.world.level.storage.PrimaryLevelData;

/**
 * A Vanilla server in this process for parity recordings: no network, no server thread, no tick
 * loop. The caller steps ticks with {@link #step()}.
 *
 * <p>Unlike {@code GameTestServer.create}, which turns on experimental features, the world uses
 * Vanilla's default data configuration (the {@code vanilla} pack and its features only). The
 * overworld is flat with the given layers and biome and without structures, features or lakes.
 */
public final class ParityServer extends MinecraftServer {
  /** A flat layer: block id and thickness. */
  public record Layer(String block, int height) {}

  private final LocalSampleLogger sampleLogger = new LocalSampleLogger(1);

  /** Runs every queued task at once, also those Vanilla holds back for a few ticks. */
  private boolean runEveryTask;

  private ParityServer(
      Thread thread,
      LevelStorageSource.LevelStorageAccess access,
      PackRepository packs,
      WorldStem stem) {
    super(
        thread,
        access,
        packs,
        stem,
        Optional.of(new GameRules(WorldDataConfiguration.DEFAULT.enabledFeatures())),
        Proxy.NO_PROXY,
        DataFixers.getDataFixer(),
        new Services(null, null, null, null, null),
        LoggingLevelLoadListener.forDedicatedServer(),
        false,
        new NotificationManager());
  }

  /**
   * Loads the Vanilla data pack and the world (seed, flat layers bottom up, biome) into a fresh
   * directory under {@code dir}, on the calling thread.
   */
  public static ParityServer start(Path dir, long seed, List<Layer> layers, String biome)
      throws Exception {
    Files.createDirectories(dir);
    LevelStorageSource.LevelStorageAccess access =
        LevelStorageSource.createDefault(dir).validateAndCreateAccess("world");
    PackRepository packs = ServerPacksSource.createPackRepository(access);
    packs.reload();
    WorldStem stem = loadStem(packs, seed, layers, biome);
    ParityServer server = new ParityServer(Thread.currentThread(), access, packs, stem);
    if (!server.initServer()) {
      throw new IllegalStateException("parity server failed to start");
    }
    return server;
  }

  /** The Vanilla data pack and world settings, loaded like {@code WorldLoader} does at start. */
  public static WorldStem loadStem(
      PackRepository packs, long seed, List<Layer> layers, String biome) throws Exception {
    WorldDataConfiguration config = WorldDataConfiguration.DEFAULT;
    LevelSettings settings =
        new LevelSettings(
            "Parity", GameType.CREATIVE, LevelSettings.DifficultySettings.DEFAULT, false, config);
    WorldLoader.InitConfig init =
        new WorldLoader.InitConfig(
            new WorldLoader.PackConfig(packs, config, false, true),
            Commands.CommandSelection.DEDICATED,
            LevelBasedPermissionSet.OWNER);
    WorldOptions options = new WorldOptions(seed, false, false);
    return Util.<WorldStem>blockUntilDone(
            (Executor executor) ->
                WorldLoader.load(
                    init,
                    ctx -> {
                      HolderLookup.Provider lookup = ctx.datapackWorldRegistries();
                      WorldDimensions dimensions =
                          WorldPresets.createNormalWorldDimensions(lookup)
                              .replaceOverworldGenerator(
                                  lookup, new FlatLevelSource(flat(lookup, layers, biome)));
                      WorldDimensions.Complete complete =
                          dimensions.bake(
                              new MappedRegistry<LevelStem>(
                                      Registries.LEVEL_STEM,
                                      com.mojang.serialization.Lifecycle.stable())
                                  .freeze());
                      PrimaryLevelData data =
                          new PrimaryLevelData(
                              settings, complete.specialWorldProperty(), complete.lifecycle());
                      return new WorldLoader.DataLoadOutput<>(
                          new LevelDataAndDimensions.WorldDataAndGenSettings(
                              data, new WorldGenSettings(options, dimensions)),
                          complete.dimensionsRegistryAccess());
                    },
                    WorldStem::new,
                    Util.backgroundExecutor(),
                    executor))
        .get();
  }

  private static FlatLevelGeneratorSettings flat(
      HolderLookup.Provider lookup, List<Layer> layers, String biome) {
    HolderGetter<Biome> biomes = lookup.lookupOrThrow(Registries.BIOME);
    Holder<Biome> holder =
        biomes.getOrThrow(ResourceKey.create(Registries.BIOME, Identifier.parse(biome)));
    List<FlatLayerInfo> infos = new ArrayList<>();
    for (Layer layer : layers) {
      infos.add(
          new FlatLayerInfo(
              layer.height(), BuiltInRegistries.BLOCK.getValue(Identifier.parse(layer.block()))));
    }
    return FlatLevelGeneratorSettings.getDefault(
            biomes,
            lookup.lookupOrThrow(Registries.STRUCTURE_SET),
            lookup.lookupOrThrow(Registries.PLACED_FEATURE))
        .withBiomeAndLayers(infos, Optional.of(HolderSet.direct()), holder);
  }

  /**
   * One full server tick, then the tasks the tick loop runs while it waits for the next one
   * (server tasks and the main thread tasks of the chunk system), as if there was time left.
   */
  public void step() {
    tickServer(() -> true);
    drainTasks();
  }

  /** Runs pending server and chunk tasks until none is left. */
  public void drainTasks() {
    boolean ran = true;
    while (ran) {
      runAllTasks();
      ran = false;
      for (ServerLevel level : getAllLevels()) {
        while (level.getChunkSource().pollTask()) {
          ran = true;
        }
      }
    }
  }

  /**
   * Like {@link #drainTasks()}, but also runs the tasks Vanilla holds back until a few ticks
   * passed (a running server runs them while it waits for the next tick); for running the chunk
   * system and entity loading without a game tick.
   */
  public void drainEveryTask() {
    runEveryTask = true;
    try {
      drainTasks();
    } finally {
      runEveryTask = false;
    }
  }

  @Override
  protected boolean shouldRun(TickTask task) {
    return runEveryTask || super.shouldRun(task);
  }

  /** Stops the server: saves and unlocks the world directory. */
  public void shutdown() {
    stopServer();
  }

  /** Runs a command as the server, e.g. {@code gamerule random_tick_speed 0}. */
  public void command(String command) {
    getCommands().performPrefixedCommand(createCommandSourceStack(), command);
  }

  public ServerLevel level() {
    return overworld();
  }

  @Override
  protected boolean initServer() throws IOException {
    setPlayerList(
        new PlayerList(this, registries(), playerDataStorage, new EmptyNotificationService()) {});
    loadLevel();
    return true;
  }

  @Override
  protected SampleLogger getTickTimeLogger() {
    return sampleLogger;
  }

  @Override
  public boolean isTickTimeLoggingEnabled() {
    return false;
  }

  @Override
  public SystemReport fillServerSystemReport(SystemReport report) {
    return report;
  }

  @Override
  public LevelBasedPermissionSet operatorUserPermissions() {
    return LevelBasedPermissionSet.OWNER;
  }

  @Override
  public PermissionSet getFunctionCompilationPermissions() {
    return LevelBasedPermissionSet.OWNER;
  }

  @Override
  public boolean shouldRconBroadcast() {
    return false;
  }

  @Override
  public boolean isDedicatedServer() {
    return false;
  }

  @Override
  public int getRateLimitPacketsPerSecond() {
    return 0;
  }

  @Override
  public int getCommandSpamThresholdSeconds() {
    return 0;
  }

  @Override
  public int getChatSpamThresholdSeconds() {
    return 0;
  }

  @Override
  public boolean useNativeTransport() {
    return false;
  }

  @Override
  public boolean isPublished() {
    return false;
  }

  @Override
  public boolean shouldInformAdmins() {
    return false;
  }

  @Override
  public int getMaxPlayers() {
    return 0;
  }

  @Override
  public boolean isSingleplayerOwner(NameAndId player) {
    return false;
  }
}

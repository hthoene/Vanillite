// Copyright Hannes Thöne. Licensed under the Apache License, Version 2.0.
package dev.vanillite.extractor.golden;

import com.google.gson.JsonArray;
import com.google.gson.JsonObject;
import com.mojang.authlib.GameProfile;
import io.netty.channel.embedded.EmbeddedChannel;
import java.lang.reflect.Field;
import java.util.ArrayList;
import java.util.IdentityHashMap;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.UUID;
import java.util.concurrent.atomic.AtomicLong;
import net.minecraft.core.BlockPos;
import net.minecraft.core.SectionPos;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.network.Connection;
import net.minecraft.network.protocol.PacketFlow;
import net.minecraft.resources.Identifier;
import net.minecraft.server.level.ServerLevel;
import net.minecraft.server.level.ServerPlayer;
import net.minecraft.server.network.CommonListenerCookie;
import net.minecraft.util.RandomSource;
import net.minecraft.world.damagesource.DamageSource;
import net.minecraft.world.entity.Entity;
import net.minecraft.world.entity.EntitySpawnReason;
import net.minecraft.world.entity.EntityType;
import net.minecraft.world.entity.LivingEntity;
import net.minecraft.world.entity.Mob;
import net.minecraft.world.entity.ai.goal.GoalSelector;
import net.minecraft.world.entity.ai.goal.WrappedGoal;
import net.minecraft.world.entity.item.ItemEntity;
import net.minecraft.world.entity.player.Player;
import net.minecraft.world.level.GameType;
import net.minecraft.world.level.levelgen.LegacyRandomSource;
import net.minecraft.world.level.pathfinder.Node;
import net.minecraft.world.level.pathfinder.Path;

/**
 * The entities of a scenario: spawned with fixed UUIDs and seeds, named for the trace, and the
 * per-entity detail the trace records (format in {@code crates/vanillite-test/src/trace.rs}).
 */
final class ScenarioEntities {
  private final ParityServer server;
  private final ServerLevel level;
  private final Map<Entity, String> names = new IdentityHashMap<>();
  private final Map<String, Entity> byName = new LinkedHashMap<>();
  private final List<ServerPlayer> players = new ArrayList<>();

  ScenarioEntities(ParityServer server, ServerLevel level) {
    this.server = server;
    this.level = level;
    // Entity ids count from 1 in every scenario, like a fresh Vanillite world: mobs choose
    // goals on ticks of the parity of tickCount + id.
    try {
      Field f = ServerLevel.class.getDeclaredField("ENTITY_COUNTER");
      f.setAccessible(true);
      ((java.util.concurrent.atomic.AtomicInteger) f.get(null)).set(0);
    } catch (ReflectiveOperationException ex) {
      throw new IllegalStateException(ex);
    }
  }

  /** Whether the scenario has entities (then the frames record the level random too). */
  boolean hasEntities() {
    return !byName.isEmpty();
  }

  /** The fixed UUID of the scenario entity with index {@code index}. */
  static UUID uuid(int index, long seed) {
    return new UUID(0x5641_4E49_0000_0000L | (index + 1), seed);
  }

  private static double[] vec(JsonArray a) {
    return new double[] {a.get(0).getAsDouble(), a.get(1).getAsDouble(), a.get(2).getAsDouble()};
  }

  private static float rot(JsonObject spec, int i) {
    return spec.has("rot") ? spec.getAsJsonArray("rot").get(i).getAsFloat() : 0.0F;
  }

  private void name(JsonObject spec, Entity e) {
    String name = spec.get("name").getAsString();
    names.put(e, name);
    byName.put(name, e);
  }

  /** A survival player on a connection nobody reads (like GameTest's mock server player). */
  void player(JsonObject spec, int index) {
    long seed = spec.has("seed") ? spec.get("seed").getAsLong() : 0;
    String name = spec.get("name").getAsString();
    CommonListenerCookie cookie =
        CommonListenerCookie.createInitial(new GameProfile(uuid(index, seed), name), false);
    ParityHooks.seedNext(seed);
    ServerPlayer player =
        new ServerPlayer(server, level, cookie.gameProfile(), cookie.clientInformation());
    Connection connection = new Connection(PacketFlow.SERVERBOUND);
    new EmbeddedChannel(connection);
    server.getPlayerList().placeNewPlayer(connection, player, cookie);
    player.setGameMode(GameType.SURVIVAL);
    double[] p = vec(spec.getAsJsonArray("pos"));
    player.snapTo(p[0], p[1], p[2], rot(spec, 0), rot(spec, 1));
    player.setYHeadRot(rot(spec, 0));
    player.setYBodyRot(rot(spec, 0));
    // The player loads, ticks and offers for spawning no chunks: the scenario's forced chunks
    // are the whole world, as in Vanillite's scenario world.
    level.getChunkSource().move(player);
    level.getChunkSource().chunkMap.getDistanceManager().removePlayer(SectionPos.of(player), player);
    name(spec, player);
    players.add(player);
  }

  /**
   * A mob like {@code /summon} makes it: created with its seed, placed, then {@code finalizeSpawn}
   * (unless {@code "finalize": false}), then made a baby if asked.
   */
  void mob(JsonObject spec, int index) {
    long seed = spec.get("seed").getAsLong();
    EntityType<?> type =
        BuiltInRegistries.ENTITY_TYPE.getValue(Identifier.parse(spec.get("type").getAsString()));
    ParityHooks.seedNext(seed);
    Entity e = type.create(level, EntitySpawnReason.COMMAND);
    if (e == null) {
      throw new IllegalArgumentException("cannot create " + spec);
    }
    e.setUUID(uuid(index, seed));
    double[] p = vec(spec.getAsJsonArray("pos"));
    e.snapTo(p[0], p[1], p[2], rot(spec, 0), rot(spec, 1));
    e.setYHeadRot(rot(spec, 0));
    e.setYBodyRot(rot(spec, 0));
    if (e instanceof Mob mob) {
      if (!spec.has("finalize") || spec.get("finalize").getAsBoolean()) {
        mob.finalizeSpawn(
            level, level.getCurrentDifficultyAt(e.blockPosition()), EntitySpawnReason.COMMAND, null);
      }
      if (spec.has("baby")) {
        mob.setBaby(spec.get("baby").getAsBoolean());
      }
    }
    if (!level.addFreshEntity(e)) {
      throw new IllegalStateException("cannot add " + spec);
    }
    name(spec, e);
  }

  /** Gives the players back to the chunk system, which removes them when the server stops. */
  void release() {
    for (ServerPlayer p : players) {
      level.getChunkSource().chunkMap.getDistanceManager().addPlayer(SectionPos.of(p), p);
    }
    players.clear();
  }

  /** {@code ServerPlayer.doTick} of the players, as their connections would. */
  void tickPlayers() {
    for (ServerPlayer p : players) {
      if (!p.isRemoved()) {
        p.doTick();
      }
    }
  }

  /** The {@code hurt} action: damage to a named entity, by a named entity or generic. */
  void hurt(JsonObject action) {
    Entity target = byName.get(action.get("entity").getAsString());
    Entity by = action.has("by") ? byName.get(action.get("by").getAsString()) : null;
    DamageSource source =
        by instanceof Player p
            ? level.damageSources().playerAttack(p)
            : by instanceof LivingEntity l
                ? level.damageSources().mobAttack(l)
                : level.damageSources().generic();
    target.hurtServer(level, source, action.get("amount").getAsFloat());
  }

  private String nameOf(Entity e) {
    String n = names.get(e);
    return n != null ? n : BuiltInRegistries.ENTITY_TYPE.getKey(e.getType()).toString();
  }

  /** {@code ,{...}} with the detail of an entity, or nothing for entities without detail. */
  String detail(Entity e) {
    StringBuilder sb = new StringBuilder();
    String name = names.get(e);
    if (name != null) {
      sb.append(",\"name\":\"").append(name).append('"');
    }
    if (e instanceof ItemEntity item) {
      sb.append(",\"item\":\"")
          .append(BuiltInRegistries.ITEM.getKey(item.getItem().getItem()))
          .append("\",\"count\":")
          .append(item.getItem().getCount());
    }
    if (e instanceof LivingEntity l) {
      sb.append(",\"rot\":[")
          .append(Float.floatToRawIntBits(l.getYRot()))
          .append(',')
          .append(Float.floatToRawIntBits(l.getXRot()))
          .append(',')
          .append(Float.floatToRawIntBits(l.getYHeadRot()))
          .append(',')
          .append(Float.floatToRawIntBits(l.yBodyRot))
          .append("],\"hp\":")
          .append(Float.floatToRawIntBits(l.getHealth()));
      if (l.deathTime > 0) {
        sb.append(",\"death\":").append(l.deathTime);
      }
    }
    if (e instanceof Mob mob) {
      sb.append(",\"rng\":").append(state(mob.getRandom()));
      if (mob.getTarget() != null) {
        sb.append(",\"target\":\"").append(nameOf(mob.getTarget())).append('"');
      }
      sb.append(",\"goals\":").append(running(selector(mob, "goalSelector")));
      sb.append(",\"targets\":").append(running(selector(mob, "targetSelector")));
      Path path = mob.getNavigation().getPath();
      if (path != null && !path.isDone()) {
        BlockPos to = path.getTarget();
        sb.append(",\"path\":{\"to\":[")
            .append(to.getX())
            .append(',')
            .append(to.getY())
            .append(',')
            .append(to.getZ())
            .append("],\"next\":")
            .append(path.getNextNodeIndex())
            .append(",\"reach\":")
            .append(path.canReach())
            .append(",\"nodes\":[");
        for (int i = 0; i < path.getNodeCount(); i++) {
          Node n = path.getNode(i);
          sb.append(i == 0 ? "" : ",")
              .append('[')
              .append(n.x)
              .append(',')
              .append(n.y)
              .append(',')
              .append(n.z)
              .append(",\"")
              .append(n.type.name())
              .append("\"]");
        }
        sb.append("]}");
      }
    }
    if (sb.isEmpty()) {
      return "";
    }
    return ",{" + sb.substring(1) + "}";
  }

  /** The 48-bit state of a legacy random ({@code LegacyRandomSource.seed}). */
  static long state(RandomSource random) {
    try {
      Field f = LegacyRandomSource.class.getDeclaredField("seed");
      f.setAccessible(true);
      return ((AtomicLong) f.get(random)).get();
    } catch (ReflectiveOperationException ex) {
      throw new IllegalStateException(ex);
    }
  }

  private static GoalSelector selector(Mob mob, String field) {
    try {
      Field f = Mob.class.getDeclaredField(field);
      f.setAccessible(true);
      return (GoalSelector) f.get(mob);
    } catch (ReflectiveOperationException ex) {
      throw new IllegalStateException(ex);
    }
  }

  /** The running goals by class name (the nearest named class), sorted. */
  private static String running(GoalSelector selector) {
    List<String> out = new ArrayList<>();
    for (WrappedGoal g : selector.getAvailableGoals()) {
      if (g.isRunning()) {
        Class<?> c = g.getGoal().getClass();
        while (c.isAnonymousClass()) {
          c = c.getSuperclass();
        }
        out.add('"' + c.getSimpleName() + '"');
      }
    }
    out.sort(null);
    return "[" + String.join(",", out) + "]";
  }
}

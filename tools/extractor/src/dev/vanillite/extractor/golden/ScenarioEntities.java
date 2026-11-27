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
import net.minecraft.world.entity.vehicle.minecart.AbstractMinecart;
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
  /**
   * Names by UUID: an entity that changes dimension is a new object with the same UUID
   * ({@code Entity.teleportCrossDimension}).
   */
  private final Map<UUID, String> byUuid = new java.util.HashMap<>();
  /**
   * Players load chunks like on a normal server (scenarios across dimensions); otherwise the
   * scenario's forced chunks are the whole world.
   */
  boolean loadingPlayers;
  /** Frames hold the players' food and hands ({@code "player_state": true}). */
  boolean playerState;
  /**
   * Frames hold the players' sleeping position, sleep timer, pose id and respawn point ({@code
   * "sleep_state": true}).
   */
  boolean sleepState;

  /** The players removed from the chunk system, given back by {@link #release()}. */
  private final List<ServerPlayer> players = new ArrayList<>();
  /** Every scenario player, for their connection tick. */
  private final List<ServerPlayer> tickedPlayers = new ArrayList<>();

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
    byUuid.put(e.getUUID(), name);
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
    name(spec, player);
    if (loadingPlayers) {
      tickedPlayers.add(player);
      return;
    }
    level.getChunkSource().chunkMap.getDistanceManager().removePlayer(SectionPos.of(player), player);
    tickedPlayers.add(player);
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
    if (spec.has("motion")) {
      double[] m = vec(spec.getAsJsonArray("motion"));
      e.setDeltaMovement(m[0], m[1], m[2]);
    }
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

  /**
   * Gives the players back to the chunk system, which removes them when the server stops: at the
   * section it last saw them in (players that teleported moved on since).
   */
  void release() {
    for (ServerPlayer p : players) {
      level.getChunkSource().chunkMap.getDistanceManager().addPlayer(p.getLastSectionPos(), p);
    }
    players.clear();
  }

  /** {@code ServerPlayer.doTick} of the players, as their connections would. */
  void tickPlayers() {
    for (ServerPlayer p : tickedPlayers) {
      if (!p.isRemoved()) {
        p.doTick();
      }
    }
  }

  /**
   * The {@code use} action ({@code ServerPlayerGameMode.useItemOn} after the checks of {@code
   * handleUseItemOn}) and the {@code attack} action ({@code START_DESTROY_BLOCK}) of a named
   * player, with the stack of the action put into its hand first.
   */
  void use(JsonObject action, boolean attack) throws Exception {
    net.minecraft.server.level.ServerPlayer p =
        (net.minecraft.server.level.ServerPlayer) byName.get(action.get("entity").getAsString());
    JsonArray at = action.getAsJsonArray("pos");
    BlockPos pos = new BlockPos(at.get(0).getAsInt(), at.get(1).getAsInt(), at.get(2).getAsInt());
    net.minecraft.core.Direction face =
        action.has("face")
            ? net.minecraft.core.Direction.byName(action.get("face").getAsString())
            : net.minecraft.core.Direction.UP;
    net.minecraft.world.InteractionHand hand =
        action.has("hand") && action.get("hand").getAsString().equals("off")
            ? net.minecraft.world.InteractionHand.OFF_HAND
            : net.minecraft.world.InteractionHand.MAIN_HAND;
    if (action.has("item") && action.get("item").getAsString().equals("minecraft:air")) {
      p.setItemInHand(hand, net.minecraft.world.item.ItemStack.EMPTY);
    } else if (action.has("item")) {
      net.minecraft.nbt.CompoundTag tag = new net.minecraft.nbt.CompoundTag();
      tag.putString("id", action.get("item").getAsString());
      tag.putInt("count", action.has("count") ? action.get("count").getAsInt() : 1);
      if (action.has("components")) {
        tag.put(
            "components",
            net.minecraft.nbt.TagParser.parseCompoundFully(action.get("components").getAsString()));
      }
      net.minecraft.world.item.ItemStack stack =
          net.minecraft.world.item.ItemStack.CODEC
              .parse(
                  level.registryAccess().createSerializationContext(net.minecraft.nbt.NbtOps.INSTANCE),
                  tag)
              .getOrThrow();
      p.setItemInHand(hand, stack);
    }
    if (action.has("food")) {
      p.getFoodData().setFoodLevel(action.get("food").getAsInt());
    }
    boolean sneak = action.has("sneak") && action.get("sneak").getAsBoolean();
    p.setShiftKeyDown(sneak);
    if (attack) {
      p.gameMode.handleBlockBreakAction(
          pos,
          net.minecraft.network.protocol.game.ServerboundPlayerActionPacket.Action.START_DESTROY_BLOCK,
          face,
          level.getMaxY(),
          0);
    } else {
      double[] hit;
      if (action.has("hit")) {
        JsonArray h = action.getAsJsonArray("hit");
        hit = new double[] {h.get(0).getAsDouble(), h.get(1).getAsDouble(), h.get(2).getAsDouble()};
      } else {
        hit =
            new double[] {
              0.5 + face.getStepX() * 0.5, 0.5 + face.getStepY() * 0.5, 0.5 + face.getStepZ() * 0.5
            };
      }
      // The packet's cursor is a float relative to the block.
      net.minecraft.world.phys.Vec3 location =
          new net.minecraft.world.phys.Vec3(
              pos.getX() + (double) (float) hit[0],
              pos.getY() + (double) (float) hit[1],
              pos.getZ() + (double) (float) hit[2]);
      net.minecraft.world.phys.BlockHitResult result =
          new net.minecraft.world.phys.BlockHitResult(location, face, pos, false);
      p.gameMode.useItemOn(p, level, p.getItemInHand(hand), hand, result);
    }
    p.setShiftKeyDown(false);
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

  /**
   * The {@code use_item} action: the named player gets {@code count} of {@code item} in its main
   * hand and uses it, as a {@code use_item} packet would ({@code ServerPlayerGameMode.useItem}).
   * The {@code interact} action uses it on the named {@code target} instead ({@code
   * Player.interactOn}).
   */
  void useItem(JsonObject action) {
    ServerPlayer player = (ServerPlayer) byName.get(action.get("entity").getAsString());
    var item =
        BuiltInRegistries.ITEM.getValue(Identifier.parse(action.get("item").getAsString()));
    int count = action.has("count") ? action.get("count").getAsInt() : 1;
    net.minecraft.world.item.ItemStack stack = new net.minecraft.world.item.ItemStack(item, count);
    player.setItemInHand(net.minecraft.world.InteractionHand.MAIN_HAND, stack);
    if (action.has("target")) {
      player.interactOn(
          byName.get(action.get("target").getAsString()),
          net.minecraft.world.InteractionHand.MAIN_HAND,
          net.minecraft.world.phys.Vec3.ZERO);
      return;
    }
    player.gameMode.useItem(
        player, player.level(), stack, net.minecraft.world.InteractionHand.MAIN_HAND);
  }

  /** The {@code effect} action: {@code addEffect} on a named living entity. */
  void effect(JsonObject action) {
    LivingEntity target = (LivingEntity) byName.get(action.get("entity").getAsString());
    var effect =
        BuiltInRegistries.MOB_EFFECT
            .get(Identifier.parse(action.get("effect").getAsString()))
            .orElseThrow();
    int amplifier = action.has("amplifier") ? action.get("amplifier").getAsInt() : 0;
    target.addEffect(
        new net.minecraft.world.effect.MobEffectInstance(
            effect, action.get("duration").getAsInt(), amplifier));
  }

  /**
   * The {@code throw_potion} action: a splash or lingering potion ({@code item}) of {@code
   * potion} at {@code pos} flying with {@code motion}, thrown by the named {@code by} if given.
   */
  void throwPotion(JsonObject action) {
    var item =
        BuiltInRegistries.ITEM.getValue(Identifier.parse(action.get("item").getAsString()));
    var potion =
        BuiltInRegistries.POTION
            .get(Identifier.parse(action.get("potion").getAsString()))
            .orElseThrow();
    var stack = net.minecraft.world.item.alchemy.PotionContents.createItemStack(item, potion);
    JsonArray p = action.getAsJsonArray("pos");
    JsonArray m = action.getAsJsonArray("motion");
    double x = p.get(0).getAsDouble();
    double y = p.get(1).getAsDouble();
    double z = p.get(2).getAsDouble();
    net.minecraft.world.entity.projectile.throwableitemprojectile.AbstractThrownPotion thrown =
        item == net.minecraft.world.item.Items.LINGERING_POTION
            ? new net.minecraft.world.entity.projectile.throwableitemprojectile
                .ThrownLingeringPotion(level, x, y, z, stack)
            : new net.minecraft.world.entity.projectile.throwableitemprojectile
                .ThrownSplashPotion(level, x, y, z, stack);
    if (action.has("by")) {
      thrown.setOwner(byName.get(action.get("by").getAsString()));
    }
    thrown.setDeltaMovement(
        m.get(0).getAsDouble(), m.get(1).getAsDouble(), m.get(2).getAsDouble());
    level.addFreshEntity(thrown);
  }

  private String nameOf(Entity e) {
    String n = name(e);
    return n != null ? n : BuiltInRegistries.ENTITY_TYPE.getKey(e.getType()).toString();
  }

  /** {@code ,{...}} with the detail of an entity, or nothing for entities without detail. */
  private static String hand(net.minecraft.world.item.ItemStack stack) {
    return "[\"" + BuiltInRegistries.ITEM.getKey(stack.getItem()) + "\"," + stack.getCount() + "]";
  }

  /** The scenario name of an entity, also after it changed dimension. */
  private String name(Entity e) {
    String name = names.get(e);
    return name != null ? name : byUuid.get(e.getUUID());
  }

  String detail(Entity e) {
    StringBuilder sb = new StringBuilder();
    String name = name(e);
    if (name != null) {
      sb.append(",\"name\":\"").append(name).append('"');
    }
    if (e instanceof ItemEntity item) {
      sb.append(",\"item\":\"")
          .append(BuiltInRegistries.ITEM.getKey(item.getItem().getItem()))
          .append("\",\"count\":")
          .append(item.getItem().getCount());
    }
    if (playerState && e instanceof Player player) {
      sb.append(",\"food\":[")
          .append(player.getFoodData().getFoodLevel())
          .append(',')
          .append(Float.floatToRawIntBits(player.getFoodData().getSaturationLevel()))
          .append("],\"hands\":[")
          .append(hand(player.getMainHandItem()))
          .append(',')
          .append(hand(player.getOffhandItem()))
          .append(']');
    }
    if (sleepState && e instanceof ServerPlayer sp) {
      BlockPos bed = sp.getSleepingPos().orElse(null);
      ServerPlayer.RespawnConfig spawn = sp.getRespawnConfig();
      sb.append(",\"sleep\":{\"pos\":")
          .append(bed == null ? "null" : "[" + bed.getX() + "," + bed.getY() + "," + bed.getZ() + "]")
          .append(",\"pose\":")
          .append(sp.getPose().id())
          .append(",\"spawn\":");
      if (spawn == null) {
        sb.append("null");
      } else {
        BlockPos at = spawn.respawnData().pos();
        sb.append('[')
            .append(at.getX())
            .append(',')
            .append(at.getY())
            .append(',')
            .append(at.getZ())
            .append(',')
            .append(spawn.forced())
            .append(']');
      }
      sb.append(",\"timer\":").append(sp.getSleepTimer()).append('}');
    }
    if (e instanceof AbstractMinecart cart) {
      sb.append(",\"rot\":[")
          .append(Float.floatToRawIntBits(cart.getYRot()))
          .append(',')
          .append(Float.floatToRawIntBits(cart.getXRot()))
          .append(']');
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

// Copyright Hannes Thöne. Licensed under the Apache License, Version 2.0.
package dev.vanillite.extractor;

import com.google.gson.Gson;
import com.google.gson.GsonBuilder;
import com.google.gson.JsonArray;
import com.google.gson.JsonElement;
import com.google.gson.JsonObject;
import java.io.IOException;
import java.io.Writer;
import it.unimi.dsi.fastutil.objects.Object2IntMap;
import java.lang.reflect.Field;
import java.lang.reflect.Method;
import java.lang.reflect.Modifier;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import java.util.Locale;
import java.util.function.Function;
import net.minecraft.SharedConstants;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.network.ProtocolInfo;
import net.minecraft.network.protocol.PacketType;
import net.minecraft.network.syncher.EntityDataAccessor;
import net.minecraft.network.syncher.EntityDataSerializer;
import net.minecraft.network.syncher.EntityDataSerializers;
import net.minecraft.resources.RegistryDataLoader;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.entity.EntityDimensions;
import net.minecraft.world.entity.EntityType;

/**
 * The extractor subset the multi-version data needs ({@code cargo xtask multiversion add}): entity
 * type facts, the synchronised registries, the packet shapes and the packet ids, in the files and
 * shapes of the full extractor and the data generator. It uses only API that every version from 1.20.5 on has (older jars are remapped
 * with Mojang's mappings first), so it also runs where the full extractor does not compile.
 *
 * <p>Usage: {@code java -cp <classes>:<server jar>:<libraries>
 * dev.vanillite.extractor.MultiversionMain <out>}
 */
public final class MultiversionMain {
  private static final Gson GSON = new GsonBuilder().disableHtmlEscaping().create();

  private MultiversionMain() {}

  public static void main(String[] args) throws Exception {
    if (args.length != 1) {
      System.err.println("usage: MultiversionMain <output directory>");
      System.exit(2);
    }
    Path out = Path.of(args[0]);
    Files.createDirectories(out);
    SharedConstants.tryDetectVersion();
    Bootstrap.bootStrap();
    write(out.resolve("entities.json"), entities());
    write(out.resolve("registries.json"), registries());
    write(out.resolve("packet_fields.json"), Packets.extract());
    write(out.resolve("packets.json"), packetIds());
    write(out.resolve("entity_data.json"), entityData());
    System.out.println("extractor: wrote " + out.toAbsolutePath());
  }

  private static JsonObject entities() {
    JsonArray types = new JsonArray();
    for (int id = 0; id < BuiltInRegistries.ENTITY_TYPE.size(); id++) {
      EntityType<?> t = BuiltInRegistries.ENTITY_TYPE.byId(id);
      EntityDimensions d = t.getDimensions();
      JsonObject o = new JsonObject();
      o.addProperty("name", BuiltInRegistries.ENTITY_TYPE.getKey(t).toString());
      o.addProperty("category", t.getCategory().getName());
      o.addProperty("width", d.width());
      o.addProperty("height", d.height());
      o.addProperty("eye_height", d.eyeHeight());
      types.add(o);
    }
    JsonObject root = new JsonObject();
    root.add("entity_types", types);
    return root;
  }

  @SuppressWarnings("unchecked")
  private static JsonObject registries() throws Exception {
    JsonArray a = new JsonArray();
    for (RegistryDataLoader.RegistryData<?> d :
        (List<RegistryDataLoader.RegistryData<?>>)
            RegistryDataLoader.class.getField("SYNCHRONIZED_REGISTRIES").get(null)) {
      Object key = d.key();
      // `location()` until 1.21.10, `identifier()` from 1.21.11.
      Method m;
      try {
        m = key.getClass().getMethod("identifier");
      } catch (NoSuchMethodException e) {
        m = key.getClass().getMethod("location");
      }
      a.add(m.invoke(key).toString());
    }
    JsonObject root = new JsonObject();
    root.add("synchronized", a);
    return root;
  }

  /**
   * Entity data layouts: the serializers by id and, per entity type, its data items {@code [index,
   * serializer, field]} (the static {@code EntityDataAccessor} fields of its class hierarchy).
   *
   * <p>An entity type's class is found by calling its factory without a level: the constructor
   * either succeeds or fails inside the class's constructor, which the stack trace names.
   */
  static JsonObject entityData() throws Exception {
    Map<Object, String> serializerNames = new HashMap<>();
    for (Field f : EntityDataSerializers.class.getDeclaredFields()) {
      if (Modifier.isStatic(f.getModifiers()) && EntityDataSerializer.class.isAssignableFrom(f.getType())) {
        f.setAccessible(true);
        serializerNames.put(f.get(null), f.getName().toLowerCase(Locale.ROOT));
      }
    }
    JsonArray serializers = new JsonArray();
    for (int id = 0; ; id++) {
      EntityDataSerializer<?> s = EntityDataSerializers.getSerializer(id);
      if (s == null) {
        break;
      }
      serializers.add(serializerNames.getOrDefault(s, "unknown_" + id));
    }
    JsonObject types = new JsonObject();
    for (int id = 0; id < BuiltInRegistries.ENTITY_TYPE.size(); id++) {
      EntityType<?> t = BuiltInRegistries.ENTITY_TYPE.byId(id);
      String name = BuiltInRegistries.ENTITY_TYPE.getKey(t).toString();
      Class<?> c = entityClass(t, name);
      JsonArray items = new JsonArray();
      if (c != null) {
        java.util.TreeMap<Integer, JsonArray> byIndex = new java.util.TreeMap<>();
        for (Class<?> k = c; k != null && k != Object.class; k = k.getSuperclass()) {
          for (Field f : k.getDeclaredFields()) {
            if (!Modifier.isStatic(f.getModifiers()) || f.getType() != EntityDataAccessor.class) {
              continue;
            }
            f.setAccessible(true);
            EntityDataAccessor<?> a = (EntityDataAccessor<?>) f.get(null);
            JsonArray item = new JsonArray();
            item.add(a.id());
            item.add(serializerNames.getOrDefault(a.serializer(), "unknown"));
            item.add(k.getSimpleName() + "." + f.getName());
            byIndex.putIfAbsent(a.id(), item);
          }
        }
        byIndex.values().forEach(items::add);
      }
      types.add(name, items);
    }
    JsonObject root = new JsonObject();
    root.add("serializers", serializers);
    root.add("types", types);
    return root;
  }

  private static Class<?> entityClass(EntityType<?> t, String name) {
    if (name.equals("minecraft:player")) {
      return net.minecraft.world.entity.player.Player.class;
    }
    try {
      Field f = EntityType.class.getDeclaredField("factory");
      f.setAccessible(true);
      Object factory = f.get(t);
      Method create = null;
      for (Method m : factory.getClass().getMethods()) {
        if (m.getName().equals("create") && m.getParameterCount() == 2) {
          create = m;
        }
      }
      create.setAccessible(true);
      Object e = create.invoke(factory, t, null);
      if (e != null) {
        return e.getClass();
      }
    } catch (Throwable thrown) {
      Throwable e = thrown instanceof java.lang.reflect.InvocationTargetException ite && ite.getCause() != null ? ite.getCause() : thrown;
      StackTraceElement[] trace = e.getStackTrace();
      // The outermost entity constructor below this method is the type's class.
      for (int i = trace.length - 1; i >= 0; i--) {
        if (trace[i].getMethodName().equals("<init>")) {
          try {
            Class<?> c = Class.forName(trace[i].getClassName());
            if (net.minecraft.world.entity.Entity.class.isAssignableFrom(c)) {
              return c;
            }
          } catch (ClassNotFoundException ignored) {
            // Not an entity class.
          }
        }
      }
    }
    for (String candidate : FACTORYLESS) {
      if (candidate.startsWith(name + "=")) {
        try {
          return Class.forName(candidate.substring(candidate.indexOf('=') + 1));
        } catch (ClassNotFoundException ignored) {
          // Moved in this version.
        }
      }
    }
    System.err.println("extractor: no class for " + name);
    return null;
  }

  /** Entity types without a factory. */
  private static final String[] FACTORYLESS = {
    "minecraft:fishing_bobber=net.minecraft.world.entity.projectile.FishingHook",
    "minecraft:fishing_bobber=net.minecraft.world.entity.projectile.fishing.FishingHook",
  };

  private static final String[] PROTOCOLS = {
    "handshake.HandshakeProtocols",
    "status.StatusProtocols",
    "login.LoginProtocols",
    "configuration.ConfigurationProtocols",
    "game.GameProtocols",
  };

  /**
   * Packet ids in the shape of the data generator's packet report, which versions before 1.21
   * lack: {@code {state: {flow: {name: {protocol_id}}}}}.
   */
  @SuppressWarnings("unchecked")
  private static JsonObject packetIds() throws Exception {
    JsonObject root = new JsonObject();
    for (String holder : PROTOCOLS) {
      Class<?> c = Class.forName("net.minecraft.network.protocol." + holder);
      for (Field f : c.getDeclaredFields()) {
        if (!Modifier.isStatic(f.getModifiers()) || !Modifier.isPublic(f.getModifiers())) {
          continue;
        }
        Object info = f.get(null);
        if (!(info instanceof ProtocolInfo<?>)) {
          info = bind(info);
        }
        if (!(info instanceof ProtocolInfo<?> p)) {
          continue;
        }
        Object2IntMap<Object> ids = (Object2IntMap<Object>) find(p.codec(), 0);
        if (ids == null) {
          throw new IllegalStateException("no packet ids in " + holder + "." + f.getName());
        }
        String state = p.id().id();
        String flow = p.flow().name().toLowerCase(Locale.ROOT);
        JsonObject s = root.has(state) ? root.getAsJsonObject(state) : new JsonObject();
        root.add(state, s);
        JsonObject packets = new JsonObject();
        for (Object2IntMap.Entry<Object> e : ids.object2IntEntrySet()) {
          JsonObject o = new JsonObject();
          o.addProperty("protocol_id", e.getIntValue());
          packets.add(((PacketType<?>) e.getKey()).id().toString(), o);
        }
        s.add(flow, packets);
      }
    }
    return root;
  }

  /** Binds an unbound protocol; its buffer decorator is never called. */
  private static Object bind(Object unbound) throws Exception {
    for (Method m : unbound.getClass().getMethods()) {
      if (m.getName().equals("bind")) {
        Object[] args = new Object[m.getParameterCount()];
        Class<?>[] types = m.getParameterTypes();
        for (int i = 0; i < args.length; i++) {
          args[i] = types[i] == Function.class ? (Function<Object, Object>) b -> b : null;
        }
        m.setAccessible(true);
        return m.invoke(unbound, args);
      }
    }
    return null;
  }

  /** The packet id map of a dispatch codec, possibly wrapped. */
  private static Object find(Object codec, int depth) throws Exception {
    if (codec == null || depth > 3) {
      return null;
    }
    for (Class<?> k = codec.getClass(); k != null && k != Object.class; k = k.getSuperclass()) {
      for (Field f : k.getDeclaredFields()) {
        if (Modifier.isStatic(f.getModifiers()) || f.getType().isPrimitive()) {
          continue;
        }
        f.setAccessible(true);
        Object v = f.get(codec);
        if (v instanceof Object2IntMap<?> && k.getSimpleName().equals("IdDispatchCodec")) {
          return v;
        }
        if (v != null && v.getClass().getName().startsWith("net.minecraft")) {
          Object found = find(v, depth + 1);
          if (found != null) {
            return found;
          }
        }
      }
    }
    return null;
  }

  private static void write(Path file, JsonElement json) throws IOException {
    try (Writer w = Files.newBufferedWriter(file, StandardCharsets.UTF_8)) {
      GSON.toJson(json, w);
    }
  }
}

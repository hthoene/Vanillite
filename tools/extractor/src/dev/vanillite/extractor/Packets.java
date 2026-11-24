// Copyright Hannes Thöne. Licensed under the Apache License, Version 2.0.
package dev.vanillite.extractor;

import com.google.gson.JsonArray;
import com.google.gson.JsonObject;
import java.lang.reflect.Field;
import java.lang.reflect.Modifier;
import java.lang.reflect.ParameterizedType;
import java.lang.reflect.RecordComponent;
import java.lang.reflect.Type;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Enumeration;
import java.util.List;
import java.util.Map;
import java.util.TreeMap;
import java.util.zip.ZipEntry;
import java.util.zip.ZipFile;
import net.minecraft.network.protocol.Packet;

/**
 * The Java shape of every packet type: {@code "<flow>/<id>" -> {class, fields}}.
 *
 * <p>Packet types are the static {@code PacketType<P>} fields of the {@code *PacketTypes} classes;
 * the type argument is the packet class. Its fields are the record components (or the instance
 * fields up the class hierarchy) with simplified generic types; records and enums nested in the
 * packet class are expanded one level. This is the in-memory shape, not the wire format, but a
 * changed field almost always means a changed codec. Only used for the version change report.
 */
final class Packets {
  private static final String PACKAGE = "net/minecraft/network/protocol/";

  private Packets() {}

  static JsonObject extract() throws Exception {
    Path jar = Path.of(Packet.class.getProtectionDomain().getCodeSource().getLocation().toURI());
    List<String> holders = new ArrayList<>();
    try (ZipFile zip = new ZipFile(jar.toFile())) {
      Enumeration<? extends ZipEntry> entries = zip.entries();
      while (entries.hasMoreElements()) {
        String name = entries.nextElement().getName();
        if (name.startsWith(PACKAGE)
            && name.endsWith("PacketTypes.class")
            && !name.contains("$")) {
          holders.add(name.substring(0, name.length() - 6).replace('/', '.'));
        }
      }
    }
    Map<String, JsonObject> out = new TreeMap<>();
    for (String holder : holders) {
      for (Field f : Class.forName(holder).getDeclaredFields()) {
        if (!Modifier.isStatic(f.getModifiers())
            || !f.getType().getName().equals("net.minecraft.network.protocol.PacketType")
            || !(f.getGenericType() instanceof ParameterizedType pt)
            || !(pt.getActualTypeArguments()[0] instanceof Class<?> packet)) {
          continue;
        }
        f.setAccessible(true);
        Object type = f.get(null);
        String flow = call(type, "flow").toLowerCase(java.util.Locale.ROOT);
        String id = call(type, "id");
        JsonObject p = new JsonObject();
        p.addProperty("class", simple(packet.getName()));
        JsonArray fields = new JsonArray();
        fields(packet, packet, "", 0, fields);
        p.add("fields", fields);
        out.put(flow + "/" + id, p);
      }
    }
    JsonObject root = new JsonObject();
    out.forEach(root::add);
    return root;
  }

  private static String call(Object o, String method) throws Exception {
    var m = o.getClass().getMethod(method);
    m.setAccessible(true);
    return String.valueOf(m.invoke(o));
  }

  private static void fields(Class<?> c, Class<?> packet, String prefix, int depth, JsonArray out) {
    if (c.isEnum()) {
      List<String> constants = new ArrayList<>();
      for (Object e : c.getEnumConstants()) {
        constants.add(((Enum<?>) e).name());
      }
      out.add(prefix + "{" + String.join(", ", constants) + "}");
      return;
    }
    List<Map.Entry<String, Type>> list = new ArrayList<>();
    if (c.isRecord()) {
      for (RecordComponent rc : c.getRecordComponents()) {
        list.add(Map.entry(rc.getName(), rc.getGenericType()));
      }
    } else {
      List<Class<?>> chain = new ArrayList<>();
      for (Class<?> k = c; k != null && k != Object.class && k != Record.class; k = k.getSuperclass()) {
        chain.add(0, k);
      }
      for (Class<?> k : chain) {
        for (Field f : k.getDeclaredFields()) {
          if (!Modifier.isStatic(f.getModifiers()) && !f.isSynthetic()) {
            list.add(Map.entry(f.getName(), f.getGenericType()));
          }
        }
      }
    }
    for (Map.Entry<String, Type> e : list) {
      out.add(prefix + e.getKey() + ": " + simple(e.getValue().getTypeName()));
      if (depth == 0) {
        List<Class<?>> nested = new ArrayList<>();
        nested(e.getValue(), packet, nested);
        for (Class<?> n : nested) {
          fields(n, packet, prefix + e.getKey() + ".", depth + 1, out);
        }
      }
    }
  }

  /** Records and enums nested in the packet class that a field type mentions. */
  private static void nested(Type t, Class<?> packet, List<Class<?>> out) {
    if (t instanceof ParameterizedType pt) {
      for (Type arg : pt.getActualTypeArguments()) {
        nested(arg, packet, out);
      }
    } else if (t instanceof Class<?> c
        && c.getName().startsWith(packet.getName() + "$")
        && (c.isRecord() || c.isEnum())
        && !out.contains(c)) {
      out.add(c);
    }
  }

  /** {@code java.util.List<net.minecraft.world.item.ItemStack>} -> {@code List<ItemStack>}. */
  private static String simple(String type) {
    return type.replaceAll("\\b[a-z_][a-z0-9_]*\\.", "").replace('$', '.');
  }
}

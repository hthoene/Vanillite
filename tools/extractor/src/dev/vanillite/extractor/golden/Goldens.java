// Copyright Hannes Thöne. Licensed under the Apache License, Version 2.0.
package dev.vanillite.extractor.golden;

import com.google.gson.Gson;
import com.google.gson.GsonBuilder;
import com.google.gson.JsonArray;
import com.google.gson.JsonElement;
import com.google.gson.JsonObject;
import java.io.IOException;
import java.io.Writer;
import java.lang.reflect.Method;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Arrays;
import java.util.Map;
import net.minecraft.SharedConstants;
import net.minecraft.server.Bootstrap;

/**
 * Parity golden recorder for world generation.
 *
 * <p>Runs Vanilla code in-process (no server start) and writes the values Vanillite must
 * reproduce bit for bit. Each part is a class in this package with a {@code public static
 * JsonElement extract()} method; its output goes to {@code <out>/<part>.json}, where {@code
 * <part>} is the class name without the {@code Goldens} suffix in snake case. A part may instead
 * have a {@code public static Map<String, JsonObject> extractFiles()} method: each entry goes to
 * {@code <out>/<key>.json} in a line layout (see {@link #writeLines}).
 *
 * <p>Parts in sub-packages are named relative to this package, e.g. {@code data.ItemsGoldens}.
 *
 * <p>Usage: {@code java -cp <classes>:<server jar>:<libraries>
 * dev.vanillite.extractor.golden.Goldens <out> <PartGoldens>...}
 */
public final class Goldens {
  private static final Gson GSON =
      new GsonBuilder().disableHtmlEscaping().serializeSpecialFloatingPointValues().create();

  /** For {@link #writeLines}: keeps null members (a missing value is meaningful there). */
  private static final Gson LINES_GSON = new GsonBuilder().disableHtmlEscaping()
      .serializeSpecialFloatingPointValues().serializeNulls().create();

  private Goldens() {}

  public static void main(String[] args) throws Exception {
    if (args.length < 2) {
      System.err.println("usage: Goldens <output directory> <PartGoldens>...");
      System.exit(2);
    }
    Path out = Path.of(args[0]);
    Files.createDirectories(out);
    SharedConstants.tryDetectVersion();
    Bootstrap.bootStrap();
    for (int i = 1; i < args.length; i++) {
      String part = args[i];
      Class<?> type = Class.forName(Goldens.class.getPackageName() + "." + part);
      Method files = Arrays.stream(type.getMethods()).filter(m -> m.getName().equals("extractFiles"))
          .findFirst().orElse(null);
      if (files != null) {
        @SuppressWarnings("unchecked")
        Map<String, JsonObject> map = (Map<String, JsonObject>) files.invoke(null);
        for (Map.Entry<String, JsonObject> e : map.entrySet()) {
          Path file = out.resolve(e.getKey() + ".json");
          try (Writer w = Files.newBufferedWriter(file, StandardCharsets.UTF_8)) {
            writeLines(e.getValue(), w);
          }
          System.out.println("goldens: wrote " + file);
        }
        continue;
      }
      Method extract = type.getMethod("extract");
      JsonElement json = (JsonElement) extract.invoke(null);
      Path file = out.resolve(fileName(part));
      try (Writer w = Files.newBufferedWriter(file, StandardCharsets.UTF_8)) {
        GSON.toJson(json, w);
      }
      System.out.println("goldens: wrote " + file);
    }
  }

  /**
   * Writes an object with one member per line and, for members that are arrays, one element per
   * line (each element compact), so a diff of the golden shows the changed records.
   */
  static void writeLines(JsonObject root, Writer w) throws IOException {
    w.write("{\n");
    int i = 0;
    for (Map.Entry<String, JsonElement> e : root.entrySet()) {
      w.write(LINES_GSON.toJson(e.getKey()));
      w.write(": ");
      if (e.getValue() instanceof JsonArray array && !array.isEmpty()) {
        w.write("[\n");
        for (int j = 0; j < array.size(); j++) {
          w.write(LINES_GSON.toJson(array.get(j)));
          w.write(j + 1 < array.size() ? ",\n" : "\n");
        }
        w.write("]");
      } else {
        w.write(LINES_GSON.toJson(e.getValue()));
      }
      w.write(++i < root.size() ? ",\n" : "\n");
    }
    w.write("}\n");
  }

  private static String fileName(String part) {
    String simple = part.substring(part.lastIndexOf('.') + 1);
    String base = simple.endsWith("Goldens") ? simple.substring(0, simple.length() - 7) : simple;
    return base.replaceAll("([a-z0-9])([A-Z])", "$1_$2").toLowerCase() + ".json";
  }

  /** Bit pattern of a float, so goldens compare exactly (JSON numbers would round-trip text). */
  public static int bits(float value) {
    return Float.floatToRawIntBits(value);
  }

  /** Bit pattern of a double. */
  public static long bits(double value) {
    return Double.doubleToRawLongBits(value);
  }
}

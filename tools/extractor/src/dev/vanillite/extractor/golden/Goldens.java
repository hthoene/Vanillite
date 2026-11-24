// Copyright Hannes Thöne. Licensed under the Apache License, Version 2.0.
package dev.vanillite.extractor.golden;

import com.google.gson.Gson;
import com.google.gson.GsonBuilder;
import com.google.gson.JsonElement;
import java.io.Writer;
import java.lang.reflect.Method;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import net.minecraft.SharedConstants;
import net.minecraft.server.Bootstrap;

/**
 * Parity golden recorder for world generation.
 *
 * <p>Runs Vanilla code in-process (no server start) and writes the values Vanillite must
 * reproduce bit for bit. Each part is a class in this package with a {@code public static
 * JsonElement extract()} method; its output goes to {@code <out>/<part>.json}, where {@code
 * <part>} is the class name without the {@code Goldens} suffix in snake case.
 *
 * <p>Usage: {@code java -cp <classes>:<server jar>:<libraries>
 * dev.vanillite.extractor.golden.Goldens <out> <PartGoldens>...}
 */
public final class Goldens {
  private static final Gson GSON =
      new GsonBuilder().disableHtmlEscaping().serializeSpecialFloatingPointValues().create();

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
      Method extract = type.getMethod("extract");
      JsonElement json = (JsonElement) extract.invoke(null);
      Path file = out.resolve(fileName(part));
      try (Writer w = Files.newBufferedWriter(file, StandardCharsets.UTF_8)) {
        GSON.toJson(json, w);
      }
      System.out.println("goldens: wrote " + file);
    }
  }

  private static String fileName(String part) {
    String base = part.endsWith("Goldens") ? part.substring(0, part.length() - 7) : part;
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

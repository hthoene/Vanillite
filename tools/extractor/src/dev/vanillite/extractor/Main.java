// Copyright Hannes Thöne. Licensed under the Apache License, Version 2.0.
package dev.vanillite.extractor;

import com.google.gson.Gson;
import com.google.gson.GsonBuilder;
import com.google.gson.JsonElement;
import java.io.IOException;
import java.io.Writer;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import net.minecraft.SharedConstants;
import net.minecraft.server.Bootstrap;

/**
 * Vanillite data extractor.
 *
 * <p>Boots the Vanilla registries of the unobfuscated server jar it is compiled against and dumps
 * the facts that Mojang's data generator does not report (per-state facts, shapes, entity type
 * facts, fluid states, dynamic registry lists, packet fields). The output is raw JSON; {@code cargo
 * xtask vanilla update} normalises it into {@code data/vanilla/}.
 *
 * <p>Usage: {@code java -cp <classes>:<server jar>:<libraries> dev.vanillite.extractor.Main <out>}
 */
public final class Main {
  private static final Gson GSON = new GsonBuilder().disableHtmlEscaping().create();

  private Main() {}

  public static void main(String[] args) throws Exception {
    if (args.length != 1) {
      System.err.println("usage: Main <output directory>");
      System.exit(2);
    }
    Path out = Path.of(args[0]);
    Files.createDirectories(out);

    SharedConstants.tryDetectVersion();
    Bootstrap.bootStrap();

    write(out.resolve("blocks.json"), Blocks.extract());
    write(out.resolve("fluid_states.json"), Fluids.extract());
    write(out.resolve("entities.json"), Entities.extract());
    write(out.resolve("items.json"), Items.extract());
    write(out.resolve("registries.json"), Registries.extract());
    write(out.resolve("packet_fields.json"), Packets.extract());
    write(out.resolve("entity_data.json"), MultiversionMain.entityData());
    System.out.println("extractor: wrote " + out.toAbsolutePath());
  }

  private static void write(Path file, JsonElement json) throws IOException {
    try (Writer w = Files.newBufferedWriter(file, StandardCharsets.UTF_8)) {
      GSON.toJson(json, w);
    }
  }
}

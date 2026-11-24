// Copyright Hannes Thöne. Licensed under the Apache License, Version 2.0.
package dev.vanillite.extractor.golden.data;

import dev.vanillite.extractor.golden.ParityServer;
import java.nio.charset.StandardCharsets;
import java.util.List;
import net.minecraft.server.WorldStem;
import net.minecraft.server.packs.repository.PackRepository;
import net.minecraft.server.packs.repository.ServerPacksSource;

/**
 * Shared helpers of the data goldens: large lists are stored as count and hash so the goldens
 * stay small enough for the repository; the Rust side hashes its tables the same way.
 */
final class DataGoldens {
  private static WorldStem stem;

  private DataGoldens() {}

  /**
   * The Vanilla data pack loaded like at server start (once per run). Loading it also binds the
   * default item components.
   */
  static synchronized WorldStem stem() throws Exception {
    if (stem == null) {
      PackRepository packs = ServerPacksSource.createVanillaTrustedRepository();
      packs.reload();
      stem =
          ParityServer.loadStem(
              packs, 0, List.of(new ParityServer.Layer("minecraft:stone", 1)), "minecraft:plains");
    }
    return stem;
  }

  /** FNV-1a 64 over the UTF-8 bytes of every line followed by {@code \n}, as 16 hex digits. */
  static String hash(List<String> lines) {
    long h = 0xcbf29ce484222325L;
    for (String line : lines) {
      for (byte b : (line + "\n").getBytes(StandardCharsets.UTF_8)) {
        h ^= b & 0xff;
        h *= 0x100000001b3L;
      }
    }
    return String.format("%016x", h);
  }
}

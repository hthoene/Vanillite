// Copyright Hannes Thöne. Licensed under the Apache License, Version 2.0.
package dev.vanillite.extractor.golden;

import com.google.gson.JsonElement;
import com.google.gson.JsonParser;
import java.io.InputStreamReader;
import java.nio.charset.StandardCharsets;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Enumeration;
import java.util.HashMap;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.zip.ZipEntry;
import java.util.zip.ZipFile;
import net.minecraft.core.Holder;
import net.minecraft.core.MappedRegistry;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.core.registries.Registries;
import net.minecraft.resources.Identifier;
import net.minecraft.tags.TagKey;
import net.minecraft.tags.TagLoader;
import net.minecraft.world.level.block.Block;

/**
 * Binds the Vanilla block tags from the server jar's data pack, which {@code Bootstrap} alone
 * leaves unbound (world generation tests {@code uncarvable} and {@code
 * blocks_motion_in_heightmap}).
 */
final class BlockTags {
  private static final String PREFIX = "data/minecraft/tags/block/";
  private static boolean bound;

  private BlockTags() {}

  static synchronized void bind() throws Exception {
    if (bound) {
      return;
    }
    Path jar = Path.of(Block.class.getProtectionDomain().getCodeSource().getLocation().toURI());
    Map<String, JsonElement> raw = new HashMap<>();
    try (ZipFile zip = new ZipFile(jar.toFile())) {
      Enumeration<? extends ZipEntry> entries = zip.entries();
      while (entries.hasMoreElements()) {
        ZipEntry entry = entries.nextElement();
        String name = entry.getName();
        if (name.startsWith(PREFIX) && name.endsWith(".json")) {
          String id = "minecraft:" + name.substring(PREFIX.length(), name.length() - 5);
          try (InputStreamReader r = new InputStreamReader(zip.getInputStream(entry), StandardCharsets.UTF_8)) {
            raw.put(id, JsonParser.parseReader(r));
          }
        }
      }
    }
    Map<TagKey<Block>, List<Holder<Block>>> tags = new HashMap<>();
    Map<String, Set<String>> resolved = new HashMap<>();
    for (String id : raw.keySet()) {
      List<Holder<Block>> holders = new ArrayList<>();
      for (String block : resolve(id, raw, resolved)) {
        BuiltInRegistries.BLOCK.get(Identifier.parse(block)).ifPresent(holders::add);
      }
      tags.put(TagKey.create(Registries.BLOCK, Identifier.parse(id)), holders);
    }
    ((MappedRegistry<Block>) BuiltInRegistries.BLOCK)
        .prepareTagReload(new TagLoader.LoadResult<>(Registries.BLOCK, tags))
        .apply();
    bound = true;
  }

  private static Set<String> resolve(String id, Map<String, JsonElement> raw, Map<String, Set<String>> done) {
    Set<String> cached = done.get(id);
    if (cached != null) {
      return cached;
    }
    Set<String> out = new LinkedHashSet<>();
    JsonElement json = raw.get(id);
    if (json != null) {
      for (JsonElement value : json.getAsJsonObject().getAsJsonArray("values")) {
        String entry = value.isJsonObject() ? value.getAsJsonObject().get("id").getAsString() : value.getAsString();
        if (entry.startsWith("#")) {
          out.addAll(resolve(qualify(entry.substring(1)), raw, done));
        } else {
          out.add(qualify(entry));
        }
      }
    }
    done.put(id, out);
    return out;
  }

  private static String qualify(String id) {
    return id.contains(":") ? id : "minecraft:" + id;
  }
}

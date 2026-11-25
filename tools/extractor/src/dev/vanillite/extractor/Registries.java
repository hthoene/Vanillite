// Copyright Hannes Thöne. Licensed under the Apache License, Version 2.0.
package dev.vanillite.extractor;

import com.google.gson.JsonArray;
import com.google.gson.JsonObject;
import java.util.List;
import net.minecraft.resources.RegistryDataLoader;

/** Which registries are data-driven, in which order Vanilla loads and syncs them. */
final class Registries {
  private Registries() {}

  static JsonObject extract() {
    JsonObject root = new JsonObject();
    // By name, so the extractor also runs against older versions (multi-version data): 26.2
    // has `WORLDGEN_REGISTRIES` and no reloadable list.
    root.add("world", keys(list("WORLD_REGISTRIES", "WORLDGEN_REGISTRIES")));
    root.add("dimension", keys(list("DIMENSION_REGISTRIES")));
    root.add("reloadable", keys(list("RELOADABLE_REGISTRIES")));
    root.add("synchronized", keys(list("SYNCHRONIZED_REGISTRIES")));
    return root;
  }

  @SuppressWarnings("unchecked")
  private static List<RegistryDataLoader.RegistryData<?>> list(String... names) {
    for (String name : names) {
      try {
        return (List<RegistryDataLoader.RegistryData<?>>)
            RegistryDataLoader.class.getField(name).get(null);
      } catch (ReflectiveOperationException e) {
        // Try the next name.
      }
    }
    return List.of();
  }

  private static JsonArray keys(List<RegistryDataLoader.RegistryData<?>> list) {
    JsonArray a = new JsonArray();
    for (RegistryDataLoader.RegistryData<?> d : list) {
      a.add(d.key().identifier().toString());
    }
    return a;
  }
}

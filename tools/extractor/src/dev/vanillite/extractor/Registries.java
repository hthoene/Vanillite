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
    root.add("world", keys(RegistryDataLoader.WORLD_REGISTRIES));
    root.add("dimension", keys(RegistryDataLoader.DIMENSION_REGISTRIES));
    root.add("reloadable", keys(RegistryDataLoader.RELOADABLE_REGISTRIES));
    root.add("synchronized", keys(RegistryDataLoader.SYNCHRONIZED_REGISTRIES));
    return root;
  }

  private static JsonArray keys(List<RegistryDataLoader.RegistryData<?>> list) {
    JsonArray a = new JsonArray();
    for (RegistryDataLoader.RegistryData<?> d : list) {
      a.add(d.key().identifier().toString());
    }
    return a;
  }
}

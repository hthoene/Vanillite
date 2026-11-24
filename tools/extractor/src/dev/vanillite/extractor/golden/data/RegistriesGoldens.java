// Copyright Hannes Thöne. Licensed under the Apache License, Version 2.0.
package dev.vanillite.extractor.golden.data;

import com.google.gson.JsonElement;
import com.google.gson.JsonNull;
import com.google.gson.JsonObject;
import com.google.gson.JsonPrimitive;
import java.util.ArrayList;
import java.util.List;
import java.util.TreeMap;
import net.minecraft.core.DefaultedRegistry;
import net.minecraft.core.Registry;
import net.minecraft.core.registries.BuiltInRegistries;

/**
 * Every built-in (static) registry as Vanilla holds it in memory: its id in the root registry,
 * the default entry and the entry names in protocol id order (count and hash).
 */
public final class RegistriesGoldens {
  private RegistriesGoldens() {}

  public static JsonElement extract() {
    TreeMap<String, JsonObject> sorted = new TreeMap<>();
    for (Registry<?> registry : BuiltInRegistries.REGISTRY) {
      sorted.put(registry.key().identifier().toString(), entry(registry));
    }
    JsonObject root = new JsonObject();
    sorted.forEach(root::add);
    return root;
  }

  private static <T> JsonObject entry(Registry<T> registry) {
    List<String> names = new ArrayList<>();
    for (int id = 0; id < registry.size(); id++) {
      names.add(registry.getKey(registry.byId(id)).toString());
    }
    JsonObject o = new JsonObject();
    @SuppressWarnings("unchecked")
    Registry<Registry<?>> root = (Registry<Registry<?>>) BuiltInRegistries.REGISTRY;
    o.addProperty("id", root.getId(registry));
    o.add(
        "default",
        registry instanceof DefaultedRegistry<?> d
            ? new JsonPrimitive(d.getDefaultKey().toString())
            : JsonNull.INSTANCE);
    o.addProperty("count", names.size());
    o.addProperty("hash", DataGoldens.hash(names));
    return o;
  }
}

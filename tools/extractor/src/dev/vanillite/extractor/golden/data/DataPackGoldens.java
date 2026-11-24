// Copyright Hannes Thöne. Licensed under the Apache License, Version 2.0.
package dev.vanillite.extractor.golden.data;

import com.google.gson.JsonElement;
import com.google.gson.JsonObject;
import java.util.ArrayList;
import java.util.List;
import java.util.TreeMap;
import net.minecraft.core.Holder;
import net.minecraft.core.HolderSet;
import net.minecraft.core.Registry;
import net.minecraft.core.RegistryAccess;
import net.minecraft.resources.RegistryDataLoader;

/**
 * The result of loading the Vanilla data pack: the entries of every data-driven world registry
 * in protocol id order, and the resolved tags of every registry (each line {@code tag entry...}
 * in tag order, entries in Vanilla's resolution order), as count and hash.
 */
public final class DataPackGoldens {
  private DataPackGoldens() {}

  public static JsonElement extract() throws Exception {
    RegistryAccess access = DataGoldens.stem().registries().compositeAccess();

    JsonObject registries = new JsonObject();
    for (RegistryDataLoader.RegistryData<?> data : RegistryDataLoader.WORLD_REGISTRIES) {
      Registry<?> registry = access.lookupOrThrow(data.key());
      registries.add(data.key().identifier().toString(), entries(registry));
    }

    TreeMap<String, JsonObject> tags = new TreeMap<>();
    access
        .registries()
        .forEach(
            e -> {
              JsonObject t = tags(e.value());
              if (t != null) {
                tags.put(e.key().identifier().toString(), t);
              }
            });
    JsonObject tagRoot = new JsonObject();
    tags.forEach(tagRoot::add);

    JsonObject root = new JsonObject();
    root.add("registries", registries);
    root.add("tags", tagRoot);
    return root;
  }

  private static <T> JsonObject entries(Registry<T> registry) {
    List<String> names = new ArrayList<>();
    for (int id = 0; id < registry.size(); id++) {
      names.add(registry.getKey(registry.byId(id)).toString());
    }
    JsonObject o = new JsonObject();
    o.addProperty("count", names.size());
    o.addProperty("hash", DataGoldens.hash(names));
    return o;
  }

  private static <T> JsonObject tags(Registry<T> registry) {
    TreeMap<String, String> lines = new TreeMap<>();
    registry
        .getTags()
        .forEach(
            (HolderSet.Named<T> set) -> {
              StringBuilder sb = new StringBuilder(set.key().location().toString());
              for (Holder<T> h : set) {
                sb.append(' ').append(h.unwrapKey().orElseThrow().identifier());
              }
              lines.put(set.key().location().toString(), sb.toString());
            });
    if (lines.isEmpty()) {
      return null;
    }
    JsonObject o = new JsonObject();
    o.addProperty("count", lines.size());
    o.addProperty("hash", DataGoldens.hash(new ArrayList<>(lines.values())));
    return o;
  }
}

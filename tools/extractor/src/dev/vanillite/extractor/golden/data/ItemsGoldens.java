// Copyright Hannes Thöne. Licensed under the Apache License, Version 2.0.
package dev.vanillite.extractor.golden.data;

import com.google.gson.JsonArray;
import com.google.gson.JsonElement;
import com.google.gson.JsonObject;
import net.minecraft.core.component.DataComponents;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.world.item.BlockItem;
import net.minecraft.world.item.Item;
import net.minecraft.world.item.Rarity;

/**
 * Core facts of every item from its default components: [max stack size, max damage (0 if
 * none), rarity, placed block ("" if none)].
 */
public final class ItemsGoldens {
  private ItemsGoldens() {}

  public static JsonElement extract() throws Exception {
    DataGoldens.stem();
    JsonObject root = new JsonObject();
    for (Item item : BuiltInRegistries.ITEM) {
      JsonArray a = new JsonArray();
      a.add(item.getDefaultMaxStackSize());
      a.add(item.components().getOrDefault(DataComponents.MAX_DAMAGE, 0));
      a.add(item.components().getOrDefault(DataComponents.RARITY, Rarity.COMMON).getSerializedName());
      a.add(
          item instanceof BlockItem b
              ? BuiltInRegistries.BLOCK.getKey(b.getBlock()).toString()
              : "");
      root.add(BuiltInRegistries.ITEM.getKey(item).toString(), a);
    }
    return root;
  }
}

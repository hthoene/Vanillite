// Copyright Hannes Thöne. Licensed under the Apache License, Version 2.0.
package dev.vanillite.extractor;

import com.google.gson.JsonArray;
import com.google.gson.JsonObject;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.world.item.BlockItem;
import net.minecraft.world.item.Item;

/** Item facts the reports lack (the block an item places). Default components come from datagen. */
final class Items {
  private Items() {}

  static JsonArray extract() {
    JsonArray out = new JsonArray();
    for (int id = 0; id < BuiltInRegistries.ITEM.size(); id++) {
      Item item = BuiltInRegistries.ITEM.byId(id);
      JsonObject o = new JsonObject();
      o.addProperty("name", BuiltInRegistries.ITEM.getKey(item).toString());
      if (item instanceof BlockItem bi) {
        o.addProperty("block", BuiltInRegistries.BLOCK.getKey(bi.getBlock()).toString());
      }
      out.add(o);
    }
    return out;
  }
}

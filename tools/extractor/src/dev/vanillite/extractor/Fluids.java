// Copyright Hannes Thöne. Licensed under the Apache License, Version 2.0.
package dev.vanillite.extractor;

import com.google.gson.JsonArray;
import com.google.gson.JsonObject;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.material.Fluid;
import net.minecraft.world.level.material.FluidState;

/** Fluid states in the order of their global ids. */
final class Fluids {
  private Fluids() {}

  static JsonArray extract() {
    JsonArray out = new JsonArray();
    for (int id = 0; id < Fluid.FLUID_STATE_REGISTRY.size(); id++) {
      FluidState s = Fluid.FLUID_STATE_REGISTRY.byId(id);
      JsonObject o = new JsonObject();
      o.addProperty("id", id);
      o.addProperty("fluid", BuiltInRegistries.FLUID.getKey(s.getType()).toString());
      o.addProperty("amount", s.getAmount());
      o.addProperty("source", s.isSource());
      o.addProperty("own_height", s.getOwnHeight());
      o.addProperty("random_ticks", s.isRandomlyTicking());
      o.addProperty("explosion_resistance", s.getExplosionResistance());
      o.addProperty("block_state", Block.getId(s.createLegacyBlock()));
      JsonObject props = new JsonObject();
      s.getValues().forEach(v -> props.addProperty(v.property().getName(), v.valueName()));
      o.add("properties", props);
      out.add(o);
    }
    return out;
  }
}

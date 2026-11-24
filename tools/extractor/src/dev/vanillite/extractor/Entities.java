// Copyright Hannes Thöne. Licensed under the Apache License, Version 2.0.
package dev.vanillite.extractor;

import com.google.gson.JsonArray;
import com.google.gson.JsonObject;
import net.minecraft.core.Holder;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.world.entity.EntityDimensions;
import net.minecraft.world.entity.EntityType;
import net.minecraft.world.entity.LivingEntity;
import net.minecraft.world.entity.ai.attributes.Attribute;
import net.minecraft.world.entity.ai.attributes.AttributeSupplier;
import net.minecraft.world.entity.ai.attributes.DefaultAttributes;

/** Entity type facts and the default attributes of living entity types. */
final class Entities {
  private Entities() {}

  @SuppressWarnings("unchecked")
  static JsonObject extract() {
    JsonArray types = new JsonArray();
    for (int id = 0; id < BuiltInRegistries.ENTITY_TYPE.size(); id++) {
      EntityType<?> t = BuiltInRegistries.ENTITY_TYPE.byId(id);
      EntityDimensions d = t.getDimensions();
      JsonObject o = new JsonObject();
      o.addProperty("name", BuiltInRegistries.ENTITY_TYPE.getKey(t).toString());
      o.addProperty("category", t.getCategory().getName());
      o.addProperty("width", d.width());
      o.addProperty("height", d.height());
      o.addProperty("eye_height", d.eyeHeight());
      o.addProperty("fixed_size", d.fixed());
      o.addProperty("fire_immune", t.fireImmune());
      o.addProperty("summonable", t.canSummon());
      o.addProperty("can_spawn_far_from_player", t.canSpawnFarFromPlayer());
      o.addProperty("tracking_range", t.clientTrackingRange());
      o.addProperty("update_interval", t.updateInterval());
      o.addProperty("track_deltas", t.trackDeltas());
      if (DefaultAttributes.hasSupplier(t)) {
        AttributeSupplier sup =
            DefaultAttributes.getSupplier((EntityType<? extends LivingEntity>) t);
        JsonObject attrs = new JsonObject();
        for (Attribute a : BuiltInRegistries.ATTRIBUTE) {
          Holder<Attribute> h = BuiltInRegistries.ATTRIBUTE.wrapAsHolder(a);
          if (sup.hasAttribute(h)) {
            attrs.addProperty(BuiltInRegistries.ATTRIBUTE.getKey(a).toString(), sup.getBaseValue(h));
          }
        }
        o.add("attributes", attrs);
      }
      types.add(o);
    }
    JsonArray attributes = new JsonArray();
    for (int id = 0; id < BuiltInRegistries.ATTRIBUTE.size(); id++) {
      Attribute a = BuiltInRegistries.ATTRIBUTE.byId(id);
      JsonObject o = new JsonObject();
      o.addProperty("name", BuiltInRegistries.ATTRIBUTE.getKey(a).toString());
      o.addProperty("default", a.getDefaultValue());
      o.addProperty("syncable", a.isClientSyncable());
      attributes.add(o);
    }
    JsonObject root = new JsonObject();
    root.add("entity_types", types);
    root.add("attributes", attributes);
    return root;
  }
}

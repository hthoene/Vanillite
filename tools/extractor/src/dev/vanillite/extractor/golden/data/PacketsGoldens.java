// Copyright Hannes Thöne. Licensed under the Apache License, Version 2.0.
package dev.vanillite.extractor.golden.data;

import com.google.gson.JsonArray;
import com.google.gson.JsonElement;
import com.google.gson.JsonObject;
import java.util.ArrayList;
import java.util.List;
import java.util.TreeMap;
import net.minecraft.network.ProtocolInfo;
import net.minecraft.network.protocol.configuration.ConfigurationProtocols;
import net.minecraft.network.protocol.game.GameProtocols;
import net.minecraft.network.protocol.handshake.HandshakeProtocols;
import net.minecraft.network.protocol.login.LoginProtocols;
import net.minecraft.network.protocol.status.StatusProtocols;

/** Packet names in id order per connection state and direction. */
public final class PacketsGoldens {
  private PacketsGoldens() {}

  public static JsonElement extract() {
    List<ProtocolInfo.Details> all =
        List.of(
            HandshakeProtocols.SERVERBOUND_TEMPLATE.details(),
            StatusProtocols.CLIENTBOUND_TEMPLATE.details(),
            StatusProtocols.SERVERBOUND_TEMPLATE.details(),
            LoginProtocols.CLIENTBOUND_TEMPLATE.details(),
            LoginProtocols.SERVERBOUND_TEMPLATE.details(),
            ConfigurationProtocols.CLIENTBOUND_TEMPLATE.details(),
            ConfigurationProtocols.SERVERBOUND_TEMPLATE.details(),
            GameProtocols.CLIENTBOUND_TEMPLATE.details(),
            GameProtocols.SERVERBOUND_TEMPLATE.details());
    TreeMap<String, JsonObject> states = new TreeMap<>();
    for (ProtocolInfo.Details d : all) {
      List<String> names = new ArrayList<>();
      d.listPackets(
          (type, id) -> {
            while (names.size() <= id) {
              names.add(null);
            }
            names.set(id, type.id().toString());
          });
      JsonArray a = new JsonArray();
      names.forEach(a::add);
      states.computeIfAbsent(d.id().id(), k -> new JsonObject()).add(d.flow().id(), a);
    }
    JsonObject root = new JsonObject();
    states.forEach(root::add);
    return root;
  }
}

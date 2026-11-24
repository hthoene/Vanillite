// Copyright Hannes Thöne. Licensed under the Apache License, Version 2.0.
package dev.vanillite.extractor.golden.data;

import com.google.gson.JsonElement;
import com.google.gson.JsonObject;
import dev.vanillite.extractor.golden.ScenarioRecorder;
import java.util.ArrayList;
import java.util.Comparator;
import java.util.List;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.state.BlockState;

/**
 * Global block state ids per block: first id, count, default state and the hash of the state
 * names ({@code name[key=value,...]}, properties sorted by name) in id order.
 */
public final class BlockStatesGoldens {
  private BlockStatesGoldens() {}

  public static JsonElement extract() {
    JsonObject root = new JsonObject();
    for (Block block : BuiltInRegistries.BLOCK) {
      List<BlockState> states = new ArrayList<>(block.getStateDefinition().getPossibleStates());
      states.sort(Comparator.comparingInt(Block::getId));
      List<String> names = new ArrayList<>();
      for (BlockState s : states) {
        names.add(ScenarioRecorder.stateName(s));
      }
      JsonObject o = new JsonObject();
      o.addProperty("first", Block.getId(states.get(0)));
      o.addProperty("count", states.size());
      o.addProperty("default", Block.getId(block.defaultBlockState()));
      o.addProperty("hash", DataGoldens.hash(names));
      root.add(BuiltInRegistries.BLOCK.getKey(block).toString(), o);
    }
    return root;
  }
}

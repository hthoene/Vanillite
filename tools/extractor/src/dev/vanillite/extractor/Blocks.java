// Copyright Hannes Thöne. Licensed under the Apache License, Version 2.0.
package dev.vanillite.extractor;

import com.google.gson.JsonArray;
import com.google.gson.JsonObject;
import java.lang.reflect.Method;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import java.util.function.Supplier;
import net.minecraft.core.BlockPos;
import net.minecraft.core.Direction;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.world.item.Items;
import net.minecraft.world.level.EmptyBlockGetter;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.SupportType;
import net.minecraft.world.level.block.entity.BlockEntityType;
import net.minecraft.world.level.block.state.BlockBehaviour;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.material.Fluid;
import net.minecraft.world.phys.AABB;
import net.minecraft.world.phys.Vec3;
import net.minecraft.world.phys.shapes.CollisionContext;
import net.minecraft.world.phys.shapes.VoxelShape;

/** Block and block state facts plus a deduplicated shape table. */
final class Blocks {
  private static final EmptyBlockGetter LEVEL = EmptyBlockGetter.INSTANCE;
  private static final BlockPos ORIGIN = BlockPos.ZERO;
  /** A second position with a different offset, to tell offset-dependent shapes apart. */
  private static final BlockPos PROBE = new BlockPos(7, 0, 3);

  private final Map<String, Integer> shapeIds = new HashMap<>();
  private final JsonArray shapes = new JsonArray();

  private Blocks() {}

  static JsonObject extract() {
    return new Blocks().run();
  }

  private JsonObject run() {
    shapeId(net.minecraft.world.phys.shapes.Shapes.empty());
    shapeId(net.minecraft.world.phys.shapes.Shapes.block());

    JsonArray blocks = new JsonArray();
    for (int id = 0; id < BuiltInRegistries.BLOCK.size(); id++) {
      Block block = BuiltInRegistries.BLOCK.byId(id);
      JsonObject b = new JsonObject();
      b.addProperty("name", BuiltInRegistries.BLOCK.getKey(block).toString());
      b.addProperty("hardness", block.defaultDestroyTime());
      b.addProperty("blast_resistance", block.getExplosionResistance());
      b.addProperty("friction", block.getFriction());
      b.addProperty("speed_factor", block.getSpeedFactor());
      b.addProperty("jump_factor", block.getJumpFactor());
      b.addProperty("dynamic_shape", block.hasDynamicShape());
      Class<?> cls = block.getClass();
      while (cls.isAnonymousClass()) {
        cls = cls.getSuperclass();
      }
      b.addProperty("class", cls.getSimpleName());
      net.minecraft.world.level.block.SoundType sound = block.defaultBlockState().getSoundType();
      JsonObject snd = new JsonObject();
      snd.addProperty("volume", sound.getVolume());
      snd.addProperty("pitch", sound.getPitch());
      snd.addProperty("break", BuiltInRegistries.SOUND_EVENT.getKey(sound.getBreakSound()).toString());
      snd.addProperty("step", BuiltInRegistries.SOUND_EVENT.getKey(sound.getStepSound()).toString());
      snd.addProperty("place", BuiltInRegistries.SOUND_EVENT.getKey(sound.getPlaceSound()).toString());
      snd.addProperty("hit", BuiltInRegistries.SOUND_EVENT.getKey(sound.getHitSound()).toString());
      snd.addProperty("fall", BuiltInRegistries.SOUND_EVENT.getKey(sound.getFallSound()).toString());
      b.add("sound", snd);
      if (block.asItem() != Items.AIR) {
        b.addProperty("item", BuiltInRegistries.ITEM.getKey(block.asItem()).toString());
      }
      BlockState def = block.defaultBlockState();
      if (def.hasOffsetFunction()) {
        b.addProperty("max_horizontal_offset", (Float) callProtected(block, "getMaxHorizontalOffset"));
        b.addProperty("max_vertical_offset", (Float) callProtected(block, "getMaxVerticalOffset"));
      }
      for (BlockEntityType<?> type : BuiltInRegistries.BLOCK_ENTITY_TYPE) {
        if (type.isValid(def)) {
          b.addProperty("block_entity", BuiltInRegistries.BLOCK_ENTITY_TYPE.getKey(type).toString());
        }
      }
      JsonArray states = new JsonArray();
      for (BlockState state : block.getStateDefinition().getPossibleStates()) {
        states.add(state(state));
      }
      b.add("states", states);
      blocks.add(b);
    }
    JsonObject root = new JsonObject();
    root.add("blocks", blocks);
    root.add("shapes", shapes);
    return root;
  }

  private JsonObject state(BlockState s) {
    JsonObject o = new JsonObject();
    o.addProperty("id", Block.getId(s));
    o.addProperty("air", s.isAir());
    o.addProperty("solid", s.isSolid());
    o.addProperty("liquid", s.liquid());
    o.addProperty("can_occlude", s.canOcclude());
    o.addProperty("solid_render", s.isSolidRender());
    o.addProperty("propagates_skylight_down", s.propagatesSkylightDown());
    o.addProperty("use_shape_for_light_occlusion", s.useShapeForLightOcclusion());
    o.addProperty("large_collision_shape", s.hasLargeCollisionShape());
    o.addProperty("random_ticks", s.isRandomlyTicking());
    o.addProperty("replaceable", s.canBeReplaced());
    o.addProperty("has_block_entity", s.hasBlockEntity());
    o.addProperty("signal_source", s.isSignalSource());
    o.addProperty("analog_output", s.hasAnalogOutputSignal());
    o.addProperty("requires_correct_tool", s.requiresCorrectToolForDrops());
    o.addProperty("ignited_by_lava", s.ignitedByLava());
    o.addProperty("has_offset", s.hasOffsetFunction());
    safe(o, "redstone_conductor", () -> s.isRedstoneConductor(LEVEL, ORIGIN));
    safe(o, "collision_full_block", () -> s.isCollisionShapeFullBlock(LEVEL, ORIGIN));
    o.addProperty("light_emission", s.getLightEmission());
    o.addProperty("light_dampening", s.getLightDampening());
    safe(o, "destroy_speed", () -> s.getDestroySpeed(LEVEL, ORIGIN));
    o.addProperty("push_reaction", s.getPistonPushReaction().name().toLowerCase());
    safe(o, "map_color", () -> s.getMapColor(LEVEL, ORIGIN).id);
    o.addProperty("instrument", s.instrument().getSerializedName());
    o.addProperty("fluid_state", Fluid.FLUID_STATE_REGISTRY.getId(s.getFluidState()));
    o.addProperty("sturdy_full", sturdy(s, SupportType.FULL));
    o.addProperty("sturdy_center", sturdy(s, SupportType.CENTER));
    o.addProperty("sturdy_rigid", sturdy(s, SupportType.RIGID));

    CollisionContext ctx = CollisionContext.empty();
    shape(o, "collision", s, p -> s.getCollisionShape(LEVEL, p, ctx));
    shape(o, "outline", s, p -> s.getShape(LEVEL, p, ctx));
    shape(o, "interaction", s, p -> s.getInteractionShape(LEVEL, p));
    shape(o, "support", s, p -> s.getBlockSupportShape(LEVEL, p));
    shape(o, "visual", s, p -> s.getVisualShape(LEVEL, p, ctx));
    o.addProperty("occlusion", shapeId(s.getOcclusionShape()));
    return o;
  }

  private static int sturdy(BlockState s, SupportType type) {
    int mask = 0;
    for (Direction d : Direction.values()) {
      try {
        if (s.isFaceSturdy(LEVEL, ORIGIN, d, type)) {
          mask |= 1 << d.get3DDataValue();
        }
      } catch (RuntimeException e) {
        // Treated as not sturdy; such blocks query the world.
      }
    }
    return mask;
  }

  private interface ShapeAt {
    VoxelShape at(BlockPos pos);
  }

  /**
   * Records a shape id. For blocks with a random offset the shape is stored without the offset, and
   * "<kind>_offset" tells whether Vanilla moves the shape by the per-position offset.
   */
  private void shape(JsonObject o, String kind, BlockState s, ShapeAt f) {
    VoxelShape atOrigin;
    try {
      atOrigin = f.at(ORIGIN);
    } catch (RuntimeException e) {
      o.addProperty(kind, -1);
      o.addProperty(kind + "_offset", false);
      return;
    }
    if (s.hasOffsetFunction() && !atOrigin.isEmpty()) {
      Vec3 o0 = s.getOffset(ORIGIN);
      Vec3 o1 = s.getOffset(PROBE);
      VoxelShape atProbe = f.at(PROBE);
      if (!sameBoxes(atOrigin, atProbe) && sameBoxes(atOrigin.move(o1.subtract(o0)), atProbe)) {
        o.addProperty(kind, shapeId(atOrigin.move(o0.reverse())));
        o.addProperty(kind + "_offset", true);
        return;
      }
    }
    o.addProperty(kind, shapeId(atOrigin));
    o.addProperty(kind + "_offset", false);
  }

  private static boolean sameBoxes(VoxelShape a, VoxelShape b) {
    List<AABB> x = a.toAabbs();
    List<AABB> y = b.toAabbs();
    if (x.size() != y.size()) {
      return false;
    }
    for (int i = 0; i < x.size(); i++) {
      AABB p = x.get(i);
      AABB q = y.get(i);
      if (Math.abs(p.minX - q.minX) > 1e-7 || Math.abs(p.minY - q.minY) > 1e-7
          || Math.abs(p.minZ - q.minZ) > 1e-7 || Math.abs(p.maxX - q.maxX) > 1e-7
          || Math.abs(p.maxY - q.maxY) > 1e-7 || Math.abs(p.maxZ - q.maxZ) > 1e-7) {
        return false;
      }
    }
    return true;
  }

  private int shapeId(VoxelShape shape) {
    JsonArray boxes = new JsonArray();
    for (AABB b : shape.toAabbs()) {
      JsonArray box = new JsonArray();
      box.add(snap(b.minX));
      box.add(snap(b.minY));
      box.add(snap(b.minZ));
      box.add(snap(b.maxX));
      box.add(snap(b.maxY));
      box.add(snap(b.maxZ));
      boxes.add(box);
    }
    String key = boxes.toString();
    Integer id = shapeIds.get(key);
    if (id == null) {
      id = shapes.size();
      shapeIds.put(key, id);
      shapes.add(boxes);
    }
    return id;
  }

  /** Removes floating point noise left by moving a shape back by its offset. */
  private static double snap(double v) {
    double r = Math.rint(v * 4096.0) / 4096.0;
    return Math.abs(r - v) < 1e-7 ? r : v;
  }

  private static void safe(JsonObject o, String key, Supplier<Object> f) {
    Object v;
    try {
      v = f.get();
    } catch (RuntimeException e) {
      return;
    }
    if (v instanceof Boolean b) {
      o.addProperty(key, b);
    } else if (v instanceof Number n) {
      o.addProperty(key, n);
    }
  }

  private static Object callProtected(Block block, String name) {
    try {
      Method m = BlockBehaviour.class.getDeclaredMethod(name);
      m.setAccessible(true);
      return m.invoke(block);
    } catch (ReflectiveOperationException e) {
      throw new IllegalStateException(name, e);
    }
  }
}

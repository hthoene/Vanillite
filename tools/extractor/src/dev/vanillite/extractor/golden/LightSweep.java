// Copyright Hannes Thöne. Licensed under the Apache License, Version 2.0.
package dev.vanillite.extractor.golden;

import java.io.BufferedOutputStream;
import java.io.DataOutputStream;
import java.io.OutputStream;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.HashSet;
import java.util.List;
import java.util.Map;
import java.util.Random;
import java.util.Set;
import java.util.TreeSet;
import java.util.zip.GZIPOutputStream;
import net.minecraft.SharedConstants;
import net.minecraft.core.BlockPos;
import net.minecraft.core.Direction;
import net.minecraft.core.SectionPos;
import net.minecraft.server.Bootstrap;
import net.minecraft.world.level.BlockGetter;
import net.minecraft.world.level.ChunkPos;
import net.minecraft.world.level.LightLayer;
import net.minecraft.world.level.block.Block;
import net.minecraft.world.level.block.Blocks;
import net.minecraft.world.level.block.SlabBlock;
import net.minecraft.world.level.block.StairBlock;
import net.minecraft.world.level.block.entity.BlockEntity;
import net.minecraft.world.level.block.state.BlockState;
import net.minecraft.world.level.block.state.properties.Half;
import net.minecraft.world.level.block.state.properties.SlabType;
import net.minecraft.world.level.chunk.DataLayer;
import net.minecraft.world.level.chunk.LevelChunkSection;
import net.minecraft.world.level.chunk.LightChunk;
import net.minecraft.world.level.chunk.LightChunkGetter;
import net.minecraft.world.level.chunk.ProtoChunk;
import net.minecraft.world.level.chunk.status.ChunkStatus;
import net.minecraft.world.level.levelgen.Heightmap;
import net.minecraft.world.level.lighting.LevelLightEngine;
import net.minecraft.world.level.material.FluidState;

/**
 * Light sweep (#237): Vanilla 26.3 generates and lights whole areas of a world in-process, then
 * changes blocks in them by a script, and Vanillite's {@code vanilla_light_sweep} test lights
 * the same blocks and replays the same changes. Compared is the light a client receives. Not a
 * golden: nothing is committed, the output is written per run.
 *
 * <p>Usage: {@code java -cp <classes>:<server jar>:<libraries> dev.vanillite.extractor.golden.LightSweep
 * <out dir> <kind>:<seed>:<min chunk x>:<min chunk z>:<size>...} with the kinds of {@link
 * WorldSweep}. Each job writes {@code <out>/<kind>_<seed>_<x>_<z>_<size>.bin.gz}.
 *
 * <p>Generation: the area plus two rings gets its features first ({@link WorldSweep.Sweep}, a
 * fixed order), so nothing writes into a chunk while it is lit (a server's parallel generation
 * can leave stale sky light behind, differently in every run). A synchronous {@link
 * LevelLightEngine} over those chunks then does what {@code ThreadedLevelLightEngine} does: the
 * {@code initializeLight} step for all of them (one update), then the {@code light} step for
 * the area plus one ring (one update), so every chunk of the area is fully lit.
 *
 * <p>Block changes: a script of batches (ticks) seeded from the area: opaque and transparent
 * blocks, light sources, shafts, caves, roofs over large parts of the area (and their removal),
 * changes on chunk and section borders and at the height limits, and a chunk unloaded (its light
 * saved as {@code SerializableChunkData} does), blocks changed two chunks from it and the chunk
 * loaded again with its saved light. Blocks next to an unloaded chunk are not changed: a server
 * does not tick those chunks, and Vanilla's light engine keeps and reuses the light of unloaded
 * chunks there, which Vanillite does not keep. Each batch's blocks are set with {@code ProtoChunk.setBlockState} (section status, sky
 * light sources and {@code checkBlock}, as {@code LevelChunk.setBlockState}), then one {@code
 * runLightUpdates}, as a server tick.
 *
 * <p>File (gzip, big-endian {@code DataOutputStream}): int magic {@code 0x4C535750}, int format 1,
 * UTF kind, long seed, int min y, int height, boolean has sky light, int min chunk x, int min
 * chunk z, int size; the blocks of the area plus two rings (z outer, x inner; per section 4096
 * shorts {@code Block.getId} in y, z, x order); the light after generation (a snapshot); int
 * batch count, per batch UTF label, int op count, ops (byte 0: set, int x, y, z, int state; byte
 * 1: unload, int chunk x, z; byte 2: load, int chunk x, z) and the light after it (a snapshot).
 * A snapshot: int chunk count, per chunk int x, z and per light section (from the one below the
 * world to the one above it) the sky then the block layer as the client receives it: byte 0 not
 * sent, 1 empty (zeros), 2 data followed by 2048 bytes.
 */
public final class LightSweep {
  private static final int BATCHES = 32;

  private LightSweep() {}

  public static void main(String[] args) throws Exception {
    if (args.length < 2) {
      System.err.println("usage: LightSweep <out dir> <kind>:<seed>:<min chunk x>:<min chunk z>:<size>...");
      System.exit(2);
    }
    Path out = Path.of(args[0]);
    Files.createDirectories(out);
    SharedConstants.tryDetectVersion();
    Bootstrap.bootStrap();
    FeatureGoldens.init();
    for (int i = 1; i < args.length; i++) {
      String[] job = args[i].split(":");
      String kind = job[0];
      long seed = Long.parseLong(job[1]);
      int x0 = Integer.parseInt(job[2]);
      int z0 = Integer.parseInt(job[3]);
      int size = Integer.parseInt(job[4]);
      long start = System.nanoTime();
      Path file = out.resolve(kind + "_" + seed + "_" + x0 + "_" + z0 + "_" + size + ".bin.gz");
      try (OutputStream raw = Files.newOutputStream(file);
          DataOutputStream o = new DataOutputStream(new BufferedOutputStream(new GZIPOutputStream(raw, 1 << 16)))) {
        new Job(kind, seed, x0, z0, size).run(o);
      }
      System.out.printf("light sweep: %s seed %d area %d %d size %d in %.1f s%n", kind, seed, x0, z0, size,
          (System.nanoTime() - start) / 1e9);
      WorldSweep.clearWorlds();
    }
  }

  /** One area: generation, light, script. */
  private static final class Job {
    final String kind;
    final long seed;
    final int x0;
    final int z0;
    final int size;
    final WorldSweep.Sweep sweep;
    final boolean hasSky;
    final int minY;
    final int height;
    final Set<Long> unloaded = new HashSet<>();
    final Map<Long, DataLayer[][]> saved = new HashMap<>();
    LevelLightEngine engine;

    Job(String kind, long seed, int x0, int z0, int size) throws Exception {
      this.kind = kind;
      this.seed = seed;
      this.x0 = x0;
      this.z0 = z0;
      this.size = size;
      this.sweep = new WorldSweep.Sweep(kind, seed);
      this.hasSky = sweep.world.dimensionType.hasSkyLight();
      this.minY = sweep.dimension.minY();
      this.height = sweep.dimension.height();
    }

    boolean inArea(int x, int z, int ring) {
      return x >= x0 - ring && x < x0 + size + ring && z >= z0 - ring && z < z0 + size + ring;
    }

    ProtoChunk chunk(int x, int z) {
      if (!inArea(x, z, 2) || unloaded.contains(ChunkPos.pack(x, z))) {
        return null;
      }
      return sweep.get(x, z);
    }

    List<ProtoChunk> ring(int ring) {
      List<ProtoChunk> out = new ArrayList<>();
      for (int z = z0 - ring; z < z0 + size + ring; z++) {
        for (int x = x0 - ring; x < x0 + size + ring; x++) {
          out.add(sweep.get(x, z));
        }
      }
      return out;
    }

    void run(DataOutputStream o) throws Exception {
      for (int z = z0 - 2; z < z0 + size + 2; z++) {
        for (int x = x0 - 2; x < x0 + size + 2; x++) {
          sweep.features(x, z);
        }
      }
      BlockGetter level = new BlockGetter() {
        @Override
        public BlockEntity getBlockEntity(BlockPos pos) {
          return null;
        }

        @Override
        public BlockState getBlockState(BlockPos pos) {
          ProtoChunk chunk = chunk(pos.getX() >> 4, pos.getZ() >> 4);
          return chunk == null ? Blocks.VOID_AIR.defaultBlockState() : chunk.getBlockState(pos);
        }

        @Override
        public FluidState getFluidState(BlockPos pos) {
          return getBlockState(pos).getFluidState();
        }

        @Override
        public int getHeight() {
          return height;
        }

        @Override
        public int getMinY() {
          return minY;
        }
      };
      engine = new LevelLightEngine(new LightChunkGetter() {
        @Override
        public LightChunk getChunkForLighting(int x, int z) {
          return chunk(x, z);
        }

        @Override
        public BlockGetter getLevel() {
          return level;
        }
      }, true, hasSky);

      // `initializeLight` of the area plus two rings: sky light sources, then (one update) the
      // section status of every non-empty section, then light disabled and data not retained.
      List<ProtoChunk> initialized = ring(2);
      for (ProtoChunk chunk : initialized) {
        chunk.initializeLightSources();
        chunk.setLightEngine(engine);
        chunk.setPersistedStatus(ChunkStatus.INITIALIZE_LIGHT);
      }
      for (ProtoChunk chunk : initialized) {
        sectionStatus(chunk);
      }
      engine.runLightUpdates();
      for (ProtoChunk chunk : initialized) {
        engine.setLightEnabled(chunk.getPos(), false);
        engine.retainData(chunk.getPos(), false);
      }
      // `lightChunk` of the area plus one ring.
      List<ProtoChunk> lit = ring(1);
      for (ProtoChunk chunk : lit) {
        chunk.setLightCorrect(false);
        engine.propagateLightSources(chunk.getPos());
      }
      engine.runLightUpdates();
      for (ProtoChunk chunk : lit) {
        chunk.setLightCorrect(true);
        chunk.setPersistedStatus(ChunkStatus.LIGHT);
      }

      o.writeInt(0x4C535750);
      o.writeInt(1);
      o.writeUTF(kind);
      o.writeLong(seed);
      o.writeInt(minY);
      o.writeInt(height);
      o.writeBoolean(hasSky);
      o.writeInt(x0);
      o.writeInt(z0);
      o.writeInt(size);
      for (ProtoChunk chunk : initialized) {
        for (LevelChunkSection section : chunk.getSections()) {
          for (int y = 0; y < 16; y++) {
            for (int z = 0; z < 16; z++) {
              for (int x = 0; x < 16; x++) {
                o.writeShort(Block.getId(section.getBlockState(x, y, z)));
              }
            }
          }
        }
      }
      List<ChunkPos> area = new ArrayList<>();
      for (ProtoChunk chunk : ring(0)) {
        area.add(chunk.getPos());
      }
      snapshot(o, area);

      Script script = new Script(new Random(seed ^ (31L * x0 + 1_000_003L * z0) ^ kind.hashCode()));
      List<Batch> batches = script.batches();
      o.writeInt(batches.size());
      for (int b = 0; b < batches.size(); b++) {
        Batch batch = batches.get(b);
        o.writeUTF(batch.label);
        o.writeInt(batch.ops.size());
        Set<Long> touched = new TreeSet<>();
        for (Op op : batch.ops) {
          o.writeByte(op.type);
          if (op.type == 0) {
            o.writeInt(op.pos.getX());
            o.writeInt(op.pos.getY());
            o.writeInt(op.pos.getZ());
            o.writeInt(Block.getId(op.state));
            ProtoChunk chunk = chunk(op.pos.getX() >> 4, op.pos.getZ() >> 4);
            if (chunk != null) {
              chunk.setBlockState(op.pos, op.state, 0);
              for (int dz = -1; dz <= 1; dz++) {
                for (int dx = -1; dx <= 1; dx++) {
                  touched.add(ChunkPos.pack((op.pos.getX() >> 4) + dx, (op.pos.getZ() >> 4) + dz));
                }
              }
            }
          } else {
            o.writeInt(op.chunk.x());
            o.writeInt(op.chunk.z());
            if (op.type == 1) {
              unload(op.chunk);
            } else {
              load(op.chunk);
            }
          }
        }
        engine.runLightUpdates();
        List<ChunkPos> changed = new ArrayList<>();
        for (long packed : touched) {
          ChunkPos pos = ChunkPos.unpack(packed);
          if (inArea(pos.x(), pos.z(), 0) && !unloaded.contains(packed)) {
            changed.add(pos);
          }
        }
        if (b == batches.size() - 1) {
          changed = new ArrayList<>();
          for (ChunkPos pos : area) {
            if (!unloaded.contains(ChunkPos.pack(pos.x(), pos.z()))) {
              changed.add(pos);
            }
          }
        }
        snapshot(o, changed);
      }
    }

    void sectionStatus(ProtoChunk chunk) {
      LevelChunkSection[] sections = chunk.getSections();
      for (int index = 0; index < sections.length; index++) {
        if (!sections[index].hasOnlyAir()) {
          engine.updateSectionStatus(SectionPos.of(chunk.getPos(), chunk.getSectionYFromSectionIndex(index)), false);
        }
      }
    }

    /**
     * {@code ChunkMap} saving and unloading a chunk: its light as {@code SerializableChunkData}
     * stores it, then {@code ThreadedLevelLightEngine.updateChunkStatus}.
     */
    void unload(ChunkPos pos) {
      int count = engine.getLightSectionCount();
      DataLayer[][] layers = new DataLayer[2][count];
      for (int i = 0; i < count; i++) {
        SectionPos section = SectionPos.of(pos, engine.getMinLightSection() + i);
        DataLayer block = engine.getLayerListener(LightLayer.BLOCK).getDataLayerData(section);
        DataLayer sky = engine.getLayerListener(LightLayer.SKY).getDataLayerData(section);
        layers[0][i] = block != null && !block.isEmpty() ? block.copy() : null;
        layers[1][i] = sky != null && !sky.isEmpty() ? sky.copy() : null;
      }
      saved.put(ChunkPos.pack(pos.x(), pos.z()), layers);
      unloaded.add(ChunkPos.pack(pos.x(), pos.z()));
      engine.retainData(pos, false);
      engine.setLightEnabled(pos, false);
      for (int i = 0; i < count; i++) {
        engine.queueSectionData(LightLayer.BLOCK, SectionPos.of(pos, engine.getMinLightSection() + i), null);
        engine.queueSectionData(LightLayer.SKY, SectionPos.of(pos, engine.getMinLightSection() + i), null);
      }
      for (int y = minY >> 4; y <= (minY + height - 1) >> 4; y++) {
        engine.updateSectionStatus(SectionPos.of(pos, y), true);
      }
    }

    /**
     * Loading a saved chunk: {@code SerializableChunkData.read} queues its light, then the
     * {@code initializeLight} and {@code light} steps of a lit chunk.
     */
    void load(ChunkPos pos) {
      long packed = ChunkPos.pack(pos.x(), pos.z());
      unloaded.remove(packed);
      DataLayer[][] layers = saved.remove(packed);
      ProtoChunk chunk = sweep.get(pos.x(), pos.z());
      boolean any = false;
      for (int i = 0; i < layers[0].length; i++) {
        boolean block = layers[0][i] != null;
        boolean sky = hasSky && layers[1][i] != null;
        if (!block && !sky) {
          continue;
        }
        if (!any) {
          engine.retainData(pos, true);
          any = true;
        }
        SectionPos section = SectionPos.of(pos, engine.getMinLightSection() + i);
        if (block) {
          engine.queueSectionData(LightLayer.BLOCK, section, layers[0][i]);
        }
        if (sky) {
          engine.queueSectionData(LightLayer.SKY, section, layers[1][i]);
        }
      }
      chunk.initializeLightSources();
      sectionStatus(chunk);
      engine.runLightUpdates();
      engine.setLightEnabled(pos, true);
      engine.retainData(pos, false);
    }

    /** Writes the light the client receives for these chunks. */
    void snapshot(DataOutputStream o, List<ChunkPos> chunks) throws Exception {
      o.writeInt(chunks.size());
      for (ChunkPos pos : chunks) {
        o.writeInt(pos.x());
        o.writeInt(pos.z());
        for (int i = 0; i < engine.getLightSectionCount(); i++) {
          SectionPos section = SectionPos.of(pos, engine.getMinLightSection() + i);
          for (LightLayer layer : new LightLayer[] {LightLayer.SKY, LightLayer.BLOCK}) {
            DataLayer data = engine.getLayerListener(layer).getDataLayerData(section);
            if (data == null) {
              o.writeByte(0);
            } else if (data.isEmpty()) {
              o.writeByte(1);
            } else {
              o.writeByte(2);
              o.write(data.copy().getData());
            }
          }
        }
      }
    }

    record Op(int type, BlockPos pos, BlockState state, ChunkPos chunk) {
      static Op set(BlockPos pos, BlockState state) {
        return new Op(0, pos.immutable(), state, null);
      }
    }

    record Batch(String label, List<Op> ops) {}

    /** The scripted block changes of the area. */
    final class Script {
      final Random random;
      final List<Batch> batches = new ArrayList<>();
      BlockPos roofMin;
      BlockPos roofMax;

      Script(Random random) {
        this.random = random;
      }

      static final BlockState[] OPAQUE = {
        Blocks.STONE.defaultBlockState(), Blocks.DIRT.defaultBlockState(), Blocks.OAK_PLANKS.defaultBlockState(),
        Blocks.NETHERRACK.defaultBlockState(), Blocks.TINTED_GLASS.defaultBlockState(),
      };

      static final BlockState[] SOURCES = {
        Blocks.TORCH.defaultBlockState(), Blocks.GLOWSTONE.defaultBlockState(), Blocks.LAVA.defaultBlockState(),
        Blocks.SEA_LANTERN.defaultBlockState(), Blocks.SHROOMLIGHT.defaultBlockState(),
        Blocks.JACK_O_LANTERN.defaultBlockState(), Blocks.SOUL_TORCH.defaultBlockState(),
        Blocks.REDSTONE_TORCH.defaultBlockState(), Blocks.MAGMA_BLOCK.defaultBlockState(),
        Blocks.LANTERN.defaultBlockState(), Blocks.END_ROD.defaultBlockState(),
        Blocks.BEACON.defaultBlockState(), Blocks.OCHRE_FROGLIGHT.defaultBlockState(),
      };

      BlockState transparent() {
        return switch (random.nextInt(16)) {
          case 0 -> Blocks.OAK_LEAVES.defaultBlockState();
          case 1 -> Blocks.WATER.defaultBlockState();
          case 2 -> Blocks.STONE_SLAB.defaultBlockState().setValue(SlabBlock.TYPE, SlabType.BOTTOM);
          case 3 -> Blocks.STONE_SLAB.defaultBlockState().setValue(SlabBlock.TYPE, SlabType.TOP);
          case 4, 5 -> Blocks.OAK_STAIRS.defaultBlockState()
              .setValue(StairBlock.FACING, Direction.from2DDataValue(random.nextInt(4)))
              .setValue(StairBlock.HALF, random.nextBoolean() ? Half.TOP : Half.BOTTOM);
          case 6 -> Blocks.ICE.defaultBlockState();
          case 7 -> Blocks.GLASS.defaultBlockState();
          case 8 -> Blocks.COBWEB.defaultBlockState();
          case 9 -> Blocks.SLIME_BLOCK.defaultBlockState();
          case 10 -> Blocks.HONEY_BLOCK.defaultBlockState();
          case 11 -> Blocks.DIRT_PATH.defaultBlockState();
          case 12 -> Blocks.FARMLAND.defaultBlockState();
          case 13 -> Blocks.SNOW.defaultBlockState();
          case 14 -> Blocks.SPRUCE_LEAVES.defaultBlockState();
          default -> Blocks.WATER.defaultBlockState();
        };
      }

      int surface(int x, int z) {
        ProtoChunk chunk = sweep.get(x >> 4, z >> 4);
        return chunk.getHeight(Heightmap.Types.WORLD_SURFACE, x & 15, z & 15) + 1;
      }

      int maxY() {
        return minY + height - 1;
      }

      int clampY(int y) {
        return Math.max(minY, Math.min(maxY(), y));
      }

      /** A random column of the area, x then z. */
      int[] column() {
        return new int[] {x0 * 16 + random.nextInt(size * 16), z0 * 16 + random.nextInt(size * 16)};
      }

      /** A random column on a chunk border of the area. */
      int[] borderColumn() {
        int[] c = column();
        if (random.nextBoolean()) {
          c[0] = (c[0] & ~15) | (random.nextBoolean() ? 0 : 15);
        }
        if (random.nextBoolean() || (c[0] & 15) % 15 != 0) {
          c[1] = (c[1] & ~15) | (random.nextBoolean() ? 0 : 15);
        }
        return c;
      }

      BlockPos near(int[] c, int below, int above) {
        int s = surface(c[0], c[1]);
        return new BlockPos(c[0], clampY(s - below + random.nextInt(below + above + 1)), c[1]);
      }

      void add(String label, List<Op> ops) {
        batches.add(new Batch(label, ops));
      }

      List<Op> scatter(int count, int below, int above, java.util.function.Supplier<BlockState> state) {
        List<Op> ops = new ArrayList<>();
        int[] center = column();
        for (int i = 0; i < count; i++) {
          int[] c = {clampX(center[0] + random.nextInt(9) - 4), clampZ(center[1] + random.nextInt(9) - 4)};
          ops.add(Op.set(near(c, below, above), state.get()));
        }
        return ops;
      }

      int clampX(int x) {
        return Math.max(x0 * 16, Math.min((x0 + size) * 16 - 1, x));
      }

      int clampZ(int z) {
        return Math.max(z0 * 16, Math.min((z0 + size) * 16 - 1, z));
      }

      List<Op> box(int xa, int ya, int za, int xb, int yb, int zb, BlockState state) {
        List<Op> ops = new ArrayList<>();
        for (int y = clampY(ya); y <= clampY(yb); y++) {
          for (int z = clampZ(za); z <= clampZ(zb); z++) {
            for (int x = clampX(xa); x <= clampX(xb); x++) {
              ops.add(Op.set(new BlockPos(x, y, z), state));
            }
          }
        }
        return ops;
      }

      List<Batch> batches() {
        BlockState air = Blocks.AIR.defaultBlockState();
        add("opaque", scatter(12, 6, 4, () -> OPAQUE[random.nextInt(OPAQUE.length)]));
        add("remove", scatter(12, 12, 0, () -> air));
        add("sources", scatter(6, 8, 3, () -> SOURCES[random.nextInt(SOURCES.length)]));
        add("transparent", scatter(16, 4, 4, this::transparent));
        {
          int[] c = column();
          int s = surface(c[0], c[1]);
          int depth = 20 + random.nextInt(60);
          int w = random.nextInt(2);
          add("shaft", box(c[0], s - depth, c[1], c[0] + w, s, c[1] + w, air));
        }
        {
          int[] c = column();
          int s = surface(c[0], c[1]);
          int y = minY + 4 + random.nextInt(Math.max(1, s - minY - 16));
          int dx = 3 + random.nextInt(7);
          int dy = 3 + random.nextInt(4);
          int dz = 3 + random.nextInt(7);
          List<Op> ops = box(c[0], y, c[1], c[0] + dx, y + dy, c[1] + dz, air);
          ops.add(Op.set(new BlockPos(clampX(c[0] + dx / 2), clampY(y), clampZ(c[1] + dz / 2)),
              Blocks.TORCH.defaultBlockState()));
          add("cave", ops);
        }
        {
          int[] c = column();
          int w = 8 + random.nextInt(25);
          int d = 8 + random.nextInt(25);
          int top = minY;
          for (int z = c[1]; z < c[1] + d; z++) {
            for (int x = c[0]; x < c[0] + w; x++) {
              top = Math.max(top, surface(clampX(x), clampZ(z)));
            }
          }
          int y = clampY(top + 2 + random.nextInt(4));
          BlockState roof = switch (random.nextInt(4)) {
            case 0 -> Blocks.GLASS.defaultBlockState();
            case 1 -> Blocks.OAK_LEAVES.defaultBlockState();
            default -> Blocks.STONE.defaultBlockState();
          };
          roofMin = new BlockPos(c[0], y, c[1]);
          roofMax = new BlockPos(c[0] + w - 1, y, c[1] + d - 1);
          add("roof", box(c[0], y, c[1], c[0] + w - 1, y, c[1] + d - 1, roof));
        }
        add("roof sources", scatter(4, 3, 0, () -> SOURCES[random.nextInt(SOURCES.length)]));
        add("remove roof", box(roofMin.getX(), roofMin.getY(), roofMin.getZ(), roofMax.getX(), roofMax.getY(),
            roofMax.getZ(), air));
        for (int i = 0; i < 3; i++) {
          List<Op> ops = new ArrayList<>();
          for (int j = 0; j < 8; j++) {
            int[] c = borderColumn();
            BlockPos p = near(c, 16, 3);
            if (random.nextBoolean()) {
              // A section border.
              p = new BlockPos(p.getX(), clampY((p.getY() & ~15) | (random.nextBoolean() ? 0 : 15)), p.getZ());
            }
            BlockState state = switch (random.nextInt(4)) {
              case 0 -> air;
              case 1 -> OPAQUE[random.nextInt(OPAQUE.length)];
              case 2 -> SOURCES[random.nextInt(SOURCES.length)];
              default -> transparent();
            };
            ops.add(Op.set(p, state));
          }
          add("borders", ops);
        }
        {
          List<Op> ops = new ArrayList<>();
          for (int j = 0; j < 6; j++) {
            int[] c = column();
            ops.add(Op.set(new BlockPos(c[0], maxY() - random.nextInt(2), c[1]), random.nextBoolean()
                ? Blocks.GLOWSTONE.defaultBlockState() : Blocks.STONE.defaultBlockState()));
            c = column();
            ops.add(Op.set(new BlockPos(c[0], minY + random.nextInt(2), c[1]), random.nextBoolean()
                ? Blocks.GLOWSTONE.defaultBlockState() : air));
          }
          add("height limits", ops);
        }
        {
          // A platform at the top of the world, then gone again.
          int[] c = column();
          add("top platform", box(c[0], maxY() - 1, c[1], c[0] + 10, maxY() - 1, c[1] + 10,
              Blocks.STONE.defaultBlockState()));
          add("top platform gone", box(c[0], maxY() - 1, c[1], c[0] + 10, maxY() - 1, c[1] + 10, air));
        }
        if (size >= 3) {
          // A chunk away, changes two chunks from it (a server's chunks next to an unloaded one
          // do not tick, so blocks change one chunk further in), loaded again with its saved
          // light, changes on its border.
          int ux = x0 + random.nextInt(size - 2);
          int uz = z0 + random.nextInt(size);
          ChunkPos u = new ChunkPos(ux, uz);
          add("unload", List.of(new Op(1, null, null, u)));
          List<Op> ops = new ArrayList<>();
          for (int j = 0; j < 10; j++) {
            int z = uz * 16 + random.nextInt(16);
            int x = (ux + 2) * 16 + random.nextInt(4);
            BlockPos p = near(new int[] {x, z}, 10, 2);
            ops.add(Op.set(p, switch (random.nextInt(3)) {
              case 0 -> air;
              case 1 -> SOURCES[random.nextInt(SOURCES.length)];
              default -> OPAQUE[random.nextInt(OPAQUE.length)];
            }));
          }
          add("near unloaded", ops);
          add("load", List.of(new Op(2, null, null, u)));
          ops = new ArrayList<>();
          for (int j = 0; j < 6; j++) {
            int z = uz * 16 + random.nextInt(16);
            int x = ux * 16 + 13 + random.nextInt(3);
            ops.add(Op.set(near(new int[] {x, z}, 10, 2), random.nextBoolean() ? air
                : SOURCES[random.nextInt(SOURCES.length)]));
          }
          add("beside reloaded", ops);
        }
        while (batches.size() < BATCHES) {
          switch (random.nextInt(4)) {
            case 0 -> add("opaque", scatter(10, 8, 4, () -> OPAQUE[random.nextInt(OPAQUE.length)]));
            case 1 -> add("remove", scatter(10, 16, 0, () -> air));
            case 2 -> add("sources", scatter(5, 10, 3, () -> SOURCES[random.nextInt(SOURCES.length)]));
            default -> add("transparent", scatter(12, 6, 4, this::transparent));
          }
        }
        return batches;
      }
    }
  }
}

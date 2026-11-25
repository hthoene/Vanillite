//! `WorldGenRegion`: the 3×3 chunks a feature stage step may read and write.

use vanillite_base::random::PositionalRandomFactory;
use vanillite_base::{BlockPos, ChunkPos};
use vanillite_chunk::{BiomeId, HeightmapKind};
use vanillite_data::block::{BlockId, BlockState};
use vanillite_data::fluid::FluidState;
use vanillite_nbt::{Compound, Tag};

use crate::anchor::WorldGenerationContext;
use crate::biome::BiomeManager;
use crate::proto::{FluidTickKind, ProtoChunk};
use crate::random::AnyRandom;
use crate::random_state::RandomState;
use crate::structure::ReferencedStarts;

/// `Block.UPDATE_*` flags of `setBlock`. Generation only looks at [`flags::KNOWN_SHAPE`].
pub mod flags {
    /// `UPDATE_NEIGHBORS`.
    pub const NEIGHBORS: i32 = 1;
    /// `UPDATE_CLIENTS`.
    pub const CLIENTS: i32 = 2;
    /// `UPDATE_ALL` (`UPDATE_NEIGHBORS | UPDATE_CLIENTS`), as `setBlockAndUpdate` uses.
    pub const ALL: i32 = 3;
    /// `UPDATE_INVISIBLE`.
    pub const INVISIBLE: i32 = 4;
    /// `UPDATE_KNOWN_SHAPE`: no post-processing mark.
    pub const KNOWN_SHAPE: i32 = 16;
}

/// A live world a feature places into (`ServerLevel` as the feature's level, e.g. a growing
/// sapling): writes and scheduled ticks go to the world with all their side effects.
pub trait LiveLevel {
    /// `setBlock(pos, state, flags)` in the world, with neighbour and shape updates.
    fn set_block(&mut self, pos: BlockPos, state: BlockState, flags: i32) -> bool;
    /// `scheduleTick(pos, block, delay)`.
    fn schedule_block_tick(&mut self, pos: BlockPos, block: BlockId, delay: i32);
    /// `scheduleTick(pos, fluid, delay)`.
    fn schedule_fluid_tick(&mut self, pos: BlockPos, fluid: FluidTickKind, delay: i32);
    /// The blocks the world changed since the last call, with their states now.
    fn take_changes(&mut self, out: &mut Vec<(BlockPos, BlockState)>);
}

/// The chunks around a centre chunk during the features stage, with Vanilla's
/// `WorldGenRegion` semantics: reads anywhere in the region, writes within the write radius,
/// `VOID_AIR` outside the build height.
pub struct WorldGenRegion<'a> {
    center: ChunkPos,
    radius: i32,
    write_radius: i32,
    /// Chunks row-major (`dz` outer, `dx` inner) over `-radius..=radius`.
    chunks: Vec<&'a mut ProtoChunk>,
    seed: i64,
    sea_level: i32,
    min_y: i32,
    height: i32,
    biome_manager: BiomeManager,
    random: AnyRandom,
    generation_context: WorldGenerationContext,
    has_sky_light: bool,
    /// The structure starts the centre chunk references (Vanilla reads them through the
    /// region's `StructureManager`).
    structure_starts: ReferencedStarts,
    /// Every successful `setBlock` in call order (position, state, flags), when recorded.
    journal: Option<Vec<(BlockPos, BlockState, i32)>>,
    /// Live mode: the world the writes go to; the chunks mirror its blocks.
    live: Option<Box<dyn LiveLevel + 'a>>,
    /// Scratch list of the live world's changes.
    live_changes: Vec<(BlockPos, BlockState)>,
}

impl<'a> WorldGenRegion<'a> {
    /// A region over `chunks` (row-major, `dz` outer, `dx` inner, side `2 * radius + 1`) around
    /// the middle chunk. The features stage uses radius 1 and write radius 1.
    pub fn new(
        chunks: Vec<&'a mut ProtoChunk>,
        radius: i32,
        write_radius: i32,
        random_state: &RandomState,
        sea_level: i32,
    ) -> Self {
        let side = (2 * radius + 1) as usize;
        assert_eq!(chunks.len(), side * side, "region needs (2r+1)² chunks");
        let middle = &chunks[chunks.len() / 2];
        let center = middle.pos();
        let (min_y, height) = (middle.min_y(), middle.height());
        let seed = random_state.seed();
        let random = random_state
            .random_factory("minecraft:worldgen_region_random")
            .at(center.min_block_x(), 0, center.min_block_z());
        Self {
            center,
            radius,
            write_radius,
            chunks,
            seed,
            sea_level,
            min_y,
            height,
            biome_manager: BiomeManager::for_world_seed(seed),
            random,
            generation_context: WorldGenerationContext::new(min_y, height, sea_level),
            has_sky_light: true,
            structure_starts: ReferencedStarts::empty(),
            journal: None,
            live: None,
            live_changes: Vec::new(),
        }
    }

    /// Records every following successful `setBlock` (`/place` replays them on a live world).
    pub fn record_writes(&mut self) {
        self.journal = Some(Vec::new());
    }

    /// The recorded writes in call order: position, state and update flags.
    pub fn take_writes(&mut self) -> Vec<(BlockPos, BlockState, i32)> {
        self.journal.take().unwrap_or_default()
    }

    /// Live mode: features write into `live` (no write zone, no placeholders, no
    /// post-processing marks) and the chunks, copies of the world's, follow every block the
    /// world changes, so later reads see the world as it is.
    pub fn set_live(&mut self, live: Box<dyn LiveLevel + 'a>) {
        self.live = Some(live);
    }

    /// The live world in live mode.
    pub fn live_level(&mut self) -> Option<&mut (dyn LiveLevel + 'a)> {
        self.live.as_deref_mut()
    }

    /// Writes the blocks the live world changed into the copies.
    fn mirror_live(&mut self) {
        let Some(live) = self.live.as_deref_mut() else {
            return;
        };
        let mut changes = std::mem::take(&mut self.live_changes);
        changes.clear();
        live.take_changes(&mut changes);
        for &(pos, state) in &changes {
            if self.has_chunk(pos.x >> 4, pos.z >> 4) {
                self.chunk_mut(pos).set_block(pos.x, pos.y, pos.z, state);
            }
        }
        self.live_changes = changes;
    }

    /// Sets the structure starts the centre chunk references.
    pub fn set_structure_starts(&mut self, starts: ReferencedStarts) {
        self.structure_starts = starts;
    }

    /// `StructureManager.startsForStructure(sectionPos, ...)` of the centre chunk: the starts it
    /// references.
    pub fn structure_starts(&self) -> &ReferencedStarts {
        &self.structure_starts
    }

    /// Sets whether the dimension has sky light (`dimensionType().hasSkyLight()`); default true.
    pub fn set_has_sky_light(&mut self, has_sky_light: bool) {
        self.has_sky_light = has_sky_light;
    }

    /// `dimensionType().hasSkyLight()`.
    pub fn has_sky_light(&self) -> bool {
        self.has_sky_light
    }

    /// Sets the generator's `WorldGenerationContext` (by default the level's height range and
    /// the sea level, which is right whenever the generator covers the whole level).
    pub fn set_generation_context(&mut self, context: WorldGenerationContext) {
        self.generation_context = context;
    }

    /// `WorldGenerationContext.of(level)`: the generator's height range clamped to the level's,
    /// and the generator's sea level.
    pub fn generation_context(&self) -> WorldGenerationContext {
        self.generation_context
    }

    /// The centre chunk position.
    pub fn center(&self) -> ChunkPos {
        self.center
    }

    /// The world seed (`getSeed`).
    pub fn seed(&self) -> i64 {
        self.seed
    }

    /// The generator's sea level (`ChunkGenerator.getSeaLevel`).
    pub fn sea_level(&self) -> i32 {
        self.sea_level
    }

    /// `getMinY()`.
    pub fn min_y(&self) -> i32 {
        self.min_y
    }

    /// `getHeight()`.
    pub fn height(&self) -> i32 {
        self.height
    }

    /// `getMaxY()`.
    pub fn max_y(&self) -> i32 {
        self.min_y + self.height - 1
    }

    /// `isOutsideBuildHeight(y)`.
    pub fn is_outside_build_height(&self, y: i32) -> bool {
        y < self.min_y || y > self.max_y()
    }

    /// `getRandom()`: the region's own generator, seeded from the centre chunk.
    pub fn random(&mut self) -> &mut AnyRandom {
        &mut self.random
    }

    fn index(&self, chunk_x: i32, chunk_z: i32) -> usize {
        let dx = chunk_x - self.center.x;
        let dz = chunk_z - self.center.z;
        assert!(
            dx.abs() <= self.radius && dz.abs() <= self.radius,
            "requested chunk [{chunk_x}, {chunk_z}] unavailable while generating {:?}",
            self.center
        );
        let side = 2 * self.radius + 1;
        ((dz + self.radius) * side + dx + self.radius) as usize
    }

    /// `hasChunk(x, z)`.
    pub fn has_chunk(&self, chunk_x: i32, chunk_z: i32) -> bool {
        (chunk_x - self.center.x).abs() <= self.radius
            && (chunk_z - self.center.z).abs() <= self.radius
    }

    /// The chunk containing a block.
    pub fn chunk(&self, pos: BlockPos) -> &ProtoChunk {
        self.chunks[self.index(pos.x >> 4, pos.z >> 4)]
    }

    /// The chunk containing a block, mutable.
    pub fn chunk_mut(&mut self, pos: BlockPos) -> &mut ProtoChunk {
        let index = self.index(pos.x >> 4, pos.z >> 4);
        self.chunks[index]
    }

    /// `getBlockState(pos)`.
    #[inline]
    pub fn block(&self, pos: BlockPos) -> BlockState {
        self.chunk(pos).block_or_void(pos.x, pos.y, pos.z)
    }

    /// `getFluidState(pos)`.
    #[inline]
    pub fn fluid(&self, pos: BlockPos) -> FluidState {
        self.block(pos).fluid_state()
    }

    /// `isEmptyBlock(pos)`: the block is air (any air).
    #[inline]
    pub fn is_empty_block(&self, pos: BlockPos) -> bool {
        self.block(pos).is_air()
    }

    /// `isStateAtPosition(pos, predicate)`.
    #[inline]
    pub fn is_state_at(&self, pos: BlockPos, predicate: impl FnOnce(BlockState) -> bool) -> bool {
        predicate(self.block(pos))
    }

    /// `getHeight(type, x, z)`: the first free Y above the highest matching block.
    pub fn height_at(&self, kind: HeightmapKind, x: i32, z: i32) -> i32 {
        self.chunk(BlockPos::new(x, 0, z))
            .heightmap(kind, (x & 15) as usize, (z & 15) as usize)
            + 1
    }

    /// `getHeightmapPos(type, pos)`.
    pub fn heightmap_pos(&self, kind: HeightmapKind, pos: BlockPos) -> BlockPos {
        BlockPos::new(pos.x, self.height_at(kind, pos.x, pos.z), pos.z)
    }

    /// `getNoiseBiome(quartX, quartY, quartZ)` from the chunk holding the quart.
    pub fn noise_biome(&self, quart_x: i32, quart_y: i32, quart_z: i32) -> BiomeId {
        self.chunks[self.index(quart_x >> 2, quart_z >> 2)].noise_biome(quart_x, quart_y, quart_z)
    }

    /// `getBiome(pos)` through the region's `BiomeManager`.
    pub fn biome(&self, pos: BlockPos) -> BiomeId {
        self.biome_manager
            .get_biome(pos.x, pos.y, pos.z, &mut |qx, qy, qz| {
                self.noise_biome(qx, qy, qz)
            })
    }

    /// `isWithinWriteZone(pos)`.
    pub fn is_within_write_zone(&self, pos: BlockPos) -> bool {
        ((pos.x >> 4) - self.center.x).abs() <= self.write_radius
            && ((pos.z >> 4) - self.center.z).abs() <= self.write_radius
    }

    /// `ensureCanWrite(pos)`.
    pub fn ensure_can_write(&self, pos: BlockPos) -> bool {
        self.is_within_write_zone(pos)
    }

    /// `setBlock(pos, state, flags)`: writes the block and its heightmaps, creates (unless a
    /// block entity was already created there) or removes the block entity placeholder and, unless `flags` has [`flags::KNOWN_SHAPE`], marks the
    /// state's post-processing position. Returns `false` outside the write zone.
    pub fn set_block(&mut self, pos: BlockPos, state: BlockState, update_flags: i32) -> bool {
        if let Some(live) = self.live.as_deref_mut() {
            let changed = live.set_block(pos, state, update_flags);
            self.mirror_live();
            return changed;
        }
        if !self.ensure_can_write(pos) {
            return false;
        }
        let chunk = self.chunk_mut(pos);
        let old = chunk.set_block(pos.x, pos.y, pos.z, state);
        let outside = chunk.is_outside(pos.y);
        if has_block_entity(state) {
            // `ChunkAccess.setBlockEntityNbt`: the placeholder is only stored where no block
            // entity was created yet (a created one stays, whatever the new block is).
            let created = chunk.block_entity(pos).is_some_and(
                |tag| !matches!(tag.get("id"), Some(Tag::String(id)) if id == "DUMMY"),
            );
            if !outside && !created {
                let mut tag = Compound::new();
                tag.insert("x", Tag::Int(pos.x));
                tag.insert("y", Tag::Int(pos.y));
                tag.insert("z", Tag::Int(pos.z));
                tag.insert("id", Tag::String("DUMMY".into()));
                chunk.set_block_entity(pos, tag);
            }
        } else if has_block_entity(old) {
            chunk.remove_block_entity(pos);
        }
        if update_flags & flags::KNOWN_SHAPE == 0
            && let Some(post) = crate::behaviour::post_process_pos(self, state, pos)
        {
            self.mark_post_processing(post);
        }
        if let Some(journal) = &mut self.journal {
            journal.push((pos, state, update_flags));
        }
        true
    }

    /// `setBlockAndUpdate(pos, state)`: [`Self::set_block`] with [`flags::ALL`].
    pub fn set_block_and_update(&mut self, pos: BlockPos, state: BlockState) -> bool {
        self.set_block(pos, state, flags::ALL)
    }

    /// `removeBlock(pos, false)`: the region sets plain air (no fluid kept).
    pub fn remove_block(&mut self, pos: BlockPos) -> bool {
        self.set_block(pos, BlockState::AIR, flags::ALL)
    }

    /// `destroyBlock(pos, false)`: the region sets plain air and drops nothing.
    pub fn destroy_block(&mut self, pos: BlockPos) -> bool {
        if self.block(pos).is_air() {
            return false;
        }
        self.set_block(pos, BlockState::AIR, flags::ALL)
    }

    /// `addFreshEntity(entity)`: stores the entity (its saved NBT with `id` and `Pos`, see
    /// [`entity_tag`]) in the chunk holding its block position.
    pub fn add_fresh_entity(&mut self, entity: Compound) -> bool {
        let [x, _, z] = entity_pos(&entity);
        let pos = BlockPos::new(x.floor() as i32, 0, z.floor() as i32);
        self.chunk_mut(pos).add_entity(entity);
        true
    }

    /// `ChunkAccess.markPosForPostProcessing(pos)` on the chunk holding `pos`.
    pub fn mark_post_processing(&mut self, pos: BlockPos) {
        self.chunk_mut(pos)
            .mark_post_processing(pos.x, pos.y, pos.z);
    }

    /// `getBlockEntity(pos)`: the block entity data, created from a placeholder on first access
    /// (`DUMMY` becomes the block's block entity type).
    pub fn block_entity_mut(&mut self, pos: BlockPos) -> Option<&mut Compound> {
        let state = self.block(pos);
        let chunk = self.chunk_mut(pos);
        let tag = chunk.block_entity_mut(pos)?;
        if matches!(tag.get("id"), Some(Tag::String(id)) if id == "DUMMY") {
            let kind = block_entity_type(state)?;
            tag.insert("id", Tag::String(kind.into()));
        }
        Some(tag)
    }

    /// `scheduleTick(pos, block, 0)` (the delay of generation ticks is always 0).
    pub fn schedule_block_tick(&mut self, pos: BlockPos, block: BlockId) {
        if let Some(live) = self.live.as_deref_mut() {
            return live.schedule_block_tick(pos, block, 0);
        }
        self.chunk_mut(pos)
            .block_ticks_mut()
            .schedule(block, pos, 0);
    }

    /// `scheduleTick(pos, fluid, 0)`; `fluid` is the fluid's source block (water or lava).
    pub fn schedule_fluid_tick(&mut self, pos: BlockPos, fluid: FluidTickKind) {
        if let Some(live) = self.live.as_deref_mut() {
            return live.schedule_fluid_tick(pos, fluid, 0);
        }
        self.chunk_mut(pos)
            .fluid_ticks_mut()
            .schedule(fluid, pos, 0);
    }
}

/// Whether the state's block has a block entity (`BlockState.hasBlockEntity`).
pub fn has_block_entity(state: BlockState) -> bool {
    state.block().info().block_entity != u16::MAX
}

/// The block entity type id of the state's block.
pub fn block_entity_type(state: BlockState) -> Option<&'static str> {
    let id = state.block().info().block_entity;
    if id == u16::MAX {
        return None;
    }
    vanillite_data::registry::Registry::by_name("minecraft:block_entity_type")
        .and_then(|r| r.name(id))
}

impl std::fmt::Debug for WorldGenRegion<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorldGenRegion")
            .field("center", &self.center)
            .field("radius", &self.radius)
            .finish_non_exhaustive()
    }
}

/// The saved NBT of a new entity of type `id` at `x, y, z` (`id` and `Pos`; generation only
/// records what it needs of entities).
pub fn entity_tag(id: &str, x: f64, y: f64, z: f64) -> Compound {
    Compound::new().with("id", id).with(
        "Pos",
        vanillite_nbt::List(vec![Tag::Double(x), Tag::Double(y), Tag::Double(z)]),
    )
}

/// The `Pos` of an entity tag (0, 0, 0 if missing).
pub fn entity_pos(entity: &Compound) -> [f64; 3] {
    let mut out = [0.0; 3];
    if let Some(Tag::List(list)) = entity.get("Pos") {
        for (slot, tag) in out.iter_mut().zip(&list.0) {
            if let Tag::Double(v) = tag {
                *slot = *v;
            }
        }
    }
    out
}

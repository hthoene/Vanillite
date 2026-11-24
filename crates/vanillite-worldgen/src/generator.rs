//! `NoiseBasedChunkGenerator`: the generation stages of one dimension.
//!
//! A [`TerrainGenerator`] is built once per dimension from the data packs and the seed. It holds
//! the compiled density program and everything derived from the seed, is `Send + Sync` and is
//! shared by all generation tasks. The stages are pure functions of the seed and the chunk
//! position (plus, for [`Stage::Terrain`], the biomes of the neighbouring chunks), so any
//! scheduler and any number of threads produce Vanilla's result.
//!
//! The entry points are batch-shaped: [`TerrainGenerator::generate_biomes`] and
//! [`TerrainGenerator::generate_terrain`] take many chunks per call and write into
//! caller-owned chunks, so a scheduler (or a future accelerated backend) can amortise setup over
//! a batch and complete batches asynchronously on its own threads.

use std::cell::RefCell;

use serde_json::Value;
use vanillite_base::ChunkPos;
use vanillite_chunk::{BiomeId, Biomes, PalettedContainer};
use vanillite_data::block::{BlockId, BlockState};
use vanillite_datapack::LoadedPacks;

use crate::Error;
use crate::anchor::WorldGenerationContext;
use crate::aquifer::{Aquifer, AquiferFunctions, GlobalFluidPicker};
use crate::biome::climate::ClimateSampler;
use crate::biome::{BiomeManager, BiomeRegistry, BiomeResolver, BiomeSource};
use crate::blocks::BlockSet;
use crate::carver::{CarverId, CarverRegistry, carve_chunk};
use crate::density::{DensityCompiler, FnId, Program, SamplerContext};
use crate::feature::{FeatureSet, WorldGenRegion};
use crate::material::MaterialSystem;
use crate::proto::{ProtoChunk, WgHeightmap};
use crate::random_state::RandomState;
use crate::settings::NoiseGeneratorSettings;
use crate::volume::DensityVolume;

/// A generation step of Vanilla's chunk pyramid that this crate implements.
///
/// Structure starts and structure references are not generated yet; they run as no-ops so a
/// scheduler can already drive the full pyramid.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Stage {
    /// `structure_starts`.
    StructureStarts,
    /// `structure_references`.
    StructureReferences,
    /// `biomes`: noise biomes from the climate functions.
    Biomes,
    /// `terrain`: noise fill with aquifers, material rules (surface, bedrock, ore veins) and
    /// carvers.
    Terrain,
    /// `features`.
    Features,
}

impl Stage {
    /// All stages in pyramid order.
    pub const ALL: [Self; 5] = [
        Self::StructureStarts,
        Self::StructureReferences,
        Self::Biomes,
        Self::Terrain,
        Self::Features,
    ];

    /// Chebyshev radius of neighbours that must have reached the previous stage
    /// (`ChunkStep.addRequirement` of the previous status). Structure dependencies (radius 8 on
    /// `structure_starts`) are left out while structures are not generated.
    pub const fn dependency_radius(self) -> u32 {
        match self {
            Self::StructureStarts | Self::StructureReferences | Self::Biomes => 0,
            Self::Terrain | Self::Features => 1,
        }
    }

    /// Chebyshev radius of chunks the stage writes blocks into (`blockStateWriteRadius`).
    pub const fn write_radius(self) -> u32 {
        match self {
            Self::Features => 1,
            _ => 0,
        }
    }
}

/// The dimension a generator is built for: its `worldgen/noise_settings` id, its biome source
/// (the `biome_source` object of the dimension's generator) and the height of its levels.
#[derive(Clone, Copy, Debug)]
pub struct DimensionSpec<'a> {
    /// `worldgen/noise_settings` id, e.g. `minecraft:overworld`.
    pub noise_settings: &'a str,
    /// The generator's `biome_source` JSON.
    pub biome_source: &'a Value,
    /// Lowest block Y of the level (dimension type `min_y`).
    pub min_y: i32,
    /// Number of block rows of the level (dimension type `height`).
    pub height: i32,
    /// Whether the dimension has sky light (dimension type `has_skylight`).
    pub has_sky_light: bool,
}

/// The noise biomes of a 3×3 chunk area around a chunk (row-major, `dz` outer, `dx` inner, the
/// centre at index 4), as the terrain stage reads them from its neighbours.
#[derive(Clone, Debug)]
pub struct NeighbourBiomes {
    min_section: i32,
    chunks: [Vec<PalettedContainer<Biomes>>; 9],
}

impl NeighbourBiomes {
    /// Collects the biomes of the 3×3 chunks around `center`; `chunk(dx, dz)` returns the
    /// neighbour at that offset (offsets in `-1..=1`), which must have finished [`Stage::Biomes`].
    pub fn collect<'a>(mut chunk: impl FnMut(i32, i32) -> &'a ProtoChunk) -> Self {
        let mut min_section = 0;
        let chunks = std::array::from_fn(|i| {
            let (dx, dz) = (i as i32 % 3 - 1, i as i32 / 3 - 1);
            let c = chunk(dx, dz);
            min_section = c.min_y() >> 4;
            c.sections().iter().map(|s| s.biomes().clone()).collect()
        });
        Self {
            min_section,
            chunks,
        }
    }

    /// `ChunkAccess.getNoiseBiome` of the chunk containing the quart, for quarts of the 3×3
    /// area around `center`.
    fn noise_biome(&self, center: ChunkPos, quart_x: i32, quart_y: i32, quart_z: i32) -> BiomeId {
        let dx = (quart_x >> 2) - center.x;
        let dz = (quart_z >> 2) - center.z;
        debug_assert!((-1..=1).contains(&dx) && (-1..=1).contains(&dz));
        let sections = &self.chunks[((dz + 1) * 3 + dx + 1) as usize];
        let min_quart = self.min_section << 2;
        let max_quart = min_quart + (sections.len() as i32) * 4 - 1;
        let qy = quart_y.clamp(min_quart, max_quart);
        let section = ((qy >> 2) - self.min_section) as usize;
        sections[section].get_at((quart_x & 3) as u32, (qy & 3) as u32, (quart_z & 3) as u32)
    }

    /// `collectPossibleBiomes(region, 1)`: every biome in the 3×3 area, sorted.
    fn possible_biomes(&self) -> Vec<BiomeId> {
        let mut out = Vec::new();
        for container in self.chunks.iter().flatten() {
            container.for_each_count(|biome, count| {
                if count > 0 && !out.contains(&biome) {
                    out.push(biome);
                }
            });
        }
        out.sort_unstable();
        out
    }
}

/// The three steps of [`Stage::Terrain`], in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TerrainStep {
    /// `doFill`: final density and aquifers.
    Noise,
    /// `buildSurface`: material rules (surface, bedrock, ore veins, underground bands).
    Surface,
    /// `generateCarvers` and `applyCarvingMask`.
    Carvers,
}

/// One chunk of a terrain batch: the chunk (at [`Stage::Biomes`]) and its neighbours' biomes.
#[derive(Debug)]
pub struct TerrainJob {
    pub chunk: ProtoChunk,
    pub neighbours: NeighbourBiomes,
}

/// Vanilla's `NoiseBasedChunkGenerator` with its `RandomState` for one dimension and seed.
#[derive(Debug)]
pub struct TerrainGenerator {
    seed: i64,
    level_min_y: i32,
    level_height: i32,
    has_sky_light: bool,
    settings: NoiseGeneratorSettings,
    random_state: RandomState,
    program: Program,
    final_density: FnId,
    climate: ClimateSampler,
    aquifer: Option<AquiferFunctions>,
    fluid_picker: GlobalFluidPicker,
    material: MaterialSystem,
    biomes: BiomeRegistry,
    biome_source: BiomeSource,
    biome_manager: BiomeManager,
    carvers: CarverRegistry,
    /// Carvers per biome id.
    biome_carvers: Vec<Vec<CarverId>>,
    uncarvable: BlockSet,
    features: FeatureSet,
    grass_block: BlockId,
    mycelium: BlockId,
    dirt: BlockId,
}

impl TerrainGenerator {
    /// Builds the generator: parses the dimension's settings, derives the random state from
    /// `seed` and compiles every density function the stages use into one program.
    pub fn new(
        packs: &LoadedPacks,
        dimension: DimensionSpec<'_>,
        seed: i64,
    ) -> Result<Self, Error> {
        let settings = NoiseGeneratorSettings::load(packs, dimension.noise_settings)?;
        let random_state = RandomState::new(packs, seed, settings.legacy_random_source)?;
        let biomes = BiomeRegistry::load(packs)?;
        let biome_source = BiomeSource::from_json(dimension.biome_source, packs, &biomes)?;
        let carvers = CarverRegistry::load(packs)?;
        let biome_carvers = biomes
            .all()
            .iter()
            .map(|b| carvers.ids(&b.carvers))
            .collect::<Result<Vec<_>, Error>>()?;

        let mut compiler = DensityCompiler::new(&random_state);
        let router = &settings.router;
        let climate = ClimateSampler {
            temperature: compiler.compile(&router.temperature),
            humidity: compiler.compile(&router.vegetation),
            continentalness: compiler.compile(&router.continents),
            erosion: compiler.compile(&router.erosion),
            depth: compiler.compile(&router.depth),
            weirdness: compiler.compile(&router.ridges),
        };
        let final_density = compiler.compile(&router.final_density);
        let aquifer = settings
            .aquifers
            .as_ref()
            .map(|config| AquiferFunctions::compile(config, &mut compiler));
        let material =
            MaterialSystem::new(packs, &settings, &random_state, &biomes, &mut compiler)?;
        let program = compiler.finish();

        let features = FeatureSet::load(
            packs,
            &biomes,
            &random_state,
            &biome_source.possible_biomes(),
            WorldGenerationContext::for_settings(&settings, dimension.min_y, dimension.height),
        )?;

        let block = |name: &str| {
            BlockId::by_name(name).ok_or_else(|| Error::new(format!("unknown block {name}")))
        };
        Ok(Self {
            seed,
            level_min_y: dimension.min_y,
            level_height: dimension.height,
            has_sky_light: dimension.has_sky_light,
            fluid_picker: GlobalFluidPicker::from_settings(&settings),
            settings,
            program,
            final_density,
            climate,
            aquifer,
            material,
            biome_manager: BiomeManager::for_world_seed(seed),
            biomes,
            biome_source,
            carvers,
            biome_carvers,
            uncarvable: BlockSet::from_tag(packs, "minecraft:uncarvable"),
            features,
            grass_block: block("minecraft:grass_block")?,
            mycelium: block("minecraft:mycelium")?,
            dirt: block("minecraft:dirt")?,
            random_state,
        })
    }

    /// The world seed.
    pub fn seed(&self) -> i64 {
        self.seed
    }

    /// The noise settings.
    pub fn settings(&self) -> &NoiseGeneratorSettings {
        &self.settings
    }

    /// The biome registry the generator writes ids of.
    pub fn biomes(&self) -> &BiomeRegistry {
        &self.biomes
    }

    /// The compiled density program (for other backends and debugging).
    pub fn program(&self) -> &Program {
        &self.program
    }

    /// The seed-derived random state.
    pub fn random_state(&self) -> &RandomState {
        &self.random_state
    }

    /// A new, empty chunk with the level's height (`ProtoChunk` at status `empty`).
    pub fn new_chunk(&self, pos: ChunkPos) -> ProtoChunk {
        let plains = self.biomes.id_of("minecraft:plains").unwrap_or(0);
        ProtoChunk::new(pos, self.level_min_y, self.level_height, plains)
    }

    /// The parsed features and their global order.
    pub fn features(&self) -> &FeatureSet {
        &self.features
    }

    /// Runs `stage` on every chunk of `batch` that needs no neighbours. Panics for
    /// [`Stage::Terrain`] and [`Stage::Features`], which need neighbours (use
    /// [`Self::generate_terrain`] and [`Self::generate_features`]).
    pub fn generate(&self, stage: Stage, batch: &mut [ProtoChunk]) {
        match stage {
            Stage::StructureStarts | Stage::StructureReferences => {}
            Stage::Biomes => self.generate_biomes(batch),
            Stage::Terrain => {
                panic!("the terrain stage needs neighbour biomes: use generate_terrain")
            }
            Stage::Features => {
                panic!("the features stage needs neighbour chunks: use generate_features")
            }
        }
    }

    /// [`Stage::Features`] for one chunk (`applyBiomeDecoration`): `area` holds the 3×3 chunks
    /// around it (row-major, `dz` outer, `dx` inner, the chunk to decorate in the middle), all
    /// past [`Stage::Terrain`]. Features write into the neighbours too, so two chunks whose
    /// areas overlap must not be decorated at the same time, and the order in which neighbouring
    /// chunks are decorated changes the result at chunk borders, exactly as in Vanilla.
    pub fn generate_features(&self, area: [&mut ProtoChunk; 9]) {
        let mut region = self.feature_region(area.into());
        self.features.decorate(&mut region);
    }

    /// The `WorldGenRegion` of the features stage over `chunks` (3×3, row-major, `dz` outer),
    /// set up for this dimension.
    pub fn feature_region<'a>(&self, chunks: Vec<&'a mut ProtoChunk>) -> WorldGenRegion<'a> {
        let mut region =
            WorldGenRegion::new(chunks, 1, 1, &self.random_state, self.settings.sea_level);
        region.set_generation_context(self.features.env.context);
        region.set_has_sky_light(self.has_sky_light);
        region
    }

    /// [`Stage::Biomes`] for a batch (`ChunkGenerator.createBiomes`).
    pub fn generate_biomes(&self, batch: &mut [ProtoChunk]) {
        for chunk in batch {
            self.biome_source
                .create_biomes(chunk, &self.program, &self.climate);
        }
    }

    /// [`Stage::Terrain`] for a batch (`NoiseBasedChunkGenerator.buildTerrain`). One sampler
    /// context is reused across the batch; it is reset per chunk, as Vanilla creates a fresh
    /// `NoiseChunk` per chunk.
    pub fn generate_terrain(&self, batch: &mut [TerrainJob]) {
        let mut ctx = SamplerContext::new(true);
        for job in batch {
            ctx.reset();
            self.build_terrain(&mut job.chunk, &job.neighbours, &mut ctx, &mut |_, _| {});
        }
    }

    /// [`Stage::Terrain`] for one chunk, calling `observe` with the chunk after each step (for
    /// parity tests and debugging).
    pub fn generate_terrain_observed(
        &self,
        job: &mut TerrainJob,
        mut observe: impl FnMut(TerrainStep, &ProtoChunk),
    ) {
        let mut ctx = SamplerContext::new(true);
        self.build_terrain(&mut job.chunk, &job.neighbours, &mut ctx, &mut observe);
    }

    /// `buildTerrain` for one chunk with a fresh caching context `ctx`.
    fn build_terrain(
        &self,
        chunk: &mut ProtoChunk,
        neighbours: &NeighbourBiomes,
        ctx: &mut SamplerContext,
        observe: &mut dyn FnMut(TerrainStep, &ProtoChunk),
    ) {
        // `NoiseSettings.clampToHeightAccessor`.
        let min_y = self.settings.min_y.max(chunk.min_y());
        let height = (self.settings.min_y + self.settings.height).min(chunk.max_y() + 1) - min_y;
        if height <= 0 {
            return;
        }
        let pos = chunk.pos();
        let volume = DensityVolume::new(16, height, 16, pos.x << 4, min_y, pos.z << 4);

        // `new NoiseChunk(...)`: the aquifer samples the surface level while it is built.
        let mut aquifer = Aquifer::for_chunk(
            self.aquifer.as_ref(),
            &self.random_state,
            &volume,
            self.fluid_picker,
            &self.program,
            ctx,
        );
        self.fill_from_noise(chunk, &volume, &mut aquifer, ctx);
        observe(TerrainStep::Noise, chunk);

        let biome_at = |x: i32, y: i32, z: i32| {
            self.biome_manager.get_biome(x, y, z, &mut |qx, qy, qz| {
                neighbours.noise_biome(pos, qx, qy, qz)
            })
        };
        self.material.build_surface(
            &self.program,
            ctx,
            chunk,
            &self.biomes,
            &volume,
            &biome_at,
            &neighbours.possible_biomes(),
        );
        observe(TerrainStep::Surface, chunk);
        self.carve(chunk, &mut aquifer, ctx);
        chunk.finish_terrain();
        observe(TerrainStep::Carvers, chunk);
    }

    /// `doFill`: the final density over the chunk volume, turned into blocks by the aquifer.
    fn fill_from_noise(
        &self,
        chunk: &mut ProtoChunk,
        volume: &DensityVolume,
        aquifer: &mut Aquifer,
        ctx: &mut SamplerContext,
    ) {
        let mut density = vec![0.0_f32; volume.len()];
        self.program
            .sample_volume(ctx, self.final_density, &mut density, volume);
        let default_block = self.settings.default_block;
        for z in 0..volume.size_z {
            let block_z = volume.block_z(z);
            for x in 0..volume.size_x {
                let block_x = volume.block_x(x);
                for y in (0..volume.size_y).rev() {
                    let block_y = volume.block_y(y);
                    let value = density[volume.index(x, y, z)];
                    let state = aquifer
                        .compute_substance(
                            &self.program,
                            ctx,
                            block_x,
                            block_y,
                            block_z,
                            f64::from(value),
                        )
                        .unwrap_or(default_block);
                    if state == BlockState::AIR {
                        continue;
                    }
                    chunk.set_block_raw(block_x, block_y, block_z, state);
                    let (lx, lz) = (x as usize, z as usize);
                    chunk.update_heightmap(WgHeightmap::OceanFloor, lx, block_y, lz, state);
                    chunk.update_heightmap(WgHeightmap::WorldSurface, lx, block_y, lz, state);
                    if aquifer.should_schedule_fluid_update() && !state.fluid_state().is_empty() {
                        chunk.mark_post_processing(block_x, block_y, block_z);
                    }
                }
            }
        }
    }

    /// `generateCarvers` and `applyCarvingMask`.
    fn carve(&self, chunk: &mut ProtoChunk, aquifer: &mut Aquifer, ctx: &mut SamplerContext) {
        let pos = chunk.pos();
        let context =
            WorldGenerationContext::for_settings(&self.settings, chunk.min_y(), chunk.height());
        // The carvers' biomes come from an uncached resolver at each source chunk's corner,
        // queried in Vanilla's loop order.
        let mut resolver = self
            .biome_source
            .create_uncached_resolver(&self.program, &self.climate);
        const RANGE: i32 = 8;
        let side = (2 * RANGE + 1) as usize;
        let mut source_carvers: Vec<&[CarverId]> = Vec::with_capacity(side * side);
        for dx in -RANGE..=RANGE {
            for dz in -RANGE..=RANGE {
                let biome = resolver.noise_biome((pos.x + dx) << 2, 0, (pos.z + dz) << 2);
                source_carvers.push(&self.biome_carvers[usize::from(biome)]);
            }
        }
        let lookup = |source: ChunkPos| {
            let dx = (source.x - pos.x + RANGE) as usize;
            let dz = (source.z - pos.z + RANGE) as usize;
            source_carvers[dx * side + dz]
        };
        let mask = carve_chunk(&self.carvers, &lookup, self.seed, pos, &context);
        if mask.is_empty() {
            return;
        }

        // `biomeManager.withDifferentSource(biomeResolver)`.
        let resolver = RefCell::new(resolver);
        let biome_at = |x: i32, y: i32, z: i32| {
            self.biome_manager
                .get_biome(x, y, z, &mut *resolver.borrow_mut())
        };
        let (min_x, min_z) = (pos.x << 4, pos.z << 4);
        mask.visit(|x, z, bottom, top| {
            let mut has_grass = false;
            let (world_x, world_z) = (min_x + x, min_z + z);
            for world_y in (bottom..=top).rev() {
                let current = chunk.block(world_x, world_y, world_z);
                if self.uncarvable.contains(current) {
                    continue;
                }
                let block = current.block();
                if block == self.grass_block || block == self.mycelium {
                    has_grass = true;
                }
                let Some(state) =
                    aquifer.compute_substance(&self.program, ctx, world_x, world_y, world_z, 0.0)
                else {
                    continue;
                };
                chunk.set_block(world_x, world_y, world_z, state);
                let fluid = !state.fluid_state().is_empty();
                if aquifer.should_schedule_fluid_update() && fluid {
                    chunk.mark_post_processing(world_x, world_y, world_z);
                }
                if has_grass && chunk.block(world_x, world_y - 1, world_z).block() == self.dirt {
                    let top_material = self.material.top_material(
                        &self.program,
                        ctx,
                        chunk,
                        &self.biomes,
                        &biome_at,
                        world_x,
                        world_y - 1,
                        world_z,
                        fluid,
                    );
                    if let Some(top_material) = top_material {
                        chunk.set_block(world_x, world_y - 1, world_z, top_material);
                        if !top_material.fluid_state().is_empty() {
                            chunk.mark_post_processing(world_x, world_y - 1, world_z);
                        }
                    }
                }
            }
        });
    }
}

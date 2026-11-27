//! Aquifers (`net.minecraft.world.level.levelgen.Aquifer`): fluid placement below the surface
//! during the noise fill and when carved blocks are filled.
//!
//! Ports `Aquifer` (the disabled aquifer, `FluidPicker`, `FluidStatus`, `Config.create`),
//! `Aquifer.NoiseBasedAquifer` and the global fluid picker of
//! `NoiseBasedChunkGenerator.createFluidPicker`.
//!
//! Vanilla 26.3 creates one aquifer per `NoiseChunk`, bound to the chunk's caching sampler set.
//! The constructor already samples `surface_level` over a volume (`maxSurfaceLevel`), and every
//! later lookup samples single values; because the per-chunk [`SamplerContext`] caches decide
//! whether a value comes from a cache or is computed anew, the calls here happen in exactly
//! Vanilla's order with the context of the chunk.

use rustc_hash::FxHashMap;
use vanillite_base::math::{clamp, clamped_map, floor, map};
use vanillite_base::random::{PositionalRandomFactory, RandomSource};
use vanillite_base::{BlockPos, ChunkPos};
use vanillite_data::block::{BlockId, BlockState, ids};

use crate::density::{DensityCompiler, FnId, Program, SamplerContext};
use crate::mth::floor_f;
use crate::random::AnyPositional;
use crate::random_state::RandomState;
use crate::settings::{AquiferConfig, NoiseGeneratorSettings};
use crate::volume::DensityVolume;

/// `DimensionType.WAY_BELOW_MIN_Y`: the fluid level of a dry aquifer cell.
pub const WAY_BELOW_MIN_Y: i32 = -2032 << 4;

/// The level below which the global fluid picker places lava.
const LAVA_LEVEL: i32 = -54;

/// `Aquifer.FluidStatus`: a fluid filling everything below `fluid_level`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FluidStatus {
    pub fluid_level: i32,
    pub fluid_type: BlockState,
}

impl FluidStatus {
    /// `at(blockY)`: the fluid below the level, air at and above it.
    #[inline]
    pub fn at(&self, block_y: i32) -> BlockState {
        if block_y < self.fluid_level {
            self.fluid_type
        } else {
            ids::AIR.default_state()
        }
    }
}

/// The global `FluidPicker` of `NoiseBasedChunkGenerator.createFluidPicker`: lava below
/// `min(-54, sea_level)`, the settings' default fluid up to the sea level elsewhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GlobalFluidPicker {
    sea_level: i32,
    lava: FluidStatus,
    sea: FluidStatus,
}

impl GlobalFluidPicker {
    /// The picker for a sea level and default fluid.
    pub fn new(sea_level: i32, default_fluid: BlockState) -> Self {
        Self {
            sea_level,
            lava: FluidStatus {
                fluid_level: LAVA_LEVEL,
                fluid_type: ids::LAVA.default_state(),
            },
            sea: FluidStatus {
                fluid_level: sea_level,
                fluid_type: default_fluid,
            },
        }
    }

    /// The picker of a generator's noise settings.
    pub fn from_settings(settings: &NoiseGeneratorSettings) -> Self {
        Self::new(settings.sea_level, settings.default_fluid)
    }

    /// `computeFluid(blockX, blockY, blockZ)`.
    #[inline]
    pub fn compute_fluid(&self, _x: i32, y: i32, _z: i32) -> FluidStatus {
        if y < LAVA_LEVEL.min(self.sea_level) {
            self.lava
        } else {
            self.sea
        }
    }
}

/// The aquifer's density functions (`Aquifer.Config`), compiled into the world's program.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AquiferFunctions {
    pub barrier: FnId,
    pub fluid_level_floodedness: FnId,
    pub fluid_level_spread: FnId,
    pub lava: FnId,
    pub exclusion: FnId,
    pub surface_level: FnId,
    /// Whether the functions that decide a cell's fluid give the same values whenever they are
    /// sampled ([`Program::is_point_exact`]), so cells may be resolved ahead of their first use.
    pub cells_point_exact: bool,
}

impl AquiferFunctions {
    /// Compiles the functions of `config` (at generator setup).
    pub fn compile(config: &AquiferConfig, compiler: &mut DensityCompiler<'_>) -> Self {
        let mut functions = Self {
            barrier: compiler.compile(&config.barrier),
            fluid_level_floodedness: compiler.compile(&config.fluid_level_floodedness),
            fluid_level_spread: compiler.compile(&config.fluid_level_spread),
            lava: compiler.compile(&config.lava),
            exclusion: compiler.compile(&config.exclusion),
            surface_level: compiler.compile(&config.surface_level),
            cells_point_exact: false,
        };
        let program = compiler.program();
        functions.cells_point_exact = [
            functions.fluid_level_floodedness,
            functions.fluid_level_spread,
            functions.lava,
            functions.exclusion,
            functions.surface_level,
        ]
        .into_iter()
        .all(|function| program.is_point_exact(function));
        functions
    }
}

/// An aquifer (`Aquifer`): decides what fills a block whose density says "not solid".
#[derive(Debug)]
pub enum Aquifer {
    /// `Aquifer.createDisabled(fluidRule)`: the global fluid everywhere.
    Disabled(GlobalFluidPicker),
    /// `Aquifer.NoiseBasedAquifer`.
    NoiseBased(Box<NoiseBasedAquifer>),
}

impl Aquifer {
    /// The aquifer of a `NoiseChunk` over `volume`: noise based if the settings configure
    /// aquifers (`functions` is `Some`), disabled otherwise. The positional random is
    /// `random_state.random_factory("minecraft:aquifer")`.
    pub fn for_chunk(
        functions: Option<&AquiferFunctions>,
        random_state: &RandomState,
        volume: &DensityVolume,
        fluid_picker: GlobalFluidPicker,
        program: &Program,
        ctx: &mut SamplerContext,
    ) -> Self {
        match functions {
            Some(functions) => Self::new(
                functions,
                random_state.random_factory("minecraft:aquifer"),
                volume,
                fluid_picker,
                program,
                ctx,
            ),
            None => Self::disabled(fluid_picker),
        }
    }

    /// `Aquifer.Config.create(cachingSamplers, positionalRandomFactory, volume, fluidRule)`.
    ///
    /// Samples `surface_level` over the volume's surroundings with `ctx` (Vanilla's
    /// `maxSurfaceLevel`), so it must be called where Vanilla constructs the `NoiseChunk`.
    pub fn new(
        functions: &AquiferFunctions,
        positional: AnyPositional,
        volume: &DensityVolume,
        fluid_picker: GlobalFluidPicker,
        program: &Program,
        ctx: &mut SamplerContext,
    ) -> Self {
        Self::NoiseBased(Box::new(NoiseBasedAquifer::new(
            *functions,
            positional,
            volume,
            fluid_picker,
            program,
            ctx,
        )))
    }

    /// `Aquifer.createDisabled(fluidRule)`.
    pub fn disabled(fluid_picker: GlobalFluidPicker) -> Self {
        Self::Disabled(fluid_picker)
    }

    /// `computeSubstance(blockX, blockY, blockZ, density)`: the fluid or air for the block,
    /// or `None` for a solid block (the generator's default block).
    #[inline]
    pub fn compute_substance(
        &mut self,
        program: &Program,
        ctx: &mut SamplerContext,
        x: i32,
        y: i32,
        z: i32,
        density: f64,
    ) -> Option<BlockState> {
        match self {
            Self::Disabled(picker) => {
                if density > 0.0 {
                    None
                } else {
                    Some(picker.compute_fluid(x, y, z).at(y))
                }
            }
            Self::NoiseBased(aquifer) => aquifer.compute_substance(program, ctx, x, y, z, density),
        }
    }

    /// The lowest Y from which every block with a density `<= 0` is air: above the aquifer's
    /// sampling limit the global fluid applies, which is air from the sea level up.
    pub fn air_from_y(&self) -> i32 {
        match self {
            Self::Disabled(picker) => picker.sea_level.max(picker.sea.fluid_level),
            Self::NoiseBased(aquifer) => aquifer
                .skip_sampling_above_y
                .saturating_add(1)
                .max(aquifer.fluid_picker.sea_level)
                .max(aquifer.fluid_picker.sea.fluid_level),
        }
    }

    /// `shouldScheduleFluidUpdate()`: whether the last fluid placed must be ticked
    /// (marked for post-processing).
    #[inline]
    pub fn should_schedule_fluid_update(&self) -> bool {
        match self {
            Self::Disabled(_) => false,
            Self::NoiseBased(aquifer) => match aquifer.air_above_fluids {
                Some((x, y, z)) => aquifer.air_schedules_update(x, y, z),
                None => aquifer.should_schedule_fluid_update,
            },
        }
    }
}

/// `similarity(distanceSqr1, distanceSqr2)`.
#[inline]
fn similarity(distance_sqr1: i32, distance_sqr2: i32) -> f64 {
    1.0 - f64::from(distance_sqr2.wrapping_sub(distance_sqr1)) / 25.0
}

/// Inserts a cell into the sorted top four (`distance_sqr` ascending, `closest` alongside);
/// ties go in front (`>=`), like Vanilla. Unrolled: this runs 12 times per fluid block.
#[inline(always)]
fn insert_closest(
    distance_sqr: &mut [i32; 4],
    closest: &mut [usize; 4],
    new_distance: i32,
    index: usize,
) {
    let [d0, d1, d2, d3] = *distance_sqr;
    let [c0, c1, c2, _] = *closest;
    if d0 >= new_distance {
        *distance_sqr = [new_distance, d0, d1, d2];
        *closest = [index, c0, c1, c2];
    } else if d1 >= new_distance {
        *distance_sqr = [d0, new_distance, d1, d2];
        *closest = [c0, index, c1, c2];
    } else if d2 >= new_distance {
        *distance_sqr = [d0, d1, new_distance, d2];
        *closest = [c0, c1, index, c2];
    } else if d3 >= new_distance {
        distance_sqr[3] = new_distance;
        closest[3] = index;
    }
}

/// `FLOWING_UPDATE_SIMULARITY`.
const FLOWING_UPDATE_SIMILARITY: f64 = 1.0 - (144 - 100) as f64 / 25.0;

/// `SURFACE_SAMPLING_OFFSETS_IN_CHUNKS`.
const SURFACE_SAMPLING_OFFSETS_IN_CHUNKS: [[i32; 2]; 13] = [
    [0, 0],
    [-2, -1],
    [-1, -1],
    [0, -1],
    [1, -1],
    [-3, 0],
    [-2, 0],
    [-1, 0],
    [1, 0],
    [-2, 1],
    [-1, 1],
    [0, 1],
    [1, 1],
];

/// Sentinel of an aquifer cell whose location was not computed yet (`Long.MAX_VALUE`).
const NO_LOCATION: i64 = i64::MAX;

#[inline]
fn grid_x(block: i32) -> i32 {
    block >> 4
}

#[inline]
fn from_grid_x(grid: i32, offset: i32) -> i32 {
    (grid << 4) + offset
}

#[inline]
fn grid_y(block: i32) -> i32 {
    block.div_euclid(12)
}

#[inline]
fn from_grid_y(grid: i32, offset: i32) -> i32 {
    grid * 12 + offset
}

#[inline]
fn grid_z(block: i32) -> i32 {
    block >> 4
}

#[inline]
fn from_grid_z(grid: i32, offset: i32) -> i32 {
    (grid << 4) + offset
}

/// `adjustSurfaceLevel(preliminarySurfaceLevel)`.
#[inline]
fn adjust_surface_level(level: i32) -> i32 {
    level + 8
}

/// Surface levels by quart position (`ChunkPos.pack`) that the aquifers of one thread have
/// sampled for one program: a level is a function of the position alone when the function is
/// point exact, and the aquifer cells of neighbouring chunks sample the same positions.
#[derive(Default)]
struct SurfaceLevels {
    /// [`Program::id`] and the surface level function the levels belong to.
    source: (u64, u32),
    levels: FxHashMap<i64, i32>,
}

impl SurfaceLevels {
    /// Most levels kept per thread (about half a megabyte); a full cache starts over.
    const MAX: usize = 1 << 15;

    /// The level at `key`, from `sample` if this thread has not sampled it yet.
    fn get_or_insert(
        program: &Program,
        function: FnId,
        key: i64,
        sample: impl FnOnce() -> i32,
    ) -> i32 {
        thread_local! {
            static LEVELS: std::cell::RefCell<SurfaceLevels> = std::cell::RefCell::default();
        }
        let source = (program.id(), function.0);
        let known = LEVELS.with_borrow_mut(|cache| {
            if cache.source != source {
                cache.source = source;
                cache.levels.clear();
            }
            cache.levels.get(&key).copied()
        });
        // (Sampled outside the borrow: sampling does not come back here.)
        known.unwrap_or_else(|| {
            let level = sample();
            LEVELS.with_borrow_mut(|cache| {
                if cache.levels.len() >= Self::MAX {
                    cache.levels.clear();
                }
                cache.levels.insert(key, level);
            });
            level
        })
    }
}

/// What the 12 cells of a grid anchor have in common.
#[derive(Clone, Copy, Debug)]
struct AnchorSummary {
    /// The highest fluid level of the cells.
    max_fluid_level: i32,
    /// The fluid of every cell, if they all have the same.
    uniform: Option<FluidStatus>,
}

/// Whether a grid anchor has its summary yet.
#[derive(Clone, Copy, Debug)]
enum AnchorState {
    /// Not yet: how often it was asked.
    Asked(u8),
    Summary(AnchorSummary),
}

/// How often an anchor is searched cell by cell before its summary is made.
const SUMMARY_AFTER: u8 = 24;

/// `Aquifer.NoiseBasedAquifer`: aquifer cells on a 16×12×16 grid, each with its own fluid
/// level and type, separated by barriers.
#[derive(Debug)]
pub struct NoiseBasedAquifer {
    functions: AquiferFunctions,
    positional: AnyPositional,
    fluid_picker: GlobalFluidPicker,
    /// `aquiferCache`.
    status_cache: Box<[Option<FluidStatus>]>,
    /// `aquiferLocationCache`: packed `BlockPos` of each cell's centre.
    location_cache: Box<[i64]>,
    should_schedule_fluid_update: bool,
    skip_sampling_above_y: i32,
    min_grid_x: i32,
    min_grid_y: i32,
    min_grid_z: i32,
    grid_size_x: i32,
    grid_size_z: i32,
    /// `surfaceLevelCache`, keyed by `ChunkPos.pack(quantizedX, quantizedZ)`.
    surface_level_cache: FxHashMap<i64, i32>,
    lava: BlockState,
    /// The grid anchor of the last fluid block and its 12 cells in search order: their centres
    /// and indices.
    anchor: Option<(i32, i32, i32)>,
    anchor_cells: [(i32, i32, i32, usize); 12],
    /// Per grid anchor (indexed like the cells): its summary, once resolved.
    anchor_summaries: Box<[AnchorState]>,
    /// The block of the last result if it was air found without the search of the closest
    /// cells: whether it schedules a fluid update is then worked out when asked (generation
    /// only asks for fluids).
    air_above_fluids: Option<(i32, i32, i32)>,
}

impl NoiseBasedAquifer {
    fn new(
        functions: AquiferFunctions,
        positional: AnyPositional,
        volume: &DensityVolume,
        fluid_picker: GlobalFluidPicker,
        program: &Program,
        ctx: &mut SamplerContext,
    ) -> Self {
        let min_grid_x = grid_x(volume.min_x + -5);
        let max_grid_x = grid_x(volume.max_x() + -5) + 1;
        let grid_size_x = max_grid_x - min_grid_x + 1;
        let min_grid_y = grid_y(volume.min_y + 1) + -1;
        let max_grid_y = grid_y(volume.max_y() + 1) + 1;
        let grid_size_y = max_grid_y - min_grid_y + 1;
        let min_grid_z = grid_z(volume.min_z + -5);
        let max_grid_z = grid_z(volume.max_z() + -5) + 1;
        let grid_size_z = max_grid_z - min_grid_z + 1;
        let total = (grid_size_x * grid_size_y * grid_size_z) as usize;
        let mut aquifer = Self {
            functions,
            positional,
            fluid_picker,
            status_cache: vec![None; total].into_boxed_slice(),
            location_cache: vec![NO_LOCATION; total].into_boxed_slice(),
            should_schedule_fluid_update: false,
            skip_sampling_above_y: 0,
            min_grid_x,
            min_grid_y,
            min_grid_z,
            grid_size_x,
            grid_size_z,
            surface_level_cache: FxHashMap::default(),
            lava: ids::LAVA.default_state(),
            anchor: None,
            anchor_cells: [(0, 0, 0, 0); 12],
            anchor_summaries: vec![AnchorState::Asked(0); total].into_boxed_slice(),
            air_above_fluids: None,
        };
        let max_surface = aquifer.max_surface_level(
            program,
            ctx,
            from_grid_x(min_grid_x, 0),
            from_grid_z(min_grid_z, 0),
            from_grid_x(max_grid_x, 9),
            from_grid_z(max_grid_z, 9),
        );
        let max_adjusted_surface_level = adjust_surface_level(max_surface);
        let skip_sampling_above_grid_y = grid_y(max_adjusted_surface_level + 12) - -1;
        aquifer.skip_sampling_above_y = from_grid_y(skip_sampling_above_grid_y, 11) - 1;
        aquifer
    }

    /// `surfaceLevel(blockX, blockZ)`: the preliminary surface at the quart position, cached.
    fn surface_level(
        &mut self,
        program: &Program,
        ctx: &mut SamplerContext,
        block_x: i32,
        block_z: i32,
    ) -> i32 {
        let quantized_x = (block_x >> 2) << 2;
        let quantized_z = (block_z >> 2) << 2;
        let key = ChunkPos::new(quantized_x, quantized_z).as_long();
        let surface = self.functions.surface_level;
        // (An unfinished program has no number of its own to keep its levels apart.)
        let shared = self.functions.cells_point_exact && program.id() != 0;
        *self.surface_level_cache.entry(key).or_insert_with(|| {
            let mut sample =
                || floor_f(program.sample_value(ctx, surface, quantized_x, 0, quantized_z));
            if shared {
                // The cells of neighbouring chunks ask for the same positions.
                SurfaceLevels::get_or_insert(program, surface, key, sample)
            } else {
                sample()
            }
        })
    }

    /// `maxSurfaceLevel(minBlockX, minBlockZ, maxBlockX, maxBlockZ)`: samples the surface over
    /// a quart grid, fills the surface cache and returns the highest level.
    fn max_surface_level(
        &mut self,
        program: &Program,
        ctx: &mut SamplerContext,
        min_block_x: i32,
        min_block_z: i32,
        max_block_x: i32,
        max_block_z: i32,
    ) -> i32 {
        let min_quart_x = min_block_x >> 2;
        let max_quart_x = max_block_x >> 2;
        let min_quart_z = min_block_z >> 2;
        let max_quart_z = max_block_z >> 2;
        let volume = DensityVolume::with_step(
            max_quart_x - min_quart_x + 1,
            1,
            max_quart_z - min_quart_z + 1,
            min_quart_x << 2,
            0,
            min_quart_z << 2,
            4,
            1,
            4,
        );
        let mut buffer = vec![0.0_f32; volume.len()];
        program.sample_volume(ctx, self.functions.surface_level, &mut buffer, &volume);
        let mut max_y = i32::MIN;
        for z in 0..volume.size_z {
            for x in 0..volume.size_x {
                let surface_level = floor_f(buffer[volume.index(x, 0, z)]);
                let key = ChunkPos::new(volume.block_x(x), volume.block_z(z)).as_long();
                self.surface_level_cache.insert(key, surface_level);
                if surface_level > max_y {
                    max_y = surface_level;
                }
            }
        }
        max_y
    }

    /// `getIndex(gridX, gridY, gridZ)`.
    #[inline]
    fn index(&self, grid_x: i32, grid_y: i32, grid_z: i32) -> usize {
        let x = grid_x - self.min_grid_x;
        let y = grid_y - self.min_grid_y;
        let z = grid_z - self.min_grid_z;
        ((y * self.grid_size_z + z) * self.grid_size_x + x) as usize
    }

    #[inline]
    fn is(state: BlockState, block: BlockId) -> bool {
        state.block() == block
    }

    /// `computeSubstance(blockX, blockY, blockZ, density)`: solid blocks return here, the rest
    /// (much rarer, and much more work) in [`Self::compute_fluid_substance`].
    #[inline]
    fn compute_substance(
        &mut self,
        program: &Program,
        ctx: &mut SamplerContext,
        block_x: i32,
        block_y: i32,
        block_z: i32,
        density: f64,
    ) -> Option<BlockState> {
        self.air_above_fluids = None;
        if density > 0.0 {
            self.should_schedule_fluid_update = false;
            return None;
        }
        self.compute_fluid_substance(program, ctx, block_x, block_y, block_z, density)
    }

    /// `computeSubstance` of a block that is not solid by its density.
    #[inline(never)]
    fn compute_fluid_substance(
        &mut self,
        program: &Program,
        ctx: &mut SamplerContext,
        block_x: i32,
        block_y: i32,
        block_z: i32,
        density: f64,
    ) -> Option<BlockState> {
        let global_fluid = self.fluid_picker.compute_fluid(block_x, block_y, block_z);
        if block_y > self.skip_sampling_above_y {
            self.should_schedule_fluid_update = false;
            return Some(global_fluid.at(block_y));
        }
        if Self::is(global_fluid.at(block_y), ids::LAVA) {
            self.should_schedule_fluid_update = false;
            return Some(self.lava);
        }

        let anchor = (
            grid_x(block_x + -5),
            grid_y(block_y + 1),
            grid_z(block_z + -5),
        );
        if self.anchor != Some(anchor) {
            self.set_anchor(anchor);
        }
        if self.functions.cells_point_exact
            && let Some(summary) = self.anchor_summary(program, ctx, anchor)
        {
            // Two cases need no search for the closest cells; both follow from the code below.
            // Far enough above every fluid level around, the block is air: each cell's fluid
            // is air here, and every barrier between two of them has a gradient below -2
            // (five blocks above the higher level), so it is negative and adds no density.
            if block_y >= summary.max_fluid_level.saturating_add(5) {
                self.air_above_fluids = Some((block_x, block_y, block_z));
                return Some(ids::AIR.default_state());
            }
            // With one and the same fluid in all cells around, the block gets that fluid:
            // no barrier (equal levels) and nothing to flow to. Water right above the global
            // lava is the one case that schedules an update depending on the distances.
            if let Some(status) = summary.uniform {
                let fluid_state = status.at(block_y);
                if !(Self::is(fluid_state, ids::WATER)
                    && Self::is(
                        self.fluid_picker
                            .compute_fluid(block_x, block_y - 1, block_z)
                            .at(block_y - 1),
                        ids::LAVA,
                    ))
                {
                    self.should_schedule_fluid_update = false;
                    return Some(fluid_state);
                }
            }
        }
        let mut distance_sqr = [i32::MAX; 4];
        let mut closest = [0_usize; 4];
        for &(x, y, z, index) in &self.anchor_cells {
            let dx = x.wrapping_sub(block_x);
            let dy = y.wrapping_sub(block_y);
            let dz = z.wrapping_sub(block_z);
            let new_distance = dx
                .wrapping_mul(dx)
                .wrapping_add(dy.wrapping_mul(dy))
                .wrapping_add(dz.wrapping_mul(dz));
            insert_closest(&mut distance_sqr, &mut closest, new_distance, index);
        }

        let status1 = self.aquifer_status(program, ctx, closest[0]);
        let similarity12 = similarity(distance_sqr[0], distance_sqr[1]);
        let fluid_state = status1.at(block_y);
        if similarity12 <= 0.0 {
            self.should_schedule_fluid_update = if similarity12 >= FLOWING_UPDATE_SIMILARITY {
                let status2 = self.aquifer_status(program, ctx, closest[1]);
                status1 != status2
            } else {
                false
            };
            return Some(fluid_state);
        }
        if Self::is(fluid_state, ids::WATER)
            && Self::is(
                self.fluid_picker
                    .compute_fluid(block_x, block_y - 1, block_z)
                    .at(block_y - 1),
                ids::LAVA,
            )
        {
            self.should_schedule_fluid_update = true;
            return Some(fluid_state);
        }

        // `barrierNoiseValue`: sampled at most once per block, on first need.
        let mut barrier_noise = f64::NAN;
        let status2 = self.aquifer_status(program, ctx, closest[1]);
        let barrier12 = similarity12
            * self.calculate_pressure(
                program,
                ctx,
                block_x,
                block_y,
                block_z,
                &mut barrier_noise,
                status1,
                status2,
            );
        if density + barrier12 > 0.0 {
            self.should_schedule_fluid_update = false;
            return None;
        }
        let status3 = self.aquifer_status(program, ctx, closest[2]);
        let similarity13 = similarity(distance_sqr[0], distance_sqr[2]);
        if similarity13 > 0.0 {
            let barrier13 = similarity12
                * similarity13
                * self.calculate_pressure(
                    program,
                    ctx,
                    block_x,
                    block_y,
                    block_z,
                    &mut barrier_noise,
                    status1,
                    status3,
                );
            if density + barrier13 > 0.0 {
                self.should_schedule_fluid_update = false;
                return None;
            }
        }
        let similarity23 = similarity(distance_sqr[1], distance_sqr[2]);
        if similarity23 > 0.0 {
            let barrier23 = similarity12
                * similarity23
                * self.calculate_pressure(
                    program,
                    ctx,
                    block_x,
                    block_y,
                    block_z,
                    &mut barrier_noise,
                    status2,
                    status3,
                );
            if density + barrier23 > 0.0 {
                self.should_schedule_fluid_update = false;
                return None;
            }
        }

        let may_flow12 = status1 != status2;
        let may_flow23 = similarity23 >= FLOWING_UPDATE_SIMILARITY && status2 != status3;
        let may_flow13 = similarity13 >= FLOWING_UPDATE_SIMILARITY && status1 != status3;
        self.should_schedule_fluid_update = if !may_flow12 && !may_flow23 && !may_flow13 {
            similarity13 >= FLOWING_UPDATE_SIMILARITY
                && similarity(distance_sqr[0], distance_sqr[3]) >= FLOWING_UPDATE_SIMILARITY
                && status1 != self.aquifer_status(program, ctx, closest[3])
        } else {
            true
        };
        Some(fluid_state)
    }

    /// `shouldScheduleFluidUpdate()` after the block at `(block_x, block_y, block_z)` was
    /// found to be air above every fluid level of the current anchor (its cells are all
    /// resolved): what `compute_fluid_substance` sets on that path, where the fluid is air
    /// and no barrier adds density.
    fn air_schedules_update(&self, block_x: i32, block_y: i32, block_z: i32) -> bool {
        let mut distance_sqr = [i32::MAX; 4];
        let mut closest = [0_usize; 4];
        for &(x, y, z, index) in &self.anchor_cells {
            let dx = x.wrapping_sub(block_x);
            let dy = y.wrapping_sub(block_y);
            let dz = z.wrapping_sub(block_z);
            let new_distance = dx
                .wrapping_mul(dx)
                .wrapping_add(dy.wrapping_mul(dy))
                .wrapping_add(dz.wrapping_mul(dz));
            insert_closest(&mut distance_sqr, &mut closest, new_distance, index);
        }
        let status = |rank: usize| self.status_cache[closest[rank]];
        let similarity12 = similarity(distance_sqr[0], distance_sqr[1]);
        if similarity12 <= 0.0 {
            return similarity12 >= FLOWING_UPDATE_SIMILARITY && status(0) != status(1);
        }
        let similarity13 = similarity(distance_sqr[0], distance_sqr[2]);
        let similarity23 = similarity(distance_sqr[1], distance_sqr[2]);
        let may_flow12 = status(0) != status(1);
        let may_flow23 = similarity23 >= FLOWING_UPDATE_SIMILARITY && status(1) != status(2);
        let may_flow13 = similarity13 >= FLOWING_UPDATE_SIMILARITY && status(0) != status(2);
        if !may_flow12 && !may_flow23 && !may_flow13 {
            similarity13 >= FLOWING_UPDATE_SIMILARITY
                && similarity(distance_sqr[0], distance_sqr[3]) >= FLOWING_UPDATE_SIMILARITY
                && status(0) != status(3)
        } else {
            true
        }
    }

    /// The fluids of the 12 cells of grid anchor `anchor` (the current one) together. They
    /// are resolved once the anchor has been asked [`SUMMARY_AFTER`] times in the chunk: for a
    /// few blocks the search of the closest cells resolves fewer cells.
    fn anchor_summary(
        &mut self,
        program: &Program,
        ctx: &mut SamplerContext,
        anchor: (i32, i32, i32),
    ) -> Option<AnchorSummary> {
        let slot = self.index(anchor.0, anchor.1, anchor.2);
        match self.anchor_summaries[slot] {
            AnchorState::Summary(summary) => return Some(summary),
            AnchorState::Asked(times) if times < SUMMARY_AFTER => {
                self.anchor_summaries[slot] = AnchorState::Asked(times + 1);
                return None;
            }
            AnchorState::Asked(_) => {}
        }
        let cells = self.anchor_cells;
        let first = self.aquifer_status(program, ctx, cells[0].3);
        let mut summary = AnchorSummary {
            max_fluid_level: first.fluid_level,
            uniform: Some(first),
        };
        for &(_, _, _, index) in &cells[1..] {
            let status = self.aquifer_status(program, ctx, index);
            summary.max_fluid_level = summary.max_fluid_level.max(status.fluid_level);
            if summary.uniform != Some(status) {
                summary.uniform = None;
            }
        }
        self.anchor_summaries[slot] = AnchorState::Summary(summary);
        Some(summary)
    }

    /// Moves to grid anchor `(x_anchor, y_anchor, z_anchor)`: the centres of its 12 cells
    /// (`aquiferLocationCache`, computed on first use) in Vanilla's search order.
    fn set_anchor(&mut self, (x_anchor, y_anchor, z_anchor): (i32, i32, i32)) {
        let mut cell = 0;
        for x1 in 0..=1 {
            for y1 in -1..=1 {
                for z1 in 0..=1 {
                    let spaced_x = x_anchor + x1;
                    let spaced_y = y_anchor + y1;
                    let spaced_z = z_anchor + z1;
                    let index = self.index(spaced_x, spaced_y, spaced_z);
                    let existing = self.location_cache[index];
                    let location = if existing != NO_LOCATION {
                        existing
                    } else {
                        let mut random = self.positional.at(spaced_x, spaced_y, spaced_z);
                        let lx = from_grid_x(spaced_x, random.next_int_bounded(10));
                        let ly = from_grid_y(spaced_y, random.next_int_bounded(9));
                        let lz = from_grid_z(spaced_z, random.next_int_bounded(10));
                        let location = BlockPos::new(lx, ly, lz).as_long();
                        self.location_cache[index] = location;
                        location
                    };
                    let pos = BlockPos::from_long(location);
                    self.anchor_cells[cell] = (pos.x, pos.y, pos.z, index);
                    cell += 1;
                }
            }
        }
        self.anchor = Some((x_anchor, y_anchor, z_anchor));
    }

    /// `calculatePressure(…)`: how strongly a barrier separates two aquifer cells at the block.
    #[allow(clippy::too_many_arguments)]
    fn calculate_pressure(
        &self,
        program: &Program,
        ctx: &mut SamplerContext,
        block_x: i32,
        block_y: i32,
        block_z: i32,
        barrier_noise: &mut f64,
        status1: FluidStatus,
        status2: FluidStatus,
    ) -> f64 {
        let type1 = status1.at(block_y);
        let type2 = status2.at(block_y);
        if (Self::is(type1, ids::LAVA) && Self::is(type2, ids::WATER))
            || (Self::is(type1, ids::WATER) && Self::is(type2, ids::LAVA))
        {
            return 2.0;
        }
        let fluid_y_diff = status1
            .fluid_level
            .wrapping_sub(status2.fluid_level)
            .wrapping_abs();
        if fluid_y_diff == 0 {
            return 0.0;
        }
        let average_fluid_y =
            0.5 * f64::from(status1.fluid_level.wrapping_add(status2.fluid_level));
        let how_far_above_average = f64::from(block_y) + 0.5 - average_fluid_y;
        let base_value = f64::from(fluid_y_diff) / 2.0;
        let distance_from_edge = base_value - how_far_above_average.abs();
        let gradient = if how_far_above_average > 0.0 {
            let center_point = 0.0 + distance_from_edge;
            if center_point > 0.0 {
                center_point / 1.5
            } else {
                center_point / 2.5
            }
        } else {
            let center_point = 3.0 + distance_from_edge;
            if center_point > 0.0 {
                center_point / 3.0
            } else {
                center_point / 10.0
            }
        };
        // Java's `!(gradient < -2.0) && !(gradient > 2.0)`, which lets NaN through.
        #[allow(clippy::neg_cmp_op_on_partial_ord)]
        let in_band = !(gradient < -2.0) && !(gradient > 2.0);
        let noise_value = if in_band {
            if barrier_noise.is_nan() {
                let value = f64::from(program.sample_value(
                    ctx,
                    self.functions.barrier,
                    block_x,
                    block_y,
                    block_z,
                ));
                *barrier_noise = value;
                value
            } else {
                *barrier_noise
            }
        } else {
            0.0
        };
        2.0 * (noise_value + gradient)
    }

    /// `getAquiferStatus(index)`: the fluid of an aquifer cell, computed on first use.
    fn aquifer_status(
        &mut self,
        program: &Program,
        ctx: &mut SamplerContext,
        index: usize,
    ) -> FluidStatus {
        if let Some(status) = self.status_cache[index] {
            return status;
        }
        let pos = BlockPos::from_long(self.location_cache[index]);
        let status = self.compute_fluid(program, ctx, pos.x, pos.y, pos.z);
        self.status_cache[index] = Some(status);
        status
    }

    /// `computeFluid(x, y, z)`: the fluid of the aquifer cell centred at the position.
    fn compute_fluid(
        &mut self,
        program: &Program,
        ctx: &mut SamplerContext,
        x: i32,
        y: i32,
        z: i32,
    ) -> FluidStatus {
        let global_fluid = self.fluid_picker.compute_fluid(x, y, z);
        let mut lowest_preliminary_surface = i32::MAX;
        let top_of_aquifer_cell = y + 12;
        let bottom_of_aquifer_cell = y - 12;
        let mut surface_at_center_is_under_global_fluid_level = false;
        for [offset_x, offset_z] in SURFACE_SAMPLING_OFFSETS_IN_CHUNKS {
            let sample_x = x + (offset_x << 4);
            let sample_z = z + (offset_z << 4);
            let surface_level = self.surface_level(program, ctx, sample_x, sample_z);
            let adjusted_surface_level = adjust_surface_level(surface_level);
            let start = offset_x == 0 && offset_z == 0;
            if start && bottom_of_aquifer_cell > adjusted_surface_level {
                return global_fluid;
            }
            let pokes_above_surface = top_of_aquifer_cell > adjusted_surface_level;
            if pokes_above_surface || start {
                let global_fluid_at_surface =
                    self.fluid_picker
                        .compute_fluid(sample_x, adjusted_surface_level, sample_z);
                if !global_fluid_at_surface.at(adjusted_surface_level).is_air() {
                    if start {
                        surface_at_center_is_under_global_fluid_level = true;
                    }
                    if pokes_above_surface {
                        return global_fluid_at_surface;
                    }
                }
            }
            lowest_preliminary_surface = lowest_preliminary_surface.min(surface_level);
        }
        let fluid_surface_level = self.compute_surface_level(
            program,
            ctx,
            x,
            y,
            z,
            global_fluid,
            lowest_preliminary_surface,
            surface_at_center_is_under_global_fluid_level,
        );
        FluidStatus {
            fluid_level: fluid_surface_level,
            fluid_type: self.compute_fluid_type(
                program,
                ctx,
                x,
                y,
                z,
                global_fluid,
                fluid_surface_level,
            ),
        }
    }

    /// `computeSurfaceLevel(…)`: the fluid level of a cell from the floodedness noise.
    #[allow(clippy::too_many_arguments)]
    fn compute_surface_level(
        &self,
        program: &Program,
        ctx: &mut SamplerContext,
        x: i32,
        y: i32,
        z: i32,
        global_fluid: FluidStatus,
        lowest_surface_level: i32,
        surface_at_center_is_under_global_fluid_level: bool,
    ) -> i32 {
        let (partially_flooded, fully_flooded) =
            if f64::from(program.sample_value(ctx, self.functions.exclusion, x, y, z)) > 0.0 {
                (-1.0, -1.0)
            } else {
                let distance_below_surface = adjust_surface_level(lowest_surface_level) - y;
                let floodedness_factor = if surface_at_center_is_under_global_fluid_level {
                    clamped_map(f64::from(distance_below_surface), 0.0, 64.0, 1.0, 0.0)
                } else {
                    0.0
                };
                let floodedness_noise = clamp(
                    f64::from(program.sample_value(
                        ctx,
                        self.functions.fluid_level_floodedness,
                        x,
                        y,
                        z,
                    )),
                    -1.0,
                    1.0,
                );
                let fully_flooded_threshold = map(floodedness_factor, 1.0, 0.0, -0.3, 0.8);
                let partially_flooded_threshold = map(floodedness_factor, 1.0, 0.0, -0.8, 0.4);
                (
                    floodedness_noise - partially_flooded_threshold,
                    floodedness_noise - fully_flooded_threshold,
                )
            };
        if fully_flooded > 0.0 {
            global_fluid.fluid_level
        } else if partially_flooded > 0.0 {
            self.compute_randomized_fluid_surface_level(program, ctx, x, y, z, lowest_surface_level)
        } else {
            WAY_BELOW_MIN_Y
        }
    }

    /// `computeRandomizedFluidSurfaceLevel(…)`.
    fn compute_randomized_fluid_surface_level(
        &self,
        program: &Program,
        ctx: &mut SamplerContext,
        x: i32,
        y: i32,
        z: i32,
        lowest_surface_level: i32,
    ) -> i32 {
        let cell_x = x.div_euclid(16);
        let cell_y = y.div_euclid(40);
        let cell_z = z.div_euclid(16);
        let cell_middle_y = cell_y * 40 + 20;
        // `sampleValue(…) * 10.0F` in float, then widened.
        let spread = f64::from(
            program.sample_value(
                ctx,
                self.functions.fluid_level_spread,
                cell_x,
                cell_y,
                cell_z,
            ) * 10.0,
        );
        // `Mth.quantize(spread, 3)`.
        let spread_quantized = floor(spread / 3.0) * 3;
        let target = cell_middle_y + spread_quantized;
        lowest_surface_level.min(target)
    }

    /// `computeFluidType(…)`: lava instead of water in deep cells where the lava noise is
    /// strong.
    #[allow(clippy::too_many_arguments)]
    fn compute_fluid_type(
        &self,
        program: &Program,
        ctx: &mut SamplerContext,
        x: i32,
        y: i32,
        z: i32,
        global_fluid: FluidStatus,
        fluid_surface_level: i32,
    ) -> BlockState {
        let mut fluid_type = global_fluid.fluid_type;
        if fluid_surface_level <= -10
            && fluid_surface_level != WAY_BELOW_MIN_Y
            && global_fluid.fluid_type != self.lava
        {
            let cell_x = x.div_euclid(64);
            let cell_y = y.div_euclid(40);
            let cell_z = z.div_euclid(64);
            let lava_noise =
                f64::from(program.sample_value(ctx, self.functions.lava, cell_x, cell_y, cell_z));
            if lava_noise.abs() > 0.3 {
                fluid_type = self.lava;
            }
        }
        fluid_type
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_fluid_picker() {
        let water = ids::WATER.default_state();
        let picker = GlobalFluidPicker::new(63, water);
        assert_eq!(
            picker.compute_fluid(0, -55, 0).at(-55),
            ids::LAVA.default_state()
        );
        assert_eq!(picker.compute_fluid(0, -54, 0).at(-54), water);
        assert_eq!(picker.compute_fluid(0, 62, 0).at(62), water);
        assert!(picker.compute_fluid(0, 63, 0).at(63).is_air());
        // The nether: sea level 32 is above -54, so lava only below -54.
        let lava_sea = GlobalFluidPicker::new(32, ids::LAVA.default_state());
        assert_eq!(lava_sea.compute_fluid(0, 10, 0).fluid_level, 32);
        // A sea level below -54 moves the lava boundary down with it.
        let low = GlobalFluidPicker::new(-60, water);
        assert_eq!(low.compute_fluid(0, -59, 0).fluid_type, water);
        assert_eq!(low.compute_fluid(0, -61, 0).fluid_level, LAVA_LEVEL);
    }

    #[test]
    fn similarity_constants() {
        assert_eq!(FLOWING_UPDATE_SIMILARITY, similarity(100, 144));
        assert_eq!(WAY_BELOW_MIN_Y, -32512);
    }
}

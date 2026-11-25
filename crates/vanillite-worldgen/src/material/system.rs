//! `MaterialSystem`: builds the surface of a chunk by running the material rule tree down every
//! column, plus the eroded badlands pillars and frozen ocean icebergs.

use std::sync::Arc;

use vanillite_base::math::{floor, floor_div, java_min};
use vanillite_base::random::{PositionalRandomFactory, RandomSource};
use vanillite_chunk::BiomeId;
use vanillite_data::block::{BlockId, BlockState};
use vanillite_datapack::LoadedPacks;

use super::context::MaterialRuleContext;
use super::rule::{FoldedRules, YBoundsInput};
use super::rules::MaterialRules;
use crate::Error;
use crate::anchor::WorldGenerationContext;
use crate::biome::{BiomeData, BiomeRegistry};
use crate::blocks::default_state;
use crate::density::{DensityCompiler, DensityParser, FnId, Program, SamplerContext};
use crate::noise::Noise;
use crate::proto::{ProtoChunk, WgHeightmap};
use crate::random::AnyPositional;
use crate::random_state::RandomState;
use crate::settings::NoiseGeneratorSettings;
use crate::volume::DensityVolume;

/// Number of clay bands (`generateBands`).
pub const CLAY_BAND_COUNT: usize = 192;
/// `DimensionType.WAY_BELOW_MIN_Y`.
const WAY_BELOW_MIN_Y: i32 = -2032 << 4;

/// The compiled density functions of one ore vein occurrence.
#[derive(Clone, Copy, Debug)]
pub(super) struct OreVeinFns {
    pub density: FnId,
    pub richness: FnId,
    pub filler_gap: FnId,
    /// `[min, max)` block Y outside of which the density is a constant `<= 0` (the third
    /// value), so the vein never places a block and its richness is never read there.
    pub y_range: Option<(i32, i32, f32)>,
}

impl OreVeinFns {
    /// The part of `expected` the densities are prefilled on (`None` when empty): its Y extent
    /// is cut to the vein's Y range, widened to multiples of `y_period` so interpolation sees
    /// the same cells.
    pub(super) fn prefill_volume(
        &self,
        expected: &DensityVolume,
        y_period: i32,
    ) -> Option<DensityVolume> {
        let Some((min, max, _)) = self.y_range else {
            return Some(*expected);
        };
        let end = expected.min_y + expected.size_y;
        let from = expected.min_y.max(floor_div(min, y_period) * y_period);
        let to = end.min((floor_div(max - 1, y_period) + 1) * y_period);
        (to > from).then(|| {
            DensityVolume::new(
                expected.size_x,
                to - from,
                expected.size_z,
                expected.min_x,
                from,
                expected.min_z,
            )
        })
    }
}

/// The surface builder of one world's noise settings (`MaterialSystem`) together with its
/// parsed material rule (`NoiseGeneratorSettings.material_rule`).
///
/// Immutable after construction and shared by all chunk tasks; every call takes the chunk's
/// [`SamplerContext`] (`noiseChunk.cachingSamplers()`), which is used in Vanilla's order.
#[derive(Debug)]
pub struct MaterialSystem {
    default_block: BlockState,
    pub(super) sea_level: i32,
    generator_min_y: i32,
    generator_height: i32,
    /// `preliminarySurfaceFunction`: the router's `chunk_surface_level`.
    pub(super) preliminary_surface: FnId,
    clay_bands: [BlockState; CLAY_BAND_COUNT],
    clay_bands_offset_noise: Arc<Noise>,
    badlands_pillar_noise: Arc<Noise>,
    badlands_pillar_roof_noise: Arc<Noise>,
    badlands_surface_noise: Arc<Noise>,
    iceberg_pillar_noise: Arc<Noise>,
    iceberg_pillar_roof_noise: Arc<Noise>,
    iceberg_surface_noise: Arc<Noise>,
    noise_random: AnyPositional,
    surface_noise: Arc<Noise>,
    surface_secondary_noise: Arc<Noise>,
    pub(super) rules: MaterialRules,
    /// `rules` with no biome condition folded, for `topMaterial`.
    unfolded: FoldedRules,
    /// `rules.biome_folds(None)`.
    no_biome_folds: Vec<Option<bool>>,
    /// Noise instances of `rules.noises`.
    pub(super) noises: Vec<Arc<Noise>>,
    /// Random factories of `rules.random_names`.
    pub(super) gradient_randoms: Vec<AnyPositional>,
    /// Density functions of `rules.ore_veins`.
    pub(super) ore_vein_fns: Vec<OreVeinFns>,
    /// `getOrCreateRandomFactory("minecraft:ore")`.
    pub(super) ore_random: AnyPositional,
    /// [`Program::interpolation_y_period`] of the program the ore veins are compiled into.
    pub(super) ore_y_period: i32,
    eroded_badlands: Option<BiomeId>,
    frozen_ocean: Option<BiomeId>,
    deep_frozen_ocean: Option<BiomeId>,
    water: BlockId,
    snow_block: BlockState,
    packed_ice: BlockState,
}

impl MaterialSystem {
    /// The material system of `settings` for a world with `random_state`: creates the noises
    /// and clay bands (`new MaterialSystem(randomState, ...)`), parses `settings.material_rule`
    /// and compiles the density functions it references into `compiler`.
    pub fn new(
        packs: &LoadedPacks,
        settings: &NoiseGeneratorSettings,
        random_state: &RandomState,
        biomes: &BiomeRegistry,
        compiler: &mut DensityCompiler<'_>,
    ) -> Result<Self, Error> {
        let rules =
            MaterialRules::parse(packs, &settings.material_rule, &|name| biomes.id_of(name))?;
        let mut parser = DensityParser::new(packs);
        let mut compile = |json: &serde_json::Value| -> Result<FnId, Error> {
            Ok(compiler.compile(&parser.parse(json)?))
        };
        let ore_vein_fns = rules
            .ore_veins
            .iter()
            .map(|vein| {
                Ok(OreVeinFns {
                    density: compile(&vein.density)?,
                    richness: compile(&vein.richness)?,
                    filler_gap: compile(&vein.filler_gap)?,
                    y_range: None,
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;
        let program = compiler.program();
        let ore_vein_fns = ore_vein_fns
            .into_iter()
            .map(|fns| OreVeinFns {
                y_range: program
                    .y_range_choice(fns.density)
                    .filter(|&(_, _, outside)| outside <= 0.0),
                ..fns
            })
            .collect();
        let ore_y_period = program.interpolation_y_period();
        let preliminary_surface = compiler.compile(&settings.router.chunk_surface_level);
        let noise_random = *random_state.random();
        let noise = |id: &str| random_state.noise(id);
        Ok(Self {
            default_block: settings.default_block,
            sea_level: settings.sea_level,
            generator_min_y: settings.min_y,
            generator_height: settings.height,
            preliminary_surface,
            clay_bands_offset_noise: noise("minecraft:clay_bands_offset"),
            clay_bands: generate_bands(&mut noise_random.with_hash_of("minecraft:clay_bands")),
            surface_noise: noise("minecraft:surface"),
            surface_secondary_noise: noise("minecraft:surface_secondary"),
            badlands_pillar_noise: noise("minecraft:badlands_pillar"),
            badlands_pillar_roof_noise: noise("minecraft:badlands_pillar_roof"),
            badlands_surface_noise: noise("minecraft:badlands_surface"),
            iceberg_pillar_noise: noise("minecraft:iceberg_pillar"),
            iceberg_pillar_roof_noise: noise("minecraft:iceberg_pillar_roof"),
            iceberg_surface_noise: noise("minecraft:iceberg_surface"),
            noise_random,
            noises: rules.noises.iter().map(|key| noise(&key.id)).collect(),
            gradient_randoms: rules
                .random_names
                .iter()
                .map(|name| random_state.random_factory(name))
                .collect(),
            ore_vein_fns,
            ore_y_period,
            ore_random: random_state.random_factory("minecraft:ore"),
            unfolded: FoldedRules::new(&rules, &rules.biome_folds(None), None),
            no_biome_folds: rules.biome_folds(None),
            rules,
            eroded_badlands: biomes.id_of("minecraft:eroded_badlands"),
            frozen_ocean: biomes.id_of("minecraft:frozen_ocean"),
            deep_frozen_ocean: biomes.id_of("minecraft:deep_frozen_ocean"),
            water: BlockId::by_name("minecraft:water").expect("water block"),
            snow_block: default_state("minecraft:snow_block"),
            packed_ice: default_state("minecraft:packed_ice"),
        })
    }

    /// `getSeaLevel()`.
    pub fn sea_level(&self) -> i32 {
        self.sea_level
    }

    /// The clay bands of the badlands (`clayBands`).
    pub fn clay_bands(&self) -> &[BlockState; CLAY_BAND_COUNT] {
        &self.clay_bands
    }

    /// `new WorldGenerationContext(generator, chunk.getHeightAccessorForGeneration())`.
    pub fn generation_context(&self, chunk: &ProtoChunk) -> WorldGenerationContext {
        WorldGenerationContext::clamped(
            self.generator_min_y,
            self.generator_height,
            self.sea_level,
            chunk.min_y(),
            chunk.height(),
        )
    }

    /// `buildSurface`: applies the material rule to every solid block of the chunk, top down per
    /// column, and adds eroded badlands pillars and frozen ocean icebergs.
    ///
    /// `noise_volume` is the noise chunk's volume (`noiseChunk.volume()`), `samplers` its caching
    /// context, `biome_at` the zoomed biome lookup (`biomeManager::getBiome`, block coordinates)
    /// and `possible_biomes` the biomes the chunk may contain.
    #[allow(clippy::too_many_arguments)]
    pub fn build_surface(
        &self,
        program: &Program,
        samplers: &mut SamplerContext,
        chunk: &mut ProtoChunk,
        biomes: &BiomeRegistry,
        noise_volume: &DensityVolume,
        biome_at: &dyn Fn(i32, i32, i32) -> BiomeId,
        possible_biomes: &[BiomeId],
    ) {
        let chunk_pos = chunk.pos();
        let min_block_x = chunk_pos.min_block_x();
        let min_block_z = chunk_pos.min_block_z();
        let min_y = chunk.min_y();
        let max_y = chunk.max_y();
        let highest_section_y = chunk.highest_filled_section_index() + (min_y >> 4);
        let max_block_y = highest_section_y * 16 + 15;
        let narrowed_volume = DensityVolume::new(
            noise_volume.size_x,
            (max_block_y - noise_volume.min_y + 1).max(1),
            noise_volume.size_z,
            noise_volume.min_x,
            noise_volume.min_y,
            noise_volume.min_z,
        );
        let generation = self.generation_context(chunk);
        let biome_folds = self.rules.biome_folds(Some(possible_biomes));
        let rules = FoldedRules::new(
            &self.rules,
            &biome_folds,
            Some(&YBoundsInput {
                generation: &generation,
                ore_veins: &self.ore_vein_fns,
            }),
        );
        let mut context = MaterialRuleContext::compile(
            self,
            program,
            samplers,
            biomes,
            biome_at,
            generation,
            narrowed_volume,
            &rules,
            biome_folds,
        );

        for x in 0..16 {
            for z in 0..16 {
                let block_x = min_block_x + x as i32;
                let block_z = min_block_z + z as i32;
                let starting_height = chunk.height_at(WgHeightmap::WorldSurface, x, z) + 1;
                let surface_biome = biome_at(block_x, starting_height, block_z);
                if Some(surface_biome) == self.eroded_badlands {
                    self.eroded_badlands_extension(chunk, block_x, block_z, starting_height);
                }

                let height = chunk.height_at(WgHeightmap::WorldSurface, x, z) + 1;
                let gradient_x = surface_gradient_x(chunk, x, z);
                let gradient_z = surface_gradient_z(chunk, x, z);
                context.update_xz(block_x, block_z, gradient_x, gradient_z);
                let mut stone_above_depth = 0;
                let mut water_height = i32::MIN;
                let mut next_ceiling_stone_y = i32::MAX;
                let end_y = chunk.min_y();

                for y in (end_y..=height).rev() {
                    let old = chunk.block(block_x, y, block_z);
                    if old.is_air() {
                        stone_above_depth = 0;
                        water_height = i32::MIN;
                    } else if !old.fluid_state().is_empty() {
                        if water_height == i32::MIN {
                            water_height = y + 1;
                        }
                    } else {
                        if next_ceiling_stone_y >= y {
                            next_ceiling_stone_y = WAY_BELOW_MIN_Y;
                            for lookahead_y in (end_y - 1..y).rev() {
                                if !is_stone(chunk.block(block_x, lookahead_y, block_z)) {
                                    next_ceiling_stone_y = lookahead_y + 1;
                                    break;
                                }
                            }
                        }

                        stone_above_depth += 1;
                        let stone_below_depth = y - next_ceiling_stone_y + 1;
                        context.update_y(stone_above_depth, stone_below_depth, water_height, y);
                        if y >= min_y
                            && y <= max_y
                            && let Some(state) = context.try_apply(block_x, y, block_z)
                        {
                            set_column_block(chunk, block_x, y, block_z, state);
                        }
                    }
                }

                if Some(surface_biome) == self.frozen_ocean
                    || Some(surface_biome) == self.deep_frozen_ocean
                {
                    let min_surface_level = context.min_surface_level();
                    self.frozen_ocean_extension(
                        min_surface_level,
                        biomes.get(surface_biome),
                        chunk,
                        block_x,
                        block_z,
                        starting_height,
                    );
                }
            }
        }
    }

    /// `topMaterial`: the block the material rule puts at the top of a column at `(x, y, z)`,
    /// as carvers use it to restore grass below carved-out blocks. Like Vanilla, the rule is
    /// compiled for this one position (ore vein densities and the preliminary surface are
    /// sampled through `samplers` for it).
    #[allow(clippy::too_many_arguments)]
    pub fn top_material(
        &self,
        program: &Program,
        samplers: &mut SamplerContext,
        chunk: &ProtoChunk,
        biomes: &BiomeRegistry,
        biome_at: &dyn Fn(i32, i32, i32) -> BiomeId,
        x: i32,
        y: i32,
        z: i32,
        under_fluid: bool,
    ) -> Option<BlockState> {
        let mut context = MaterialRuleContext::compile(
            self,
            program,
            samplers,
            biomes,
            biome_at,
            self.generation_context(chunk),
            DensityVolume::new(1, 1, 1, x, y, z),
            &self.unfolded,
            self.no_biome_folds.clone(),
        );
        let (local_x, local_z) = ((x & 15) as usize, (z & 15) as usize);
        let gradient_x = surface_gradient_x(chunk, local_x, local_z);
        let gradient_z = surface_gradient_z(chunk, local_x, local_z);
        context.update_xz(x, z, gradient_x, gradient_z);
        let water_height = if under_fluid { y + 1 } else { i32::MIN };
        context.update_y(1, 1, water_height, y);
        context.try_apply(x, y, z)
    }

    /// `getSurfaceDepth(x, z)`: the depth of the surface layer of a column.
    pub fn surface_depth(&self, block_x: i32, block_z: i32) -> i32 {
        let noise_value = f64::from(self.surface_noise.get(
            f64::from(block_x),
            0.0,
            f64::from(block_z),
        ));
        (noise_value * 2.75 + 3.0 + self.noise_random.at(block_x, 0, block_z).next_double() * 0.25)
            as i32
    }

    /// `getSurfaceSecondary(x, z)`.
    pub fn surface_secondary(&self, block_x: i32, block_z: i32) -> f64 {
        f64::from(
            self.surface_secondary_noise
                .get(f64::from(block_x), 0.0, f64::from(block_z)),
        )
    }

    /// `getBand(x, y, z)`: the clay band at a position.
    pub fn band(&self, block_x: i32, y: i32, block_z: i32) -> BlockState {
        let offset = crate::mth::round_f(
            self.clay_bands_offset_noise
                .get(f64::from(block_x), 0.0, f64::from(block_z))
                * 4.0,
        );
        let len = CLAY_BAND_COUNT as i32;
        self.clay_bands[(y.wrapping_add(offset).wrapping_add(len) % len) as usize]
    }

    /// `erodedBadlandsExtension`: terracotta pillars above eroded badlands.
    fn eroded_badlands_extension(
        &self,
        chunk: &mut ProtoChunk,
        block_x: i32,
        block_z: i32,
        height: i32,
    ) {
        let (x, z) = (f64::from(block_x), f64::from(block_z));
        let pillar_buffer = java_min(
            (f64::from(self.badlands_surface_noise.get(x, 0.0, z)) * 8.25).abs(),
            f64::from(self.badlands_pillar_noise.get(x * 0.2, 0.0, z * 0.2) * 15.0),
        );
        if pillar_buffer <= 0.0 {
            return;
        }
        let pillar_floor =
            (f64::from(self.badlands_pillar_roof_noise.get(x * 0.75, 0.0, z * 0.75)) * 1.5).abs();
        let extension_top = 64.0
            + java_min(
                pillar_buffer * pillar_buffer * 2.5,
                (pillar_floor * 50.0).ceil() + 24.0,
            );
        let start_y = floor(extension_top);
        if height > start_y {
            return;
        }
        let min_y = chunk.min_y();
        for y in (min_y..=start_y).rev() {
            let old = chunk.block(block_x, y, block_z).block();
            if old == self.default_block.block() {
                break;
            }
            if old == self.water {
                return;
            }
        }
        let mut y = start_y;
        while y >= min_y && chunk.block(block_x, y, block_z).is_air() {
            set_column_block(chunk, block_x, y, block_z, self.default_block);
            y -= 1;
        }
    }

    /// `frozenOceanExtension`: icebergs in frozen oceans.
    fn frozen_ocean_extension(
        &self,
        min_surface_level: i32,
        surface_biome: &BiomeData,
        chunk: &mut ProtoChunk,
        block_x: i32,
        block_z: i32,
        height: i32,
    ) {
        let (x, z) = (f64::from(block_x), f64::from(block_z));
        let iceberg = java_min(
            (f64::from(self.iceberg_surface_noise.get(x, 0.0, z)) * 8.25).abs(),
            f64::from(self.iceberg_pillar_noise.get(x * 1.28, 0.0, z * 1.28) * 15.0),
        );
        if iceberg <= 1.8 {
            return;
        }
        let iceberg_roof =
            (f64::from(self.iceberg_pillar_roof_noise.get(x * 1.17, 0.0, z * 1.17)) * 1.5).abs();
        let mut top = java_min(iceberg * iceberg * 1.2, (iceberg_roof * 40.0).ceil() + 14.0);
        let sea_level = self.sea_level;
        if should_melt_frozen_ocean_iceberg_slightly(
            surface_biome,
            block_x,
            sea_level,
            block_z,
            sea_level,
        ) {
            top -= 2.0;
        }
        if top <= 2.0 {
            return;
        }
        let extension_bottom = f64::from(sea_level) - top - 7.0;
        top += f64::from(sea_level);
        let extension_top = top;
        let mut random = self.noise_random.at(block_x, 0, block_z);
        let max_snow_depth = 2 + random.next_int_bounded(4);
        let min_snow_height = sea_level + 18 + random.next_int_bounded(10);
        let mut snow_depth = 0;
        let start_y = height.max((top as i32).wrapping_add(1));
        for y in (min_surface_level..=start_y).rev() {
            let place = (chunk.block(block_x, y, block_z).is_air()
                && y < extension_top as i32
                && random.next_double() > 0.01)
                || (chunk.block(block_x, y, block_z).block() == self.water
                    && y > extension_bottom as i32
                    && y < sea_level
                    && random.next_double() > 0.15);
            if place {
                if snow_depth <= max_snow_depth && y > min_snow_height {
                    set_column_block(chunk, block_x, y, block_z, self.snow_block);
                    snow_depth += 1;
                } else {
                    set_column_block(chunk, block_x, y, block_z, self.packed_ice);
                }
            }
        }
    }
}

/// `generateBands(random)`: the 192 terracotta bands of the badlands.
pub fn generate_bands(random: &mut impl RandomSource) -> [BlockState; CLAY_BAND_COUNT] {
    let terracotta = default_state("minecraft:terracotta");
    let mut bands = [terracotta; CLAY_BAND_COUNT];
    let len = CLAY_BAND_COUNT as i32;

    let orange = default_state("minecraft:orange_terracotta");
    let mut i = 0;
    while i < len {
        i += random.next_int_bounded(5) + 1;
        if i < len {
            bands[i as usize] = orange;
        }
        i += 1;
    }

    make_bands(
        random,
        &mut bands,
        1,
        default_state("minecraft:yellow_terracotta"),
    );
    make_bands(
        random,
        &mut bands,
        2,
        default_state("minecraft:brown_terracotta"),
    );
    make_bands(
        random,
        &mut bands,
        1,
        default_state("minecraft:red_terracotta"),
    );

    let white = default_state("minecraft:white_terracotta");
    let light_gray = default_state("minecraft:light_gray_terracotta");
    let white_band_count = random.next_int_between_inclusive(9, 15);
    let mut count = 0;
    let mut start = 0;
    while count < white_band_count && start < len {
        bands[start as usize] = white;
        if start - 1 > 0 && random.next_boolean() {
            bands[start as usize - 1] = light_gray;
        }
        if start + 1 < len && random.next_boolean() {
            bands[start as usize + 1] = light_gray;
        }
        count += 1;
        start += random.next_int_bounded(16) + 4;
    }
    bands
}

/// `makeBands`: `6..=15` bands of `state`, each `base_width..base_width + 2` wide.
fn make_bands(
    random: &mut impl RandomSource,
    bands: &mut [BlockState; CLAY_BAND_COUNT],
    base_width: i32,
    state: BlockState,
) {
    let len = CLAY_BAND_COUNT as i32;
    let band_count = random.next_int_between_inclusive(6, 15);
    for _ in 0..band_count {
        let width = base_width + random.next_int_bounded(3);
        let start = random.next_int_bounded(len);
        let mut p = 0;
        while start + p < len && p < width {
            bands[(start + p) as usize] = state;
            p += 1;
        }
    }
}

/// `getSurfaceGradientX`.
fn surface_gradient_x(chunk: &ProtoChunk, x: usize, z: usize) -> i32 {
    chunk.height_at(WgHeightmap::WorldSurface, (x + 1).min(15), z)
        - chunk.height_at(WgHeightmap::WorldSurface, x.saturating_sub(1), z)
}

/// `getSurfaceGradientZ`.
fn surface_gradient_z(chunk: &ProtoChunk, x: usize, z: usize) -> i32 {
    chunk.height_at(WgHeightmap::WorldSurface, x, (z + 1).min(15))
        - chunk.height_at(WgHeightmap::WorldSurface, x, z.saturating_sub(1))
}

/// `isStone`: neither air nor fluid.
fn is_stone(state: BlockState) -> bool {
    !state.is_air() && state.fluid_state().is_empty()
}

/// The `BlockColumn.setBlock` of `buildSurface`: sets blocks inside the chunk's height and
/// marks fluids for post-processing.
fn set_column_block(chunk: &mut ProtoChunk, x: i32, y: i32, z: i32, state: BlockState) {
    if y >= chunk.min_y() && y <= chunk.max_y() {
        chunk.set_block(x, y, z, state);
        if !state.fluid_state().is_empty() {
            chunk.mark_post_processing(x, y, z);
        }
    }
}

/// `Biome.coldEnoughToSnow(pos, seaLevel)`.
pub(super) fn cold_enough_to_snow(
    biome: &BiomeData,
    x: i32,
    y: i32,
    z: i32,
    sea_level: i32,
) -> bool {
    biome.cold_enough_to_snow(x, y, z, sea_level)
}

/// `Biome.shouldMeltFrozenOceanIcebergSlightly(pos, seaLevel)`.
fn should_melt_frozen_ocean_iceberg_slightly(
    biome: &BiomeData,
    x: i32,
    y: i32,
    z: i32,
    sea_level: i32,
) -> bool {
    biome.should_melt_frozen_ocean_iceberg_slightly(x, y, z, sea_level)
}

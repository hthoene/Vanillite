//! Carvers (`net.minecraft.world.level.levelgen.carver`): caves and canyons cut into the
//! terrain after the surface is placed.
//!
//! Ports `WorldCarver` (with `carveEllipsoid` and `canReach`), `CaveWorldCarver`,
//! `CanyonWorldCarver`, `WorldCarverTypes`, `CarvingMask` and the carver loop of
//! `NoiseBasedChunkGenerator.generateCarvers` ([`carve_chunk`]).
//!
//! In 26.3 carvers only mark blocks in a [`CarvingMask`]; they never read or write the chunk.
//! The generator then applies the mask (`applyCarvingMask`: uncarvable blocks, the aquifer with
//! density `0.0`, top material under carved grass).

mod canyon;
mod cave;
mod mask;
pub mod providers;

use serde_json::Value;
use vanillite_base::ChunkPos;
use vanillite_base::math::floor;
use vanillite_base::random::{
    LegacyPositionalFactory, LegacyRandom, PositionalRandomFactory, RandomSource,
};
use vanillite_datapack::LoadedPacks;

use crate::Error;
use crate::random::WorldgenRandom;

pub use canyon::{CanyonCarver, CanyonShape};
pub use cave::CaveCarver;
pub use mask::CarvingMask;
pub use providers::WorldGenerationContext;

/// `WorldCarver.getRange()`: how many chunks a carver started in one chunk may reach.
const RANGE: i32 = 4;

/// Chunks carved from each side of the target chunk (`generateCarvers`' `range`).
const SOURCE_RANGE: i32 = 8;

/// Blocks at the top of the generation range that carvers leave alone (`protectedBlocksOnTop`
/// for chunks that are not being upgraded).
const PROTECTED_BLOCKS_ON_TOP: i32 = 7;

/// A configured carver (`WorldCarver`), dispatched on its type (`WorldCarverTypes`).
#[derive(Clone, Debug, PartialEq)]
pub enum WorldCarver {
    /// `minecraft:cave`.
    Cave(CaveCarver),
    /// `minecraft:canyon`.
    Canyon(CanyonCarver),
}

impl WorldCarver {
    /// Parses `WorldCarver.DIRECT_CODEC`.
    pub fn from_json(json: &Value) -> Result<Self, Error> {
        let kind = json
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::new("carver without type"))?;
        match kind.strip_prefix("minecraft:").unwrap_or(kind) {
            "cave" => Ok(Self::Cave(CaveCarver::from_json(json)?)),
            "canyon" => Ok(Self::Canyon(CanyonCarver::from_json(json)?)),
            other => Err(Error::new(format!("unknown carver type {other}"))),
        }
    }

    /// `isStartChunk(random)`: `random.nextFloat() <= probability`.
    pub fn is_start_chunk<R: RandomSource>(&self, random: &mut R) -> bool {
        let probability = match self {
            Self::Cave(c) => c.probability,
            Self::Canyon(c) => c.probability,
        };
        random.next_float() <= probability
    }

    /// `carve(context, random, chunkPos, sourceChunkPos, output)`: marks the blocks of `chunk`
    /// that the carver started in `source` cuts.
    pub fn carve<R: RandomSource>(
        &self,
        context: &WorldGenerationContext,
        random: &mut R,
        chunk: ChunkPos,
        source: ChunkPos,
        output: &mut CarvingMask,
    ) {
        match self {
            Self::Cave(c) => c.carve(context, random, chunk, source, output),
            Self::Canyon(c) => c.carve(context, random, chunk, source, output),
        }
    }
}

/// Handle of a carver in a [`CarverRegistry`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CarverId(pub u32);

/// The `minecraft:worldgen/carver` registry.
#[derive(Clone, Debug, Default)]
pub struct CarverRegistry {
    names: Vec<String>,
    carvers: Vec<WorldCarver>,
}

impl CarverRegistry {
    /// Reads `minecraft:worldgen/carver` of `packs`.
    pub fn load(packs: &LoadedPacks) -> Result<Self, Error> {
        let mut registry = Self::default();
        if let Some(entries) = packs.registry("minecraft:worldgen/carver") {
            for entry in entries.entries() {
                let json: Value = serde_json::from_str(entry.json.get())
                    .map_err(|e| Error::new(format!("carver {}: {e}", entry.id)))?;
                let carver = WorldCarver::from_json(&json)
                    .map_err(|e| Error::new(format!("carver {}: {e}", entry.id)))?;
                registry.insert(entry.id.clone(), carver);
            }
        }
        Ok(registry)
    }

    /// Adds (or replaces) a carver.
    pub fn insert(&mut self, name: String, carver: WorldCarver) -> CarverId {
        if let Some(id) = self.id(&name) {
            self.carvers[id.0 as usize] = carver;
            return id;
        }
        self.names.push(name);
        self.carvers.push(carver);
        CarverId(self.carvers.len() as u32 - 1)
    }

    /// The id of a carver by registry name (`minecraft:` may be omitted).
    pub fn id(&self, name: &str) -> Option<CarverId> {
        let name = name.strip_prefix('#').unwrap_or(name);
        let qualified;
        let name = if name.contains(':') {
            name
        } else {
            qualified = format!("minecraft:{name}");
            &qualified
        };
        self.names
            .iter()
            .position(|n| n == name)
            .map(|i| CarverId(i as u32))
    }

    /// The ids of a biome's carver list, failing on unknown names.
    pub fn ids<S: AsRef<str>>(&self, names: &[S]) -> Result<Vec<CarverId>, Error> {
        names
            .iter()
            .map(|n| {
                self.id(n.as_ref())
                    .ok_or_else(|| Error::new(format!("unknown carver {}", n.as_ref())))
            })
            .collect()
    }

    /// A carver by id.
    pub fn get(&self, id: CarverId) -> &WorldCarver {
        &self.carvers[id.0 as usize]
    }

    /// The registry name of a carver.
    pub fn name(&self, id: CarverId) -> &str {
        &self.names[id.0 as usize]
    }

    /// Number of carvers.
    pub fn len(&self) -> usize {
        self.carvers.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.carvers.is_empty()
    }
}

/// The carver loop of `NoiseBasedChunkGenerator.generateCarvers`: runs the carvers of every
/// chunk within 8 chunks of `chunk` and returns the blocks they cut in `chunk`.
///
/// `biome_carvers(source)` is the carver list of the biome at
/// `(QuartPos.fromBlock(source.minBlockX), 0, QuartPos.fromBlock(source.minBlockZ))` from an
/// uncached biome resolver (`getBiomeGenerationSettingsForCarver`). The mask spans
/// `min_gen_y + 1 ..= min_gen_y + gen_depth - 8` (a chunk that is not being upgraded).
pub fn carve_chunk<'a>(
    registry: &CarverRegistry,
    biome_carvers: &dyn Fn(ChunkPos) -> &'a [CarverId],
    seed: i64,
    chunk: ChunkPos,
    context: &WorldGenerationContext,
) -> CarvingMask {
    let max_y = context.min_gen_y() + context.gen_depth() - 1 - PROTECTED_BLOCKS_ON_TOP;
    let mut mask = CarvingMask::new(context.min_gen_y() + 1, max_y);
    // Vanilla seeds this from `RandomSupport.generateUniqueSeed()`, but every use reseeds it.
    let mut random = CarverRandom::new();
    for dx in -SOURCE_RANGE..=SOURCE_RANGE {
        for dz in -SOURCE_RANGE..=SOURCE_RANGE {
            let source = ChunkPos::new(chunk.x + dx, chunk.z + dz);
            for (index, &id) in biome_carvers(source).iter().enumerate() {
                let carver = registry.get(id);
                random.set_large_feature_seed(seed.wrapping_add(index as i64), source.x, source.z);
                if carver.is_start_chunk(&mut random) {
                    carver.carve(context, &mut random, chunk, source, &mut mask);
                }
            }
        }
    }
    mask
}

/// `new WorldgenRandom(new LegacyRandomSource(seed))` exactly as the carver loop sees it.
///
/// `WorldgenRandom` extends `LegacyRandomSource` but overrides `setSeed` to reseed only the
/// wrapped source, so its own Gaussian cache (`nextGaussian` of the outer object) is never
/// reset between carvers. Only providers with a normal distribution observe this.
#[derive(Clone, Debug)]
pub struct CarverRandom {
    inner: WorldgenRandom,
    next_next_gaussian: Option<f64>,
}

impl CarverRandom {
    /// A generator to be seeded with [`Self::set_large_feature_seed`].
    pub fn new() -> Self {
        Self {
            inner: WorldgenRandom::new(0),
            next_next_gaussian: None,
        }
    }

    /// `setLargeFeatureSeed(seed, chunkX, chunkZ)`.
    pub fn set_large_feature_seed(&mut self, seed: i64, chunk_x: i32, chunk_z: i32) {
        self.inner.set_large_feature_seed(seed, chunk_x, chunk_z);
    }
}

impl From<LegacyRandom> for CarverRandom {
    /// A plain `LegacyRandomSource` (what `fork` and positional factories hand out).
    fn from(random: LegacyRandom) -> Self {
        Self {
            inner: WorldgenRandom { random },
            next_next_gaussian: None,
        }
    }
}

/// `LegacyRandomSource.LegacyPositionalRandomFactory` handing out [`CarverRandom`]s.
#[derive(Clone, Copy, Debug)]
pub struct CarverPositional(pub LegacyPositionalFactory);

impl PositionalRandomFactory for CarverPositional {
    type Random = CarverRandom;

    fn at(&self, x: i32, y: i32, z: i32) -> CarverRandom {
        self.0.at(x, y, z).into()
    }

    fn with_hash_of(&self, name: &str) -> CarverRandom {
        self.0.with_hash_of(name).into()
    }

    fn with_seed(&self, seed: i64) -> CarverRandom {
        self.0.with_seed(seed).into()
    }
}

impl Default for CarverRandom {
    fn default() -> Self {
        Self::new()
    }
}

impl RandomSource for CarverRandom {
    type Positional = CarverPositional;

    fn set_seed(&mut self, seed: i64) {
        self.inner.random.set_seed(seed);
    }

    fn fork(&mut self) -> Self {
        Self::from(self.inner.random.fork())
    }

    fn fork_positional(&mut self) -> CarverPositional {
        CarverPositional(self.inner.random.fork_positional())
    }

    fn next_int(&mut self) -> i32 {
        self.inner.random.next_int()
    }

    fn next_int_bounded(&mut self, bound: i32) -> i32 {
        self.inner.random.next_int_bounded(bound)
    }

    fn next_long(&mut self) -> i64 {
        self.inner.random.next_long()
    }

    fn next_boolean(&mut self) -> bool {
        self.inner.random.next_boolean()
    }

    fn next_float(&mut self) -> f32 {
        self.inner.random.next_float()
    }

    fn next_double(&mut self) -> f64 {
        self.inner.random.next_double()
    }

    /// `MarsagliaPolarGaussian.nextGaussian()` over the outer object's own cache.
    fn next_gaussian(&mut self) -> f64 {
        if let Some(value) = self.next_next_gaussian.take() {
            return value;
        }
        loop {
            let x = 2.0 * self.next_double() - 1.0;
            let y = 2.0 * self.next_double() - 1.0;
            let radius_squared = x * x + y * y;
            if radius_squared < 1.0 && radius_squared != 0.0 {
                let multiplier = (-2.0 * radius_squared.ln() / radius_squared).sqrt();
                self.next_next_gaussian = Some(y * multiplier);
                return x * multiplier;
            }
        }
    }

    fn consume_count(&mut self, count: i32) {
        self.inner.random.consume_count(count);
    }
}

/// `chunkPos.getBlockX(offset)`.
#[inline]
fn block_x(chunk: ChunkPos, offset: i32) -> i32 {
    chunk.min_block_x() + offset
}

/// `chunkPos.getBlockZ(offset)`.
#[inline]
fn block_z(chunk: ChunkPos, offset: i32) -> i32 {
    chunk.min_block_z() + offset
}

/// The `probability` field: `Codec.floatRange(0, 1)`.
fn probability(json: &Value) -> Result<f32, Error> {
    let p = json
        .get("probability")
        .and_then(Value::as_f64)
        .ok_or_else(|| Error::new("carver: missing probability"))? as f32;
    if (0.0..=1.0).contains(&p) {
        Ok(p)
    } else {
        Err(Error::new(format!("carver: probability out of range: {p}")))
    }
}

/// `WorldCarver.carveEllipsoid(…)`: marks the blocks of `chunk` inside an ellipsoid unless
/// `skip(xd, yd, zd, y)` says otherwise.
#[allow(clippy::too_many_arguments)]
fn carve_ellipsoid(
    chunk: ChunkPos,
    x: f64,
    y: f64,
    z: f64,
    horizontal_radius: f64,
    vertical_radius: f64,
    output: &mut CarvingMask,
    skip: impl Fn(f64, f64, f64, i32) -> bool,
) {
    let center_x = f64::from(block_x(chunk, 8));
    let center_z = f64::from(block_z(chunk, 8));
    let max_delta = 16.0 + horizontal_radius * 2.0;
    if (x - center_x).abs() > max_delta || (z - center_z).abs() > max_delta {
        return;
    }
    let chunk_min_x = chunk.min_block_x();
    let chunk_min_z = chunk.min_block_z();
    let min_x_index = (floor(x - horizontal_radius) - chunk_min_x - 1).max(0);
    let max_x_index = (floor(x + horizontal_radius) - chunk_min_x).min(15);
    let min_y = (floor(y - vertical_radius) - 1).max(output.min_y());
    let max_y = (floor(y + vertical_radius) + 1).min(output.max_y());
    let min_z_index = (floor(z - horizontal_radius) - chunk_min_z - 1).max(0);
    let max_z_index = (floor(z + horizontal_radius) - chunk_min_z).min(15);
    for x_index in min_x_index..=max_x_index {
        let world_x = block_x(chunk, x_index);
        let xd = (f64::from(world_x) + 0.5 - x) / horizontal_radius;
        for z_index in min_z_index..=max_z_index {
            let world_z = block_z(chunk, z_index);
            let zd = (f64::from(world_z) + 0.5 - z) / horizontal_radius;
            // `!(xd * xd + zd * zd >= 1.0)`: NaN carves on, like Java.
            #[allow(clippy::neg_cmp_op_on_partial_ord)]
            let inside = !(xd * xd + zd * zd >= 1.0);
            if inside {
                let mut world_y = max_y;
                while world_y > min_y {
                    let yd = (f64::from(world_y) - 0.5 - y) / vertical_radius;
                    if !skip(xd, yd, zd, world_y) {
                        output.carve(x_index, world_y, z_index);
                    }
                    world_y -= 1;
                }
            }
        }
    }
}

/// `WorldCarver.canReach(…)`: whether a tunnel at `x, z` can still reach `chunk` in its
/// remaining steps.
fn can_reach(
    chunk: ChunkPos,
    x: f64,
    z: f64,
    current_step: i32,
    total_steps: i32,
    thickness: f32,
) -> bool {
    let x_mid = f64::from(block_x(chunk, 8));
    let z_mid = f64::from(block_z(chunk, 8));
    let xd = x - x_mid;
    let zd = z - z_mid;
    let remaining = f64::from(total_steps - current_step);
    let rr = f64::from(thickness + 2.0 + 16.0);
    xd * xd + zd * zd - remaining * remaining <= rr * rr
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::packs;

    #[test]
    fn loads_vanilla_carvers() {
        let registry = CarverRegistry::load(packs()).expect("carvers");
        for name in ["cave", "cave_extra_underground", "canyon", "nether_cave"] {
            assert!(registry.id(name).is_some(), "{name}");
        }
        let nether = registry.get(registry.id("minecraft:nether_cave").unwrap());
        let WorldCarver::Cave(cave) = nether else {
            panic!("nether_cave is a cave carver")
        };
        assert_eq!(
            cave.start_vertical_radius_multiplier,
            providers::FloatProvider::Constant(5.0)
        );
        assert!(!cave.weird_thickness_bias);
    }

    #[test]
    fn carver_random_keeps_gaussian_across_reseeds() {
        let mut random = CarverRandom::new();
        random.set_large_feature_seed(1, 2, 3);
        let first = random.next_gaussian();
        random.set_large_feature_seed(1, 2, 3);
        let second = random.next_gaussian();
        assert_ne!(first.to_bits(), second.to_bits());
    }
}

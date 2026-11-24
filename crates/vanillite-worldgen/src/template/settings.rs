//! `StructurePlaceSettings` and the random sources template placement reads.

use vanillite_base::BlockPos;
use vanillite_base::math::get_seed;
use vanillite_base::random::{LegacyRandom, RandomSource};

use super::bounding_box::BoundingBox;
use super::processor::StructureProcessor;
use super::structure::Palette;
use super::transform::{Mirror, Rotation};

/// The random calls template placement and its processors make, object safe so placement is not
/// generic over the caller's generator (any [`RandomSource`] is one).
pub trait TemplateRandom {
    /// `nextInt(bound)`.
    fn next_int(&mut self, bound: i32) -> i32;
    /// `nextFloat()`.
    fn next_float(&mut self) -> f32;
    /// `nextLong()`.
    fn next_long(&mut self) -> i64;
}

impl<R: RandomSource> TemplateRandom for R {
    fn next_int(&mut self, bound: i32) -> i32 {
        self.next_int_bounded(bound)
    }

    fn next_float(&mut self) -> f32 {
        RandomSource::next_float(self)
    }

    fn next_long(&mut self) -> i64 {
        RandomSource::next_long(self)
    }
}

/// `StructurePlaceSettings.random`: which generator `getRandom(pos)` returns.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SettingsRandom {
    /// `null` (every structure piece): a fresh `LegacyRandomSource(Mth.getSeed(pos))` per call.
    #[default]
    Positional,
    /// `setRandom(random)` with the generator passed to `placeInWorld` (`FossilFeature`,
    /// `TemplateFeature`).
    Placement,
}

/// The generator `StructurePlaceSettings.getRandom(pos)` returns.
#[derive(Debug)]
pub enum PieceRandom<'r> {
    /// The placement's generator.
    Placement(&'r mut dyn TemplateRandom),
    /// A generator seeded from the position.
    Positional(LegacyRandom),
}

impl std::fmt::Debug for dyn TemplateRandom + '_ {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TemplateRandom")
    }
}

impl TemplateRandom for PieceRandom<'_> {
    fn next_int(&mut self, bound: i32) -> i32 {
        match self {
            Self::Placement(r) => r.next_int(bound),
            Self::Positional(r) => r.next_int_bounded(bound),
        }
    }

    fn next_float(&mut self) -> f32 {
        match self {
            Self::Placement(r) => r.next_float(),
            Self::Positional(r) => RandomSource::next_float(r),
        }
    }

    fn next_long(&mut self) -> i64 {
        match self {
            Self::Placement(r) => r.next_long(),
            Self::Positional(r) => RandomSource::next_long(r),
        }
    }
}

/// `LiquidSettings`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LiquidSettings {
    IgnoreWaterlogging,
    #[default]
    ApplyWaterlogging,
}

impl LiquidSettings {
    /// Parses the serialized name.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "ignore_waterlogging" => Some(Self::IgnoreWaterlogging),
            "apply_waterlogging" => Some(Self::ApplyWaterlogging),
            _ => None,
        }
    }

    /// The serialized name.
    pub fn name(self) -> &'static str {
        match self {
            Self::IgnoreWaterlogging => "ignore_waterlogging",
            Self::ApplyWaterlogging => "apply_waterlogging",
        }
    }
}

/// `StructurePlaceSettings`: how a template is placed. Processors are borrowed from their
/// processor lists.
#[derive(Clone, Debug, Default)]
pub struct StructurePlaceSettings<'p> {
    pub mirror: Mirror,
    pub rotation: Rotation,
    pub rotation_pivot: BlockPos,
    pub ignore_entities: bool,
    pub bounding_box: Option<BoundingBox>,
    pub liquid_settings: LiquidSettings,
    pub random: SettingsRandom,
    pub processors: Vec<&'p StructureProcessor>,
    pub known_shape: bool,
    pub finalize_entities: bool,
}

impl<'p> StructurePlaceSettings<'p> {
    /// `new StructurePlaceSettings()`.
    pub fn new() -> Self {
        Self::default()
    }

    /// `setRotation`.
    #[must_use]
    pub fn with_rotation(mut self, rotation: Rotation) -> Self {
        self.rotation = rotation;
        self
    }

    /// `setMirror`.
    #[must_use]
    pub fn with_mirror(mut self, mirror: Mirror) -> Self {
        self.mirror = mirror;
        self
    }

    /// `setBoundingBox`.
    #[must_use]
    pub fn with_bounding_box(mut self, bounding_box: BoundingBox) -> Self {
        self.bounding_box = Some(bounding_box);
        self
    }

    /// `setRandom(random)` with the generator later passed to `placeInWorld`.
    #[must_use]
    pub fn with_placement_random(mut self) -> Self {
        self.random = SettingsRandom::Placement;
        self
    }

    /// `clearProcessors()`.
    pub fn clear_processors(&mut self) {
        self.processors.clear();
    }

    /// `addProcessor` for every processor of a list.
    pub fn add_processors(&mut self, processors: &'p [StructureProcessor]) {
        self.processors.extend(processors);
    }

    /// `shouldApplyWaterlogging()`.
    pub fn should_apply_waterlogging(&self) -> bool {
        self.liquid_settings == LiquidSettings::ApplyWaterlogging
    }

    /// `getRandom(pos)`: the placement's generator or one seeded from `pos`.
    pub fn random<'r>(
        &self,
        pos: BlockPos,
        placement: &'r mut dyn TemplateRandom,
    ) -> PieceRandom<'r> {
        match self.random {
            SettingsRandom::Placement => PieceRandom::Placement(placement),
            SettingsRandom::Positional => {
                PieceRandom::Positional(LegacyRandom::new(get_seed(pos.x, pos.y, pos.z)))
            }
        }
    }

    /// `getRandomPalette(palettes, pos)`; `None` without palettes (Vanilla throws).
    pub fn random_palette<'t>(
        &self,
        palettes: &'t [Palette],
        pos: BlockPos,
        placement: &mut dyn TemplateRandom,
    ) -> Option<&'t Palette> {
        if palettes.is_empty() {
            return None;
        }
        let size = i32::try_from(palettes.len()).ok()?;
        let index = self.random(pos, placement).next_int(size);
        palettes.get(index as usize)
    }
}

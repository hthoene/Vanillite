//! Material conditions (`net.minecraft.world.level.levelgen.material.condition`): the tests that
//! gate material rules, and their evaluation (`compile` + `ConditionEvaluator.test`).
//!
//! Vanilla wraps most conditions in `LazyXZCondition`/`LazyYCondition`, which memoise the result
//! until the context moves to the next column or block. Every condition is visited at most once
//! per `tryApply`, and `tryApply` runs once per context update, so those caches never return a
//! stored result; their computations are pure (no density sampling), so they are evaluated
//! directly here. The caches that do save work (biome, noise values, surface secondary, minimum
//! surface level) live in [`MaterialRuleContext`].

use vanillite_base::math;
use vanillite_base::random::{PositionalRandomFactory, RandomSource};
use vanillite_chunk::BiomeId;

use super::anchor::VerticalAnchor;
use super::context::MaterialRuleContext;
use super::system::cold_enough_to_snow;

/// Index of a condition in [`super::rules::MaterialRules::conditions`].
pub(crate) type CondId = u32;

/// A parsed material condition (`MaterialCondition`); registry references are inlined.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Condition {
    /// `BiomeCondition`: index into `MaterialRules::biome_sets`.
    Biome(u32),
    /// `NoiseThresholdCondition`: `noise` indexes `MaterialRules::noises` (id and 2D/3D).
    NoiseThreshold {
        noise: u32,
        min_threshold: f64,
        max_threshold: f64,
    },
    /// `VerticalGradientCondition`: `random` indexes `MaterialRules::random_names`.
    VerticalGradient {
        random: u32,
        true_at_and_below: VerticalAnchor,
        false_at_and_above: VerticalAnchor,
    },
    /// `YCondition` (`y_above`).
    Y {
        anchor: VerticalAnchor,
        surface_depth_multiplier: i32,
        add_stone_depth: bool,
    },
    /// `WaterCondition`.
    Water {
        offset: i32,
        surface_depth_multiplier: i32,
        add_stone_depth: bool,
    },
    /// `TemperatureCondition`.
    Temperature,
    /// `SteepCondition`.
    Steep,
    /// `NotCondition`.
    Not(CondId),
    /// `HoleCondition`.
    Hole,
    /// `AbovePreliminarySurfaceCondition`.
    AbovePreliminarySurface,
    /// `StoneDepthCondition`; `ceiling` is `surface_type == CaveSurface.CEILING`.
    StoneDepth {
        offset: i32,
        add_surface_depth: bool,
        secondary_depth_range: i32,
        ceiling: bool,
    },
}

/// The biomes of a `BiomeCondition` (`HolderSet<Biome>`), one bit per biome id.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct BiomeSet {
    bits: Vec<u64>,
}

impl BiomeSet {
    /// Adds a biome.
    pub(crate) fn insert(&mut self, biome: BiomeId) {
        let word = usize::from(biome) / 64;
        if word >= self.bits.len() {
            self.bits.resize(word + 1, 0);
        }
        self.bits[word] |= 1 << (biome % 64);
    }

    /// `HolderSet.contains(biome)`.
    pub(crate) fn contains(&self, biome: BiomeId) -> bool {
        self.bits
            .get(usize::from(biome) / 64)
            .is_some_and(|w| w & (1 << (biome % 64)) != 0)
    }

    /// The constant `BiomeCondition.compile` folds the condition to for a chunk whose biomes are
    /// all in `possible` (`canNeverMatch` → false, `willAlwaysMatch` → true), or `None` when the
    /// biome has to be looked up per block.
    pub(crate) fn fold(&self, possible: &[BiomeId]) -> Option<bool> {
        if !possible.iter().any(|&b| self.contains(b)) {
            Some(false)
        } else if possible.iter().all(|&b| self.contains(b)) {
            Some(true)
        } else {
            None
        }
    }
}

impl MaterialRuleContext<'_> {
    /// `ConditionEvaluator.test()` of condition `id`.
    pub(super) fn test(&mut self, id: CondId) -> bool {
        let system = self.system;
        match system.rules.conditions[id as usize] {
            Condition::Biome(set) => match self.biome_folds[set as usize] {
                Some(constant) => constant,
                None => system.rules.biome_sets[set as usize].contains(self.biome()),
            },
            Condition::NoiseThreshold {
                noise,
                min_threshold,
                max_threshold,
            } => {
                let value = self.noise_value(noise);
                value >= min_threshold && value <= max_threshold
            }
            Condition::VerticalGradient {
                random,
                true_at_and_below,
                false_at_and_above,
            } => {
                let true_at_and_below = self.resolve_anchor_y(true_at_and_below);
                let false_at_and_above = self.resolve_anchor_y(false_at_and_above);
                let block_y = self.block_y;
                if block_y <= true_at_and_below {
                    true
                } else if block_y >= false_at_and_above {
                    false
                } else {
                    let probability = math::map(
                        f64::from(block_y),
                        f64::from(true_at_and_below),
                        f64::from(false_at_and_above),
                        1.0,
                        0.0,
                    );
                    let mut random = system.gradient_randoms[random as usize].at(
                        self.block_x,
                        block_y,
                        self.block_z,
                    );
                    f64::from(random.next_float()) < probability
                }
            }
            Condition::Y {
                anchor,
                surface_depth_multiplier,
                add_stone_depth,
            } => {
                let stone_depth = if add_stone_depth {
                    self.stone_depth_above
                } else {
                    0
                };
                self.block_y.wrapping_add(stone_depth)
                    >= self
                        .resolve_anchor_y(anchor)
                        .wrapping_add(self.surface_depth.wrapping_mul(surface_depth_multiplier))
            }
            Condition::Water {
                offset,
                surface_depth_multiplier,
                add_stone_depth,
            } => {
                let stone_depth = if add_stone_depth {
                    self.stone_depth_above
                } else {
                    0
                };
                self.water_height == i32::MIN
                    || self.block_y.wrapping_add(stone_depth)
                        >= self
                            .water_height
                            .wrapping_add(offset)
                            .wrapping_add(self.surface_depth.wrapping_mul(surface_depth_multiplier))
            }
            Condition::Temperature => {
                let biome = self.biome();
                cold_enough_to_snow(
                    self.biomes.get(biome),
                    self.block_x,
                    self.block_y,
                    self.block_z,
                    system.sea_level,
                )
            }
            Condition::Steep => self.surface_gradient_x <= -4 || self.surface_gradient_z >= 4,
            Condition::Not(target) => !self.test(target),
            Condition::Hole => self.surface_depth <= 0,
            Condition::AbovePreliminarySurface => self.block_y >= self.min_surface_level(),
            Condition::StoneDepth {
                offset,
                add_surface_depth,
                secondary_depth_range,
                ceiling,
            } => {
                let stone_depth = if ceiling {
                    self.stone_depth_below
                } else {
                    self.stone_depth_above
                };
                let surface_depth = if add_surface_depth {
                    self.surface_depth
                } else {
                    0
                };
                let secondary_surface_depth = if secondary_depth_range == 0 {
                    0
                } else {
                    math::map(
                        self.surface_secondary(),
                        -1.0,
                        1.0,
                        0.0,
                        f64::from(secondary_depth_range),
                    ) as i32
                };
                stone_depth
                    <= 1_i32
                        .wrapping_add(offset)
                        .wrapping_add(surface_depth)
                        .wrapping_add(secondary_surface_depth)
            }
        }
    }

    /// Value of noise slot `slot` at the current column (2D) or block (3D), cached until the
    /// context moves (`getNoiseSampler(noise, is3d)`).
    fn noise_value(&mut self, slot: u32) -> f64 {
        let index = slot as usize;
        let three_d = self.system.rules.noises[index].is_3d;
        let stamp = if three_d {
            self.last_update_y
        } else {
            self.last_update_xz
        };
        let cached = &mut self.noise_values[index];
        if cached.0 != stamp {
            let noise = &self.system.noises[index];
            let y = if three_d {
                f64::from(self.block_y)
            } else {
                0.0
            };
            *cached = (
                stamp,
                f64::from(noise.get(f64::from(self.block_x), y, f64::from(self.block_z))),
            );
        }
        cached.1
    }
}

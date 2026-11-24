//! Material rules (`net.minecraft.world.level.levelgen.material`): Vanilla 26.3's surface
//! system, including bedrock, ore veins and the underground bands, run by `MaterialSystem`.
//!
//! - [`MaterialSystem`] (`MaterialSystem`) owns the noises, the clay bands and the parsed rule
//!   of one world's noise settings, and builds chunk surfaces (`buildSurface`) and carver top
//!   blocks (`topMaterial`).
//! - `rules` (`MaterialRules`) parses `worldgen/material_rule` / `worldgen/material_condition`
//!   data into one flat, immutable tree; `rule` and `condition` hold the rule and condition
//!   types (`rule/*`, `condition/*`) and evaluate them.
//! - `context` (`MaterialRuleContext`) is the per-chunk evaluation state. Vanilla compiles the
//!   rule into closures per chunk; here "compiling" only folds biome conditions and prefills ore
//!   vein densities into the context, and the tree is walked per block without allocating.
//! - [`VerticalAnchor`] and [`WorldGenerationContext`] (`net.minecraft.world.level.levelgen`).

mod anchor;
mod condition;
mod context;
mod rule;
mod rules;
mod system;

pub use anchor::{VerticalAnchor, WorldGenerationContext};
pub use system::{CLAY_BAND_COUNT, MaterialSystem, generate_bands};

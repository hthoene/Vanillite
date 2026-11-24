//! Small features: `FillLayerFeature`, `OverlayFeature` and `ReplaceBlockFeature`.

use serde_json::Value;
use vanillite_base::BlockPos;
use vanillite_data::block::BlockState;

use super::rule_test::BlockReplacement;
use super::state_field;
use crate::Error;
use crate::feature::region::flags;
use crate::feature::{
    FeatureContext, FeatureParser, FeatureType, PlacedFeatureId, field, int_field,
};

/// `fill_layer`: fills the air of one layer of the chunk at `minY + height`.
#[derive(Debug)]
pub struct FillLayerFeature {
    height: i32,
    state: BlockState,
}

impl FeatureType for FillLayerFeature {
    fn parse(json: &Value, _parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        let height = int_field(json, "height")?;
        // `Codec.intRange(0, DimensionType.Y_SIZE)`
        if !(0..=4064).contains(&height) {
            return Err(Error::new(format!(
                "fill_layer height {height} out of range"
            )));
        }
        Ok(Self {
            height,
            state: state_field(json, "state")?,
        })
    }

    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        let y = ctx.level.min_y() + self.height;
        for dx in 0..16 {
            for dz in 0..16 {
                let pos = BlockPos::new(origin.x + dx, y, origin.z + dz);
                if ctx.level.block(pos).is_air() {
                    ctx.level.set_block(pos, self.state, flags::CLIENTS);
                }
            }
        }
        true
    }
}

/// `overlay`: places every placed feature of the list at the origin.
#[derive(Debug)]
pub struct OverlayFeature {
    features: Vec<PlacedFeatureId>,
}

impl FeatureType for OverlayFeature {
    fn parse(json: &Value, parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        let features = parser.placed_list(field(json, "features")?)?;
        if features.is_empty() {
            return Err(Error::new("overlay: features must not be empty"));
        }
        Ok(Self { features })
    }

    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        let mut placed_any = false;
        for &feature in &self.features {
            placed_any |= ctx.place_placed(feature, origin);
        }
        placed_any
    }
}

/// `replace_single_block`: replaces the origin with the first target whose rule matches.
#[derive(Debug)]
pub struct ReplaceBlockFeature {
    replacements: Vec<BlockReplacement>,
}

impl FeatureType for ReplaceBlockFeature {
    fn parse(json: &Value, parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        Ok(Self {
            replacements: BlockReplacement::parse_list(json, parser)?,
        })
    }

    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        for replacement in &self.replacements {
            let state = ctx.level.block(origin);
            if replacement.target.test(state, origin, ctx.random) {
                ctx.level
                    .set_block(origin, replacement.state, flags::CLIENTS);
                break;
            }
        }
        true
    }
}

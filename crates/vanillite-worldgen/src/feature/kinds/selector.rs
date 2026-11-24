//! Features that pick or chain other placed features.

use serde_json::Value;
use vanillite_base::BlockPos;
use vanillite_base::random::RandomSource;

use crate::Error;
use crate::carver::providers::WeightedList;
use crate::feature::{
    FeatureContext, FeatureParser, FeatureType, PlacedFeatureId, field, float_field,
};

/// `no_op`.
#[derive(Debug)]
pub struct NoOpFeature;

impl FeatureType for NoOpFeature {
    fn parse(_json: &Value, _parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        Ok(Self)
    }

    fn place(&self, _ctx: &mut FeatureContext<'_, '_>, _origin: BlockPos) -> bool {
        true
    }
}

/// `random_selector`: the first entry whose chance hits, else the default.
#[derive(Debug)]
pub struct RandomSelectorFeature {
    features: Vec<(PlacedFeatureId, f32)>,
    default: PlacedFeatureId,
}

impl FeatureType for RandomSelectorFeature {
    fn parse(json: &Value, parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        let features = field(json, "features")?
            .as_array()
            .ok_or_else(|| Error::new("features is not a list"))?
            .iter()
            .map(|entry| {
                Ok((
                    parser.placed(field(entry, "feature")?)?,
                    float_field(entry, "chance")?,
                ))
            })
            .collect::<Result<_, Error>>()?;
        Ok(Self {
            features,
            default: parser.placed(field(json, "default")?)?,
        })
    }

    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        for &(feature, chance) in &self.features {
            if ctx.random.next_float() < chance {
                return ctx.place_placed(feature, origin);
            }
        }
        ctx.place_placed(self.default, origin)
    }
}

/// `random_boolean_selector`.
#[derive(Debug)]
pub struct RandomBooleanSelectorFeature {
    feature_true: PlacedFeatureId,
    feature_false: PlacedFeatureId,
}

impl FeatureType for RandomBooleanSelectorFeature {
    fn parse(json: &Value, parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        Ok(Self {
            feature_true: parser.placed(field(json, "feature_true")?)?,
            feature_false: parser.placed(field(json, "feature_false")?)?,
        })
    }

    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        let feature = if ctx.random.next_boolean() {
            self.feature_true
        } else {
            self.feature_false
        };
        ctx.place_placed(feature, origin)
    }
}

/// `simple_random_selector`: a uniformly random entry.
#[derive(Debug)]
pub struct SimpleRandomSelectorFeature {
    features: Vec<PlacedFeatureId>,
}

impl FeatureType for SimpleRandomSelectorFeature {
    fn parse(json: &Value, parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        let features = parser.placed_list(field(json, "features")?)?;
        if features.is_empty() {
            return Err(Error::new("features must not be empty"));
        }
        Ok(Self { features })
    }

    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        let index = ctx.random.next_int_bounded(self.features.len() as i32);
        ctx.place_placed(self.features[index as usize], origin)
    }
}

/// `weighted_random_selector`.
#[derive(Debug)]
pub struct WeightedRandomSelectorFeature {
    features: WeightedList<PlacedFeatureId>,
}

impl FeatureType for WeightedRandomSelectorFeature {
    fn parse(json: &Value, parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        Ok(Self {
            features: WeightedList::from_json(field(json, "features")?, |f| parser.placed(f))?,
        })
    }

    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        let feature = *self.features.get_random(ctx.random);
        ctx.place_placed(feature, origin)
    }
}

/// `sequence`: places every entry in order until one fails.
#[derive(Debug)]
pub struct SequenceFeature {
    features: Vec<PlacedFeatureId>,
}

impl FeatureType for SequenceFeature {
    fn parse(json: &Value, parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        let features = parser.placed_list(field(json, "features")?)?;
        if features.is_empty() {
            return Err(Error::new("features must not be empty"));
        }
        Ok(Self { features })
    }

    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        self.features
            .iter()
            .all(|&feature| ctx.place_placed(feature, origin))
    }
}

//! Feature sizes (`featuresize/`): how much free space a tree needs around its trunk.

use serde_json::Value;

use crate::Error;
use crate::feature::int_or;

/// `FeatureSize`.
#[derive(Clone, Debug)]
pub struct FeatureSize {
    /// `min_clipped_height`: the lowest height a tree may be clipped to.
    min_clipped_height: Option<i32>,
    kind: SizeKind,
}

#[derive(Clone, Debug)]
enum SizeKind {
    /// `TwoLayersFeatureSize`.
    TwoLayers {
        limit: i32,
        lower_size: i32,
        upper_size: i32,
    },
    /// `ThreeLayersFeatureSize`.
    ThreeLayers {
        limit: i32,
        upper_limit: i32,
        lower_size: i32,
        middle_size: i32,
        upper_size: i32,
    },
}

/// An optional integer field in `min..=max` with a default (`Codec.intRange(...)
/// .optionalFieldOf(name, default)`).
fn ranged(json: &Value, name: &str, default: i32, min: i32, max: i32) -> Result<i32, Error> {
    let value = int_or(json, name, default);
    if (min..=max).contains(&value) {
        Ok(value)
    } else {
        Err(Error::new(format!("{name} {value} outside [{min}, {max}]")))
    }
}

impl FeatureSize {
    /// Parses `FeatureSize.CODEC`.
    pub fn parse(json: &Value) -> Result<Self, Error> {
        let kind = json
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::new("feature size without type"))?;
        let min_clipped_height = match json.get("min_clipped_height") {
            Some(_) => Some(ranged(json, "min_clipped_height", 0, 0, 80)?),
            None => None,
        };
        let kind = match kind.strip_prefix("minecraft:").unwrap_or(kind) {
            "two_layers_feature_size" => SizeKind::TwoLayers {
                limit: ranged(json, "limit", 1, 0, 81)?,
                lower_size: ranged(json, "lower_size", 0, 0, 16)?,
                upper_size: ranged(json, "upper_size", 1, 0, 16)?,
            },
            "three_layers_feature_size" => SizeKind::ThreeLayers {
                limit: ranged(json, "limit", 1, 0, 80)?,
                upper_limit: ranged(json, "upper_limit", 1, 0, 80)?,
                lower_size: ranged(json, "lower_size", 0, 0, 16)?,
                middle_size: ranged(json, "middle_size", 1, 0, 16)?,
                upper_size: ranged(json, "upper_size", 1, 0, 16)?,
            },
            other => return Err(Error::new(format!("unknown feature size type {other}"))),
        };
        Ok(Self {
            min_clipped_height,
            kind,
        })
    }

    /// `minClippedHeight()`.
    pub fn min_clipped_height(&self) -> Option<i32> {
        self.min_clipped_height
    }

    /// `getSizeAtHeight(treeHeight, yo)`: the radius that must be free at height `yo`.
    pub fn size_at_height(&self, tree_height: i32, yo: i32) -> i32 {
        match self.kind {
            SizeKind::TwoLayers {
                limit,
                lower_size,
                upper_size,
            } => {
                if yo < limit {
                    lower_size
                } else {
                    upper_size
                }
            }
            SizeKind::ThreeLayers {
                limit,
                upper_limit,
                lower_size,
                middle_size,
                upper_size,
            } => {
                if yo < limit {
                    lower_size
                } else if yo >= tree_height - upper_limit {
                    upper_size
                } else {
                    middle_size
                }
            }
        }
    }
}

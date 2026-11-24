//! `VerticalAnchor` and `WorldGenerationContext` (`net.minecraft.world.level.levelgen`): Y
//! positions in data that are relative to the generated height range or the sea level.

use serde_json::Value;

use crate::Error;
use crate::settings::NoiseGeneratorSettings;

/// `DimensionType.MIN_Y`.
const DIMENSION_MIN_Y: i32 = -2032;
/// `DimensionType.MAX_Y`.
const DIMENSION_MAX_Y: i32 = 2031;

/// The height range world generation works in (`WorldGenerationContext`): the intersection of the
/// generator's range with the level's, and the generator's sea level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorldGenerationContext {
    min_y: i32,
    height: i32,
    sea_level: i32,
}

impl WorldGenerationContext {
    /// A context with the given generation range and sea level.
    pub const fn new(min_gen_y: i32, gen_depth: i32, sea_level: i32) -> Self {
        Self {
            min_y: min_gen_y,
            height: gen_depth,
            sea_level,
        }
    }

    /// `new WorldGenerationContext(generator, heightAccessor)`: the generator's range
    /// (`generator_min_y`, `generator_height`) clamped to the level's (`level_min_y`,
    /// `level_height`), and the generator's sea level.
    pub const fn clamped(
        generator_min_y: i32,
        generator_height: i32,
        sea_level: i32,
        level_min_y: i32,
        level_height: i32,
    ) -> Self {
        Self {
            min_y: if level_min_y > generator_min_y {
                level_min_y
            } else {
                generator_min_y
            },
            height: if level_height < generator_height {
                level_height
            } else {
                generator_height
            },
            sea_level,
        }
    }

    /// The context of a noise based generator with `settings` for a level (chunk) from
    /// `level_min_y` spanning `level_height` blocks.
    pub const fn for_settings(
        settings: &NoiseGeneratorSettings,
        level_min_y: i32,
        level_height: i32,
    ) -> Self {
        Self::clamped(
            settings.min_y,
            settings.height,
            settings.sea_level,
            level_min_y,
            level_height,
        )
    }

    /// `getMinGenY()`.
    pub const fn min_gen_y(&self) -> i32 {
        self.min_y
    }

    /// `getGenDepth()`.
    pub const fn gen_depth(&self) -> i32 {
        self.height
    }

    /// `seaLevel()`.
    pub const fn sea_level(&self) -> i32 {
        self.sea_level
    }
}

/// A Y coordinate given relative to the bottom or top of the generated range, the sea level, or
/// absolutely (`VerticalAnchor`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VerticalAnchor {
    /// `{"absolute": y}`.
    Absolute(i32),
    /// `{"above_bottom": offset}`.
    AboveBottom(i32),
    /// `{"below_top": offset}`.
    BelowTop(i32),
    /// `{"relative_to_sea_level": offset}`.
    RelativeToSeaLevel(i32),
}

impl VerticalAnchor {
    /// `VerticalAnchor.BOTTOM`.
    pub const BOTTOM: Self = Self::AboveBottom(0);
    /// `VerticalAnchor.TOP`.
    pub const TOP: Self = Self::BelowTop(0);

    /// Parses `VerticalAnchor.CODEC`: an object with exactly one of the four keys, combined like
    /// DFU's nested `Codec.xor` (absolute, then above bottom, below top, relative to sea level).
    pub fn parse(json: &Value) -> Result<Self, Error> {
        xor(
            offset(json, "absolute").map(Self::Absolute),
            xor(
                offset(json, "above_bottom").map(Self::AboveBottom),
                xor(
                    offset(json, "below_top").map(Self::BelowTop),
                    offset(json, "relative_to_sea_level").map(Self::RelativeToSeaLevel),
                ),
            ),
        )
        .map_err(|e| Error::new(format!("vertical anchor {json}: {e}")))
    }

    /// `resolveY(context)`.
    pub const fn resolve_y(self, context: &WorldGenerationContext) -> i32 {
        match self {
            Self::Absolute(y) => y,
            Self::AboveBottom(offset) => context.min_gen_y().wrapping_add(offset),
            Self::BelowTop(offset) => context
                .gen_depth()
                .wrapping_sub(1)
                .wrapping_add(context.min_gen_y())
                .wrapping_sub(offset),
            Self::RelativeToSeaLevel(offset) => context.sea_level().wrapping_add(offset),
        }
    }
}

/// One alternative: `Codec.intRange(MIN_Y, MAX_Y).fieldOf(name)`.
fn offset(json: &Value, name: &str) -> Result<i32, Error> {
    let value = json
        .get(name)
        .ok_or_else(|| Error::new(format!("no key {name}")))?;
    codec_int(value)
        .filter(|v| (DIMENSION_MIN_Y..=DIMENSION_MAX_Y).contains(v))
        .ok_or_else(|| {
            Error::new(format!(
                "{name}: {value} is not an int in [{DIMENSION_MIN_Y}, {DIMENSION_MAX_Y}]"
            ))
        })
}

/// `Codec.INT` on a JSON number: Gson's `Number.intValue()`, which truncates fractions and keeps
/// the low 32 bits of large integers. `None` for non-numbers and fractions beyond `i64`.
pub(crate) fn codec_int(json: &Value) -> Option<i32> {
    if let Some(v) = json.as_i64() {
        Some(v as i32)
    } else if let Some(v) = json.as_u64() {
        Some(v as i32)
    } else {
        let v = json.as_f64()?.trunc();
        (v.abs() < 9.2e18).then_some(v as i64 as i32)
    }
}

/// `Codec.xor`: exactly one alternative must succeed.
fn xor<T>(first: Result<T, Error>, second: Result<T, Error>) -> Result<T, Error> {
    match (first, second) {
        (Ok(_), Ok(_)) => Err(Error::new(
            "both alternatives read successfully, can not pick the correct one",
        )),
        (Ok(v), Err(_)) | (Err(_), Ok(v)) => Ok(v),
        (Err(e), Err(_)) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn parses_and_resolves() {
        let overworld = WorldGenerationContext::clamped(-64, 384, 63, -64, 384);
        let cases = [
            (json!({"absolute": 30}), 30),
            (json!({"above_bottom": 5}), -59),
            (json!({"below_top": 0}), 319),
            (json!({"relative_to_sea_level": -2}), 61),
        ];
        for (json, y) in cases {
            assert_eq!(
                VerticalAnchor::parse(&json).unwrap().resolve_y(&overworld),
                y
            );
        }
    }

    #[test]
    fn xor_semantics() {
        assert!(VerticalAnchor::parse(&json!({})).is_err());
        assert!(VerticalAnchor::parse(&json!({"absolute": 1, "below_top": 2})).is_err());
        assert!(VerticalAnchor::parse(&json!({"absolute": 2032})).is_err());
        // The two inner alternatives cancel each other out, so the outer one wins.
        assert_eq!(
            VerticalAnchor::parse(
                &json!({"above_bottom": 1, "below_top": 2, "relative_to_sea_level": 3})
            )
            .unwrap(),
            VerticalAnchor::AboveBottom(1)
        );
    }
}

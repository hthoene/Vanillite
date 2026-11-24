//! `MultiNoiseBiomeSourceParameterList.Preset`: the built-in biome parameter lists `nether` and
//! `overworld`.
//!
//! Vanilla computes the overworld list in Java (`OverworldBiomeBuilder`); the data pack's
//! `worldgen/multi_noise_biome_source_parameter_list` entries only name the preset. The list is
//! embedded from Vanilla's `biome_parameters` report (`data/vanilla/biome_parameters/`), which
//! writes every quantized value `q` as the float `q / 10000`; rounding `value * 10000` recovers
//! `q` exactly, so the list equals the one Vanilla builds (the goldens check a hash of it). The
//! nether list is five literal entries, ported as code.

use std::sync::OnceLock;

use serde_json::Value;

use super::climate::{Parameter, ParameterPoint, parameter_bounds};
use crate::Error;

/// The overworld report: `[{"biome": id, "parameters": {...}}, ...]`.
const OVERWORLD_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../data/vanilla/biome_parameters/overworld.json"
));

/// A built-in parameter list preset.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Preset {
    /// `minecraft:nether`.
    Nether,
    /// `minecraft:overworld`.
    Overworld,
}

impl Preset {
    /// The preset with the id `id` (`Preset.CODEC`).
    pub fn by_id(id: &str) -> Option<Self> {
        match id.strip_prefix("minecraft:").unwrap_or(id) {
            "nether" => Some(Self::Nether),
            "overworld" => Some(Self::Overworld),
            _ => None,
        }
    }

    /// The preset id.
    pub fn id(self) -> &'static str {
        match self {
            Self::Nether => "minecraft:nether",
            Self::Overworld => "minecraft:overworld",
        }
    }

    /// The entries in Vanilla's order with biome ids (`provider.apply(key -> key)`).
    pub fn entries(self) -> &'static [(ParameterPoint, String)] {
        static NETHER: OnceLock<Vec<(ParameterPoint, String)>> = OnceLock::new();
        static OVERWORLD: OnceLock<Vec<(ParameterPoint, String)>> = OnceLock::new();
        match self {
            Self::Nether => NETHER.get_or_init(nether),
            Self::Overworld => OVERWORLD.get_or_init(|| {
                parse_report(OVERWORLD_JSON).expect("embedded overworld biome parameters")
            }),
        }
    }
}

/// `Preset.NETHER`.
fn nether() -> Vec<(ParameterPoint, String)> {
    let p = ParameterPoint::of_points;
    [
        (p(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0), "nether_wastes"),
        (p(0.0, -0.5, 0.0, 0.0, 0.0, 0.0, 0.0), "soul_sand_valley"),
        (p(0.4, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0), "crimson_forest"),
        (p(0.0, 0.5, 0.0, 0.0, 0.0, 0.0, 0.375), "warped_forest"),
        (p(-0.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.175), "basalt_deltas"),
    ]
    .into_iter()
    .map(|(point, name)| (point, format!("minecraft:{name}")))
    .collect()
}

/// Parses a `biome_parameters` report, recovering the exact quantized values.
fn parse_report(text: &str) -> Result<Vec<(ParameterPoint, String)>, Error> {
    let json: Value = serde_json::from_str(text).map_err(|e| Error::new(e.to_string()))?;
    let entries = json
        .as_array()
        .ok_or_else(|| Error::new("biome parameters: not a list"))?;
    entries
        .iter()
        .map(|entry| {
            let biome = entry["biome"]
                .as_str()
                .ok_or_else(|| Error::new("biome parameters: biome missing"))?;
            let p = &entry["parameters"];
            let param = |name: &str| -> Result<Parameter, Error> {
                let (min, max) = parameter_bounds(&p[name])
                    .ok_or_else(|| Error::new(format!("biome parameters: bad {name}")))?;
                Ok(Parameter {
                    min: recover(min),
                    max: recover(max),
                })
            };
            let offset = p["offset"]
                .as_f64()
                .ok_or_else(|| Error::new("biome parameters: offset missing"))?;
            let point = ParameterPoint {
                temperature: param("temperature")?,
                humidity: param("humidity")?,
                continentalness: param("continentalness")?,
                erosion: param("erosion")?,
                depth: param("depth")?,
                weirdness: param("weirdness")?,
                offset: recover(offset as f32),
            };
            Ok((point, biome.to_owned()))
        })
        .collect()
}

/// The quantized value `q` of a reported `unquantizeCoord(q)`.
fn recover(value: f32) -> i64 {
    (f64::from(value) * 10000.0).round() as i64
}

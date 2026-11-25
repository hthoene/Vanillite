//! `MaterialRules`: parsing material rules and conditions (`MaterialRule.CODEC`,
//! `MaterialCondition.CODEC` and the type registries of `MaterialRules.bootstrapRules` /
//! `bootstrapConditions`) into one flat tree.
//!
//! A string is a reference into `worldgen/material_rule` or `worldgen/material_condition`, an
//! object is an inline value dispatched on `type`. References are inlined: Vanilla compiles a
//! `HolderHolder`'s target anew at every occurrence, so each occurrence is its own node here.
//! Parsing needs only the data packs; noises, random factories and density functions are bound
//! by [`super::MaterialSystem::new`] through the side tables.

use rustc_hash::FxHashMap;
use serde_json::Value;
use vanillite_chunk::BiomeId;
use vanillite_datapack::LoadedPacks;

use super::condition::{BiomeSet, CondId, Condition};
use super::rule::{OreVein, Rule, RuleId};
use crate::Error;
use crate::anchor::{VerticalAnchor, codec_int};
use crate::blocks::{parse_block_state, qualify};

/// The `worldgen/material_rule` registry.
const RULE_REGISTRY: &str = "minecraft:worldgen/material_rule";
/// The `worldgen/material_condition` registry.
const CONDITION_REGISTRY: &str = "minecraft:worldgen/material_condition";
/// The `worldgen/noise` registry.
const NOISE_REGISTRY: &str = "minecraft:worldgen/noise";
/// The biome tag registry.
const BIOME_REGISTRY: &str = "minecraft:worldgen/biome";
/// Deepest reference nesting accepted (Vanilla would overflow its stack on cycles).
const MAX_DEPTH: usize = 256;

/// A noise a `NoiseThresholdCondition` reads; 2D and 3D samplers of one noise are separate
/// (`noiseSamplers2d`, `noiseSamplers3d`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct NoiseKey {
    pub id: String,
    pub is_3d: bool,
}

/// A parsed material rule tree with its side tables.
#[derive(Clone, Debug, Default)]
pub(crate) struct MaterialRules {
    pub rules: Vec<Rule>,
    pub conditions: Vec<Condition>,
    pub root: RuleId,
    /// Biome sets of `Condition::Biome`.
    pub biome_sets: Vec<BiomeSet>,
    /// Distinct noises of `Condition::NoiseThreshold`.
    pub noises: Vec<NoiseKey>,
    /// Random names of `Condition::VerticalGradient`.
    pub random_names: Vec<String>,
    /// `Rule::OreVein`s in compile order.
    pub ore_veins: Vec<OreVein>,
}

impl MaterialRules {
    /// Parses a rule (`NoiseGeneratorSettings.material_rule`: an id or an inline rule),
    /// resolving biome ids with `biome_id`.
    pub(crate) fn parse(
        packs: &LoadedPacks,
        json: &Value,
        biome_id: &dyn Fn(&str) -> Option<BiomeId>,
    ) -> Result<Self, Error> {
        let mut parser = Parser {
            packs,
            biome_id,
            out: Self::default(),
            noise_slots: FxHashMap::default(),
            references: Vec::new(),
        };
        parser.out.root = parser.rule(json)?;
        Ok(parser.out)
    }

    /// Per biome set: the constant its `BiomeCondition` compiles to for a chunk whose biomes
    /// are all in `possible` (none when not given).
    pub(crate) fn biome_folds(&self, possible: Option<&[BiomeId]>) -> Vec<Option<bool>> {
        self.biome_sets
            .iter()
            .map(|set| possible.and_then(|possible| set.fold(possible)))
            .collect()
    }
}

struct Parser<'a> {
    packs: &'a LoadedPacks,
    biome_id: &'a dyn Fn(&str) -> Option<BiomeId>,
    out: MaterialRules,
    noise_slots: FxHashMap<NoiseKey, u32>,
    /// References being expanded, outermost first.
    references: Vec<String>,
}

impl Parser<'_> {
    /// `MaterialRule.CODEC`.
    fn rule(&mut self, json: &Value) -> Result<RuleId, Error> {
        if let Value::String(id) = json {
            return self.reference(RULE_REGISTRY, id, Self::rule);
        }
        let rule = match type_of(json)?.as_str() {
            "minecraft:block" => Rule::Block(parse_block_state(field(json, "result_state")?)?),
            "minecraft:bandlands" => Rule::Bandlands,
            "minecraft:sequence" => Rule::Sequence(
                array(json, "sequence")?
                    .iter()
                    .map(|rule| self.rule(rule))
                    .collect::<Result<_, _>>()?,
            ),
            "minecraft:condition" => {
                let if_true = self.condition(field(json, "if_true")?)?;
                let then_run = self.rule(field(json, "then_run")?)?;
                Rule::Condition { if_true, then_run }
            }
            "minecraft:ore_vein" => {
                let raw_ore_chance = float(json, "raw_ore_chance")?;
                if !(0.0..=1.0).contains(&raw_ore_chance) {
                    return Err(Error::new(format!(
                        "ore_vein: raw_ore_chance {raw_ore_chance} outside of [0, 1]"
                    )));
                }
                self.out.ore_veins.push(OreVein {
                    ore_block: parse_block_state(field(json, "ore_block")?)?,
                    raw_ore_block: parse_block_state(field(json, "raw_ore_block")?)?,
                    filler_block: parse_block_state(field(json, "filler_block")?)?,
                    raw_ore_chance,
                    density: field(json, "density")?.clone(),
                    richness: field(json, "richness")?.clone(),
                    filler_gap: field(json, "filler_gap")?.clone(),
                });
                Rule::OreVein(self.out.ore_veins.len() as u32 - 1)
            }
            other => return Err(Error::new(format!("unknown material rule type {other}"))),
        };
        self.out.rules.push(rule);
        Ok(self.out.rules.len() as RuleId - 1)
    }

    /// `MaterialCondition.CODEC`.
    fn condition(&mut self, json: &Value) -> Result<CondId, Error> {
        if let Value::String(id) = json {
            return self.reference(CONDITION_REGISTRY, id, Self::condition);
        }
        let condition = match type_of(json)?.as_str() {
            "minecraft:biome" => {
                let set = self.biome_set(field(json, "biome_is")?)?;
                self.out.biome_sets.push(set);
                Condition::Biome(self.out.biome_sets.len() as u32 - 1)
            }
            "minecraft:noise_threshold" => {
                let id = qualify(string(json, "noise")?);
                if self
                    .packs
                    .registry(NOISE_REGISTRY)
                    .and_then(|r| r.get(&id))
                    .is_none()
                {
                    return Err(Error::new(format!("noise_threshold: unknown noise {id}")));
                }
                let is_3d = match json.get("is_3d") {
                    None => false,
                    Some(v) => v
                        .as_bool()
                        .ok_or_else(|| Error::new("noise_threshold: is_3d is not a boolean"))?,
                };
                let key = NoiseKey { id, is_3d };
                let next = self.out.noises.len() as u32;
                let noise = *self.noise_slots.entry(key.clone()).or_insert_with(|| {
                    self.out.noises.push(key);
                    next
                });
                Condition::NoiseThreshold {
                    noise,
                    min_threshold: double(json, "min_threshold")?,
                    max_threshold: double(json, "max_threshold")?,
                }
            }
            "minecraft:vertical_gradient" => {
                self.out
                    .random_names
                    .push(qualify(string(json, "random_name")?));
                Condition::VerticalGradient {
                    random: self.out.random_names.len() as u32 - 1,
                    true_at_and_below: VerticalAnchor::parse(field(json, "true_at_and_below")?)?,
                    false_at_and_above: VerticalAnchor::parse(field(json, "false_at_and_above")?)?,
                }
            }
            "minecraft:y_above" => Condition::Y {
                anchor: VerticalAnchor::parse(field(json, "anchor")?)?,
                surface_depth_multiplier: surface_depth_multiplier(json)?,
                add_stone_depth: boolean(json, "add_stone_depth")?,
            },
            "minecraft:water" => Condition::Water {
                offset: int(json, "offset")?,
                surface_depth_multiplier: surface_depth_multiplier(json)?,
                add_stone_depth: boolean(json, "add_stone_depth")?,
            },
            "minecraft:temperature" => Condition::Temperature,
            "minecraft:steep" => Condition::Steep,
            "minecraft:not" => Condition::Not(self.condition(field(json, "invert")?)?),
            "minecraft:hole" => Condition::Hole,
            "minecraft:above_preliminary_surface" => Condition::AbovePreliminarySurface,
            "minecraft:stone_depth" => Condition::StoneDepth {
                offset: int(json, "offset")?,
                add_surface_depth: boolean(json, "add_surface_depth")?,
                secondary_depth_range: int(json, "secondary_depth_range")?,
                ceiling: match string(json, "surface_type")? {
                    "ceiling" => true,
                    "floor" => false,
                    other => {
                        return Err(Error::new(format!(
                            "stone_depth: unknown surface_type {other}"
                        )));
                    }
                },
            },
            other => {
                return Err(Error::new(format!(
                    "unknown material condition type {other}"
                )));
            }
        };
        self.out.conditions.push(condition);
        Ok(self.out.conditions.len() as CondId - 1)
    }

    /// Expands the registry entry `id` with `parse`.
    fn reference<T>(
        &mut self,
        registry: &str,
        id: &str,
        parse: fn(&mut Self, &Value) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let id = qualify(id);
        if self.references.contains(&id) || self.references.len() >= MAX_DEPTH {
            return Err(Error::new(format!("{registry}: reference cycle at {id}")));
        }
        let entry = self
            .packs
            .registry(registry)
            .and_then(|r| r.get(&id))
            .ok_or_else(|| Error::new(format!("{registry}: unknown entry {id}")))?;
        let json: Value = serde_json::from_str(entry.json.get())
            .map_err(|e| Error::new(format!("{registry} {id}: {e}")))?;
        self.references.push(id);
        let result = parse(self, &json);
        let id = self.references.pop().unwrap_or_default();
        result.map_err(|e| Error::new(format!("{id}: {e}")))
    }

    /// `RegistryCodecs.holderSet(BIOME)`: `"#tag"`, one id, or a list of ids.
    fn biome_set(&self, json: &Value) -> Result<BiomeSet, Error> {
        let mut set = BiomeSet::default();
        let mut add = |name: &str| -> Result<(), Error> {
            let name = qualify(name);
            let id = (self.biome_id)(&name)
                .ok_or_else(|| Error::new(format!("biome: unknown biome {name}")))?;
            set.insert(id);
            Ok(())
        };
        match json {
            Value::String(tag) if tag.starts_with('#') => {
                let tag = qualify(&tag[1..]);
                let entries = self
                    .packs
                    .tags(BIOME_REGISTRY)
                    .and_then(|tags| tags.get(&tag));
                for name in entries.into_iter().flatten() {
                    add(name)?;
                }
            }
            Value::String(name) => add(name)?,
            Value::Array(names) => {
                for name in names {
                    add(name
                        .as_str()
                        .ok_or_else(|| Error::new("biome: biome_is entry is not an id"))?)?;
                }
            }
            _ => return Err(Error::new("biome: invalid biome_is")),
        }
        Ok(set)
    }
}

fn field<'v>(json: &'v Value, name: &str) -> Result<&'v Value, Error> {
    json.get(name)
        .ok_or_else(|| Error::new(format!("missing field {name}")))
}

fn type_of(json: &Value) -> Result<String, Error> {
    Ok(qualify(string(json, "type")?))
}

fn string<'v>(json: &'v Value, name: &str) -> Result<&'v str, Error> {
    field(json, name)?
        .as_str()
        .ok_or_else(|| Error::new(format!("{name} is not a string")))
}

fn array<'v>(json: &'v Value, name: &str) -> Result<&'v [Value], Error> {
    field(json, name)?
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| Error::new(format!("{name} is not a list")))
}

fn int(json: &Value, name: &str) -> Result<i32, Error> {
    codec_int(field(json, name)?).ok_or_else(|| Error::new(format!("{name} is not an int")))
}

fn boolean(json: &Value, name: &str) -> Result<bool, Error> {
    field(json, name)?
        .as_bool()
        .ok_or_else(|| Error::new(format!("{name} is not a boolean")))
}

fn double(json: &Value, name: &str) -> Result<f64, Error> {
    field(json, name)?
        .as_f64()
        .ok_or_else(|| Error::new(format!("{name} is not a number")))
}

/// `Codec.FLOAT`: Gson parses the number's text as a float directly (`Float.parseFloat`).
fn float(json: &Value, name: &str) -> Result<f32, Error> {
    match field(json, name)? {
        Value::Number(n) => n
            .to_string()
            .parse()
            .map_err(|_| Error::new(format!("{name} is not a float"))),
        _ => Err(Error::new(format!("{name} is not a number"))),
    }
}

/// `Codec.intRange(-20, 20).fieldOf("surface_depth_multiplier")`.
fn surface_depth_multiplier(json: &Value) -> Result<i32, Error> {
    let value = int(json, "surface_depth_multiplier")?;
    if (-20..=20).contains(&value) {
        Ok(value)
    } else {
        Err(Error::new(format!(
            "surface_depth_multiplier {value} outside of [-20, 20]"
        )))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::test_support::packs;

    fn biome_id(name: &str) -> Option<BiomeId> {
        packs()
            .registry(BIOME_REGISTRY)
            .and_then(|r| r.id_of(name))
            .map(|i| i as BiomeId)
    }

    fn parse(json: &Value) -> Result<MaterialRules, Error> {
        MaterialRules::parse(packs(), json, &biome_id)
    }

    /// Names of the rule and condition kinds that occur in `rules`.
    fn kinds(rules: &MaterialRules) -> Vec<&'static str> {
        let mut kinds: Vec<&'static str> = rules
            .rules
            .iter()
            .map(|r| match r {
                Rule::Block(_) => "block",
                Rule::Bandlands => "bandlands",
                Rule::Sequence(_) => "sequence",
                Rule::Condition { .. } => "condition",
                Rule::OreVein(_) => "ore_vein",
            })
            .chain(rules.conditions.iter().map(|c| match c {
                Condition::Biome(_) => "biome",
                Condition::NoiseThreshold { .. } => "noise_threshold",
                Condition::VerticalGradient { .. } => "vertical_gradient",
                Condition::Y { .. } => "y_above",
                Condition::Water { .. } => "water",
                Condition::Temperature => "temperature",
                Condition::Steep => "steep",
                Condition::Not(_) => "not",
                Condition::Hole => "hole",
                Condition::AbovePreliminarySurface => "above_preliminary_surface",
                Condition::StoneDepth { .. } => "stone_depth",
            }))
            .collect();
        kinds.sort_unstable();
        kinds.dedup();
        kinds
    }

    #[test]
    fn every_registry_entry_parses() {
        let packs = packs();
        for entry in packs.registry(RULE_REGISTRY).unwrap().entries() {
            parse(&json!(entry.id)).unwrap_or_else(|e| panic!("{}: {e}", entry.id));
        }
        for entry in packs.registry(CONDITION_REGISTRY).unwrap().entries() {
            let rule = json!({
                "type": "minecraft:condition",
                "if_true": entry.id,
                "then_run": {"type": "minecraft:block", "result_state": "minecraft:stone"}
            });
            parse(&rule).unwrap_or_else(|e| panic!("{}: {e}", entry.id));
        }
    }

    #[test]
    fn noise_settings_rules_parse() {
        let registry = packs()
            .registry("minecraft:worldgen/noise_settings")
            .unwrap();
        let mut all = Vec::new();
        for id in [
            "minecraft:overworld",
            "minecraft:large_biomes",
            "minecraft:amplified",
            "minecraft:nether",
            "minecraft:end",
            "minecraft:caves",
            "minecraft:floating_islands",
        ] {
            let settings: Value =
                serde_json::from_str(registry.get(id).unwrap().json.get()).unwrap();
            let rules = parse(&settings["material_rule"]).unwrap_or_else(|e| panic!("{id}: {e}"));
            assert_eq!(
                rules.root as usize,
                rules.rules.len() - 1,
                "{id}: root last"
            );
            all.extend(kinds(&rules));
        }
        all.sort_unstable();
        all.dedup();
        // Every rule and condition type the Vanilla data uses (temperature is unused).
        assert_eq!(
            all,
            [
                "above_preliminary_surface",
                "bandlands",
                "biome",
                "block",
                "condition",
                "hole",
                "noise_threshold",
                "not",
                "ore_vein",
                "sequence",
                "steep",
                "stone_depth",
                "vertical_gradient",
                "water",
                "y_above",
            ]
        );
    }

    #[test]
    fn overworld_tree() {
        let rules = parse(&json!("minecraft:overworld")).unwrap();
        // Copper before iron: ore veins are prefilled in compile order.
        assert_eq!(rules.ore_veins.len(), 2);
        assert_eq!(
            rules.ore_veins[0].ore_block,
            crate::blocks::default_state("minecraft:copper_ore")
        );
        assert_eq!(
            rules.ore_veins[1].ore_block,
            crate::blocks::default_state("minecraft:deepslate_iron_ore")
        );
        assert_eq!(rules.ore_veins[1].raw_ore_chance, 0.02_f32);
        assert_eq!(
            rules.random_names,
            ["minecraft:bedrock_floor", "minecraft:deepslate"]
        );
        // The root sequence starts with the inlined bedrock floor.
        let Rule::Sequence(root) = &rules.rules[rules.root as usize] else {
            panic!("root is not a sequence");
        };
        let Rule::Condition { if_true, then_run } = rules.rules[root[0] as usize] else {
            panic!("bedrock floor is not a condition");
        };
        assert_eq!(
            rules.conditions[if_true as usize],
            Condition::VerticalGradient {
                random: 0,
                true_at_and_below: VerticalAnchor::AboveBottom(0),
                false_at_and_above: VerticalAnchor::AboveBottom(5),
            }
        );
        assert_eq!(
            rules.rules[then_run as usize],
            Rule::Block(crate::blocks::default_state("minecraft:bedrock"))
        );
        // 2D and 3D samplers are distinct slots, each noise once per kind.
        let mut noises = rules.noises.clone();
        noises.sort_by(|a, b| (&a.id, a.is_3d).cmp(&(&b.id, b.is_3d)));
        noises.dedup();
        assert_eq!(noises.len(), rules.noises.len());
        assert!(rules.noises.iter().any(|n| n.is_3d));
    }

    #[test]
    fn inline_conditions() {
        let rule = json!({
            "type": "condition",
            "if_true": {"type": "not", "invert": {"type": "temperature"}},
            "then_run": {"type": "sequence", "sequence": [
                {"type": "condition",
                 "if_true": {"type": "biome", "biome_is": ["plains", "minecraft:desert"]},
                 "then_run": {"type": "bandlands"}},
                {"type": "condition",
                 "if_true": {"type": "stone_depth", "offset": 1, "add_surface_depth": true,
                             "secondary_depth_range": 6, "surface_type": "ceiling"},
                 "then_run": {"type": "block", "result_state":
                     {"Name": "minecraft:water", "Properties": {"level": "0"}}}},
                {"type": "condition",
                 "if_true": {"type": "y_above", "anchor": {"relative_to_sea_level": -4},
                             "surface_depth_multiplier": -1, "add_stone_depth": true},
                 "then_run": {"type": "block", "result_state": "stone"}}
            ]}
        });
        let rules = parse(&rule).unwrap();
        assert_eq!(rules.conditions.len(), 5);
        assert_eq!(rules.conditions[0], Condition::Temperature);
        assert_eq!(rules.conditions[1], Condition::Not(0));
        let set = &rules.biome_sets[0];
        assert!(set.contains(biome_id("minecraft:plains").unwrap()));
        assert!(set.contains(biome_id("minecraft:desert").unwrap()));
        assert!(!set.contains(biome_id("minecraft:forest").unwrap()));
        assert_eq!(
            rules.conditions[3],
            Condition::StoneDepth {
                offset: 1,
                add_surface_depth: true,
                secondary_depth_range: 6,
                ceiling: true,
            }
        );
        assert_eq!(
            rules.conditions[4],
            Condition::Y {
                anchor: VerticalAnchor::RelativeToSeaLevel(-4),
                surface_depth_multiplier: -1,
                add_stone_depth: true,
            }
        );
    }

    #[test]
    fn biome_fold() {
        let mut set = BiomeSet::default();
        set.insert(3);
        set.insert(70);
        assert_eq!(set.fold(&[1, 2]), Some(false));
        assert_eq!(set.fold(&[]), Some(false));
        assert_eq!(set.fold(&[3, 70]), Some(true));
        assert_eq!(set.fold(&[3, 4]), None);
    }

    #[test]
    fn invalid_rules() {
        let block = json!({"type": "block", "result_state": "stone"});
        for bad in [
            json!({"type": "minecraft:nope"}),
            json!("minecraft:no_such_rule"),
            json!({"type": "block"}),
            json!({"type": "condition", "if_true": {"type": "water", "offset": 0,
                "surface_depth_multiplier": 21, "add_stone_depth": false}, "then_run": block}),
            json!({"type": "condition", "if_true": {"type": "noise_threshold",
                "noise": "minecraft:nope", "min_threshold": 0.0, "max_threshold": 1.0},
                "then_run": block}),
            json!({"type": "condition", "if_true": {"type": "biome", "biome_is": "nope"},
                "then_run": block}),
            json!({"type": "condition", "if_true": {"type": "stone_depth", "offset": 0,
                "add_surface_depth": false, "surface_type": "floor"}, "then_run": block}),
        ] {
            assert!(parse(&bad).is_err(), "{bad}");
        }
    }
}

//! Rule processor parts: block rule tests (`RuleTest`), position rule tests (`PosRuleTest`),
//! block entity modifiers (`RuleBlockEntityModifier`) and `ProcessorRule`.

use serde_json::Value;
use vanillite_base::BlockPos;
use vanillite_data::block::{BlockId, BlockState};
use vanillite_datapack::LoadedPacks;
use vanillite_nbt::{Compound, List, Tag};

use super::qualified_str;
use super::settings::TemplateRandom;
use crate::Error;
use crate::blocks::parse_block_state;
use crate::blocks::{BlockSet, qualify};
use crate::feature::region::WorldGenRegion;
use crate::feature::{field, float_field, int_or};

/// `RuleTest` (registry `rule_test`, dispatched on `predicate_type`).
#[derive(Clone, Debug)]
pub enum RuleTest {
    /// `always_true`.
    AlwaysTrue,
    /// `block_match`.
    BlockMatch(BlockId),
    /// `blockstate_match`.
    BlockStateMatch(BlockState),
    /// `tag_match`.
    TagMatch(BlockSet),
    /// `random_block_match`.
    RandomBlockMatch { block: BlockId, probability: f32 },
    /// `random_blockstate_match`.
    RandomBlockStateMatch { state: BlockState, probability: f32 },
    /// `all_of`.
    AllOf(Vec<RuleTest>),
    /// `any_of`.
    AnyOf(Vec<RuleTest>),
    /// `not`.
    Not(Box<RuleTest>),
    /// `height_match`.
    HeightMatch {
        min_inclusive: i32,
        max_inclusive: i32,
    },
}

fn block_by_name(json: &Value) -> Result<BlockId, Error> {
    let name = qualified_str(json, "block")?;
    BlockId::by_name(&name).ok_or_else(|| Error::new(format!("unknown block {name}")))
}

fn rule_list(packs: &LoadedPacks, json: &Value) -> Result<Vec<RuleTest>, Error> {
    field(json, "rules")?
        .as_array()
        .ok_or_else(|| Error::new("rules is not a list"))?
        .iter()
        .map(|r| RuleTest::parse(packs, r))
        .collect()
}

impl RuleTest {
    /// Parses a rule test.
    pub fn parse(packs: &LoadedPacks, json: &Value) -> Result<Self, Error> {
        let kind = qualified_str(json, "predicate_type")?;
        Ok(match kind.as_str() {
            "minecraft:always_true" => Self::AlwaysTrue,
            "minecraft:block_match" => Self::BlockMatch(block_by_name(json)?),
            "minecraft:blockstate_match" => {
                Self::BlockStateMatch(parse_block_state(field(json, "block_state")?)?)
            }
            "minecraft:tag_match" => {
                let tag = field(json, "tag")?
                    .as_str()
                    .ok_or_else(|| Error::new("tag is not a string"))?;
                Self::TagMatch(BlockSet::from_tag(packs, &qualify(tag)))
            }
            "minecraft:random_block_match" => Self::RandomBlockMatch {
                block: block_by_name(json)?,
                probability: float_field(json, "probability")?,
            },
            "minecraft:random_blockstate_match" => Self::RandomBlockStateMatch {
                state: parse_block_state(field(json, "block_state")?)?,
                probability: float_field(json, "probability")?,
            },
            "minecraft:all_of" => Self::AllOf(rule_list(packs, json)?),
            "minecraft:any_of" => Self::AnyOf(rule_list(packs, json)?),
            "minecraft:not" => Self::Not(Box::new(Self::parse(packs, field(json, "rule")?)?)),
            "minecraft:height_match" => Self::HeightMatch {
                min_inclusive: crate::feature::int_field(json, "min_inclusive")?,
                max_inclusive: crate::feature::int_field(json, "max_inclusive")?,
            },
            other => return Err(Error::new(format!("unknown rule test {other}"))),
        })
    }

    /// `test(state, pos, random)`.
    pub fn test(&self, state: BlockState, pos: BlockPos, random: &mut dyn TemplateRandom) -> bool {
        match self {
            Self::AlwaysTrue => true,
            Self::BlockMatch(block) => state.block() == *block,
            Self::BlockStateMatch(wanted) => state == *wanted,
            Self::TagMatch(set) => set.contains(state),
            Self::RandomBlockMatch { block, probability } => {
                state.block() == *block && random.next_float() < *probability
            }
            Self::RandomBlockStateMatch {
                state: wanted,
                probability,
            } => state == *wanted && random.next_float() < *probability,
            Self::AllOf(rules) => rules.iter().all(|r| r.test(state, pos, random)),
            Self::AnyOf(rules) => rules.iter().any(|r| r.test(state, pos, random)),
            Self::Not(rule) => !rule.test(state, pos, random),
            Self::HeightMatch {
                min_inclusive,
                max_inclusive,
            } => *min_inclusive <= pos.y && pos.y <= *max_inclusive,
        }
    }

    /// `testAgainstWorldState(level, pos, random)`: `always_true` does not read the block.
    pub fn test_against_world(
        &self,
        level: &WorldGenRegion<'_>,
        pos: BlockPos,
        random: &mut dyn TemplateRandom,
    ) -> bool {
        match self {
            Self::AlwaysTrue => true,
            _ => self.test(level.block(pos), pos, random),
        }
    }
}

/// `PosRuleTest` (registry `pos_rule_test`, dispatched on `predicate_type`).
#[derive(Clone, Debug)]
pub enum PosRuleTest {
    /// `always_true`.
    AlwaysTrue,
    /// `linear_pos`: chance by Manhattan distance to the reference.
    LinearPos(LinearChance),
    /// `axis_aligned_linear_pos`: chance by the distance along one axis (0 = x, 1 = y, 2 = z).
    AxisAlignedLinearPos { chance: LinearChance, axis: usize },
}

/// The shared fields of the linear position tests.
#[derive(Clone, Copy, Debug)]
pub struct LinearChance {
    min_chance: f32,
    max_chance: f32,
    min_dist: i32,
    max_dist: i32,
}

impl LinearChance {
    fn parse(json: &Value) -> Result<Self, Error> {
        let chance = Self {
            min_chance: crate::feature::float_or(json, "min_chance", 0.0),
            max_chance: crate::feature::float_or(json, "max_chance", 0.0),
            min_dist: int_or(json, "min_dist", 0),
            max_dist: int_or(json, "max_dist", 0),
        };
        if chance.min_dist >= chance.max_dist {
            return Err(Error::new(format!(
                "Invalid range: [{},{}]",
                chance.min_dist, chance.max_dist
            )));
        }
        Ok(chance)
    }

    /// `rnd <= Mth.clampedLerp(Mth.inverseLerp(dist, minDist, maxDist), minChance, maxChance)`.
    fn test(&self, dist: i32, random: &mut dyn TemplateRandom) -> bool {
        let rnd = random.next_float();
        let factor =
            (dist as f32 - self.min_dist as f32) / (self.max_dist as f32 - self.min_dist as f32);
        let chance = if factor < 0.0 {
            self.min_chance
        } else if factor > 1.0 {
            self.max_chance
        } else {
            self.min_chance + factor * (self.max_chance - self.min_chance)
        };
        rnd <= chance
    }
}

impl PosRuleTest {
    /// Parses a position rule test.
    pub fn parse(json: &Value) -> Result<Self, Error> {
        let kind = qualified_str(json, "predicate_type")?;
        Ok(match kind.as_str() {
            "minecraft:always_true" => Self::AlwaysTrue,
            "minecraft:linear_pos" => Self::LinearPos(LinearChance::parse(json)?),
            "minecraft:axis_aligned_linear_pos" => Self::AxisAlignedLinearPos {
                chance: LinearChance::parse(json)?,
                axis: match json.get("axis").and_then(Value::as_str).unwrap_or("y") {
                    "x" => 0,
                    "y" => 1,
                    "z" => 2,
                    other => return Err(Error::new(format!("unknown axis {other}"))),
                },
            },
            other => return Err(Error::new(format!("unknown position rule test {other}"))),
        })
    }

    /// `test(inTemplatePos, worldPos, worldReference, random)`.
    pub fn test(
        &self,
        _in_template: BlockPos,
        world: BlockPos,
        reference: BlockPos,
        random: &mut dyn TemplateRandom,
    ) -> bool {
        match self {
            Self::AlwaysTrue => true,
            Self::LinearPos(chance) => {
                let dist = (world.x - reference.x).abs()
                    + (world.y - reference.y).abs()
                    + (world.z - reference.z).abs();
                chance.test(dist, random)
            }
            Self::AxisAlignedLinearPos { chance, axis } => {
                let step = |a: usize| i32::from(*axis == a);
                let xd = ((world.x - reference.x) * step(0)).abs() as f32;
                let yd = ((world.y - reference.y) * step(1)).abs() as f32;
                let zd = ((world.z - reference.z) * step(2)).abs() as f32;
                chance.test((xd + yd + zd) as i32, random)
            }
        }
    }
}

/// `RuleBlockEntityModifier` (registry `rule_block_entity_modifier`, dispatched on `type`).
#[derive(Clone, Debug)]
pub enum BlockEntityModifier {
    /// `clear`: an empty tag.
    Clear,
    /// `passthrough`: the tag unchanged.
    Passthrough,
    /// `append_static`: merges a fixed tag.
    AppendStatic(Compound),
    /// `append_loot`: sets a loot table and a random loot table seed.
    AppendLoot(String),
}

impl BlockEntityModifier {
    /// Parses a modifier.
    pub fn parse(json: &Value) -> Result<Self, Error> {
        let kind = qualified_str(json, "type")?;
        Ok(match kind.as_str() {
            "minecraft:clear" => Self::Clear,
            "minecraft:passthrough" => Self::Passthrough,
            "minecraft:append_static" => match json_to_nbt(field(json, "data")?) {
                Tag::Compound(tag) => Self::AppendStatic(tag),
                _ => return Err(Error::new("append_static data is not a compound")),
            },
            "minecraft:append_loot" => Self::AppendLoot(qualified_str(json, "loot_table")?),
            other => return Err(Error::new(format!("unknown block entity modifier {other}"))),
        })
    }

    /// `apply(random, existingTag)`.
    pub fn apply(
        &self,
        random: &mut dyn TemplateRandom,
        existing: Option<Compound>,
    ) -> Option<Compound> {
        match self {
            Self::Clear => Some(Compound::new()),
            Self::Passthrough => existing,
            Self::AppendStatic(tag) => Some(match existing {
                None => tag.clone(),
                Some(mut existing) => {
                    merge(&mut existing, tag);
                    existing
                }
            }),
            Self::AppendLoot(table) => {
                let mut result = existing.unwrap_or_default();
                result.insert("LootTable", Tag::String(table.clone()));
                result.insert("LootTableSeed", Tag::Long(random.next_long()));
                Some(result)
            }
        }
    }
}

/// `CompoundTag.merge(other)`: nested compounds merge, everything else is replaced.
pub fn merge(target: &mut Compound, other: &Compound) {
    for (name, value) in other.iter() {
        if let Tag::Compound(other_inner) = value
            && let Some(Tag::Compound(mut inner)) = target.remove(name)
        {
            merge(&mut inner, other_inner);
            target.insert(name, Tag::Compound(inner));
            continue;
        }
        target.insert(name, value.clone());
    }
}

/// `JsonOps` → `NbtOps` conversion of `CompoundTag.CODEC`: numbers become doubles, booleans
/// bytes.
fn json_to_nbt(json: &Value) -> Tag {
    match json {
        Value::Null => Tag::Compound(Compound::new()),
        Value::Bool(b) => Tag::Byte(i8::from(*b)),
        Value::Number(n) => Tag::Double(n.as_f64().unwrap_or_default()),
        Value::String(s) => Tag::String(s.clone()),
        Value::Array(items) => Tag::List(List(items.iter().map(json_to_nbt).collect())),
        Value::Object(map) => {
            let mut compound = Compound::with_capacity(map.len());
            for (key, value) in map {
                compound.insert(key.as_str(), json_to_nbt(value));
            }
            Tag::Compound(compound)
        }
    }
}

/// `ProcessorRule`: input, location and position tests, the output state and the block entity
/// modifier.
#[derive(Clone, Debug)]
pub struct ProcessorRule {
    pub input_predicate: RuleTest,
    pub location_predicate: RuleTest,
    pub position_predicate: PosRuleTest,
    pub output_state: BlockState,
    pub block_entity_modifier: BlockEntityModifier,
}

impl ProcessorRule {
    /// Parses a rule. The position predicate and the block entity modifier are lenient optional
    /// fields: invalid values fall back to the default, as in Vanilla.
    pub fn parse(packs: &LoadedPacks, json: &Value) -> Result<Self, Error> {
        Ok(Self {
            input_predicate: RuleTest::parse(packs, field(json, "input_predicate")?)?,
            location_predicate: RuleTest::parse(packs, field(json, "location_predicate")?)?,
            position_predicate: json
                .get("position_predicate")
                .and_then(|p| PosRuleTest::parse(p).ok())
                .unwrap_or(PosRuleTest::AlwaysTrue),
            output_state: parse_block_state(field(json, "output_state")?)?,
            block_entity_modifier: json
                .get("block_entity_modifier")
                .and_then(|m| BlockEntityModifier::parse(m).ok())
                .unwrap_or(BlockEntityModifier::Passthrough),
        })
    }

    /// `test(level, inputState, inTemplatePos, worldPos, reference, random)`.
    pub fn test(
        &self,
        level: &WorldGenRegion<'_>,
        input: BlockState,
        in_template: BlockPos,
        world: BlockPos,
        reference: BlockPos,
        random: &mut dyn TemplateRandom,
    ) -> bool {
        self.input_predicate.test(input, world, random)
            && self
                .location_predicate
                .test_against_world(level, world, random)
            && self
                .position_predicate
                .test(in_template, world, reference, random)
    }
}

//! `RuleTest` (`structure.templatesystem`) and `BlockReplacement`, the targets of ore and
//! single block replacement features.

use serde_json::Value;
use vanillite_base::BlockPos;
use vanillite_base::random::RandomSource;
use vanillite_data::block::{BlockId, BlockState};

use super::state_field;
use crate::Error;
use crate::blocks::parse_block_state;
use crate::blocks::{BlockSet, qualify};
use crate::feature::{FeatureParser, field, float_field, int_field};

/// A rule test on a block state at a position.
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
    RandomBlockMatch(BlockId, f32),
    /// `random_blockstate_match`.
    RandomBlockStateMatch(BlockState, f32),
    /// `height_match`.
    HeightMatch {
        min_inclusive: i32,
        max_inclusive: i32,
    },
    /// `all_of`.
    AllOf(Vec<RuleTest>),
    /// `any_of`.
    AnyOf(Vec<RuleTest>),
    /// `not`.
    Not(Box<RuleTest>),
}

impl RuleTest {
    /// Parses `RuleTest.CODEC` (dispatched on `predicate_type`).
    pub fn parse(json: &Value, parser: &FeatureParser<'_>) -> Result<Self, Error> {
        let kind = field(json, "predicate_type")?
            .as_str()
            .ok_or_else(|| Error::new("predicate_type is not a string"))?;
        let block = |name: &str| -> Result<BlockId, Error> {
            let id = field(json, name)?
                .as_str()
                .ok_or_else(|| Error::new(format!("{name} is not a block id")))?;
            let id = qualify(id);
            BlockId::by_name(&id).ok_or_else(|| Error::new(format!("unknown block {id}")))
        };
        let list = |parser: &FeatureParser<'_>| -> Result<Vec<RuleTest>, Error> {
            field(json, "rules")?
                .as_array()
                .ok_or_else(|| Error::new("rules is not a list"))?
                .iter()
                .map(|r| Self::parse(r, parser))
                .collect()
        };
        Ok(match kind.strip_prefix("minecraft:").unwrap_or(kind) {
            "always_true" => Self::AlwaysTrue,
            "block_match" => Self::BlockMatch(block("block")?),
            "blockstate_match" => Self::BlockStateMatch(state_field(json, "block_state")?),
            "tag_match" => {
                let tag = field(json, "tag")?
                    .as_str()
                    .ok_or_else(|| Error::new("tag is not a string"))?;
                Self::TagMatch(parser.block_tag(tag))
            }
            "random_block_match" => {
                Self::RandomBlockMatch(block("block")?, float_field(json, "probability")?)
            }
            "random_blockstate_match" => Self::RandomBlockStateMatch(
                parse_block_state(field(json, "block_state")?)?,
                float_field(json, "probability")?,
            ),
            "height_match" => Self::HeightMatch {
                min_inclusive: int_field(json, "min_inclusive")?,
                max_inclusive: int_field(json, "max_inclusive")?,
            },
            "all_of" => Self::AllOf(list(parser)?),
            "any_of" => Self::AnyOf(list(parser)?),
            "not" => Self::Not(Box::new(Self::parse(field(json, "rule")?, parser)?)),
            other => return Err(Error::new(format!("unknown rule test {other}"))),
        })
    }

    /// The result of [`Self::test`] for every state of `block` at any position with block Y
    /// `y`, when those alone decide it (then the test draws no random number either).
    pub fn test_block_at(&self, block: BlockId, y: i32) -> Option<bool> {
        match self {
            Self::AlwaysTrue => Some(true),
            Self::BlockMatch(expected) => Some(block == *expected),
            Self::TagMatch(tag) => Some(tag.contains(block.default_state())),
            Self::HeightMatch {
                min_inclusive,
                max_inclusive,
            } => Some(*min_inclusive <= y && y <= *max_inclusive),
            Self::AllOf(rules) => rules
                .iter()
                .try_fold(true, |all, rule| Some(all & rule.test_block_at(block, y)?)),
            Self::AnyOf(rules) => rules
                .iter()
                .try_fold(false, |any, rule| Some(any | rule.test_block_at(block, y)?)),
            Self::Not(rule) => rule.test_block_at(block, y).map(|value| !value),
            Self::BlockStateMatch(_)
            | Self::RandomBlockMatch(..)
            | Self::RandomBlockStateMatch(..) => None,
        }
    }

    /// Adds the block Ys where a `HeightMatch` of the test changes its value.
    pub fn height_breaks(&self, breaks: &mut Vec<i32>) {
        match self {
            Self::HeightMatch {
                min_inclusive,
                max_inclusive,
            } => {
                breaks.push(*min_inclusive);
                breaks.push(max_inclusive.saturating_add(1));
            }
            Self::AllOf(rules) | Self::AnyOf(rules) => {
                rules.iter().for_each(|rule| rule.height_breaks(breaks));
            }
            Self::Not(rule) => rule.height_breaks(breaks),
            _ => {}
        }
    }

    /// `test(state, pos, random)`.
    pub fn test<R: RandomSource>(&self, state: BlockState, pos: BlockPos, random: &mut R) -> bool {
        match self {
            Self::AlwaysTrue => true,
            Self::BlockMatch(block) => state.block() == *block,
            Self::BlockStateMatch(expected) => state == *expected,
            Self::TagMatch(tag) => tag.contains(state),
            Self::RandomBlockMatch(block, probability) => {
                state.block() == *block && random.next_float() < *probability
            }
            Self::RandomBlockStateMatch(expected, probability) => {
                state == *expected && random.next_float() < *probability
            }
            Self::HeightMatch {
                min_inclusive,
                max_inclusive,
            } => *min_inclusive <= pos.y && pos.y <= *max_inclusive,
            Self::AllOf(rules) => rules.iter().all(|r| r.test(state, pos, random)),
            Self::AnyOf(rules) => rules.iter().any(|r| r.test(state, pos, random)),
            Self::Not(rule) => !rule.test(state, pos, random),
        }
    }
}

/// `BlockReplacement`: a rule test and the state that replaces matching blocks.
#[derive(Clone, Debug)]
pub struct BlockReplacement {
    pub target: RuleTest,
    pub state: BlockState,
}

impl BlockReplacement {
    /// Parses one `{"target": …, "state": …}` entry.
    pub fn parse(json: &Value, parser: &FeatureParser<'_>) -> Result<Self, Error> {
        Ok(Self {
            target: RuleTest::parse(field(json, "target")?, parser)?,
            state: state_field(json, "state")?,
        })
    }

    /// Parses the `targets` list of a feature.
    pub fn parse_list(json: &Value, parser: &FeatureParser<'_>) -> Result<Vec<Self>, Error> {
        field(json, "targets")?
            .as_array()
            .ok_or_else(|| Error::new("targets is not a list"))?
            .iter()
            .map(|t| Self::parse(t, parser))
            .collect()
    }
}

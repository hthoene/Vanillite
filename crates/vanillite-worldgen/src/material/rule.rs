//! Material rules (`net.minecraft.world.level.levelgen.material.rule`): what block a rule puts
//! at a position, and their evaluation (`compile` + `RuleEvaluator.tryApply`).

use serde_json::Value;
use vanillite_base::random::{PositionalRandomFactory, RandomSource};
use vanillite_data::block::BlockState;

use super::condition::CondId;
use super::context::MaterialRuleContext;

/// Index of a rule in [`super::rules::MaterialRules::rules`].
pub(crate) type RuleId = u32;

/// A parsed material rule (`MaterialRule`); registry references are inlined, so every
/// occurrence is compiled on its own as Vanilla's `HolderHolder` does.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Rule {
    /// `BlockRule`.
    Block(BlockState),
    /// `BandlandsRule`: the badlands terracotta bands.
    Bandlands,
    /// `SequenceRule`: the first rule that places a block wins.
    Sequence(Box<[RuleId]>),
    /// `ConditionRule`.
    Condition { if_true: CondId, then_run: RuleId },
    /// `OreVeinRule`: index into `MaterialRules::ore_veins`.
    OreVein(u32),
}

/// `OreVeinRule`: copper and iron veins. The density functions stay JSON until
/// [`super::MaterialSystem::new`] compiles them.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OreVein {
    pub ore_block: BlockState,
    pub raw_ore_block: BlockState,
    pub filler_block: BlockState,
    pub raw_ore_chance: f32,
    pub density: Value,
    pub richness: Value,
    pub filler_gap: Value,
}

impl MaterialRuleContext<'_> {
    /// `RuleEvaluator.tryApply(x, y, z)` of rule `id`: the block to place, if any.
    pub(super) fn try_apply(&mut self, id: RuleId, x: i32, y: i32, z: i32) -> Option<BlockState> {
        let system = self.system;
        match &system.rules.rules[id as usize] {
            Rule::Block(state) => Some(*state),
            Rule::Bandlands => Some(system.band(x, y, z)),
            Rule::Sequence(sequence) => sequence
                .iter()
                .find_map(|&rule| self.try_apply(rule, x, y, z)),
            &Rule::Condition { if_true, then_run } => {
                if self.test(if_true) {
                    self.try_apply(then_run, x, y, z)
                } else {
                    None
                }
            }
            &Rule::OreVein(vein) => self.ore_vein(vein as usize, x, y, z),
        }
    }

    /// The evaluator of `OreVeinRule.compile` (ore veins are never disabled outside debug).
    fn ore_vein(&mut self, vein: usize, x: i32, y: i32, z: i32) -> Option<BlockState> {
        let system = self.system;
        let rule = &system.rules.ore_veins[vein];
        let density = self.prefilled_density(vein, 0);
        if density <= 0.0 {
            return None;
        }
        let mut random = system.ore_random.at(x, y, z);
        if random.next_float() > density {
            return None;
        }
        let richness = self.prefilled_density(vein, 1);
        if random.next_float() < richness && self.filler_gap(vein) < 0.0 {
            Some(if random.next_float() < rule.raw_ore_chance {
                rule.raw_ore_block
            } else {
                rule.ore_block
            })
        } else {
            Some(rule.filler_block)
        }
    }
}

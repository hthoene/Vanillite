//! Material rules (`net.minecraft.world.level.levelgen.material.rule`): what block a rule puts
//! at a position, and their evaluation (`compile` + `RuleEvaluator.tryApply`).

use serde_json::Value;
use vanillite_base::random::{PositionalRandomFactory, RandomSource};
use vanillite_data::block::BlockState;

use super::condition::{CondId, Condition};
use super::context::MaterialRuleContext;
use super::rules::MaterialRules;
use super::system::OreVeinFns;
use crate::anchor::WorldGenerationContext;

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

/// The rule tree as `compile` leaves it for one chunk: conditions that are constant there
/// (biome conditions the chunk's possible biomes decide, and their negations) are folded away,
/// with the rules they gate, and sequences end at their first rule that always places a block.
/// Evaluating it gives the same blocks as the full tree: the folded conditions have no effects.
///
/// Each node also has the block Y range outside of which it never places a block (from Y-only
/// conditions and ore vein ranges), so sequences skip members without evaluating them.
#[derive(Clone, Debug, Default)]
pub(super) struct FoldedRules {
    nodes: Vec<Node>,
    /// Per node: `[min, max]` block Y where it may place a block.
    y_bounds: Vec<[i32; 2]>,
    /// Per node: whether it is gated by `above_preliminary_surface` (so it places nothing
    /// below the minimum surface level).
    above_surface: Vec<bool>,
    /// Members of the sequences, each a contiguous run.
    members: Vec<u32>,
    /// `None` when the root never places a block.
    root: Option<u32>,
}

/// A node of [`FoldedRules`].
#[derive(Clone, Copy, Debug)]
enum Node {
    Block(BlockState),
    Bandlands,
    OreVein(u32),
    /// `members[start..end]`, at least two.
    Sequence {
        start: u32,
        end: u32,
    },
    Condition {
        if_true: CondId,
        then_run: u32,
    },
}

/// What [`FoldedRules`] derives node Y ranges from: the chunk's generation context (to resolve
/// anchors) and the ore veins' density Y ranges.
pub(super) struct YBoundsInput<'a> {
    pub generation: &'a WorldGenerationContext,
    pub ore_veins: &'a [OreVeinFns],
}

/// No Y restriction.
const UNBOUNDED: [i32; 2] = [i32::MIN, i32::MAX];

/// What [`FoldedRules`] does at a block below its column's minimum surface level, as far as the
/// block's Y alone decides it (see [`FoldedRules::fold_below_surface`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BelowSurface {
    /// No block is placed.
    Nothing,
    /// This block is placed.
    Place(BlockState),
    /// The rules have to be evaluated.
    Unknown,
}

/// [`BelowSurface`] of one Y, valid where none of the ore veins of `ores` (bit `v`: vein `v`)
/// has a positive density: those veins come before the outcome and place their own blocks.
#[derive(Clone, Copy, Debug)]
pub(super) struct FoldedY {
    pub outcome: BelowSurface,
    pub ores: u32,
}

impl FoldedRules {
    /// Folds `rules` with `biome_folds` (per biome set: its constant value, if any); node Y
    /// ranges are derived when `bounds` is given (unbounded otherwise).
    pub(super) fn new(
        rules: &MaterialRules,
        biome_folds: &[Option<bool>],
        bounds: Option<&YBoundsInput<'_>>,
    ) -> Self {
        let mut folded = Self::default();
        folded.root = folded.fold(rules, biome_folds, bounds, rules.root);
        folded
    }

    /// Folds rule `id`; `None` when it never places a block.
    fn fold(
        &mut self,
        rules: &MaterialRules,
        folds: &[Option<bool>],
        bounds: Option<&YBoundsInput<'_>>,
        id: RuleId,
    ) -> Option<u32> {
        let node = match &rules.rules[id as usize] {
            Rule::Block(state) => Node::Block(*state),
            Rule::Bandlands => Node::Bandlands,
            Rule::OreVein(vein) => Node::OreVein(*vein),
            Rule::Sequence(sequence) => {
                let mut members = Vec::with_capacity(sequence.len());
                for &rule in sequence.iter() {
                    if let Some(node) = self.fold(rules, folds, bounds, rule) {
                        members.push(node);
                        if self.always_places(node) {
                            break;
                        }
                    }
                }
                match members[..] {
                    [] => return None,
                    [single] => return Some(single),
                    _ => {
                        let start = self.members.len() as u32;
                        self.members.extend_from_slice(&members);
                        Node::Sequence {
                            start,
                            end: self.members.len() as u32,
                        }
                    }
                }
            }
            &Rule::Condition { if_true, then_run } => match fold_condition(rules, folds, if_true) {
                Some(false) => return None,
                Some(true) => return self.fold(rules, folds, bounds, then_run),
                None => Node::Condition {
                    if_true,
                    then_run: self.fold(rules, folds, bounds, then_run)?,
                },
            },
        };
        let y_bounds = match bounds {
            Some(input) => self.y_bounds_of(rules, input, node),
            None => UNBOUNDED,
        };
        // Only a node's own condition: testing it first is what evaluating the node does.
        let above_surface = matches!(
            node,
            Node::Condition { if_true, .. }
                if matches!(
                    rules.conditions[if_true as usize],
                    Condition::AbovePreliminarySurface
                )
        );
        self.nodes.push(node);
        self.y_bounds.push(y_bounds);
        self.above_surface.push(above_surface);
        Some(self.nodes.len() as u32 - 1)
    }

    /// The block Y range where `node` (whose members are already folded) may place a block.
    fn y_bounds_of(&self, rules: &MaterialRules, input: &YBoundsInput<'_>, node: Node) -> [i32; 2] {
        match node {
            Node::Block(_) | Node::Bandlands => UNBOUNDED,
            Node::OreVein(vein) => match input.ore_veins[vein as usize].y_range {
                // Outside `[min, max)` the density is `<= 0`: no block.
                Some((min, max, _)) => [min, max - 1],
                None => UNBOUNDED,
            },
            Node::Sequence { start, end } => self.members[start as usize..end as usize]
                .iter()
                .map(|&member| self.y_bounds[member as usize])
                .fold([i32::MAX, i32::MIN], |[lo, hi], [min, max]| {
                    [lo.min(min), hi.max(max)]
                }),
            Node::Condition { if_true, then_run } => {
                let [lo, hi] = condition_y_bounds(rules, input.generation, if_true);
                let [min, max] = self.y_bounds[then_run as usize];
                [lo.max(min), hi.min(max)]
            }
        }
    }

    /// What the tree does at block Y `y` of a column whose minimum surface level is above
    /// `y`, decided from the conditions that depend on Y alone (and the surface level); the
    /// other inputs of a block (stone depth, water height, biome, noises) are then irrelevant.
    /// Ore veins whose bit is not in `ore_present` never place a block in this chunk.
    pub(super) fn fold_below_surface(
        &self,
        rules: &MaterialRules,
        generation: &WorldGenerationContext,
        ore_present: u32,
        y: i32,
    ) -> FoldedY {
        let mut ores = 0;
        let outcome = match self.root {
            Some(root) => self.fold_node(rules, generation, ore_present, y, root, &mut ores),
            None => BelowSurface::Nothing,
        };
        FoldedY { outcome, ores }
    }

    /// [`Self::fold_below_surface`] of `node`, following `apply`.
    fn fold_node(
        &self,
        rules: &MaterialRules,
        generation: &WorldGenerationContext,
        ore_present: u32,
        y: i32,
        node: u32,
        ores: &mut u32,
    ) -> BelowSurface {
        let [min, max] = self.y_bounds[node as usize];
        if y < min || y > max {
            return BelowSurface::Nothing;
        }
        match self.nodes[node as usize] {
            Node::Block(state) => BelowSurface::Place(state),
            Node::Bandlands => BelowSurface::Unknown,
            Node::OreVein(vein) if vein >= 32 => BelowSurface::Unknown,
            Node::OreVein(vein) => {
                // Without a positive density the vein places nothing and the sequence goes on.
                *ores |= ore_present & (1 << vein);
                BelowSurface::Nothing
            }
            Node::Sequence { start, end } => {
                for &member in &self.members[start as usize..end as usize] {
                    match self.fold_node(rules, generation, ore_present, y, member, ores) {
                        BelowSurface::Nothing => {}
                        decided => return decided,
                    }
                }
                BelowSurface::Nothing
            }
            Node::Condition { if_true, then_run } => {
                match condition_below_surface(rules, generation, if_true, y) {
                    Some(false) => BelowSurface::Nothing,
                    Some(true) => self.fold_node(rules, generation, ore_present, y, then_run, ores),
                    None => BelowSurface::Unknown,
                }
            }
        }
    }

    /// Whether `node` places a block wherever it is evaluated.
    fn always_places(&self, node: u32) -> bool {
        match self.nodes[node as usize] {
            Node::Block(_) | Node::Bandlands => true,
            // A sequence ends at its first member that always places a block.
            Node::Sequence { end, .. } => self.always_places(self.members[end as usize - 1]),
            Node::OreVein(_) | Node::Condition { .. } => false,
        }
    }
}

/// The block Y range where condition `id` may be true (exact for Y-only conditions, unbounded
/// for the others), with anchors resolved as `test` does.
fn condition_y_bounds(
    rules: &MaterialRules,
    generation: &WorldGenerationContext,
    id: CondId,
) -> [i32; 2] {
    match rules.conditions[id as usize] {
        Condition::VerticalGradient {
            true_at_and_below,
            false_at_and_above,
            ..
        } => {
            // True at and below `true_at_and_below`, false at and above `false_at_and_above`
            // otherwise, random in between.
            let true_at_and_below = true_at_and_below.resolve_y(generation);
            let false_at_and_above = false_at_and_above.resolve_y(generation);
            [
                i32::MIN,
                true_at_and_below.max(false_at_and_above.saturating_sub(1)),
            ]
        }
        Condition::Y {
            anchor,
            surface_depth_multiplier: 0,
            add_stone_depth: false,
        } => [anchor.resolve_y(generation), i32::MAX],
        _ => UNBOUNDED,
    }
}

/// The value of condition `id` at block Y `y` below the column's minimum surface level, when Y
/// decides it (as `test` evaluates it).
fn condition_below_surface(
    rules: &MaterialRules,
    generation: &WorldGenerationContext,
    id: CondId,
    y: i32,
) -> Option<bool> {
    match rules.conditions[id as usize] {
        Condition::AbovePreliminarySurface => Some(false),
        Condition::VerticalGradient {
            true_at_and_below,
            false_at_and_above,
            ..
        } => {
            if y <= true_at_and_below.resolve_y(generation) {
                Some(true)
            } else if y >= false_at_and_above.resolve_y(generation) {
                Some(false)
            } else {
                None
            }
        }
        Condition::Y {
            anchor,
            surface_depth_multiplier: 0,
            add_stone_depth: false,
        } => Some(y >= anchor.resolve_y(generation)),
        Condition::Not(target) => {
            condition_below_surface(rules, generation, target, y).map(|value| !value)
        }
        _ => None,
    }
}

/// The constant condition `id` folds to with `folds`, if any.
fn fold_condition(rules: &MaterialRules, folds: &[Option<bool>], id: CondId) -> Option<bool> {
    match rules.conditions[id as usize] {
        Condition::Biome(set) => folds[set as usize],
        Condition::Not(target) => fold_condition(rules, folds, target).map(|value| !value),
        _ => None,
    }
}

impl MaterialRuleContext<'_> {
    /// `RuleEvaluator.tryApply(x, y, z)` of the root rule: the block to place, if any.
    pub(super) fn try_apply(&mut self, x: i32, y: i32, z: i32) -> Option<BlockState> {
        let rules = self.rules;
        let root = rules.root?;
        self.apply(rules, root, x, y, z)
    }

    /// `tryApply` of folded node `node`.
    fn apply(
        &mut self,
        rules: &FoldedRules,
        mut node: u32,
        x: i32,
        y: i32,
        z: i32,
    ) -> Option<BlockState> {
        // Passed conditions and last sequence members continue in this frame; only the other
        // sequence members recurse.
        loop {
            match rules.nodes[node as usize] {
                Node::Block(state) => return Some(state),
                Node::Bandlands => return Some(self.system.band(x, y, z)),
                Node::OreVein(vein) => return self.ore_vein(vein as usize, x, y, z),
                Node::Condition { if_true, then_run } => {
                    if !self.test(if_true) {
                        return None;
                    }
                    node = then_run;
                }
                Node::Sequence { start, end } => {
                    let members = &rules.members[start as usize..end as usize];
                    let (&last, rest) = members.split_last()?;
                    for &member in rest {
                        let [min, max] = rules.y_bounds[member as usize];
                        if y < min
                            || y > max
                            || (rules.above_surface[member as usize]
                                && y < self.min_surface_level())
                        {
                            continue;
                        }
                        // Most members are gated by a condition that fails: test it here
                        // instead of in a call.
                        let state = match rules.nodes[member as usize] {
                            Node::Condition { if_true, then_run } => {
                                if !self.test(if_true) {
                                    continue;
                                }
                                self.apply(rules, then_run, x, y, z)
                            }
                            Node::OreVein(vein) => self.ore_vein(vein as usize, x, y, z),
                            _ => self.apply(rules, member, x, y, z),
                        };
                        if state.is_some() {
                            return state;
                        }
                    }
                    let [min, max] = rules.y_bounds[last as usize];
                    if y < min || y > max {
                        return None;
                    }
                    node = last;
                }
            }
        }
    }

    /// The evaluator of `OreVeinRule.compile` (ore veins are never disabled outside debug).
    fn ore_vein(&mut self, vein: usize, x: i32, y: i32, z: i32) -> Option<BlockState> {
        let system = self.system;
        let rule = &system.rules.ore_veins[vein];
        if let Some((min, max, _)) = system.ore_vein_fns[vein].y_range
            && !(min..max).contains(&y)
        {
            // The density is a constant `<= 0` there.
            return None;
        }
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

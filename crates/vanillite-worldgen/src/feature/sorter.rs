//! `FeatureSorter.buildFeaturesPerStep`: one global order of placed features per step that
//! respects every biome's own order.

use std::collections::{BTreeMap, BTreeSet};

use rustc_hash::FxHashMap;

use super::PlacedFeatureId;
use crate::Error;

/// A node of the order graph, compared by step, then first-seen index (`FeatureData`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Node {
    step: usize,
    index: usize,
    feature: PlacedFeatureId,
}

/// Sorts the placed features of `sources` (per source: per step, its features) into one list per
/// step, as Vanilla does for the biome source's possible biomes. Errors on an order cycle.
pub fn build_features_per_step(
    sources: &[&[Vec<PlacedFeatureId>]],
) -> Result<Vec<Vec<PlacedFeatureId>>, Error> {
    let mut first_index: FxHashMap<PlacedFeatureId, usize> = FxHashMap::default();
    let mut edges: BTreeMap<Node, BTreeSet<Node>> = BTreeMap::new();
    let mut max_step = 0;
    for steps in sources {
        max_step = max_step.max(steps.len());
        let mut list = Vec::new();
        for (step, features) in steps.iter().enumerate() {
            for &feature in features {
                let next = first_index.len();
                let index = *first_index.entry(feature).or_insert(next);
                list.push(Node {
                    step,
                    index,
                    feature,
                });
            }
        }
        for (i, node) in list.iter().enumerate() {
            let out = edges.entry(*node).or_default();
            if let Some(next) = list.get(i + 1) {
                out.insert(*next);
            }
        }
    }

    let mut discovered = BTreeSet::new();
    let mut visiting = BTreeSet::new();
    let mut sorted = Vec::new();
    for &node in edges.keys() {
        if !discovered.contains(&node)
            && depth_first(&edges, &mut discovered, &mut visiting, &mut sorted, node)
        {
            return Err(Error::new("feature order cycle found"));
        }
    }
    sorted.reverse();
    Ok((0..max_step)
        .map(|step| {
            sorted
                .iter()
                .filter(|n| n.step == step)
                .map(|n| n.feature)
                .collect()
        })
        .collect())
}

/// `Graph.depthFirstSearch`: true if a cycle was found.
fn depth_first(
    edges: &BTreeMap<Node, BTreeSet<Node>>,
    discovered: &mut BTreeSet<Node>,
    visiting: &mut BTreeSet<Node>,
    sorted: &mut Vec<Node>,
    current: Node,
) -> bool {
    if discovered.contains(&current) {
        return false;
    }
    if !visiting.insert(current) {
        return true;
    }
    if let Some(next) = edges.get(&current) {
        for &n in next {
            if depth_first(edges, discovered, visiting, sorted, n) {
                return true;
            }
        }
    }
    visiting.remove(&current);
    discovered.insert(current);
    sorted.push(current);
    false
}

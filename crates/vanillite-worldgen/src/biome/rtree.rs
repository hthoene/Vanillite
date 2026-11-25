//! `Climate.RTree`: the search tree over the climate regions of a parameter list.
//!
//! The tree is built exactly like Vanilla's (same sorts, same bucket sizes, same stable sort
//! order on ties) because the search result on ties depends on the leaf order.
//!
//! # Ties and the last-result cache
//!
//! `SubTree.search` keeps a candidate and only descends into children that are strictly closer
//! than it, replacing it only by strictly closer leaves. Node distances are lower bounds of their
//! leaves' distances, so the search always returns a leaf at the minimal distance, and:
//!
//! * without a candidate it returns the *first* minimal leaf in depth-first tree order (every
//!   subtree containing a minimal leaf is closer than any non-minimal candidate, so it is
//!   entered, and once a minimal leaf is found nothing replaces it);
//! * with a candidate that is itself at the minimal distance it returns the candidate.
//!
//! Vanilla passes the previous result of the same thread (`ThreadLocal lastResult`) as the
//! candidate, so on exact ties (common: neighbouring regions share their boundary values, and
//! a quantized climate lands on a boundary for about one in 10000 float values per axis) the
//! biome depends on the previous search. This port keeps the cache explicit
//! ([`super::climate::SearchCache`]) and owned by each caller, which reproduces Vanilla for the
//! same sequence of searches. Vanilla's own results differ from that only when its worker thread
//! searched the same tree for something else just before (another chunk), which is
//! scheduling-dependent and not reproducible in Vanilla either.

use super::climate::{Parameter, distance};

/// Number of dimensions of the parameter space (six climate values and the offset).
const DIMENSIONS: usize = 7;

/// The R-tree; nodes `0..leaf_count` are the leaves in value order, subtrees follow.
#[derive(Debug)]
pub(crate) struct RTree {
    /// Parameter space (bounding box) of every node.
    spaces: Vec<[Parameter; DIMENSIONS]>,
    /// Children of every subtree as a range into `child_list` (empty for leaves).
    children: Vec<(u32, u32)>,
    child_list: Vec<u32>,
    /// The parameter space of each `child_list` entry, so a node's children are contiguous.
    child_spaces: Vec<[Parameter; DIMENSIONS]>,
    leaf_count: usize,
    root: usize,
}

impl RTree {
    /// `RTree.create(values, childrenPerNode)` over the parameter spaces of the values.
    pub(crate) fn build(leaves: &[[Parameter; DIMENSIONS]], children_per_node: usize) -> Self {
        assert!(!leaves.is_empty(), "need at least one value");
        let mut tree = Self {
            spaces: leaves.to_vec(),
            children: vec![(0, 0); leaves.len()],
            child_list: Vec::new(),
            child_spaces: Vec::new(),
            leaf_count: leaves.len(),
            root: 0,
        };
        tree.root = tree.build_node((0..leaves.len()).collect(), children_per_node);
        tree
    }

    /// `RTree.build(dimensions, children, childrenPerNode)`.
    fn build_node(&mut self, mut children: Vec<usize>, children_per_node: usize) -> usize {
        if children.len() == 1 {
            return children[0];
        }
        if children.len() <= children_per_node {
            children.sort_by_key(|&c| {
                self.spaces[c]
                    .iter()
                    .fold(0i64, |sum, p| sum.wrapping_add(p.center().wrapping_abs()))
            });
            return self.push_subtree(&children);
        }
        let mut min_cost = i64::MAX;
        let mut min_dimension = 0;
        let mut min_buckets: Vec<Vec<usize>> = Vec::new();
        for d in 0..DIMENSIONS {
            // Each sort starts from the previous order, as Vanilla sorts the same list in place.
            children.sort_by(|&a, &b| compare_spaces(&self.spaces[a], &self.spaces[b], d, false));
            let buckets = bucketize(&children, children_per_node);
            let total_cost = buckets.iter().fold(0i64, |sum, b| {
                sum.wrapping_add(cost(&self.union_space(b.iter().copied())))
            });
            if min_cost > total_cost {
                min_cost = total_cost;
                min_dimension = d;
                min_buckets = buckets;
            }
        }
        let mut buckets: Vec<(Vec<usize>, [Parameter; DIMENSIONS])> = min_buckets
            .into_iter()
            .map(|b| {
                let space = self.union_space(b.iter().copied());
                (b, space)
            })
            .collect();
        // `sort(minBuckets, dimensions, minDimension, true)`.
        buckets.sort_by(|a, b| compare_spaces(&a.1, &b.1, min_dimension, true));
        let nodes: Vec<usize> = buckets
            .into_iter()
            .map(|(b, _)| self.build_node(b, children_per_node))
            .collect();
        self.push_subtree(&nodes)
    }

    /// `buildParameterSpace(children)`: the union of the children's spaces.
    fn union_space(&self, nodes: impl Iterator<Item = usize>) -> [Parameter; DIMENSIONS] {
        let mut space: Option<[Parameter; DIMENSIONS]> = None;
        for n in nodes {
            let s = &self.spaces[n];
            space = Some(match space {
                None => *s,
                Some(acc) => std::array::from_fn(|d| s[d].union(&acc[d])),
            });
        }
        space.expect("SubTree needs at least one child")
    }

    fn push_subtree(&mut self, children: &[usize]) -> usize {
        let space = self.union_space(children.iter().copied());
        let start = self.child_list.len() as u32;
        self.child_list.extend(children.iter().map(|&c| c as u32));
        let spaces: Vec<_> = children.iter().map(|&c| self.spaces[c]).collect();
        self.child_spaces.extend(spaces);
        self.spaces.push(space);
        self.children.push((start, children.len() as u32));
        self.spaces.len() - 1
    }

    #[inline]
    fn is_leaf(&self, node: usize) -> bool {
        node < self.leaf_count
    }

    /// `RTree.search(target)` with the previous result as candidate; returns the value index.
    #[inline]
    pub(crate) fn search(&self, target: &[i64; DIMENSIONS], candidate: Option<usize>) -> usize {
        let candidate = candidate.filter(|&c| self.is_leaf(c));
        if self.is_leaf(self.root) {
            return self.root;
        }
        // Vanilla throws (NullPointerException) if no leaf is closer than `Long.MAX_VALUE`, which
        // needs absurd targets; fall back to the first value.
        let candidate = candidate.map(|c| (c, distance(&self.spaces[c], target)));
        self.search_subtree(self.root, target, candidate)
            .map_or(0, |(leaf, _)| leaf)
    }

    /// `SubTree.search(target, candidate, Node::distance)`; leaves come with their distance
    /// to `target` (computed once instead of at every level).
    fn search_subtree(
        &self,
        node: usize,
        target: &[i64; DIMENSIONS],
        candidate: Option<(usize, i64)>,
    ) -> Option<(usize, i64)> {
        let mut min_distance = candidate.map_or(i64::MAX, |(_, d)| d);
        let mut closest = candidate;
        let (start, len) = self.children[node];
        let range = start as usize..(start + len) as usize;
        for (&child, space) in self.child_list[range.clone()]
            .iter()
            .zip(&self.child_spaces[range])
        {
            let child = child as usize;
            let child_distance = distance(space, target);
            if min_distance > child_distance {
                let (leaf, leaf_distance) = if self.is_leaf(child) {
                    (child, child_distance)
                } else {
                    let Some(found) = self.search_subtree(child, target, closest) else {
                        continue;
                    };
                    found
                };
                if min_distance > leaf_distance {
                    min_distance = leaf_distance;
                    closest = Some((leaf, leaf_distance));
                }
            }
        }
        closest
    }
}

/// `RTree.comparator(dimension, absolute)` chained over all dimensions starting at `dimension`.
fn compare_spaces(
    a: &[Parameter; DIMENSIONS],
    b: &[Parameter; DIMENSIONS],
    dimension: usize,
    absolute: bool,
) -> std::cmp::Ordering {
    let key = |p: &Parameter| {
        let c = p.center();
        if absolute { c.wrapping_abs() } else { c }
    };
    for k in 0..DIMENSIONS {
        let d = (dimension + k) % DIMENSIONS;
        let ord = key(&a[d]).cmp(&key(&b[d]));
        if ord.is_ne() {
            return ord;
        }
    }
    std::cmp::Ordering::Equal
}

/// `RTree.bucketize(nodes, childrenPerNode)`.
fn bucketize(nodes: &[usize], children_per_node: usize) -> Vec<Vec<usize>> {
    let per_node = children_per_node as f64;
    let expected = per_node.powf(((nodes.len() as f64 - 0.01).ln() / per_node.ln()).floor()) as i32;
    let expected = expected.max(1) as usize;
    nodes.chunks(expected).map(<[usize]>::to_vec).collect()
}

/// `RTree.cost(parameterSpace)`.
fn cost(space: &[Parameter; DIMENSIONS]) -> i64 {
    space.iter().fold(0i64, |sum, p| {
        sum.wrapping_add(p.max.wrapping_sub(p.min).wrapping_abs())
    })
}

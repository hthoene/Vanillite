//! The dragon's flight graph: 24 fixed nodes around the main island (`EnderDragon.nodes`,
//! `nodeAdjacency`) and Vanilla's A* over them (`findPath` with `BinaryHeap`).

#![allow(clippy::neg_cmp_op_on_partial_ord)] // Vanilla's `!(a < b)` keeps NaN costs in place.

use vanillite_base::BlockPos;

/// `nodeAdjacency`: bit `j` of entry `i` links node `i` to node `j`.
const ADJACENCY: [u32; 24] = [
    6146, 8197, 8202, 16404, 32808, 32848, 65696, 131_392, 131_712, 263_424, 526_848, 525_313,
    1_581_057, 3_166_214, 2_138_120, 6_373_424, 4_358_208, 12_910_976, 9_044_480, 9_706_496,
    15_216_640, 13_688_832, 11_763_712, 8_257_536,
];

/// A graph node (`Node`): a block position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Node {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

impl Node {
    pub const fn new(x: i32, y: i32, z: i32) -> Node {
        Node { x, y, z }
    }

    /// `distanceTo` in float arithmetic.
    #[allow(clippy::cast_precision_loss)]
    pub fn distance_to(self, to: Node) -> f32 {
        let (xd, yd, zd) = (
            (to.x - self.x) as f32,
            (to.y - self.y) as f32,
            (to.z - self.z) as f32,
        );
        (xd * xd + yd * yd + zd * zd).sqrt()
    }

    /// `distanceToSqr` in float arithmetic.
    #[allow(clippy::cast_precision_loss)]
    pub fn distance_to_sqr(self, to: Node) -> f32 {
        let (xd, yd, zd) = (
            (to.x - self.x) as f32,
            (to.y - self.y) as f32,
            (to.z - self.z) as f32,
        );
        xd * xd + yd * yd + zd * zd
    }
}

/// A path through the graph (`Path`): its nodes and the index of the next one.
#[derive(Clone, Debug, PartialEq)]
pub struct Path {
    pub nodes: Vec<Node>,
    pub next: usize,
}

impl Path {
    /// `advance`.
    pub fn advance(&mut self) {
        self.next += 1;
    }

    /// `isDone`.
    pub fn is_done(&self) -> bool {
        self.next >= self.nodes.len()
    }

    /// `getNextNodePos`.
    pub fn next_node(&self) -> Node {
        self.nodes[self.next.min(self.nodes.len().saturating_sub(1))]
    }
}

/// The 24 nodes, computed once from the terrain (`findClosestNode`'s lazy setup).
#[derive(Clone, Debug, PartialEq)]
pub struct Graph {
    pub nodes: [Node; 24],
}

/// The search state of one node (`Node.f/g/h/closed/cameFrom/heapIdx`).
#[derive(Clone, Copy, Debug)]
struct Search {
    f: f32,
    g: f32,
    h: f32,
    closed: bool,
    came_from: Option<usize>,
    heap_idx: i32,
}

/// Index of the extra final node in the search arrays.
const FINAL: usize = 24;

impl Graph {
    /// The node positions: an outer ring of 12 at radius 60, a ring of 8 at 40 (10 higher) and
    /// an inner ring of 4 at 20; each at least at y 73 and 5 (15) above the surface `height`
    /// (`getHeightmapPos(MOTION_BLOCKING_NO_LEAVES)`).
    pub fn new(mut height: impl FnMut(i32, i32) -> i32) -> Graph {
        let pi = std::f32::consts::PI;
        let nodes = std::array::from_fn(|i| {
            let mut y_adjust = 5;
            let (x, z) = if i < 12 {
                let a = 2.0 * (-pi + pi / 12.0 * i as f32);
                (
                    vanillite_base::math::floor(f64::from(
                        60.0 * vanillite_base::math::cos(f64::from(a)),
                    )),
                    vanillite_base::math::floor(f64::from(
                        60.0 * vanillite_base::math::sin(f64::from(a)),
                    )),
                )
            } else if i < 20 {
                let m = (i - 12) as f32;
                let a = 2.0 * (-pi + pi / 8.0 * m);
                y_adjust += 10;
                (
                    vanillite_base::math::floor(f64::from(
                        40.0 * vanillite_base::math::cos(f64::from(a)),
                    )),
                    vanillite_base::math::floor(f64::from(
                        40.0 * vanillite_base::math::sin(f64::from(a)),
                    )),
                )
            } else {
                let m = (i - 20) as f32;
                let a = 2.0 * (-pi + pi / 4.0 * m);
                (
                    vanillite_base::math::floor(f64::from(
                        20.0 * vanillite_base::math::cos(f64::from(a)),
                    )),
                    vanillite_base::math::floor(f64::from(
                        20.0 * vanillite_base::math::sin(f64::from(a)),
                    )),
                )
            };
            let y = 73.max(height(x, z) + y_adjust);
            Node::new(x, y, z)
        });
        Graph { nodes }
    }

    /// `findClosestNode(x, y, z)`: the outer ring only counts while crystals are alive.
    pub fn closest(&self, x: f64, y: f64, z: f64, crystals_alive: bool) -> usize {
        let pos = Node::new(
            vanillite_base::math::floor(x),
            vanillite_base::math::floor(y),
            vanillite_base::math::floor(z),
        );
        let start = if crystals_alive { 0 } else { 12 };
        let mut closest = 10_000.0_f32;
        let mut index = 0;
        for (i, node) in self.nodes.iter().enumerate().skip(start) {
            let d = node.distance_to_sqr(pos);
            if d < closest {
                closest = d;
                index = i;
            }
        }
        index
    }

    /// `findPath(start, end, finalNode)`.
    pub fn find_path(
        &self,
        start: usize,
        end: usize,
        final_node: Option<Node>,
        crystals_alive: bool,
    ) -> Option<Path> {
        let mut s = [Search {
            f: 0.0,
            g: 0.0,
            h: 0.0,
            closed: false,
            came_from: None,
            heap_idx: -1,
        }; 25];
        let node = |i: usize| -> Node {
            if i == FINAL {
                final_node.unwrap_or(Node::new(0, 0, 0))
            } else {
                self.nodes[i]
            }
        };
        let to = end;
        s[start].g = 0.0;
        s[start].h = node(start).distance_to(node(to));
        s[start].f = s[start].h;
        let mut heap = Heap::default();
        heap.insert(&mut s, start);
        let mut closest = start;
        let min_index = if crystals_alive { 0 } else { 12 };
        while !heap.is_empty() {
            let open = heap.pop(&mut s);
            // `openNode.equals(to)`: by position.
            if node(open) == node(to) {
                let mut last = to;
                if final_node.is_some() {
                    s[FINAL].came_from = Some(to);
                    last = FINAL;
                }
                return Some(reconstruct(&s, last, node));
            }
            if node(open).distance_to(node(to)) < node(closest).distance_to(node(to)) {
                closest = open;
            }
            s[open].closed = true;
            for j in min_index..24 {
                if ADJACENCY[open] & (1 << j) == 0 || s[j].closed {
                    continue;
                }
                let tentative = s[open].g + node(open).distance_to(node(j));
                if s[j].heap_idx < 0 || tentative < s[j].g {
                    s[j].came_from = Some(open);
                    s[j].g = tentative;
                    s[j].h = node(j).distance_to(node(to));
                    if s[j].heap_idx >= 0 {
                        let cost = s[j].g + s[j].h;
                        heap.change_cost(&mut s, j, cost);
                    } else {
                        s[j].f = s[j].g + s[j].h;
                        heap.insert(&mut s, j);
                    }
                }
            }
        }
        if closest == start {
            return None;
        }
        let mut last = closest;
        if final_node.is_some() {
            s[FINAL].came_from = Some(closest);
            last = FINAL;
        }
        Some(reconstruct(&s, last, node))
    }
}

/// `reconstructPath`.
fn reconstruct(s: &[Search; 25], last: usize, node: impl Fn(usize) -> Node) -> Path {
    let mut nodes = vec![node(last)];
    let mut at = last;
    while let Some(from) = s[at].came_from {
        nodes.push(node(from));
        at = from;
    }
    nodes.reverse();
    Path { nodes, next: 0 }
}

/// Vanilla's `BinaryHeap` over node indices (ties resolve exactly as Vanilla's).
#[derive(Default)]
struct Heap {
    heap: Vec<usize>,
}

impl Heap {
    fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }

    fn insert(&mut self, s: &mut [Search; 25], node: usize) {
        self.heap.push(node);
        s[node].heap_idx = (self.heap.len() - 1) as i32;
        self.up(s, self.heap.len() - 1);
    }

    fn pop(&mut self, s: &mut [Search; 25]) -> usize {
        let popped = self.heap[0];
        let last = self.heap.pop().unwrap_or(popped);
        if !self.heap.is_empty() {
            self.heap[0] = last;
            self.down(s, 0);
        }
        s[popped].heap_idx = -1;
        popped
    }

    fn change_cost(&mut self, s: &mut [Search; 25], node: usize, cost: f32) {
        let old = s[node].f;
        s[node].f = cost;
        let idx = s[node].heap_idx as usize;
        if cost < old {
            self.up(s, idx);
        } else {
            self.down(s, idx);
        }
    }

    fn up(&mut self, s: &mut [Search; 25], mut idx: usize) {
        let node = self.heap[idx];
        let cost = s[node].f;
        while idx > 0 {
            let parent_idx = (idx - 1) >> 1;
            let parent = self.heap[parent_idx];
            if !(cost < s[parent].f) {
                break;
            }
            self.heap[idx] = parent;
            s[parent].heap_idx = idx as i32;
            idx = parent_idx;
        }
        self.heap[idx] = node;
        s[node].heap_idx = idx as i32;
    }

    fn down(&mut self, s: &mut [Search; 25], mut idx: usize) {
        let node = self.heap[idx];
        let cost = s[node].f;
        let size = self.heap.len();
        loop {
            let left = 1 + (idx << 1);
            let right = left + 1;
            if left >= size {
                break;
            }
            let left_node = self.heap[left];
            let left_cost = s[left_node].f;
            let (right_node, right_cost) = if right >= size {
                (usize::MAX, f32::INFINITY)
            } else {
                (self.heap[right], s[self.heap[right]].f)
            };
            if left_cost < right_cost {
                if !(left_cost < cost) {
                    break;
                }
                self.heap[idx] = left_node;
                s[left_node].heap_idx = idx as i32;
                idx = left;
            } else {
                if !(right_cost < cost) {
                    break;
                }
                self.heap[idx] = right_node;
                s[right_node].heap_idx = idx as i32;
                idx = right;
            }
        }
        self.heap[idx] = node;
        s[node].heap_idx = idx as i32;
    }
}

/// A block position as a node.
pub fn node_of(pos: BlockPos) -> Node {
    Node::new(pos.x, pos.y, pos.z)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat() -> Graph {
        Graph::new(|_, _| 64)
    }

    #[test]
    fn rings_and_heights() {
        let g = flat();
        assert_eq!(g.nodes[0], Node::new(60, 73, 0));
        assert_eq!(g.nodes[6], Node::new(-60, 73, 0));
        // The middle ring is 10 higher than the floor of 73.
        assert_eq!(g.nodes[12].y, 79);
        assert_eq!(g.nodes[20].y, 73);
        assert_eq!(g.nodes[20], Node::new(20, 73, 0));
    }

    #[test]
    fn closest_skips_outer_ring_without_crystals() {
        let g = flat();
        assert_eq!(g.closest(-60.0, 73.0, 0.0, true), 6);
        assert!(g.closest(-60.0, 73.0, 0.0, false) >= 12);
    }

    #[test]
    fn path_follows_adjacency() {
        let g = flat();
        let path = g.find_path(0, 3, None, true).expect("path");
        assert_eq!(path.nodes.first(), Some(&g.nodes[0]));
        assert_eq!(path.nodes.last(), Some(&g.nodes[3]));
        for w in path.nodes.windows(2) {
            let a = g.nodes.iter().position(|n| *n == w[0]).unwrap();
            let b = g.nodes.iter().position(|n| *n == w[1]).unwrap();
            assert!(ADJACENCY[a] & (1 << b) != 0, "{a} -> {b}");
        }
        let with_final = g
            .find_path(0, 3, Some(Node::new(1, 2, 3)), true)
            .expect("path");
        assert_eq!(with_final.nodes.last(), Some(&Node::new(1, 2, 3)));
    }
}

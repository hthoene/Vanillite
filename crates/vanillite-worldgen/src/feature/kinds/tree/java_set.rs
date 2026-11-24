//! A set of block positions that iterates exactly like Java's `HashSet<BlockPos>`.
//!
//! Trees collect their roots, logs, leaves and decorations in `HashSet`s; Vanilla then turns
//! them into lists (decorators pick and shuffle positions from them) and runs the leaves
//! distance update by repeatedly taking the first element of a `HashSet`. The iteration order of
//! those sets therefore decides which blocks a tree places, so this type reproduces the layout of
//! `java.util.HashMap`:
//!
//! - `BlockPos.hashCode()` spread by `h ^ (h >>> 16)`, power of two tables starting at 16 that
//!   double when the size exceeds three quarters of the capacity, splitting each bucket into a
//!   low and a high bucket in order;
//! - new entries appended to their bucket's chain; a chain that grows past eight entries makes
//!   a table smaller than 64 grow, and turns into a red-black tree bin otherwise;
//! - tree bins: nodes ordered by hash, the linked (iteration) order changed by moving the root
//!   to the front and by linking new nodes after their tree parent; removal through the
//!   iterator (`movable = false`) keeps the bin a tree; resizing turns halves of six or fewer
//!   nodes back into chains and rebuilds the others.
//!
//! One thing cannot be reproduced: for two distinct positions with equal hash codes in one tree
//! bin, Java breaks the tie with `System.identityHashCode`, which is not deterministic even in
//! Vanilla. This set orders them by `(y, z, x)` instead.

use vanillite_base::BlockPos;

/// No node.
const NONE: u32 = u32::MAX;
/// `HashMap.DEFAULT_INITIAL_CAPACITY`.
const INITIAL_CAPACITY: usize = 16;
/// `HashMap.MIN_TREEIFY_CAPACITY`.
const MIN_TREEIFY_CAPACITY: usize = 64;
/// `HashMap.TREEIFY_THRESHOLD - 1`: the chain length (before an insertion) that triggers
/// `treeifyBin`.
const TREEIFY_CHAIN: usize = 7;
/// `HashMap.UNTREEIFY_THRESHOLD`.
const UNTREEIFY_THRESHOLD: usize = 6;

/// A `HashMap.Node`, with the `TreeNode` links used while its bin is a tree.
#[derive(Clone, Copy, Debug)]
struct Node {
    pos: BlockPos,
    hash: i32,
    next: u32,
    /// Whether this is a `TreeNode` (its bin is a tree bin).
    tree: bool,
    prev: u32,
    parent: u32,
    left: u32,
    right: u32,
    red: bool,
}

/// `HashSet<BlockPos>` with Java's iteration order.
#[derive(Clone, Debug, Default)]
pub struct JavaHashSet {
    /// First node of each bucket (`HashMap.table`); empty until the first insertion.
    heads: Vec<u32>,
    /// Node storage; removed nodes are never reused.
    nodes: Vec<Node>,
    len: usize,
    threshold: usize,
    /// No bucket below this index holds a node.
    first_bucket: usize,
}

/// `HashMap.hash(pos)`: `BlockPos.hashCode()` with the high bits spread into the low ones.
#[inline]
fn spread_hash(pos: BlockPos) -> i32 {
    let h = pos
        .y
        .wrapping_add(pos.z.wrapping_mul(31))
        .wrapping_mul(31)
        .wrapping_add(pos.x);
    h ^ ((h as u32) >> 16) as i32
}

/// `HashMap.TreeNode.tieBreakOrder` for distinct positions with equal hashes (see the module
/// documentation): never 0.
#[inline]
fn tie_break(a: BlockPos, b: BlockPos) -> i32 {
    if (a.y, a.z, a.x) <= (b.y, b.z, b.x) {
        -1
    } else {
        1
    }
}

impl JavaHashSet {
    /// An empty set (`new HashSet<>()`).
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of positions.
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the set is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline]
    fn bucket(&self, hash: i32) -> usize {
        hash as u32 as usize & (self.heads.len() - 1)
    }

    #[inline]
    fn node(&self, id: u32) -> &Node {
        &self.nodes[id as usize]
    }

    #[inline]
    fn node_mut(&mut self, id: u32) -> &mut Node {
        &mut self.nodes[id as usize]
    }

    fn push(&mut self, pos: BlockPos, hash: i32, next: u32, tree: bool) -> u32 {
        let id = self.nodes.len() as u32;
        self.nodes.push(Node {
            pos,
            hash,
            next,
            tree,
            prev: NONE,
            parent: NONE,
            left: NONE,
            right: NONE,
            red: false,
        });
        id
    }

    /// Whether the chain starting at `node` holds `pos`.
    fn chain_contains(&self, mut node: u32, hash: i32, pos: BlockPos) -> bool {
        while node != NONE {
            let n = self.node(node);
            if n.hash == hash && n.pos == pos {
                return true;
            }
            node = n.next;
        }
        false
    }

    /// `contains(pos)`.
    pub fn contains(&self, pos: BlockPos) -> bool {
        if self.heads.is_empty() {
            return false;
        }
        let hash = spread_hash(pos);
        self.chain_contains(self.heads[self.bucket(hash)], hash, pos)
    }

    /// `add(pos)` (`HashMap.putVal`): whether the position was new.
    pub fn insert(&mut self, pos: BlockPos) -> bool {
        if self.heads.is_empty() {
            self.resize();
        }
        let hash = spread_hash(pos);
        let index = self.bucket(hash);
        let head = self.heads[index];
        if head == NONE {
            let id = self.push(pos, hash, NONE, false);
            self.heads[index] = id;
            self.first_bucket = self.first_bucket.min(index);
        } else if self.node(head).tree {
            if !self.put_tree_val(index, hash, pos) {
                return false;
            }
        } else {
            let mut node = head;
            let mut chain = 0;
            loop {
                let n = *self.node(node);
                if n.hash == hash && n.pos == pos {
                    return false;
                }
                if n.next == NONE {
                    break;
                }
                node = n.next;
                chain += 1;
            }
            let id = self.push(pos, hash, NONE, false);
            self.node_mut(node).next = id;
            if chain >= TREEIFY_CHAIN {
                self.treeify_bin(index);
            }
        }
        self.len += 1;
        if self.len > self.threshold {
            self.resize();
        }
        true
    }

    /// `iterator().next()` followed by `iterator.remove()`: removes and returns the first
    /// position in iteration order.
    pub fn pop_first(&mut self) -> Option<BlockPos> {
        if self.len == 0 {
            return None;
        }
        while self.heads[self.first_bucket] == NONE {
            self.first_bucket += 1;
        }
        let head = self.heads[self.first_bucket];
        let node = *self.node(head);
        if node.tree {
            self.remove_tree_node(head);
        } else {
            self.heads[self.first_bucket] = node.next;
        }
        self.len -= 1;
        Some(node.pos)
    }

    /// The positions in Java's iteration order.
    pub fn iter(&self) -> impl Iterator<Item = BlockPos> + '_ {
        self.heads.iter().flat_map(move |&head| {
            let mut node = head;
            std::iter::from_fn(move || {
                (node != NONE).then(|| {
                    let n = self.node(node);
                    node = n.next;
                    n.pos
                })
            })
        })
    }

    /// `new ObjectArrayList<>(set)` sorted stably by Y, as `TreeDecorator.Context` does.
    pub fn to_sorted_by_y(&self) -> Vec<BlockPos> {
        let mut list: Vec<BlockPos> = self.iter().collect();
        list.sort_by_key(|p| p.y);
        list
    }

    /// `HashMap.resize()`: doubles the table (16 initially); chains split into a low and a high
    /// half in order, tree bins through `TreeNode.split`.
    fn resize(&mut self) {
        let old_capacity = self.heads.len();
        let capacity = if old_capacity == 0 {
            INITIAL_CAPACITY
        } else {
            old_capacity * 2
        };
        self.threshold = capacity / 4 * 3;
        let old = std::mem::replace(&mut self.heads, vec![NONE; capacity]);
        self.first_bucket = 0;
        for (j, &head) in old.iter().enumerate() {
            if head == NONE {
                continue;
            }
            let first = *self.node(head);
            if first.next == NONE {
                let index = self.bucket(first.hash);
                self.heads[index] = head;
            } else if first.tree {
                self.split(head, j, old_capacity);
            } else {
                let (mut lo_head, mut lo_tail, mut hi_head, mut hi_tail) = (NONE, NONE, NONE, NONE);
                let mut node = head;
                while node != NONE {
                    let n = *self.node(node);
                    self.node_mut(node).next = NONE;
                    let (h, t) = if n.hash as u32 as usize & old_capacity == 0 {
                        (&mut lo_head, &mut lo_tail)
                    } else {
                        (&mut hi_head, &mut hi_tail)
                    };
                    if *t == NONE {
                        *h = node;
                    } else {
                        self.nodes[*t as usize].next = node;
                    }
                    *t = node;
                    node = n.next;
                }
                self.heads[j] = lo_head;
                self.heads[j + old_capacity] = hi_head;
            }
        }
    }

    /// `HashMap.treeifyBin`: grows small tables, otherwise turns the bin into a tree.
    fn treeify_bin(&mut self, index: usize) {
        if self.heads.len() < MIN_TREEIFY_CAPACITY {
            self.resize();
            return;
        }
        let mut node = self.heads[index];
        let mut tail = NONE;
        while node != NONE {
            let n = self.node_mut(node);
            n.tree = true;
            n.prev = tail;
            tail = node;
            node = n.next;
        }
        self.treeify(self.heads[index]);
    }

    /// `TreeNode.treeify`: builds a red-black tree from the chain at `head` (in chain order)
    /// and moves its root to the front of the bin.
    fn treeify(&mut self, head: u32) {
        let mut root = NONE;
        let mut x = head;
        while x != NONE {
            let next = self.node(x).next;
            let xn = self.node_mut(x);
            xn.left = NONE;
            xn.right = NONE;
            if root == NONE {
                xn.parent = NONE;
                xn.red = false;
                root = x;
            } else {
                let (hash, pos) = (xn.hash, xn.pos);
                let mut p = root;
                loop {
                    let pn = *self.node(p);
                    let dir = if pn.hash > hash {
                        -1
                    } else if pn.hash < hash {
                        1
                    } else {
                        tie_break(pos, pn.pos)
                    };
                    let child = if dir <= 0 { pn.left } else { pn.right };
                    if child == NONE {
                        self.node_mut(x).parent = p;
                        if dir <= 0 {
                            self.node_mut(p).left = x;
                        } else {
                            self.node_mut(p).right = x;
                        }
                        root = self.balance_insertion(root, x);
                        break;
                    }
                    p = child;
                }
            }
            x = next;
        }
        self.move_root_to_front(root);
    }

    /// `TreeNode.untreeify`: the chain at `head` as plain nodes, in order.
    fn untreeify(&mut self, head: u32) -> u32 {
        let mut node = head;
        while node != NONE {
            let n = self.node_mut(node);
            n.tree = false;
            node = n.next;
        }
        head
    }

    /// `TreeNode.split`: divides the tree bin at old index `index` into the low and high bins
    /// of the new table.
    fn split(&mut self, head: u32, index: usize, bit: usize) {
        let (mut lo_head, mut lo_tail, mut hi_head, mut hi_tail) = (NONE, NONE, NONE, NONE);
        let (mut lo_count, mut hi_count) = (0, 0);
        let mut node = head;
        while node != NONE {
            let next = self.node(node).next;
            let hash = self.node(node).hash;
            self.node_mut(node).next = NONE;
            let (h, t, count) = if hash as u32 as usize & bit == 0 {
                (&mut lo_head, &mut lo_tail, &mut lo_count)
            } else {
                (&mut hi_head, &mut hi_tail, &mut hi_count)
            };
            self.nodes[node as usize].prev = *t;
            if *t == NONE {
                *h = node;
            } else {
                self.nodes[*t as usize].next = node;
            }
            *t = node;
            *count += 1;
            node = next;
        }
        if lo_head != NONE {
            if lo_count <= UNTREEIFY_THRESHOLD {
                self.heads[index] = self.untreeify(lo_head);
            } else {
                self.heads[index] = lo_head;
                if hi_head != NONE {
                    self.treeify(lo_head);
                }
            }
        }
        if hi_head != NONE {
            if hi_count <= UNTREEIFY_THRESHOLD {
                self.heads[index + bit] = self.untreeify(hi_head);
            } else {
                self.heads[index + bit] = hi_head;
                if lo_head != NONE {
                    self.treeify(hi_head);
                }
            }
        }
    }

    /// `TreeNode.root()`.
    fn root_of(&self, mut node: u32) -> u32 {
        loop {
            let parent = self.node(node).parent;
            if parent == NONE {
                return node;
            }
            node = parent;
        }
    }

    /// `TreeNode.moveRootToFront`: makes `root` the first node of its bin.
    fn move_root_to_front(&mut self, root: u32) {
        if root == NONE {
            return;
        }
        let index = self.bucket(self.node(root).hash);
        let first = self.heads[index];
        if root == first {
            return;
        }
        self.heads[index] = root;
        let (prev, next) = (self.node(root).prev, self.node(root).next);
        if next != NONE {
            self.node_mut(next).prev = prev;
        }
        if prev != NONE {
            self.node_mut(prev).next = next;
        }
        if first != NONE {
            self.node_mut(first).prev = root;
        }
        let r = self.node_mut(root);
        r.next = first;
        r.prev = NONE;
    }

    /// `TreeNode.putTreeVal` for a position known to be new or not: whether it was inserted.
    fn put_tree_val(&mut self, index: usize, hash: i32, pos: BlockPos) -> bool {
        let head = self.heads[index];
        // Java finds an existing key by descending and searching the equal-hash subtree; the
        // outcome is whether the bin holds the key.
        if self.chain_contains(head, hash, pos) {
            return false;
        }
        let root = if self.node(head).parent != NONE {
            self.root_of(head)
        } else {
            head
        };
        let mut p = root;
        loop {
            let pn = *self.node(p);
            let dir = if pn.hash > hash {
                -1
            } else if pn.hash < hash {
                1
            } else {
                tie_break(pos, pn.pos)
            };
            let child = if dir <= 0 { pn.left } else { pn.right };
            if child == NONE {
                let xpn = pn.next;
                let x = self.push(pos, hash, xpn, true);
                if dir <= 0 {
                    self.node_mut(p).left = x;
                } else {
                    self.node_mut(p).right = x;
                }
                self.node_mut(p).next = x;
                let xn = self.node_mut(x);
                xn.parent = p;
                xn.prev = p;
                if xpn != NONE {
                    self.node_mut(xpn).prev = x;
                }
                let root = self.balance_insertion(root, x);
                self.move_root_to_front(root);
                return true;
            }
            p = child;
        }
    }

    /// `TreeNode.removeTreeNode(map, tab, movable = false)` for the first node `p` of its bin,
    /// as `HashIterator.remove` does: the bin stays a tree and its root is not moved.
    fn remove_tree_node(&mut self, p: u32) {
        let index = self.bucket(self.node(p).hash);
        let mut root = self.heads[index];
        let (succ, pred) = (self.node(p).next, self.node(p).prev);
        let first = if pred == NONE {
            self.heads[index] = succ;
            succ
        } else {
            self.node_mut(pred).next = succ;
            self.heads[index]
        };
        if succ != NONE {
            self.node_mut(succ).prev = pred;
        }
        if first == NONE {
            return;
        }
        if self.node(root).parent != NONE {
            root = self.root_of(root);
        }
        let (pl, pr) = (self.node(p).left, self.node(p).right);
        let replacement = if pl != NONE && pr != NONE {
            let mut s = pr;
            while self.node(s).left != NONE {
                s = self.node(s).left;
            }
            let color = self.node(s).red;
            self.node_mut(s).red = self.node(p).red;
            self.node_mut(p).red = color;
            let sr = self.node(s).right;
            let pp = self.node(p).parent;
            if s == pr {
                self.node_mut(p).parent = s;
                self.node_mut(s).right = p;
            } else {
                let sp = self.node(s).parent;
                self.node_mut(p).parent = sp;
                if sp != NONE {
                    if s == self.node(sp).left {
                        self.node_mut(sp).left = p;
                    } else {
                        self.node_mut(sp).right = p;
                    }
                }
                self.node_mut(s).right = pr;
                self.node_mut(pr).parent = s;
            }
            self.node_mut(p).left = NONE;
            self.node_mut(p).right = sr;
            if sr != NONE {
                self.node_mut(sr).parent = p;
            }
            self.node_mut(s).left = pl;
            self.node_mut(pl).parent = s;
            self.node_mut(s).parent = pp;
            if pp == NONE {
                root = s;
            } else if p == self.node(pp).left {
                self.node_mut(pp).left = s;
            } else {
                self.node_mut(pp).right = s;
            }
            if sr != NONE { sr } else { p }
        } else if pl != NONE {
            pl
        } else if pr != NONE {
            pr
        } else {
            p
        };
        if replacement != p {
            let pp = self.node(p).parent;
            self.node_mut(replacement).parent = pp;
            if pp == NONE {
                root = replacement;
                self.node_mut(replacement).red = false;
            } else if p == self.node(pp).left {
                self.node_mut(pp).left = replacement;
            } else {
                self.node_mut(pp).right = replacement;
            }
            let pn = self.node_mut(p);
            pn.left = NONE;
            pn.right = NONE;
            pn.parent = NONE;
        }
        if !self.node(p).red {
            self.balance_deletion(root, replacement);
        }
        if replacement == p {
            let pp = self.node(p).parent;
            self.node_mut(p).parent = NONE;
            if pp != NONE {
                if p == self.node(pp).left {
                    self.node_mut(pp).left = NONE;
                } else if p == self.node(pp).right {
                    self.node_mut(pp).right = NONE;
                }
            }
        }
    }

    /// `TreeNode.rotateLeft`.
    fn rotate_left(&mut self, mut root: u32, p: u32) -> u32 {
        if p == NONE {
            return root;
        }
        let r = self.node(p).right;
        if r == NONE {
            return root;
        }
        let rl = self.node(r).left;
        self.node_mut(p).right = rl;
        if rl != NONE {
            self.node_mut(rl).parent = p;
        }
        let pp = self.node(p).parent;
        self.node_mut(r).parent = pp;
        if pp == NONE {
            root = r;
            self.node_mut(r).red = false;
        } else if self.node(pp).left == p {
            self.node_mut(pp).left = r;
        } else {
            self.node_mut(pp).right = r;
        }
        self.node_mut(r).left = p;
        self.node_mut(p).parent = r;
        root
    }

    /// `TreeNode.rotateRight`.
    fn rotate_right(&mut self, mut root: u32, p: u32) -> u32 {
        if p == NONE {
            return root;
        }
        let l = self.node(p).left;
        if l == NONE {
            return root;
        }
        let lr = self.node(l).right;
        self.node_mut(p).left = lr;
        if lr != NONE {
            self.node_mut(lr).parent = p;
        }
        let pp = self.node(p).parent;
        self.node_mut(l).parent = pp;
        if pp == NONE {
            root = l;
            self.node_mut(l).red = false;
        } else if self.node(pp).right == p {
            self.node_mut(pp).right = l;
        } else {
            self.node_mut(pp).left = l;
        }
        self.node_mut(l).right = p;
        self.node_mut(p).parent = l;
        root
    }

    #[inline]
    fn is_red(&self, node: u32) -> bool {
        node != NONE && self.node(node).red
    }

    #[inline]
    fn parent(&self, node: u32) -> u32 {
        if node == NONE {
            NONE
        } else {
            self.node(node).parent
        }
    }

    /// `TreeNode.balanceInsertion`.
    fn balance_insertion(&mut self, mut root: u32, mut x: u32) -> u32 {
        self.node_mut(x).red = true;
        loop {
            let mut xp = self.node(x).parent;
            if xp == NONE {
                self.node_mut(x).red = false;
                return x;
            }
            if !self.node(xp).red {
                return root;
            }
            let mut xpp = self.node(xp).parent;
            if xpp == NONE {
                return root;
            }
            let xppl = self.node(xpp).left;
            if xp == xppl {
                let xppr = self.node(xpp).right;
                if self.is_red(xppr) {
                    self.node_mut(xppr).red = false;
                    self.node_mut(xp).red = false;
                    self.node_mut(xpp).red = true;
                    x = xpp;
                } else {
                    if x == self.node(xp).right {
                        x = xp;
                        root = self.rotate_left(root, x);
                        xp = self.node(x).parent;
                        xpp = self.parent(xp);
                    }
                    if xp != NONE {
                        self.node_mut(xp).red = false;
                        if xpp != NONE {
                            self.node_mut(xpp).red = true;
                            root = self.rotate_right(root, xpp);
                        }
                    }
                }
            } else if self.is_red(xppl) {
                self.node_mut(xppl).red = false;
                self.node_mut(xp).red = false;
                self.node_mut(xpp).red = true;
                x = xpp;
            } else {
                if x == self.node(xp).left {
                    x = xp;
                    root = self.rotate_right(root, x);
                    xp = self.node(x).parent;
                    xpp = self.parent(xp);
                }
                if xp != NONE {
                    self.node_mut(xp).red = false;
                    if xpp != NONE {
                        self.node_mut(xpp).red = true;
                        root = self.rotate_left(root, xpp);
                    }
                }
            }
        }
    }

    /// `TreeNode.balanceDeletion`.
    fn balance_deletion(&mut self, mut root: u32, mut x: u32) -> u32 {
        loop {
            if x == NONE || x == root {
                return root;
            }
            let mut xp = self.node(x).parent;
            if xp == NONE {
                self.node_mut(x).red = false;
                return x;
            }
            if self.node(x).red {
                self.node_mut(x).red = false;
                return root;
            }
            let xpl = self.node(xp).left;
            if xpl == x {
                let mut xpr = self.node(xp).right;
                if self.is_red(xpr) {
                    self.node_mut(xpr).red = false;
                    self.node_mut(xp).red = true;
                    root = self.rotate_left(root, xp);
                    xp = self.node(x).parent;
                    xpr = if xp == NONE {
                        NONE
                    } else {
                        self.node(xp).right
                    };
                }
                if xpr == NONE {
                    x = xp;
                } else {
                    let (sl, mut sr) = (self.node(xpr).left, self.node(xpr).right);
                    if !self.is_red(sr) && !self.is_red(sl) {
                        self.node_mut(xpr).red = true;
                        x = xp;
                    } else {
                        if !self.is_red(sr) {
                            if sl != NONE {
                                self.node_mut(sl).red = false;
                            }
                            self.node_mut(xpr).red = true;
                            root = self.rotate_right(root, xpr);
                            xp = self.node(x).parent;
                            xpr = if xp == NONE {
                                NONE
                            } else {
                                self.node(xp).right
                            };
                        }
                        if xpr != NONE {
                            self.node_mut(xpr).red = xp != NONE && self.node(xp).red;
                            sr = self.node(xpr).right;
                            if sr != NONE {
                                self.node_mut(sr).red = false;
                            }
                        }
                        if xp != NONE {
                            self.node_mut(xp).red = false;
                            root = self.rotate_left(root, xp);
                        }
                        x = root;
                    }
                }
            } else {
                let mut xpl = xpl;
                if self.is_red(xpl) {
                    self.node_mut(xpl).red = false;
                    self.node_mut(xp).red = true;
                    root = self.rotate_right(root, xp);
                    xp = self.node(x).parent;
                    xpl = if xp == NONE { NONE } else { self.node(xp).left };
                }
                if xpl == NONE {
                    x = xp;
                } else {
                    let (mut sl, sr) = (self.node(xpl).left, self.node(xpl).right);
                    if !self.is_red(sl) && !self.is_red(sr) {
                        self.node_mut(xpl).red = true;
                        x = xp;
                    } else {
                        if !self.is_red(sl) {
                            if sr != NONE {
                                self.node_mut(sr).red = false;
                            }
                            self.node_mut(xpl).red = true;
                            root = self.rotate_left(root, xpl);
                            xp = self.node(x).parent;
                            xpl = if xp == NONE { NONE } else { self.node(xp).left };
                        }
                        if xpl != NONE {
                            self.node_mut(xpl).red = xp != NONE && self.node(xp).red;
                            sl = self.node(xpl).left;
                            if sl != NONE {
                                self.node_mut(sl).red = false;
                            }
                        }
                        if xp != NONE {
                            self.node_mut(xp).red = false;
                            root = self.rotate_right(root, xp);
                        }
                        x = root;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iterates_in_bucket_order() {
        let mut set = JavaHashSet::new();
        // Hash codes 0..=19; once the table has 32 buckets Java iterates them in order.
        for x in (0..20).rev() {
            set.insert(BlockPos::new(x, 0, 0));
        }
        let order: Vec<i32> = set.iter().map(|p| p.x).collect();
        assert_eq!(order, (0..20).collect::<Vec<_>>());
        assert!(!set.insert(BlockPos::new(3, 0, 0)));
        assert_eq!(set.len(), 20);
    }

    #[test]
    fn chains_keep_insertion_order_and_pop_first() {
        let mut set = JavaHashSet::new();
        // Same bucket in a 16 table: hashes differ by 16.
        set.insert(BlockPos::new(16, 0, 0));
        set.insert(BlockPos::new(0, 0, 0));
        set.insert(BlockPos::new(1, 0, 0));
        let order: Vec<i32> = set.iter().map(|p| p.x).collect();
        assert_eq!(order, vec![16, 0, 1]);
        assert_eq!(set.pop_first(), Some(BlockPos::new(16, 0, 0)));
        assert_eq!(set.pop_first(), Some(BlockPos::new(0, 0, 0)));
        assert!(set.contains(BlockPos::new(1, 0, 0)));
        assert!(!set.contains(BlockPos::new(0, 0, 0)));
        assert_eq!(set.pop_first(), Some(BlockPos::new(1, 0, 0)));
        assert_eq!(set.pop_first(), None);
    }

    #[test]
    fn long_chain_in_small_table_resizes() {
        let mut set = JavaHashSet::new();
        // Nine positions whose hashes are equal modulo 16 but differ modulo 64.
        for i in 0..9 {
            set.insert(BlockPos::new(i * 16, 0, 0));
        }
        // The ninth insertion resized 16 -> 32 (treeifyBin), not a threshold resize.
        assert_eq!(set.heads.len(), 32);
        let order: Vec<i32> = set.iter().map(|p| p.x).collect();
        assert_eq!(order, vec![0, 32, 64, 96, 128, 16, 48, 80, 112]);
    }

    /// Recorded with Java 25: `HashSet` of positions `(x, 0, 0)` (hash = x) with tree bins,
    /// iterator removal and re-insertion.
    #[test]
    fn matches_java_with_tree_bins() {
        let mut set = JavaHashSet::new();
        for k in 0..40 {
            set.insert(BlockPos::new(128 * k + k % 2, 0, 0));
        }
        let popped: Vec<i32> = (0..10).map(|_| set.pop_first().unwrap().x).collect();
        assert_eq!(
            popped,
            [1792, 768, 0, 256, 512, 1024, 1280, 1536, 2048, 2304]
        );
        for k in 0..20 {
            set.insert(BlockPos::new(128 * k + 2, 0, 0));
        }
        for k in 0..30 {
            set.insert(BlockPos::new(7 * k + 5, 0, 0));
        }
        let order: Vec<i32> = set.iter().map(|p| p.x).collect();
        assert_eq!(
            order,
            [
                2560, 2816, 3072, 3328, 3584, 3840, 4096, 4352, 4608, 4864, 1921, 897, 129, 385,
                641, 1153, 1409, 1665, 2177, 2433, 2689, 2945, 3201, 3457, 3713, 3969, 4225, 4481,
                4737, 4993, 898, 386, 2, 130, 258, 514, 642, 770, 1026, 1154, 1282, 1410, 1538,
                1666, 1794, 1922, 2050, 2178, 2306, 2434, 131, 5, 138, 12, 145, 19, 152, 26, 159,
                33, 166, 40, 173, 47, 180, 54, 187, 61, 194, 68, 201, 75, 208, 82, 89, 96, 103,
                110, 117, 124,
            ]
        );
    }
}

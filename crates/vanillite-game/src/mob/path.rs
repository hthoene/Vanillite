//! Path finding like Vanilla's `PathFinder` with the `WalkNodeEvaluator`: A* over block
//! nodes with path type costs, jumping up one block, falling down to the ground and diagonal
//! moves.

use std::sync::OnceLock;

use vanillite_base::hash::FxHashMap;
use vanillite_base::math::floor;
use vanillite_base::{Aabb, BlockPos, Vec3};
use vanillite_data::block::{BlockState, STATE_COUNT, flags, ids};
use vanillite_worldgen::behaviour::{Class, class_of_block};

use super::is_water;
use super::physics::{Blocks, no_collision};

/// `PathType` with its default malus.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum PathType {
    Blocked,
    Open,
    Walkable,
    WalkableDoor,
    Trapdoor,
    PowderSnow,
    OnTopOfPowderSnow,
    Fence,
    Lava,
    Water,
    WaterBorder,
    Rail,
    UnpassableRail,
    FireInNeighbor,
    Fire,
    DamagingInNeighbor,
    Damaging,
    DoorOpen,
    DoorWoodClosed,
    DoorIronClosed,
    Breach,
    Leaves,
    StickyHoney,
    Cocoa,
    DamageCautious,
    OnTopOfTrapdoor,
    BigMobsCloseToDanger,
}

impl PathType {
    /// `getMalus()`: the default cost.
    pub fn malus(self) -> f32 {
        use PathType::*;
        match self {
            Blocked | PowderSnow | Fence | Lava | UnpassableRail | Damaging | DoorWoodClosed
            | DoorIronClosed | Leaves => -1.0,
            Water | WaterBorder | FireInNeighbor | DamagingInNeighbor | StickyHoney => 8.0,
            Fire => 16.0,
            Breach | BigMobsCloseToDanger => 4.0,
            _ => 0.0,
        }
    }

    fn from_u8(v: u8) -> PathType {
        // Only values written by `state_type` are stored.
        use PathType::*;
        const ALL: [PathType; 27] = [
            Blocked,
            Open,
            Walkable,
            WalkableDoor,
            Trapdoor,
            PowderSnow,
            OnTopOfPowderSnow,
            Fence,
            Lava,
            Water,
            WaterBorder,
            Rail,
            UnpassableRail,
            FireInNeighbor,
            Fire,
            DamagingInNeighbor,
            Damaging,
            DoorOpen,
            DoorWoodClosed,
            DoorIronClosed,
            Breach,
            Leaves,
            StickyHoney,
            Cocoa,
            DamageCautious,
            OnTopOfTrapdoor,
            BigMobsCloseToDanger,
        ];
        ALL.get(usize::from(v)).copied().unwrap_or(Blocked)
    }
}

/// `BlockBehaviour.isPathfindable(state, LAND)`.
pub fn is_pathfindable_land(state: BlockState) -> bool {
    let block = state.block();
    match class_of_block(block) {
        Class::AbstractBed
        | Class::BambooStalk
        | Class::Skull
        | Class::WallSkull
        | Class::Anvil
        | Class::CandleCake
        | Class::Cake
        | Class::Azalea
        | Class::Cocoa
        | Class::Campfire
        | Class::EndPortalFrame
        | Class::Bell
        | Class::Cactus
        | Class::EnderChest
        | Class::Farmland
        | Class::FlowerPot
        | Class::Grindstone
        | Class::ChorusPlant
        | Class::Chest
        | Class::CopperChest
        | Class::Lantern
        | Class::Fence
        | Class::IronBars
        | Class::Chain
        | Class::Conduit
        | Class::Hopper
        | Class::DecoratedPot
        | Class::Path
        | Class::HeavyCore
        | Class::Rod
        | Class::LightningRod
        | Class::SculkSensor
        | Class::CalibratedSculkSensor
        | Class::SeaPickle
        | Class::Stonecutter
        | Class::Lectern
        | Class::Wall
        | Class::ShelfMushroom
        | Class::WallHangingSign
        | Class::Stair
        | Class::Speleothem
        | Class::Slab
        | Class::CopperGolemStatue
        | Class::Shelf => false,
        Class::Door | Class::FenceGate | Class::TrapDoor => state.value("open") == Some("true"),
        Class::Liquid => !block.name().ends_with("lava"),
        Class::SnowLayer => state
            .value("layers")
            .and_then(|l| l.parse::<u8>().ok())
            .is_some_and(|l| l < 5),
        _ => {
            let name = block.name();
            if matches!(
                name,
                "minecraft:brewing_stand"
                    | "minecraft:enchanting_table"
                    | "minecraft:cauldron"
                    | "minecraft:water_cauldron"
                    | "minecraft:lava_cauldron"
                    | "minecraft:powder_snow_cauldron"
                    | "minecraft:composter"
                    | "minecraft:dragon_egg"
                    | "minecraft:mud"
                    | "minecraft:soul_sand"
                    | "minecraft:respawn_anchor"
            ) {
                return false;
            }
            if block == ids::POWDER_SNOW {
                return true;
            }
            !state.has(flags::COLLISION_FULL_BLOCK)
        }
    }
}

/// `WalkNodeEvaluator.getPathTypeFromState`, uncached.
fn compute_state_type(state: BlockState) -> PathType {
    if state.is_air() {
        return PathType::Open;
    }
    let block = state.block();
    let class = class_of_block(block);
    let name = block.name();
    if class == Class::TrapDoor || class == Class::LilyPad || class == Class::BigDripleaf {
        return PathType::Trapdoor;
    }
    if block == ids::POWDER_SNOW {
        return PathType::PowderSnow;
    }
    if block == ids::CACTUS || block == ids::SWEET_BERRY_BUSH {
        return PathType::Damaging;
    }
    if block == ids::HONEY_BLOCK {
        return PathType::StickyHoney;
    }
    if block == ids::COCOA {
        return PathType::Cocoa;
    }
    if block == ids::WITHER_ROSE || class == Class::Speleothem {
        return PathType::DamageCautious;
    }
    if super::is_lava(state) {
        return PathType::Lava;
    }
    let burning = block == ids::FIRE
        || block == ids::SOUL_FIRE
        || block == ids::MAGMA_BLOCK
        || block == ids::LAVA_CAULDRON
        || (class == Class::Campfire && state.value("lit") == Some("true"));
    if burning {
        return PathType::Fire;
    }
    if class == Class::Door {
        if state.value("open") == Some("true") {
            return PathType::DoorOpen;
        }
        return if name == "minecraft:iron_door" {
            PathType::DoorIronClosed
        } else {
            PathType::DoorWoodClosed
        };
    }
    if matches!(
        class,
        Class::Rail | Class::PoweredRail | Class::DetectorRail
    ) {
        return PathType::Rail;
    }
    if class == Class::Leaves {
        return PathType::Leaves;
    }
    if class == Class::Fence
        || class == Class::Wall
        || (class == Class::FenceGate && state.value("open") != Some("true"))
    {
        return PathType::Fence;
    }
    if !is_pathfindable_land(state) {
        return PathType::Blocked;
    }
    if super::is_water(state) {
        PathType::Water
    } else {
        PathType::Open
    }
}

/// The path type of every block state, computed once.
fn state_table() -> &'static [u8] {
    static TABLE: OnceLock<Vec<u8>> = OnceLock::new();
    TABLE.get_or_init(|| {
        (0..STATE_COUNT)
            .map(|s| {
                let state = BlockState(u16::try_from(s).unwrap_or(0));
                compute_state_type(state) as u8
            })
            .collect()
    })
}

/// `getPathTypeFromState` (Vanilla caches it in `PathTypeCache`; this table holds every state).
#[inline]
pub fn state_type(state: BlockState) -> PathType {
    state_table()
        .get(usize::from(state.0))
        .map_or(PathType::Blocked, |&v| PathType::from_u8(v))
}

/// `WalkNodeEvaluator.getPathTypeStatic`.
pub fn path_type_static(blocks: &dyn Blocks, min_y: i32, x: i32, y: i32, z: i32) -> PathType {
    let here = state_type(blocks.state(BlockPos::new(x, y, z)));
    if here == PathType::Open && y > min_y {
        match state_type(blocks.state(BlockPos::new(x, y - 1, z))) {
            PathType::Open | PathType::Water | PathType::Lava | PathType::Walkable => {
                PathType::Open
            }
            PathType::Fire => PathType::Fire,
            PathType::Damaging => PathType::Damaging,
            PathType::StickyHoney => PathType::StickyHoney,
            PathType::PowderSnow => PathType::OnTopOfPowderSnow,
            PathType::DamageCautious => PathType::DamageCautious,
            PathType::Trapdoor => PathType::OnTopOfTrapdoor,
            _ => check_neighbour_blocks(blocks, x, y, z, PathType::Walkable),
        }
    } else {
        here
    }
}

/// `WalkNodeEvaluator.checkNeighbourBlocks`.
fn check_neighbour_blocks(
    blocks: &dyn Blocks,
    x: i32,
    y: i32,
    z: i32,
    fallback: PathType,
) -> PathType {
    for dx in -1..=1 {
        for dy in -1..=1 {
            for dz in -1..=1 {
                if dx == 0 && dz == 0 {
                    continue;
                }
                match state_type(blocks.state(BlockPos::new(x + dx, y + dy, z + dz))) {
                    PathType::Damaging => return PathType::DamagingInNeighbor,
                    PathType::Fire | PathType::Lava => return PathType::FireInNeighbor,
                    PathType::Water => return PathType::WaterBorder,
                    PathType::DamageCautious => return PathType::DamageCautious,
                    _ => {}
                }
            }
        }
    }
    fallback
}

/// `WalkNodeEvaluator.getFloorLevel(level, pos)`.
pub fn floor_level(blocks: &dyn Blocks, pos: BlockPos) -> f64 {
    let below = BlockPos::new(pos.x, pos.y - 1, pos.z);
    let shape = blocks.state(below).collision_shape();
    let top = shape
        .iter()
        .map(|b| b[4])
        .fold(None, |m: Option<f64>, v| Some(m.map_or(v, |m| m.max(v))));
    f64::from(below.y) + top.unwrap_or(0.0)
}

/// `SwimNodeEvaluator.getPathTypeOfMob`.
fn swim_type_of_mob(eval: &Eval<'_>, x: i32, y: i32, z: i32) -> PathType {
    let blocks = eval.blocks;
    let mut last = BlockPos::new(x, y, z);
    for xx in x..x + eval.entity_width {
        for yy in y..y + eval.entity_height {
            for zz in z..z + eval.entity_depth {
                let pos = BlockPos::new(xx, yy, zz);
                last = pos;
                let state = blocks.state(pos);
                let fluid = state.fluid_state();
                let below = blocks.state(BlockPos::new(xx, yy - 1, zz));
                if fluid.is_empty() && is_pathfindable_water(below) && state.is_air() {
                    return PathType::Breach;
                }
                if !is_water(state) {
                    return PathType::Blocked;
                }
            }
        }
    }
    if is_pathfindable_water(blocks.state(last)) {
        PathType::Water
    } else {
        PathType::Blocked
    }
}

/// `BlockState.isPathfindable(PathComputationType.WATER)`.
pub fn is_pathfindable_water(state: BlockState) -> bool {
    match class_of_block(state.block()) {
        Class::AbstractBed
        | Class::BambooStalk
        | Class::Skull
        | Class::WallSkull
        | Class::Anvil
        | Class::CandleCake
        | Class::Cake
        | Class::Azalea
        | Class::Cocoa
        | Class::Campfire
        | Class::EndPortalFrame
        | Class::Bell
        | Class::Cactus
        | Class::EnderChest
        | Class::Farmland
        | Class::FlowerPot
        | Class::Grindstone
        | Class::ChorusPlant
        | Class::Chest
        | Class::CopperChest
        | Class::Lantern
        | Class::Fence
        | Class::IronBars
        | Class::Chain
        | Class::Conduit
        | Class::Hopper
        | Class::DecoratedPot
        | Class::Path
        | Class::HeavyCore
        | Class::Rod
        | Class::LightningRod
        | Class::SculkSensor
        | Class::CalibratedSculkSensor
        | Class::SeaPickle
        | Class::Stonecutter
        | Class::Lectern
        | Class::Wall
        | Class::ShelfMushroom
        | Class::WallHangingSign
        | Class::Stair
        | Class::Speleothem
        | Class::Door
        | Class::FenceGate
        | Class::SnowLayer => false,
        Class::TrapDoor => state.value("waterlogged") == Some("true"),
        Class::Liquid => !state.block().name().ends_with("lava"),
        _ => {
            if matches!(
                state.block().name(),
                "minecraft:brewing_stand"
                    | "minecraft:enchanting_table"
                    | "minecraft:cauldron"
                    | "minecraft:water_cauldron"
                    | "minecraft:lava_cauldron"
                    | "minecraft:powder_snow_cauldron"
                    | "minecraft:composter"
                    | "minecraft:dragon_egg"
                    | "minecraft:mud"
                    | "minecraft:soul_sand"
                    | "minecraft:respawn_anchor"
            ) {
                return false;
            }
            if state.block() == ids::POWDER_SNOW {
                return true;
            }
            is_water(state)
        }
    }
}

/// What the path finder needs to know about the mob.
#[derive(Clone, Copy, Debug)]
pub struct PathMob {
    pub pos: Vec3,
    pub aabb: Aabb,
    pub width: f32,
    pub height: f32,
    pub on_ground: bool,
    pub max_up_step: f32,
    pub max_fall_distance: i32,
    /// `isInFloatableFluid` (water).
    pub in_water: bool,
    pub can_float: bool,
    pub can_open_doors: bool,
    pub can_pass_doors: bool,
}

/// The malus overrides a mob kind sets: `(type, cost)`.
pub type MalusTable = [(PathType, f32)];

impl PathMob {
    fn block_pos(&self) -> BlockPos {
        BlockPos::containing(self.pos.x, self.pos.y, self.pos.z)
    }
}

/// `Mob.getPathfindingMalus`.
pub fn malus_of(table: &MalusTable, t: PathType) -> f32 {
    table
        .iter()
        .find(|(k, _)| *k == t)
        .map_or_else(|| t.malus(), |&(_, v)| v)
}

/// A node of a computed path.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PathNode {
    pub x: i32,
    pub y: i32,
    pub z: i32,
    pub kind: PathType,
}

impl PathNode {
    /// The node's block position.
    pub fn pos(&self) -> BlockPos {
        BlockPos::new(self.x, self.y, self.z)
    }
}

/// `Path`: the nodes, the next node to walk to and the target.
#[derive(Clone, Debug, PartialEq)]
pub struct Path {
    pub nodes: Vec<PathNode>,
    pub next: usize,
    pub target: BlockPos,
    pub dist_to_target: f32,
    pub reached: bool,
}

impl Path {
    /// `isDone`.
    pub fn is_done(&self) -> bool {
        self.next >= self.nodes.len()
    }

    /// `getEntityPosAtNode`.
    pub fn entity_pos_at(&self, width: f32, index: usize) -> Vec3 {
        let Some(n) = self.nodes.get(index) else {
            return Vec3::ZERO;
        };
        // `(int)(width + 1.0F) * 0.5`.
        #[allow(clippy::cast_possible_truncation)]
        let half = f64::from((width + 1.0) as i32) * 0.5;
        Vec3::new(f64::from(n.x) + half, f64::from(n.y), f64::from(n.z) + half)
    }

    /// `getNextEntityPos`.
    pub fn next_entity_pos(&self, width: f32) -> Vec3 {
        self.entity_pos_at(width, self.next)
    }

    /// `getEndNode`.
    pub fn end(&self) -> Option<&PathNode> {
        self.nodes.last()
    }

    /// `sameAs`: the same nodes.
    pub fn same_nodes(&self, other: &Path) -> bool {
        self.nodes == other.nodes
    }
}

#[derive(Clone, Copy, Debug)]
struct Node {
    x: i32,
    y: i32,
    z: i32,
    heap_idx: i32,
    g: f32,
    h: f32,
    f: f32,
    came_from: u32,
    closed: bool,
    walked: f32,
    cost_malus: f32,
    kind: PathType,
}

const NONE: u32 = u32::MAX;

impl Node {
    fn new(x: i32, y: i32, z: i32) -> Node {
        Node {
            x,
            y,
            z,
            heap_idx: -1,
            g: 0.0,
            h: 0.0,
            f: 0.0,
            came_from: NONE,
            closed: false,
            walked: 0.0,
            cost_malus: 0.0,
            kind: PathType::Blocked,
        }
    }

    fn distance_to(&self, x: i32, y: i32, z: i32) -> f32 {
        // `float xd = to.x - this.x`: int to float conversions.
        #[allow(clippy::cast_precision_loss)]
        let (dx, dy, dz) = (
            (x - self.x) as f32,
            (y - self.y) as f32,
            (z - self.z) as f32,
        );
        (dx * dx + dy * dy + dz * dz).sqrt()
    }

    fn manhattan(&self, x: i32, y: i32, z: i32) -> f32 {
        #[allow(clippy::cast_precision_loss)]
        let d = ((x - self.x).abs() + (y - self.y).abs() + (z - self.z).abs()) as f32;
        d
    }
}

fn key(x: i32, y: i32, z: i32) -> i64 {
    BlockPos::new(x, y, z).as_long()
}

/// The reusable state of a path search (`PathFinder` + `WalkNodeEvaluator`). One per world;
/// a search leaves nothing behind but its result.
#[derive(Debug, Default)]
pub struct PathFinder {
    nodes: Vec<Node>,
    index: FxHashMap<i64, u32>,
    heap: Vec<u32>,
    types: FxHashMap<i64, PathType>,
    collisions: FxHashMap<[u64; 6], bool>,
    boxes: Vec<Aabb>,
    /// Nodes visited by searches since the last [`Self::take_visited`] (for budgets).
    visited: u64,
}

/// One target of a search (`Target`).
#[derive(Clone, Copy, Debug)]
struct Target {
    x: i32,
    y: i32,
    z: i32,
    pos: BlockPos,
    best_h: f32,
    best: u32,
    reached: bool,
}

/// The parameters of one search besides the mob.
#[derive(Clone, Copy, Debug)]
pub struct Search<'a> {
    pub max_path_length: f32,
    pub reach_range: i32,
    pub max_visited_nodes: i32,
    pub malus: &'a MalusTable,
    pub min_y: i32,
    /// `SwimNodeEvaluator` (water-bound navigation) instead of the `WalkNodeEvaluator`.
    pub swim: bool,
}

struct Eval<'a> {
    blocks: &'a dyn Blocks,
    mob: &'a PathMob,
    search: Search<'a>,
    entity_width: i32,
    entity_height: i32,
    entity_depth: i32,
}

impl PathFinder {
    /// A search state.
    pub fn new() -> PathFinder {
        PathFinder::default()
    }

    /// Nodes visited since the last call.
    pub fn take_visited(&mut self) -> u64 {
        std::mem::take(&mut self.visited)
    }

    fn node(&mut self, x: i32, y: i32, z: i32) -> u32 {
        let k = key(x, y, z);
        if let Some(&i) = self.index.get(&k) {
            return i;
        }
        let i = u32::try_from(self.nodes.len()).unwrap_or(NONE);
        self.nodes.push(Node::new(x, y, z));
        self.index.insert(k, i);
        i
    }

    fn n(&self, i: u32) -> &Node {
        &self.nodes[i as usize]
    }

    fn nm(&mut self, i: u32) -> &mut Node {
        &mut self.nodes[i as usize]
    }

    // --- Binary heap (`BinaryHeap`) -------------------------------------------------------

    fn heap_insert(&mut self, i: u32) {
        let idx = self.heap.len();
        self.heap.push(i);
        self.nm(i).heap_idx = i32::try_from(idx).unwrap_or(-1);
        self.up_heap(idx);
    }

    fn heap_pop(&mut self) -> u32 {
        let popped = self.heap[0];
        let last = self.heap.pop().unwrap_or(popped);
        if !self.heap.is_empty() {
            self.heap[0] = last;
            self.nm(last).heap_idx = 0;
            self.down_heap(0);
        }
        self.nm(popped).heap_idx = -1;
        popped
    }

    fn change_cost(&mut self, i: u32, cost: f32) {
        let old = self.n(i).f;
        self.nm(i).f = cost;
        let idx = self.n(i).heap_idx as usize;
        if cost < old {
            self.up_heap(idx);
        } else {
            self.down_heap(idx);
        }
    }

    #[allow(clippy::neg_cmp_op_on_partial_ord)]
    fn up_heap(&mut self, mut idx: usize) {
        let node = self.heap[idx];
        let cost = self.n(node).f;
        while idx > 0 {
            let parent_idx = (idx - 1) >> 1;
            let parent = self.heap[parent_idx];
            if !(cost < self.n(parent).f) {
                break;
            }
            self.heap[idx] = parent;
            self.nm(parent).heap_idx = idx as i32;
            idx = parent_idx;
        }
        self.heap[idx] = node;
        self.nm(node).heap_idx = idx as i32;
    }

    #[allow(clippy::neg_cmp_op_on_partial_ord)]
    fn down_heap(&mut self, mut idx: usize) {
        let node = self.heap[idx];
        let cost = self.n(node).f;
        let size = self.heap.len();
        loop {
            let left = 1 + (idx << 1);
            let right = left + 1;
            if left >= size {
                break;
            }
            let left_node = self.heap[left];
            let left_cost = self.n(left_node).f;
            let (right_node, right_cost) = if right >= size {
                (NONE, f32::INFINITY)
            } else {
                let r = self.heap[right];
                (r, self.n(r).f)
            };
            if left_cost < right_cost {
                if !(left_cost < cost) {
                    break;
                }
                self.heap[idx] = left_node;
                self.nm(left_node).heap_idx = idx as i32;
                idx = left;
            } else {
                if !(right_cost < cost) {
                    break;
                }
                self.heap[idx] = right_node;
                self.nm(right_node).heap_idx = idx as i32;
                idx = right;
            }
        }
        self.heap[idx] = node;
        self.nm(node).heap_idx = idx as i32;
    }

    // --- Search (`PathFinder.findPath`) ---------------------------------------------------

    /// `PathFinder.findPath` to the nearest of `targets`: `None` without a start node.
    pub fn find_path(
        &mut self,
        blocks: &dyn Blocks,
        mob: &PathMob,
        targets: &[BlockPos],
        search: Search<'_>,
    ) -> Option<Path> {
        self.nodes.clear();
        self.index.clear();
        self.heap.clear();
        self.types.clear();
        self.collisions.clear();
        let w = floor(f64::from(mob.width) + 1.0);
        let h = floor(f64::from(mob.height) + 1.0);
        let eval = Eval {
            blocks,
            mob,
            search,
            entity_width: w,
            entity_height: h,
            entity_depth: w,
        };
        let from = if search.swim {
            self.swim_start(&eval)
        } else {
            self.start(&eval)?
        };
        let mut tgts: Vec<Target> = targets
            .iter()
            .map(|&pos| Target {
                x: pos.x,
                y: pos.y,
                z: pos.z,
                pos,
                best_h: f32::MAX,
                best: NONE,
                reached: false,
            })
            .collect();
        let path = self.search(&eval, from, &mut tgts);
        Some(path)
    }

    fn best_h(&self, node: u32, targets: &mut [Target]) -> f32 {
        let mut best = f32::MAX;
        for t in targets.iter_mut() {
            let h = self.n(node).distance_to(t.x, t.y, t.z);
            if h < t.best_h {
                t.best_h = h;
                t.best = node;
            }
            best = best.min(h);
        }
        best
    }

    fn search(&mut self, eval: &Eval<'_>, from: u32, targets: &mut [Target]) -> Path {
        let max_len = eval.search.max_path_length;
        let h = self.best_h(from, targets);
        {
            let f = self.nm(from);
            f.g = 0.0;
            f.h = h;
            f.f = h;
        }
        self.heap.clear();
        self.heap_insert(from);
        let mut count = 0;
        let mut any_reached = false;
        let mut neighbors = [NONE; 32];
        let (fx, fy, fz) = {
            let f = self.n(from);
            (f.x, f.y, f.z)
        };
        while !self.heap.is_empty() {
            count += 1;
            if count >= eval.search.max_visited_nodes {
                break;
            }
            let current = self.heap_pop();
            self.nm(current).closed = true;
            self.visited += 1;
            let range = eval.search.reach_range;
            #[allow(clippy::cast_precision_loss)]
            let range = range as f32;
            for t in targets.iter_mut() {
                if self.n(current).manhattan(t.x, t.y, t.z) <= range {
                    t.reached = true;
                    any_reached = true;
                }
            }
            if any_reached {
                break;
            }
            if self.n(current).distance_to(fx, fy, fz) >= max_len {
                continue;
            }
            let count_n = if eval.search.swim {
                self.swim_neighbors(eval, &mut neighbors, current)
            } else {
                self.neighbors(eval, &mut neighbors, current)
            };
            for &neighbor in neighbors.iter().take(count_n) {
                let (cx, cy, cz) = {
                    let c = self.n(current);
                    (c.x, c.y, c.z)
                };
                let distance = self.n(neighbor).distance_to(cx, cy, cz);
                let walked = self.n(current).walked + distance;
                self.nm(neighbor).walked = walked;
                let tentative = self.n(current).g + distance + self.n(neighbor).cost_malus;
                let in_open = self.n(neighbor).heap_idx >= 0;
                if walked < max_len && (!in_open || tentative < self.n(neighbor).g) {
                    let h = self.best_h(neighbor, targets) * 1.5;
                    {
                        let n = self.nm(neighbor);
                        n.came_from = current;
                        n.g = tentative;
                        n.h = h;
                    }
                    if in_open {
                        let g = self.n(neighbor).g;
                        self.change_cost(neighbor, g + h);
                    } else {
                        let n = self.nm(neighbor);
                        n.f = n.g + n.h;
                        self.heap_insert(neighbor);
                    }
                }
            }
        }
        let mut best: Option<Path> = None;
        for t in targets.iter() {
            if any_reached && !t.reached {
                continue;
            }
            let path = self.reconstruct(t.best, t.pos, any_reached);
            let better = match &best {
                None => true,
                Some(b) if any_reached => path.nodes.len() < b.nodes.len(),
                Some(b) => {
                    path.dist_to_target < b.dist_to_target
                        || (path.dist_to_target == b.dist_to_target
                            && path.nodes.len() < b.nodes.len())
                }
            };
            if better {
                best = Some(path);
            }
        }
        best.unwrap_or(Path {
            nodes: Vec::new(),
            next: 0,
            target: BlockPos::ZERO,
            dist_to_target: f32::MAX,
            reached: false,
        })
    }

    fn reconstruct(&self, closest: u32, target: BlockPos, reached: bool) -> Path {
        let mut nodes = Vec::new();
        let mut i = closest;
        while i != NONE {
            let n = self.n(i);
            nodes.push(PathNode {
                x: n.x,
                y: n.y,
                z: n.z,
                kind: n.kind,
            });
            i = n.came_from;
        }
        nodes.reverse();
        let dist = nodes.last().map_or(f32::MAX, |n| {
            #[allow(clippy::cast_precision_loss)]
            let d =
                ((target.x - n.x).abs() + (target.y - n.y).abs() + (target.z - n.z).abs()) as f32;
            d
        });
        Path {
            nodes,
            next: 0,
            target,
            dist_to_target: dist,
            reached,
        }
    }

    // --- `WalkNodeEvaluator` ----------------------------------------------------------------

    fn malus(eval: &Eval<'_>, t: PathType) -> f32 {
        malus_of(eval.search.malus, t)
    }

    fn start(&mut self, eval: &Eval<'_>) -> Option<u32> {
        let mob = eval.mob;
        let blocks = eval.blocks;
        let mut start_y = floor(mob.pos.y);
        let at = |y: i32| BlockPos::containing(mob.pos.x, f64::from(y), mob.pos.z);
        if eval.mob.can_float && mob.in_water {
            while is_water(blocks.state(at(start_y))) {
                start_y += 1;
            }
            start_y -= 1;
        } else if mob.on_ground {
            start_y = floor(mob.pos.y + 0.5);
        } else {
            let mut y = floor(mob.pos.y + 1.0);
            while y > eval.search.min_y {
                start_y = y;
                y -= 1;
                let below = blocks.state(at(y));
                if !below.is_air() && !is_pathfindable_land(below) {
                    break;
                }
            }
        }
        let start = mob.block_pos();
        if !self.can_start_at(eval, start.x, start_y, start.z) {
            let b = mob.aabb;
            for (x, z) in [
                (b.min_x, b.min_z),
                (b.min_x, b.max_z),
                (b.max_x, b.min_z),
                (b.max_x, b.max_z),
            ] {
                let p = BlockPos::containing(x, f64::from(start_y), z);
                if self.can_start_at(eval, p.x, p.y, p.z) {
                    return Some(self.start_node(eval, p.x, p.y, p.z));
                }
            }
        }
        Some(self.start_node(eval, start.x, start_y, start.z))
    }

    fn start_node(&mut self, eval: &Eval<'_>, x: i32, y: i32, z: i32) -> u32 {
        let t = self.cached_type(eval, x, y, z);
        let i = self.node(x, y, z);
        let n = self.nm(i);
        n.kind = t;
        n.cost_malus = Self::malus(eval, t);
        i
    }

    fn can_start_at(&mut self, eval: &Eval<'_>, x: i32, y: i32, z: i32) -> bool {
        let t = self.cached_type(eval, x, y, z);
        t != PathType::Open && Self::malus(eval, t) >= 0.0
    }

    fn neighbors(&mut self, eval: &Eval<'_>, out: &mut [u32; 32], pos: u32) -> usize {
        let (px, py, pz) = {
            let n = self.n(pos);
            (n.x, n.y, n.z)
        };
        let mut p = 0;
        let mut jump_size = 0;
        let above = self.cached_type(eval, px, py + 1, pz);
        let current = self.cached_type(eval, px, py, pz);
        if Self::malus(eval, above) >= 0.0 && current != PathType::StickyHoney {
            jump_size = floor(f64::from(eval.mob.max_up_step.max(1.0)));
        }
        let height = self.floor_level(eval, BlockPos::new(px, py, pz));
        // `Direction.Plane.HORIZONTAL`: north, east, south, west; 2D data values are
        // south 0, west 1, north 2, east 3.
        const DIRS: [(i32, i32, usize); 4] = [(0, -1, 2), (1, 0, 3), (0, 1, 0), (-1, 0, 1)];
        let mut reusable = [NONE; 4];
        for &(dx, dz, d2) in &DIRS {
            let node = self.find_accepted_node(
                eval,
                px + dx,
                py,
                pz + dz,
                jump_size,
                height,
                (dx, dz),
                current,
            );
            reusable[d2] = node.unwrap_or(NONE);
            if let Some(node) = node
                && self.neighbor_valid(node, pos)
            {
                out[p] = node;
                p += 1;
            }
        }
        for &(dx, dz, d2) in &DIRS {
            // `getClockWise`: north → east → south → west.
            let (cx, cz, c2) = match d2 {
                2 => (1, 0, 3),
                3 => (0, 1, 0),
                0 => (-1, 0, 1),
                _ => (0, -1, 2),
            };
            if self.diagonal_valid(eval, pos, reusable[d2], reusable[c2]) {
                let diag = self.find_accepted_node(
                    eval,
                    px + dx + cx,
                    py,
                    pz + dz + cz,
                    jump_size,
                    height,
                    (dx, dz),
                    current,
                );
                if let Some(d) = diag
                    && self.diagonal_node_valid(d)
                {
                    out[p] = d;
                    p += 1;
                }
            }
        }
        p
    }

    fn neighbor_valid(&self, neighbor: u32, current: u32) -> bool {
        let n = self.n(neighbor);
        !n.closed && (n.cost_malus >= 0.0 || self.n(current).cost_malus < 0.0)
    }

    fn diagonal_valid(&self, eval: &Eval<'_>, pos: u32, ew: u32, ns: u32) -> bool {
        if ns == NONE || ew == NONE {
            return false;
        }
        let (p, e, n) = (self.n(pos), self.n(ew), self.n(ns));
        if n.y > p.y || e.y > p.y {
            return false;
        }
        if e.kind == PathType::WalkableDoor || n.kind == PathType::WalkableDoor {
            return false;
        }
        let width = eval.mob.width;
        if width > 1.0 && (e.cost_malus > 0.0 || n.cost_malus > 0.0) {
            return false;
        }
        let between_posts = n.kind == PathType::Fence && e.kind == PathType::Fence && width < 0.5;
        (n.y < p.y || n.cost_malus >= 0.0 || between_posts)
            && (e.y < p.y || e.cost_malus >= 0.0 || between_posts)
    }

    fn diagonal_node_valid(&self, d: u32) -> bool {
        let n = self.n(d);
        !n.closed && n.kind != PathType::WalkableDoor && n.cost_malus >= 0.0
    }

    fn floor_level(&self, eval: &Eval<'_>, pos: BlockPos) -> f64 {
        if eval.mob.can_float && is_water(eval.blocks.state(pos)) {
            f64::from(pos.y) + 0.5
        } else {
            floor_level(eval.blocks, pos)
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn find_accepted_node(
        &mut self,
        eval: &Eval<'_>,
        x: i32,
        y: i32,
        z: i32,
        jump_size: i32,
        node_height: f64,
        dir: (i32, i32),
        current: PathType,
    ) -> Option<u32> {
        let max_y_target = self.floor_level(eval, BlockPos::new(x, y, z));
        let jump_height = f64::from(eval.mob.max_up_step).max(1.125);
        if max_y_target - node_height > jump_height {
            return None;
        }
        let t = self.cached_type(eval, x, y, z);
        let cost = Self::malus(eval, t);
        let mut best = None;
        if cost >= 0.0 {
            best = Some(self.node_cost_to_max(x, y, z, t, cost));
        }
        let partial = |t: PathType| {
            matches!(
                t,
                PathType::Fence | PathType::DoorWoodClosed | PathType::DoorIronClosed
            )
        };
        if partial(current)
            && let Some(b) = best
            && self.n(b).cost_malus >= 0.0
            && !self.can_reach_without_collision(eval, b)
        {
            best = None;
        }
        if t == PathType::Walkable {
            return best;
        }
        let best_blocked = best.is_none_or(|b| self.n(b).cost_malus < 0.0);
        if best_blocked
            && jump_size > 0
            && t != PathType::Fence
            && t != PathType::UnpassableRail
            && t != PathType::Trapdoor
            && t != PathType::PowderSnow
        {
            best = self.try_jump_on(eval, x, y, z, jump_size, node_height, dir, current);
        } else if t == PathType::Water && !eval.mob.can_float {
            best = self.try_first_non_water_below(eval, x, y, z, best);
        } else if t == PathType::Open {
            best = Some(self.try_first_ground_below(eval, x, y, z));
        } else if partial(t) && best.is_none() {
            best = Some(self.closed_node(x, y, z, t));
        }
        best
    }

    fn node_cost_to_max(&mut self, x: i32, y: i32, z: i32, t: PathType, cost: f32) -> u32 {
        let i = self.node(x, y, z);
        let n = self.nm(i);
        n.kind = t;
        n.cost_malus = n.cost_malus.max(cost);
        i
    }

    fn blocked_node(&mut self, x: i32, y: i32, z: i32) -> u32 {
        let i = self.node(x, y, z);
        let n = self.nm(i);
        n.kind = PathType::Blocked;
        n.cost_malus = -1.0;
        i
    }

    fn closed_node(&mut self, x: i32, y: i32, z: i32, t: PathType) -> u32 {
        let i = self.node(x, y, z);
        let n = self.nm(i);
        n.closed = true;
        n.kind = t;
        n.cost_malus = t.malus();
        i
    }

    #[allow(clippy::too_many_arguments)]
    fn try_jump_on(
        &mut self,
        eval: &Eval<'_>,
        x: i32,
        y: i32,
        z: i32,
        jump_size: i32,
        node_height: f64,
        dir: (i32, i32),
        current: PathType,
    ) -> Option<u32> {
        let above =
            self.find_accepted_node(eval, x, y + 1, z, jump_size - 1, node_height, dir, current)?;
        let mob = eval.mob;
        if mob.width >= 1.0 {
            return Some(above);
        }
        let kind = self.n(above).kind;
        if kind != PathType::Open && kind != PathType::Walkable {
            return Some(above);
        }
        let center_x = f64::from(x - dir.0) + 0.5;
        let center_z = f64::from(z - dir.1) + 0.5;
        let half = f64::from(mob.width) / 2.0;
        let (ax, ay, az) = {
            let n = self.n(above);
            (n.x, n.y, n.z)
        };
        let grow = Aabb::new(
            center_x - half,
            self.floor_level(
                eval,
                BlockPos::containing(center_x, f64::from(y + 1), center_z),
            ) + 0.001,
            center_z - half,
            center_x + half,
            f64::from(mob.height) + self.floor_level(eval, BlockPos::new(ax, ay, az)) - 0.002,
            center_z + half,
        );
        if self.has_collisions(eval, grow) {
            None
        } else {
            Some(above)
        }
    }

    fn try_first_non_water_below(
        &mut self,
        eval: &Eval<'_>,
        x: i32,
        mut y: i32,
        z: i32,
        mut best: Option<u32>,
    ) -> Option<u32> {
        y -= 1;
        while y > eval.search.min_y {
            let t = self.cached_type(eval, x, y, z);
            if t != PathType::Water {
                return best;
            }
            best = Some(self.node_cost_to_max(x, y, z, t, Self::malus(eval, t)));
            y -= 1;
        }
        best
    }

    fn try_first_ground_below(&mut self, eval: &Eval<'_>, x: i32, y: i32, z: i32) -> u32 {
        let mut cy = y - 1;
        while cy >= eval.search.min_y {
            if y - cy > eval.mob.max_fall_distance {
                return self.blocked_node(x, cy, z);
            }
            let t = self.cached_type(eval, x, cy, z);
            let cost = Self::malus(eval, t);
            if t != PathType::Open {
                if cost >= 0.0 {
                    return self.node_cost_to_max(x, cy, z, t, cost);
                }
                return self.blocked_node(x, cy, z);
            }
            cy -= 1;
        }
        self.blocked_node(x, y, z)
    }

    fn can_reach_without_collision(&mut self, eval: &Eval<'_>, to: u32) -> bool {
        let mob = eval.mob;
        let mut bb = mob.aabb;
        let (tx, ty, tz) = {
            let n = self.n(to);
            (f64::from(n.x), f64::from(n.y), f64::from(n.z))
        };
        let xs = bb.max_x - bb.min_x;
        let ys = bb.max_y - bb.min_y;
        let zs = bb.max_z - bb.min_z;
        let mut delta = Vec3::new(
            tx - mob.pos.x + xs / 2.0,
            ty - mob.pos.y + ys / 2.0,
            tz - mob.pos.z + zs / 2.0,
        );
        let size = (xs + ys + zs) / 3.0;
        let steps = vanillite_base::math::ceil(delta.length() / size).max(1);
        delta = delta * (1.0 / f64::from(steps));
        for _ in 1..=steps {
            bb = bb.offset(delta.x, delta.y, delta.z);
            if self.has_collisions(eval, bb) {
                return false;
            }
        }
        true
    }

    fn has_collisions(&mut self, eval: &Eval<'_>, aabb: Aabb) -> bool {
        let k = [
            aabb.min_x.to_bits(),
            aabb.min_y.to_bits(),
            aabb.min_z.to_bits(),
            aabb.max_x.to_bits(),
            aabb.max_y.to_bits(),
            aabb.max_z.to_bits(),
        ];
        if let Some(&c) = self.collisions.get(&k) {
            return c;
        }
        let c = !no_collision(eval.blocks, &aabb, &mut self.boxes);
        self.collisions.insert(k, c);
        c
    }

    // --- `SwimNodeEvaluator` -----------------------------------------------------------------

    /// `SwimNodeEvaluator.getStart`.
    fn swim_start(&mut self, eval: &Eval<'_>) -> u32 {
        let b = eval.mob.aabb;
        self.node(floor(b.min_x), floor(b.min_y + 0.5), floor(b.min_z))
    }

    /// `SwimNodeEvaluator.getNeighbors` (without breaching).
    fn swim_neighbors(&mut self, eval: &Eval<'_>, out: &mut [u32; 32], pos: u32) -> usize {
        let (px, py, pz) = {
            let n = self.n(pos);
            (n.x, n.y, n.z)
        };
        // `Direction.values()`: down, up, north, south, west, east.
        const DIRS: [(i32, i32, i32); 6] = [
            (0, -1, 0),
            (0, 1, 0),
            (0, 0, -1),
            (0, 0, 1),
            (-1, 0, 0),
            (1, 0, 0),
        ];
        let mut found = [NONE; 6];
        let mut p = 0;
        for (i, &(dx, dy, dz)) in DIRS.iter().enumerate() {
            let node = self.swim_accepted_node(eval, px + dx, py + dy, pz + dz);
            found[i] = node;
            if node != NONE && !self.n(node).closed {
                out[p] = node;
                p += 1;
            }
        }
        // `Direction.Plane.HORIZONTAL` (north, east, south, west) with the clockwise one.
        const HORIZONTAL: [(usize, usize, i32, i32); 4] =
            [(2, 5, 0, -1), (5, 3, 1, 0), (3, 4, 0, 1), (4, 2, -1, 0)];
        for &(a, b, dx, dz) in &HORIZONTAL {
            let has_malus = |n: u32| n != NONE && self.n(n).cost_malus >= 0.0;
            if has_malus(found[a]) && has_malus(found[b]) {
                let (cx, cz) = (DIRS[b].0, DIRS[b].2);
                let diag = self.swim_accepted_node(eval, px + dx + cx, py, pz + dz + cz);
                if diag != NONE && !self.n(diag).closed {
                    out[p] = diag;
                    p += 1;
                }
            }
        }
        p
    }

    /// `SwimNodeEvaluator.findAcceptedNode`: water nodes only.
    fn swim_accepted_node(&mut self, eval: &Eval<'_>, x: i32, y: i32, z: i32) -> u32 {
        let t = self.cached_swim_type(eval, x, y, z);
        if t != PathType::Water {
            return NONE;
        }
        let cost = Self::malus(eval, t);
        if cost < 0.0 {
            return NONE;
        }
        let i = self.node(x, y, z);
        let fluid_empty = eval
            .blocks
            .state(BlockPos::new(x, y, z))
            .fluid_state()
            .is_empty();
        let n = self.nm(i);
        n.kind = t;
        n.cost_malus = n.cost_malus.max(cost);
        if fluid_empty {
            n.cost_malus += 8.0;
        }
        i
    }

    fn cached_swim_type(&mut self, eval: &Eval<'_>, x: i32, y: i32, z: i32) -> PathType {
        let k = key(x, y, z);
        if let Some(&t) = self.types.get(&k) {
            return t;
        }
        let t = swim_type_of_mob(eval, x, y, z);
        self.types.insert(k, t);
        t
    }

    fn cached_type(&mut self, eval: &Eval<'_>, x: i32, y: i32, z: i32) -> PathType {
        let k = key(x, y, z);
        if let Some(&t) = self.types.get(&k) {
            return t;
        }
        let t = Self::type_of_mob(eval, x, y, z);
        self.types.insert(k, t);
        t
    }

    /// `getPathTypeOfMob`.
    fn type_of_mob(eval: &Eval<'_>, x: i32, y: i32, z: i32) -> PathType {
        let mut kinds: [Option<PathType>; 8] = [None; 8];
        let mut count = 0usize;
        let mut overflow = Vec::new();
        let blocks = eval.blocks;
        let min_y = eval.search.min_y;
        let mob_pos = eval.mob.block_pos();
        for dx in 0..eval.entity_width {
            for dy in 0..eval.entity_height {
                for dz in 0..eval.entity_depth {
                    let mut t = path_type_static(blocks, min_y, x + dx, y + dy, z + dz);
                    if t == PathType::DoorWoodClosed
                        && eval.mob.can_open_doors
                        && eval.mob.can_pass_doors
                    {
                        t = PathType::WalkableDoor;
                    }
                    if t == PathType::DoorOpen && !eval.mob.can_pass_doors {
                        t = PathType::Blocked;
                    }
                    if t == PathType::Rail
                        && path_type_static(blocks, min_y, mob_pos.x, mob_pos.y, mob_pos.z)
                            != PathType::Rail
                        && path_type_static(blocks, min_y, mob_pos.x, mob_pos.y - 1, mob_pos.z)
                            != PathType::Rail
                    {
                        t = PathType::UnpassableRail;
                    }
                    let seen =
                        kinds.iter().take(count).any(|k| *k == Some(t)) || overflow.contains(&t);
                    if !seen {
                        if count < kinds.len() {
                            kinds[count] = Some(t);
                            count += 1;
                        } else {
                            overflow.push(t);
                        }
                    }
                }
            }
        }
        let mut set: Vec<PathType> = kinds.iter().take(count).flatten().copied().collect();
        set.extend(overflow);
        // `EnumSet` iterates in declaration order.
        set.sort_by_key(|t| *t as u8);
        if set.len() == 1 {
            return set[0];
        }
        if set.contains(&PathType::Fence) {
            return PathType::Fence;
        }
        if set.contains(&PathType::UnpassableRail) {
            return PathType::UnpassableRail;
        }
        let mut highest_type = PathType::Blocked;
        let mut highest = Self::malus(eval, highest_type);
        for &t in &set {
            let m = Self::malus(eval, t);
            if m < 0.0 {
                return t;
            }
            if m >= highest {
                highest = m;
                highest_type = t;
            }
        }
        let current = path_type_static(blocks, min_y, x, y, z);
        if eval.entity_width > 1 {
            let cheaper = Self::malus(eval, current) < highest;
            let cap = cheaper && Self::malus(eval, PathType::BigMobsCloseToDanger) < highest;
            if cap {
                PathType::BigMobsCloseToDanger
            } else {
                highest_type
            }
        } else if current == PathType::Open && highest_type != PathType::Open && highest == 0.0 {
            PathType::Open
        } else {
            highest_type
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Flat;
    impl Blocks for Flat {
        fn state(&self, pos: BlockPos) -> BlockState {
            if pos.y < 64 || (pos.y == 64 && pos.x == 3 && pos.z <= 2) {
                ids::STONE.default_state()
            } else {
                BlockState::AIR
            }
        }
    }

    fn mob(pos: Vec3) -> PathMob {
        PathMob {
            pos,
            aabb: Aabb::new(
                pos.x - 0.3,
                pos.y,
                pos.z - 0.3,
                pos.x + 0.3,
                pos.y + 1.95,
                pos.z + 0.3,
            ),
            width: 0.6,
            height: 1.95,
            on_ground: true,
            max_up_step: 0.6,
            max_fall_distance: 3,
            in_water: false,
            can_float: false,
            can_open_doors: false,
            can_pass_doors: true,
        }
    }

    #[test]
    fn finds_a_path_over_a_step() {
        let mut finder = PathFinder::new();
        let search = Search {
            max_path_length: 16.0,
            reach_range: 0,
            max_visited_nodes: 256,
            malus: &[],
            min_y: -64,
            swim: false,
        };
        let path = finder
            .find_path(
                &Flat,
                &mob(Vec3::new(0.5, 64.0, 0.5)),
                &[BlockPos::new(6, 64, 0)],
                search,
            )
            .unwrap_or_else(|| panic!("no start"));
        assert!(path.reached);
        assert_eq!(
            path.nodes.first().map(PathNode::pos),
            Some(BlockPos::new(0, 64, 0))
        );
        assert_eq!(
            path.nodes.last().map(PathNode::pos),
            Some(BlockPos::new(6, 64, 0))
        );
        // The wall at x = 3 is one block high: the path jumps on it.
        assert!(path.nodes.iter().any(|n| n.y == 65));
    }

    /// A pool of water from y 60 to 63 within 8 blocks, with a stone wall at x = 3 up to
    /// y 62.
    struct Pool;
    impl Blocks for Pool {
        fn state(&self, pos: BlockPos) -> BlockState {
            let inside = pos.x.abs() <= 8 && pos.z.abs() <= 8;
            if pos.x == 3 && (60..63).contains(&pos.y) && inside {
                ids::STONE.default_state()
            } else if inside && (60..64).contains(&pos.y) {
                ids::WATER.default_state()
            } else if pos.y >= 64 {
                BlockState::AIR
            } else {
                ids::STONE.default_state()
            }
        }
    }

    #[test]
    fn swims_over_a_wall_and_stays_in_water() {
        let mut finder = PathFinder::new();
        let search = Search {
            max_path_length: 16.0,
            reach_range: 0,
            max_visited_nodes: 480,
            malus: &[(PathType::Water, 0.0)],
            min_y: -64,
            swim: true,
        };
        let pos = Vec3::new(0.5, 61.0, 0.5);
        let guardian = PathMob {
            aabb: Aabb::new(
                pos.x - 0.425,
                pos.y,
                pos.z - 0.425,
                pos.x + 0.425,
                pos.y + 0.85,
                pos.z + 0.425,
            ),
            width: 0.85,
            height: 0.85,
            in_water: true,
            ..mob(pos)
        };
        let path = finder
            .find_path(&Pool, &guardian, &[BlockPos::new(6, 61, 0)], search)
            .unwrap_or_else(|| panic!("no start"));
        assert!(path.reached);
        assert!(path.nodes.iter().skip(1).all(|n| n.kind == PathType::Water));
        assert!(path.nodes.iter().any(|n| n.y == 63));
        assert_eq!(
            path.nodes.last().map(PathNode::pos),
            Some(BlockPos::new(6, 61, 0))
        );
    }

    #[test]
    fn swim_path_types() {
        let mob = mob(Vec3::new(0.5, 61.0, 0.5));
        let search = Search {
            max_path_length: 16.0,
            reach_range: 0,
            max_visited_nodes: 64,
            malus: &[],
            min_y: -64,
            swim: true,
        };
        let eval = Eval {
            blocks: &Pool,
            mob: &mob,
            search,
            entity_width: 1,
            entity_height: 1,
            entity_depth: 1,
        };
        assert_eq!(swim_type_of_mob(&eval, 0, 61, 0), PathType::Water);
        assert_eq!(swim_type_of_mob(&eval, 3, 61, 0), PathType::Blocked);
        assert_eq!(swim_type_of_mob(&eval, 0, 64, 0), PathType::Breach);
    }

    #[test]
    fn walkable_ground_and_open_air() {
        assert_eq!(path_type_static(&Flat, -64, 0, 64, 0), PathType::Walkable);
        assert_eq!(path_type_static(&Flat, -64, 0, 66, 0), PathType::Open);
        assert_eq!(path_type_static(&Flat, -64, 0, 63, 0), PathType::Blocked);
    }
}

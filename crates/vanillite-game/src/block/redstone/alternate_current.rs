//! Redstone wire after Alternate Current (rule `performance.redstone.implementation =
//! "alternate-current"`).
//!
//! The algorithm follows Alternate Current by Space Walker (MIT,
//! <https://github.com/SpaceWalkerRS/alternate-current>); the code is written anew. Instead of
//! Vanilla's recursion, where every wire that changes power updates the 42 blocks around it and
//! every wire among them computes its power again, a change computes the whole affected wire
//! network at once:
//!
//! 1. The wire that got the update takes its target power. If that is lower, it and every wire
//!    that may have been powered by it (transitively, with lower power) drop to the power they
//!    get from blocks alone.
//! 2. Power spreads from the strongest wires down, one less per wire, into every wire that
//!    reads it (a bucket queue, so each wire is finished once).
//! 3. Every wire whose power changed is set (clients only, like Vanilla), then the blocks
//!    around them (distance two) get one neighbour update each, wires excluded: their power is
//!    already final.
//!
//! Final powers equal Vanilla's. The update order differs: wires nearest the change come
//! first, and around each wire the order is relative to the direction power flows in (front,
//! back, right, left, down, up), not Vanilla's `HashSet` order, so a circuit behaves the same
//! in every rotation and place. Wires powered by blocks only count as flowing north.

use std::cell::Cell;

use vanillite_base::hash::{FxHashMap, FxHashSet};
use vanillite_base::{BlockPos, Direction};
use vanillite_data::block::{BlockId, BlockState, ids};
use vanillite_worldgen::behaviour::{HORIZONTAL, PosExt, StateExt, clockwise, counter_clockwise};

use super::wire;
use crate::block::{Behaviours, drops};
use crate::level::signal::{wire_power, with_power};
use crate::level::{Level, update};

/// Replaces the placement, removal and neighbour behaviour of redstone wire with this one.
pub fn install(behaviours: &mut Behaviours) {
    let mut b = *behaviours.get(ids::REDSTONE_WIRE);
    b.on_place = Some(on_place);
    b.affect_neighbors_after_removal = Some(affect_neighbors_after_removal);
    b.neighbor_changed = Some(neighbor_changed);
    behaviours.set(ids::REDSTONE_WIRE, b);
}

/// `onPlace`: as Vanilla, with the network update instead of `updatePowerStrength`.
fn on_place(level: &mut Level, state: BlockState, pos: BlockPos, old: BlockState, _: bool) {
    if old.block() == state.block() {
        return;
    }
    update_network(level, pos, None);
    for direction in wire::VERTICAL {
        level.update_neighbors_at(pos.relative(direction), ids::REDSTONE_WIRE);
    }
    wire::update_neighbors_of_neighboring_wires(level, pos);
}

/// `affectNeighborsAfterRemoval`: the removed wire's power leaves the network; the blocks
/// around it are updated once each.
fn affect_neighbors_after_removal(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    moved: bool,
) {
    if moved {
        return;
    }
    update_network(level, pos, Some(wire_power(state)));
    wire::update_neighbors_of_neighboring_wires(level, pos);
}

/// `neighborChanged`.
fn neighbor_changed(level: &mut Level, state: BlockState, pos: BlockPos, _: BlockId, _: bool) {
    if level.can_survive(state, pos) {
        update_network(level, pos, None);
    } else {
        drops::drop_resources(level, pos, state, None);
        level.remove_block(pos, false);
    }
}

/// A wire of the network being updated.
#[derive(Clone, Copy, Debug)]
struct Node {
    pos: BlockPos,
    /// The power before the update.
    current: u8,
    /// The power after the update.
    power: u8,
    /// Dropped in the search: its power is computed anew.
    reset: bool,
    /// False for the position of a removed wire.
    wire: bool,
    /// The horizontal direction power flows in at this wire.
    flow: Direction,
}

/// The work space of one update, reused between updates of a thread.
#[derive(Default)]
struct Network {
    index: FxHashMap<BlockPos, u32>,
    nodes: Vec<Node>,
    /// Reset nodes, in the order they were found.
    reset: Vec<u32>,
    /// Nodes by the power they spread, 1..=15.
    buckets: [Vec<u32>; 16],
    /// Blocks that got their neighbour update.
    updated: FxHashSet<BlockPos>,
}

/// Capacity kept between updates; a larger network gives its memory back.
const KEEP_NODES: usize = 4096;

thread_local! {
    static NETWORK: Cell<Option<Box<Network>>> = const { Cell::new(None) };
}

impl Network {
    /// The node at `pos`, added with its current power if it is new.
    fn node(&mut self, level: &Level, pos: BlockPos) -> usize {
        if let Some(&i) = self.index.get(&pos) {
            return i as usize;
        }
        let i = self.nodes.len();
        let current = wire_power(level.block(pos)) as u8;
        self.index.insert(pos, i as u32);
        self.nodes.push(Node {
            pos,
            current,
            power: current,
            reset: false,
            wire: true,
            flow: Direction::North,
        });
        i
    }

    fn clear(&mut self) {
        let shrink = self.nodes.capacity() > KEEP_NODES;
        self.index.clear();
        self.nodes.clear();
        self.reset.clear();
        self.updated.clear();
        for bucket in &mut self.buckets {
            bucket.clear();
        }
        if shrink {
            self.index.shrink_to(KEEP_NODES);
            self.nodes.shrink_to(KEEP_NODES);
            self.reset.shrink_to(KEEP_NODES);
            self.updated.shrink_to(KEEP_NODES * 4);
            for bucket in &mut self.buckets {
                bucket.shrink_to(KEEP_NODES);
            }
        }
    }
}

/// Updates the network around the wire at `pos`, or around the wire with `removed` power that
/// was removed from `pos`.
fn update_network(level: &mut Level, pos: BlockPos, removed: Option<i32>) {
    // Nested updates (a neighbour update that reaches another wire network) take a fresh
    // work space; the outer one is put back last.
    let mut net = NETWORK.take().unwrap_or_default();
    run(level, &mut net, pos, removed);
    net.clear();
    NETWORK.set(Some(net));
}

fn run(level: &mut Level, net: &mut Network, pos: BlockPos, removed: Option<i32>) {
    if let Some(power) = removed {
        net.index.insert(pos, 0);
        net.nodes.push(Node {
            pos,
            current: power as u8,
            power: 0,
            reset: true,
            wire: false,
            flow: Direction::North,
        });
        net.reset.push(0);
    } else {
        let external = level.wire_block_signal(pos);
        let (incoming, from) = incoming(level, pos, |_| None);
        let target = external.max(incoming) as u8;
        let i = net.node(level, pos);
        let root = &mut net.nodes[i];
        if target == root.current {
            return;
        }
        if target > root.current {
            root.power = target;
            if incoming > external
                && let Some(from) = from
            {
                root.flow = from;
            }
            net.buckets[usize::from(target)].push(i as u32);
        } else {
            root.reset = true;
            root.power = external as u8;
            net.reset.push(i as u32);
        }
    }

    // 1. Wires that may have been powered by a reset wire drop as well.
    let mut cursor = 0;
    while cursor < net.reset.len() {
        let node = net.nodes[net.reset[cursor] as usize];
        cursor += 1;
        let (receivers, count) = receivers(level, node.pos);
        for &(receiver, direction) in &receivers[..count] {
            let j = net.node(level, receiver);
            let n = &net.nodes[j];
            if n.reset || n.current == 0 || n.current >= node.current {
                continue;
            }
            let external = level.wire_block_signal(receiver) as u8;
            let n = &mut net.nodes[j];
            n.reset = true;
            n.power = external;
            n.flow = direction;
            net.reset.push(j as u32);
        }
    }

    // 2. Reset wires take the power of the wires around them that keep theirs, then power
    // spreads from the strongest wire down.
    for k in 0..net.reset.len() {
        let i = net.reset[k] as usize;
        let node = net.nodes[i];
        if !node.wire {
            continue;
        }
        let (incoming, from) = incoming(level, node.pos, |p| {
            net.index
                .get(&p)
                .map(|&j| net.nodes[j as usize])
                .filter(|n| n.reset)
                .map(|_| 0)
        });
        let n = &mut net.nodes[i];
        if incoming > i32::from(n.power) {
            n.power = incoming as u8;
            if let Some(from) = from {
                n.flow = from;
            }
        }
        if n.power > 1 {
            net.buckets[usize::from(n.power)].push(i as u32);
        }
    }
    for power in (2..=15u8).rev() {
        while let Some(i) = net.buckets[usize::from(power)].pop() {
            let node = net.nodes[i as usize];
            if node.power != power {
                continue;
            }
            let (receivers, count) = receivers(level, node.pos);
            for &(receiver, direction) in &receivers[..count] {
                let j = net.node(level, receiver);
                let n = &mut net.nodes[j];
                if n.wire && n.power < power - 1 {
                    n.power = power - 1;
                    n.flow = direction;
                    if n.power > 1 {
                        net.buckets[usize::from(n.power)].push(j as u32);
                    }
                }
            }
        }
    }

    // 3. Set the changed wires, then update the blocks around them once each.
    for node in &net.nodes {
        if node.wire && node.power != node.current {
            let state = level.block(node.pos);
            if state.is(ids::REDSTONE_WIRE) {
                level.set_block(
                    node.pos,
                    with_power(state, i32::from(node.power)),
                    update::CLIENTS,
                );
            }
        }
    }
    for k in 0..net.nodes.len() {
        let node = net.nodes[k];
        if node.wire && node.power == node.current {
            continue;
        }
        let f = node.flow;
        let order = [
            f,
            f.opposite(),
            clockwise(f),
            counter_clockwise(f),
            Direction::Down,
            Direction::Up,
        ];
        for direction in order {
            notify(level, &mut net.updated, node.pos.relative(direction));
        }
        for direction in order {
            let side = node.pos.relative(direction);
            for second in order {
                notify(level, &mut net.updated, side.relative(second));
            }
        }
    }
}

/// One neighbour update of the block at `pos` unless it had one or is a wire.
fn notify(level: &mut Level, updated: &mut FxHashSet<BlockPos>, pos: BlockPos) {
    if updated.insert(pos) && !level.block(pos).is(ids::REDSTONE_WIRE) {
        level.neighbor_changed(pos, ids::REDSTONE_WIRE);
    }
}

fn is_wire(level: &Level, pos: BlockPos) -> bool {
    level.block(pos).is(ids::REDSTONE_WIRE)
}

fn conducts(level: &Level, pos: BlockPos) -> bool {
    Level::is_redstone_conductor(level.block(pos))
}

/// The strongest wire signal into `pos` (`getIncomingWireSignal`) and the direction it flows
/// in. `known` gives the power of wires already in the network.
fn incoming(
    level: &Level,
    pos: BlockPos,
    known: impl Fn(BlockPos) -> Option<u8>,
) -> (i32, Option<Direction>) {
    let mut best = 0;
    let mut from = None;
    let mut take = |p: BlockPos, state: BlockState, direction: Direction| {
        if state.is(ids::REDSTONE_WIRE) {
            let power = known(p).map_or_else(|| wire_power(state), i32::from);
            if power > best {
                best = power;
                from = Some(direction.opposite());
            }
        }
    };
    let above_conducts = conducts(level, pos.above());
    for direction in HORIZONTAL {
        let side = pos.relative(direction);
        let state = level.block(side);
        take(side, state, direction);
        let side_conducts = Level::is_redstone_conductor(state);
        if side_conducts && !above_conducts {
            let p = side.above();
            take(p, level.block(p), direction);
        } else if !side_conducts {
            let p = side.below();
            take(p, level.block(p), direction);
        }
    }
    ((best - 1).max(0), from)
}

/// The wires that read the wire signal of `pos` in their `getIncomingWireSignal`, with the
/// horizontal direction from `pos` to them.
fn receivers(level: &Level, pos: BlockPos) -> ([(BlockPos, Direction); 12], usize) {
    let mut out = [(pos, Direction::North); 12];
    let mut count = 0;
    let above_conducts = conducts(level, pos.above());
    let below_conducts = conducts(level, pos.below());
    for direction in HORIZONTAL {
        let side = pos.relative(direction);
        let state = level.block(side);
        if state.is(ids::REDSTONE_WIRE) {
            out[count] = (side, direction);
            count += 1;
        }
        // A wire one up reads down through a non-conductor above `pos`.
        let up = side.above();
        if !above_conducts && is_wire(level, up) {
            out[count] = (up, direction);
            count += 1;
        }
        // A wire one down reads up through the conductor below `pos` if nothing conducts above
        // itself.
        let down = side.below();
        if below_conducts && !Level::is_redstone_conductor(state) && is_wire(level, down) {
            out[count] = (down, direction);
            count += 1;
        }
    }
    (out, count)
}

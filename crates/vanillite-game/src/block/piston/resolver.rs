//! `PistonStructureResolver`: the blocks a piston moves and the blocks it breaks.

use vanillite_base::{BlockPos, Direction};
use vanillite_data::block::{BlockState, ids};

use super::{is_pushable, push_reaction_of};
use crate::level::Level;
use vanillite_data::block::PushReaction;

/// `PistonStructureResolver.MAX_PUSH_DEPTH`.
pub const MAX_PUSH_DEPTH: usize = 12;

/// A resolved structure; the lists are reused between resolutions.
#[derive(Debug, Default)]
pub struct Structure {
    /// `toPush`, in Vanilla's order.
    pub to_push: Vec<BlockPos>,
    /// `toDestroy`.
    pub to_destroy: Vec<BlockPos>,
    /// Scratch for `reorderListAtCollision`.
    reorder: Vec<BlockPos>,
}

/// The resolver's inputs.
#[derive(Clone, Copy, Debug)]
pub(super) struct Resolver {
    piston_pos: BlockPos,
    piston_direction: Direction,
    push_direction: Direction,
    start_pos: BlockPos,
    extending: bool,
}

impl Resolver {
    pub(super) fn new(piston_pos: BlockPos, direction: Direction, extending: bool) -> Resolver {
        let (push_direction, start_pos) = if extending {
            (direction, piston_pos.relative(direction))
        } else {
            (
                direction.opposite(),
                piston_pos.relative(direction).relative(direction),
            )
        };
        Resolver {
            piston_pos,
            piston_direction: direction,
            push_direction,
            start_pos,
            extending,
        }
    }

    /// `resolve()`.
    pub(super) fn resolve(&self, level: &Level, out: &mut Structure) -> bool {
        out.to_push.clear();
        out.to_destroy.clear();
        let next = level.block(self.start_pos);
        if !is_pushable(
            next,
            level,
            self.start_pos,
            self.push_direction,
            false,
            self.piston_direction,
        ) {
            if self.extending && push_reaction_of(next) == PushReaction::Popped {
                out.to_destroy.push(self.start_pos);
                return true;
            }
            return false;
        }
        if !self.add_block_line(level, out, self.start_pos, self.push_direction) {
            return false;
        }
        let mut i = 0;
        while i < out.to_push.len() {
            let pos = out.to_push[i];
            if is_sticky(level.block(pos)) && !self.add_branching_blocks(level, out, pos) {
                return false;
            }
            i += 1;
        }
        true
    }

    /// `addBlockLine(start, direction)`.
    fn add_block_line(
        &self,
        level: &Level,
        out: &mut Structure,
        start: BlockPos,
        direction: Direction,
    ) -> bool {
        let mut next = level.block(start);
        if next.is_air()
            || !is_pushable(next, level, start, self.push_direction, false, direction)
            || start == self.piston_pos
            || out.to_push.contains(&start)
        {
            return true;
        }
        let back = self.push_direction.opposite();
        let mut block_count = 1;
        if block_count + out.to_push.len() > MAX_PUSH_DEPTH {
            return false;
        }
        while is_sticky(next) {
            let pos = offset(start, back, block_count as i32);
            let previous = next;
            next = level.block(pos);
            if next.is_air()
                || !can_stick_to_each_other(previous, next)
                || !is_pushable(next, level, pos, self.push_direction, false, back)
                || pos == self.piston_pos
            {
                break;
            }
            block_count += 1;
            if block_count + out.to_push.len() > MAX_PUSH_DEPTH {
                return false;
            }
        }
        let mut blocks_added = 0;
        for i in (0..block_count).rev() {
            out.to_push.push(offset(start, back, i as i32));
            blocks_added += 1;
        }
        let mut i = 1;
        loop {
            let pos = offset(start, self.push_direction, i);
            if let Some(collision) = out.to_push.iter().position(|p| *p == pos) {
                reorder_list_at_collision(out, blocks_added, collision);
                for j in 0..=collision + blocks_added {
                    let blocked = out.to_push[j];
                    if is_sticky(level.block(blocked))
                        && !self.add_branching_blocks(level, out, blocked)
                    {
                        return false;
                    }
                }
                return true;
            }
            next = level.block(pos);
            if next.is_air() {
                return true;
            }
            if !is_pushable(
                next,
                level,
                pos,
                self.push_direction,
                true,
                self.push_direction,
            ) || pos == self.piston_pos
            {
                return false;
            }
            if push_reaction_of(next) == PushReaction::Popped {
                out.to_destroy.push(pos);
                return true;
            }
            if out.to_push.len() >= MAX_PUSH_DEPTH {
                return false;
            }
            out.to_push.push(pos);
            blocks_added += 1;
            i += 1;
        }
    }

    /// `addBranchingBlocks(fromPos)`.
    fn add_branching_blocks(&self, level: &Level, out: &mut Structure, from: BlockPos) -> bool {
        let from_state = level.block(from);
        for direction in Direction::ALL {
            if axis(direction) != axis(self.push_direction) {
                let neighbour = from.relative(direction);
                let neighbour_state = level.block(neighbour);
                if can_stick_to_each_other(neighbour_state, from_state)
                    && !self.add_block_line(level, out, neighbour, direction)
                {
                    return false;
                }
            }
        }
        true
    }
}

/// `reorderListAtCollision(blocksAdded, collisionPos)`: the line just added moves in front of
/// the blocks from the collision on.
fn reorder_list_at_collision(out: &mut Structure, blocks_added: usize, collision: usize) {
    let len = out.to_push.len();
    out.reorder.clear();
    out.reorder.extend_from_slice(&out.to_push[..collision]);
    out.reorder
        .extend_from_slice(&out.to_push[len - blocks_added..]);
    out.reorder
        .extend_from_slice(&out.to_push[collision..len - blocks_added]);
    std::mem::swap(&mut out.to_push, &mut out.reorder);
}

/// `pos.relative(direction, distance)`.
#[inline]
pub(super) fn offset(pos: BlockPos, direction: Direction, distance: i32) -> BlockPos {
    let [dx, dy, dz] = direction.normal();
    pos.offset(dx * distance, dy * distance, dz * distance)
}

/// The axis of a direction (0 = X, 1 = Y, 2 = Z).
#[inline]
pub(super) fn axis(direction: Direction) -> u8 {
    match direction {
        Direction::West | Direction::East => 0,
        Direction::Down | Direction::Up => 1,
        Direction::North | Direction::South => 2,
    }
}

/// `isSticky(state)`: slime and honey blocks.
#[inline]
pub fn is_sticky(state: BlockState) -> bool {
    state.block() == ids::SLIME_BLOCK || state.block() == ids::HONEY_BLOCK
}

/// `canStickToEachOther(state1, state2)`: slime and honey do not stick to each other.
#[inline]
pub fn can_stick_to_each_other(a: BlockState, b: BlockState) -> bool {
    let (a_honey, a_slime) = (a.block() == ids::HONEY_BLOCK, a.block() == ids::SLIME_BLOCK);
    let (b_honey, b_slime) = (b.block() == ids::HONEY_BLOCK, b.block() == ids::SLIME_BLOCK);
    if (a_honey && b_slime) || (a_slime && b_honey) {
        return false;
    }
    is_sticky(a) || is_sticky(b)
}

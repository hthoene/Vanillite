//! Pistons. Java moves blocks as `moving_piston` blocks without block entity data: the Java
//! client animates them itself from `block_event`. Bedrock draws the arm from the piston's
//! `PistonArm` block actor and moved blocks from `MovingBlock` actors, so the session animates
//! both over the two ticks of a move (the approach of Geyser's `PistonBlockEntity`, MIT,
//! Copyright (c) 2019-2026 GeyserMC). `moving_piston` updates are not shown: the client keeps
//! what it shows until the move ends.
//!
//! The moved blocks come from a small memory of Java block states: the blocks in front of the
//! pistons of each chunk and recent block updates. A moved block it does not know stays where it
//! was until the server sends the block at its new place.

use rustc_hash::FxHashMap;
use vanillite_data::block::BlockState;

use super::Session;
use crate::nbt::Compound;
use crate::packets::{self as bp, id};

/// Most remembered block states (a full memory is cleared; misses only cost the animation).
const MEMORY_MAX: usize = 16_384;

/// Farthest block a piston moves (12 pushed blocks plus the head).
const REACH: i32 = 13;

/// Java `block_event` types of pistons.
const EXTEND: u8 = 0;
const CONTRACT: u8 = 1;

/// A remembered Java block state.
#[derive(Debug, Clone, Copy)]
struct Known {
    state: u32,
    /// The state before the current tick (a piston's block updates of a tick come before its
    /// `block_event`, so this is the block that moved).
    before: u32,
    tick: u32,
}

/// A piston moving on the client.
#[derive(Debug)]
struct Moving {
    pos: [i32; 3],
    sticky: bool,
    extending: bool,
    progress: f32,
    last: f32,
    /// Start positions of the moved blocks, flattened.
    attached: Vec<i32>,
}

/// The pistons of a session.
#[derive(Debug, Default)]
pub(super) struct Pistons {
    memory: FxHashMap<[i32; 3], Known>,
    /// Positions of this tick's `moving_piston` updates.
    pending: Vec<[i32; 3]>,
    active: Vec<Moving>,
    tick: u32,
}

impl Pistons {
    fn remember(&mut self, pos: [i32; 3], state: u32) {
        if self.memory.len() >= MEMORY_MAX && !self.memory.contains_key(&pos) {
            self.memory.clear();
        }
        let tick = self.tick;
        let k = self.memory.entry(pos).or_insert(Known {
            state,
            before: state,
            tick,
        });
        if k.tick != tick {
            k.before = k.state;
            k.tick = tick;
        }
        k.state = state;
    }

    /// The state a block had before this tick.
    fn before(&self, pos: [i32; 3]) -> Option<u32> {
        self.memory.get(&pos).map(|k| {
            if k.tick == self.tick {
                k.before
            } else {
                k.state
            }
        })
    }

    /// Forgets everything (another dimension).
    pub(super) fn clear(&mut self) {
        self.memory.clear();
        self.pending.clear();
        self.active.clear();
    }

    /// Blocks in front of a chunk's pistons ([`crate::chunk::translate`]).
    pub(super) fn remember_chunk(&mut self, blocks: &[([i32; 3], u32)]) {
        for &(pos, state) in blocks {
            if self.memory.len() >= MEMORY_MAX {
                self.memory.clear();
            }
            self.memory.insert(
                pos,
                Known {
                    state,
                    before: state,
                    tick: self.tick.wrapping_sub(1),
                },
            );
        }
    }

    fn animating(&self, pos: [i32; 3]) -> bool {
        self.active.iter().any(|m| m.pos == pos)
    }
}

/// Whether a Java state is `moving_piston`.
fn is_moving_piston(state: u32) -> bool {
    u16::try_from(state).is_ok_and(|s| {
        let s = BlockState(s);
        s.is_valid() && s.block().name() == "minecraft:moving_piston"
    })
}

fn add(a: [i32; 3], b: [i32; 3]) -> [i32; 3] {
    [
        a[0].wrapping_add(b[0]),
        a[1].wrapping_add(b[1]),
        a[2].wrapping_add(b[2]),
    ]
}

fn sub(a: [i32; 3], b: [i32; 3]) -> [i32; 3] {
    [
        a[0].wrapping_sub(b[0]),
        a[1].wrapping_sub(b[1]),
        a[2].wrapping_sub(b[2]),
    ]
}

fn piston_arm(m: &Moving, attached: &[i32]) -> Vec<u8> {
    // 0 retracted, 1 extending, 2 extended, 3 retracting.
    let state = if m.extending {
        if m.progress >= 1.0 && m.last >= 1.0 {
            2
        } else {
            1
        }
    } else if m.progress <= 0.0 && m.last <= 0.0 {
        0
    } else {
        3
    };
    let [x, y, z] = m.pos;
    let mut c = Compound::default();
    c.string("id", "PistonArm")
        .ints("AttachedBlocks", attached)
        .float("Progress", m.progress)
        .float("LastProgress", m.last)
        .byte("NewState", state)
        .byte("State", state)
        .byte("Sticky", u8::from(m.sticky))
        .int("x", x)
        .int("y", y)
        .int("z", z)
        .byte("isMovable", 0);
    bp::block_actor_data(m.pos, &c.root())
}

impl Session {
    /// A Java block update: `moving_piston` is not shown (the move is animated instead).
    /// Returns whether the update is done.
    pub(super) fn piston_block_update(&mut self, pos: [i32; 3], state: u32) -> bool {
        if is_moving_piston(state) {
            self.pistons.pending.push(pos);
            return true;
        }
        self.pistons.remember(pos, state);
        false
    }

    /// Whether the session animates the piston at `pos` (its block actor is sent per tick).
    pub(super) fn piston_animating(&self, pos: [i32; 3]) -> bool {
        self.pistons.animating(pos)
    }

    /// A piston's `block_event`: start the arm and the moved blocks.
    pub(super) fn piston_event(&mut self, pos: [i32; 3], action: u8, direction: u8, sticky: bool) {
        let extending = action == EXTEND;
        let facing = crate::blockentity::direction_step(direction);
        // Extending pushes along the facing; a sticky piston contracting pulls one block back.
        let pulls = action == CONTRACT && sticky;
        let movement = if extending {
            facing
        } else {
            sub([0; 3], facing)
        };
        let head = add(pos, facing);
        let pending = std::mem::take(&mut self.pistons.pending);
        let mut moved: Vec<([i32; 3], [i32; 3], u32)> = Vec::new();
        let mut rest = Vec::new();
        for d in pending {
            let near = (0..3).all(|i| {
                d.get(i)
                    .zip(pos.get(i))
                    .is_some_and(|(a, b)| a.wrapping_sub(*b).abs() <= REACH)
            });
            let ours = near && d != pos && (extending || pulls) && !(extending && d == head);
            if !ours {
                rest.push(d);
                continue;
            }
            let origin = sub(d, movement);
            match self.pistons.before(origin) {
                Some(state) if state != 0 && !is_moving_piston(state) => {
                    moved.push((origin, d, state));
                }
                _ => {}
            }
        }
        self.pistons.pending = rest;
        let palette = self.version.palette;
        let blocks = palette.blocks();
        let states = palette.states();
        let mut attached = Vec::with_capacity(moved.len().saturating_mul(3));
        if let Some(moving_block) = states.moving_block {
            for &(origin, _, _) in &moved {
                self.send(id::UPDATE_BLOCK, &bp::update_block(origin, blocks.air, 0));
                self.send(id::UPDATE_BLOCK, &bp::update_block(origin, blocks.air, 1));
            }
            for &(origin, d, state) in &moved {
                let rid = u32::from(blocks.entry(state) & !crate::data::WATERLOGGED);
                let Some(block) = states.get(rid) else {
                    continue;
                };
                attached.extend_from_slice(&origin);
                let [x, y, z] = d;
                let mut c = Compound::default();
                c.string("id", "MovingBlock")
                    .raw_compound("movingBlock", block)
                    .byte("expanding", u8::from(extending))
                    .int("pistonPosX", pos[0])
                    .int("pistonPosY", pos[1])
                    .int("pistonPosZ", pos[2])
                    .int("x", x)
                    .int("y", y)
                    .int("z", z)
                    .byte("isMovable", 1);
                self.send(id::UPDATE_BLOCK, &bp::update_block(d, moving_block, 0));
                self.send(id::BLOCK_ACTOR_DATA, &bp::block_actor_data(d, &c.root()));
            }
        }
        // A new event replaces a running move of the same piston.
        self.pistons.active.retain(|m| m.pos != pos);
        let start = if extending { 0.0 } else { 1.0 };
        let m = Moving {
            pos,
            sticky,
            extending,
            progress: start,
            last: start,
            attached,
        };
        let body = piston_arm(&m, &m.attached);
        self.send(id::BLOCK_ACTOR_DATA, &body);
        self.pistons.active.push(m);
    }

    /// One client tick: advance the moving pistons (half a block per tick, like Java).
    pub(super) fn piston_tick(&mut self) {
        let p = &mut self.pistons;
        p.tick = p.tick.wrapping_add(1);
        p.pending.clear();
        if p.active.is_empty() {
            return;
        }
        let mut frames = Vec::with_capacity(p.active.len());
        p.active.retain_mut(|m| {
            m.last = m.progress;
            m.progress = if m.extending {
                (m.progress + 0.5).min(1.0)
            } else {
                (m.progress - 0.5).max(0.0)
            };
            let done = if m.extending {
                m.last >= 1.0
            } else {
                m.last <= 0.0
            };
            frames.push(piston_arm(m, if done { &[] } else { &m.attached }));
            !done
        });
        for f in frames {
            self.send(id::BLOCK_ACTOR_DATA, &f);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn memory_keeps_the_state_before_the_tick() {
        let mut p = Pistons::default();
        p.remember([0, 0, 0], 5);
        p.tick = 1;
        p.remember([0, 0, 0], 0);
        assert_eq!(p.before([0, 0, 0]), Some(5));
        p.tick = 2;
        assert_eq!(p.before([0, 0, 0]), Some(0));
        p.remember_chunk(&[([1, 2, 3], 7)]);
        assert_eq!(p.before([1, 2, 3]), Some(7));
    }
}

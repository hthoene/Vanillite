//! Vanilla's `CollectingNeighborUpdater`: neighbour and shape updates run breadth-first per
//! layer instead of recursing, and a chain stops after `max-chained-neighbor-updates`.
//!
//! An update that is triggered while updates run is collected for the current layer; after the
//! running update finished its current step, the collected ones go on top of the stack in the
//! order they were added. Multi-neighbour updates run one neighbour per step, in
//! `NeighborUpdater.UPDATE_ORDER` (west, east, down, up, north, south).

use vanillite_base::{BlockPos, Direction};
use vanillite_data::block::{BlockId, BlockState, ids};

use super::Level;
use super::update;

/// `NeighborUpdater.UPDATE_ORDER`.
pub const UPDATE_ORDER: [Direction; 6] = [
    Direction::West,
    Direction::East,
    Direction::Down,
    Direction::Up,
    Direction::North,
    Direction::South,
];

/// One queued update (`CollectingNeighborUpdater.NeighborUpdates`).
#[derive(Clone, Copy, Debug)]
pub(super) enum Update {
    /// `ShapeUpdate`.
    Shape {
        direction: Direction,
        neighbour_state: BlockState,
        pos: BlockPos,
        neighbour_pos: BlockPos,
        flags: u32,
        limit: i32,
    },
    /// `SimpleNeighborUpdate`.
    Simple { pos: BlockPos, source: BlockId },
    /// `MultiNeighborUpdate`: the neighbours of `source_pos` except `skip`, one per step.
    Multi {
        source_pos: BlockPos,
        source: BlockId,
        skip: Option<Direction>,
        index: usize,
    },
}

impl Update {
    /// A multi-neighbour update starting at the first direction that is not skipped.
    pub(super) fn multi(source_pos: BlockPos, source: BlockId, skip: Option<Direction>) -> Update {
        let index = usize::from(Some(UPDATE_ORDER[0]) == skip);
        Update::Multi {
            source_pos,
            source,
            skip,
            index,
        }
    }
}

/// What one step of an update does.
enum Step {
    Shape {
        direction: Direction,
        neighbour_state: BlockState,
        pos: BlockPos,
        neighbour_pos: BlockPos,
        flags: u32,
        limit: i32,
    },
    Neighbor {
        pos: BlockPos,
        source: BlockId,
    },
}

/// The updater state of a level.
#[derive(Debug)]
pub struct NeighborUpdater {
    max_chained: i32,
    stack: Vec<Update>,
    added_this_layer: Vec<Update>,
    count: i32,
}

impl NeighborUpdater {
    pub(super) fn new(max_chained: i32) -> NeighborUpdater {
        NeighborUpdater {
            max_chained,
            stack: Vec::new(),
            added_this_layer: Vec::new(),
            count: 0,
        }
    }

    pub(super) fn set_max_chained(&mut self, max_chained: i32) {
        self.max_chained = max_chained;
    }
}

impl Level {
    /// `addAndRun(pos, update)`.
    pub(super) fn add_and_run(&mut self, new: Update) {
        let updater = &mut self.updater;
        let running = updater.count > 0;
        let too_many = updater.max_chained >= 0 && updater.count >= updater.max_chained;
        updater.count += 1;
        if !too_many {
            if running {
                updater.added_this_layer.push(new);
            } else {
                updater.stack.push(new);
            }
        } else if updater.count - 1 == updater.max_chained {
            log::error!(
                "Too many chained neighbor updates. Skipping the rest. First skipped position: {:?}",
                first_pos(&new)
            );
        }
        if !running {
            self.run_updates();
        }
    }

    /// `runUpdates()`.
    fn run_updates(&mut self) {
        while !self.updater.stack.is_empty() || !self.updater.added_this_layer.is_empty() {
            // The collected updates go on the stack so that the first added runs first.
            let updater = &mut self.updater;
            while let Some(added) = updater.added_this_layer.pop() {
                updater.stack.push(added);
            }
            while self.updater.added_this_layer.is_empty() {
                let Some(top) = self.updater.stack.last_mut() else {
                    break;
                };
                let (step, more) = next_step(top);
                if !more {
                    self.updater.stack.pop();
                }
                self.run_step(step);
                if !more {
                    break;
                }
            }
        }
        self.updater.stack.clear();
        self.updater.added_this_layer.clear();
        self.updater.count = 0;
    }

    fn run_step(&mut self, step: Option<Step>) {
        match step {
            Some(Step::Shape {
                direction,
                neighbour_state,
                pos,
                neighbour_pos,
                flags,
                limit,
            }) => {
                // `NeighborUpdater.executeShapeUpdate`.
                let current = self.block(pos);
                if flags & update::SKIP_SHAPE_UPDATE_ON_WIRE == 0
                    || !current.block().eq(&ids::REDSTONE_WIRE)
                {
                    let new =
                        self.update_shape(current, pos, direction, neighbour_pos, neighbour_state);
                    self.update_or_destroy(current, new, pos, flags, limit);
                }
            }
            Some(Step::Neighbor { pos, source }) => {
                // `NeighborUpdater.executeUpdate`.
                let state = self.block(pos);
                self.handle_neighbor_changed(state, pos, source, false);
            }
            None => {}
        }
    }
}

/// `runNext`: the next step of `update` and whether more steps follow.
fn next_step(update: &mut Update) -> (Option<Step>, bool) {
    match *update {
        Update::Shape {
            direction,
            neighbour_state,
            pos,
            neighbour_pos,
            flags,
            limit,
        } => (
            Some(Step::Shape {
                direction,
                neighbour_state,
                pos,
                neighbour_pos,
                flags,
                limit,
            }),
            false,
        ),
        Update::Simple { pos, source } => (Some(Step::Neighbor { pos, source }), false),
        Update::Multi {
            source_pos,
            source,
            skip,
            ref mut index,
        } => {
            let direction = UPDATE_ORDER[*index];
            *index += 1;
            if *index < UPDATE_ORDER.len() && Some(UPDATE_ORDER[*index]) == skip {
                *index += 1;
            }
            (
                Some(Step::Neighbor {
                    pos: source_pos.relative(direction),
                    source,
                }),
                *index < UPDATE_ORDER.len(),
            )
        }
    }
}

fn first_pos(update: &Update) -> BlockPos {
    match *update {
        Update::Shape { pos, .. } | Update::Simple { pos, .. } => pos,
        Update::Multi { source_pos, .. } => source_pos,
    }
}

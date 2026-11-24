//! Worlds as the server sees them, their tick context and cross-world hand-offs.

use std::fmt;
use std::sync::Arc;

use vanillite_config::WorldRules;

use crate::compute::Compute;
use crate::global::Global;

/// A world (dimension) of this server: its index in Vanilla's tick order (overworld, nether,
/// end, then others).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WorldId(pub u16);

impl WorldId {
    /// The index into the server's world list.
    pub fn index(self) -> usize {
        usize::from(self.0)
    }
}

/// Work that runs on a world's owner with exclusive access to it.
pub type WorldTask = Box<dyn FnOnce(&mut ServerWorld) + Send + 'static>;

/// One loaded world, owned by exactly one thread at a time: its world thread in parallel mode,
/// the server thread in sequential mode.
///
/// Today worlds are empty and only count ticks. The world runtime (`vanillite-world`) is owned
/// here once it exists; its tick runs in [`ServerWorld::tick`].
pub struct ServerWorld {
    id: WorldId,
    dimension: String,
    rules: Arc<WorldRules>,
    ticks: u64,
}

impl fmt::Debug for ServerWorld {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerWorld")
            .field("id", &self.id)
            .field("dimension", &self.dimension)
            .field("ticks", &self.ticks)
            .finish_non_exhaustive()
    }
}

impl ServerWorld {
    pub(crate) fn new(id: WorldId, dimension: String, rules: Arc<WorldRules>) -> ServerWorld {
        ServerWorld {
            id,
            dimension,
            rules,
            ticks: 0,
        }
    }

    /// This world's id.
    pub fn id(&self) -> WorldId {
        self.id
    }

    /// The dimension key, e.g. `minecraft:overworld`.
    pub fn dimension(&self) -> &str {
        &self.dimension
    }

    /// The rules of this world. Hot paths read plain fields.
    pub fn rules(&self) -> &WorldRules {
        &self.rules
    }

    /// Ticks this world has run.
    pub fn ticks(&self) -> u64 {
        self.ticks
    }

    /// Swaps the rules; called between ticks after a config reload.
    pub(crate) fn set_rules(&mut self, rules: Arc<WorldRules>) {
        self.rules = rules;
    }

    /// One world tick in Vanilla's order (architecture 3.3). Empty worlds only count.
    pub(crate) fn tick(&mut self, ctx: &mut TickContext<'_>) {
        debug_assert_eq!(ctx.world, self.id);
        if ctx.runs_normally {
            self.ticks += 1;
        }
    }

    /// Saves the world. `flush` waits until everything is on disk (shutdown).
    pub(crate) fn save(&mut self, flush: bool) {
        log::debug!("Saving {} (flush: {flush})", self.dimension);
    }
}

/// What a world tick gets besides its world.
pub struct TickContext<'a> {
    /// The world being ticked.
    pub world: WorldId,
    /// The server tick count.
    pub tick: u64,
    /// Whether game elements run this tick (`false` while frozen).
    pub runs_normally: bool,
    /// The compute pool for parallel phases.
    pub compute: &'a Compute,
    /// Server-wide state.
    pub global: &'a Global,
    /// The other worlds, when they are accessible right now (sequential mode).
    others: Option<OtherWorlds<'a>>,
    /// Cross-world work that runs after the world phase.
    outbox: &'a mut Vec<(WorldId, WorldTask)>,
}

struct OtherWorlds<'a> {
    before: &'a mut [ServerWorld],
    after: &'a mut [ServerWorld],
}

impl fmt::Debug for TickContext<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TickContext")
            .field("world", &self.world)
            .field("tick", &self.tick)
            .field("immediate", &self.others.is_some())
            .finish_non_exhaustive()
    }
}

impl<'a> TickContext<'a> {
    pub(crate) fn new(
        world: WorldId,
        tick: u64,
        runs_normally: bool,
        compute: &'a Compute,
        global: &'a Global,
        outbox: &'a mut Vec<(WorldId, WorldTask)>,
    ) -> Self {
        TickContext {
            world,
            tick,
            runs_normally,
            compute,
            global,
            others: None,
            outbox,
        }
    }

    /// Gives the context direct access to the other worlds (sequential mode). `before` holds
    /// the worlds with lower ids, `after` those with higher ids.
    pub(crate) fn with_others(
        mut self,
        before: &'a mut [ServerWorld],
        after: &'a mut [ServerWorld],
    ) -> Self {
        self.others = Some(OtherWorlds { before, after });
        self
    }

    /// Runs `op` on another world: immediately when that world is accessible (sequential
    /// mode, exact Vanilla order), otherwise after the world phase of this tick, before the
    /// target's next tick (parallel mode). This is the one hand-off primitive: entities and
    /// players changing worlds are moved by such an operation into the target's inbox.
    pub fn in_world(
        &mut self,
        target: WorldId,
        op: impl FnOnce(&mut ServerWorld) + Send + 'static,
    ) {
        let current = self.world.index();
        let index = target.index();
        let world = self.others.as_mut().and_then(|others| {
            if index < current {
                others.before.get_mut(index)
            } else {
                index
                    .checked_sub(current + 1)
                    .and_then(|i| others.after.get_mut(i))
            }
        });
        match world {
            Some(world) => op(world),
            None => self.outbox.push((target, Box::new(op))),
        }
    }
}

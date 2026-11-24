//! Worlds as the server sees them, their tick context and cross-world hand-offs.

use std::fmt;
use std::sync::Arc;

use vanillite_base::ChunkPos;
use vanillite_base::hash::FxHashMap;
use vanillite_config::WorldRules;
use vanillite_game::entities::Entities;
use vanillite_world::chunks::{Chunks, TicketId};
use vanillite_world::entity::EntityId;
use vanillite_world::tracking::{EntityTracker, TrackerInput, TrackerOutput, Viewer};

use crate::compute::Compute;
use crate::global::Global;
use crate::players::WorldPlayers;
use crate::world_setup::ChunkPackets;

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
/// Today a world holds its entities and their tracker; chunks follow with the world runtime.
pub struct ServerWorld {
    id: WorldId,
    dimension: String,
    rules: Arc<WorldRules>,
    ticks: u64,
    entities: Entities,
    tracker: Box<dyn EntityTracker>,
    /// The tracker's result of the last tick.
    entity_updates: TrackerOutput,
    viewers: Vec<Viewer>,
    /// Chunks sent to players this tick, for the tracker.
    chunks_sent: Vec<(EntityId, ChunkPos)>,
    pub(crate) players: WorldPlayers,
    chunks: Chunks,
    packets: ChunkPackets,
    /// The chunk ticket of every player: its centre chunk and the ticket.
    player_tickets: FxHashMap<EntityId, (ChunkPos, TicketId)>,
}

impl fmt::Debug for ServerWorld {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerWorld")
            .field("id", &self.id)
            .field("dimension", &self.dimension)
            .field("ticks", &self.ticks)
            .field("entities", &self.entities.store.len())
            .finish_non_exhaustive()
    }
}

impl ServerWorld {
    pub(crate) fn new(
        id: WorldId,
        dimension: String,
        rules: Arc<WorldRules>,
        tracker: Box<dyn EntityTracker>,
        players: WorldPlayers,
        chunks: Chunks,
    ) -> ServerWorld {
        ServerWorld {
            id,
            dimension,
            rules,
            ticks: 0,
            entities: Entities::new(),
            tracker,
            entity_updates: TrackerOutput::default(),
            viewers: Vec::new(),
            chunks_sent: Vec::new(),
            players,
            chunks,
            packets: ChunkPackets::default(),
            player_tickets: FxHashMap::default(),
        }
    }

    /// The chunks of this world.
    pub fn chunks(&self) -> &Chunks {
        &self.chunks
    }

    /// The chunks of this world, mutably (tickets, blocks).
    pub fn chunks_mut(&mut self) -> &mut Chunks {
        &mut self.chunks
    }

    /// Keeps every player's view loaded: one ticket at its centre chunk, moved when the
    /// centre changes, removed when the player is gone.
    fn update_player_tickets(&mut self) {
        let level = self.chunks.player_ticket_level();
        for (id, player) in self.entities.players.iter() {
            let center = player.chunk_view.center;
            match self.player_tickets.get(&id) {
                Some(&(old, _)) if old == center => {}
                _ => {
                    let ticket = self.chunks.add_ticket(center, level);
                    if let Some((_, old)) = self.player_tickets.insert(id, (center, ticket)) {
                        self.chunks.remove_ticket(old);
                    }
                }
            }
        }
        let entities = &self.entities;
        let chunks = &mut self.chunks;
        self.player_tickets.retain(|&id, &mut (_, ticket)| {
            let alive = entities.player(id).is_some();
            if !alive {
                chunks.remove_ticket(ticket);
            }
            alive
        });
    }

    /// The entities of this world.
    pub fn entities(&self) -> &Entities {
        &self.entities
    }

    /// The entities of this world, mutably.
    pub fn entities_mut(&mut self) -> &mut Entities {
        &mut self.entities
    }

    /// What the entity tracker produced at the end of the last tick: encode each message
    /// once and send it to its viewers ([`TrackerOutput::fan_out`]).
    pub fn entity_updates(&self) -> &TrackerOutput {
        &self.entity_updates
    }

    /// Places a player that joined the server into this world.
    pub(crate) fn join_player(&mut self, global: &Global, arrival: crate::players::Arrival) {
        self.players
            .join(&mut self.entities, global, self.id, arrival);
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

    /// One world tick in Vanilla's order (architecture 3.3): the players' packets (Vanilla
    /// handles them between ticks), the chunk source (player tickets, loading, generation,
    /// unloading), the entities in tick order (no behaviours yet), chunk sending, entity
    /// tracking and its updates, then the connection tick.
    pub(crate) fn tick(&mut self, ctx: &mut TickContext<'_>) {
        debug_assert_eq!(ctx.world, self.id);
        self.players.handle_packets(
            &mut self.entities,
            &self.rules,
            ctx.runs_normally,
            ctx.global,
            self.id,
            i64::try_from(self.ticks).unwrap_or(i64::MAX),
        );
        self.update_player_tickets();
        self.chunks.tick();
        self.packets.invalidate(&mut self.chunks);
        if ctx.runs_normally {
            self.ticks += 1;
            let store = &mut self.entities.store;
            let mut cursor = store.tick_cursor();
            while let Some(id) = store.next_to_tick(&mut cursor) {
                if let Some(mut entity) = store.get_mut(id) {
                    entity.set_old_pos();
                }
            }
        }
        self.players.tick_entities(&mut self.entities);
        WorldPlayers::send_chunks(&mut self.entities, &self.packets.source(&self.chunks));
        self.track_entities();
        WorldPlayers::send_entity_updates(&self.entities, &self.entity_updates);
        self.players
            .tick_connections(&mut self.entities, ctx.tick, ctx.global, self.id);
    }

    /// Runs the entity tracker over this tick's state and resets the per-tick sync state.
    fn track_entities(&mut self) {
        self.viewers.clear();
        self.viewers
            .extend(self.entities.players.iter().map(|(entity, player)| Viewer {
                entity,
                view_center: player.chunk_view.center,
                view_distance: player.chunk_view.view_distance,
            }));
        self.entity_updates.clear();
        self.chunks_sent.clear();
        let mut sent = Vec::new();
        for (entity, player) in self.entities.players.iter_mut() {
            player.connection.chunks.take_sent(&mut sent);
            self.chunks_sent
                .extend(sent.drain(..).map(|pos| (entity, pos)));
        }
        let entities = &self.entities;
        let input = TrackerInput {
            entities: &entities.store,
            viewers: &self.viewers,
            chunk_pending: &|player, chunk| WorldPlayers::chunk_pending(entities, player, chunk),
            chunks_sent: &self.chunks_sent,
        };
        self.tracker.update(&input, &mut self.entity_updates);
        self.entities.store.clear_removed();
        self.entities.store.clear_sync_flags();
    }

    /// Saves the world. `flush` waits until everything is on disk (shutdown).
    pub(crate) fn save(&mut self, flush: bool) {
        log::debug!("Saving {} (flush: {flush})", self.dimension);
        self.players.save_all(&self.entities);
        self.chunks.save_all(flush);
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

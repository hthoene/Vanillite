//! The rule catalogue: every deviation from Vanilla and every Vanillite setting, declared once.
//!
//! [`ServerRules`] apply to the whole server, [`WorldRules`] can be overridden per world in
//! `[worlds."<dimension>"]`. Both read their keys from the top level of `vanillite.toml`, so a
//! group key must be unique across both sets. Performance knobs live under `performance.*`.

use crate::rule::{DisabledOr, VanillaOr};
use crate::rules;

rules! {
    /// Rules and settings of server scope.
    pub struct ServerRules {
        /// The tick loop.
        tick: TickRules = "performance.tick" {
            /// Dimensions tick in parallel, each on its own thread (decision D2). Cross-dimension
            /// effects then apply at the end of the tick instead of immediately: an entity
            /// entering a portal from the overworld arrives one tick later, `execute in` other
            /// dimensions runs at the end of the tick. `false` gives exact Vanilla order.
            parallel_dimensions: bool = "parallel-dimensions", Performance, restart = true,
                vanilla = true;
        }
        /// Thread counts.
        threads: ThreadRules = "performance.threads" {
            /// Worker threads of the compute pool (world generation, lighting, saving, parallel
            /// tick phases). 0 = number of cores minus the busy tick threads, at least 1.
            compute: u16 = "compute", Setting, range = [0, 1024], restart = true, vanilla = 0;
        }
        /// Saving.
        autosave: AutosaveRules = "performance.autosave" {
            /// Ticks between autosaves. Vanilla saves every 5 minutes of game time.
            interval: VanillaOr<u32> = "interval", Setting, range = [20, 1_728_000],
                vanilla = VanillaOr::Vanilla;
        }
        /// The watchdog that detects a hanging server. The hard limit is `max-tick-time` in
        /// `server.properties`.
        watchdog: WatchdogRules = "watchdog" {
            /// Seconds without a finished tick after which a first warning is logged.
            early_warning_after: DisabledOr<u32> = "early-warning-after", Setting,
                range = [1, 3600], vanilla = DisabledOr::Value(10);
            /// Seconds between further warnings while the server hangs.
            warning_interval: u32 = "warning-interval", Setting, range = [1, 3600], vanilla = 5;
        }
        /// Limits against abuse.
        security: SecurityRules = "security" {
            /// Logins accepted into the game per tick. Protects the tick from join floods.
            max_joins_per_tick: u32 = "max-joins-per-tick", Security, range = [1, 1000],
                vanilla = 5;
        }
    }
}

rules! {
    /// Rules of one world. Overridable per world in `[worlds."<dimension>"]`.
    pub struct WorldRules {
        /// Deterministic Vanilla mechanics that are commonly called exploits (decision D1).
        /// `true` keeps Vanilla's behaviour.
        exploits: ExploitRules = "exploits" {
            /// TNT can be duplicated with pistons and slime or honey blocks.
            tnt_duplication: bool = "tnt-duplication", Exploit, vanilla = true, paper = false;
            /// Carpets can be duplicated with pistons.
            carpet_duplication: bool = "carpet-duplication", Exploit, vanilla = true, paper = false;
            /// Rails can be duplicated with pistons.
            rail_duplication: bool = "rail-duplication", Exploit, vanilla = true, paper = false;
            /// Pistons without a head can be created.
            headless_pistons: bool = "headless-pistons", Exploit, vanilla = true, paper = false;
            /// Bedrock and end portal frames can be broken with pistons and explosions.
            break_unbreakable_blocks: bool = "break-unbreakable-blocks", Exploit,
                vanilla = true, paper = false;
            /// Falling blocks can be duplicated through end portals.
            gravity_block_duplication: bool = "gravity-block-duplication", Exploit,
                vanilla = true, paper = false;
            /// String can be duplicated with tripwire mechanics.
            string_duplication: bool = "string-duplication", Exploit, vanilla = true, paper = false;
            /// End crystals can be made invulnerable.
            invulnerable_end_crystals: bool = "invulnerable-end-crystals", Exploit,
                vanilla = true, paper = false;
        }
        /// View and simulation distance of this world.
        distances: DistanceRules = "performance.distances" {
            /// View distance in chunks; "vanilla" uses `view-distance` of server.properties.
            view_distance: VanillaOr<u8> = "view-distance", Performance, range = [2, 32],
                vanilla = VanillaOr::Vanilla;
            /// Simulation distance in chunks; "vanilla" uses `simulation-distance` of
            /// server.properties.
            simulation_distance: VanillaOr<u8> = "simulation-distance", Performance,
                range = [2, 32], vanilla = VanillaOr::Vanilla;
        }
        /// Entity tracking: how far away players see entities. Ranges are in blocks and still
        /// capped by the view distance. "vanilla" uses the entity type's own range scaled by
        /// `entity-broadcast-range-percentage`. Entity types are grouped by their mob category:
        /// `monster`, the animal categories (creatures, ambient and water mobs) and `misc`.
        tracking: TrackingRules = "performance.entity-tracking" {
            /// Tracking range of players.
            player_range: VanillaOr<u16> = "player-range", Performance, range = [8, 512],
                restart = true, vanilla = VanillaOr::Vanilla, paper = VanillaOr::Value(128),
                spigot = VanillaOr::Value(128);
            /// Tracking range of animals, ambient and water mobs.
            animal_range: VanillaOr<u16> = "animal-range", Performance, range = [8, 512],
                restart = true, vanilla = VanillaOr::Vanilla, paper = VanillaOr::Value(96),
                spigot = VanillaOr::Value(96);
            /// Tracking range of monsters.
            monster_range: VanillaOr<u16> = "monster-range", Performance, range = [8, 512],
                restart = true, vanilla = VanillaOr::Vanilla, paper = VanillaOr::Value(96),
                spigot = VanillaOr::Value(96);
            /// Tracking range of everything else (items, projectiles, vehicles, decorations).
            misc_range: VanillaOr<u16> = "misc-range", Performance, range = [8, 512],
                restart = true, vanilla = VanillaOr::Vanilla, paper = VanillaOr::Value(96),
                spigot = VanillaOr::Value(96);
        }
        /// Server-side checks of player movement (Vanilla's `ServerGamePacketListenerImpl`).
        movement: MovementRules = "movement" {
            /// Players that move faster than Vanilla allows are teleported back ("moved too
            /// quickly"). Vanilla's `playerMovementCheck` game rule.
            too_fast_check: bool = "too-fast-check", Security, vanilla = true;
            /// The "moved too quickly" check also applies while gliding with an elytra.
            /// Vanilla's `elytraMovementCheck` game rule.
            elytra_too_fast_check: bool = "elytra-too-fast-check", Security, vanilla = true;
            /// Players whose movement does not match the server's collision result are
            /// teleported back ("moved wrongly"). Not enforced yet: it needs block collision
            /// (milestone 3).
            wrong_movement_check: bool = "wrong-movement-check", Security, vanilla = true;
            /// Players floating in the air longer than Vanilla allows are kicked unless
            /// `allow-flight` is on. Not enforced yet: it needs block collision (milestone 3).
            flying_kick: bool = "flying-kick", Security, vanilla = true;
        }
        /// Chunk loading, generation and unloading.
        chunks: ChunkRules = "performance.chunks" {
            /// Ticks a chunk stays loaded after no player or ticket needs it, so walking back
            /// does not load it again.
            unload_delay: u16 = "unload-delay", Performance, range = [0, 6000], vanilla = 100;
            /// Background chunk jobs (loads, generation steps, light) in flight per world. Higher
            /// values load faster with many players at the cost of memory for queued work.
            max_jobs: u16 = "max-jobs", Performance, range = [1, 4096], vanilla = 64;
        }
        /// Entity collisions.
        collisions: CollisionRules = "performance.collisions" {
            /// Entity collisions processed per entity and tick.
            max_entity_collisions: VanillaOr<u32> = "max-entity-collisions", Performance,
                range = [0, 1024], vanilla = VanillaOr::Vanilla, paper = VanillaOr::Value(8);
        }
    }
}

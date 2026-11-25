//! The rule catalogue: every deviation from Vanilla and every Vanillite setting, declared once.
//!
//! [`ServerRules`] apply to the whole server, [`WorldRules`] can be overridden per world in
//! `[worlds."<dimension>"]`. Both read their keys from the top level of `vanillite.toml`, so a
//! group key must be unique across both sets. Performance knobs live under `performance.*`.

use crate::rule::{AntiXrayMode, DisabledOr, StorageLayout, VanillaOr, WorldFormat};
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
            /// tick phases). 0 = number of cores minus one (the tick threads), at least 1.
            compute: u16 = "compute", Setting, range = [0, 1024], restart = true, vanilla = 0;
        }
        /// Saving.
        autosave: AutosaveRules = "performance.autosave" {
            /// Ticks between autosaves. Vanilla saves every 5 minutes of game time.
            interval: VanillaOr<u32> = "interval", Setting, range = [20, 1_728_000],
                vanilla = VanillaOr::Vanilla;
        }
        /// World storage.
        storage: StorageRules = "storage" {
            /// Folder layout of the dimensions: `"vanilla"` keeps every dimension in
            /// `<level>/dimensions/` like Vanilla and Paper; `"spigot"` puts the nether into
            /// `<level>_nether/` and the end into `<level>_the_end/` like Spigot and CraftBukkit,
            /// so the world can be opened by Spigot without conversion. A world found in the
            /// other layout is moved into this one once at startup.
            layout: StorageLayout = "layout", Setting, restart = true,
                vanilla = StorageLayout::Vanilla, spigot = StorageLayout::Spigot;
            /// Format of the chunk, entity and POI files of new worlds: `"vanillite"`, the own
            /// format (about 1.75 × smaller than Anvil, loads and saves several times faster,
            /// `vanillite export` writes a Vanilla copy), or `"anvil"`, Vanilla's region files.
            /// An existing world keeps its format unless `convert-world` is set.
            world_format: WorldFormat = "world-format", Setting, restart = true,
                vanilla = WorldFormat::Vanillite;
            /// Converts an existing world into `world-format` at startup, before it loads.
            /// The new files are read back and compared before the old ones are removed; if
            /// that fails, the world stays as it is and the server starts with it.
            convert_world: bool = "convert-world", Setting, restart = true, vanilla = false;
            /// After a conversion the original files are moved into
            /// `world-format-backup/<time>/` in the server folder instead of being deleted.
            keep_converted_files: bool = "keep-converted-files", Setting, restart = true,
                vanilla = true;
            /// zstd level of chunk records in the own format, 1 (fastest) to 19 (smallest).
            /// Takes effect for chunks saved from then on.
            compression_level: u8 = "compression-level", Performance, range = [1, 19],
                restart = true, vanilla = 3;
            /// Disk IO threads shared by all dimensions. They only read and write files;
            /// compression runs on the compute pool. More help on network volumes with high
            /// latency.
            io_threads: u16 = "io-threads", Performance, range = [1, 16], restart = true,
                vanilla = 2;
            /// Seconds after which saves of unloaded chunks are made durable (synced when
            /// `sync-chunk-writes` is on). Save rounds and `save-all flush` commit at their end.
            commit_interval: u32 = "commit-interval", Performance, range = [5, 300],
                vanilla = 30;
            /// Reads the neighbours of a loaded chunk in its 4 × 4 tile along when they lie
            /// next to it in the file, and keeps them for a short time: fewer disk reads for
            /// moving players.
            read_ahead: bool = "read-ahead", Performance, restart = true, vanilla = true;
            /// MiB of chunk records read ahead kept per dimension.
            read_cache: u32 = "read-cache", Performance, range = [0, 65_536], restart = true,
                vanilla = 64;
            /// Region files kept open per dimension while unused; files in use always stay
            /// open.
            max_open_files: u32 = "max-open-files", Performance, range = [16, 65_536],
                restart = true, vanilla = 256;
            /// MiB of saved chunks waiting for the disk per dimension above which no more
            /// saves are queued: changed chunks stay loaded and save rounds pause until the
            /// disk caught up. Bounds RAM on slow disks.
            max_pending_writes: u32 = "max-pending-writes", Performance, range = [4, 65_536],
                vanilla = 64;
        }
        /// Autosave without tick spikes: a save round captures the changed chunks after the
        /// world tick from the tick's remaining time, spread over as many ticks as needed.
        storage_autosave: StorageAutosaveRules = "storage.autosave" {
            /// Most microseconds of one tick spent capturing chunks for a save round; only the
            /// time left in the tick is used. ~380 chunks fit into the default.
            tick_budget: u32 = "tick-budget", Performance, range = [100, 10_000], vanilla = 500;
            /// Chunks captured per tick even when the tick has no time left, so a round ends
            /// on an overloaded server.
            min_chunks_per_tick: u32 = "min-chunks-per-tick", Performance, range = [1, 1024],
                vanilla = 16;
        }
        /// Automatic backups: zip archives of the world, made in the background from a fresh
        /// save while the server runs. Vanilla has none.
        backup: BackupRules = "backup" {
            /// Minutes between backups; disabled makes none. The first one is made one interval
            /// after the start.
            interval: DisabledOr<u32> = "interval", Setting, range = [1, 525_600],
                vanilla = DisabledOr::Disabled;
            /// Backups kept; older ones are deleted after a new one is complete. 0 keeps all.
            keep: u32 = "keep", Setting, range = [0, 10_000], vanilla = 10;
            /// Folder of the archives, relative to the server folder.
            directory: String = "directory", Setting, vanilla = "backups".to_owned();
            /// Deflate level of the archives, 1 (fast) to 9 (small); 0 stores without
            /// compression. Region files are compressed already, so higher levels gain little.
            compression_level: u8 = "compression-level", Performance, range = [0, 9],
                vanilla = 1;
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
        /// Limits of command parsing and tab completion (SEC-04, SEC-11).
        commands: CommandRules = "commands" {
            /// Longest command line that is parsed (Vanilla: the packet's 32767 characters).
            max_command_length: u32 = "max-command-length", Security, range = [16, 32767],
                vanilla = 32767;
            /// Longest tab completion input; longer requests get no suggestions (Vanilla
            /// accepts 32500 characters).
            max_suggestion_input: u32 = "max-suggestion-input", Security, range = [16, 32500],
                vanilla = 2048;
            /// Suggestions per answer.
            max_suggestions: u32 = "max-suggestions", Security, range = [1, 10000],
                vanilla = 1000;
            /// Characters of a message argument (`say`, `msg`, `kick`).
            max_message_length: u32 = "max-message-length", Security, range = [16, 32767],
                vanilla = 256;
            /// Nesting depth of SNBT in arguments.
            max_nbt_depth: u32 = "max-nbt-depth", Security, range = [1, 512], vanilla = 512;
            /// Nodes of an NBT path.
            max_nbt_path_nodes: u32 = "max-nbt-path-nodes", Security, range = [1, 4096],
                vanilla = 256;
            /// Options inside one entity selector `[...]`.
            max_selector_options: u32 = "max-selector-options", Security, range = [1, 1024],
                vanilla = 64;
            /// Entity selectors inside one message argument.
            max_message_selectors: u32 = "max-message-selectors", Security, range = [0, 256],
                vanilla = 16;
            /// Nested redirects followed while parsing one command.
            max_redirect_depth: u32 = "max-redirect-depth", Security, range = [1, 1024],
                vanilla = 64;
        }
        /// Survival: time and weather.
        survival: SurvivalRules = "performance.survival" {
            /// Ticks between the game time packets every player gets (Vanilla: every second).
            /// Clients run the clocks themselves in between; a longer interval saves traffic
            /// with many players, the game time on the client may then lag behind a little.
            time_sync_interval: u32 = "time-sync-interval", Performance, range = [1, 1200],
                vanilla = 20;
        }
        /// Development and test helpers. Never needed on a production server.
        debug: DebugRules = "debug" {
            /// In-process fake players (`TestBot`, `TestBot2`, ...) that join over virtual
            /// connections, walk in a circle around the spawn and sneak now and then, so a
            /// single tester sees other players. Virtual connections need no authentication,
            /// so keep this at 0 outside test servers.
            test_bots: u8 = "test-bots", Setting, range = [0, 16], restart = true, vanilla = 0;
        }
        /// Player info forwarding from a proxy in front of the server (Velocity "modern"
        /// forwarding, also used by `vanillite-proxy`).
        proxy: ProxyRules = "proxy" {
            /// Shared secret of modern forwarding (the proxy's `forwarding.secret`). Empty turns
            /// forwarding off. With a secret, players' identities and addresses come from the
            /// proxy, signed with it; `online-mode` then only applies to direct connections.
            velocity_secret: String = "velocity-secret", Setting, restart = true,
                vanilla = String::new();
            /// With forwarding on, also accept players who connect directly instead of through
            /// the proxy (authenticated as usual). Off: they are refused.
            allow_direct_connections: bool = "allow-direct-connections", Setting, restart = true,
                vanilla = false;
        }
        /// Clients of older Minecraft versions (`vanillite-multiversion`): their packets are
        /// translated to the server's version and back, like with ViaVersion.
        multiversion: MultiversionRules = "multiversion" {
            /// Older versions whose clients may join: `"all"` (every version this build
            /// translates, currently 1.20.5 to 26.2), `"none"`, or a comma-separated list of
            /// releases such as `"1.21.4, 26.2"` (a release accepts every release of its
            /// protocol). Clients of the server's own version are not translated and pay
            /// nothing.
            versions: String = "versions", Setting, restart = true,
                vanilla = String::from("all");
        }
        /// Native plugins in `plugins/`: packages with a `plugin.toml` and a library built
        /// against the plugin API. Plugins are trusted code with the server's rights.
        plugins: PluginRules = "plugins" {
            /// Load the plugins in `plugins/`.
            enabled: bool = "enabled", Setting, restart = true, vanilla = true;
            /// After a crash inside a plugin's code (a fatal signal or a watchdog kill), skip
            /// that plugin at the next starts until its library changes or `plugins/.crashed`
            /// is removed, so a supervisor (Docker, systemd) brings the server back.
            safe_mode_after_crash: bool = "safe-mode-after-crash", Setting, restart = true,
                vanilla = true;
            /// Development: the console command `vanillite plugin reload <id>` loads a new
            /// build of a plugin at runtime and moves its state. Old builds stay in memory.
            /// Not for production servers.
            development: bool = "development", Setting, restart = true, vanilla = false;
            /// Pinned SHA-256 hashes of plugin libraries, `id=hex,id=hex`. A plugin whose
            /// library has another hash is refused.
            pins: String = "pins", Security, restart = true, vanilla = String::new();
        }
        /// Bedrock Edition players (`vanillite-bedrock`): a RakNet listener on UDP that
        /// translates Bedrock clients into virtual connections. The address is `server-ip`.
        bedrock: BedrockRules = "bedrock" {
            /// Accept Bedrock players.
            enabled: bool = "enabled", Setting, restart = true, vanilla = false;
            /// UDP port. Bedrock's default is 19132.
            port: u16 = "port", Setting, range = [1, 65535], restart = true, vanilla = 19132;
            /// Require an Xbox Live login. Off accepts any identity a client claims: only for
            /// tests on closed networks.
            online_mode: bool = "online-mode", Security, restart = true, vanilla = true;
            /// Oldest Bedrock network protocol that may join. 0 accepts every Bedrock version
            /// this build supports (listed at startup); e.g. 2225 admits only Bedrock 1.26.60
            /// and newer.
            min_protocol: u32 = "min-protocol", Setting, range = [0, 100_000], restart = true,
                vanilla = 0;
            /// Prefix of Bedrock players' Java names, so they never collide with Java accounts.
            /// Bedrock players get the UUID `00000000-0000-0000-xxxx-xxxxxxxxxxxx` of their XUID
            /// (like Floodgate); whitelist and operators use that UUID.
            name_prefix: String = "name-prefix", Setting, restart = true,
                vanilla = String::from(".");
            /// Bedrock connections per IP address.
            max_connections_per_ip: u32 = "max-connections-per-ip", Security,
                range = [1, 1000], restart = true, vanilla = 8;
            /// Packets per second a Bedrock client may send before it is kicked.
            max_packets_per_second: u32 = "max-packets-per-second", Security,
                range = [50, 100_000], restart = true, vanilla = 1000;
            /// Largest chunk radius granted to Bedrock clients.
            max_view_distance: u8 = "max-view-distance", Performance, range = [2, 32],
                restart = true, vanilla = 12;
            /// Translated chunk sections kept for all Bedrock players (each section is
            /// translated once for every viewer). About 4 KiB each.
            chunk_cache_sections: u32 = "chunk-cache-sections", Performance,
                range = [0, 1_048_576], restart = true, vanilla = 16_384;
            /// Threads of the Bedrock frontend.
            threads: u16 = "threads", Performance, range = [1, 64], restart = true, vanilla = 2;
        }
        /// Chat. Spam thresholds are `chat-spam-threshold-seconds` and
        /// `command-spam-threshold-seconds` in `server.properties`.
        chat: ChatRules = "chat" {
            /// Signed messages a player may have received without acknowledging them before it
            /// is kicked (`too_many_pending_chats`).
            max_pending_messages: u32 = "max-pending-messages", Security,
                range = [64, 65536], vanilla = 4096;
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
            /// Overload protection: while a world tick takes longer than this many
            /// milliseconds (smoothed), movement updates of tracked entities (position,
            /// rotation, head) are sent less often, up to `overload-max-interval-scale` times
            /// their Vanilla interval, and back at the Vanilla rate once ticks are fast again.
            /// Spawns, removals and entity data are never delayed. "disabled" keeps Vanilla's
            /// intervals: an overloaded server then slows down like Vanilla.
            overload_tick_millis: DisabledOr<u32> = "overload-tick-millis", Performance,
                range = [1, 1000], vanilla = DisabledOr::Disabled;
            /// Largest factor on the movement update interval under overload.
            overload_max_interval_scale: u32 = "overload-max-interval-scale", Performance,
                range = [1, 20], vanilla = 4;
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
        /// World simulation: scheduled ticks, neighbour updates and light after block changes.
        /// Work over a budget waits for the next tick.
        simulation: SimulationRules = "performance.simulation" {
            /// Scheduled block ticks run per tick.
            max_block_ticks: u32 = "max-block-ticks", Performance, range = [1, 1_000_000],
                vanilla = 65536;
            /// Scheduled fluid ticks run per tick.
            max_fluid_ticks: u32 = "max-fluid-ticks", Performance, range = [1, 1_000_000],
                vanilla = 65536;
            /// Chained neighbour and shape updates of one block change before the rest is
            /// skipped; "vanilla" uses `max-chained-neighbor-updates` of server.properties.
            max_chained_neighbor_updates: VanillaOr<u32> = "max-chained-neighbor-updates",
                Performance, range = [1, 100_000_000], vanilla = VanillaOr::Vanilla;
            /// Chunks whose light is recomputed per tick after block changes, each as one
            /// background job over its 5 × 5 area. Lower values spread bursts over more ticks.
            max_relights_per_tick: u32 = "max-relights-per-tick", Performance,
                range = [1, 65536], vanilla = 1024;
        }
        /// Entity collisions.
        collisions: CollisionRules = "performance.collisions" {
            /// Entity collisions processed per entity and tick.
            max_entity_collisions: VanillaOr<u32> = "max-entity-collisions", Performance,
                range = [0, 1024], vanilla = VanillaOr::Vanilla, paper = VanillaOr::Value(8);
        }
        /// Mobs and animals: AI work per tick. The Vanilla profile keeps every mob exact.
        mobs: MobRules = "performance.mobs" {
            /// Path finding nodes all mobs of the world may visit per tick; searches over the
            /// budget fail and are retried later. 0 = unlimited (Vanilla).
            path_nodes_per_tick: u32 = "path-nodes-per-tick", Performance,
                range = [0, 10_000_000], vanilla = 0;
            /// Mobs farther than this many blocks from every player tick only every 20th tick
            /// (activation range). 0 = off (Vanilla: every mob in simulation distance ticks).
            activation_range: u16 = "activation-range", Performance, range = [0, 512],
                vanilla = 0, paper = 32;
        }
        /// Item entities (dropped items).
        items: ItemRules = "performance.items" {
            /// Ticks an item lies on the ground before it despawns. Lower values keep fewer
            /// item entities around on busy servers.
            despawn_ticks: u16 = "despawn-ticks", Performance, range = [20, 32766],
                vanilla = 6000;
        }
        /// Anti-Xray: ores and other hidden blocks that touch no transparent block are kept from
        /// clients, so x-ray clients and texture packs cannot find them. Chunk packets stay
        /// shared by every player of the world; blocks around a changed block are sent with
        /// their real state.
        anti_xray: AntiXrayRules = "anticheat.anti-xray" {
            /// "disabled" sends the real blocks (Vanilla). "hide" sends hidden blocks as the
            /// dimension's base stone (deepslate below y 0, netherrack, end stone). "obfuscate"
            /// sends hidden and replacement blocks as random hidden blocks (bigger chunk packets).
            engine_mode: AntiXrayMode = "engine-mode", Limit, vanilla = AntiXrayMode::Disabled,
                paper = AntiXrayMode::Hide;
            /// Blocks that are hidden (block ids).
            hidden_blocks: Vec<String> = "hidden-blocks", Limit,
                vanilla = anti_xray_hidden_blocks();
            /// Blocks that "obfuscate" also replaces with random hidden blocks (block ids).
            replacement_blocks: Vec<String> = "replacement-blocks", Limit,
                vanilla = anti_xray_replacement_blocks();
            /// Blocks at and above this height are always sent as they are.
            max_block_height: i32 = "max-block-height", Limit, range = [-2048, 2048],
                vanilla = 64;
            /// Distance in blocks around a changed block (mined, exploded, moved) within which
            /// hidden blocks are sent with their real state.
            update_radius: u8 = "update-radius", Limit, range = [1, 4], vanilla = 2;
        }
    }
}

/// Default of `anticheat.anti-xray.hidden-blocks`: ores, raw ore blocks and blocks that mark
/// dungeons, mineshafts and strongholds, in all three dimensions.
fn anti_xray_hidden_blocks() -> Vec<String> {
    [
        "coal_ore",
        "deepslate_coal_ore",
        "copper_ore",
        "deepslate_copper_ore",
        "raw_copper_block",
        "iron_ore",
        "deepslate_iron_ore",
        "raw_iron_block",
        "gold_ore",
        "deepslate_gold_ore",
        "redstone_ore",
        "deepslate_redstone_ore",
        "lapis_ore",
        "deepslate_lapis_ore",
        "diamond_ore",
        "deepslate_diamond_ore",
        "emerald_ore",
        "deepslate_emerald_ore",
        "nether_gold_ore",
        "nether_quartz_ore",
        "ancient_debris",
        "mossy_cobblestone",
        "obsidian",
        "clay",
        "chest",
        "ender_chest",
    ]
    .map(|id| format!("minecraft:{id}"))
    .to_vec()
}

/// Default of `anticheat.anti-xray.replacement-blocks`: the common blocks around hidden ones.
fn anti_xray_replacement_blocks() -> Vec<String> {
    [
        "stone",
        "deepslate",
        "oak_planks",
        "netherrack",
        "end_stone",
    ]
    .map(|id| format!("minecraft:{id}"))
    .to_vec()
}

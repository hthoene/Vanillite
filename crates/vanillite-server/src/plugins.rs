//! The plugin host wired into the server (architecture 6): the services plugins call, world
//! access for calls on a world's owner, the game hooks, and the block state text form plugins
//! use instead of numeric ids.

use std::collections::HashMap;
use std::sync::{Arc, Weak};

use vanillite_base::BlockPos;
use vanillite_config::{Config, RuleSet};
use vanillite_data::block::{BlockId, BlockState};
use vanillite_game::hooks::GameHooks;
use vanillite_plugin_host::{
    AccessOp, BlockBox, BossBarOp, InteractTarget, Location, NewText, Particles, PlayerInfo,
    PluginHost, ScoreReply, ScoreboardOp, ServerAccess, ServerStats, SetMode, Settings, Sound,
    Text, Title, WorldAccess,
};
use vanillite_protocol::packets::play::clientbound::SystemChat;
use vanillite_protocol::text::Component;

use crate::compute::Compute;
use crate::global::Global;
use crate::player_list::Uuid;
use crate::server::DIMENSIONS;
use crate::world::ServerWorld;

mod server_api;
mod world_api;

/// Priority of plugin background jobs in the compute pool (after urgent world work).
const BACKGROUND_PRIORITY: u32 = 1 << 20;

/// Loads the plugins of `dir/plugins` with the `plugins.*` settings.
pub(crate) fn load(dir: &std::path::Path, config: &Config) -> PluginHost {
    let rules = &config.server.plugins;
    PluginHost::load(
        &dir.join("plugins"),
        Settings {
            enabled: rules.enabled,
            safe_mode_after_crash: rules.safe_mode_after_crash,
            development: rules.development,
            pins: rules.pins.clone(),
            data_version: vanillite_data::version::DATA_VERSION,
        },
    )
}

/// A block state as text: `minecraft:oak_log[axis=y]`.
pub(crate) fn state_text(state: BlockState) -> String {
    let mut text = state.block().name().to_owned();
    let mut properties = state.properties().peekable();
    if properties.peek().is_some() {
        text.push('[');
        for (i, (name, value)) in properties.enumerate() {
            if i > 0 {
                text.push(',');
            }
            text.push_str(name);
            text.push('=');
            text.push_str(value);
        }
        text.push(']');
    }
    text
}

/// Parses a block state text (`stone`, `minecraft:oak_log[axis=x]`); unspecified properties
/// keep their defaults.
pub(crate) fn parse_state(text: &str) -> Option<BlockState> {
    let text = text.trim();
    let (name, properties) = match text.split_once('[') {
        Some((name, rest)) => (name, Some(rest.strip_suffix(']')?)),
        None => (text, None),
    };
    let block = if name.contains(':') {
        BlockId::by_name(name)?
    } else {
        BlockId::by_name(&format!("minecraft:{name}"))?
    };
    let mut state = block.default_state();
    for pair in properties
        .into_iter()
        .flat_map(|p| p.split(','))
        .filter(|p| !p.trim().is_empty())
    {
        let (key, value) = pair.split_once('=')?;
        state = state.with(key.trim(), value.trim())?;
    }
    Some(state)
}

/// The world index of a dimension key.
fn world_index(dimension: &str) -> u32 {
    DIMENSIONS
        .iter()
        .position(|d| *d == dimension)
        .and_then(|i| u32::try_from(i).ok())
        .unwrap_or(0)
}

fn component(text: Text<'_>) -> Component {
    match text {
        Text::Plain(s) => Component::text(s),
        Text::Json(s) => match serde_json::from_str::<serde_json::Value>(s) {
            Ok(v) => Component::from_json(&v),
            Err(_) => Component::text(s),
        },
    }
}

/// The server-wide services plugins call from any thread.
pub(crate) struct Services {
    pub(crate) global: Weak<Global>,
    pub(crate) compute: Compute,
    /// `vanillite.toml` rules as TOML text, by key (server rules and the overworld's).
    pub(crate) rules: HashMap<String, String>,
}

impl Services {
    pub(crate) fn new(global: &Arc<Global>, compute: &Compute, config: &Config) -> Services {
        let mut rules = HashMap::new();
        let server = &config.server;
        for info in vanillite_config::ServerRules::RULES {
            if let Some(v) = server.get(info.key) {
                rules.insert(info.key.to_owned(), v.to_string());
            }
        }
        let world = config.world_rules(DIMENSIONS[0]);
        for info in vanillite_config::WorldRules::RULES {
            if let Some(v) = world.get(info.key) {
                rules.insert(info.key.to_owned(), v.to_string());
            }
        }
        Services {
            global: Arc::downgrade(global),
            compute: compute.clone(),
            rules,
        }
    }
}

impl ServerAccess for Services {
    fn online_players(&self) -> Vec<u128> {
        self.global
            .upgrade()
            .map(|g| g.players.snapshot().iter().map(|p| p.uuid.0).collect())
            .unwrap_or_default()
    }

    fn player_name(&self, player: u128) -> Option<String> {
        let global = self.global.upgrade()?;
        let players = global.players.snapshot();
        players
            .into_iter()
            .find(|p| p.uuid.0 == player)
            .map(|p| p.name)
    }

    fn player_world(&self, player: u128) -> Option<u32> {
        let global = self.global.upgrade()?;
        let players = global.players.snapshot();
        players
            .iter()
            .find(|p| p.uuid.0 == player)
            .map(|p| u32::from(p.world.0))
    }

    fn send_message(&self, player: u128, text: Text<'_>) -> bool {
        let Some(global) = self.global.upgrade() else {
            return false;
        };
        let Some((sink, _)) = global.players.connection(Uuid(player)) else {
            return false;
        };
        sink.send(&SystemChat {
            content: component(text),
            overlay: false,
        });
        true
    }

    fn kick(&self, player: u128, reason: Text<'_>) -> bool {
        let Some(global) = self.global.upgrade() else {
            return false;
        };
        let Some((sink, _)) = global.players.connection(Uuid(player)) else {
            return false;
        };
        let world = self.player_world(player).unwrap_or(0);
        if let Some(reason) = player_kick(&global.plugins, world, player, component(reason)) {
            sink.kick(reason);
        }
        true
    }

    fn set_tab_list(&self, player: u128, header: Text<'_>, footer: Text<'_>) -> bool {
        let Some(global) = self.global.upgrade() else {
            return false;
        };
        let Some((sink, _)) = global.players.connection(Uuid(player)) else {
            return false;
        };
        sink.send(&vanillite_protocol::packets::play::clientbound::TabList {
            header: component(header),
            footer: component(footer),
        });
        true
    }

    fn broadcast(&self, text: Text<'_>) {
        if let Some(global) = self.global.upgrade() {
            let content = component(text);
            log::info!("{}", content.plain());
            global.players.broadcast_system(content, false, None);
        }
    }

    fn is_operator(&self, player: u128) -> bool {
        self.global
            .upgrade()
            .is_some_and(|g| g.access.op_level(player).is_some())
    }

    fn world_name(&self, world: u32) -> Option<String> {
        DIMENSIONS
            .get(usize::try_from(world).ok()?)
            .map(|d| (*d).to_owned())
    }

    fn block_state_id(&self, state: &str) -> Option<u32> {
        parse_state(state).map(|s| u32::from(s.0))
    }

    fn block_state_name(&self, state: u32) -> Option<String> {
        let state = BlockState(u16::try_from(state).ok()?);
        state.is_valid().then(|| state_text(state))
    }

    fn rule(&self, key: &str) -> Option<String> {
        self.rules.get(key).cloned()
    }

    fn run_command(&self, line: &str) {
        if let Some(handle) = self.global.upgrade().and_then(|g| g.handle.get().cloned()) {
            handle.console(line.to_owned());
        }
    }

    fn spawn_background(&self, job: Box<dyn FnOnce() + Send>) {
        self.compute.spawn_background(BACKGROUND_PRIORITY, job);
    }

    fn shutdown(&self, reason: &str) {
        log::error!("Stopping the server: {reason}");
        if let Some(global) = self.global.upgrade() {
            global.stop();
        }
    }

    fn send_action_bar(&self, player: u128, text: Text<'_>) -> bool {
        let Some(global) = self.global.upgrade() else {
            return false;
        };
        let Some((sink, _)) = global.players.connection(Uuid(player)) else {
            return false;
        };
        sink.send(
            &vanillite_protocol::packets::play::clientbound::SetActionBarText {
                text: component(text),
            },
        );
        true
    }

    fn send_title(&self, player: u128, title: &Title<'_>) -> bool {
        use vanillite_protocol::packets::play::clientbound::{
            SetSubtitleText, SetTitleText, SetTitlesAnimation,
        };
        let Some(global) = self.global.upgrade() else {
            return false;
        };
        let Some((sink, _)) = global.players.connection(Uuid(player)) else {
            return false;
        };
        let [fade_in, stay, fade_out] = title.times;
        sink.send(&SetTitlesAnimation {
            fade_in,
            stay,
            fade_out,
        });
        if let Some(subtitle) = title.subtitle {
            sink.send(&SetSubtitleText {
                text: component(subtitle),
            });
        }
        // The title shows the subtitle too: it is sent even when empty.
        sink.send(&SetTitleText {
            text: title.title.map_or_else(Component::empty, component),
        });
        true
    }

    fn play_sound(&self, player: u128, sound: &Sound<'_>) -> bool {
        let Ok(id) = vanillite_protocol::types::Identifier::new(sound.sound) else {
            return false;
        };
        let Some((sink, _)) = self.connection(player) else {
            return false;
        };
        // `(int)(x * 8.0)`: Java's saturating cast.
        #[allow(clippy::cast_possible_truncation, clippy::as_conversions)]
        let eighths = |v: f64| (v * 8.0) as i32;
        let [x, y, z] = sound.pos;
        sink.send(
            &vanillite_protocol::packets::play::clientbound::SoundDirect {
                sound: id,
                fixed_range: None,
                source: crate::commands::sound_source(u8::try_from(sound.source).unwrap_or(0)),
                x: eighths(x),
                y: eighths(y),
                z: eighths(z),
                volume: sound.volume,
                pitch: sound.pitch,
                seed: sound.seed,
            },
        );
        true
    }

    fn stop_sound(&self, player: u128, sound: Option<&str>, source: Option<u32>) -> bool {
        let sound = match sound.map(vanillite_protocol::types::Identifier::new) {
            Some(Ok(id)) => Some(id),
            Some(Err(_)) => return false,
            None => None,
        };
        let Some((sink, _)) = self.connection(player) else {
            return false;
        };
        sink.send(&vanillite_protocol::packets::play::clientbound::StopSound {
            source: source
                .and_then(|s| u8::try_from(s).ok())
                .map(crate::commands::sound_source),
            sound,
        });
        true
    }

    fn spawn_particle(&self, player: u128, particles: &Particles<'_>) -> Result<(), i32> {
        use vanillite_abi::status;
        let particle = crate::commands::particle(particles.particle, Some(&particles.options))
            .map_err(|_| status::INVALID_ARGUMENT)?;
        let (sink, _) = self.connection(player).ok_or(status::NOT_FOUND)?;
        let [x, y, z] = particles.pos;
        #[allow(clippy::cast_possible_truncation, clippy::as_conversions)]
        let [dx, dy, dz] = particles.delta.map(|v| v as f32);
        sink.send(
            &vanillite_protocol::packets::play::clientbound::LevelParticles {
                particle,
                override_limiter: particles.force,
                always_show: false,
                x,
                y,
                z,
                x_dist: dx,
                y_dist: dy,
                z_dist: dz,
                x_max_speed: particles.speed,
                y_max_speed: particles.speed,
                z_max_speed: particles.speed,
                count: particles.count,
                randomization: 0,
            },
        );
        Ok(())
    }

    fn transfer(&self, player: u128, host: &str, port: u16) -> bool {
        let Some((sink, _)) = self.connection(player) else {
            return false;
        };
        sink.send(&vanillite_protocol::packets::play::clientbound::Transfer {
            host: host.to_owned(),
            port: i32::from(port),
        });
        true
    }

    fn send_plugin_message(&self, player: u128, channel: &str, data: &[u8]) -> bool {
        let Ok(channel) = vanillite_protocol::types::Identifier::new(channel) else {
            return false;
        };
        let Some((sink, _)) = self.connection(player) else {
            return false;
        };
        sink.send(
            &vanillite_protocol::packets::play::clientbound::CustomPayload {
                channel,
                data: vanillite_protocol::types::RawBytes(data.to_vec()),
            },
        );
        true
    }

    fn player_info(&self, player: u128) -> Option<PlayerInfo> {
        let global = self.global.upgrade()?;
        let entry = global
            .players
            .snapshot()
            .into_iter()
            .find(|p| p.uuid.0 == player)?;
        Some(PlayerInfo {
            world: u32::from(entry.world.0),
            game_mode: plugin_game_mode(entry.info.game_mode),
            op_level: global.access.op_level(player).map_or(0, u32::from),
            ping: i32::try_from(entry.info.latency).unwrap_or(i32::MAX),
            address: entry.ip,
        })
    }

    fn scoreboard(&self, op: ScoreboardOp<'_>) -> Result<ScoreReply, i32> {
        let global = self
            .global
            .upgrade()
            .ok_or(vanillite_abi::status::WRONG_PHASE)?;
        server_api::scoreboard(&global, op)
    }

    fn boss_bar(&self, op: BossBarOp<'_>) -> Result<(), i32> {
        let global = self
            .global
            .upgrade()
            .ok_or(vanillite_abi::status::WRONG_PHASE)?;
        server_api::boss_bar(&global, op)
    }

    fn stats(&self) -> Option<ServerStats> {
        let global = self.global.upgrade()?;
        let (tick, nanos) = {
            let times = global
                .tick_times
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            (times.ticks(), times.average_nanos())
        };
        let tick_rate = f64::from(global.tick_rate().tick_rate());
        #[allow(clippy::cast_precision_loss)]
        let mspt = nanos as f64 / 1e6;
        let tps = if mspt > 0.0 {
            (1000.0 / mspt).min(tick_rate)
        } else {
            tick_rate
        };
        Some(ServerStats {
            tick,
            mspt,
            tps,
            tick_rate,
            online: u32::try_from(global.players.count()).unwrap_or(u32::MAX),
            max_players: u32::try_from(global.players.max_players()).unwrap_or(u32::MAX),
        })
    }

    fn set_motd(&self, motd: Text<'_>) -> bool {
        let Some(global) = self.global.upgrade() else {
            return false;
        };
        *global
            .motd
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(component(motd));
        true
    }

    fn access(&self, op: AccessOp<'_>) -> Result<bool, i32> {
        let global = self
            .global
            .upgrade()
            .ok_or(vanillite_abi::status::WRONG_PHASE)?;
        server_api::access(&global, op)
    }

    fn world_time(&self, world: u32) -> Option<i64> {
        server_api::world_time(&*self.global.upgrade()?, world)
    }

    fn set_world_time(&self, world: u32, time: i64) -> Result<(), i32> {
        let global = self
            .global
            .upgrade()
            .ok_or(vanillite_abi::status::WRONG_PHASE)?;
        server_api::set_world_time(&global, world, time)
    }

    fn set_weather(&self, _world: u32, weather: u32, duration: i32) -> Result<(), i32> {
        let global = self
            .global
            .upgrade()
            .ok_or(vanillite_abi::status::WRONG_PHASE)?;
        server_api::set_weather(&global, weather, duration)
    }

    fn game_rule(&self, name: &str) -> Option<String> {
        server_api::game_rule(&*self.global.upgrade()?, name)
    }

    fn set_game_rule(&self, name: &str, value: &str) -> Result<(), i32> {
        let global = self
            .global
            .upgrade()
            .ok_or(vanillite_abi::status::WRONG_PHASE)?;
        server_api::set_game_rule(&global, name, value)
    }
}

impl Services {
    /// The connection of an online player.
    fn connection(&self, player: u128) -> Option<crate::player_list::Receiver> {
        self.global.upgrade()?.players.connection(Uuid(player))
    }
}

/// A join or leave message after the plugins: `fire` gets it as JSON and returns a plugin's
/// replacement; an empty replacement shows none.
pub(crate) fn player_message(
    global: &Global,
    message: Component,
    fire: impl FnOnce(&str) -> Option<NewText>,
) -> Option<Component> {
    let wanted = global.plugins.wants(vanillite_abi::event::PLAYER_JOIN)
        || global.plugins.wants(vanillite_abi::event::PLAYER_LEAVE);
    if !wanted {
        return Some(message);
    }
    match fire(&message.to_json_string()) {
        None => Some(message),
        Some(text) if is_empty(&text) => None,
        Some(text) => Some(new_component(&text)),
    }
}

/// Whether a plugin's replacement text is empty (shows nothing).
fn is_empty(text: &NewText) -> bool {
    match text {
        NewText::Plain(s) => s.is_empty(),
        NewText::Json(s) => matches!(s.trim(), "" | "\"\""),
    }
}

/// A plugin's replacement text as a component.
pub(crate) fn new_component(text: &NewText) -> Component {
    match text {
        NewText::Plain(s) => component(Text::Plain(s)),
        NewText::Json(s) => component(Text::Json(s)),
    }
}

/// Bukkit's `PlayerKickEvent` for a kick the server decided (`/kick`, a plugin, idling,
/// spam; not protocol errors and shutdown) of `player` in `world`: `None` when a plugin
/// cancelled it, else the reason, maybe replaced.
pub(crate) fn player_kick(
    host: &PluginHost,
    world: u32,
    player: u128,
    reason: Component,
) -> Option<Component> {
    if !host.wants(vanillite_abi::event::PLAYER_KICK) {
        return Some(reason);
    }
    let text = reason.to_json_string();
    let mut event = vanillite_plugin_host::GameEvent {
        kind: vanillite_abi::event::PLAYER_KICK,
        world,
        entity: player,
        entity_type: "minecraft:player",
        text: &text,
        ..vanillite_plugin_host::GameEvent::default()
    };
    host.game_event(None, &mut event);
    if event.cancelled {
        return None;
    }
    Some(event.text_out.map_or(reason, |t| new_component(&t)))
}

/// The game mode as plugins number it.
pub(crate) fn plugin_game_mode(mode: vanillite_config::GameMode) -> u32 {
    use vanillite_abi::game_mode;
    match mode {
        vanillite_config::GameMode::Survival => game_mode::SURVIVAL,
        vanillite_config::GameMode::Creative => game_mode::CREATIVE,
        vanillite_config::GameMode::Adventure => game_mode::ADVENTURE,
        vanillite_config::GameMode::Spectator => game_mode::SPECTATOR,
    }
}

/// Asks the plugins whether a player's game mode may change; `true` when nobody objects.
pub(crate) fn game_mode_change(
    global: &Global,
    world: u32,
    player: u128,
    from: vanillite_config::GameMode,
    to: vanillite_config::GameMode,
) -> bool {
    !global
        .plugins
        .wants(vanillite_abi::event::PLAYER_GAME_MODE_CHANGE)
        || global.plugins.game_mode_change(
            world,
            player,
            plugin_game_mode(from),
            plugin_game_mode(to),
        )
}

/// The plugins' say on a login the server refused with `refusal` (or let in): the refusal
/// that stands, `None` to let the player in.
pub(crate) fn login(
    global: &Global,
    player: u128,
    name: &str,
    address: &str,
    reason: u32,
    refusal: Option<Component>,
) -> Option<Component> {
    let message = refusal
        .as_ref()
        .map(Component::to_json_string)
        .unwrap_or_default();
    let Some(outcome) = global
        .plugins
        .player_login(player, name, address, reason, &message)
    else {
        return refusal;
    };
    if !outcome.refused {
        return None;
    }
    match outcome.message {
        Some(text) => Some(new_component(&text)),
        None => {
            Some(refusal.unwrap_or_else(|| {
                Component::translatable("multiplayer.disconnect.generic", vec![])
            }))
        }
    }
}

/// A world lent to a plugin call by its owner, with the server's shared state.
pub(crate) struct PluginWorld<'a>(pub(crate) &'a mut ServerWorld, pub(crate) &'a Global);

impl WorldAccess for PluginWorld<'_> {
    fn id(&self) -> u32 {
        u32::from(self.0.id().0)
    }

    fn block(&self, [x, y, z]: [i32; 3]) -> Option<String> {
        let pos = BlockPos::new(x, y, z);
        let level = self.0.level();
        level.is_loaded(pos).then(|| state_text(level.block(pos)))
    }

    fn set_block(&mut self, [x, y, z]: [i32; 3], state: &str) -> Result<(), i32> {
        use vanillite_abi::status;
        let Some(state) = parse_state(state) else {
            return Err(status::INVALID_ARGUMENT);
        };
        let pos = BlockPos::new(x, y, z);
        let level = self.0.level_mut();
        if !level.is_loaded(pos) {
            return Err(status::NOT_FOUND);
        }
        level.set_block(pos, state, vanillite_game::level::update::ALL);
        Ok(())
    }

    fn player_position(&self, player: u128) -> Option<[f64; 3]> {
        let entities = self.0.entities();
        let id = crate::commands::find_player(entities, player)?;
        let pos = entities.store.get(id)?.pos();
        Some([pos.x, pos.y, pos.z])
    }

    fn has_entity(&self, entity: u128) -> bool {
        let entities = self.0.entities();
        crate::commands::find_player(entities, entity).is_some()
    }

    fn entity_data(&self, entity: u128) -> Result<Option<&vanillite_nbt::Compound>, i32> {
        let entities = self.0.entities();
        let id = any_entity(entities, entity).ok_or(vanillite_abi::status::NOT_FOUND)?;
        Ok(entities.persistent_data(id))
    }

    fn entity_data_mut(&mut self, entity: u128) -> Result<&mut vanillite_nbt::Compound, i32> {
        let entities = self.0.entities_mut();
        let id = any_entity(entities, entity).ok_or(vanillite_abi::status::NOT_FOUND)?;
        entities
            .persistent_data_mut(id)
            .ok_or(vanillite_abi::status::NOT_FOUND)
    }

    fn block_id(&self, [x, y, z]: [i32; 3]) -> Option<u32> {
        let pos = BlockPos::new(x, y, z);
        let level = self.0.level();
        level.is_loaded(pos).then(|| u32::from(level.block(pos).0))
    }

    fn set_block_id(
        &mut self,
        [x, y, z]: [i32; 3],
        state: u32,
        mode: SetMode,
    ) -> Result<bool, i32> {
        use vanillite_abi::status;
        let state = plugin_state(state).ok_or(status::INVALID_ARGUMENT)?;
        let pos = BlockPos::new(x, y, z);
        let level = self.0.level_mut();
        if !level.is_loaded(pos) {
            return Err(status::NOT_FOUND);
        }
        Ok(level.set_block(pos, state, set_flags(mode)))
    }

    fn set_blocks(&mut self, area: BlockBox, states: &[u32], mode: SetMode) -> Result<usize, i32> {
        use vanillite_abi::{limits, status};
        if states
            .iter()
            .any(|&s| s != limits::NO_BLOCK && plugin_state(s).is_none())
        {
            return Err(status::INVALID_ARGUMENT);
        }
        let flags = set_flags(mode);
        let level = self.0.level_mut();
        let mut changed = 0;
        for ([x, y, z], &state) in area.positions().zip(states) {
            let pos = BlockPos::new(x, y, z);
            if let Some(state) = plugin_state(state)
                && level.is_loaded(pos)
                && level.set_block(pos, state, flags)
            {
                changed += 1;
            }
        }
        Ok(changed)
    }

    fn player_get(&self, player: u128, prop: u32) -> Result<f64, i32> {
        world_api::player_get(self, player, prop)
    }

    fn player_set(&mut self, player: u128, prop: u32, value: f64) -> Result<(), i32> {
        world_api::player_set(self, player, prop, value)
    }

    fn entity_location(&self, entity: u128) -> Result<Location, i32> {
        world_api::entity_location(self, entity)
    }

    fn entity_type(&self, entity: u128) -> Result<String, i32> {
        world_api::entity_type(self, entity)
    }

    fn teleport(&mut self, entity: u128, to: Location) -> Result<(), i32> {
        world_api::teleport(self, entity, to)
    }

    fn set_velocity(&mut self, entity: u128, velocity: [f64; 3]) -> Result<(), i32> {
        world_api::set_velocity(self, entity, velocity)
    }

    fn spawn_entity(
        &mut self,
        kind: &str,
        pos: [f64; 3],
        nbt: &vanillite_nbt::Compound,
    ) -> Result<u128, i32> {
        world_api::spawn_entity(self, kind, pos, nbt)
    }

    fn remove_entity(&mut self, entity: u128) -> Result<(), i32> {
        world_api::remove_entity(self, entity)
    }

    fn entities_near(&self, center: [f64; 3], radius: f64, kind: Option<&str>) -> Vec<u128> {
        world_api::entities_near(self, center, radius, kind)
    }

    fn entity_nbt(&self, entity: u128) -> Result<vanillite_nbt::Compound, i32> {
        world_api::entity_nbt(self, entity)
    }

    fn merge_entity_nbt(&mut self, entity: u128, nbt: &vanillite_nbt::Compound) -> Result<(), i32> {
        world_api::merge_entity_nbt(self, entity, nbt)
    }

    fn add_effect(
        &mut self,
        entity: u128,
        effect: &str,
        duration: i32,
        amplifier: i32,
        flags: u32,
    ) -> Result<(), i32> {
        world_api::add_effect(self, entity, effect, duration, amplifier, flags)
    }

    fn remove_effect(&mut self, entity: u128, effect: Option<&str>) -> Result<(), i32> {
        world_api::remove_effect(self, entity, effect)
    }

    fn give_item(&mut self, player: u128, item: &vanillite_nbt::Compound) -> Result<(), i32> {
        world_api::give_item(self, player, item)
    }

    fn item(&self, entity: u128, slot: &str) -> Result<vanillite_nbt::Compound, i32> {
        world_api::item(self, entity, slot)
    }

    fn set_item(
        &mut self,
        entity: u128,
        slot: &str,
        item: &vanillite_nbt::Compound,
    ) -> Result<(), i32> {
        world_api::set_item(self, entity, slot, item)
    }

    fn block_entity_nbt(&self, pos: [i32; 3]) -> Result<vanillite_nbt::Compound, i32> {
        world_api::block_entity_nbt(self, pos)
    }

    fn merge_block_entity_nbt(
        &mut self,
        pos: [i32; 3],
        nbt: &vanillite_nbt::Compound,
    ) -> Result<(), i32> {
        world_api::merge_block_entity_nbt(self, pos, nbt)
    }

    fn set_advancement(
        &mut self,
        player: u128,
        advancement: &str,
        criterion: Option<&str>,
        grant: bool,
    ) -> Result<(), i32> {
        world_api::set_advancement(self, player, advancement, criterion, grant)
    }

    fn advancement_done(&self, player: u128, advancement: &str) -> Result<bool, i32> {
        world_api::advancement_done(self, player, advancement)
    }
}

/// An entity of a world by UUID: players by their index, other entities by a scan.
fn any_entity(
    entities: &vanillite_game::entities::Entities,
    uuid: u128,
) -> Option<vanillite_world::entity::EntityId> {
    entities
        .player_by_uuid(uuid)
        .or_else(|| crate::commands::find_uuid(entities, uuid))
}

/// A block state id from a plugin, if it is one.
fn plugin_state(state: u32) -> Option<BlockState> {
    let state = BlockState(u16::try_from(state).ok()?);
    state.is_valid().then_some(state)
}

/// The `setBlock` flags of a plugin's block change.
fn set_flags(mode: SetMode) -> u32 {
    use vanillite_game::level::update;
    match mode {
        SetMode::Update => update::ALL,
        SetMode::NoPhysics => update::CLIENTS | update::KNOWN_SHAPE,
    }
}

/// The game hooks of one world, forwarding to the plugins.
pub(crate) struct Hooks<'a> {
    pub(crate) host: &'a PluginHost,
    pub(crate) world: u32,
}

impl<'a> Hooks<'a> {
    /// The hooks of the world with this dimension key.
    pub(crate) fn of(host: &'a PluginHost, dimension: &str) -> Self {
        Hooks {
            host,
            world: world_index(dimension),
        }
    }
}

impl Hooks<'_> {
    /// A player event about a block; `false` when a plugin cancelled it.
    fn player_block(&self, kind: u32, player: u128, pos: BlockPos, state: BlockState) -> bool {
        !self.host.wants(kind)
            || self.host.player_block(
                kind,
                self.world,
                player,
                [pos.x, pos.y, pos.z],
                &state_text(state),
            )
    }
}

/// The game hooks a level keeps ([`vanillite_game::level::Level::set_hooks`]): the hooks of
/// one world, owning a handle of the plugin host.
#[derive(Debug)]
pub(crate) struct WorldHooks {
    host: PluginHost,
    world: u32,
}

impl WorldHooks {
    pub(crate) fn new(host: &PluginHost, dimension: &str) -> Self {
        WorldHooks {
            host: host.clone(),
            world: world_index(dimension),
        }
    }

    fn hooks(&self) -> Hooks<'_> {
        Hooks {
            host: &self.host,
            world: self.world,
        }
    }
}

impl GameHooks for WorldHooks {
    fn wants(&self, kind: u32) -> bool {
        self.host.wants(kind)
    }

    fn event(&self, event: &mut vanillite_game::hooks::Event) {
        self.hooks().event(event);
    }

    fn entity_damage(
        &self,
        entity: u128,
        entity_type: &str,
        damage_type: &str,
        amount: f32,
        attacker: Option<u128>,
        direct: Option<u128>,
    ) -> Option<f32> {
        self.hooks()
            .entity_damage(entity, entity_type, damage_type, amount, attacker, direct)
    }

    fn block_break(&self, player: u128, pos: BlockPos, state: BlockState) -> bool {
        self.hooks().block_break(player, pos, state)
    }

    fn block_place(&self, player: u128, pos: BlockPos, state: BlockState) -> bool {
        self.hooks().block_place(player, pos, state)
    }

    fn bed_enter(&self, player: u128, pos: BlockPos, state: BlockState) -> bool {
        self.hooks().bed_enter(player, pos, state)
    }

    fn bed_leave(&self, player: u128, pos: BlockPos, state: BlockState) {
        self.hooks().bed_leave(player, pos, state);
    }

    fn spawn_change(&self, player: u128, pos: BlockPos, state: BlockState) -> bool {
        self.hooks().spawn_change(player, pos, state)
    }

    fn player_death(
        &self,
        player: u128,
        damage_type: &str,
        killer: Option<u128>,
        death: &mut vanillite_game::hooks::Death,
    ) {
        self.hooks().player_death(player, damage_type, killer, death);
    }

    fn entity_death(
        &self,
        entity: u128,
        entity_type: &str,
        damage_type: &str,
        killer: Option<u128>,
    ) {
        self.hooks()
            .entity_death(entity, entity_type, damage_type, killer);
    }

    fn player_interact(
        &self,
        player: u128,
        action: u32,
        hand: u32,
        item: &str,
        block: Option<(BlockPos, u32, BlockState)>,
    ) -> bool {
        self.hooks()
            .player_interact(player, action, hand, item, block)
    }
}

fn place(world: u32, p: vanillite_game::hooks::Place) -> Location {
    Location {
        world: p.world.unwrap_or(world),
        pos: p.pos,
        yaw: p.yaw,
        pitch: p.pitch,
    }
}

impl Hooks<'_> {
    /// Fires an event of the kinds since ABI 1.6 without world access; see
    /// [`Hooks::event_in`].
    pub(crate) fn event(&self, event: &mut vanillite_game::hooks::Event) {
        self.event_in(None, event);
    }

    /// Fires an event of the kinds since ABI 1.6; `world` is the event's world when the
    /// caller owns it whole (handlers may then touch it).
    pub(crate) fn event_in(
        &self,
        world: Option<&mut dyn WorldAccess>,
        event: &mut vanillite_game::hooks::Event,
    ) {
        if !self.host.wants(event.kind) {
            return;
        }
        let state = event.state.map(state_text).unwrap_or_default();
        let mut e = vanillite_plugin_host::GameEvent {
            kind: event.kind,
            cancelled: event.cancelled,
            world: self.world,
            entity: event.entity,
            entity_type: &event.entity_type,
            other: event.other,
            other_type: &event.other_type,
            source: event.source,
            player: event.player,
            cause: event.cause,
            key: &event.key,
            pos: [event.pos.x, event.pos.y, event.pos.z],
            state: &state,
            from: place(self.world, event.from),
            to: place(self.world, event.to),
            item: &event.item,
            other_item: &event.other_item,
            text: &event.text,
            amount: event.amount,
            value: event.value,
            extra: event.extra,
            flags: event.flags,
            item_out: None,
            text_out: None,
        };
        self.host.game_event(world, &mut e);
        event.cancelled = e.cancelled;
        event.to = vanillite_game::hooks::Place {
            world: (e.to.world != self.world).then_some(e.to.world),
            pos: e.to.pos,
            yaw: e.to.yaw,
            pitch: e.to.pitch,
        };
        event.amount = e.amount;
        event.value = e.value;
        event.extra = e.extra;
        event.flags = e.flags;
        event.item_out = e.item_out;
        event.text_out = e.text_out.map(|t| match t {
            NewText::Plain(s) => vanillite_game::hooks::NewText::Plain(s),
            NewText::Json(s) => vanillite_game::hooks::NewText::Json(s),
        });
    }
}

impl GameHooks for Hooks<'_> {
    fn wants(&self, kind: u32) -> bool {
        self.host.wants(kind)
    }

    fn event(&self, event: &mut vanillite_game::hooks::Event) {
        self.event_in(None, event);
    }

    fn entity_damage(
        &self,
        entity: u128,
        entity_type: &str,
        damage_type: &str,
        amount: f32,
        attacker: Option<u128>,
        direct: Option<u128>,
    ) -> Option<f32> {
        self.host.entity_damage(
            self.world,
            entity,
            entity_type,
            damage_type,
            amount,
            attacker,
            direct,
        )
    }

    fn block_break(&self, player: u128, pos: BlockPos, state: BlockState) -> bool {
        use vanillite_abi::event;
        !self.host.wants(event::BLOCK_BREAK)
            || self.host.block_change(
                false,
                self.world,
                player,
                [pos.x, pos.y, pos.z],
                &state_text(state),
            )
    }

    fn block_place(&self, player: u128, pos: BlockPos, state: BlockState) -> bool {
        use vanillite_abi::event;
        !self.host.wants(event::BLOCK_PLACE)
            || self.host.block_change(
                true,
                self.world,
                player,
                [pos.x, pos.y, pos.z],
                &state_text(state),
            )
    }

    fn bed_enter(&self, player: u128, pos: BlockPos, state: BlockState) -> bool {
        self.player_block(vanillite_abi::event::PLAYER_BED_ENTER, player, pos, state)
    }

    fn bed_leave(&self, player: u128, pos: BlockPos, state: BlockState) {
        self.player_block(vanillite_abi::event::PLAYER_BED_LEAVE, player, pos, state);
    }

    fn spawn_change(&self, player: u128, pos: BlockPos, state: BlockState) -> bool {
        self.player_block(
            vanillite_abi::event::PLAYER_SPAWN_CHANGE,
            player,
            pos,
            state,
        )
    }

    fn player_death(
        &self,
        player: u128,
        damage_type: &str,
        killer: Option<u128>,
        death: &mut vanillite_game::hooks::Death,
    ) {
        if !self.host.wants(vanillite_abi::event::PLAYER_DEATH) {
            return;
        }
        let message = death
            .message
            .as_ref()
            .map(Component::to_json_string)
            .unwrap_or_default();
        let Some(outcome) = self.host.player_death(
            self.world,
            player,
            damage_type,
            killer,
            &message,
            death.keep_inventory,
        ) else {
            return;
        };
        death.keep_inventory = outcome.keep_inventory;
        if let Some(text) = outcome.message {
            death.message = (!is_empty(&text)).then(|| new_component(&text));
        }
    }

    fn entity_death(
        &self,
        entity: u128,
        entity_type: &str,
        damage_type: &str,
        killer: Option<u128>,
    ) {
        self.host
            .entity_death(self.world, entity, entity_type, damage_type, killer);
    }

    fn player_interact(
        &self,
        player: u128,
        action: u32,
        hand: u32,
        item: &str,
        block: Option<(BlockPos, u32, BlockState)>,
    ) -> bool {
        if !self.host.wants(vanillite_abi::event::PLAYER_INTERACT) {
            return true;
        }
        let state = block.map(|(_, _, state)| state_text(state));
        let target = block.map(|(pos, face, _)| InteractTarget {
            pos: [pos.x, pos.y, pos.z],
            face,
            state: state.as_deref().unwrap_or(""),
        });
        self.host
            .player_interact(self.world, player, action, hand, item, target)
    }
}

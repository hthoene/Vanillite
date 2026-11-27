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
    BlockBox, PluginHost, ServerAccess, SetMode, Settings, Text, WorldAccess,
};
use vanillite_protocol::packets::play::clientbound::SystemChat;
use vanillite_protocol::text::Component;

use crate::compute::Compute;
use crate::global::Global;
use crate::player_list::Uuid;
use crate::server::DIMENSIONS;
use crate::world::ServerWorld;

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
        sink.kick(component(reason));
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
}

/// A world lent to a plugin call by its owner.
pub(crate) struct PluginWorld<'a>(pub(crate) &'a mut ServerWorld);

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

impl GameHooks for Hooks<'_> {
    fn entity_damage(
        &self,
        entity: u128,
        entity_type: &str,
        damage_type: &str,
        amount: f32,
    ) -> Option<f32> {
        self.host
            .entity_damage(self.world, entity, entity_type, damage_type, amount)
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
}

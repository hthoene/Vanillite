//! Registration in `load`: events, commands, the permission provider, providers and config.

use std::ffi::c_void;
use std::marker::PhantomData;

use vanillite_abi::{self as abi, Status, status};

use crate::Plugin;
use crate::command::{self, Command};
use crate::events::{
    AdvancementDone, BlockChange, CustomEvent, EntityDamage, EntityDeath, GameModeChange, Interact,
    Login, PlayerDeath, PlayerJoin, PlayerText, PlayerWorld, PlayersMoved, PluginMessage,
};
use crate::ffi::{Host, guard, str, with_plugin};
use crate::generator::{self, ChunkGeneratorProvider};
use crate::server::{Ctx, Server};
use crate::types::{Error, Uuid};

type Handler<P> = Box<dyn FnMut(&mut P, &mut Ctx<'_>, *mut abi::EventHeader) + Send>;
type PermissionCheck = Box<dyn Fn(Uuid, &str) -> Permission + Send + Sync>;

/// The answer of a permission provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Permission {
    /// Granted.
    Allow,
    /// Denied.
    Deny,
    /// Not decided: operators and the console have it, other players not.
    Undefined,
}

/// Who has a registered permission node when no permission provider decides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionDefault {
    /// Nobody.
    False,
    /// Everyone.
    True,
    /// Operators (the default of unregistered nodes).
    Op,
    /// Everyone but operators.
    NotOp,
}

/// Registers what a plugin provides. Only valid in [`Plugin::load`].
pub struct Registrar<P> {
    host: Host,
    error: Option<Error>,
    /// [`abi::subscribe_flag`] bits for the next subscription.
    next_flags: u32,
    _plugin: PhantomData<fn(P)>,
}

impl<P> std::fmt::Debug for Registrar<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registrar")
            .field("plugin", &self.host.id)
            .finish()
    }
}

impl<P: Plugin> Registrar<P> {
    pub(crate) fn new(host: Host) -> Self {
        Registrar {
            host,
            error: None,
            next_flags: 0,
            _plugin: PhantomData,
        }
    }

    /// The first registration error, as the status `load` returns.
    pub(crate) fn finish(self) -> Status {
        self.error.map_or(status::OK, |e| e.status())
    }

    fn record(&mut self, s: Status) {
        if let Err(e) = Error::check(s)
            && self.error.is_none()
        {
            self.error = Some(e);
        }
    }

    /// Thread-safe server access (config, logging, data folder already work in `load`).
    pub fn server(&self) -> Server {
        Server::from_host(self.host)
    }

    /// Loads the plugin's `config.toml`, writing `defaults` first when it does not exist. Read
    /// values with [`Server::config_string`] and its siblings.
    pub fn config(&mut self, defaults: &str) -> Result<(), Error> {
        // SAFETY: valid host function and string.
        Error::check(unsafe { (self.host.api.config_load)(self.host.id, abi::Str::new(defaults)) })
    }

    fn subscribe(&mut self, kind: u32, priority: i32, handler: Handler<P>) -> &mut Self {
        let user = Box::into_raw(Box::new(handler)).cast::<c_void>();
        let flags = std::mem::take(&mut self.next_flags);
        // SAFETY: valid host functions; `user` stays alive for the life of the library.
        let s = unsafe {
            if flags == 0 {
                (self.host.api.subscribe)(self.host.id, kind, priority, on_event::<P>, user)
            } else {
                (self.host.api.subscribe_flags)(
                    self.host.id,
                    kind,
                    priority,
                    flags,
                    on_event::<P>,
                    user,
                )
            }
        };
        self.record(s);
        self
    }

    /// The next `on_*` handler is skipped while its event is cancelled (Bukkit's
    /// `ignoreCancelled = true`), e.g. `registrar.ignore_cancelled().on_chat(...)`. Since
    /// ABI 1.5.
    pub fn ignore_cancelled(&mut self) -> &mut Self {
        self.next_flags |= abi::subscribe_flag::IGNORE_CANCELLED;
        self
    }

    /// Registers a permission node with who has it when no permission provider decides.
    /// Since ABI 1.5.
    pub fn permission(&mut self, node: &str, default: PermissionDefault) -> &mut Self {
        let default = match default {
            PermissionDefault::False => abi::permission_default::FALSE,
            PermissionDefault::True => abi::permission_default::TRUE,
            PermissionDefault::Op => abi::permission_default::OP,
            PermissionDefault::NotOp => abi::permission_default::NOT_OP,
        };
        // SAFETY: valid host function and string.
        let s = unsafe {
            (self.host.api.permission_register)(self.host.id, abi::Str::new(node), default)
        };
        self.record(s);
        self
    }

    /// Receives the plugin messages players send on `channel` (e.g. `myplugin:sync`). No
    /// world access. Since ABI 1.5.
    pub fn on_plugin_message(
        &mut self,
        channel: &str,
        priority: i32,
        mut f: impl FnMut(&mut P, &mut Ctx<'_>, &PluginMessage<'_>) + Send + 'static,
    ) -> &mut Self {
        // SAFETY: valid host function and string.
        let s = unsafe { (self.host.api.register_channel)(self.host.id, abi::Str::new(channel)) };
        self.record(s);
        let channel = channel.to_owned();
        self.subscribe(
            abi::event::PLAYER_PLUGIN_MESSAGE,
            priority,
            Box::new(move |p, ctx, e| {
                // SAFETY: the host passes a `PluginMessageEvent` for this kind.
                let raw = unsafe { &*e.cast::<abi::PluginMessageEvent>() };
                let message = PluginMessage { raw };
                if message.channel() == channel {
                    f(p, ctx, &message);
                }
            }),
        )
    }

    /// A player completed an advancement. No world access. Since ABI 1.5.
    pub fn on_advancement(
        &mut self,
        priority: i32,
        mut f: impl FnMut(&mut P, &mut Ctx<'_>, &AdvancementDone<'_>) + Send + 'static,
    ) -> &mut Self {
        self.subscribe(
            abi::event::PLAYER_ADVANCEMENT_DONE,
            priority,
            Box::new(move |p, ctx, e| {
                // SAFETY: the host passes an `AdvancementEvent` for this kind.
                let raw = unsafe { &*e.cast::<abi::AdvancementEvent>() };
                f(p, ctx, &AdvancementDone { raw });
            }),
        )
    }

    /// A plugin event named `name` (any plugin may fire it with [`Server::fire_event`]); the
    /// context is the firing plugin's. Since ABI 1.5.
    pub fn on_plugin_event(
        &mut self,
        name: &str,
        priority: i32,
        mut f: impl FnMut(&mut P, &mut Ctx<'_>, &mut CustomEvent<'_>) + Send + 'static,
    ) -> &mut Self {
        let Some(kind) = self.server().event_kind(name) else {
            self.record(status::UNSUPPORTED);
            return self;
        };
        self.subscribe(
            kind,
            priority,
            Box::new(move |p, ctx, e| {
                // SAFETY: the host passes a `CustomEvent` for plugin event kinds.
                let raw = unsafe { &mut *e.cast::<abi::CustomEvent>() };
                f(p, ctx, &mut CustomEvent { raw });
            }),
        )
    }

    /// A player joined (after the join message). The context owns the player's world.
    pub fn on_join(
        &mut self,
        priority: i32,
        mut f: impl FnMut(&mut P, &mut Ctx<'_>, &PlayerJoin<'_>) + Send + 'static,
    ) -> &mut Self {
        self.subscribe(
            abi::event::PLAYER_JOIN,
            priority,
            Box::new(move |p, ctx, e| {
                // SAFETY: the host passes a `PlayerEvent` for this kind.
                let raw = unsafe { &*e.cast::<abi::PlayerEvent>() };
                f(p, ctx, &PlayerJoin { raw });
            }),
        )
    }

    /// A player left.
    pub fn on_leave(
        &mut self,
        priority: i32,
        mut f: impl FnMut(&mut P, &mut Ctx<'_>, &PlayerJoin<'_>) + Send + 'static,
    ) -> &mut Self {
        self.subscribe(
            abi::event::PLAYER_LEAVE,
            priority,
            Box::new(move |p, ctx, e| {
                // SAFETY: the host passes a `PlayerEvent` for this kind.
                let raw = unsafe { &*e.cast::<abi::PlayerEvent>() };
                f(p, ctx, &PlayerJoin { raw });
            }),
        )
    }

    /// A player sends a chat message; cancellable.
    pub fn on_chat(
        &mut self,
        priority: i32,
        mut f: impl FnMut(&mut P, &mut Ctx<'_>, &mut PlayerText<'_>) + Send + 'static,
    ) -> &mut Self {
        self.subscribe(
            abi::event::PLAYER_CHAT,
            priority,
            Box::new(move |p, ctx, e| {
                // SAFETY: the host passes a `PlayerTextEvent` for this kind.
                let raw = unsafe { &mut *e.cast::<abi::PlayerTextEvent>() };
                f(p, ctx, &mut PlayerText { raw });
            }),
        )
    }

    /// A player runs a command (any command, before it is parsed); cancellable.
    pub fn on_command(
        &mut self,
        priority: i32,
        mut f: impl FnMut(&mut P, &mut Ctx<'_>, &mut PlayerText<'_>) + Send + 'static,
    ) -> &mut Self {
        self.subscribe(
            abi::event::PLAYER_COMMAND,
            priority,
            Box::new(move |p, ctx, e| {
                // SAFETY: the host passes a `PlayerTextEvent` for this kind.
                let raw = unsafe { &mut *e.cast::<abi::PlayerTextEvent>() };
                f(p, ctx, &mut PlayerText { raw });
            }),
        )
    }

    /// The players of a world that moved this tick, in one batch per world and tick. The
    /// context owns that world.
    pub fn on_players_moved(
        &mut self,
        priority: i32,
        mut f: impl FnMut(&mut P, &mut Ctx<'_>, &PlayersMoved<'_>) + Send + 'static,
    ) -> &mut Self {
        self.subscribe(
            abi::event::PLAYERS_MOVED,
            priority,
            Box::new(move |p, ctx, e| {
                // SAFETY: the host passes a `PlayersMovedEvent` for this kind.
                let raw = unsafe { &*e.cast::<abi::PlayersMovedEvent>() };
                f(p, ctx, &PlayersMoved { raw });
            }),
        )
    }

    /// An entity is about to take damage; cancellable, the amount can be changed.
    pub fn on_entity_damage(
        &mut self,
        priority: i32,
        mut f: impl FnMut(&mut P, &mut Ctx<'_>, &mut EntityDamage<'_>) + Send + 'static,
    ) -> &mut Self {
        self.subscribe(
            abi::event::ENTITY_DAMAGE,
            priority,
            Box::new(move |p, ctx, e| {
                // SAFETY: the host passes an `EntityDamageEvent` for this kind.
                let raw = unsafe { &mut *e.cast::<abi::EntityDamageEvent>() };
                f(p, ctx, &mut EntityDamage { raw });
            }),
        )
    }

    /// A player is about to break a block; cancellable.
    pub fn on_block_break(
        &mut self,
        priority: i32,
        mut f: impl FnMut(&mut P, &mut Ctx<'_>, &mut BlockChange<'_>) + Send + 'static,
    ) -> &mut Self {
        self.subscribe(
            abi::event::BLOCK_BREAK,
            priority,
            Box::new(move |p, ctx, e| {
                // SAFETY: the host passes a `BlockEvent` for this kind.
                let raw = unsafe { &mut *e.cast::<abi::BlockEvent>() };
                f(p, ctx, &mut BlockChange { raw });
            }),
        )
    }

    /// A player is about to place a block; cancellable.
    pub fn on_block_place(
        &mut self,
        priority: i32,
        mut f: impl FnMut(&mut P, &mut Ctx<'_>, &mut BlockChange<'_>) + Send + 'static,
    ) -> &mut Self {
        self.subscribe(
            abi::event::BLOCK_PLACE,
            priority,
            Box::new(move |p, ctx, e| {
                // SAFETY: the host passes a `BlockEvent` for this kind.
                let raw = unsafe { &mut *e.cast::<abi::BlockEvent>() };
                f(p, ctx, &mut BlockChange { raw });
            }),
        )
    }

    /// A player is about to fall asleep in a bed (the position is the bed's head);
    /// cancellable. Since ABI 1.3.
    pub fn on_bed_enter(
        &mut self,
        priority: i32,
        f: impl FnMut(&mut P, &mut Ctx<'_>, &mut BlockChange<'_>) + Send + 'static,
    ) -> &mut Self {
        self.on_player_block(abi::event::PLAYER_BED_ENTER, priority, f)
    }

    /// A player left its bed (the position is the bed's head). Since ABI 1.3.
    pub fn on_bed_leave(
        &mut self,
        priority: i32,
        f: impl FnMut(&mut P, &mut Ctx<'_>, &mut BlockChange<'_>) + Send + 'static,
    ) -> &mut Self {
        self.on_player_block(abi::event::PLAYER_BED_LEAVE, priority, f)
    }

    /// A bed or respawn anchor is about to become a player's respawn point; cancellable.
    /// Since ABI 1.3.
    pub fn on_spawn_change(
        &mut self,
        priority: i32,
        f: impl FnMut(&mut P, &mut Ctx<'_>, &mut BlockChange<'_>) + Send + 'static,
    ) -> &mut Self {
        self.on_player_block(abi::event::PLAYER_SPAWN_CHANGE, priority, f)
    }

    fn on_player_block(
        &mut self,
        kind: u32,
        priority: i32,
        mut f: impl FnMut(&mut P, &mut Ctx<'_>, &mut BlockChange<'_>) + Send + 'static,
    ) -> &mut Self {
        self.subscribe(
            kind,
            priority,
            Box::new(move |p, ctx, e| {
                // SAFETY: the host passes a `BlockEvent` for this kind.
                let raw = unsafe { &mut *e.cast::<abi::BlockEvent>() };
                f(p, ctx, &mut BlockChange { raw });
            }),
        )
    }

    /// A player logs in, after the server's checks (bans, whitelist, player limit) and
    /// before it joins a world: refuse it or let a refused player in. No world access.
    /// Since ABI 1.4.
    pub fn on_login(
        &mut self,
        priority: i32,
        mut f: impl FnMut(&mut P, &mut Ctx<'_>, &mut Login<'_>) + Send + 'static,
    ) -> &mut Self {
        self.subscribe(
            abi::event::PLAYER_LOGIN,
            priority,
            Box::new(move |p, ctx, e| {
                // SAFETY: the host passes a `LoginEvent` for this kind.
                let raw = unsafe { &mut *e.cast::<abi::LoginEvent>() };
                f(p, ctx, &mut Login { raw });
            }),
        )
    }

    /// A player died: change its death message or let it keep its inventory. No world
    /// access (the world is in the middle of its tick). Since ABI 1.4.
    pub fn on_death(
        &mut self,
        priority: i32,
        mut f: impl FnMut(&mut P, &mut Ctx<'_>, &mut PlayerDeath<'_>) + Send + 'static,
    ) -> &mut Self {
        self.subscribe(
            abi::event::PLAYER_DEATH,
            priority,
            Box::new(move |p, ctx, e| {
                // SAFETY: the host passes a `PlayerDeathEvent` for this kind.
                let raw = unsafe { &mut *e.cast::<abi::PlayerDeathEvent>() };
                f(p, ctx, &mut PlayerDeath { raw });
            }),
        )
    }

    /// A mob died. No world access (the world is in the middle of its tick). Since ABI 1.4.
    pub fn on_entity_death(
        &mut self,
        priority: i32,
        mut f: impl FnMut(&mut P, &mut Ctx<'_>, &EntityDeath<'_>) + Send + 'static,
    ) -> &mut Self {
        self.subscribe(
            abi::event::ENTITY_DEATH,
            priority,
            Box::new(move |p, ctx, e| {
                // SAFETY: the host passes an `EntityDeathEvent` for this kind.
                let raw = unsafe { &*e.cast::<abi::EntityDeathEvent>() };
                f(p, ctx, &EntityDeath { raw });
            }),
        )
    }

    /// A player respawned (after death or the end credits). The context owns its new world.
    /// Since ABI 1.4.
    pub fn on_respawn(
        &mut self,
        priority: i32,
        f: impl FnMut(&mut P, &mut Ctx<'_>, &PlayerWorld<'_>) + Send + 'static,
    ) -> &mut Self {
        self.on_player_world(abi::event::PLAYER_RESPAWN, priority, f)
    }

    /// A player arrived in another world (portal or teleport). The context owns the new
    /// world. Since ABI 1.4.
    pub fn on_change_world(
        &mut self,
        priority: i32,
        f: impl FnMut(&mut P, &mut Ctx<'_>, &PlayerWorld<'_>) + Send + 'static,
    ) -> &mut Self {
        self.on_player_world(abi::event::PLAYER_CHANGE_WORLD, priority, f)
    }

    fn on_player_world(
        &mut self,
        kind: u32,
        priority: i32,
        mut f: impl FnMut(&mut P, &mut Ctx<'_>, &PlayerWorld<'_>) + Send + 'static,
    ) -> &mut Self {
        self.subscribe(
            kind,
            priority,
            Box::new(move |p, ctx, e| {
                // SAFETY: the host passes a `PlayerWorldEvent` for this kind.
                let raw = unsafe { &*e.cast::<abi::PlayerWorldEvent>() };
                f(p, ctx, &PlayerWorld { raw });
            }),
        )
    }

    /// A player's game mode is about to change; cancellable. No world access. Since ABI 1.4.
    pub fn on_game_mode_change(
        &mut self,
        priority: i32,
        mut f: impl FnMut(&mut P, &mut Ctx<'_>, &mut GameModeChange<'_>) + Send + 'static,
    ) -> &mut Self {
        self.subscribe(
            abi::event::PLAYER_GAME_MODE_CHANGE,
            priority,
            Box::new(move |p, ctx, e| {
                // SAFETY: the host passes a `GameModeEvent` for this kind.
                let raw = unsafe { &mut *e.cast::<abi::GameModeEvent>() };
                f(p, ctx, &mut GameModeChange { raw });
            }),
        )
    }

    /// A player uses its item on a block or in the air; cancellable. No world access (the
    /// player's world is busy with its packets). Since ABI 1.4.
    pub fn on_interact(
        &mut self,
        priority: i32,
        mut f: impl FnMut(&mut P, &mut Ctx<'_>, &mut Interact<'_>) + Send + 'static,
    ) -> &mut Self {
        self.subscribe(
            abi::event::PLAYER_INTERACT,
            priority,
            Box::new(move |p, ctx, e| {
                // SAFETY: the host passes an `InteractEvent` for this kind.
                let raw = unsafe { &mut *e.cast::<abi::InteractEvent>() };
                f(p, ctx, &mut Interact { raw });
            }),
        )
    }

    /// Adds a command tree to the server's command tree. Its commands keep their place in the
    /// tree for the life of the server; a development reload replaces their callbacks.
    pub fn command(&mut self, command: Command<P>) -> &mut Self {
        let (nodes, tree) = command::flatten(command);
        let raw: Vec<abi::CommandNode> = nodes
            .iter()
            .map(|(name, node, permission)| abi::CommandNode {
                // SAFETY: the bytes come from a `String`.
                name: abi::Str::new(unsafe { std::str::from_utf8_unchecked(name) }),
                permission: permission.as_deref().map_or(abi::Str::EMPTY, abi::Str::new),
                ..*node
            })
            .collect();
        let user = Box::into_raw(Box::new(tree)).cast::<c_void>();
        let spec = abi::CommandSpec {
            size: abi::size_of::<abi::CommandSpec>(),
            nodes: raw.as_ptr(),
            count: raw.len(),
            stride: size_of::<abi::CommandNode>(),
            execute: command::execute::<P>,
            suggest: Some(command::suggest::<P>),
            user,
        };
        // SAFETY: valid host function; the spec and its strings live for the call (the host
        // copies them), `user` lives for the life of the library.
        let s = unsafe { (self.host.api.register_command)(self.host.id, &spec) };
        self.record(s);
        self
    }

    /// Becomes the server's permission provider. `f` is called from several threads at once.
    pub fn permission_provider(
        &mut self,
        f: impl Fn(Uuid, &str) -> Permission + Send + Sync + 'static,
    ) -> &mut Self {
        let provider: Box<PermissionCheck> = Box::new(Box::new(f));
        let user = Box::into_raw(provider).cast::<c_void>();
        // SAFETY: valid host function; `user` lives for the life of the library.
        let s = unsafe { (self.host.api.set_permission_provider)(self.host.id, permission, user) };
        self.record(s);
        self
    }

    /// Fills the `chunk-generator` slot with `provider`. Its generators work with numeric block
    /// state ids resolved through [`Server::block_state_id`].
    pub fn chunk_generator(&mut self, provider: impl ChunkGeneratorProvider) -> &mut Self {
        let provider: Box<Box<dyn ChunkGeneratorProvider>> = Box::new(Box::new(provider));
        let table = abi::ChunkGeneratorProvider {
            size: abi::size_of::<abi::ChunkGeneratorProvider>(),
            data_version: 0,
            user: Box::into_raw(provider).cast(),
            create: generator::create,
            generate: generator::generate,
            destroy: generator::destroy,
        };
        // SAFETY: valid host function; the host copies the table, `user` lives for the life
        // of the library.
        let s = unsafe { (self.host.api.provide_chunk_generator)(self.host.id, &table) };
        self.record(s);
        self
    }
}

unsafe extern "C" fn on_event<P: Plugin>(
    user: *mut c_void,
    ctx: *mut abi::Context,
    event: *mut abi::EventHeader,
) -> Status {
    guard(|| {
        if event.is_null() {
            return status::INVALID_ARGUMENT;
        }
        let mut ctx = Ctx::new(ctx);
        with_plugin::<P, _>(|p| {
            // SAFETY: `user` is the leaked handler of `subscribe`; calls are serialized by the
            // plugin lock held here.
            let handler = unsafe { &mut *user.cast::<Handler<P>>() };
            handler(p, &mut ctx, event);
        })
        .map_or(status::WRONG_PHASE, |()| status::OK)
    })
}

unsafe extern "C" fn permission(
    user: *mut c_void,
    player: abi::Uuid,
    node: abi::Str,
    out: *mut u32,
) -> Status {
    guard(|| {
        if out.is_null() {
            return status::INVALID_ARGUMENT;
        }
        // SAFETY: `user` is the leaked provider of `permission_provider`; the node is valid
        // for the call.
        let (f, node) = unsafe { (&*user.cast::<PermissionCheck>(), str(node)) };
        let answer = match f(Uuid::from_abi(player), node) {
            Permission::Allow => abi::permission::ALLOW,
            Permission::Deny => abi::permission::DENY,
            Permission::Undefined => abi::permission::UNDEFINED,
        };
        // SAFETY: checked above.
        unsafe { *out = answer };
        status::OK
    })
}

//! Event views: borrowed for the handler call, with accessors over the host's records.

use vanillite_abi as abi;

use crate::ffi::str;
use crate::types::{BlockPos, GameMode, Hand, Text, Uuid, Vec3, World};

/// A player joined or left. With join handlers the join message goes out after them.
#[derive(Debug)]
pub struct PlayerJoin<'a> {
    pub(crate) raw: &'a abi::PlayerEvent,
}

/// A player left. Same view as [`PlayerJoin`].
pub type PlayerLeave<'a> = PlayerJoin<'a>;

impl PlayerJoin<'_> {
    /// The player.
    pub fn player(&self) -> Uuid {
        Uuid::from_abi(self.raw.player)
    }

    /// Its name.
    pub fn name(&self) -> &str {
        // SAFETY: the host's strings are valid for the call.
        unsafe { str(self.raw.name) }
    }

    /// Its world.
    pub fn world(&self) -> World {
        World(self.raw.world)
    }

    /// The join or leave message as a JSON text component. Since ABI 1.5.
    pub fn message(&self) -> &str {
        // SAFETY: the host's strings are valid for the call.
        unsafe { str(self.raw.message) }
    }

    /// Replaces the join or leave message; an empty text shows none. Since ABI 1.5.
    pub fn set_message(&self, message: impl Into<Text>) {
        set_text(self.raw.message_out, &message.into());
    }
}

/// A plugin message a player sent on a registered channel. Since ABI 1.5.
#[derive(Debug)]
pub struct PluginMessage<'a> {
    pub(crate) raw: &'a abi::PluginMessageEvent,
}

impl PluginMessage<'_> {
    /// The player.
    pub fn player(&self) -> Uuid {
        Uuid::from_abi(self.raw.player)
    }

    /// Its world.
    pub fn world(&self) -> World {
        World(self.raw.world)
    }

    /// The channel.
    pub fn channel(&self) -> &str {
        // SAFETY: the host's strings are valid for the call.
        unsafe { str(self.raw.channel) }
    }

    /// The payload.
    pub fn data(&self) -> &[u8] {
        bytes(self.raw.data)
    }
}

/// A player completed an advancement. Since ABI 1.5.
#[derive(Debug)]
pub struct AdvancementDone<'a> {
    pub(crate) raw: &'a abi::AdvancementEvent,
}

impl AdvancementDone<'_> {
    /// The player.
    pub fn player(&self) -> Uuid {
        Uuid::from_abi(self.raw.player)
    }

    /// Its world.
    pub fn world(&self) -> World {
        World(self.raw.world)
    }

    /// The advancement, e.g. `minecraft:story/mine_stone`.
    pub fn advancement(&self) -> &str {
        // SAFETY: the host's strings are valid for the call.
        unsafe { str(self.raw.advancement) }
    }
}

/// A plugin event ([`Server::fire_event`](crate::Server::fire_event)); cancellable. Since
/// ABI 1.5.
#[derive(Debug)]
pub struct CustomEvent<'a> {
    pub(crate) raw: &'a mut abi::CustomEvent,
}

impl CustomEvent<'_> {
    /// The event's name.
    pub fn name(&self) -> &str {
        // SAFETY: the host's strings are valid for the call.
        unsafe { str(self.raw.name) }
    }

    /// The id of the plugin that fired it.
    pub fn source(&self) -> &str {
        // SAFETY: the host's strings are valid for the call.
        unsafe { str(self.raw.source) }
    }

    /// The payload.
    pub fn data(&self) -> &[u8] {
        bytes(self.raw.data)
    }

    /// Whether a handler cancelled it.
    pub fn is_cancelled(&self) -> bool {
        self.raw.header.cancelled != 0
    }

    /// Cancels it or takes the cancellation back.
    pub fn set_cancelled(&mut self, cancelled: bool) {
        self.raw.header.cancelled = u32::from(cancelled);
    }
}

/// Bytes of a host record, valid for the handler call.
fn bytes<'a>(b: abi::Bytes) -> &'a [u8] {
    if b.len == 0 || b.ptr.is_null() {
        return &[];
    }
    // SAFETY: the host's bytes are valid for the call.
    unsafe { std::slice::from_raw_parts(b.ptr, b.len) }
}

/// A chat message or a command line of a player; cancellable.
#[derive(Debug)]
pub struct PlayerText<'a> {
    pub(crate) raw: &'a mut abi::PlayerTextEvent,
}

impl PlayerText<'_> {
    /// The player.
    pub fn player(&self) -> Uuid {
        Uuid::from_abi(self.raw.player)
    }

    /// Its name.
    pub fn name(&self) -> &str {
        // SAFETY: the host's strings are valid for the call.
        unsafe { str(self.raw.name) }
    }

    /// The message, or the command line without `/`.
    pub fn text(&self) -> &str {
        // SAFETY: the host's strings are valid for the call.
        unsafe { str(self.raw.text) }
    }

    /// Whether an earlier handler cancelled it.
    pub fn is_cancelled(&self) -> bool {
        self.raw.header.cancelled != 0
    }

    /// Cancels (or un-cancels) it: the message is not sent, the command not run.
    pub fn set_cancelled(&mut self, cancelled: bool) {
        self.raw.header.cancelled = u32::from(cancelled);
    }
}

/// All players of a world that moved in one tick (batched: one call per world and tick).
#[derive(Debug)]
pub struct PlayersMoved<'a> {
    pub(crate) raw: &'a abi::PlayersMovedEvent,
}

impl PlayersMoved<'_> {
    /// The world.
    pub fn world(&self) -> World {
        World(self.raw.world)
    }

    /// The moves: player, position at the start of the tick, position now.
    pub fn moves(&self) -> impl Iterator<Item = (Uuid, Vec3, Vec3)> + '_ {
        let raw = self.raw;
        (0..raw.count).map(move |i| {
            // SAFETY: the host passes `count` records `stride` bytes apart, valid for the call;
            // the SDK's record is a prefix of the host's.
            let m = unsafe {
                &*raw
                    .moves
                    .cast::<u8>()
                    .add(i * raw.stride)
                    .cast::<abi::PlayerMove>()
            };
            (
                Uuid::from_abi(m.player),
                Vec3::from_abi(m.from),
                Vec3::from_abi(m.to),
            )
        })
    }
}

/// An entity is about to take damage; cancellable, the amount can be changed.
#[derive(Debug)]
pub struct EntityDamage<'a> {
    pub(crate) raw: &'a mut abi::EntityDamageEvent,
}

impl EntityDamage<'_> {
    /// The world.
    pub fn world(&self) -> World {
        World(self.raw.world)
    }

    /// The entity.
    pub fn entity(&self) -> Uuid {
        Uuid::from_abi(self.raw.entity)
    }

    /// Its type, e.g. `minecraft:player`.
    pub fn entity_type(&self) -> &str {
        // SAFETY: the host's strings are valid for the call.
        unsafe { str(self.raw.entity_type) }
    }

    /// The damage type, e.g. `minecraft:fall`.
    pub fn damage_type(&self) -> &str {
        // SAFETY: the host's strings are valid for the call.
        unsafe { str(self.raw.damage_type) }
    }

    /// The damage.
    pub fn amount(&self) -> f32 {
        self.raw.amount
    }

    /// Changes the damage.
    pub fn set_amount(&mut self, amount: f32) {
        self.raw.amount = amount;
    }

    /// Whether an earlier handler cancelled it.
    pub fn is_cancelled(&self) -> bool {
        self.raw.header.cancelled != 0
    }

    /// Cancels (or un-cancels) the damage.
    pub fn set_cancelled(&mut self, cancelled: bool) {
        self.raw.header.cancelled = u32::from(cancelled);
    }
}

/// A player and a block: breaking or placing it, entering or leaving a bed, a new respawn
/// point; cancellable except leaving a bed.
#[derive(Debug)]
pub struct BlockChange<'a> {
    pub(crate) raw: &'a mut abi::BlockEvent,
}

impl BlockChange<'_> {
    /// The world.
    pub fn world(&self) -> World {
        World(self.raw.world)
    }

    /// The player.
    pub fn player(&self) -> Uuid {
        Uuid::from_abi(self.raw.player)
    }

    /// The block.
    pub fn pos(&self) -> BlockPos {
        BlockPos::from_abi(self.raw.pos)
    }

    /// The state broken or to be placed (the bed or anchor), e.g. `minecraft:oak_log[axis=y]`.
    pub fn state(&self) -> &str {
        // SAFETY: the host's strings are valid for the call.
        unsafe { str(self.raw.state) }
    }

    /// Whether an earlier handler cancelled it.
    pub fn is_cancelled(&self) -> bool {
        self.raw.header.cancelled != 0
    }

    /// Cancels (or un-cancels) it.
    pub fn set_cancelled(&mut self, cancelled: bool) {
        self.raw.header.cancelled = u32::from(cancelled);
    }
}

fn killer(u: abi::Uuid) -> Option<Uuid> {
    let u = Uuid::from_abi(u);
    (u.0 != 0).then_some(u)
}

/// Writes a replacement text through the host's text output.
fn set_text(out: *const abi::TextOut, text: &Text) {
    // SAFETY: the host passes a valid output record for the handler call.
    if let Some(out) = unsafe { out.as_ref() } {
        let (s, format) = text.parts();
        // SAFETY: the host copies the text before it returns.
        unsafe { (out.set)(out.state, abi::Str::new(s), format) };
    }
}

/// Why the server refused a login.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginReason {
    /// Not refused.
    Allowed,
    /// Banned (name or address).
    Banned,
    /// Not on the whitelist.
    NotWhitelisted,
    /// The server is full.
    Full,
    /// Refused by a plugin.
    Plugin,
}

/// A player logs in, after the server's checks and before it joins a world. Runs on the
/// server thread without world access.
#[derive(Debug)]
pub struct Login<'a> {
    pub(crate) raw: &'a mut abi::LoginEvent,
}

impl Login<'_> {
    /// The player.
    pub fn player(&self) -> Uuid {
        Uuid::from_abi(self.raw.player)
    }

    /// Its name.
    pub fn name(&self) -> &str {
        // SAFETY: the host's strings are valid for the call.
        unsafe { str(self.raw.name) }
    }

    /// Its IP address.
    pub fn address(&self) -> &str {
        // SAFETY: the host's strings are valid for the call.
        unsafe { str(self.raw.address) }
    }

    /// Why the server refused it.
    pub fn reason(&self) -> LoginReason {
        match self.raw.reason {
            abi::login_reason::ALLOWED => LoginReason::Allowed,
            abi::login_reason::BANNED => LoginReason::Banned,
            abi::login_reason::NOT_WHITELISTED => LoginReason::NotWhitelisted,
            abi::login_reason::FULL => LoginReason::Full,
            _ => LoginReason::Plugin,
        }
    }

    /// The server's refusal message as a JSON text component; empty when it was not refused.
    pub fn message(&self) -> &str {
        // SAFETY: the host's strings are valid for the call.
        unsafe { str(self.raw.message) }
    }

    /// Whether the login is refused (by the server or an earlier handler).
    pub fn is_refused(&self) -> bool {
        self.raw.header.cancelled != 0
    }

    /// Refuses the login with a message.
    pub fn refuse(&mut self, message: impl Into<Text>) {
        self.raw.header.cancelled = 1;
        set_text(self.raw.message_out, &message.into());
    }

    /// Lets the player in, even when the server refused it (e.g. a full server).
    pub fn allow(&mut self) {
        self.raw.header.cancelled = 0;
    }
}

/// A player died. The death message and whether it keeps its inventory can be changed.
#[derive(Debug)]
pub struct PlayerDeath<'a> {
    pub(crate) raw: &'a mut abi::PlayerDeathEvent,
}

impl PlayerDeath<'_> {
    /// The world.
    pub fn world(&self) -> World {
        World(self.raw.world)
    }

    /// The player.
    pub fn player(&self) -> Uuid {
        Uuid::from_abi(self.raw.player)
    }

    /// The damage type that killed it, e.g. `minecraft:fall`.
    pub fn damage_type(&self) -> &str {
        // SAFETY: the host's strings are valid for the call.
        unsafe { str(self.raw.damage_type) }
    }

    /// The entity responsible.
    pub fn killer(&self) -> Option<Uuid> {
        killer(self.raw.killer)
    }

    /// The server's death message as a JSON text component; empty when none is shown.
    pub fn message(&self) -> &str {
        // SAFETY: the host's strings are valid for the call.
        unsafe { str(self.raw.message) }
    }

    /// Replaces the death message; an empty text shows none.
    pub fn set_message(&mut self, message: impl Into<Text>) {
        set_text(self.raw.message_out, &message.into());
    }

    /// Whether the player keeps its inventory and experience.
    pub fn keep_inventory(&self) -> bool {
        self.raw.keep_inventory != 0
    }

    /// Lets the player keep (or drop) its inventory and experience.
    pub fn set_keep_inventory(&mut self, keep: bool) {
        self.raw.keep_inventory = u32::from(keep);
    }
}

/// A mob died.
#[derive(Debug)]
pub struct EntityDeath<'a> {
    pub(crate) raw: &'a abi::EntityDeathEvent,
}

impl EntityDeath<'_> {
    /// The world.
    pub fn world(&self) -> World {
        World(self.raw.world)
    }

    /// The entity.
    pub fn entity(&self) -> Uuid {
        Uuid::from_abi(self.raw.entity)
    }

    /// Its type, e.g. `minecraft:zombie`.
    pub fn entity_type(&self) -> &str {
        // SAFETY: the host's strings are valid for the call.
        unsafe { str(self.raw.entity_type) }
    }

    /// The damage type that killed it.
    pub fn damage_type(&self) -> &str {
        // SAFETY: the host's strings are valid for the call.
        unsafe { str(self.raw.damage_type) }
    }

    /// The entity responsible.
    pub fn killer(&self) -> Option<Uuid> {
        killer(self.raw.killer)
    }
}

/// A player respawned or arrived from another world.
#[derive(Debug)]
pub struct PlayerWorld<'a> {
    pub(crate) raw: &'a abi::PlayerWorldEvent,
}

impl PlayerWorld<'_> {
    /// The player.
    pub fn player(&self) -> Uuid {
        Uuid::from_abi(self.raw.player)
    }

    /// The world it came from.
    pub fn from(&self) -> World {
        World(self.raw.from)
    }

    /// The world it is in now (the context owns it).
    pub fn world(&self) -> World {
        World(self.raw.world)
    }

    /// Its position there.
    pub fn pos(&self) -> Vec3 {
        Vec3::from_abi(self.raw.pos)
    }
}

/// A player's game mode is about to change; cancellable.
#[derive(Debug)]
pub struct GameModeChange<'a> {
    pub(crate) raw: &'a mut abi::GameModeEvent,
}

impl GameModeChange<'_> {
    /// The player's world.
    pub fn world(&self) -> World {
        World(self.raw.world)
    }

    /// The player.
    pub fn player(&self) -> Uuid {
        Uuid::from_abi(self.raw.player)
    }

    /// Its game mode.
    pub fn from(&self) -> GameMode {
        GameMode::from_abi(self.raw.from)
    }

    /// The new game mode.
    pub fn to(&self) -> GameMode {
        GameMode::from_abi(self.raw.to)
    }

    /// Whether an earlier handler cancelled it.
    pub fn is_cancelled(&self) -> bool {
        self.raw.header.cancelled != 0
    }

    /// Cancels (or un-cancels) the change.
    pub fn set_cancelled(&mut self, cancelled: bool) {
        self.raw.header.cancelled = u32::from(cancelled);
    }
}

/// A player uses its item on a block or in the air; cancellable.
#[derive(Debug)]
pub struct Interact<'a> {
    pub(crate) raw: &'a mut abi::InteractEvent,
}

impl Interact<'_> {
    /// The world.
    pub fn world(&self) -> World {
        World(self.raw.world)
    }

    /// The player.
    pub fn player(&self) -> Uuid {
        Uuid::from_abi(self.raw.player)
    }

    /// The hand.
    pub fn hand(&self) -> Hand {
        if self.raw.hand == abi::hand::OFF {
            Hand::Off
        } else {
            Hand::Main
        }
    }

    /// The item in that hand, e.g. `minecraft:flint_and_steel` (`minecraft:air` for none).
    pub fn item(&self) -> &str {
        // SAFETY: the host's strings are valid for the call.
        unsafe { str(self.raw.item) }
    }

    /// The block used, its clicked face (0 down, 1 up, 2 north, 3 south, 4 west, 5 east) and
    /// its state; `None` for a use in the air.
    pub fn block(&self) -> Option<(BlockPos, u32, &str)> {
        (self.raw.action == abi::interact::USE_ON_BLOCK).then(|| {
            (
                BlockPos::from_abi(self.raw.pos),
                self.raw.face,
                // SAFETY: the host's strings are valid for the call.
                unsafe { str(self.raw.state) },
            )
        })
    }

    /// Whether an earlier handler cancelled it.
    pub fn is_cancelled(&self) -> bool {
        self.raw.header.cancelled != 0
    }

    /// Cancels (or un-cancels) the use: nothing happens and the client is corrected.
    pub fn set_cancelled(&mut self, cancelled: bool) {
        self.raw.header.cancelled = u32::from(cancelled);
    }
}

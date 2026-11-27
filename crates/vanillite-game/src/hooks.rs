//! Hooks the server installs into the game so plugins can observe and change game events
//! (block breaking and placing, damage, deaths, item use). Each method has a no-op default; the server checks
//! for listeners before it does any work, so an unused hook costs one virtual call.
//!
//! The event kinds since plugin ABI 1.6 go through one generic [`Event`]: a hook site asks
//! [`Level::hooks_for`](crate::level::Level::hooks_for) (no plugins: a `None` check; with
//! plugins: one bit of the subscription mask), builds the event only then, fires it with
//! [`GameHooks::event`] and applies what the kind allows.

use vanillite_base::BlockPos;
use vanillite_data::block::BlockState;
use vanillite_protocol::text::Component;

/// What a player's death does that hooks may change.
#[derive(Clone, Debug, PartialEq)]
pub struct Death {
    /// The death message; `None` shows none.
    pub message: Option<Component>,
    /// The player keeps its inventory and experience.
    pub keep_inventory: bool,
}

/// A place in a world for [`Event::from`] and [`Event::to`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Place {
    /// The world index; `None` for the hooks' own world.
    pub world: Option<u32>,
    /// The position.
    pub pos: [f64; 3],
    /// Yaw in degrees.
    pub yaw: f32,
    /// Pitch in degrees.
    pub pitch: f32,
}

impl Place {
    /// A position in the hooks' own world.
    pub fn at(pos: vanillite_base::Vec3) -> Place {
        Place {
            world: None,
            pos: [pos.x, pos.y, pos.z],
            yaw: 0.0,
            pitch: 0.0,
        }
    }

    /// A position with a rotation in the hooks' own world.
    pub fn looking(pos: vanillite_base::Vec3, yaw: f32, pitch: f32) -> Place {
        Place {
            yaw,
            pitch,
            ..Place::at(pos)
        }
    }

    /// The position.
    pub fn vec(&self) -> vanillite_base::Vec3 {
        vanillite_base::Vec3::new(self.pos[0], self.pos[1], self.pos[2])
    }
}

/// A text a handler put in place of the event's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NewText {
    /// Plain text.
    Plain(String),
    /// A JSON text component.
    Json(String),
}

impl NewText {
    /// As a text component (bad JSON becomes plain text).
    pub fn component(&self) -> Component {
        match self {
            NewText::Plain(s) => Component::text(s),
            NewText::Json(s) => serde_json::from_str::<serde_json::Value>(s)
                .map(|v| Component::from_json(&v))
                .unwrap_or_else(|_| Component::text(s)),
        }
    }

    /// The text as given.
    pub fn as_str(&self) -> &str {
        match self {
            NewText::Plain(s) | NewText::Json(s) => s,
        }
    }
}

/// A plugin event of the kinds since ABI 1.6 (`vanillite_abi::event`, record `GameEvent`).
/// The hook site fills what the kind documents; after [`GameHooks::event`] the fields hold the
/// handlers' changes (`cancelled`, `to`, `amount`, `value`, `extra`, `flags`, `item_out`,
/// `text_out`) and the site applies what the kind allows.
#[derive(Clone, Debug, Default)]
pub struct Event {
    /// The kind (`vanillite_abi::event`).
    pub kind: u32,
    /// Cancelled.
    pub cancelled: bool,
    /// The entity the event is about (0 for none).
    pub entity: u128,
    /// Its type, e.g. `minecraft:zombie`.
    pub entity_type: String,
    /// A second entity (0 for none).
    pub other: u128,
    /// Its type.
    pub other_type: String,
    /// A third entity (0 for none).
    pub source: u128,
    /// The player behind it (0 for none).
    pub player: u128,
    /// A code of the kind.
    pub cause: u32,
    /// An identifier of the kind.
    pub key: String,
    /// A block.
    pub pos: BlockPos,
    /// A block state.
    pub state: Option<BlockState>,
    /// Where from.
    pub from: Place,
    /// Where to.
    pub to: Place,
    /// An item or a list of items as SNBT ([`item_snbt`], [`items_snbt`]).
    pub item: String,
    /// A second item as SNBT.
    pub other_item: String,
    /// A text.
    pub text: String,
    /// A number.
    pub amount: f64,
    /// An integer.
    pub value: i64,
    /// A second integer.
    pub extra: i64,
    /// Flags.
    pub flags: u32,
    /// Out: the item SNBT a handler put in place of `item` ([`parse_item`], [`parse_items`]).
    pub item_out: Option<String>,
    /// Out: the text a handler put in place of `text`.
    pub text_out: Option<NewText>,
}

impl Event {
    /// An event of `kind` with everything else empty.
    pub fn new(kind: u32) -> Event {
        Event {
            kind,
            pos: BlockPos::new(0, 0, 0),
            ..Event::default()
        }
    }
}

/// An item stack as SNBT (`{id:"minecraft:stone",count:3}`; `{}` for none).
pub fn item_snbt(stack: &crate::item::ItemStack) -> String {
    if stack.is_empty() {
        return "{}".to_owned();
    }
    vanillite_nbt::snbt::to_string(&vanillite_nbt::Tag::Compound(
        crate::item_nbt::stack_to_nbt(stack),
    ))
}

/// Item stacks as an SNBT list.
pub fn items_snbt<'a>(stacks: impl IntoIterator<Item = &'a crate::item::ItemStack>) -> String {
    let list = stacks
        .into_iter()
        .filter(|s| !s.is_empty())
        .map(|s| vanillite_nbt::Tag::Compound(crate::item_nbt::stack_to_nbt(s)))
        .collect();
    vanillite_nbt::snbt::to_string(&vanillite_nbt::Tag::List(vanillite_nbt::List(list)))
}

/// An item stack from SNBT (`{}` and unknown items: empty); `None` for bad SNBT.
pub fn parse_item(snbt: &str) -> Option<crate::item::ItemStack> {
    if snbt.trim().is_empty() {
        return Some(crate::item::ItemStack::EMPTY);
    }
    let c = vanillite_nbt::snbt::parse_compound(snbt).ok()?;
    if c.is_empty() {
        return Some(crate::item::ItemStack::EMPTY);
    }
    Some(crate::item_nbt::stack_from_nbt(&c))
}

/// Item stacks from an SNBT list (empty stacks dropped); `None` for bad SNBT.
pub fn parse_items(snbt: &str) -> Option<Vec<crate::item::ItemStack>> {
    if snbt.trim().is_empty() {
        return Some(Vec::new());
    }
    match vanillite_nbt::snbt::parse(snbt).ok()? {
        vanillite_nbt::Tag::List(list) => Some(
            list.0
                .iter()
                .filter_map(|t| match t {
                    vanillite_nbt::Tag::Compound(c) => Some(crate::item_nbt::stack_from_nbt(c)),
                    _ => None,
                })
                .filter(|s| !s.is_empty())
                .collect(),
        ),
        _ => None,
    }
}

/// The most experience a handler may set (`value` of [`vanillite_abi::event::ENTITY_DROPS`]
/// and [`vanillite_abi::event::ENTITY_BREED`]): a few hundred orbs at most.
pub const MAX_EXPERIENCE: i64 = 1_000_000;

/// The most stacks a handler may put in place of drops (`item_out`).
pub const MAX_DROPS: usize = 256;

/// The most an explosion's radius may become by a handler (`amount` of
/// [`vanillite_abi::event::ENTITY_EXPLODE`]); Vanilla's fireballs reach 127.
pub const MAX_EXPLOSION_RADIUS: f64 = 128.0;

/// A handler's experience (`value`), clamped to `0..=`[`MAX_EXPERIENCE`].
pub fn experience_value(value: i64) -> i32 {
    i32::try_from(value.clamp(0, MAX_EXPERIENCE)).unwrap_or(0)
}

/// The drops a handler put in place of an event's (`item_out`, an SNBT list): `None` when
/// it changed nothing or sent bad SNBT, else at most [`MAX_DROPS`] stacks.
pub fn drops_out(event: &Event) -> Option<Vec<crate::item::ItemStack>> {
    let mut items = parse_items(event.item_out.as_deref()?)?;
    items.truncate(MAX_DROPS);
    Some(items)
}

/// What a plugin goal sees of its mob (`vanillite_abi::GoalMob`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GoalMob {
    /// The mob.
    pub mob: u128,
    /// Its position.
    pub pos: [f64; 3],
    /// Its yaw.
    pub yaw: f32,
    /// Its pitch.
    pub pitch: f32,
    /// Its health.
    pub health: f32,
    /// On the ground.
    pub on_ground: bool,
    /// Its navigation follows a path.
    pub navigating: bool,
    /// Its target (0 for none).
    pub target: u128,
    /// The target's position.
    pub target_pos: [f64; 3],
    /// The world's game time.
    pub game_time: i64,
}

/// What a plugin goal's tick makes its mob do (`vanillite_abi::GoalAction`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GoalAction {
    /// `vanillite_abi::goal_action` bits.
    pub actions: u32,
    /// Where to walk.
    pub move_to: [f64; 3],
    /// Speed modifier.
    pub speed: f64,
    /// Where to look.
    pub look_at: [f64; 3],
    /// The new target (0 forgets it).
    pub target: u128,
}

/// A plugin goal of one mob, run by the mob's goal selector like a Vanilla goal.
pub trait PluginGoal: Send + Sync + std::fmt::Debug {
    /// Priority (lower runs first).
    fn priority(&self) -> i32;
    /// `Goal.Flag` bits it controls (`vanillite_abi::goal_flag`).
    fn flags(&self) -> u32;
    /// It belongs to the target selector.
    fn is_target(&self) -> bool;
    /// Whether it should start.
    fn can_use(&self, mob: &GoalMob) -> bool;
    /// Whether it should go on.
    fn can_continue(&self, mob: &GoalMob) -> bool;
    /// It starts.
    fn start(&self, mob: &GoalMob);
    /// It stops.
    fn stop(&self, mob: &GoalMob);
    /// One tick.
    fn tick(&self, mob: &GoalMob) -> GoalAction;
}

/// Game events the server forwards to plugins.
pub trait GameHooks: Sync {
    /// Whether a plugin listens to an event kind of [`Event`]. With no plugin at all the
    /// level holds no hooks, so this is only asked when plugins are loaded.
    fn wants(&self, _kind: u32) -> bool {
        false
    }

    /// Fires an event of the kinds since ABI 1.6; the handlers' changes are written back.
    fn event(&self, _event: &mut Event) {}

    /// An entity is about to take `amount` damage of `damage_type` (e.g. `minecraft:fall`)
    /// from `attacker` (the responsible entity) and `direct` (e.g. its projectile). Returns
    /// the damage to apply, or `None` to cancel it.
    fn entity_damage(
        &self,
        _entity: u128,
        _entity_type: &str,
        _damage_type: &str,
        amount: f32,
        _attacker: Option<u128>,
        _direct: Option<u128>,
    ) -> Option<f32> {
        Some(amount)
    }

    /// A player is about to break the block `state` at `pos`. `false` cancels it.
    fn block_break(&self, _player: u128, _pos: BlockPos, _state: BlockState) -> bool {
        true
    }

    /// A player is about to place `state` at `pos`. `false` cancels it.
    fn block_place(&self, _player: u128, _pos: BlockPos, _state: BlockState) -> bool {
        true
    }

    /// A player is about to fall asleep in the bed `state` whose head is at `pos`. `false`
    /// cancels it (the respawn point a bed sets is already set).
    fn bed_enter(&self, _player: u128, _pos: BlockPos, _state: BlockState) -> bool {
        true
    }

    /// A player left the bed `state` at `pos` (woke up, was woken or left it).
    fn bed_leave(&self, _player: u128, _pos: BlockPos, _state: BlockState) {}

    /// A bed or respawn anchor (`state` at `pos`) is about to become the player's respawn
    /// point. `false` cancels it.
    fn spawn_change(&self, _player: u128, _pos: BlockPos, _state: BlockState) -> bool {
        true
    }

    /// A player died of `damage_type`, caused by `killer` when known; hooks may change its
    /// death message and whether it keeps its inventory.
    fn player_death(
        &self,
        _player: u128,
        _damage_type: &str,
        _killer: Option<u128>,
        _death: &mut Death,
    ) {
    }

    /// A mob of `entity_type` died of `damage_type`, caused by `killer` when known.
    fn entity_death(
        &self,
        _entity: u128,
        _entity_type: &str,
        _damage_type: &str,
        _killer: Option<u128>,
    ) {
    }

    /// A player uses `item` in `hand` (0 main, 1 off) on a block (position, clicked face as
    /// `Direction` index, state) or in the air. `false` cancels it.
    fn player_interact(
        &self,
        _player: u128,
        _hand: u32,
        _item: &str,
        _block: Option<(BlockPos, u32, BlockState)>,
    ) -> bool {
        true
    }
}

/// No hooks (tests, worlds without plugins).
#[derive(Debug, Default, Clone, Copy)]
pub struct NoHooks;

impl GameHooks for NoHooks {}

//! Values of the game-specific argument types (Vanilla's `ScoreHolderArgument.Result`,
//! `BlockInput`, `ItemInput`, `ResourceOrIdArgument.Result`, ...). The parsers only check
//! syntax and what the [`Source`](crate::Source) knows; the game resolves the values.

use vanillite_nbt::{Compound, Tag};
use vanillite_protocol::text::{Component, Style};

use crate::selector::EntitySelector;

/// `minecraft:score_holder`.
#[derive(Debug, Clone, PartialEq)]
pub enum ScoreHolderArg {
    /// `@...`: the selected entities (players by name, others by UUID).
    Selector(Box<EntitySelector>),
    /// A plain name (also a UUID string; the scoreboard stores what was typed).
    Name(String),
    /// `*`: every holder with a score (only where the game allows it).
    Wildcard,
}

/// `minecraft:operation` (`scoreboard players operation`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoreOperation {
    /// `=`.
    Assign,
    /// `+=`.
    Add,
    /// `-=`.
    Sub,
    /// `*=`.
    Mul,
    /// `/=` (floor division; division by zero leaves the target).
    Div,
    /// `%=` (floor modulo; zero leaves the target).
    Mod,
    /// `<`.
    Min,
    /// `>`.
    Max,
    /// `><`.
    Swap,
}

/// `minecraft:scoreboard_slot` (`DisplaySlot`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DisplaySlot {
    /// `list`.
    List,
    /// `sidebar`.
    Sidebar,
    /// `below_name`.
    BelowName,
    /// `sidebar.team.<color>`: the `ChatFormatting` id of the colour (0..=15).
    SidebarTeam(u8),
}

impl DisplaySlot {
    /// The id in the display objective packet (`DisplaySlot.id`).
    pub fn id(self) -> i32 {
        match self {
            Self::List => 0,
            Self::Sidebar => 1,
            Self::BelowName => 2,
            Self::SidebarTeam(c) => 3_i32.saturating_add(i32::from(c)),
        }
    }
}

/// `minecraft:swizzle`: the axes given.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Swizzle {
    /// `x`.
    pub x: bool,
    /// `y`.
    pub y: bool,
    /// `z`.
    pub z: bool,
}

/// `minecraft:heightmap` (the types kept after world generation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeightmapKind {
    /// `world_surface`.
    WorldSurface,
    /// `ocean_floor`.
    OceanFloor,
    /// `motion_blocking`.
    MotionBlocking,
    /// `motion_blocking_no_leaves`.
    MotionBlockingNoLeaves,
}

/// `minecraft:function`: a function or a function tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionRef {
    /// Written with `#`.
    pub tag: bool,
    /// The identifier (with namespace).
    pub id: String,
}

/// `minecraft:resource_or_tag` and `resource_or_tag_key`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceOrTag {
    /// An entry.
    Resource(String),
    /// `#tag`.
    Tag(String),
}

/// `ResourceOrIdArgument` (`loot_table`, `loot_predicate`, `loot_modifier`, `dialog`,
/// `feature`, `context_*_provider`, the holder of `slot_source`): a registered id or an inline
/// value in SNBT.
#[derive(Debug, Clone, PartialEq)]
pub enum InlineOrId {
    /// A registered entry.
    Id(String),
    /// An inline definition.
    Inline(Tag),
}

/// `minecraft:block_state` (`BlockInput`).
#[derive(Debug, Clone, PartialEq)]
pub struct BlockStateArg {
    /// The block id.
    pub block: String,
    /// The state: the default state with the given properties.
    pub state: u16,
    /// The properties written, in order.
    pub properties: Vec<(String, String)>,
    /// Block entity data.
    pub nbt: Option<Compound>,
}

/// `minecraft:block_predicate`.
#[derive(Debug, Clone, PartialEq)]
pub struct BlockPredicateArg {
    /// A block id or `#tag`.
    pub block: ResourceOrTag,
    /// Properties that must match (for tags: those the block has).
    pub properties: Vec<(String, String)>,
    /// Block entity data that must match.
    pub nbt: Option<Compound>,
}

/// `minecraft:item_stack` (`ItemInput`).
#[derive(Debug, Clone, PartialEq)]
pub struct ItemArg {
    /// The item id.
    pub item: String,
    /// Component changes in order: a value, or `None` for `!component` (removed).
    pub components: Vec<(String, Option<Tag>)>,
}

/// One test of an item predicate (`ComponentPredicateParser`).
#[derive(Debug, Clone, PartialEq)]
pub enum ItemTest {
    /// `component`: the component is present.
    Has(String),
    /// `component=value`: the component equals the value.
    Equals(String, Tag),
    /// `predicate~value`: an item sub-predicate (`minecraft:enchantments`, `count`, ...).
    Matches(String, Tag),
    /// `!test`.
    Not(Box<ItemTest>),
}

/// `minecraft:item_predicate`: what the item must be and the tests, as alternatives (`|`) of
/// conjunctions (`,`).
#[derive(Debug, Clone, PartialEq)]
pub struct ItemPredicateArg {
    /// `*`, an item id or `#tag`.
    pub item: Option<ResourceOrTag>,
    /// Any of the inner lists must fully match; empty means no tests.
    pub tests: Vec<Vec<ItemTest>>,
}

/// `minecraft:slot_source`.
#[derive(Debug, Clone, PartialEq)]
pub enum SlotSourceArg {
    /// A slot range name (`container.*`, `armor.head`, ...) and its slot indices.
    Range(String, Vec<i32>),
    /// A slot source from the registry or inline.
    Source(InlineOrId),
}

/// `minecraft:particle`.
#[derive(Debug, Clone, PartialEq)]
pub struct ParticleArg {
    /// The particle type id.
    pub id: String,
    /// Its options in SNBT, when given.
    pub options: Option<Compound>,
}

/// `minecraft:angle`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AngleArg {
    /// Degrees.
    pub angle: f32,
    /// Written with `~`.
    pub relative: bool,
}

/// `minecraft:team_color`: a `ChatFormatting` colour id (0..=15) or `reset` (`None`).
pub type TeamColor = Option<u8>;

/// Game argument values that need their own variant in `ArgValue`.
#[derive(Debug, Clone, PartialEq)]
pub enum GameValue {
    ScoreHolder(ScoreHolderArg),
    Operation(ScoreOperation),
    DisplaySlot(DisplaySlot),
    Swizzle(Swizzle),
    Heightmap(HeightmapKind),
    Function(FunctionRef),
    ResourceOrTag(ResourceOrTag),
    InlineOrId(InlineOrId),
    BlockState(Box<BlockStateArg>),
    BlockPredicate(Box<BlockPredicateArg>),
    Item(Box<ItemArg>),
    ItemPredicate(Box<ItemPredicateArg>),
    /// `item_slot`: one slot index.
    Slot(i32),
    /// `item_slots`: a slot range name and its indices.
    Slots(String, Vec<i32>),
    SlotSource(SlotSourceArg),
    Particle(Box<ParticleArg>),
    Angle(AngleArg),
    Component(Box<Component>),
    Style(Box<Style>),
    TeamColor(TeamColor),
    /// `hex_color`: RGB.
    HexColor(i32),
}

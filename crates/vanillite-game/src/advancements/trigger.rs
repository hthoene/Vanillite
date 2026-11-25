//! Criterion triggers (`CriteriaTriggers`, `SimpleCriterionTrigger`): the trigger types, the
//! conditions of their instances, and the events the game fires them with.

use serde_json::{Map, Value};
use vanillite_base::{BlockPos, Vec3};
use vanillite_data::block::BlockState;

use super::predicate::{
    Bounds, Condition, Context, EntityView, IdSet, ItemPredicate, Res, StateMatcher, World,
    block_matches, object, qualify, string,
};
use crate::inventory::PlayerInventory;
use crate::item::ItemStack;

/// The trigger types in `CriteriaTriggers` registration order.
pub const TRIGGERS: [&str; 58] = [
    "impossible",
    "player_killed_entity",
    "entity_killed_player",
    "enter_block",
    "inventory_changed",
    "recipe_unlocked",
    "player_hurt_entity",
    "entity_hurt_player",
    "enchanted_item",
    "filled_bucket",
    "brewed_potion",
    "construct_beacon",
    "used_ender_eye",
    "summoned_entity",
    "bred_animals",
    "location",
    "slept_in_bed",
    "cured_zombie_villager",
    "villager_trade",
    "item_durability_changed",
    "levitation",
    "changed_dimension",
    "tick",
    "tame_animal",
    "placed_block",
    "consume_item",
    "effects_changed",
    "used_totem",
    "nether_travel",
    "fishing_rod_hooked",
    "channeled_lightning",
    "shot_crossbow",
    "spear_mobs",
    "killed_by_arrow",
    "hero_of_the_village",
    "voluntary_exile",
    "slide_down_block",
    "bee_nest_destroyed",
    "target_hit",
    "item_used_on_block",
    "default_block_use",
    "any_block_use",
    "player_generates_container_loot",
    "thrown_item_picked_up_by_entity",
    "thrown_item_picked_up_by_player",
    "player_interacted_with_entity",
    "player_sheared_equipment",
    "started_riding",
    "lightning_strike",
    "using_item",
    "fall_from_height",
    "ride_entity_in_lava",
    "kill_mob_near_sculk_catalyst",
    "allay_drop_item_on_block",
    "avoid_vibration",
    "recipe_crafted",
    "crafter_recipe_crafted",
    "fall_after_explosion",
];

/// A trigger type: its index in [`TRIGGERS`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TriggerKind(pub u8);

impl TriggerKind {
    pub const IMPOSSIBLE: TriggerKind = TriggerKind(0);
    pub const ENTER_BLOCK: TriggerKind = TriggerKind(3);
    pub const INVENTORY_CHANGED: TriggerKind = TriggerKind(4);
    pub const RECIPE_UNLOCKED: TriggerKind = TriggerKind(5);
    pub const LOCATION: TriggerKind = TriggerKind(15);
    pub const CHANGED_DIMENSION: TriggerKind = TriggerKind(21);
    pub const TICK: TriggerKind = TriggerKind(22);
    pub const PLACED_BLOCK: TriggerKind = TriggerKind(24);
    pub const CONSUME_ITEM: TriggerKind = TriggerKind(25);
    pub const ITEM_USED_ON_BLOCK: TriggerKind = TriggerKind(39);
    pub const DEFAULT_BLOCK_USE: TriggerKind = TriggerKind(40);
    pub const ANY_BLOCK_USE: TriggerKind = TriggerKind(41);
    pub const RECIPE_CRAFTED: TriggerKind = TriggerKind(55);
    pub const CRAFTER_RECIPE_CRAFTED: TriggerKind = TriggerKind(56);

    /// By id (`minecraft:tick` or `tick`).
    pub fn by_name(name: &str) -> Option<TriggerKind> {
        let path = qualify(name);
        let path = path.strip_prefix("minecraft:")?;
        TRIGGERS
            .iter()
            .position(|t| *t == path)
            .and_then(|i| u8::try_from(i).ok())
            .map(TriggerKind)
    }

    /// The index into per-trigger tables.
    pub fn index(self) -> usize {
        usize::from(self.0)
    }
}

/// The trigger-specific conditions of a criterion (`TriggerInstance` without `player`).
#[derive(Clone, Debug, PartialEq)]
enum Data {
    /// No conditions besides `player` (`impossible`, `tick`, `location`, ...).
    None,
    /// `inventory_changed`: occupied, full and empty slot bounds and the items.
    Inventory {
        slots: [Bounds; 3],
        items: Vec<ItemPredicate>,
    },
    /// `location` of the block triggers.
    Location(Option<Condition>),
    /// `enter_block`.
    EnterBlock {
        blocks: Option<IdSet>,
        state: Option<StateMatcher>,
    },
    /// `recipe_unlocked`, `recipe_crafted`, `crafter_recipe_crafted`.
    Recipes {
        recipes: IdSet,
        ingredients: Vec<ItemPredicate>,
    },
    /// `changed_dimension`.
    Dimension {
        from: Option<String>,
        to: Option<String>,
    },
    /// `consume_item`.
    Item(Option<ItemPredicate>),
    /// A trigger whose conditions are not evaluated: never matches.
    Unsupported,
}

/// The conditions of one criterion.
#[derive(Clone, Debug, PartialEq)]
pub struct Conditions {
    player: Option<Condition>,
    data: Data,
}

fn condition(o: &Map<String, Value>, key: &str) -> Res<Option<Condition>> {
    o.get(key).map(Condition::parse).transpose()
}

fn item_list(o: &Map<String, Value>, key: &str) -> Res<Vec<ItemPredicate>> {
    match o.get(key) {
        None => Ok(Vec::new()),
        Some(Value::Array(items)) => items.iter().map(ItemPredicate::parse).collect(),
        Some(other) => Err(format!("{key} is not a list: {other}")),
    }
}

impl Conditions {
    /// Parses the `conditions` of a criterion of `kind`.
    pub fn parse(kind: TriggerKind, v: Option<&Value>) -> Res<Conditions> {
        let empty = Map::new();
        let o = match v {
            Some(v) => object(v)?,
            None => &empty,
        };
        let data = match kind {
            TriggerKind::IMPOSSIBLE | TriggerKind::TICK | TriggerKind::LOCATION => Data::None,
            TriggerKind::INVENTORY_CHANGED => {
                let slots = match o.get("slots") {
                    Some(s) => {
                        let s = object(s)?;
                        let b = |k: &str| s.get(k).map(Bounds::parse).transpose();
                        [
                            b("occupied")?.unwrap_or_default(),
                            b("full")?.unwrap_or_default(),
                            b("empty")?.unwrap_or_default(),
                        ]
                    }
                    None => [Bounds::default(); 3],
                };
                Data::Inventory {
                    slots,
                    items: item_list(o, "items")?,
                }
            }
            TriggerKind::PLACED_BLOCK
            | TriggerKind::ITEM_USED_ON_BLOCK
            | TriggerKind::DEFAULT_BLOCK_USE
            | TriggerKind::ANY_BLOCK_USE => Data::Location(condition(o, "location")?),
            TriggerKind::ENTER_BLOCK => Data::EnterBlock {
                blocks: o.get("blocks").map(IdSet::parse).transpose()?,
                state: o.get("state").map(StateMatcher::parse).transpose()?,
            },
            TriggerKind::RECIPE_UNLOCKED
            | TriggerKind::RECIPE_CRAFTED
            | TriggerKind::CRAFTER_RECIPE_CRAFTED => Data::Recipes {
                recipes: IdSet::parse(o.get("recipes").ok_or("missing recipes")?)?,
                ingredients: item_list(o, "ingredients")?,
            },
            TriggerKind::CHANGED_DIMENSION => Data::Dimension {
                from: o.get("from").map(string).transpose()?.map(qualify),
                to: o.get("to").map(string).transpose()?.map(qualify),
            },
            TriggerKind::CONSUME_ITEM => {
                Data::Item(o.get("item").map(ItemPredicate::parse).transpose()?)
            }
            _ => Data::Unsupported,
        };
        Ok(Conditions {
            player: condition(o, "player")?,
            data,
        })
    }
}

/// A trigger with its data (the arguments of `CriterionTrigger.trigger`).
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// `tick`: every player tick.
    Tick,
    /// `location`: every 20 ticks.
    Location,
    /// `inventory_changed` with the stack of the changed slot.
    InventoryChanged(ItemStack),
    /// `placed_block` at `pos` with the used stack.
    PlacedBlock(BlockPos, ItemStack),
    /// `item_used_on_block` at `pos` with the stack before its use.
    ItemUsedOnBlock(BlockPos, ItemStack),
    /// `default_block_use` at `pos`.
    DefaultBlockUse(BlockPos),
    /// `any_block_use` at `pos` with the held stack.
    AnyBlockUse(BlockPos, ItemStack),
    /// `enter_block` with the state the player is inside.
    EnterBlock(BlockState),
    /// `recipe_crafted` with the recipe id and the used ingredients.
    RecipeCrafted(String, Vec<ItemStack>),
    /// `recipe_unlocked` with the recipe id.
    RecipeUnlocked(String),
    /// `changed_dimension` from, to.
    ChangedDimension(String, String),
    /// `consume_item` with the consumed stack.
    ConsumeItem(ItemStack),
}

impl Event {
    /// The trigger type of the event.
    pub fn kind(&self) -> TriggerKind {
        match self {
            Event::Tick => TriggerKind::TICK,
            Event::Location => TriggerKind::LOCATION,
            Event::InventoryChanged(_) => TriggerKind::INVENTORY_CHANGED,
            Event::PlacedBlock(..) => TriggerKind::PLACED_BLOCK,
            Event::ItemUsedOnBlock(..) => TriggerKind::ITEM_USED_ON_BLOCK,
            Event::DefaultBlockUse(_) => TriggerKind::DEFAULT_BLOCK_USE,
            Event::AnyBlockUse(..) => TriggerKind::ANY_BLOCK_USE,
            Event::EnterBlock(_) => TriggerKind::ENTER_BLOCK,
            Event::RecipeCrafted(..) => TriggerKind::RECIPE_CRAFTED,
            Event::RecipeUnlocked(_) => TriggerKind::RECIPE_UNLOCKED,
            Event::ChangedDimension(..) => TriggerKind::CHANGED_DIMENSION,
            Event::ConsumeItem(_) => TriggerKind::CONSUME_ITEM,
        }
    }
}

/// What triggers are evaluated against: the world and the player.
pub struct TriggerContext<'a> {
    pub world: &'a dyn World,
    pub player: &'a EntityView<'a>,
    pub inventory: &'a PlayerInventory,
    /// Occupied, full and empty inventory slots, counted once.
    counts: std::cell::OnceCell<[u32; 3]>,
}

impl<'a> TriggerContext<'a> {
    /// A context over the world and the player.
    pub fn new(
        world: &'a dyn World,
        player: &'a EntityView<'a>,
        inventory: &'a PlayerInventory,
    ) -> TriggerContext<'a> {
        TriggerContext {
            world,
            player,
            inventory,
            counts: std::cell::OnceCell::new(),
        }
    }

    /// `InventoryChangeTrigger.trigger`'s slot counts.
    fn counts(&self) -> [u32; 3] {
        *self.counts.get_or_init(|| {
            let (mut occupied, mut full, mut empty) = (0u32, 0u32, 0u32);
            for stack in inventory_slots(self.inventory) {
                if stack.is_empty() {
                    empty = empty.saturating_add(1);
                } else {
                    occupied = occupied.saturating_add(1);
                    if stack.count >= crate::item::max_stack_size(stack) {
                        full = full.saturating_add(1);
                    }
                }
            }
            [occupied, full, empty]
        })
    }
}

impl std::fmt::Debug for TriggerContext<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TriggerContext")
            .field("player", &self.player)
            .finish_non_exhaustive()
    }
}

/// The slots of `Inventory` (main, armour, off hand, body, saddle) for `inventory_changed`.
fn inventory_slots(inv: &PlayerInventory) -> impl Iterator<Item = &ItemStack> {
    static EMPTY: ItemStack = ItemStack::EMPTY;
    inv.items
        .iter()
        .chain(inv.armor.iter())
        .chain(std::iter::once(&inv.offhand))
        .chain([&EMPTY, &EMPTY])
}

fn center(pos: BlockPos) -> Vec3 {
    Vec3::new(
        f64::from(pos.x) + 0.5,
        f64::from(pos.y) + 0.5,
        f64::from(pos.z) + 0.5,
    )
}

impl Conditions {
    /// The `player` condition (`EntityPredicate.createContext(player, player)`).
    fn player_matches(&self, ctx: &TriggerContext<'_>) -> bool {
        self.player.as_ref().is_none_or(|c| {
            c.test(&Context {
                world: ctx.world,
                this: Some(ctx.player),
                origin: Some(ctx.player.pos),
                block_state: None,
                tool: None,
            })
        })
    }

    /// Whether the criterion is met by `event` (`TriggerInstance.matches` and `player`).
    pub fn matches(&self, event: &Event, ctx: &TriggerContext<'_>) -> bool {
        let world = ctx.world;
        let location = |c: &Option<Condition>, pos: BlockPos, tool: Option<&ItemStack>| {
            c.as_ref().is_none_or(|c| {
                c.test(&Context {
                    world,
                    this: Some(ctx.player),
                    origin: Some(center(pos)),
                    block_state: world.block(pos),
                    tool,
                })
            })
        };
        let data_ok = match (&self.data, event) {
            (Data::None, Event::Tick | Event::Location) => true,
            (Data::Inventory { slots, items }, Event::InventoryChanged(changed)) => {
                inventory_matches(slots, items, changed, ctx)
            }
            (
                Data::Location(c),
                Event::PlacedBlock(pos, tool)
                | Event::ItemUsedOnBlock(pos, tool)
                | Event::AnyBlockUse(pos, tool),
            ) => location(c, *pos, Some(tool)),
            (Data::Location(c), Event::DefaultBlockUse(pos)) => location(c, *pos, None),
            (Data::EnterBlock { blocks, state }, Event::EnterBlock(s)) => {
                block_matches(blocks.as_ref(), state.as_ref(), *s, world)
            }
            (Data::Recipes { recipes, .. }, Event::RecipeUnlocked(id)) => {
                recipes.contains("minecraft:recipe", id, world)
            }
            (
                Data::Recipes {
                    recipes,
                    ingredients,
                },
                Event::RecipeCrafted(id, used),
            ) => {
                recipes.contains("minecraft:recipe", id, world) && {
                    let mut remaining: Vec<&ItemStack> = used.iter().collect();
                    ingredients.iter().all(|p| {
                        match remaining.iter().position(|s| p.test(s, world)) {
                            Some(i) => {
                                remaining.remove(i);
                                true
                            }
                            None => false,
                        }
                    })
                }
            }
            (Data::Dimension { from, to }, Event::ChangedDimension(f, t)) => {
                from.as_ref().is_none_or(|d| d == f) && to.as_ref().is_none_or(|d| d == t)
            }
            (Data::Item(p), Event::ConsumeItem(stack)) => {
                p.as_ref().is_none_or(|p| p.test(stack, world))
            }
            _ => false,
        };
        data_ok && self.player_matches(ctx)
    }
}

/// `InventoryChangeTrigger.TriggerInstance.matches`.
fn inventory_matches(
    slots: &[Bounds; 3],
    items: &[ItemPredicate],
    changed: &ItemStack,
    ctx: &TriggerContext<'_>,
) -> bool {
    let [occupied, full, empty] = ctx.counts();
    let [occupied_b, full_b, empty_b] = slots;
    if !(full_b.matches(f64::from(full))
        && empty_b.matches(f64::from(empty))
        && occupied_b.matches(f64::from(occupied)))
    {
        return false;
    }
    match items {
        [] => true,
        [single] => !changed.is_empty() && single.test(changed, ctx.world),
        _ => {
            let mut left: Vec<&ItemPredicate> = items.iter().collect();
            for stack in inventory_slots(ctx.inventory) {
                if left.is_empty() {
                    return true;
                }
                if !stack.is_empty() {
                    left.retain(|p| !p.test(stack, ctx.world));
                }
            }
            left.is_empty()
        }
    }
}

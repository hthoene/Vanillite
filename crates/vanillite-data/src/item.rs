//! Items and the default values of their core components.
//!
//! The full default component set of every item is in `data/vanilla/items.json`; typed component
//! structs are generated from it as gameplay needs them.

pub use crate::generated::items::{ITEMS, Rarity, TOOLS, ids};

use crate::block::BlockId;

/// An item (registry id of `minecraft:item`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct ItemId(pub u16);

/// Static facts of an item.
#[derive(Debug)]
pub struct ItemInfo {
    pub name: &'static str,
    pub max_stack_size: u8,
    /// Durability, 0 if the item has no `minecraft:max_damage`.
    pub max_damage: u16,
    pub rarity: Rarity,
    /// Block placed by this item, or `u16::MAX`.
    pub block: u16,
    /// Index into [`TOOLS`] of the default `minecraft:tool` component, or `u16::MAX`.
    pub tool: u16,
    /// `EquipmentSlot` ordinal (mainhand, offhand, feet, legs, chest, head, body, saddle) of the
    /// default `minecraft:equippable` component, or 255.
    pub equip_slot: u8,
    /// The default `minecraft:attribute_modifiers`.
    pub modifiers: &'static [ItemModifier],
    /// The default `minecraft:weapon` component.
    pub weapon: Option<WeaponInfo>,
    /// The default `minecraft:food` component.
    pub food: Option<FoodInfo>,
    /// The default `minecraft:consumable` component.
    pub consumable: Option<&'static ConsumableInfo>,
    /// The item id of the default `minecraft:use_remainder` component, empty without one.
    pub use_remainder: &'static str,
    /// The seconds of the default `minecraft:use_cooldown` component, 0 without one.
    pub use_cooldown: f32,
    /// `interact_vibrations` of the default `minecraft:use_effects` component.
    pub use_vibrations: bool,
    /// The default `minecraft:blocks_attacks` component (shields).
    pub blocks_attacks: Option<&'static BlocksAttacksInfo>,
    /// The default `minecraft:kinetic_weapon` component (spears).
    pub kinetic_weapon: Option<&'static KineticWeaponInfo>,
    /// The default `minecraft:piercing_weapon` component (spears).
    pub piercing_weapon: Option<&'static PiercingWeaponInfo>,
    /// The default `minecraft:attack_range` component: min reach, max reach, min creative
    /// reach, max creative reach, hitbox margin, mob factor.
    pub attack_range: Option<&'static [f32; 6]>,
    /// The default `minecraft:minimum_attack_charge` component, 0 without one.
    pub minimum_attack_charge: f32,
    /// `duration` of the default `minecraft:attack_animation` component.
    pub attack_animation_ticks: u16,
    /// The default `minecraft:damage_type` component, empty without one.
    pub damage_type: &'static str,
    /// The `death_effects` of the default `minecraft:death_protection` component.
    pub death_protection: Option<&'static [ConsumeEffectInfo]>,
    /// The `minecraft:context_int_provider` of the default `minecraft:compostable` component
    /// (its `layers`), empty if the item cannot be composted.
    pub compostable: &'static str,
    /// The `minecraft:jukebox_song` of the default `minecraft:jukebox_playable` component,
    /// empty if the item is no music disc.
    pub jukebox_song: &'static str,
    /// The `minecraft:block_transformer` of the default component (axes, shovels, hoes),
    /// empty without one.
    pub block_transformer: &'static str,
    /// The equip sound of the default `minecraft:equippable` component.
    pub equip_sound: &'static str,
    /// Whether the default `minecraft:equippable` component is `swappable` by players (and
    /// allows every entity).
    pub equip_swappable: bool,
    /// The damage type tag of the default `minecraft:damage_resistant` component (e.g.
    /// `minecraft:is_fire`), empty without one.
    pub damage_resistant: &'static str,
}

/// One entry of a `minecraft:attribute_modifiers` component.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ItemModifier {
    /// Attribute id (index into `ATTRIBUTES`).
    pub attribute: u16,
    pub amount: f64,
    /// `AttributeModifier.Operation` ordinal: add value, add multiplied base, add multiplied
    /// total.
    pub operation: u8,
    /// `EquipmentSlotGroup` ordinal: any, mainhand, offhand, hand, feet, legs, chest, head,
    /// armor, body, saddle.
    pub slot: u8,
}

/// A `minecraft:weapon` component.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WeaponInfo {
    pub item_damage_per_attack: u32,
    pub disable_blocking_for_seconds: f32,
}

/// A `minecraft:food` component.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FoodInfo {
    pub nutrition: i32,
    pub saturation: f32,
    pub can_always_eat: bool,
}

/// A `minecraft:consumable` component.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConsumableInfo {
    pub consume_seconds: f32,
    /// `ItemUseAnimation`: `eat`, `drink`, ...
    pub animation: &'static str,
    pub sound: &'static str,
    pub has_consume_particles: bool,
    pub on_consume_effects: &'static [ConsumeEffectInfo],
}

/// One `ConsumeEffect` of a `minecraft:consumable` component.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConsumeEffectInfo {
    /// `apply_effects`.
    ApplyEffects {
        effects: &'static [EffectInstanceInfo],
        probability: f32,
    },
    /// `remove_effects`: mob effect ids, or one tag with `#`.
    RemoveEffects(&'static [&'static str]),
    /// `clear_all_effects`.
    ClearAllEffects,
    /// `teleport_randomly` with its diameter.
    TeleportRandomly(f32),
    /// `play_sound`.
    PlaySound(&'static str),
}

/// A `MobEffectInstance` of an item component.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EffectInstanceInfo {
    pub id: &'static str,
    pub duration: i32,
    pub amplifier: i32,
    pub ambient: bool,
    pub show_particles: bool,
    pub show_icon: bool,
}

/// A `minecraft:kinetic_weapon` component.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KineticWeaponInfo {
    pub contact_cooldown_ticks: i32,
    pub delay_ticks: i32,
    pub dismount_conditions: Option<KineticConditionInfo>,
    pub knockback_conditions: Option<KineticConditionInfo>,
    pub damage_conditions: Option<KineticConditionInfo>,
    pub forward_movement: f32,
    pub damage_multiplier: f32,
    /// Empty without the sound.
    pub sound: &'static str,
    /// Empty without the sound.
    pub hit_sound: &'static str,
}

/// A `KineticWeapon.Condition`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KineticConditionInfo {
    pub max_duration_ticks: i32,
    pub min_speed: f32,
    pub min_relative_speed: f32,
}

/// A `minecraft:piercing_weapon` component.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PiercingWeaponInfo {
    pub deals_knockback: bool,
    pub dismounts: bool,
    /// Empty without the sound.
    pub sound: &'static str,
    /// Empty without the sound.
    pub hit_sound: &'static str,
}

/// A `minecraft:blocks_attacks` component.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BlocksAttacksInfo {
    pub block_delay_seconds: f32,
    pub disable_cooldown_scale: f32,
    pub damage_reductions: &'static [DamageReductionInfo],
    /// `ItemDamageFunction`: threshold, base, factor.
    pub item_damage: [f32; 3],
    /// Damage type ids, or one tag with `#`; empty without the field.
    pub bypassed_by: &'static [&'static str],
    /// Empty without the sound.
    pub block_sound: &'static str,
    /// Empty without the sound.
    pub disabled_sound: &'static str,
}

/// A `BlocksAttacks.DamageReduction`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DamageReductionInfo {
    pub horizontal_blocking_angle: f32,
    /// Damage type ids, or one tag with `#`; empty for every type.
    pub types: &'static [&'static str],
    pub base: f32,
    pub factor: f32,
}

/// The blocks a tool rule applies to.
#[derive(Debug)]
pub enum BlockSet {
    /// A block tag id without `#`, e.g. `minecraft:mineable/pickaxe` (resolved at run time).
    Tag(&'static str),
    /// Block ids.
    Blocks(&'static [u16]),
}

/// One rule of a `minecraft:tool` component.
#[derive(Debug)]
pub struct ToolRule {
    pub blocks: BlockSet,
    pub speed: Option<f32>,
    pub correct_for_drops: Option<bool>,
}

/// A `minecraft:tool` component.
#[derive(Debug)]
pub struct ToolInfo {
    pub rules: &'static [ToolRule],
    pub default_mining_speed: f32,
    pub damage_per_block: u32,
    pub can_destroy_blocks_in_creative: bool,
}

impl ItemId {
    #[inline]
    pub fn info(self) -> &'static ItemInfo {
        &ITEMS[usize::from(self.0)]
    }

    #[inline]
    pub fn name(self) -> &'static str {
        self.info().name
    }

    /// The block this item places, if it is a block item.
    #[inline]
    pub fn block(self) -> Option<BlockId> {
        let b = self.info().block;
        (b != u16::MAX).then_some(BlockId(b))
    }

    /// The default `minecraft:tool` component.
    #[inline]
    pub fn tool(self) -> Option<&'static ToolInfo> {
        TOOLS.get(usize::from(self.info().tool))
    }

    /// Looks an item up by its registry name.
    pub fn by_name(name: &str) -> Option<Self> {
        crate::registry::ITEM.id(name).map(Self)
    }
}

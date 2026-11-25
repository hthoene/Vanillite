//! Item stacks as gameplay sees them: effective component values (patch over the item's
//! defaults), tools and enchantments.

pub use vanillite_protocol::item::{ComponentPatch, ItemStack};

use vanillite_data::block::{BLOCK_COUNT, BlockState};
use vanillite_data::item::{BlockSet, ItemId, TOOLS, ToolInfo};
use vanillite_protocol::codec::Reader;
use vanillite_protocol::components::ids;

/// `EquipmentSlot` in Vanilla order (its ordinal is the wire id of `set_equipment`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum EquipmentSlot {
    MainHand = 0,
    OffHand = 1,
    Feet = 2,
    Legs = 3,
    Chest = 4,
    Head = 5,
    Body = 6,
    Saddle = 7,
}

impl EquipmentSlot {
    /// The slot of an ordinal.
    pub fn from_ordinal(v: u8) -> Option<EquipmentSlot> {
        Some(match v {
            0 => EquipmentSlot::MainHand,
            1 => EquipmentSlot::OffHand,
            2 => EquipmentSlot::Feet,
            3 => EquipmentSlot::Legs,
            4 => EquipmentSlot::Chest,
            5 => EquipmentSlot::Head,
            6 => EquipmentSlot::Body,
            7 => EquipmentSlot::Saddle,
            _ => return None,
        })
    }

    /// The ordinal (wire id).
    pub fn ordinal(self) -> u8 {
        #[allow(clippy::as_conversions)]
        let v = self as u8;
        v
    }

    /// Whether this is a humanoid armour slot.
    pub fn is_armor(self) -> bool {
        matches!(
            self,
            EquipmentSlot::Feet | EquipmentSlot::Legs | EquipmentSlot::Chest | EquipmentSlot::Head
        )
    }
}

/// Reads a VarInt component value.
fn varint_value(data: &[u8]) -> Option<i32> {
    let mut r = Reader::new(data);
    r.varint().ok()
}

/// The item of a stack.
#[inline]
pub fn item_id(stack: &ItemStack) -> ItemId {
    ItemId(stack.item)
}

/// `getMaxStackSize`: the `max_stack_size` component, patched or default.
pub fn max_stack_size(stack: &ItemStack) -> i32 {
    if let Some(p) = stack.patch() {
        if let Some(v) = p.get(ids::MAX_STACK_SIZE).and_then(varint_value) {
            return v;
        }
        if p.is_removed(ids::MAX_STACK_SIZE) {
            return 1;
        }
    }
    i32::from(item_id(stack).info().max_stack_size)
}

/// `max_damage`, 0 if the stack has none.
pub fn max_damage(stack: &ItemStack) -> i32 {
    if let Some(p) = stack.patch() {
        if let Some(v) = p.get(ids::MAX_DAMAGE).and_then(varint_value) {
            return v;
        }
        if p.is_removed(ids::MAX_DAMAGE) {
            return 0;
        }
    }
    i32::from(item_id(stack).info().max_damage)
}

/// `getDamageValue`.
pub fn damage(stack: &ItemStack) -> i32 {
    stack
        .component(ids::DAMAGE)
        .and_then(varint_value)
        .unwrap_or(0)
        .clamp(0, max_damage(stack).max(0))
}

/// `isDamageableItem`: has `max_damage` and `damage` and is not unbreakable.
pub fn is_damageable(stack: &ItemStack) -> bool {
    max_damage(stack) > 0 && stack.component(ids::UNBREAKABLE).is_none()
}

/// `isStackable`.
pub fn is_stackable(stack: &ItemStack) -> bool {
    max_stack_size(stack) > 1 && (!is_damageable(stack) || damage(stack) == 0)
}

/// Sets the `damage` component.
pub fn set_damage(stack: &mut ItemStack, value: i32) {
    let mut bytes = Vec::with_capacity(5);
    vanillite_protocol::codec::write_varint(&mut bytes, value.max(0));
    stack.patch_mut().set(ids::DAMAGE, bytes.into());
}

/// The equipment slot the item goes to (`getEquipmentSlotForItem`; patched `equippable`
/// components are not read yet).
pub fn equipment_slot(stack: &ItemStack) -> EquipmentSlot {
    EquipmentSlot::from_ordinal(item_id(stack).info().equip_slot)
        .filter(|s| !matches!(s, EquipmentSlot::Body | EquipmentSlot::Saddle))
        .unwrap_or(EquipmentSlot::MainHand)
}

/// Enchantments of a stack: `(minecraft:enchantment id, level)`.
pub fn enchantments(stack: &ItemStack) -> Vec<(u32, i32)> {
    let Some(data) = stack.component(ids::ENCHANTMENTS) else {
        return Vec::new();
    };
    let mut r = Reader::new(data);
    let mut out = Vec::new();
    let Ok(n) = r.varint() else {
        return out;
    };
    for _ in 0..n.clamp(0, 256) {
        let (Ok(id), Ok(level)) = (r.varint(), r.varint()) else {
            break;
        };
        if let Ok(id) = u32::try_from(id) {
            out.push((id, level));
        }
    }
    out
}

/// A tool rule with its block set resolved to a bit set over block ids.
#[derive(Debug, Clone)]
struct ResolvedRule {
    blocks: Box<[u64]>,
    speed: Option<f32>,
    correct_for_drops: Option<bool>,
}

impl ResolvedRule {
    fn contains(&self, state: BlockState) -> bool {
        let b = usize::from(state.block().0);
        self.blocks
            .get(b / 64)
            .is_some_and(|w| w & (1u64 << (b % 64)) != 0)
    }
}

/// The default `minecraft:tool` components with their block tags resolved (built once at
/// start from the loaded tags).
#[derive(Debug, Clone, Default)]
pub struct ToolTable {
    tools: Vec<Vec<ResolvedRule>>,
}

impl ToolTable {
    /// Resolves every tool rule; `tag` returns the block ids of a block tag.
    pub fn new(tag: &dyn Fn(&str) -> Vec<u16>) -> ToolTable {
        let words = BLOCK_COUNT.div_ceil(64);
        let tools = TOOLS
            .iter()
            .map(|tool| {
                tool.rules
                    .iter()
                    .map(|rule| {
                        let mut bits = vec![0u64; words];
                        let ids = match rule.blocks {
                            BlockSet::Tag(t) => tag(t),
                            BlockSet::Blocks(b) => b.to_vec(),
                        };
                        for id in ids {
                            let i = usize::from(id);
                            if let Some(w) = bits.get_mut(i / 64) {
                                *w |= 1u64 << (i % 64);
                            }
                        }
                        ResolvedRule {
                            blocks: bits.into(),
                            speed: rule.speed,
                            correct_for_drops: rule.correct_for_drops,
                        }
                    })
                    .collect()
            })
            .collect();
        ToolTable { tools }
    }

    fn tool(&self, stack: &ItemStack) -> Option<(&'static ToolInfo, &[ResolvedRule])> {
        if stack.is_empty() {
            return None;
        }
        if stack
            .patch()
            .is_some_and(|p| p.is_removed(ids::TOOL) || p.get(ids::TOOL).is_some())
        {
            // Patched tool components are not interpreted yet: the stack mines like a hand.
            return None;
        }
        let index = usize::from(item_id(stack).info().tool);
        Some((TOOLS.get(index)?, self.tools.get(index)?))
    }

    /// `ItemStack.getDestroySpeed`.
    pub fn destroy_speed(&self, stack: &ItemStack, state: BlockState) -> f32 {
        match self.tool(stack) {
            Some((info, rules)) => rules
                .iter()
                .find_map(|r| r.speed.filter(|_| r.contains(state)))
                .unwrap_or(info.default_mining_speed),
            None => 1.0,
        }
    }

    /// `ItemStack.isCorrectToolForDrops`.
    pub fn is_correct_for_drops(&self, stack: &ItemStack, state: BlockState) -> bool {
        self.tool(stack).is_some_and(|(_, rules)| {
            rules
                .iter()
                .find_map(|r| r.correct_for_drops.filter(|_| r.contains(state)))
                .unwrap_or(false)
        })
    }

    /// Whether the stack has a `minecraft:tool` component.
    pub fn is_tool(&self, stack: &ItemStack) -> bool {
        self.tool(stack).is_some()
    }

    /// `Tool.damagePerBlock`, 0 without a tool.
    pub fn damage_per_block(&self, stack: &ItemStack) -> u32 {
        self.tool(stack)
            .map_or(0, |(info, _)| info.damage_per_block)
    }

    /// `Item.canDestroyBlock`: tools that cannot destroy blocks in creative (swords, maces,
    /// tridents) cannot.
    pub fn can_destroy_in_creative(&self, stack: &ItemStack) -> bool {
        self.tool(stack)
            .is_none_or(|(info, _)| info.can_destroy_blocks_in_creative)
    }
}

/// `ItemStack.matches`: same item, count and components.
pub fn matches(a: &ItemStack, b: &ItemStack) -> bool {
    (a.is_empty() && b.is_empty()) || (a.count == b.count && a.same_item_same_components(b))
}

/// The attribute id of `name` (index into `ATTRIBUTES`).
pub fn attribute_id(name: &str) -> Option<u16> {
    vanillite_data::entity::ATTRIBUTES
        .iter()
        .position(|a| a.name == name)
        .and_then(|i| u16::try_from(i).ok())
}

/// The `attribute_modifiers` of a stack for `attribute` while worn in `slot`, as
/// `(operation, amount)`, followed by those of its enchantments (`minecraft:attributes`).
/// Patched modifier components are not decoded yet: a stack uses its item's defaults.
pub fn attribute_modifiers(
    stack: &ItemStack,
    slot: EquipmentSlot,
    attribute: &str,
    out: &mut Vec<(u8, f64)>,
) {
    if stack.is_empty() {
        return;
    }
    let removed = stack
        .patch()
        .is_some_and(|p| p.is_removed(ids::ATTRIBUTE_MODIFIERS));
    if !removed && let Some(id) = attribute_id(attribute) {
        for m in item_id(stack).info().modifiers {
            if m.attribute == id && crate::enchant::slot_group_matches(m.slot, slot) {
                out.push((m.operation, m.amount));
            }
        }
    }
    if let Some(data) = crate::player::play::game_data() {
        data.enchants
            .attribute_modifiers(stack, slot, attribute, out);
    }
}

/// `AttributeInstance.calculateValue`: the base plus the modifiers in operation order,
/// clamped to the attribute's range.
pub fn attribute_value(base: f64, modifiers: &[(u8, f64)], min: f64, max: f64) -> f64 {
    let mut v = base;
    for &(op, amount) in modifiers {
        if op == 0 {
            v += amount;
        }
    }
    let mut result = v;
    for &(op, amount) in modifiers {
        if op == 1 {
            result += v * amount;
        }
    }
    for &(op, amount) in modifiers {
        if op == 2 {
            result *= 1.0 + amount;
        }
    }
    result.clamp(min, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sword_attack_damage_and_speed() {
        let sword = ItemStack::new(vanillite_data::item::ids::DIAMOND_SWORD.0, 1);
        let mut damage = Vec::new();
        attribute_modifiers(
            &sword,
            EquipmentSlot::MainHand,
            "minecraft:attack_damage",
            &mut damage,
        );
        assert!((attribute_value(1.0, &damage, 0.0, 2048.0) - 7.0).abs() < 1e-9);
        let mut speed = Vec::new();
        attribute_modifiers(
            &sword,
            EquipmentSlot::MainHand,
            "minecraft:attack_speed",
            &mut speed,
        );
        assert!((attribute_value(4.0, &speed, 0.0, 1024.0) - 1.6).abs() < 1e-6);
        // In the off hand it does nothing.
        let mut off = Vec::new();
        attribute_modifiers(
            &sword,
            EquipmentSlot::OffHand,
            "minecraft:attack_damage",
            &mut off,
        );
        assert!(off.is_empty());
    }

    #[test]
    fn attribute_operations_in_order() {
        // (base + add) * (1 + mult_base) * (1 + mult_total).
        let mods = [(2, 0.5), (0, 2.0), (1, 1.0)];
        assert!((attribute_value(1.0, &mods, 0.0, 100.0) - 9.0).abs() < 1e-9);
    }
}

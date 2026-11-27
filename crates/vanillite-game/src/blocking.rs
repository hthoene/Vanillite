//! The `blocks_attacks` component (`BlocksAttacks`, shields): the delay before a raised item
//! blocks, how much of a hit it takes, what the hit costs the item and how long a disabling
//! weapon puts it on cool-down. Read from the stack's patch or the item's default component.

use std::borrow::Cow;

use vanillite_protocol::codec::Reader;
use vanillite_protocol::components::ids;

use crate::consumable::{
    HolderSet, Patched, Sound, patched, read_bool, read_holder_set, read_optional_sound,
};
use crate::item::{self, ItemStack};
use crate::survival::damage::{DamageKind, DamageTypes, tag};

/// A set of damage types (`HolderSet<DamageType>`).
#[derive(Clone, Debug, PartialEq)]
pub enum DamageSet {
    /// A damage type tag (without `#`); only the tags the game tracks ([`tag::NAMES`]) match.
    Tag(Cow<'static, str>),
    /// Network ids.
    Ids(Vec<u16>),
    /// Registry names.
    Names(&'static [&'static str]),
}

impl DamageSet {
    fn of_names(names: &'static [&'static str]) -> Option<DamageSet> {
        match names {
            [] => None,
            [one] if one.starts_with('#') => {
                Some(DamageSet::Tag(Cow::Borrowed(one.trim_start_matches('#'))))
            }
            _ => Some(DamageSet::Names(names)),
        }
    }

    fn read(r: &mut Reader<'_>) -> Option<DamageSet> {
        Some(match read_holder_set(r)? {
            HolderSet::Tag(name) => DamageSet::Tag(Cow::Owned(name)),
            HolderSet::Ids(ids) => DamageSet::Ids(ids),
        })
    }

    fn read_optional(r: &mut Reader<'_>) -> Option<Option<DamageSet>> {
        if read_bool(r)? {
            DamageSet::read(r).map(Some)
        } else {
            Some(None)
        }
    }

    /// `HolderSet.contains(source.typeHolder())`.
    pub fn contains(&self, types: &DamageTypes, kind: DamageKind) -> bool {
        match self {
            DamageSet::Tag(name) => tag::NAMES
                .iter()
                .find(|(n, _)| *n == name.as_ref())
                .is_some_and(|&(_, bit)| types.is(kind, bit)),
            DamageSet::Ids(ids) => ids.contains(&kind.0),
            DamageSet::Names(names) => names.iter().any(|n| types.kind(n) == Some(kind)),
        }
    }
}

/// `BlocksAttacks.DamageReduction`.
#[derive(Clone, Debug, PartialEq)]
pub struct DamageReduction {
    pub horizontal_blocking_angle: f32,
    /// The damage types it applies to (`None`: all).
    pub types: Option<DamageSet>,
    pub base: f32,
    pub factor: f32,
}

impl DamageReduction {
    /// `resolve(source, dealtDamage, angle)`.
    fn resolve(&self, types: &DamageTypes, kind: DamageKind, damage: f32, angle: f64) -> f32 {
        if angle > f64::from(std::f32::consts::PI / 180.0 * self.horizontal_blocking_angle) {
            return 0.0;
        }
        if self
            .types
            .as_ref()
            .is_some_and(|t| !t.contains(types, kind))
        {
            return 0.0;
        }
        (self.base + self.factor * damage).clamp(0.0, damage)
    }
}

/// The `blocks_attacks` component.
#[derive(Clone, Debug, PartialEq)]
pub struct BlocksAttacks {
    pub block_delay_seconds: f32,
    pub disable_cooldown_scale: f32,
    pub damage_reductions: Vec<DamageReduction>,
    /// `ItemDamageFunction`: threshold, base, factor.
    pub item_damage: [f32; 3],
    pub bypassed_by: Option<DamageSet>,
    pub block_sound: Option<Sound>,
    pub disabled_sound: Option<Sound>,
}

/// `Math.round(float)`.
#[allow(clippy::cast_possible_truncation)]
fn round(v: f32) -> i32 {
    (v + 0.5).floor() as i32
}

impl BlocksAttacks {
    /// `blockDelayTicks`.
    pub fn block_delay_ticks(&self) -> i32 {
        round(self.block_delay_seconds * 20.0)
    }

    /// `disableBlockingForTicks(baseSeconds)`.
    pub fn disable_ticks(&self, base_seconds: f32) -> i32 {
        let seconds = base_seconds * self.disable_cooldown_scale;
        if seconds > 0.0 {
            round(seconds * 20.0)
        } else {
            0
        }
    }

    /// `resolveBlockedDamage(source, dealtDamage, angle)`.
    pub fn resolve_blocked_damage(
        &self,
        types: &DamageTypes,
        kind: DamageKind,
        damage: f32,
        angle: f64,
    ) -> f32 {
        let blocked: f32 = self
            .damage_reductions
            .iter()
            .map(|r| r.resolve(types, kind, damage, angle))
            .sum();
        blocked.clamp(0.0, damage)
    }

    /// `ItemDamageFunction.apply(dealtDamage)`: the durability a blocked hit costs.
    #[allow(clippy::cast_possible_truncation)]
    pub fn item_damage(&self, blocked: f32) -> i32 {
        let [threshold, base, factor] = self.item_damage;
        if blocked < threshold {
            0
        } else {
            (base + factor * blocked).floor() as i32
        }
    }

    /// Whether damage of `kind` goes through (`bypassed_by`).
    pub fn bypassed_by(&self, types: &DamageTypes, kind: DamageKind) -> bool {
        self.bypassed_by
            .as_ref()
            .is_some_and(|set| set.contains(types, kind))
    }
}

/// `BlocksAttacks.STREAM_CODEC`.
fn decode(v: &[u8]) -> Option<BlocksAttacks> {
    let mut r = Reader::new(v);
    let block_delay_seconds = r.f32().ok()?;
    let disable_cooldown_scale = r.f32().ok()?;
    let count = usize::try_from(r.varint().ok()?).ok()?;
    let mut damage_reductions = Vec::with_capacity(count.min(16));
    for _ in 0..count {
        damage_reductions.push(DamageReduction {
            horizontal_blocking_angle: r.f32().ok()?,
            types: DamageSet::read_optional(&mut r)?,
            base: r.f32().ok()?,
            factor: r.f32().ok()?,
        });
    }
    Some(BlocksAttacks {
        block_delay_seconds,
        disable_cooldown_scale,
        damage_reductions,
        item_damage: [r.f32().ok()?, r.f32().ok()?, r.f32().ok()?],
        bypassed_by: DamageSet::read_optional(&mut r)?,
        block_sound: read_optional_sound(&mut r)?,
        disabled_sound: read_optional_sound(&mut r)?,
    })
}

/// The stack's `blocks_attacks` component.
pub fn blocks_attacks(stack: &ItemStack) -> Option<BlocksAttacks> {
    if stack.is_empty() {
        return None;
    }
    match patched(stack, ids::BLOCKS_ATTACKS) {
        Patched::Value(v) => decode(v),
        Patched::Removed => None,
        Patched::Default => {
            let b = item::item_id(stack).info().blocks_attacks?;
            let sound = |s: &'static str| (!s.is_empty()).then_some(Cow::Borrowed(s));
            Some(BlocksAttacks {
                block_delay_seconds: b.block_delay_seconds,
                disable_cooldown_scale: b.disable_cooldown_scale,
                damage_reductions: b
                    .damage_reductions
                    .iter()
                    .map(|r| DamageReduction {
                        horizontal_blocking_angle: r.horizontal_blocking_angle,
                        types: DamageSet::of_names(r.types),
                        base: r.base,
                        factor: r.factor,
                    })
                    .collect(),
                item_damage: b.item_damage,
                bypassed_by: DamageSet::of_names(b.bypassed_by),
                block_sound: sound(b.block_sound),
                disabled_sound: sound(b.disabled_sound),
            })
        }
    }
}

/// `blockDelayTicks` of the stack's `blocks_attacks` component, `None` without one.
pub fn block_delay_ticks(stack: &ItemStack) -> Option<i32> {
    if stack.is_empty() {
        return None;
    }
    let seconds = match patched(stack, ids::BLOCKS_ATTACKS) {
        Patched::Value(v) => Reader::new(v).f32().ok()?,
        Patched::Removed => return None,
        Patched::Default => {
            item::item_id(stack)
                .info()
                .blocks_attacks?
                .block_delay_seconds
        }
    };
    Some(round(seconds * 20.0))
}

/// Whether the stack has a `blocks_attacks` component.
pub fn has_blocks_attacks(stack: &ItemStack) -> bool {
    if stack.is_empty() {
        return false;
    }
    match patched(stack, ids::BLOCKS_ATTACKS) {
        Patched::Value(_) => true,
        Patched::Removed => false,
        Patched::Default => item::item_id(stack).info().blocks_attacks.is_some(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shield_defaults() {
        let shield = ItemStack::new(
            vanillite_data::registry::ITEM
                .id("minecraft:shield")
                .unwrap_or(0),
            1,
        );
        let b = blocks_attacks(&shield).expect("shield");
        assert_eq!(b.block_delay_ticks(), 5);
        assert_eq!(b.disable_ticks(5.0), 100);
        assert_eq!(b.item_damage(2.9), 0);
        assert_eq!(b.item_damage(3.0), 4);
        let types = DamageTypes::built_in();
        let mob = types.named("minecraft:mob_attack");
        let quarter = std::f64::consts::FRAC_PI_4;
        assert!((b.resolve_blocked_damage(&types, mob, 6.0, quarter) - 6.0).abs() < 1e-6);
        assert!(b.resolve_blocked_damage(&types, mob, 6.0, 3.0).abs() < 1e-6);
        assert!(!b.bypassed_by(&types, mob));
        // `bypassed_by` is the tag `#minecraft:bypasses_shield`.
        for id in ["minecraft:magic", "minecraft:fall", "minecraft:arrow"] {
            let kind = types.named(id);
            assert_eq!(
                b.bypassed_by(&types, kind),
                types.is(kind, tag::BYPASSES_SHIELD),
                "{id}"
            );
        }
    }
}

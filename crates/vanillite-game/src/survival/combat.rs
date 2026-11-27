//! `CombatTracker` (the damage a living entity took recently, for its death message) and
//! `CombatRules` (armour).

use vanillite_protocol::text::{ClickEvent, Component, HoverEvent};

use super::damage::{DamageSource, DamageTypes, DeathMessageType, tag};

/// `FallLocation`: where a fall started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FallLocation {
    Generic,
    Ladder,
    Vines,
    WeepingVines,
    TwistingVines,
    Scaffolding,
    OtherClimbable,
    Water,
}

impl FallLocation {
    /// `languageKey`.
    pub fn language_key(self) -> &'static str {
        match self {
            FallLocation::Generic => "death.fell.accident.generic",
            FallLocation::Ladder => "death.fell.accident.ladder",
            FallLocation::Vines => "death.fell.accident.vines",
            FallLocation::WeepingVines => "death.fell.accident.weeping_vines",
            FallLocation::TwistingVines => "death.fell.accident.twisting_vines",
            FallLocation::Scaffolding => "death.fell.accident.scaffolding",
            FallLocation::OtherClimbable => "death.fell.accident.other_climbable",
            FallLocation::Water => "death.fell.accident.water",
        }
    }

    /// `blockToFallLocation` by block name.
    pub fn of_block(name: &str, is_trapdoor: bool) -> FallLocation {
        match name {
            "minecraft:ladder" => FallLocation::Ladder,
            _ if is_trapdoor => FallLocation::Ladder,
            "minecraft:vine" => FallLocation::Vines,
            "minecraft:weeping_vines" | "minecraft:weeping_vines_plant" => {
                FallLocation::WeepingVines
            }
            "minecraft:twisting_vines" | "minecraft:twisting_vines_plant" => {
                FallLocation::TwistingVines
            }
            "minecraft:scaffolding" => FallLocation::Scaffolding,
            _ => FallLocation::OtherClimbable,
        }
    }
}

/// `getDisplayName` of an entity behind a damage source and the display name of the item in
/// its main hand when that has a custom name (`getMainHandItem`, living entities only).
#[derive(Clone, Debug, PartialEq)]
pub struct AttackerName {
    pub name: Component,
    pub item: Option<Component>,
}

/// The names of a damage source's entities, resolved when the damage is taken by callers that
/// have the entities (the death message needs them after the entities may be gone).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SourceNames {
    /// The causing entity (`getEntity`).
    pub cause: Option<AttackerName>,
    /// The direct entity (`getDirectEntity`).
    pub direct: Option<Component>,
    /// The player responsible for the damage (`resolvePlayerResponsibleForDamage`).
    pub responsible: Option<Component>,
}

/// `CombatEntry`.
#[derive(Clone, Debug, PartialEq)]
pub struct CombatEntry {
    pub source: DamageSource,
    pub damage: f32,
    pub fall_location: Option<FallLocation>,
    pub fall_distance: f32,
    /// The names of the source's entities.
    pub names: SourceNames,
}

/// `CombatTracker`. `tick_count` is the owner's `tickCount`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CombatTracker {
    entries: Vec<CombatEntry>,
    last_damage_time: i32,
    combat_start_time: i32,
    combat_end_time: i32,
    in_combat: bool,
    taking_damage: bool,
}

/// What [`CombatTracker::record_damage`] changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CombatChange {
    /// The combat duration when the owner left combat first.
    pub left: Option<i32>,
    /// The owner entered combat.
    pub entered: bool,
}

/// The `chat.square_brackets` link of `INTENTIONAL_GAME_DESIGN` death messages.
const INTENTIONAL_GAME_DESIGN_URL: &str = "https://bugs.mojang.com/browse/MCPE-28723";

impl CombatTracker {
    /// `recordDamage`: combat starts when the source is a living entity. Returns what changed
    /// (`onLeaveCombat` of the recheck, then `onEnterCombat`).
    pub fn record_damage(
        &mut self,
        entry: CombatEntry,
        tick_count: i32,
        alive: bool,
        source_is_living: bool,
    ) -> CombatChange {
        let left = self.recheck_status(tick_count, alive);
        self.entries.push(entry);
        self.last_damage_time = tick_count;
        self.taking_damage = true;
        let entered = !self.in_combat && alive && source_is_living;
        if entered {
            self.in_combat = true;
            self.combat_start_time = tick_count;
            self.combat_end_time = tick_count;
        }
        CombatChange { left, entered }
    }

    /// `recheckStatus`: forgets the entries 100 ticks after the last damage (300 in combat) or
    /// after death. Returns the combat duration when the owner left combat (`onLeaveCombat`).
    pub fn recheck_status(&mut self, tick_count: i32, alive: bool) -> Option<i32> {
        let reset = if self.in_combat { 300 } else { 100 };
        if self.taking_damage && (!alive || tick_count.wrapping_sub(self.last_damage_time) > reset)
        {
            let was_in_combat = self.in_combat;
            self.taking_damage = false;
            self.in_combat = false;
            self.combat_end_time = tick_count;
            self.entries.clear();
            return was_in_combat.then(|| self.combat_duration(tick_count));
        }
        None
    }

    /// `getCombatDuration`.
    pub fn combat_duration(&self, tick_count: i32) -> i32 {
        if self.in_combat {
            tick_count.wrapping_sub(self.combat_start_time)
        } else {
            self.combat_end_time.wrapping_sub(self.combat_start_time)
        }
    }

    /// `getMostSignificantFall`.
    fn most_significant_fall(&self, types: &DamageTypes) -> Option<&CombatEntry> {
        let mut result = None;
        let mut alternative = None;
        let mut alt_damage = 0.0f32;
        let mut best_fall = 0.0f32;
        for (i, entry) in self.entries.iter().enumerate() {
            let is_fake_fall = types.is(entry.source.kind, tag::ALWAYS_MOST_SIGNIFICANT_FALL);
            let fall_distance = if is_fake_fall {
                f32::MAX
            } else {
                entry.fall_distance
            };
            if (types.is(entry.source.kind, tag::IS_FALL) || is_fake_fall)
                && fall_distance > 0.0
                && (result.is_none() || fall_distance > best_fall)
            {
                result = if i > 0 {
                    self.entries.get(i - 1)
                } else {
                    Some(entry)
                };
                best_fall = fall_distance;
            }
            if entry.fall_location.is_some() && (alternative.is_none() || entry.damage > alt_damage)
            {
                alternative = Some(entry);
                alt_damage = entry.damage;
            }
        }
        if best_fall > 5.0 && result.is_some() {
            result
        } else if alt_damage > 5.0 && alternative.is_some() {
            alternative
        } else {
            None
        }
    }

    /// `getDeathMessage`. `victim` is the victim's display name, `kill_credit` the display
    /// name of its `getKillCredit`.
    pub fn death_message(
        &self,
        types: &DamageTypes,
        victim: Component,
        kill_credit: Option<Component>,
    ) -> Component {
        let Some(killing) = self.entries.last() else {
            return Component::translatable("death.attack.generic", vec![victim]);
        };
        let killing_type = types.get(killing.source.kind);
        let knock_off = self.most_significant_fall(types);
        match (killing_type.death_message_type, knock_off) {
            (DeathMessageType::FallVariants, Some(knock_off)) => {
                fall_message(types, knock_off, killing, victim)
            }
            (DeathMessageType::IntentionalGameDesign, _) => {
                let key = format!("death.attack.{}", killing_type.msg_id);
                let link = Component::translatable(
                    "chat.square_brackets",
                    vec![Component::translatable(format!("{key}.link"), vec![])],
                )
                .click(ClickEvent::OpenUrl(INTENTIONAL_GAME_DESIGN_URL.to_owned()))
                .hover(HoverEvent::ShowText(Component::text("MCPE-28723")));
                Component::translatable(format!("{key}.message"), vec![victim, link])
            }
            _ => localized_death_message(types, killing, victim, kill_credit),
        }
    }
}

/// The display name of a source's causing entity, else the type name it was recorded with.
fn cause_name(entry: &CombatEntry) -> Option<AttackerName> {
    entry.source.cause?;
    entry.names.cause.clone().or_else(|| {
        entry.source.cause_name.map(|key| AttackerName {
            name: Component::translatable(key, vec![]),
            item: None,
        })
    })
}

/// `CombatTracker.getFallMessage`.
fn fall_message(
    types: &DamageTypes,
    knock_off: &CombatEntry,
    killing: &CombatEntry,
    victim: Component,
) -> Component {
    let k = knock_off.source.kind;
    if types.is(k, tag::IS_FALL) || types.is(k, tag::ALWAYS_MOST_SIGNIFICANT_FALL) {
        let location = knock_off.fall_location.unwrap_or(FallLocation::Generic);
        return Component::translatable(location.language_key(), vec![victim]);
    }
    let killer = cause_name(killing);
    let attacker = cause_name(knock_off);
    // `getMessageForAssistedFall`.
    let assisted = |a: AttackerName, with_item: &str, without_item: &str| match a.item {
        Some(item) => Component::translatable(with_item, vec![victim.clone(), a.name, item]),
        None => Component::translatable(without_item, vec![victim.clone(), a.name]),
    };
    match (attacker, killer) {
        (Some(a), k) if k.as_ref().is_none_or(|k| k.name != a.name) => {
            assisted(a, "death.fell.assist.item", "death.fell.assist")
        }
        (_, Some(k)) => assisted(k, "death.fell.finish.item", "death.fell.finish"),
        (_, None) => Component::translatable("death.fell.killer", vec![victim]),
    }
}

/// `DamageSource.getLocalizedDeathMessage`.
fn localized_death_message(
    types: &DamageTypes,
    killing: &CombatEntry,
    victim: Component,
    kill_credit: Option<Component>,
) -> Component {
    let key = format!("death.attack.{}", types.get(killing.source.kind).msg_id);
    let source = &killing.source;
    if source.cause.is_none() && source.direct.is_none() {
        return match kill_credit {
            Some(credit) => Component::translatable(format!("{key}.player"), vec![victim, credit]),
            None => Component::translatable(key, vec![victim]),
        };
    }
    let (name, item) = match cause_name(killing) {
        Some(a) => (a.name, a.item),
        None if source.cause.is_some() => (Component::empty(), None),
        None => (
            killing
                .names
                .direct
                .clone()
                .unwrap_or_else(Component::empty),
            None,
        ),
    };
    match item {
        Some(item) => Component::translatable(format!("{key}.item"), vec![victim, name, item]),
        None => Component::translatable(key, vec![victim, name]),
    }
}

/// `CombatRules.getDamageAfterAbsorb` without weapon enchantments.
pub fn damage_after_absorb(damage: f32, total_armor: f32, armor_toughness: f32) -> f32 {
    damage_after_absorb_with(damage, total_armor, armor_toughness, None)
}

/// `CombatRules.getDamageAfterAbsorb`: `modify` is the armour effectiveness of the source's
/// weapon (`EnchantmentHelper.modifyArmorEffectiveness`, Breach), clamped to 0..=1.
pub fn damage_after_absorb_with(
    damage: f32,
    total_armor: f32,
    armor_toughness: f32,
    modify: Option<&dyn Fn(f32) -> f32>,
) -> f32 {
    let toughness = 2.0 + armor_toughness / 4.0;
    let real_armor = (total_armor - damage / toughness).clamp(total_armor * 0.2, 20.0);
    let fraction = real_armor / 25.0;
    let fraction = modify.map_or(fraction, |m| m(fraction).clamp(0.0, 1.0));
    damage * (1.0 - fraction)
}

/// `CombatRules.getDamageAfterMagicAbsorb`.
pub fn damage_after_magic_absorb(damage: f32, total_magic_armor: f32) -> f32 {
    let real_armor = total_magic_armor.clamp(0.0, 20.0);
    damage * (1.0 - real_armor / 25.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::survival::damage::DamageSource;
    use vanillite_world::entity::EntityId;

    /// `/kill Bob` on Vanilla 26.3: the death screen packet carries the same message.
    #[test]
    fn kill_death_message_matches_vanilla() {
        use vanillite_protocol::packets::play::clientbound::PlayerCombatKill;
        use vanillite_protocol::sets::ClientboundPlay;
        let vanilla: &[u8] =
            include_bytes!("../../../vanillite-protocol/tests/vectors/play/player_combat_kill.bin");
        let Ok(ClientboundPlay::PlayerCombatKill(packet)) = ClientboundPlay::decode(vanilla) else {
            panic!("vector is no player_combat_kill");
        };
        let types = DamageTypes::built_in();
        let mut tracker = CombatTracker::default();
        let entry = CombatEntry {
            source: DamageSource::of(types.generic_kill),
            damage: f32::MAX,
            fall_location: None,
            fall_distance: 0.0,
            names: SourceNames::default(),
        };
        tracker.record_damage(entry, 1, true, false);
        let uuid = vanillite_protocol::types::Uuid::offline("Bob").0;
        let name = crate::player::join::display_name("Bob", uuid);
        let ours = PlayerCombatKill {
            player_id: packet.player_id,
            message: tracker.death_message(&types, name, None),
        };
        assert_eq!(
            ClientboundPlay::PlayerCombatKill(ours).encode_body(),
            vanilla
        );
    }

    fn hit(source: DamageSource, names: SourceNames, fall_distance: f32) -> CombatEntry {
        CombatEntry {
            source,
            damage: 4.0,
            fall_location: None,
            fall_distance,
            names,
        }
    }

    /// The types the death message tests need (the built-in set lacks the entity sources).
    fn message_types() -> DamageTypes {
        use crate::survival::damage::{DamageType, Effects, Scaling};
        let t = |id: &str, msg: &str, dm: DeathMessageType, tags: u32| DamageType {
            id: format!("minecraft:{id}"),
            msg_id: msg.to_owned(),
            scaling: Scaling::Never,
            exhaustion: 0.1,
            effects: Effects::Hurt,
            death_message_type: dm,
            tags,
        };
        DamageTypes::new(vec![
            t("generic", "generic", DeathMessageType::Default, 0),
            t("fall", "fall", DeathMessageType::FallVariants, tag::IS_FALL),
            t("lava", "lava", DeathMessageType::Default, 0),
            t(
                "indirect_magic",
                "indirectMagic",
                DeathMessageType::Default,
                0,
            ),
            t("player_attack", "player", DeathMessageType::Default, 0),
            t("arrow", "arrow", DeathMessageType::Default, 0),
        ])
    }

    fn eid(n: u64) -> EntityId {
        use vanillite_base::slot_map::{RawKey, SlotKey};
        EntityId::from_raw(RawKey::from_bits(1 << 32 | n).expect("live key"))
    }

    fn named(name: &str) -> Option<AttackerName> {
        Some(AttackerName {
            name: Component::text(name),
            item: None,
        })
    }

    /// Dragon breath (#231): `indirectMagic(cloud, dragon)` names the dragon, never a raw
    /// `%2$s`.
    #[test]
    fn entity_death_messages_name_the_cause() {
        let types = message_types();
        let mut tracker = CombatTracker::default();
        let source = DamageSource {
            cause: Some(eid(7)),
            direct: Some(eid(8)),
            ..DamageSource::of(types.indirect_magic)
        };
        let names = SourceNames {
            cause: named("Ender Dragon"),
            direct: Some(Component::text("Area Effect Cloud")),
            responsible: None,
        };
        tracker.record_damage(hit(source, names, 0.0), 1, true, true);
        assert_eq!(
            tracker.death_message(&types, Component::text("Alice"), None),
            Component::translatable(
                "death.attack.indirectMagic",
                vec![Component::text("Alice"), Component::text("Ender Dragon")]
            )
        );
        // A player's custom-named weapon: the `.item` variant.
        let mut tracker = CombatTracker::default();
        let source = DamageSource {
            cause: Some(eid(9)),
            direct: Some(eid(9)),
            ..DamageSource::of(types.player_attack)
        };
        let names = SourceNames {
            cause: Some(AttackerName {
                name: Component::text("Bob"),
                item: Some(Component::text("[Sting]")),
            }),
            ..SourceNames::default()
        };
        tracker.record_damage(hit(source, names, 0.0), 1, true, true);
        assert_eq!(
            tracker.death_message(&types, Component::text("Alice"), None),
            Component::translatable(
                "death.attack.player.item",
                vec![
                    Component::text("Alice"),
                    Component::text("Bob"),
                    Component::text("[Sting]")
                ]
            )
        );
        // An arrow without owner names the arrow.
        let mut tracker = CombatTracker::default();
        let source = DamageSource {
            direct: Some(eid(3)),
            ..DamageSource::of(types.arrow)
        };
        let names = SourceNames {
            direct: Some(Component::text("Arrow")),
            ..SourceNames::default()
        };
        tracker.record_damage(hit(source, names, 0.0), 1, true, false);
        assert_eq!(
            tracker.death_message(&types, Component::text("Alice"), None),
            Component::translatable(
                "death.attack.arrow",
                vec![Component::text("Alice"), Component::text("Arrow")]
            )
        );
    }

    /// `getKillCredit` of sources without entities, and an assisted fall.
    #[test]
    fn kill_credit_and_assisted_fall() {
        let types = message_types();
        let mut tracker = CombatTracker::default();
        tracker.record_damage(
            hit(DamageSource::of(types.lava), SourceNames::default(), 0.0),
            1,
            true,
            false,
        );
        assert_eq!(
            tracker.death_message(
                &types,
                Component::text("Alice"),
                Some(Component::text("Zed"))
            ),
            Component::translatable(
                "death.attack.lava.player",
                vec![Component::text("Alice"), Component::text("Zed")]
            )
        );
        let mut tracker = CombatTracker::default();
        let knock = DamageSource {
            cause: Some(eid(5)),
            direct: Some(eid(5)),
            ..DamageSource::of(types.player_attack)
        };
        let names = SourceNames {
            cause: named("Bob"),
            ..SourceNames::default()
        };
        tracker.record_damage(hit(knock, names, 0.0), 1, true, true);
        let mut fall = hit(DamageSource::of(types.fall), SourceNames::default(), 12.0);
        fall.damage = 20.0;
        tracker.record_damage(fall, 30, true, false);
        assert_eq!(
            tracker.death_message(&types, Component::text("Alice"), None),
            Component::translatable(
                "death.fell.assist",
                vec![Component::text("Alice"), Component::text("Bob")]
            )
        );
    }

    #[test]
    fn armour_formula() {
        // Full diamond (20 armour, 8 toughness) against 10 damage: 20 - 10/4 = 17.5 → 30%.
        assert!((damage_after_absorb(10.0, 20.0, 8.0) - 3.0).abs() < 1e-5);
        assert_eq!(damage_after_absorb(7.0, 0.0, 0.0), 7.0);
    }

    #[test]
    fn fall_death_message_uses_the_fall_location() {
        let types = DamageTypes::built_in();
        let mut tracker = CombatTracker::default();
        tracker.record_damage(
            CombatEntry {
                source: DamageSource::of(types.fall),
                damage: 20.0,
                fall_location: None,
                fall_distance: 23.0,
                names: SourceNames::default(),
            },
            10,
            true,
            false,
        );
        let message = tracker.death_message(&types, Component::text("Alice"), None);
        assert_eq!(
            message,
            Component::translatable(
                "death.fell.accident.generic",
                vec![Component::text("Alice")]
            )
        );
        // A short fall dies with the plain fall message.
        let mut tracker = CombatTracker::default();
        tracker.record_damage(
            CombatEntry {
                source: DamageSource::of(types.fall),
                damage: 2.0,
                fall_location: None,
                fall_distance: 5.0,
                names: SourceNames::default(),
            },
            10,
            true,
            false,
        );
        let message = tracker.death_message(&types, Component::text("Alice"), None);
        assert_eq!(
            message,
            Component::translatable("death.attack.fall", vec![Component::text("Alice")])
        );
    }
}

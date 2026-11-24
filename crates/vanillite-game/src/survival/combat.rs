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

/// `CombatEntry`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CombatEntry {
    pub source: DamageSource,
    pub damage: f32,
    pub fall_location: Option<FallLocation>,
    pub fall_distance: f32,
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

/// The `chat.square_brackets` link of `INTENTIONAL_GAME_DESIGN` death messages.
const INTENTIONAL_GAME_DESIGN_URL: &str = "https://bugs.mojang.com/browse/MCPE-28723";

impl CombatTracker {
    /// `recordDamage`. `entered_combat` is set when the source is a living entity.
    pub fn record_damage(
        &mut self,
        entry: CombatEntry,
        tick_count: i32,
        alive: bool,
        source_is_living: bool,
    ) {
        self.recheck_status(tick_count, alive);
        self.entries.push(entry);
        self.last_damage_time = tick_count;
        self.taking_damage = true;
        if !self.in_combat && alive && source_is_living {
            self.in_combat = true;
            self.combat_start_time = tick_count;
            self.combat_end_time = tick_count;
        }
    }

    /// `recheckStatus`: forgets the entries 100 ticks after the last damage (300 in combat) or
    /// after death.
    pub fn recheck_status(&mut self, tick_count: i32, alive: bool) {
        let reset = if self.in_combat { 300 } else { 100 };
        if self.taking_damage && (!alive || tick_count.wrapping_sub(self.last_damage_time) > reset)
        {
            self.taking_damage = false;
            self.in_combat = false;
            self.combat_end_time = tick_count;
            self.entries.clear();
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

    /// `getDeathMessage` for a victim without kill credit and sources without entities.
    /// `victim` is the victim's display name.
    pub fn death_message(&self, types: &DamageTypes, victim: Component) -> Component {
        let Some(killing) = self.entries.last() else {
            return Component::translatable("death.attack.generic", vec![victim]);
        };
        let killing_type = types.get(killing.source.kind);
        let knock_off = self.most_significant_fall(types);
        match (killing_type.death_message_type, knock_off) {
            (DeathMessageType::FallVariants, Some(knock_off)) => {
                let k = knock_off.source.kind;
                if types.is(k, tag::IS_FALL) || types.is(k, tag::ALWAYS_MOST_SIGNIFICANT_FALL) {
                    let location = knock_off.fall_location.unwrap_or(FallLocation::Generic);
                    Component::translatable(location.language_key(), vec![victim])
                } else {
                    Component::translatable("death.fell.killer", vec![victim])
                }
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
            _ => Component::translatable(
                format!("death.attack.{}", killing_type.msg_id),
                vec![victim],
            ),
        }
    }
}

/// `CombatRules.getDamageAfterAbsorb` without weapon enchantments.
pub fn damage_after_absorb(damage: f32, total_armor: f32, armor_toughness: f32) -> f32 {
    let toughness = 2.0 + armor_toughness / 4.0;
    let real_armor = (total_armor - damage / toughness).clamp(total_armor * 0.2, 20.0);
    damage * (1.0 - real_armor / 25.0)
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
        };
        tracker.record_damage(entry, 1, true, false);
        let uuid = vanillite_protocol::types::Uuid::offline("Bob").0;
        let name = crate::player::join::display_name("Bob", uuid);
        let ours = PlayerCombatKill {
            player_id: packet.player_id,
            message: tracker.death_message(&types, name),
        };
        assert_eq!(
            ClientboundPlay::PlayerCombatKill(ours).encode_body(),
            vanilla
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
            },
            10,
            true,
            false,
        );
        let message = tracker.death_message(&types, Component::text("Alice"));
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
            },
            10,
            true,
            false,
        );
        let message = tracker.death_message(&types, Component::text("Alice"));
        assert_eq!(
            message,
            Component::translatable("death.attack.fall", vec![Component::text("Alice")])
        );
    }
}

//! Game rules (Vanilla's `GameRules` of 26.3) and the difficulty.
//!
//! The rules are server-wide like Vanilla's global game rules: one value per rule, saved in
//! `data/minecraft/game_rules.dat`. Worlds read a [`GameRules`] copy once per tick.

use vanillite_nbt::{Compound, Tag};

/// Value type of a rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuleType {
    /// `true`/`false`.
    Bool,
    /// An integer with the inclusive bounds of the rule.
    Int { min: i32, max: i32 },
}

/// One rule: its name without namespace, type and default.
#[derive(Clone, Copy, Debug)]
pub struct RuleInfo {
    pub name: &'static str,
    pub kind: RuleType,
    pub default: i32,
}

const fn b(name: &'static str, default: bool) -> RuleInfo {
    RuleInfo {
        name,
        kind: RuleType::Bool,
        default: if default { 1 } else { 0 },
    }
}

const fn i(name: &'static str, default: i32, min: i32, max: i32) -> RuleInfo {
    RuleInfo {
        name,
        kind: RuleType::Int { min, max },
        default,
    }
}

/// All rules in registry order (`minecraft:game_rule`).
pub const RULES: [RuleInfo; 59] = [
    b("advance_time", true),
    b("advance_weather", true),
    b("allow_entering_nether_using_portals", true),
    b("block_drops", true),
    b("block_explosion_drop_decay", true),
    b("command_blocks_work", true),
    b("command_block_output", true),
    b("drowning_damage", true),
    b("elytra_movement_check", true),
    b("ender_pearls_vanish_on_death", true),
    b("entity_drops", true),
    b("fall_damage", true),
    b("fire_damage", true),
    i("fire_spread_radius_around_player", 128, -1, i32::MAX),
    b("forgive_dead_players", true),
    b("freeze_damage", true),
    b("global_sound_events", true),
    b("immediate_respawn", false),
    b("keep_inventory", false),
    b("lava_source_conversion", false),
    b("limited_crafting", false),
    b("locator_bar", true),
    b("log_admin_commands", true),
    i("max_block_modifications", 32768, 1, i32::MAX),
    i("max_command_forks", 65536, 0, i32::MAX),
    i("max_command_sequence_length", 65536, 0, i32::MAX),
    i("max_entity_cramming", 24, 0, i32::MAX),
    i("max_minecart_speed", 8, 1, 1000),
    i("max_snow_accumulation_height", 1, 0, 8),
    b("mob_drops", true),
    b("mob_explosion_drop_decay", true),
    b("mob_griefing", true),
    b("natural_health_regeneration", true),
    b("player_movement_check", true),
    i("players_nether_portal_creative_delay", 0, 0, i32::MAX),
    i("players_nether_portal_default_delay", 80, 0, i32::MAX),
    i("players_sleeping_percentage", 100, 0, i32::MAX),
    b("projectiles_can_break_blocks", true),
    b("pvp", true),
    b("raids", true),
    i("random_tick_speed", 3, 0, i32::MAX),
    b("reduced_debug_info", false),
    i("respawn_radius", 10, 0, i32::MAX),
    b("send_command_feedback", true),
    b("show_advancement_messages", true),
    b("show_death_messages", true),
    b("spawner_blocks_work", true),
    b("spawn_mobs", true),
    b("spawn_monsters", true),
    b("spawn_patrols", true),
    b("spawn_phantoms", true),
    b("spawn_wandering_traders", true),
    b("spawn_wardens", true),
    b("spectators_generate_chunks", true),
    b("spread_vines", true),
    b("tnt_explodes", true),
    b("tnt_explosion_drop_decay", false),
    b("universal_anger", false),
    b("water_source_conversion", true),
];

/// A rule, as an index into [`RULES`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rule(pub usize);

impl Rule {
    pub const ADVANCE_TIME: Rule = Rule(0);
    pub const ADVANCE_WEATHER: Rule = Rule(1);
    pub const ALLOW_ENTERING_NETHER_USING_PORTALS: Rule = Rule(2);
    pub const DROWNING_DAMAGE: Rule = Rule(7);
    pub const FALL_DAMAGE: Rule = Rule(11);
    pub const FIRE_DAMAGE: Rule = Rule(12);
    pub const FREEZE_DAMAGE: Rule = Rule(15);
    pub const IMMEDIATE_RESPAWN: Rule = Rule(17);
    pub const KEEP_INVENTORY: Rule = Rule(18);
    pub const LIMITED_CRAFTING: Rule = Rule(20);
    pub const BLOCK_EXPLOSION_DROP_DECAY: Rule = Rule(4);
    pub const MAX_ENTITY_CRAMMING: Rule = Rule(26);
    pub const MOB_DROPS: Rule = Rule(29);
    pub const MOB_EXPLOSION_DROP_DECAY: Rule = Rule(30);
    pub const MOB_GRIEFING: Rule = Rule(31);
    pub const NATURAL_HEALTH_REGENERATION: Rule = Rule(32);
    pub const PLAYER_MOVEMENT_CHECK: Rule = Rule(33);
    pub const PLAYERS_NETHER_PORTAL_CREATIVE_DELAY: Rule = Rule(34);
    pub const PLAYERS_NETHER_PORTAL_DEFAULT_DELAY: Rule = Rule(35);
    pub const PVP: Rule = Rule(38);
    pub const REDUCED_DEBUG_INFO: Rule = Rule(41);
    pub const RESPAWN_RADIUS: Rule = Rule(42);
    pub const SHOW_ADVANCEMENT_MESSAGES: Rule = Rule(44);
    pub const SHOW_DEATH_MESSAGES: Rule = Rule(45);
    pub const SPAWN_MOBS: Rule = Rule(47);
    pub const SPAWN_MONSTERS: Rule = Rule(48);
    pub const TNT_EXPLOSION_DROP_DECAY: Rule = Rule(56);
    pub const UNIVERSAL_ANGER: Rule = Rule(57);

    /// The rule named `name`, with or without the `minecraft:` namespace.
    pub fn by_name(name: &str) -> Option<Rule> {
        let name = name.strip_prefix("minecraft:").unwrap_or(name);
        RULES.iter().position(|r| r.name == name).map(Rule)
    }

    /// Static facts of this rule.
    pub fn info(self) -> &'static RuleInfo {
        &RULES[self.0]
    }

    /// `GameRule.serialize`: `true`/`false` or the number.
    pub fn serialize(self, value: i32) -> String {
        match self.info().kind {
            RuleType::Bool => (value != 0).to_string(),
            RuleType::Int { .. } => value.to_string(),
        }
    }
}

/// The values of all rules (booleans as `0`/`1`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GameRules {
    values: [i32; RULES.len()],
}

impl Default for GameRules {
    fn default() -> Self {
        let mut values = [0; RULES.len()];
        for (v, r) in values.iter_mut().zip(RULES.iter()) {
            *v = r.default;
        }
        GameRules { values }
    }
}

impl GameRules {
    /// The value of a rule.
    #[inline]
    pub fn get(&self, rule: Rule) -> i32 {
        self.values[rule.0]
    }

    /// A boolean rule.
    #[inline]
    pub fn bool(&self, rule: Rule) -> bool {
        self.get(rule) != 0
    }

    /// Sets a rule (clamped to its bounds).
    pub fn set(&mut self, rule: Rule, value: i32) {
        self.values[rule.0] = match rule.info().kind {
            RuleType::Bool => i32::from(value != 0),
            RuleType::Int { min, max } => value.clamp(min, max),
        };
    }

    /// `GameRuleMap.CODEC`: reads `minecraft:<rule>` entries (bytes for booleans, ints for
    /// numbers); unknown or invalid entries keep their default.
    pub fn from_nbt(data: &Compound) -> GameRules {
        let mut rules = GameRules::default();
        for (key, tag) in data.iter() {
            let Some(rule) = Rule::by_name(key) else {
                continue;
            };
            if let Some(v) = tag.as_i64().and_then(|v| i32::try_from(v).ok()) {
                rules.set(rule, v);
            }
        }
        rules
    }

    /// The `data` compound of `game_rules.dat`.
    pub fn to_nbt(&self) -> Compound {
        let mut out = Compound::with_capacity(RULES.len());
        for (info, &v) in RULES.iter().zip(&self.values) {
            let key = format!("minecraft:{}", info.name);
            match info.kind {
                RuleType::Bool => out.insert(key, Tag::Byte(i8::from(v != 0))),
                RuleType::Int { .. } => out.insert(key, Tag::Int(v)),
            }
        }
        out
    }
}

/// `Difficulty`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Difficulty {
    Peaceful,
    #[default]
    Easy,
    Normal,
    Hard,
}

impl Difficulty {
    /// `Difficulty.getId`.
    pub fn id(self) -> i32 {
        match self {
            Difficulty::Peaceful => 0,
            Difficulty::Easy => 1,
            Difficulty::Normal => 2,
            Difficulty::Hard => 3,
        }
    }

    /// From `Difficulty.getId`; out of range falls back to easy.
    pub fn from_id(id: i32) -> Difficulty {
        match id {
            0 => Difficulty::Peaceful,
            2 => Difficulty::Normal,
            3 => Difficulty::Hard,
            _ => Difficulty::Easy,
        }
    }

    /// The server property value.
    pub fn from_config(d: vanillite_config::Difficulty) -> Difficulty {
        match d {
            vanillite_config::Difficulty::Peaceful => Difficulty::Peaceful,
            vanillite_config::Difficulty::Easy => Difficulty::Easy,
            vanillite_config::Difficulty::Normal => Difficulty::Normal,
            vanillite_config::Difficulty::Hard => Difficulty::Hard,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule_indices_match_names() {
        for (rule, name) in [
            (Rule::ADVANCE_TIME, "advance_time"),
            (Rule::ADVANCE_WEATHER, "advance_weather"),
            (Rule::DROWNING_DAMAGE, "drowning_damage"),
            (Rule::FALL_DAMAGE, "fall_damage"),
            (Rule::FIRE_DAMAGE, "fire_damage"),
            (Rule::FREEZE_DAMAGE, "freeze_damage"),
            (Rule::IMMEDIATE_RESPAWN, "immediate_respawn"),
            (Rule::KEEP_INVENTORY, "keep_inventory"),
            (Rule::LIMITED_CRAFTING, "limited_crafting"),
            (Rule::MOB_GRIEFING, "mob_griefing"),
            (
                Rule::NATURAL_HEALTH_REGENERATION,
                "natural_health_regeneration",
            ),
            (Rule::PLAYER_MOVEMENT_CHECK, "player_movement_check"),
            (Rule::PVP, "pvp"),
            (Rule::REDUCED_DEBUG_INFO, "reduced_debug_info"),
            (Rule::RESPAWN_RADIUS, "respawn_radius"),
            (Rule::SHOW_ADVANCEMENT_MESSAGES, "show_advancement_messages"),
            (Rule::SHOW_DEATH_MESSAGES, "show_death_messages"),
            (Rule::SPAWN_MOBS, "spawn_mobs"),
        ] {
            assert_eq!(rule.info().name, name);
        }
    }

    #[test]
    fn rules_follow_the_registry() {
        let registry = &vanillite_data::registry::GAME_RULE;
        assert_eq!(registry.len(), RULES.len());
        for (id, info) in RULES.iter().enumerate() {
            let name = registry.name(u16::try_from(id).unwrap_or(u16::MAX));
            assert_eq!(name, Some(format!("minecraft:{}", info.name).as_str()));
        }
    }

    #[test]
    fn nbt_round_trip() {
        let mut rules = GameRules::default();
        rules.set(Rule::KEEP_INVENTORY, 1);
        rules.set(Rule::RESPAWN_RADIUS, 3);
        assert_eq!(GameRules::from_nbt(&rules.to_nbt()), rules);
    }
}

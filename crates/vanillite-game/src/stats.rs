//! Statistics (`Stats`, `StatType`, `Stat`) and a player's counter (`ServerStatsCounter`): the
//! values, the ones the statistics screen has not seen yet, and the score changes of the
//! stat criteria the server applies to the scoreboard.

// SEC-07: the statistics file is input.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::as_conversions
)]

use std::collections::BTreeMap;

use vanillite_base::hash::{FxHashMap, FxHashSet};
use vanillite_data::registry::{self, Registry};
use vanillite_protocol::packets::play::clientbound::{AwardStats, StatValue};

/// The stat types in `minecraft:stat_type` order (`Stats`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum StatType {
    Mined,
    Crafted,
    Used,
    Broken,
    PickedUp,
    Dropped,
    Killed,
    KilledBy,
    Custom,
}

impl StatType {
    /// All types in registry order.
    pub const ALL: [StatType; 9] = [
        StatType::Mined,
        StatType::Crafted,
        StatType::Used,
        StatType::Broken,
        StatType::PickedUp,
        StatType::Dropped,
        StatType::Killed,
        StatType::KilledBy,
        StatType::Custom,
    ];

    /// The id in `minecraft:stat_type`.
    pub fn id(self) -> u16 {
        Self::ALL
            .iter()
            .position(|t| *t == self)
            .and_then(|i| u16::try_from(i).ok())
            .unwrap_or(0)
    }

    /// The registry name, e.g. `minecraft:mined`.
    pub fn name(self) -> &'static str {
        registry::STAT_TYPE
            .name(self.id())
            .unwrap_or("minecraft:custom")
    }

    /// The registry of the values (`StatType.getRegistry`).
    pub fn registry(self) -> &'static Registry {
        match self {
            StatType::Mined => &registry::BLOCK,
            StatType::Killed | StatType::KilledBy => &registry::ENTITY_TYPE,
            StatType::Custom => &registry::CUSTOM_STAT,
            _ => &registry::ITEM,
        }
    }

    /// By registry name.
    pub fn by_name(name: &str) -> Option<StatType> {
        let id = registry::STAT_TYPE.id(name)?;
        Self::ALL.get(usize::from(id)).copied()
    }
}

/// A statistic: a type and a value of its registry (`Stat`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Stat {
    pub kind: StatType,
    /// The id in [`StatType::registry`].
    pub value: u16,
}

/// `Identifier.bySeparator(s, '.')`: the first `.` separates the namespace.
fn by_separator(s: &str) -> String {
    match s.split_once('.') {
        Some((ns, path)) => format!("{ns}:{path}"),
        None => format!("minecraft:{s}"),
    }
}

impl Stat {
    /// A statistic of `kind` for the registry id `value` (`StatType.get`).
    pub const fn of(kind: StatType, value: u16) -> Stat {
        Stat { kind, value }
    }

    /// A `minecraft:custom` statistic by its id in `minecraft:custom_stat`.
    pub const fn custom(value: u16) -> Stat {
        Stat::of(StatType::Custom, value)
    }

    /// A `minecraft:custom` statistic by its name, e.g. `minecraft:eat_cake_slice`.
    pub fn custom_named(name: &str) -> Stat {
        Stat::custom(StatType::Custom.registry().id(name).unwrap_or(0))
    }

    /// `Stat.buildName`: `minecraft.custom:minecraft.jump`.
    pub fn name(self) -> String {
        let value = self.kind.registry().name(self.value).unwrap_or("");
        format!(
            "{}:{}",
            self.kind.name().replace(':', "."),
            value.replace(':', ".")
        )
    }

    /// The statistic of a stat criterion name (`ObjectiveCriteria.byName` for `type:value`).
    pub fn by_criteria_name(name: &str) -> Option<Stat> {
        let (kind, value) = name.split_once(':')?;
        let kind = StatType::by_name(&by_separator(kind))?;
        let value = kind.registry().id(&by_separator(value))?;
        Some(Stat { kind, value })
    }
}

/// The `minecraft:custom` statistics by their `minecraft:custom_stat` id.
pub mod custom {
    use super::Stat;

    pub const LEAVE_GAME: Stat = Stat::custom(0);
    pub const PLAY_TIME: Stat = Stat::custom(1);
    pub const TOTAL_WORLD_TIME: Stat = Stat::custom(2);
    pub const TIME_SINCE_DEATH: Stat = Stat::custom(3);
    pub const TIME_SINCE_REST: Stat = Stat::custom(4);
    /// `minecraft:sneak_time` (`Stats.CROUCH_TIME`).
    pub const CROUCH_TIME: Stat = Stat::custom(5);
    pub const WALK_ONE_CM: Stat = Stat::custom(6);
    pub const CROUCH_ONE_CM: Stat = Stat::custom(7);
    pub const SPRINT_ONE_CM: Stat = Stat::custom(8);
    pub const WALK_ON_WATER_ONE_CM: Stat = Stat::custom(9);
    pub const FALL_ONE_CM: Stat = Stat::custom(10);
    pub const CLIMB_ONE_CM: Stat = Stat::custom(11);
    pub const FLY_ONE_CM: Stat = Stat::custom(12);
    pub const WALK_UNDER_WATER_ONE_CM: Stat = Stat::custom(13);
    pub const MINECART_ONE_CM: Stat = Stat::custom(14);
    pub const BOAT_ONE_CM: Stat = Stat::custom(15);
    pub const PIG_ONE_CM: Stat = Stat::custom(16);
    pub const HAPPY_GHAST_ONE_CM: Stat = Stat::custom(17);
    pub const HORSE_ONE_CM: Stat = Stat::custom(18);
    pub const AVIATE_ONE_CM: Stat = Stat::custom(19);
    pub const SWIM_ONE_CM: Stat = Stat::custom(20);
    pub const STRIDER_ONE_CM: Stat = Stat::custom(21);
    pub const NAUTILUS_ONE_CM: Stat = Stat::custom(22);
    pub const JUMP: Stat = Stat::custom(23);
    pub const DROP: Stat = Stat::custom(24);
    pub const DAMAGE_DEALT: Stat = Stat::custom(25);
    pub const DAMAGE_DEALT_ABSORBED: Stat = Stat::custom(26);
    pub const DAMAGE_DEALT_RESISTED: Stat = Stat::custom(27);
    pub const DAMAGE_TAKEN: Stat = Stat::custom(28);
    pub const DAMAGE_BLOCKED_BY_SHIELD: Stat = Stat::custom(29);
    pub const DAMAGE_ABSORBED: Stat = Stat::custom(30);
    pub const DAMAGE_RESISTED: Stat = Stat::custom(31);
    pub const DEATHS: Stat = Stat::custom(32);
    pub const MOB_KILLS: Stat = Stat::custom(33);
    pub const ANIMALS_BRED: Stat = Stat::custom(34);
    pub const PLAYER_KILLS: Stat = Stat::custom(35);
    pub const FISH_CAUGHT: Stat = Stat::custom(36);
    pub const TALKED_TO_VILLAGER: Stat = Stat::custom(37);
    pub const TRADED_WITH_VILLAGER: Stat = Stat::custom(38);
    pub const EAT_CAKE_SLICE: Stat = Stat::custom(39);
    pub const FILL_CAULDRON: Stat = Stat::custom(40);
    pub const USE_CAULDRON: Stat = Stat::custom(41);
    pub const CLEAN_ARMOR: Stat = Stat::custom(42);
    pub const CLEAN_BANNER: Stat = Stat::custom(43);
    pub const CLEAN_SHULKER_BOX: Stat = Stat::custom(44);
    pub const INTERACT_WITH_BREWINGSTAND: Stat = Stat::custom(45);
    pub const INTERACT_WITH_BEACON: Stat = Stat::custom(46);
    pub const INSPECT_DROPPER: Stat = Stat::custom(47);
    pub const INSPECT_HOPPER: Stat = Stat::custom(48);
    pub const INSPECT_DISPENSER: Stat = Stat::custom(49);
    pub const PLAY_NOTEBLOCK: Stat = Stat::custom(50);
    pub const TUNE_NOTEBLOCK: Stat = Stat::custom(51);
    pub const POT_FLOWER: Stat = Stat::custom(52);
    pub const TRIGGER_TRAPPED_CHEST: Stat = Stat::custom(53);
    pub const OPEN_ENDERCHEST: Stat = Stat::custom(54);
    pub const ENCHANT_ITEM: Stat = Stat::custom(55);
    pub const PLAY_RECORD: Stat = Stat::custom(56);
    pub const INTERACT_WITH_FURNACE: Stat = Stat::custom(57);
    pub const INTERACT_WITH_CRAFTING_TABLE: Stat = Stat::custom(58);
    pub const OPEN_CHEST: Stat = Stat::custom(59);
    pub const SLEEP_IN_BED: Stat = Stat::custom(60);
    pub const SLEEP_IN_STRAW_BED: Stat = Stat::custom(61);
    pub const OPEN_SHULKER_BOX: Stat = Stat::custom(62);
    pub const OPEN_BARREL: Stat = Stat::custom(63);
    pub const INTERACT_WITH_BLAST_FURNACE: Stat = Stat::custom(64);
    pub const INTERACT_WITH_SMOKER: Stat = Stat::custom(65);
    pub const INTERACT_WITH_LECTERN: Stat = Stat::custom(66);
    pub const INTERACT_WITH_CAMPFIRE: Stat = Stat::custom(67);
    pub const INTERACT_WITH_CARTOGRAPHY_TABLE: Stat = Stat::custom(68);
    pub const INTERACT_WITH_LOOM: Stat = Stat::custom(69);
    pub const INTERACT_WITH_STONECUTTER: Stat = Stat::custom(70);
    pub const BELL_RING: Stat = Stat::custom(71);
    pub const RAID_TRIGGER: Stat = Stat::custom(72);
    pub const RAID_WIN: Stat = Stat::custom(73);
    pub const INTERACT_WITH_ANVIL: Stat = Stat::custom(74);
    pub const INTERACT_WITH_GRINDSTONE: Stat = Stat::custom(75);
    pub const TARGET_HIT: Stat = Stat::custom(76);
    pub const INTERACT_WITH_SMITHING_TABLE: Stat = Stat::custom(77);
}

/// How a statistic change reaches the objectives of its criterion
/// (`Scoreboard.forAllObjectives(stat, player, ...)`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScoreChange {
    /// `score.add(count)`.
    Add(i32),
    /// `ScoreAccess::reset`.
    Reset,
}

/// A player's statistics (`ServerStatsCounter`).
#[derive(Debug, Default)]
pub struct StatsCounter {
    values: FxHashMap<Stat, i32>,
    dirty: FxHashSet<Stat>,
    scores: Vec<(Stat, ScoreChange)>,
}

impl StatsCounter {
    /// `getValue`.
    pub fn get(&self, stat: Stat) -> i32 {
        self.values.get(&stat).copied().unwrap_or(0)
    }

    /// `setValue`: marks the statistic for the statistics screen.
    pub fn set(&mut self, stat: Stat, value: i32) {
        self.values.insert(stat, value);
        self.dirty.insert(stat);
    }

    /// Every statistic with its value (also zeros), in no particular order.
    pub fn iter(&self) -> impl Iterator<Item = (Stat, i32)> + '_ {
        self.values.iter().map(|(s, v)| (*s, *v))
    }

    /// `ServerPlayer.awardStat`: adds `count` (saturating) and to the scores of its criterion.
    pub fn award(&mut self, stat: Stat, count: i32) {
        let value = i64::from(self.get(stat))
            .saturating_add(i64::from(count))
            .min(i64::from(i32::MAX));
        self.set(stat, i32::try_from(value).unwrap_or(i32::MIN));
        self.scores.push((stat, ScoreChange::Add(count)));
    }

    /// `ServerPlayer.resetStat`.
    pub fn reset(&mut self, stat: Stat) {
        self.set(stat, 0);
        self.scores.push((stat, ScoreChange::Reset));
    }

    /// The score changes since the last call, in order.
    pub fn take_score_changes(&mut self) -> Vec<(Stat, ScoreChange)> {
        std::mem::take(&mut self.scores)
    }

    /// Bukkit's `PlayerStatisticIncrementEvent` for the increases in `changes` (from
    /// [`StatsCounter::take_score_changes`], in award order), fired after them: a cancelled
    /// increase is taken back and its score change dropped. Nothing happens unless a plugin
    /// listens.
    pub fn fire_increases(
        &mut self,
        changes: &mut Vec<(Stat, ScoreChange)>,
        hooks: &dyn crate::hooks::GameHooks,
        player: u128,
    ) {
        use vanillite_abi::event;
        if changes.is_empty() || !hooks.wants(event::PLAYER_STATISTIC) {
            return;
        }
        // The value before and after each increase, walking back from the current values.
        let mut current: FxHashMap<Stat, i64> = FxHashMap::default();
        let mut steps: Vec<(i64, i64)> = changes
            .iter()
            .rev()
            .map(|&(stat, change)| {
                let value = current
                    .entry(stat)
                    .or_insert_with(|| i64::from(self.get(stat)));
                let new = *value;
                match change {
                    ScoreChange::Add(count) => *value = new.saturating_sub(i64::from(count)),
                    // Before a reset the value is unknown; the earlier increases see 0.
                    ScoreChange::Reset => *value = 0,
                }
                (*value, new)
            })
            .collect();
        steps.reverse();
        let mut taken_back: FxHashMap<Stat, i64> = FxHashMap::default();
        let mut keep = Vec::with_capacity(changes.len());
        for (&(stat, change), (old, new)) in changes.iter().zip(steps) {
            let ScoreChange::Add(count) = change else {
                taken_back.remove(&stat);
                keep.push(true);
                continue;
            };
            let back = taken_back.get(&stat).copied().unwrap_or(0);
            let mut ev = crate::hooks::Event {
                entity: player,
                entity_type: "minecraft:player".to_owned(),
                key: stat.name(),
                value: new.saturating_sub(back),
                extra: old.saturating_sub(back),
                ..crate::hooks::Event::new(event::PLAYER_STATISTIC)
            };
            hooks.event(&mut ev);
            if ev.cancelled {
                *taken_back.entry(stat).or_insert(0) = back.saturating_add(i64::from(count));
            }
            keep.push(!ev.cancelled);
        }
        for (stat, back) in taken_back {
            if back != 0 {
                let value = i64::from(self.get(stat)).saturating_sub(back);
                let value = value.clamp(i64::from(i32::MIN), i64::from(i32::MAX));
                self.set(stat, i32::try_from(value).unwrap_or(0));
            }
        }
        let mut keep = keep.into_iter();
        changes.retain(|_| keep.next().unwrap_or(true));
    }

    /// `markAllDirty` (on join).
    pub fn mark_all_dirty(&mut self) {
        self.dirty.extend(self.values.keys().copied());
    }

    /// `sendStats`: the statistics changed since the last request.
    pub fn award_packet(&mut self) -> AwardStats {
        let mut changed: Vec<Stat> = self.dirty.drain().collect();
        changed.sort_unstable();
        AwardStats {
            stats: changed
                .into_iter()
                .map(|stat| StatValue {
                    stat_type: i32::from(stat.kind.id()),
                    value_id: i32::from(stat.value),
                    count: self.get(stat),
                })
                .collect(),
        }
    }

    /// The statistics file (`toJson`): `stats` grouped by type, then `DataVersion`.
    pub fn to_json(&self) -> String {
        let mut grouped: BTreeMap<StatType, Vec<(&'static str, i32)>> = BTreeMap::new();
        for (stat, value) in &self.values {
            if let Some(name) = stat.kind.registry().name(stat.value) {
                grouped.entry(stat.kind).or_default().push((name, *value));
            }
        }
        let mut out = String::from("{\n  \"stats\": {");
        for (i, (kind, mut values)) in grouped.into_iter().enumerate() {
            values.sort_unstable();
            out.push_str(if i == 0 { "\n" } else { ",\n" });
            out.push_str(&format!("    \"{}\": {{", kind.name()));
            for (j, (name, value)) in values.into_iter().enumerate() {
                out.push_str(if j == 0 { "\n" } else { ",\n" });
                out.push_str(&format!("      \"{name}\": {value}"));
            }
            out.push_str("\n    }");
        }
        out.push_str(&format!(
            "\n  }},\n  \"DataVersion\": {}\n}}",
            vanillite_data::version::DATA_VERSION
        ));
        out
    }

    /// Reads a statistics file (`parse`); entries of unknown types or values are left out.
    pub fn from_json(text: &str) -> Result<StatsCounter, String> {
        let root: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let mut counter = StatsCounter::default();
        let Some(types) = root.get("stats").and_then(serde_json::Value::as_object) else {
            return Ok(counter);
        };
        for (kind, values) in types {
            let Some(kind) = StatType::by_name(kind) else {
                continue;
            };
            let Some(values) = values.as_object() else {
                continue;
            };
            for (name, value) in values {
                let (Some(id), Some(value)) = (
                    kind.registry().id(name),
                    value.as_i64().and_then(|v| i32::try_from(v).ok()),
                ) else {
                    continue;
                };
                counter.values.insert(Stat { kind, value: id }, value);
            }
        }
        Ok(counter)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn custom_ids_match_the_registry() {
        let named = [
            (custom::LEAVE_GAME, "leave_game"),
            (custom::PLAY_TIME, "play_time"),
            (custom::TOTAL_WORLD_TIME, "total_world_time"),
            (custom::TIME_SINCE_DEATH, "time_since_death"),
            (custom::TIME_SINCE_REST, "time_since_rest"),
            (custom::CROUCH_TIME, "sneak_time"),
            (custom::WALK_ONE_CM, "walk_one_cm"),
            (custom::CROUCH_ONE_CM, "crouch_one_cm"),
            (custom::SPRINT_ONE_CM, "sprint_one_cm"),
            (custom::WALK_ON_WATER_ONE_CM, "walk_on_water_one_cm"),
            (custom::FALL_ONE_CM, "fall_one_cm"),
            (custom::CLIMB_ONE_CM, "climb_one_cm"),
            (custom::FLY_ONE_CM, "fly_one_cm"),
            (custom::WALK_UNDER_WATER_ONE_CM, "walk_under_water_one_cm"),
            (custom::AVIATE_ONE_CM, "aviate_one_cm"),
            (custom::SWIM_ONE_CM, "swim_one_cm"),
            (custom::JUMP, "jump"),
            (custom::DROP, "drop"),
            (custom::DAMAGE_TAKEN, "damage_taken"),
            (custom::DAMAGE_ABSORBED, "damage_absorbed"),
            (custom::DEATHS, "deaths"),
            (
                custom::INTERACT_WITH_BREWINGSTAND,
                "interact_with_brewingstand",
            ),
            (custom::INTERACT_WITH_BEACON, "interact_with_beacon"),
            (custom::INSPECT_DROPPER, "inspect_dropper"),
            (custom::INSPECT_HOPPER, "inspect_hopper"),
            (custom::INSPECT_DISPENSER, "inspect_dispenser"),
            (custom::TRIGGER_TRAPPED_CHEST, "trigger_trapped_chest"),
            (custom::OPEN_ENDERCHEST, "open_enderchest"),
            (custom::INTERACT_WITH_FURNACE, "interact_with_furnace"),
            (
                custom::INTERACT_WITH_CRAFTING_TABLE,
                "interact_with_crafting_table",
            ),
            (custom::OPEN_CHEST, "open_chest"),
            (custom::OPEN_SHULKER_BOX, "open_shulker_box"),
            (custom::OPEN_BARREL, "open_barrel"),
            (
                custom::INTERACT_WITH_BLAST_FURNACE,
                "interact_with_blast_furnace",
            ),
            (custom::INTERACT_WITH_SMOKER, "interact_with_smoker"),
            (
                custom::INTERACT_WITH_CARTOGRAPHY_TABLE,
                "interact_with_cartography_table",
            ),
            (custom::INTERACT_WITH_LOOM, "interact_with_loom"),
            (
                custom::INTERACT_WITH_STONECUTTER,
                "interact_with_stonecutter",
            ),
            (custom::INTERACT_WITH_ANVIL, "interact_with_anvil"),
            (custom::INTERACT_WITH_GRINDSTONE, "interact_with_grindstone"),
            (
                custom::INTERACT_WITH_SMITHING_TABLE,
                "interact_with_smithing_table",
            ),
        ];
        for (stat, name) in named {
            assert_eq!(
                registry::CUSTOM_STAT.name(stat.value),
                Some(format!("minecraft:{name}").as_str())
            );
        }
    }

    #[test]
    fn criteria_names() {
        assert_eq!(custom::JUMP.name(), "minecraft.custom:minecraft.jump");
        assert_eq!(Stat::by_criteria_name("custom:jump"), Some(custom::JUMP));
        assert_eq!(
            Stat::by_criteria_name("minecraft.custom:minecraft.jump"),
            Some(custom::JUMP)
        );
        let stone = registry::BLOCK.id("minecraft:stone").unwrap();
        assert_eq!(
            Stat::by_criteria_name("minecraft.mined:minecraft.stone"),
            Some(Stat {
                kind: StatType::Mined,
                value: stone
            })
        );
        assert_eq!(Stat::by_criteria_name("minecraft.mined:nope"), None);
        assert_eq!(Stat::by_criteria_name("foo:bar"), None);
    }

    #[test]
    fn award_saturates_and_round_trips() {
        let mut stats = StatsCounter::default();
        stats.set(custom::JUMP, i32::MAX - 1);
        stats.award(custom::JUMP, 5);
        assert_eq!(stats.get(custom::JUMP), i32::MAX);
        stats.award(custom::PLAY_TIME, 20);
        stats.reset(custom::TIME_SINCE_DEATH);
        let changes = stats.take_score_changes();
        assert_eq!(changes.len(), 3);
        assert_eq!(changes[2], (custom::TIME_SINCE_DEATH, ScoreChange::Reset));
        let json = stats.to_json();
        assert!(json.contains("\"minecraft:custom\": {"));
        let back = StatsCounter::from_json(&json).unwrap();
        assert_eq!(back.get(custom::JUMP), i32::MAX);
        assert_eq!(back.get(custom::PLAY_TIME), 20);
        let packet = stats.award_packet();
        assert_eq!(packet.stats.len(), 3);
        assert!(stats.award_packet().stats.is_empty());
    }
}

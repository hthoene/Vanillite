//! Statistics, criterion triggers and recipe unlocks that block and menu code awards to a
//! player whose data it cannot reach (the player handlers hold it, or the code only knows the
//! player's id): queued on the level or the inventory and applied by the server after the
//! players' tick.

use super::trigger::Event;
use crate::player::PlayerData;
use crate::stats::Stat;

/// `player.awardStat(stat, count)`, `CriteriaTriggers.X.trigger(player, ...)` or
/// `player.awardRecipes(recipes)`.
#[derive(Clone, Debug)]
pub enum Award {
    Stat(Stat, i32),
    Trigger(Event),
    Recipes(Vec<String>),
}

impl Award {
    /// Applies the award to the player.
    pub fn apply(self, player: &mut PlayerData) {
        match self {
            Award::Stat(stat, count) => player.stats.award(stat, count),
            Award::Trigger(event) => player.advancements.trigger(event),
            Award::Recipes(ids) => {
                let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
                player.unlock_recipes(&ids);
            }
        }
    }
}

//! Hunger: `FoodData` (food level, saturation, exhaustion, the regeneration timer) and the
//! `minecraft:food` item component (`FoodProperties`).

use vanillite_nbt::{Compound, Tag};

use super::rules::Difficulty;

/// `FoodConstants.MAX_FOOD`.
pub const MAX_FOOD: i32 = 20;
/// Exhaustion that costs one saturation or food point.
const EXHAUSTION_DROP: f32 = 4.0;
/// `FoodConstants.MAX_SATURATION`... the exhaustion cap.
const MAX_EXHAUSTION: f32 = 40.0;

/// Exhaustion costs of actions (`FoodConstants`).
pub mod exhaustion {
    pub const SWIM: f32 = 0.01;
    pub const SPRINT: f32 = 0.1;
    pub const JUMP: f32 = 0.05;
    pub const SPRINT_JUMP: f32 = 0.2;
    pub const HEAL: f32 = 6.0;
}

/// The `minecraft:food` component of an item.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FoodProperties {
    pub nutrition: i32,
    pub saturation: f32,
    pub can_always_eat: bool,
}

/// What [`FoodData::tick`] asks the player to do.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FoodAction {
    None,
    /// `player.heal(amount)`.
    Heal(f32),
    /// `player.hurtServer(starve, 1.0)`.
    Starve,
}

/// `FoodData`.
#[derive(Clone, Debug, PartialEq)]
pub struct FoodData {
    pub food_level: i32,
    pub saturation: f32,
    pub exhaustion: f32,
    pub tick_timer: i32,
}

impl Default for FoodData {
    fn default() -> Self {
        FoodData {
            food_level: MAX_FOOD,
            saturation: 5.0,
            exhaustion: 0.0,
            tick_timer: 0,
        }
    }
}

impl FoodData {
    fn add(&mut self, food: i32, saturation: f32) {
        self.food_level = food.saturating_add(self.food_level).clamp(0, MAX_FOOD);
        // `Mth.clamp(saturation + this.saturationLevel, 0.0F, (float)this.foodLevel)`.
        #[allow(clippy::cast_precision_loss, clippy::as_conversions)]
        let max = self.food_level as f32;
        self.saturation = (saturation + self.saturation).clamp(0.0, max);
    }

    /// `eat(FoodProperties)`.
    pub fn eat(&mut self, food: &FoodProperties) {
        self.add(food.nutrition, food.saturation);
    }

    /// `addExhaustion`.
    pub fn add_exhaustion(&mut self, amount: f32) {
        self.exhaustion = (self.exhaustion + amount).min(MAX_EXHAUSTION);
    }

    /// `needsFood`.
    pub fn needs_food(&self) -> bool {
        self.food_level < MAX_FOOD
    }

    /// `hasEnoughFood`: sprinting is allowed above 6.
    pub fn has_enough_food(&self) -> bool {
        self.food_level > 6
    }

    /// `tick`: spends exhaustion, then regenerates or starves. `is_hurt` and `health` are the
    /// player's state before; the returned action happens in the same tick.
    pub fn tick(
        &mut self,
        difficulty: Difficulty,
        natural_regen: bool,
        is_hurt: bool,
        health: f32,
    ) -> FoodAction {
        if self.exhaustion > EXHAUSTION_DROP {
            self.exhaustion -= EXHAUSTION_DROP;
            if self.saturation > 0.0 {
                self.saturation = (self.saturation - 1.0).max(0.0);
            } else if difficulty != Difficulty::Peaceful {
                self.food_level = (self.food_level - 1).max(0);
            }
        }
        if natural_regen && self.saturation > 0.0 && is_hurt && self.food_level >= MAX_FOOD {
            self.tick_timer += 1;
            if self.tick_timer >= 10 {
                let spent = self.saturation.min(6.0);
                self.add_exhaustion(spent);
                self.tick_timer = 0;
                return FoodAction::Heal(spent / 6.0);
            }
        } else if natural_regen && self.food_level >= 18 && is_hurt {
            self.tick_timer += 1;
            if self.tick_timer >= 80 {
                self.add_exhaustion(exhaustion::HEAL);
                self.tick_timer = 0;
                return FoodAction::Heal(1.0);
            }
        } else if self.food_level <= 0 {
            self.tick_timer += 1;
            if self.tick_timer >= 80 {
                self.tick_timer = 0;
                let starve = health > 10.0
                    || difficulty == Difficulty::Hard
                    || health > 1.0 && difficulty == Difficulty::Normal;
                if starve {
                    return FoodAction::Starve;
                }
            }
        } else {
            self.tick_timer = 0;
        }
        FoodAction::None
    }

    /// `readAdditionalSaveData`.
    pub fn load(data: &Compound) -> FoodData {
        let int = |k: &str, d: i32| {
            data.get(k)
                .and_then(Tag::as_i64)
                .and_then(|v| i32::try_from(v).ok())
                .unwrap_or(d)
        };
        let float = |k: &str, d: f32| match data.get(k) {
            Some(Tag::Float(v)) => *v,
            _ => d,
        };
        FoodData {
            food_level: int("foodLevel", MAX_FOOD),
            tick_timer: int("foodTickTimer", 0),
            saturation: float("foodSaturationLevel", 5.0),
            exhaustion: float("foodExhaustionLevel", 0.0),
        }
    }

    /// `addAdditionalSaveData`.
    pub fn save(&self, out: &mut Compound) {
        out.insert("foodLevel", Tag::Int(self.food_level));
        out.insert("foodTickTimer", Tag::Int(self.tick_timer));
        out.insert("foodSaturationLevel", Tag::Float(self.saturation));
        out.insert("foodExhaustionLevel", Tag::Float(self.exhaustion));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Saturation regeneration: every 10 ticks heal `min(saturation, 6) / 6` and pay the
    /// saturation as exhaustion (Vanilla 26.3 `FoodData.tick`).
    #[test]
    fn saturation_regeneration_timing() {
        let mut food = FoodData::default();
        for tick in 1..=9 {
            assert_eq!(
                food.tick(Difficulty::Easy, true, true, 11.0),
                FoodAction::None,
                "{tick}"
            );
        }
        assert_eq!(
            food.tick(Difficulty::Easy, true, true, 11.0),
            FoodAction::Heal(5.0 / 6.0)
        );
        assert_eq!(food.exhaustion, 5.0);
        // The next tick spends 4 exhaustion: one saturation point.
        assert_eq!(
            food.tick(Difficulty::Easy, true, true, 11.0),
            FoodAction::None
        );
        assert_eq!(food.saturation, 4.0);
        assert_eq!(food.exhaustion, 1.0);
    }

    /// Food-level regeneration: one health every 80 ticks at 18+ food without saturation.
    #[test]
    fn food_regeneration_timing() {
        let mut food = FoodData {
            food_level: 18,
            saturation: 0.0,
            ..FoodData::default()
        };
        for _ in 1..80 {
            assert_eq!(
                food.tick(Difficulty::Normal, true, true, 10.0),
                FoodAction::None
            );
        }
        assert_eq!(
            food.tick(Difficulty::Normal, true, true, 10.0),
            FoodAction::Heal(1.0)
        );
        assert_eq!(food.exhaustion, 6.0);
    }

    /// Starvation stops at 10 health on easy, 1 on normal, never on hard.
    #[test]
    fn starvation_limits() {
        for (difficulty, health, starves) in [
            (Difficulty::Easy, 10.0, false),
            (Difficulty::Easy, 10.5, true),
            (Difficulty::Normal, 1.0, false),
            (Difficulty::Normal, 2.0, true),
            (Difficulty::Hard, 0.5, true),
        ] {
            let mut food = FoodData {
                food_level: 0,
                saturation: 0.0,
                ..FoodData::default()
            };
            for _ in 1..80 {
                food.tick(difficulty, true, true, health);
            }
            let expected = if starves {
                FoodAction::Starve
            } else {
                FoodAction::None
            };
            assert_eq!(food.tick(difficulty, true, true, health), expected);
        }
    }

    #[test]
    fn eating_clamps_saturation_to_food() {
        let mut food = FoodData {
            food_level: 2,
            saturation: 0.0,
            ..FoodData::default()
        };
        // Bread: 5 nutrition, 6 saturation.
        food.eat(&FoodProperties {
            nutrition: 5,
            saturation: 6.0,
            can_always_eat: false,
        });
        assert_eq!(food.food_level, 7);
        assert_eq!(food.saturation, 6.0);
        food.eat(&FoodProperties {
            nutrition: 20,
            saturation: 30.0,
            can_always_eat: false,
        });
        assert_eq!((food.food_level, food.saturation), (20, 20.0));
    }
}

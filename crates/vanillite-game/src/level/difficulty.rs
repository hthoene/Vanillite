//! Local (regional) difficulty (`DifficultyInstance`, `ServerLevel.getCurrentDifficultyAt`):
//! the base difficulty scaled by the overworld clock, the chunk's inhabited time and the moon.

use vanillite_base::BlockPos;

use crate::level::Level;
use crate::survival::rules::Difficulty;

/// `DifficultyInstance`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DifficultyInstance {
    pub base: Difficulty,
    /// `getEffectiveDifficulty`.
    pub effective: f32,
}

impl DifficultyInstance {
    /// `new DifficultyInstance(base, totalGameTime, localGameTime, moonBrightness)`.
    pub fn new(
        base: Difficulty,
        total_game_time: i64,
        local_game_time: i64,
        moon_brightness: f32,
    ) -> DifficultyInstance {
        DifficultyInstance {
            base,
            effective: calculate(base, total_game_time, local_game_time, moon_brightness),
        }
    }

    /// `isHard`.
    pub fn is_hard(&self) -> bool {
        self.effective >= 3.0
    }

    /// `isHarderThan(required)`.
    pub fn is_harder_than(&self, required: f32) -> bool {
        self.effective > required
    }

    /// `getSpecialMultiplier`.
    pub fn special_multiplier(&self) -> f32 {
        if self.effective < 2.0 {
            0.0
        } else if self.effective > 4.0 {
            1.0
        } else {
            (self.effective - 2.0) / 2.0
        }
    }
}

/// `DifficultyInstance.calculateDifficulty`.
#[allow(clippy::cast_precision_loss, clippy::as_conversions)]
fn calculate(
    base: Difficulty,
    total_game_time: i64,
    local_game_time: i64,
    moon_brightness: f32,
) -> f32 {
    if base == Difficulty::Peaceful {
        return 0.0;
    }
    let hard = base == Difficulty::Hard;
    let mut scale = 0.75_f32;
    let global = ((total_game_time as f32 + -72_000.0) / 1_440_000.0).clamp(0.0, 1.0) * 0.25;
    scale += global;
    let mut local = 0.0_f32;
    local += (local_game_time as f32 / 3_600_000.0).clamp(0.0, 1.0) * if hard { 1.0 } else { 0.75 };
    local += (moon_brightness * 0.25).clamp(0.0, global);
    if base == Difficulty::Easy {
        local *= 0.5;
    }
    scale += local;
    base.id() as f32 * scale
}

/// `ServerLevel.getCurrentDifficultyAt(pos)`: without a full chunk the inhabited time and the
/// moon count as 0.
pub fn at(level: &Level, pos: BlockPos) -> DifficultyInstance {
    let settings = &level.settings;
    let (local, moon) = level
        .chunks
        .inhabited_time(pos.chunk())
        .map_or((0, 0.0), |t| (t, settings.moon_brightness));
    DifficultyInstance::new(
        settings.difficulty,
        settings.overworld_clock_time,
        local,
        moon,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_world_and_full_regional_difficulty() {
        let fresh = DifficultyInstance::new(Difficulty::Normal, 0, 0, 1.0);
        assert_eq!(fresh.effective, 1.5);
        assert_eq!(fresh.special_multiplier(), 0.0);
        let hard = DifficultyInstance::new(Difficulty::Hard, 2_000_000, 4_000_000, 1.0);
        assert_eq!(hard.effective, 6.75);
        assert_eq!(hard.special_multiplier(), 1.0);
        assert!(hard.is_hard());
        let peaceful = DifficultyInstance::new(Difficulty::Peaceful, 2_000_000, 4_000_000, 1.0);
        assert_eq!(peaceful.effective, 0.0);
    }
}

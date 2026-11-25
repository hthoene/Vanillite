//! Raids (`Raid`, `Raids`): bad omen and raid omen, waves by difficulty, the raid bar,
//! victory, loss and the heroes of the village.

use vanillite_base::BlockPos;

/// `Raid.RaidStatus`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RaidStatus {
    #[default]
    Ongoing,
    Victory,
    Loss,
    Stopped,
}

/// `Raid`.
#[derive(Clone, Debug, Default)]
pub struct Raid {
    pub center: BlockPos,
    pub active: bool,
    pub status: RaidStatus,
    /// `groupsSpawned`.
    pub groups_spawned: i32,
    /// `raidCooldownTicks`.
    pub raid_cooldown_ticks: i32,
    /// Raiders alive (`getTotalRaidersAlive`).
    pub raiders_alive: i32,
}

impl Raid {
    /// `isOver`.
    pub fn is_over(&self) -> bool {
        self.is_victory() || self.is_loss()
    }

    /// `isBetweenWaves`.
    pub fn is_between_waves(&self) -> bool {
        self.has_first_wave_spawned() && self.raiders_alive == 0 && self.raid_cooldown_ticks > 0
    }

    /// `hasFirstWaveSpawned`.
    pub fn has_first_wave_spawned(&self) -> bool {
        self.groups_spawned > 0
    }

    /// `isStopped`.
    pub fn is_stopped(&self) -> bool {
        self.status == RaidStatus::Stopped
    }

    /// `isVictory`.
    pub fn is_victory(&self) -> bool {
        self.status == RaidStatus::Victory
    }

    /// `isLoss`.
    pub fn is_loss(&self) -> bool {
        self.status == RaidStatus::Loss
    }

    /// `isActive`.
    pub fn is_active(&self) -> bool {
        self.active
    }
}

/// `Raids`: the raids of a dimension by id.
#[derive(Clone, Debug, Default)]
pub struct Raids {
    /// `(id, raid)` in `raidMap` order.
    pub raids: Vec<(i32, Raid)>,
    /// `nextId`.
    pub next_id: i32,
    /// `tick`.
    pub tick: i32,
}

impl Raids {
    /// `getNearbyRaid(pos, maxDistSqr)`: the closest active raid.
    pub fn nearby_raid(&self, pos: BlockPos, max_dist_sqr: i32) -> Option<(i32, &Raid)> {
        let mut best: Option<(i32, &Raid)> = None;
        let mut closest = f64::from(max_dist_sqr);
        for (id, raid) in &self.raids {
            let d = raid.center.dist_sqr(pos);
            if raid.is_active() && d < closest {
                best = Some((*id, raid));
                closest = d;
            }
        }
        best
    }

    /// `ServerLevel.getRaidAt(pos)`: the closest active raid within 96 blocks.
    pub fn raid_at(&self, pos: BlockPos) -> Option<&Raid> {
        self.nearby_raid(pos, 9216).map(|(_, r)| r)
    }

    /// `ServerLevel.isRaided(pos)`.
    pub fn is_raided(&self, pos: BlockPos) -> bool {
        self.raid_at(pos).is_some()
    }
}

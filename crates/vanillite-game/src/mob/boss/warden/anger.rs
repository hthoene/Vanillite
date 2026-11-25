//! `AngerManagement` and `AngerLevel` of the warden.

use vanillite_nbt::{Compound, List, Tag};
use vanillite_world::entity::EntityId;

/// `AngerManagement.MAX_ANGER`.
pub const MAX_ANGER: i32 = 150;

/// `AngerLevel`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AngerLevel {
    Calm,
    Agitated,
    Angry,
}

impl AngerLevel {
    /// `byAnger(anger)`.
    pub fn by_anger(anger: i32) -> AngerLevel {
        if anger >= 80 {
            AngerLevel::Angry
        } else if anger >= 40 {
            AngerLevel::Agitated
        } else {
            AngerLevel::Calm
        }
    }

    /// `getMinimumAnger`.
    pub fn minimum_anger(self) -> i32 {
        match self {
            AngerLevel::Calm => 0,
            AngerLevel::Agitated => 40,
            AngerLevel::Angry => 80,
        }
    }

    /// `isAngry`.
    pub fn is_angry(self) -> bool {
        self == AngerLevel::Angry
    }

    /// `getAmbientSound`.
    pub fn ambient_sound(self) -> &'static str {
        match self {
            AngerLevel::Calm => "minecraft:entity.warden.ambient",
            AngerLevel::Agitated => "minecraft:entity.warden.agitated",
            AngerLevel::Angry => "minecraft:entity.warden.angry",
        }
    }

    /// `getListeningSound`.
    pub fn listening_sound(self) -> &'static str {
        match self {
            AngerLevel::Calm => "minecraft:entity.warden.listening",
            _ => "minecraft:entity.warden.listening_angry",
        }
    }
}

/// One suspect: the entity, its UUID (for saving) and whether it is a player.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Suspect {
    pub id: EntityId,
    pub uuid: u128,
    pub player: bool,
    pub anger: i32,
}

/// `AngerManagement`: anger per suspect, sorted with the angriest (angry players first) on top,
/// plus the anger of suspects that are not loaded (by UUID).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AngerManagement {
    /// `conversionDelay`.
    pub conversion_delay: i32,
    /// `highestAnger`.
    pub highest_anger: i32,
    /// `suspects` with `angerBySuspect`, sorted.
    pub suspects: Vec<Suspect>,
    /// `angerByUuid`.
    pub by_uuid: Vec<(u128, i32)>,
}

/// What [`AngerManagement::tick`] needs to know about a suspect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SuspectState {
    /// Loaded and a valid target.
    Valid,
    /// Loaded but not a valid target (dead, creative, ...).
    Invalid,
    /// Gone: `keep` when it left with its chunk or dimension (its anger is kept by UUID).
    Gone { keep: bool },
}

impl AngerManagement {
    /// A fresh anger management (`conversionDelay` random in 0..=2).
    pub fn new(conversion_delay: i32) -> AngerManagement {
        AngerManagement {
            conversion_delay,
            ..AngerManagement::default()
        }
    }

    /// `tick(level, validEntity)` (every 20 ticks): UUID suspects that are loaded again
    /// (`resolve`) come back, every anger decays by one, invalid or gone suspects go (gone ones
    /// that only unloaded are kept by UUID), then the suspects are sorted.
    pub fn tick(
        &mut self,
        mut resolve: impl FnMut(u128) -> Option<(EntityId, bool)>,
        mut state: impl FnMut(EntityId) -> SuspectState,
    ) {
        self.conversion_delay -= 1;
        if self.conversion_delay <= 0 {
            // `convertFromUuids`.
            let mut i = 0;
            while i < self.by_uuid.len() {
                let (uuid, anger) = self.by_uuid[i];
                if let Some((id, player)) = resolve(uuid) {
                    self.by_uuid.remove(i);
                    match self.suspects.iter_mut().find(|s| s.id == id) {
                        Some(s) => s.anger = anger,
                        None => self.suspects.push(Suspect {
                            id,
                            uuid,
                            player,
                            anger,
                        }),
                    }
                } else {
                    i += 1;
                }
            }
            self.conversion_delay = 2;
        }
        self.by_uuid.retain_mut(|(_, anger)| {
            if *anger <= 1 {
                false
            } else {
                *anger -= 1;
                true
            }
        });
        let mut kept = Vec::new();
        self.suspects.retain_mut(|s| match state(s.id) {
            SuspectState::Valid if s.anger > 1 => {
                s.anger -= 1;
                true
            }
            SuspectState::Gone { keep: true } if s.anger > 1 => {
                kept.push((s.uuid, s.anger - 1));
                false
            }
            _ => false,
        });
        self.by_uuid.extend(kept);
        self.sort_and_update_highest_anger();
    }

    /// `sortAndUpdateHighestAnger`: the sorter's comparisons see every suspect once there are
    /// two or more, so the highest anger is the maximum.
    fn sort_and_update_highest_anger(&mut self) {
        self.suspects.sort_by(|a, b| {
            let (aa, ab) = (
                AngerLevel::by_anger(a.anger).is_angry(),
                AngerLevel::by_anger(b.anger).is_angry(),
            );
            ab.cmp(&aa)
                .then(b.player.cmp(&a.player))
                .then(b.anger.cmp(&a.anger))
        });
        self.highest_anger = match self.suspects.len() {
            0 => 0,
            1 => self.suspects[0].anger,
            _ => self.suspects.iter().map(|s| s.anger).max().unwrap_or(0),
        };
    }

    /// `increaseAnger(entity, increment)`: capped at 150 (a returning suspect adds its anger by
    /// UUID on top, as Vanilla does). Returns the new anger.
    pub fn increase_anger(
        &mut self,
        id: EntityId,
        uuid: u128,
        player: bool,
        increment: i32,
    ) -> i32 {
        let current = match self.suspects.iter_mut().find(|s| s.id == id) {
            Some(s) => {
                s.anger = (s.anger + increment).min(MAX_ANGER);
                s.anger
            }
            None => {
                let mut anger = increment.min(MAX_ANGER);
                if let Some(i) = self.by_uuid.iter().position(|(u, _)| *u == uuid) {
                    anger += self.by_uuid.remove(i).1;
                }
                self.suspects.push(Suspect {
                    id,
                    uuid,
                    player,
                    anger,
                });
                anger
            }
        };
        self.sort_and_update_highest_anger();
        current
    }

    /// `clearAnger(entity)`.
    pub fn clear_anger(&mut self, id: EntityId) {
        self.suspects.retain(|s| s.id != id);
        self.sort_and_update_highest_anger();
    }

    /// The anger at `id` (`angerBySuspect.getInt`).
    pub fn anger_at(&self, id: EntityId) -> i32 {
        self.suspects
            .iter()
            .find(|s| s.id == id)
            .map_or(0, |s| s.anger)
    }

    /// `getActiveAnger(currentTarget)`.
    pub fn active_anger(&self, target: Option<EntityId>) -> i32 {
        match target {
            None => self.highest_anger,
            Some(t) => self.anger_at(t),
        }
    }

    /// `getTopSuspect`: the first suspect `valid` accepts.
    pub fn top_suspect(&self, mut valid: impl FnMut(EntityId) -> bool) -> Option<EntityId> {
        self.suspects.iter().map(|s| s.id).find(|&id| valid(id))
    }

    /// The `anger` NBT (`{suspects: [{uuid, anger}]}`).
    pub fn save(&self) -> Compound {
        let list = self
            .suspects
            .iter()
            .map(|s| (s.uuid, s.anger))
            .chain(self.by_uuid.iter().copied())
            .map(|(uuid, anger)| {
                let mut c = Compound::new();
                c.insert("uuid", Tag::IntArray(uuid_ints(uuid)));
                c.insert("anger", Tag::Int(anger.max(0)));
                Tag::Compound(c)
            })
            .collect();
        let mut out = Compound::new();
        out.insert("suspects", Tag::List(List(list)));
        out
    }

    /// Reads the `anger` NBT: every suspect waits by UUID until it is loaded.
    pub fn load(&mut self, tag: &Compound) {
        self.suspects.clear();
        self.by_uuid.clear();
        let Some(Tag::List(list)) = tag.get("suspects") else {
            return;
        };
        for t in &list.0 {
            let Tag::Compound(c) = t else { continue };
            let (Some(Tag::IntArray(u)), Some(Tag::Int(a))) = (c.get("uuid"), c.get("anger"))
            else {
                continue;
            };
            if u.len() == 4 && *a >= 0 {
                let uuid = u
                    .iter()
                    .fold(0u128, |acc, &w| acc << 32 | u128::from(w as u32));
                self.by_uuid.push((uuid, *a));
            }
        }
    }
}

fn uuid_ints(uuid: u128) -> Vec<i32> {
    (0..4)
        .map(|i| (uuid >> (96 - 32 * i)) as u32 as i32)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u64) -> EntityId {
        use vanillite_base::slot_map::{RawKey, SlotKey};
        EntityId::from_raw(RawKey::from_bits(1 << 32 | n).expect("live key"))
    }

    #[test]
    fn levels_by_anger() {
        assert_eq!(AngerLevel::by_anger(0), AngerLevel::Calm);
        assert_eq!(AngerLevel::by_anger(39), AngerLevel::Calm);
        assert_eq!(AngerLevel::by_anger(40), AngerLevel::Agitated);
        assert_eq!(AngerLevel::by_anger(80), AngerLevel::Angry);
    }

    #[test]
    fn angry_players_sort_first_and_anger_caps() {
        let mut a = AngerManagement::new(2);
        a.increase_anger(id(1), 1, false, 100);
        a.increase_anger(id(2), 2, true, 50);
        a.increase_anger(id(3), 3, true, 90);
        let order: Vec<u128> = a.suspects.iter().map(|s| s.uuid).collect();
        // Angry ones first (players before mobs), then the calmer player.
        assert_eq!(order, vec![3, 1, 2]);
        assert_eq!(a.highest_anger, 100);
        assert_eq!(a.increase_anger(id(2), 2, true, 500), MAX_ANGER);
        assert_eq!(a.suspects[0].uuid, 2);
    }

    #[test]
    fn anger_decays_and_unloaded_suspects_are_kept_by_uuid() {
        let mut a = AngerManagement::new(5);
        a.increase_anger(id(1), 11, true, 3);
        a.increase_anger(id(2), 22, false, 2);
        a.tick(
            |_| None,
            |e| {
                if e == id(1) {
                    SuspectState::Gone { keep: true }
                } else {
                    SuspectState::Valid
                }
            },
        );
        assert_eq!(a.by_uuid, vec![(11, 2)]);
        assert_eq!(a.anger_at(id(2)), 1);
        // A suspect at 1 goes on the next tick.
        a.tick(|_| None, |_| SuspectState::Valid);
        assert!(a.suspects.is_empty());
        assert_eq!(a.by_uuid, vec![(11, 1)]);
        // Coming back adds the kept anger.
        a.by_uuid = vec![(11, 30)];
        assert_eq!(a.increase_anger(id(7), 11, true, 35), 65);
        assert!(a.by_uuid.is_empty());
    }
}

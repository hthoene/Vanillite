//! Entities per section, in Vanilla's order.

use rustc_hash::FxHashMap;
use vanillite_base::Aabb;
use vanillite_base::SectionPos;
use vanillite_base::math::floor;

use super::EntityId;

/// The classes an entity is indexed under besides "all", so that typed queries skip the
/// others (like Vanilla's `ClassInstanceMultiMap`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EntityClass {
    /// Players.
    Player,
    /// Everything else.
    Other,
}

/// The entities of one section, in insertion order (Vanilla keeps `ArrayList` order).
#[derive(Debug, Default)]
struct Section {
    all: Vec<EntityId>,
    players: Vec<EntityId>,
}

impl Section {
    fn list(&self, players_only: bool) -> &[EntityId] {
        if players_only {
            &self.players
        } else {
            &self.all
        }
    }
}

/// Entity ids per section (`EntitySectionStorage`). Empty sections are dropped.
#[derive(Debug, Default)]
pub struct SectionIndex {
    sections: FxHashMap<i64, Section>,
}

impl SectionIndex {
    pub(super) fn insert(&mut self, section: SectionPos, class: EntityClass, id: EntityId) {
        let entry = self.sections.entry(section.as_long()).or_default();
        entry.all.push(id);
        if class == EntityClass::Player {
            entry.players.push(id);
        }
    }

    pub(super) fn remove(&mut self, section: SectionPos, class: EntityClass, id: EntityId) {
        let key = section.as_long();
        let Some(entry) = self.sections.get_mut(&key) else {
            return;
        };
        remove_ordered(&mut entry.all, id);
        if class == EntityClass::Player {
            remove_ordered(&mut entry.players, id);
        }
        if entry.all.is_empty() {
            self.sections.remove(&key);
        }
    }

    /// The entities (or only the players) of one section.
    pub fn in_section(&self, section: SectionPos, players_only: bool) -> &[EntityId] {
        self.sections
            .get(&section.as_long())
            .map_or(&[], |s| s.list(players_only))
    }

    /// Number of non-empty sections.
    pub fn len(&self) -> usize {
        self.sections.len()
    }

    /// Whether no section holds entities.
    pub fn is_empty(&self) -> bool {
        self.sections.is_empty()
    }

    /// Calls `f` for the entities of all sections near `aabb` in Vanilla's order
    /// (`forEachAccessibleNonEmptySection`: X ascending, then the packed section key, which
    /// orders Z and Y as unsigned fields). The caller filters by bounding box.
    pub(super) fn for_each_near(
        &self,
        aabb: &Aabb,
        players_only: bool,
        mut f: impl FnMut(EntityId),
    ) {
        let min_x = floor(aabb.min_x - 2.0) >> 4;
        let min_y = floor(aabb.min_y - 4.0) >> 4;
        let min_z = floor(aabb.min_z - 2.0) >> 4;
        let max_x = floor(aabb.max_x + 2.0) >> 4;
        let max_y = floor(aabb.max_y) >> 4;
        let max_z = floor(aabb.max_z + 2.0) >> 4;
        let span =
            |min: i32, max: i32| u64::try_from(i64::from(max) - i64::from(min) + 1).unwrap_or(0);
        let volume = span(min_x, max_x)
            .saturating_mul(span(min_y, max_y))
            .saturating_mul(span(min_z, max_z));
        let in_range = |s: SectionPos| {
            (min_x..=max_x).contains(&s.x)
                && (min_y..=max_y).contains(&s.y)
                && (min_z..=max_z).contains(&s.z)
        };
        let mut keys: Vec<i64> = if volume <= self.sections.len() as u64 {
            let mut keys = Vec::new();
            for x in min_x..=max_x {
                for z in min_z..=max_z {
                    for y in min_y..=max_y {
                        let key = SectionPos::new(x, y, z).as_long();
                        if self.sections.contains_key(&key) {
                            keys.push(key);
                        }
                    }
                }
            }
            keys
        } else {
            self.sections
                .keys()
                .copied()
                .filter(|&key| in_range(SectionPos::from_long(key)))
                .collect()
        };
        // X sits in the top bits with its sign, so signed order is X ascending; within one X
        // the remaining bits compare like Vanilla's sorted long set.
        keys.sort_unstable();
        for key in keys {
            if let Some(section) = self.sections.get(&key) {
                section.list(players_only).iter().copied().for_each(&mut f);
            }
        }
    }
}

fn remove_ordered(list: &mut Vec<EntityId>, id: EntityId) {
    if let Some(index) = list.iter().position(|&e| e == id) {
        list.remove(index);
    }
}

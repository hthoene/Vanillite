//! A player's advancements (`PlayerAdvancements`, `AdvancementProgress`,
//! `AdvancementVisibilityEvaluator`): criterion progress, the trigger listeners of the
//! unfinished criteria indexed by trigger type, visibility and the `update_advancements` deltas.

use std::collections::BTreeSet;
use std::sync::Arc;

use vanillite_base::hash::FxHashMap;
use vanillite_protocol::packets::play::clientbound::{
    AdvancementProgress, SelectAdvancementsTab, UpdateAdvancements,
};
use vanillite_protocol::types::Identifier;

use super::Advancements;
use super::trigger::{Event, TRIGGERS, TriggerContext, TriggerKind};
use crate::inventory::PlayerInventory;
use crate::item::ItemStack;

/// `VISIBILITY_DEPTH`: how many ancestors decide the visibility of an unfinished advancement.
const VISIBILITY_DEPTH: usize = 2;
/// Main inventory, armour and off hand.
const SLOTS: usize = 41;

/// Epoch milliseconds now (`Instant.now()`).
fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

/// `AdvancementVisibilityEvaluator.VisibilityRule`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Rule {
    Show,
    Hide,
    NoChange,
}

/// What `/advancement` changed for one player.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grant {
    /// `GRANT`.
    Grant,
    /// `REVOKE`.
    Revoke,
}

/// A player's advancement progress.
#[derive(Debug)]
pub struct PlayerAdvancements {
    set: Arc<Advancements>,
    /// Obtained times (epoch milliseconds) of the criteria of advancements with progress.
    progress: FxHashMap<u32, Box<[Option<i64>]>>,
    visible: Vec<bool>,
    changed: BTreeSet<u32>,
    roots_to_update: BTreeSet<u32>,
    first_packet: bool,
    selected_tab: Option<u32>,
    /// Unfinished criteria by trigger type (`activeTriggers`).
    listeners: Vec<BTreeSet<(u32, u16)>>,
    /// Events fired by the game since the last [`process`](Self::process).
    events: Vec<Event>,
    /// Advancements completed since the last [`take_completed`](Self::take_completed).
    completed: Vec<u32>,
    /// The inventory as the listener last saw it (`lastSlots` of its slots).
    seen_inventory: Vec<ItemStack>,
}

impl PlayerAdvancements {
    /// `load`: the progress file (if any), then the listeners of everything unfinished.
    pub fn load(set: Arc<Advancements>, json: Option<&str>) -> PlayerAdvancements {
        let mut me = PlayerAdvancements {
            visible: vec![false; set.len()],
            set,
            progress: FxHashMap::default(),
            changed: BTreeSet::new(),
            roots_to_update: BTreeSet::new(),
            first_packet: true,
            selected_tab: None,
            listeners: vec![BTreeSet::new(); TRIGGERS.len()],
            events: Vec::new(),
            completed: Vec::new(),
            seen_inventory: Vec::new(),
        };
        if let Some(json) = json {
            me.apply_json(json);
        }
        let set = Arc::clone(&me.set);
        for (index, _) in set.iter() {
            me.register_listeners(index);
        }
        me
    }

    /// `applyFrom`: in the order of the first progress date, unknown advancements left out.
    fn apply_json(&mut self, json: &str) {
        let root: serde_json::Value = match serde_json::from_str(json) {
            Ok(v) => v,
            Err(e) => {
                log::error!("Couldn't parse player advancements: {e}");
                return;
            }
        };
        let Some(entries) = root.as_object() else {
            return;
        };
        let mut loaded = Vec::new();
        for (id, value) in entries {
            if id == "DataVersion" {
                continue;
            }
            let Some(index) = self.set.index(id) else {
                log::warn!(
                    "Ignored advancement '{id}' in progress file - it doesn't exist anymore?"
                );
                continue;
            };
            let Some(a) = self.set.get(index) else {
                continue;
            };
            let mut times = vec![None; a.criteria.len()].into_boxed_slice();
            let criteria = value.get("criteria").and_then(serde_json::Value::as_object);
            for (name, date) in criteria.into_iter().flatten() {
                let at = date
                    .as_str()
                    .and_then(vanillite_base::date::parse_date)
                    .and_then(|s| i64::try_from(s).ok())
                    .map(|s| s.saturating_mul(1000));
                if let (Some(c), Some(at)) = (a.criterion(name), at)
                    && let Some(slot) = times.get_mut(usize::from(c))
                {
                    *slot = Some(at);
                }
            }
            let first = times.iter().flatten().min().copied();
            loaded.push((first.is_none(), first, index, times));
        }
        loaded.sort_by_key(|(none, first, _, _)| (*none, *first));
        for (_, _, index, times) in loaded {
            self.progress.insert(index, times);
            self.changed.insert(index);
            self.mark_for_visibility_update(index);
        }
    }

    /// The progress file (`save`): advancements with progress in the order of their first
    /// progress, then `DataVersion`.
    pub fn to_json(&self) -> String {
        let mut entries: Vec<(i64, u32, &[Option<i64>])> = self
            .progress
            .iter()
            .filter_map(|(&i, times)| Some((times.iter().flatten().min().copied()?, i, &**times)))
            .collect();
        entries.sort_unstable_by_key(|(first, i, _)| (*first, *i));
        let mut out = String::from("{\n");
        for (_, index, times) in entries {
            let Some(a) = self.set.get(index) else {
                continue;
            };
            out.push_str(&format!("  \"{}\": {{\n    \"criteria\": {{", a.id));
            let mut first = true;
            for (c, time) in a.criteria.iter().zip(times) {
                let Some(ms) = time else {
                    continue;
                };
                out.push_str(if first { "\n" } else { ",\n" });
                first = false;
                let secs = u64::try_from(ms.div_euclid(1000)).unwrap_or(0);
                out.push_str(&format!(
                    "      {}: \"{}\"",
                    serde_json::Value::String(c.name.clone()),
                    vanillite_base::date::format_date(secs)
                ));
            }
            out.push_str(&format!(
                "\n    }},\n    \"done\": {}\n  }},\n",
                self.is_done(index)
            ));
        }
        out.push_str(&format!(
            "  \"DataVersion\": {}\n}}",
            vanillite_data::version::DATA_VERSION
        ));
        out
    }

    /// The advancements.
    pub fn advancements(&self) -> &Arc<Advancements> {
        &self.set
    }

    fn criterion_time(&self, index: u32, criterion: u16) -> Option<i64> {
        self.progress
            .get(&index)
            .and_then(|t| t.get(usize::from(criterion)).copied().flatten())
    }

    /// Whether a criterion is done.
    pub fn criterion_done(&self, index: u32, criterion: u16) -> bool {
        self.criterion_time(index, criterion).is_some()
    }

    /// `AdvancementProgress.isDone`: every requirement list has a done criterion.
    pub fn is_done(&self, index: u32) -> bool {
        let Some(a) = self.set.get(index) else {
            return false;
        };
        !a.requirements.is_empty()
            && a.requirements
                .iter()
                .all(|list| list.iter().any(|&c| self.criterion_done(index, c)))
    }

    /// `hasProgress`.
    pub fn has_progress(&self, index: u32) -> bool {
        self.progress
            .get(&index)
            .is_some_and(|t| t.iter().any(Option::is_some))
    }

    fn set_time(&mut self, index: u32, criterion: u16, time: Option<i64>) -> bool {
        let Some(a) = self.set.get(index) else {
            return false;
        };
        let len = a.criteria.len();
        let times = self
            .progress
            .entry(index)
            .or_insert_with(|| vec![None; len].into_boxed_slice());
        match times.get_mut(usize::from(criterion)) {
            Some(slot) if slot.is_some() != time.is_some() => {
                *slot = time;
                true
            }
            _ => false,
        }
    }

    /// `award`: grants a criterion; returns whether it changed. A completed advancement is
    /// queued for its rewards and announcement ([`take_completed`](Self::take_completed)).
    pub fn award(&mut self, index: u32, criterion: u16) -> bool {
        let was_done = self.is_done(index);
        let result = self.set_time(index, criterion, Some(now_millis()));
        if result {
            self.unregister_listeners(index);
            self.changed.insert(index);
            if !was_done && self.is_done(index) {
                self.completed.push(index);
            }
        }
        if !was_done && self.is_done(index) {
            self.mark_for_visibility_update(index);
        }
        result
    }

    /// `revoke`.
    pub fn revoke(&mut self, index: u32, criterion: u16) -> bool {
        let was_done = self.is_done(index);
        let result = self.set_time(index, criterion, None);
        if result {
            self.register_listeners(index);
            self.changed.insert(index);
        }
        if was_done && !self.is_done(index) {
            self.mark_for_visibility_update(index);
        }
        result
    }

    /// `AdvancementCommands.Action.perform`: grants the remaining or revokes the completed
    /// criteria; whether the advancement was not done (grant) or had progress (revoke).
    pub fn perform(&mut self, action: Grant, index: u32) -> bool {
        let count = self.set.get(index).map_or(0, |a| a.criteria.len());
        let criteria = (0..count).filter_map(|c| u16::try_from(c).ok());
        match action {
            Grant::Grant => {
                if self.is_done(index) {
                    return false;
                }
                for c in criteria {
                    if !self.criterion_done(index, c) {
                        self.award(index, c);
                    }
                }
                true
            }
            Grant::Revoke => {
                if !self.has_progress(index) {
                    return false;
                }
                for c in criteria {
                    if self.criterion_done(index, c) {
                        self.revoke(index, c);
                    }
                }
                true
            }
        }
    }

    /// Whether rewards and announcements are due.
    pub fn has_completed(&self) -> bool {
        !self.completed.is_empty()
    }

    /// The completed advancements whose rewards and announcements are due.
    pub fn take_completed(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.completed)
    }

    fn mark_for_visibility_update(&mut self, index: u32) {
        if self.set.get(index).is_some_and(|a| a.in_tree) {
            self.roots_to_update.insert(self.set.root(index));
        }
    }

    fn register_listeners(&mut self, index: u32) {
        if self.is_done(index) {
            return;
        }
        let set = Arc::clone(&self.set);
        let Some(a) = set.get(index) else {
            return;
        };
        for (c, criterion) in (0u16..).zip(&a.criteria) {
            if !self.criterion_done(index, c)
                && let Some(l) = self.listeners.get_mut(criterion.trigger.index())
            {
                l.insert((index, c));
            }
        }
    }

    fn unregister_listeners(&mut self, index: u32) {
        let done = self.is_done(index);
        let set = Arc::clone(&self.set);
        let Some(a) = set.get(index) else {
            return;
        };
        for (c, criterion) in (0u16..).zip(&a.criteria) {
            if (done || self.criterion_done(index, c))
                && let Some(l) = self.listeners.get_mut(criterion.trigger.index())
            {
                l.remove(&(index, c));
            }
        }
    }

    /// Whether an unfinished criterion listens for `kind`.
    pub fn listens(&self, kind: TriggerKind) -> bool {
        self.listeners
            .get(kind.index())
            .is_some_and(|l| !l.is_empty())
    }

    /// Queues an event of the game for [`process`](Self::process) if anything listens for it.
    pub fn trigger(&mut self, event: Event) {
        if self.listens(event.kind()) {
            self.events.push(event);
        }
    }

    /// `ContainerListener.slotChanged` of the inventory: `inventory_changed` for every slot of
    /// the inventory that changed since the last call.
    pub fn check_inventory(&mut self, inventory: &PlayerInventory) {
        let slots = inventory
            .items
            .iter()
            .chain(inventory.armor.iter())
            .chain(std::iter::once(&inventory.offhand));
        self.seen_inventory.resize(SLOTS, ItemStack::EMPTY);
        let listens = self.listens(TriggerKind::INVENTORY_CHANGED);
        for (seen, now) in self.seen_inventory.iter_mut().zip(slots) {
            if !crate::item::matches(seen, now) {
                *seen = now.clone();
                if listens {
                    self.events.push(Event::InventoryChanged(now.clone()));
                }
            }
        }
    }

    /// Whether events wait for [`process`](Self::process).
    pub fn has_events(&self) -> bool {
        !self.events.is_empty()
    }

    /// Evaluates the queued events, then `extra`.
    pub fn process(&mut self, extra: impl IntoIterator<Item = Event>, ctx: &TriggerContext<'_>) {
        for event in std::mem::take(&mut self.events).into_iter().chain(extra) {
            self.fire(&event, ctx);
        }
    }

    /// `SimpleCriterionTrigger.trigger`: awards every listening criterion the event meets.
    pub fn fire(&mut self, event: &Event, ctx: &TriggerContext<'_>) {
        let Some(listeners) = self.listeners.get(event.kind().index()) else {
            return;
        };
        if listeners.is_empty() {
            return;
        }
        let set = Arc::clone(&self.set);
        let matched: Vec<(u32, u16)> = listeners
            .iter()
            .copied()
            .filter(|&(a, c)| {
                set.get(a)
                    .and_then(|a| a.criteria.get(usize::from(c)))
                    .is_some_and(|c| c.conditions.matches(event, ctx))
            })
            .collect();
        for (a, c) in matched {
            self.award(a, c);
        }
    }

    /// `evaluateVisibility` of the tree under `index`; returns whether it or a descendant is
    /// done.
    fn evaluate(&self, index: u32, stack: &mut Vec<Rule>, out: &mut Vec<(u32, bool)>) -> bool {
        let done = self.is_done(index);
        let a = self.set.get(index);
        let rule = match a.and_then(|a| a.display.as_ref()) {
            None => Rule::Hide,
            Some(_) if done => Rule::Show,
            Some(d) if d.hidden => Rule::Hide,
            Some(_) => Rule::NoChange,
        };
        let mut any_done = done;
        stack.push(rule);
        for &child in a.map_or(&[][..], |a| a.children.as_slice()) {
            any_done |= self.evaluate(child, stack, out);
        }
        let shown = any_done
            || stack
                .iter()
                .rev()
                .take(VISIBILITY_DEPTH.saturating_add(1))
                .find(|r| **r != Rule::NoChange)
                .is_some_and(|r| *r == Rule::Show);
        stack.pop();
        out.push((index, shown));
        any_done
    }

    fn progress_entry(&self, index: u32) -> Option<AdvancementProgress> {
        let a = self.set.get(index)?;
        Some(AdvancementProgress {
            id: Identifier::new(&a.id).ok()?,
            criteria: (0u16..)
                .zip(&a.criteria)
                .map(|(c, criterion)| (criterion.name.clone(), self.criterion_time(index, c)))
                .collect(),
        })
    }

    /// `flushDirty`: the visibility and progress changes since the last call; the first call
    /// resets the client's advancements.
    pub fn flush(&mut self, show_advancements: bool) -> Option<UpdateAdvancements> {
        let mut packet = None;
        if self.first_packet || !self.roots_to_update.is_empty() || !self.changed.is_empty() {
            let mut added = Vec::new();
            let mut removed = Vec::new();
            for root in std::mem::take(&mut self.roots_to_update) {
                let mut out = Vec::new();
                let mut stack = vec![Rule::NoChange; VISIBILITY_DEPTH.saturating_add(1)];
                self.evaluate(root, &mut stack, &mut out);
                for (index, shown) in out {
                    let Some(slot) = self
                        .visible
                        .get_mut(usize::try_from(index).unwrap_or(usize::MAX))
                    else {
                        continue;
                    };
                    if shown && !*slot {
                        *slot = true;
                        if let Some(a) = self.set.get(index) {
                            added.push(a.entry());
                        }
                        self.changed.insert(index);
                    } else if !shown && *slot {
                        *slot = false;
                        if let Some(id) = self
                            .set
                            .get(index)
                            .and_then(|a| Identifier::new(&a.id).ok())
                        {
                            removed.push(id);
                        }
                    }
                }
            }
            let progress: Vec<AdvancementProgress> = std::mem::take(&mut self.changed)
                .into_iter()
                .filter(|&i| self.is_visible(i))
                .filter_map(|i| self.progress_entry(i))
                .collect();
            if !progress.is_empty() || !added.is_empty() || !removed.is_empty() {
                packet = Some(UpdateAdvancements {
                    reset: self.first_packet,
                    added,
                    removed,
                    progress,
                    show_advancements,
                });
            }
        }
        self.first_packet = false;
        packet
    }

    fn is_visible(&self, index: u32) -> bool {
        usize::try_from(index)
            .ok()
            .and_then(|i| self.visible.get(i))
            .copied()
            .unwrap_or(false)
    }

    /// `setSelectedTab`: a visible root, or none; the packet when the tab changed.
    pub fn select_tab(&mut self, id: Option<&str>) -> Option<SelectAdvancementsTab> {
        let old = self.selected_tab;
        self.selected_tab = id.and_then(|id| self.set.index(id)).filter(|&i| {
            self.set
                .get(i)
                .is_some_and(|a| a.is_root() && a.display.is_some())
        });
        (old != self.selected_tab).then(|| SelectAdvancementsTab {
            tab: self
                .selected_tab
                .and_then(|i| self.set.get(i))
                .and_then(|a| Identifier::new(&a.id).ok()),
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn set() -> Arc<Advancements> {
        let mut files = BTreeMap::new();
        let display = |bg: bool, hidden: bool| {
            format!(
                "\"display\": {{{} \"hidden\": {hidden}, \"icon\": {{\"id\": \"minecraft:stone\"}}, \"title\": \"t\", \"description\": \"d\"}}",
                if bg {
                    "\"background\": \"minecraft:b\","
                } else {
                    ""
                }
            )
        };
        files.insert(
            "minecraft:root".to_owned(),
            format!(
                "{{\"criteria\": {{\"x\": {{\"trigger\": \"minecraft:tick\"}}}}, {}}}",
                display(true, false)
            ),
        );
        files.insert(
            "minecraft:child".to_owned(),
            format!(
                "{{\"parent\": \"minecraft:root\", \"criteria\": {{\"a\": {{\"trigger\": \"minecraft:impossible\"}}, \"b\": {{\"trigger\": \"minecraft:impossible\"}}}}, {}}}",
                display(false, false)
            ),
        );
        files.insert(
            "minecraft:secret".to_owned(),
            format!(
                "{{\"parent\": \"minecraft:child\", \"criteria\": {{\"a\": {{\"trigger\": \"minecraft:impossible\"}}}}, {}}}",
                display(false, true)
            ),
        );
        Arc::new(Advancements::load(&files))
    }

    #[test]
    fn award_visibility_and_save() {
        let set = set();
        let root = set.index("minecraft:root").unwrap();
        let child = set.index("minecraft:child").unwrap();
        let secret = set.index("minecraft:secret").unwrap();
        let mut p = PlayerAdvancements::load(Arc::clone(&set), None);
        assert!(p.listens(TriggerKind::TICK));
        // A fresh player sees nothing.
        assert!(p.flush(true).is_none());
        assert!(p.award(root, 0));
        assert!(!p.award(root, 0));
        assert_eq!(p.take_completed(), vec![root]);
        assert!(!p.listens(TriggerKind::TICK));
        let packet = p.flush(true).unwrap();
        assert!(!packet.reset);
        // The root and its child are visible, the hidden grandchild is not.
        let added: Vec<&str> = packet.added.iter().map(|e| e.id.as_str()).collect();
        assert!(added.contains(&"minecraft:root") && added.contains(&"minecraft:child"));
        assert!(!added.contains(&"minecraft:secret"));
        assert!(p.perform(Grant::Grant, child));
        assert!(!p.perform(Grant::Grant, child));
        // A hidden advancement stays hidden until it is done.
        let packet = p.flush(false).unwrap();
        assert!(packet.added.is_empty() && packet.progress.len() == 1);
        let json = p.to_json();
        let back = PlayerAdvancements::load(Arc::clone(&set), Some(&json));
        assert!(back.is_done(root) && back.is_done(child) && !back.is_done(secret));
        let mut back = back;
        let first = back.flush(true).unwrap();
        assert!(first.reset);
        assert_eq!(first.progress.len(), 2);
        assert!(back.perform(Grant::Revoke, child));
        assert!(!back.is_done(child));
        assert!(back.select_tab(Some("minecraft:root")).is_some());
        assert!(back.select_tab(Some("minecraft:root")).is_none());
        assert!(
            back.select_tab(Some("minecraft:child"))
                .unwrap()
                .tab
                .is_none()
        );
    }
}

//! Advancements of the data packs (`Advancement`, `ServerAdvancementManager`,
//! `AdvancementTree`, `TreeNodePosition`): definitions parsed once, the tree with the positions
//! of the advancement screen, and the network form encoded once at load. A player's progress
//! is [`PlayerAdvancements`].

// SEC-07: data pack files and the progress files are input.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::as_conversions
)]

pub mod predicate;
mod progress;
pub mod trigger;

use std::collections::{BTreeMap, HashMap};

pub use progress::{Grant, PlayerAdvancements};
use serde_json::Value;
use vanillite_protocol::packets::play::clientbound::{AdvancementDisplay, AdvancementEntry};
use vanillite_protocol::text::{Color, Component, HoverEvent, NamedColor};
use vanillite_protocol::types::Identifier;

use predicate::{Res, object, qualify, string};
use trigger::{Conditions, TriggerKind};

/// `AdvancementType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Frame {
    Task,
    Challenge,
    Goal,
}

impl Frame {
    /// The serialized name.
    pub fn name(self) -> &'static str {
        match self {
            Frame::Task => "task",
            Frame::Challenge => "challenge",
            Frame::Goal => "goal",
        }
    }

    fn id(self) -> i32 {
        match self {
            Frame::Task => 0,
            Frame::Challenge => 1,
            Frame::Goal => 2,
        }
    }

    /// `getChatColor`.
    pub fn color(self) -> Color {
        Color::Named(match self {
            Frame::Challenge => NamedColor::DarkPurple,
            _ => NamedColor::Green,
        })
    }
}

/// `DisplayInfo`.
#[derive(Clone, Debug, PartialEq)]
pub struct Display {
    pub title: Component,
    pub description: Component,
    /// Icon item id and count (component changes of icons are not kept).
    pub icon: (u16, i32),
    pub background: Option<String>,
    pub frame: Frame,
    pub show_toast: bool,
    pub announce_to_chat: bool,
    pub hidden: bool,
}

/// `AdvancementRewards`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rewards {
    pub experience: i32,
    /// Loot table ids.
    pub loot: Vec<String>,
    /// Recipe ids.
    pub recipes: Vec<String>,
    /// The function id.
    pub function: Option<String>,
}

/// A named criterion.
#[derive(Clone, Debug, PartialEq)]
pub struct Criterion {
    pub name: String,
    pub trigger: TriggerKind,
    pub conditions: Conditions,
}

/// An advancement with its place in the tree.
#[derive(Clone, Debug, PartialEq)]
pub struct Advancement {
    pub id: String,
    /// The parent's index; `None` for roots and advancements outside the tree.
    pub parent: Option<u32>,
    pub display: Option<Display>,
    pub rewards: Rewards,
    /// The criteria, sorted by name.
    pub criteria: Vec<Criterion>,
    /// `AdvancementRequirements`: all lists, any criterion (index) of each list.
    pub requirements: Vec<Vec<u16>>,
    pub sends_telemetry_event: bool,
    /// Whether it is in the tree (its parents exist).
    pub in_tree: bool,
    /// Children in insertion order.
    pub children: Vec<u32>,
    /// `Advancement.name`: the decorated title, or the id.
    pub name: Component,
    parent_id: Option<String>,
    x: f32,
    y: f32,
}

impl Advancement {
    /// A criterion's index by name.
    pub fn criterion(&self, name: &str) -> Option<u16> {
        self.criteria
            .iter()
            .position(|c| c.name == name)
            .and_then(|i| u16::try_from(i).ok())
    }

    /// `isRoot`.
    pub fn is_root(&self) -> bool {
        self.parent_id.is_none()
    }

    /// `ClientboundUpdateAdvancementsPacket.PositionedAdvancement`.
    pub fn entry(&self) -> AdvancementEntry {
        let id = |s: &str| Identifier::new(s).unwrap_or_else(|_| Identifier::vanilla("empty"));
        AdvancementEntry {
            id: id(&self.id),
            parent: self.parent_id.as_deref().map(id),
            display: self.display.as_ref().map(|d| AdvancementDisplay {
                title: d.title.clone(),
                description: d.description.clone(),
                icon_item: i32::from(d.icon.0),
                icon_count: d.icon.1,
                frame: d.frame.id(),
                background: d.background.as_deref().map(id),
                show_toast: d.show_toast,
                hidden: d.hidden,
            }),
            requirements: self
                .requirements
                .iter()
                .map(|list| {
                    list.iter()
                        .filter_map(|&c| self.criteria.get(usize::from(c)))
                        .map(|c| c.name.clone())
                        .collect()
                })
                .collect(),
            sends_telemetry_event: self.sends_telemetry_event,
            x: self.x,
            y: self.y,
        }
    }
}

/// The advancements of the data packs (`ServerAdvancementManager` with its tree).
#[derive(Debug, Default)]
pub struct Advancements {
    list: Vec<Advancement>,
    by_id: HashMap<String, u32>,
    /// Indices in tree insertion order.
    tree_order: Vec<u32>,
}

/// `DisplayInfo.CODEC`.
fn parse_display(v: &Value) -> Res<Display> {
    let o = object(v)?;
    let icon = object(o.get("icon").ok_or("display without icon")?)?;
    let item_name = qualify(string(icon.get("id").ok_or("icon without id")?)?);
    let item = vanillite_data::registry::ITEM
        .id(&item_name)
        .ok_or_else(|| format!("unknown item {item_name}"))?;
    let count = icon
        .get("count")
        .map_or(Some(1), |c| c.as_i64().and_then(|c| i32::try_from(c).ok()))
        .ok_or("bad icon count")?;
    let component = |key: &str| -> Res<Component> {
        Component::try_from_json(o.get(key).ok_or_else(|| format!("display without {key}"))?)
            .map_err(|e| format!("{key}: {e}"))
    };
    let flag = |key: &str, default: bool| -> Res<bool> {
        o.get(key)
            .map_or(Some(default), Value::as_bool)
            .ok_or_else(|| format!("{key} is not a boolean"))
    };
    let frame = match o.get("frame").map(string).transpose()? {
        None | Some("task") => Frame::Task,
        Some("challenge") => Frame::Challenge,
        Some("goal") => Frame::Goal,
        Some(other) => return Err(format!("unknown frame {other}")),
    };
    Ok(Display {
        title: component("title")?,
        description: component("description")?,
        icon: (item, count),
        background: o.get("background").map(string).transpose()?.map(qualify),
        frame,
        show_toast: flag("show_toast", true)?,
        announce_to_chat: flag("announce_to_chat", true)?,
        hidden: flag("hidden", false)?,
    })
}

/// `AdvancementRewards.CODEC`.
fn parse_rewards(v: Option<&Value>) -> Res<Rewards> {
    let Some(v) = v else {
        return Ok(Rewards::default());
    };
    let o = object(v)?;
    let ids = |key: &str| -> Res<Vec<String>> {
        match o.get(key) {
            None => Ok(Vec::new()),
            Some(Value::String(s)) => Ok(vec![qualify(s)]),
            Some(Value::Array(list)) => list.iter().map(|e| string(e).map(qualify)).collect(),
            Some(other) => Err(format!("{key}: not a list {other}")),
        }
    };
    Ok(Rewards {
        experience: o
            .get("experience")
            .map_or(Some(0), |e| e.as_i64().and_then(|e| i32::try_from(e).ok()))
            .ok_or("bad experience")?,
        loot: ids("loot")?,
        recipes: ids("recipes")?,
        function: o.get("function").map(string).transpose()?.map(qualify),
    })
}

/// `Advancement.decorateName`: `[title]` in the frame's colour with the title and the
/// description on hover.
fn decorate_name(d: &Display) -> Component {
    let color = d.frame.color();
    let mut tooltip = d.title.clone();
    if tooltip.style.color.is_none() {
        tooltip.style.color = Some(color);
    }
    let tooltip = tooltip
        .append(Component::text("\n"))
        .append(d.description.clone());
    let title = d.title.clone().hover(HoverEvent::ShowText(tooltip));
    Component::translatable("chat.square_brackets", vec![title]).color(color)
}

/// `Advancement.CODEC` with its validation.
fn parse(id: &str, text: &str) -> Res<Advancement> {
    let root: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let o = object(&root)?;
    let parent_id = o.get("parent").map(string).transpose()?.map(qualify);
    let display = o.get("display").map(parse_display).transpose()?;
    let criteria_json = object(o.get("criteria").ok_or("missing criteria")?)?;
    if criteria_json.is_empty() {
        return Err("Advancement criteria cannot be empty".into());
    }
    let mut criteria = Vec::with_capacity(criteria_json.len());
    for (name, c) in criteria_json {
        let c = object(c)?;
        let trigger_name = string(c.get("trigger").ok_or("criterion without trigger")?)?;
        let trigger = TriggerKind::by_name(trigger_name)
            .ok_or_else(|| format!("unknown trigger {trigger_name}"))?;
        let conditions = Conditions::parse(trigger, c.get("conditions"))
            .map_err(|e| format!("criterion {name}: {e}"))?;
        criteria.push(Criterion {
            name: name.clone(),
            trigger,
            conditions,
        });
    }
    let index = |name: &str| -> Res<u16> {
        criteria
            .iter()
            .position(|c| c.name == name)
            .and_then(|i| u16::try_from(i).ok())
            .ok_or_else(|| format!("requirements name unknown criterion {name}"))
    };
    let requirements = match o.get("requirements") {
        None => (0..criteria.len())
            .filter_map(|i| u16::try_from(i).ok())
            .map(|i| vec![i])
            .collect(),
        Some(Value::Array(lists)) => {
            let mut out = Vec::with_capacity(lists.len());
            for list in lists {
                let names = list.as_array().ok_or("requirements are not lists")?;
                out.push(
                    names
                        .iter()
                        .map(|n| string(n).and_then(index))
                        .collect::<Res<Vec<u16>>>()?,
                );
            }
            out
        }
        Some(other) => return Err(format!("bad requirements {other}")),
    };
    let referenced: std::collections::BTreeSet<u16> =
        requirements.iter().flatten().copied().collect();
    if referenced.len() != criteria.len() {
        return Err(
            "Advancement completion requirements did not exactly match specified criteria".into(),
        );
    }
    let has_background = display.as_ref().is_some_and(|d| d.background.is_some());
    if parent_id.is_none() && display.is_some() && !has_background {
        return Err("Visible advancement roots must have background".into());
    }
    if parent_id.is_some() && has_background {
        return Err("Only advancement roots can have background".into());
    }
    let name = display
        .as_ref()
        .map_or_else(|| Component::text(id), decorate_name);
    Ok(Advancement {
        id: id.to_owned(),
        parent: None,
        display,
        rewards: parse_rewards(o.get("rewards"))?,
        criteria,
        requirements,
        sends_telemetry_event: o
            .get("sends_telemetry_event")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        in_tree: false,
        children: Vec::new(),
        name,
        parent_id,
        x: 0.0,
        y: 0.0,
    })
}

impl Advancements {
    /// Parses the advancement files (id → JSON) in id order, builds the tree and lays out the
    /// tabs. Invalid files are logged and left out, like a failed registry entry.
    pub fn load(files: &BTreeMap<String, String>) -> Advancements {
        let mut set = Advancements::default();
        for (id, text) in files {
            match parse(id, text) {
                Ok(a) => {
                    let index = u32::try_from(set.list.len()).unwrap_or(u32::MAX);
                    set.by_id.insert(id.clone(), index);
                    set.list.push(a);
                }
                Err(e) => log::error!("Couldn't parse advancement {id}: {e}"),
            }
        }
        set.build_tree();
        set.reposition();
        log::info!("Loaded {} advancements", set.tree_order.len());
        set
    }

    /// `AdvancementTree.addAll`: passes over the advancements until no more fit.
    fn build_tree(&mut self) {
        let mut pending: Vec<u32> = (0..self.list.len())
            .filter_map(|i| u32::try_from(i).ok())
            .collect();
        while !pending.is_empty() {
            let before = pending.len();
            let mut left = Vec::new();
            for i in pending {
                let parent = match self.get(i).and_then(|a| a.parent_id.clone()) {
                    None => None,
                    Some(p) => match self.by_id.get(&p).copied() {
                        Some(pi) if self.get(pi).is_some_and(|a| a.in_tree) => Some(pi),
                        _ => {
                            left.push(i);
                            continue;
                        }
                    },
                };
                if let Some(pi) = parent
                    && let Some(p) = self.get_mut(pi)
                {
                    p.children.push(i);
                }
                if let Some(a) = self.get_mut(i) {
                    a.parent = parent;
                    a.in_tree = true;
                }
                self.tree_order.push(i);
            }
            if left.len() == before {
                let ids: Vec<&str> = left
                    .iter()
                    .filter_map(|&i| self.get(i).map(|a| a.id.as_str()))
                    .collect();
                log::error!("Couldn't load advancements: {ids:?}");
                break;
            }
            pending = left;
        }
    }

    /// `repositionNodes`: lays out every visible root's tab.
    fn reposition(&mut self) {
        let roots: Vec<u32> = self
            .tree_order
            .iter()
            .copied()
            .filter(|&i| {
                self.get(i)
                    .is_some_and(|a| a.parent.is_none() && a.display.is_some())
            })
            .collect();
        for root in roots {
            for (i, x, y) in layout::run(self, root) {
                if let Some(a) = self.get_mut(i) {
                    a.x = x;
                    a.y = y;
                }
            }
        }
    }

    /// The number of advancements.
    pub fn len(&self) -> usize {
        self.list.len()
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// An advancement.
    pub fn get(&self, index: u32) -> Option<&Advancement> {
        self.list.get(usize::try_from(index).ok()?)
    }

    fn get_mut(&mut self, index: u32) -> Option<&mut Advancement> {
        self.list.get_mut(usize::try_from(index).ok()?)
    }

    /// An advancement's index by id.
    pub fn index(&self, id: &str) -> Option<u32> {
        self.by_id.get(id).copied()
    }

    /// All advancements in id order (`getAllAdvancements`).
    pub fn iter(&self) -> impl Iterator<Item = (u32, &Advancement)> {
        (0u32..).zip(self.list.iter())
    }

    /// The root of an advancement's tree (`AdvancementNode.root`).
    pub fn root(&self, mut index: u32) -> u32 {
        while let Some(parent) = self.get(index).and_then(|a| a.parent) {
            index = parent;
        }
        index
    }

    /// The parents (nearest first), for `/advancement ... until|through`.
    pub fn parents(&self, index: u32) -> Vec<u32> {
        let mut out = Vec::new();
        let mut at = index;
        while let Some(parent) = self.get(at).and_then(|a| a.parent) {
            out.push(parent);
            at = parent;
        }
        out
    }

    /// All descendants in depth-first order (`AdvancementCommands.addChildren`).
    pub fn descendants(&self, index: u32, out: &mut Vec<u32>) {
        if let Some(a) = self.get(index) {
            for &child in &a.children {
                out.push(child);
                self.descendants(child, out);
            }
        }
    }
}

/// `TreeNodePosition`: the Reingold-Tilford layout of one tab, invisible nodes skipped.
mod layout {
    use super::Advancements;

    struct Node {
        adv: u32,
        parent: Option<usize>,
        previous: Option<usize>,
        child_index: i32,
        children: Vec<usize>,
        ancestor: usize,
        thread: Option<usize>,
        x: i32,
        y: f32,
        modifier: f32,
        change: f32,
        shift: f32,
    }

    struct Tree<'a> {
        set: &'a Advancements,
        nodes: Vec<Node>,
    }

    fn visible(set: &Advancements, adv: u32) -> bool {
        set.get(adv).is_some_and(|a| a.display.is_some())
    }

    impl Tree<'_> {
        fn node(&self, i: usize) -> Option<&Node> {
            self.nodes.get(i)
        }

        fn n(&mut self, i: usize) -> &mut Node {
            // Indices come from `nodes` itself.
            #[allow(clippy::indexing_slicing)]
            &mut self.nodes[i]
        }

        fn y(&self, i: usize) -> f32 {
            self.node(i).map_or(0.0, |n| n.y)
        }

        fn modifier(&self, i: usize) -> f32 {
            self.node(i).map_or(0.0, |n| n.modifier)
        }

        /// The constructor: the node and its visible descendants.
        fn create(
            &mut self,
            adv: u32,
            parent: Option<usize>,
            previous: Option<usize>,
            child_index: i32,
            depth: i32,
        ) -> usize {
            let me = self.nodes.len();
            self.nodes.push(Node {
                adv,
                parent,
                previous,
                child_index,
                children: Vec::new(),
                ancestor: me,
                thread: None,
                x: depth,
                y: -1.0,
                modifier: 0.0,
                change: 0.0,
                shift: 0.0,
            });
            let mut last = None;
            let children = self
                .set
                .get(adv)
                .map(|a| a.children.clone())
                .unwrap_or_default();
            for child in children {
                last = self.add_child(me, child, last);
            }
            me
        }

        /// `addChild`: visible children become nodes, invisible ones pass their children on.
        fn add_child(&mut self, me: usize, adv: u32, previous: Option<usize>) -> Option<usize> {
            if visible(self.set, adv) {
                let index = i32::try_from(self.n(me).children.len())
                    .unwrap_or(i32::MAX)
                    .saturating_add(1);
                let depth = self.n(me).x.saturating_add(1);
                let child = self.create(adv, Some(me), previous, index, depth);
                self.n(me).children.push(child);
                Some(child)
            } else {
                let mut previous = previous;
                let children = self
                    .set
                    .get(adv)
                    .map(|a| a.children.clone())
                    .unwrap_or_default();
                for grandchild in children {
                    previous = self.add_child(me, grandchild, previous);
                }
                previous
            }
        }

        fn first_walk(&mut self, me: usize) {
            let children = self.n(me).children.clone();
            let previous = self.n(me).previous;
            if children.is_empty() {
                let y = previous.map_or(0.0, |p| self.y(p) + 1.0);
                self.n(me).y = y;
                return;
            }
            let mut default_ancestor: Option<usize> = None;
            for &child in &children {
                self.first_walk(child);
                let d = default_ancestor.unwrap_or(child);
                default_ancestor = Some(self.apportion(child, d));
            }
            self.execute_shifts(me);
            let first = children.first().map_or(0.0, |&c| self.y(c));
            let last = children.last().map_or(0.0, |&c| self.y(c));
            let midpoint = (first + last) / 2.0;
            match previous {
                Some(p) => {
                    let y = self.y(p) + 1.0;
                    let n = self.n(me);
                    n.y = y;
                    n.modifier = y - midpoint;
                }
                None => self.n(me).y = midpoint,
            }
        }

        fn second_walk(&mut self, me: usize, mod_sum: f32, depth: i32, mut min: f32) -> f32 {
            let n = self.n(me);
            n.y += mod_sum;
            n.x = depth;
            if n.y < min {
                min = n.y;
            }
            let modifier = n.modifier;
            for child in self.n(me).children.clone() {
                min = self.second_walk(child, mod_sum + modifier, depth.saturating_add(1), min);
            }
            min
        }

        fn third_walk(&mut self, me: usize, offset: f32) {
            self.n(me).y += offset;
            for child in self.n(me).children.clone() {
                self.third_walk(child, offset);
            }
        }

        fn execute_shifts(&mut self, me: usize) {
            let mut shift = 0.0;
            let mut change = 0.0;
            for child in self.n(me).children.clone().into_iter().rev() {
                let c = self.n(child);
                c.y += shift;
                c.modifier += shift;
                change += c.change;
                shift += c.shift + change;
            }
        }

        fn previous_or_thread(&self, me: usize) -> Option<usize> {
            let n = self.node(me)?;
            n.thread.or_else(|| n.children.first().copied())
        }

        fn next_or_thread(&self, me: usize) -> Option<usize> {
            let n = self.node(me)?;
            n.thread.or_else(|| n.children.last().copied())
        }

        fn apportion(&mut self, me: usize, default_ancestor: usize) -> usize {
            let Some(previous) = self.n(me).previous else {
                return default_ancestor;
            };
            let Some(first) = self
                .n(me)
                .parent
                .and_then(|p| self.node(p))
                .and_then(|p| p.children.first().copied())
            else {
                return default_ancestor;
            };
            let (mut vir, mut vor, mut vil, mut vol) = (me, me, previous, first);
            let mut sir = self.modifier(me);
            let mut sor = sir;
            let mut sil = self.modifier(vil);
            let mut sol = self.modifier(vol);
            while let (Some(nil), Some(nir)) =
                (self.next_or_thread(vil), self.previous_or_thread(vir))
            {
                vil = nil;
                vir = nir;
                vol = self.previous_or_thread(vol).unwrap_or(vol);
                vor = self.next_or_thread(vor).unwrap_or(vor);
                self.n(vor).ancestor = me;
                let shift = self.y(vil) + sil - (self.y(vir) + sir) + 1.0;
                if shift > 0.0 {
                    let left = self.ancestor_of(vil, me, default_ancestor);
                    self.move_subtree(left, me, shift);
                    sir += shift;
                    sor += shift;
                }
                sil += self.modifier(vil);
                sir += self.modifier(vir);
                sol += self.modifier(vol);
                sor += self.modifier(vor);
            }
            if let Some(next) = self.next_or_thread(vil)
                && self.next_or_thread(vor).is_none()
            {
                let n = self.n(vor);
                n.thread = Some(next);
                n.modifier += sil - sor;
                return default_ancestor;
            }
            if let Some(prev) = self.previous_or_thread(vir)
                && self.previous_or_thread(vol).is_none()
            {
                let n = self.n(vol);
                n.thread = Some(prev);
                n.modifier += sir - sol;
            }
            me
        }

        /// `getAncestor`.
        fn ancestor_of(&self, me: usize, other: usize, default: usize) -> usize {
            let Some(ancestor) = self.node(me).map(|n| n.ancestor) else {
                return default;
            };
            let siblings = self
                .node(other)
                .and_then(|o| o.parent)
                .and_then(|p| self.node(p))
                .is_some_and(|p| p.children.contains(&ancestor));
            if siblings { ancestor } else { default }
        }

        /// `moveSubtree`.
        #[allow(clippy::cast_precision_loss, clippy::as_conversions)]
        fn move_subtree(&mut self, left: usize, right: usize, shift: f32) {
            let subtrees = self
                .n(right)
                .child_index
                .saturating_sub(self.n(left).child_index) as f32;
            if subtrees != 0.0 {
                self.n(right).change -= shift / subtrees;
                self.n(left).change += shift / subtrees;
            }
            let r = self.n(right);
            r.shift += shift;
            r.y += shift;
            r.modifier += shift;
        }
    }

    /// `TreeNodePosition.run` for a visible root: (advancement, x, y) of its visible nodes.
    #[allow(clippy::cast_precision_loss, clippy::as_conversions)]
    pub(super) fn run(set: &Advancements, root: u32) -> Vec<(u32, f32, f32)> {
        let mut tree = Tree {
            set,
            nodes: Vec::new(),
        };
        let top = tree.create(root, None, None, 1, 0);
        tree.first_walk(top);
        let start = tree.y(top);
        let min = tree.second_walk(top, 0.0, 0, start);
        if min < 0.0 {
            tree.third_walk(top, -min);
        }
        tree.nodes
            .iter()
            .map(|n| (n.adv, n.x as f32, n.y))
            .collect()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn adv(parent: Option<&str>, background: bool) -> String {
        let parent = parent.map_or(String::new(), |p| format!("\"parent\": \"{p}\","));
        let bg = if background {
            "\"background\": \"minecraft:b\","
        } else {
            ""
        };
        format!(
            "{{{parent} \"criteria\": {{\"a\": {{\"trigger\": \"minecraft:impossible\"}}}},
              \"display\": {{{bg} \"icon\": {{\"id\": \"minecraft:stone\"}},
              \"title\": \"t\", \"description\": \"d\"}}}}"
        )
    }

    /// Every advancement of the Vanilla data pack loads; picking up a crafting table completes
    /// `story/root` through `inventory_changed`.
    #[test]
    fn vanilla_advancements() {
        use crate::advancements::predicate::{EntityView, World};
        use crate::advancements::trigger::{TriggerContext, TriggerKind};
        use crate::inventory::PlayerInventory;
        use crate::item::ItemStack;

        let cache = std::env::var_os("VANILLITE_CACHE").map_or_else(
            || {
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../target/vanilla-cache")
            },
            std::path::PathBuf::from,
        );
        let jar = std::env::var_os("VANILLITE_SERVER_JAR").map(std::path::PathBuf::from);
        let pack = vanillite_datapack::vanilla::ensure_vanilla_pack(&cache, jar.as_deref())
            .expect("Vanilla data pack (set VANILLITE_SERVER_JAR for an offline copy)");
        let dir = pack.join("data/minecraft/advancement");
        let mut files = BTreeMap::new();
        let mut stack = vec![dir.clone()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap().map(Result::unwrap) {
                let path = e.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    let rel = path.strip_prefix(&dir).unwrap().with_extension("");
                    let id = format!("minecraft:{}", rel.to_string_lossy());
                    files.insert(id, std::fs::read_to_string(&path).unwrap());
                }
            }
        }
        let set = std::sync::Arc::new(Advancements::load(&files));
        assert_eq!(set.len(), files.len());
        assert_eq!(set.tree_order.len(), files.len());
        let root = set.index("minecraft:story/root").unwrap();
        let mine = set.index("minecraft:story/mine_stone").unwrap();
        assert_eq!(set.get(mine).unwrap().parent, Some(root));
        assert_eq!(set.get(root).unwrap().entry().x, 0.0);
        assert_eq!(set.get(mine).unwrap().entry().x, 1.0);

        struct Nowhere;
        impl World for Nowhere {
            fn dimension(&self) -> &str {
                "minecraft:overworld"
            }
            fn block(
                &self,
                _: vanillite_base::BlockPos,
            ) -> Option<vanillite_data::block::BlockState> {
                None
            }
            fn biome(&self, _: vanillite_base::BlockPos) -> Option<String> {
                None
            }
            fn in_tag(&self, _: &str, _: &str, _: &str) -> bool {
                false
            }
        }
        let mut progress = super::PlayerAdvancements::load(std::sync::Arc::clone(&set), None);
        assert!(progress.listens(TriggerKind::INVENTORY_CHANGED));
        let mut inventory = PlayerInventory::default();
        inventory.items[0] = ItemStack::new(
            vanillite_data::registry::ITEM
                .id("minecraft:crafting_table")
                .unwrap(),
            1,
        );
        progress.check_inventory(&inventory);
        let empty = ItemStack::EMPTY;
        let view = EntityView {
            kind: "minecraft:player",
            pos: vanillite_base::Vec3::ZERO,
            on_ground: true,
            on_fire: false,
            crouching: false,
            sprinting: false,
            swimming: false,
            flying: false,
            fall_flying: false,
            in_water: false,
            equipment: [&empty; 7],
            team: None,
            player: None,
        };
        progress.process([], &TriggerContext::new(&Nowhere, &view, &inventory));
        assert!(progress.is_done(root));
        assert!(progress.take_completed().contains(&root));
        let packet = progress.flush(true).unwrap();
        assert!(packet.reset);
        assert!(
            packet
                .added
                .iter()
                .any(|e| e.id.as_str() == "minecraft:story/root")
        );
    }

    #[test]
    fn tree_and_layout() {
        let mut files = BTreeMap::new();
        files.insert("minecraft:r".to_owned(), adv(None, true));
        files.insert("minecraft:a".to_owned(), adv(Some("minecraft:r"), false));
        files.insert("minecraft:b".to_owned(), adv(Some("minecraft:r"), false));
        files.insert("minecraft:c".to_owned(), adv(Some("minecraft:a"), false));
        files.insert(
            "minecraft:orphan".to_owned(),
            adv(Some("minecraft:none"), false),
        );
        files.insert("minecraft:bad".to_owned(), adv(None, false));
        let set = Advancements::load(&files);
        assert_eq!(set.len(), 5);
        let r = set.index("minecraft:r").unwrap();
        let a = set.index("minecraft:a").unwrap();
        let b = set.index("minecraft:b").unwrap();
        let c = set.index("minecraft:c").unwrap();
        assert!(
            !set.get(set.index("minecraft:orphan").unwrap())
                .unwrap()
                .in_tree
        );
        assert_eq!(set.root(c), r);
        assert_eq!(set.parents(c), vec![a, r]);
        let pos = |i: u32| {
            let e = set.get(i).unwrap().entry();
            (e.x, e.y)
        };
        assert_eq!(pos(r), (0.0, 0.5));
        assert_eq!(pos(a), (1.0, 0.0));
        assert_eq!(pos(c), (2.0, 0.0));
        assert_eq!(pos(b), (1.0, 1.0));
        let mut out = Vec::new();
        set.descendants(r, &mut out);
        assert_eq!(out, vec![a, c, b]);
    }
}

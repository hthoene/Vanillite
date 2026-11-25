//! The loom (`LoomMenu`), cartography table (`CartographyTableMenu`), crafter (`CrafterMenu`)
//! and lectern (`LecternMenu`) menus.

use vanillite_data::block::BlockKind;
use vanillite_data::item::ids as items;
use vanillite_protocol::codec::{Reader, write_varint};
use vanillite_protocol::components::ids as components;

use super::container::{ContainerView, MenuKind, Storage};
use super::{ClickContext, MenuSlots};
use crate::inventory::Drop;
use crate::item::{self, ItemStack};
use crate::level::BlockEntity;

/// `BannerBlockEntity.MAX_PATTERNS`.
const MAX_PATTERNS: usize = 6;

/// The `minecraft:banner_pattern` registry: names in id order and tags.
#[derive(Clone, Debug, Default)]
pub struct BannerPatterns {
    /// Names by id.
    pub names: Vec<String>,
    /// Tag id (without `#`) → pattern ids in tag order.
    pub tags: std::collections::HashMap<String, Vec<u32>>,
}

fn game_data() -> Option<std::sync::Arc<crate::player::play::GameData>> {
    crate::player::play::game_data()
}

/// `DyeColor` ids by name.
const DYE_COLORS: [&str; 16] = [
    "white",
    "orange",
    "magenta",
    "light_blue",
    "yellow",
    "lime",
    "pink",
    "gray",
    "light_gray",
    "cyan",
    "purple",
    "blue",
    "brown",
    "green",
    "red",
    "black",
];

/// The `dye` component of a stack (patched or the item's default), as a `DyeColor` id.
pub fn dye_color(stack: &ItemStack) -> Option<i32> {
    if let Some(p) = stack.patch() {
        if let Some(v) = p.get(components::DYE) {
            return Reader::new(v).varint().ok();
        }
        if p.is_removed(components::DYE) {
            return None;
        }
    }
    let name = item::item_id(stack).name();
    let color = name.strip_prefix("minecraft:")?.strip_suffix("_dye")?;
    DYE_COLORS
        .iter()
        .position(|c| *c == color)
        .and_then(|i| i32::try_from(i).ok())
}

/// The patterns of the `provides_banner_patterns` component (patched or the item's default).
fn provided_patterns(stack: &ItemStack) -> Option<Vec<u32>> {
    let patterns = &game_data()?.banner_patterns;
    if let Some(p) = stack.patch() {
        if let Some(v) = p.get(components::PROVIDES_BANNER_PATTERNS) {
            let mut r = Reader::new(v);
            let n = r.varint().ok()?;
            if n == 0 {
                let tag = r.string(32767).ok()?;
                return Some(patterns.tags.get(tag).cloned().unwrap_or_default());
            }
            let mut ids = Vec::new();
            for _ in 1..n {
                ids.push(u32::try_from(r.varint().ok()?).ok()?);
            }
            return Some(ids);
        }
        if p.is_removed(components::PROVIDES_BANNER_PATTERNS) {
            return None;
        }
    }
    // `<name>_banner_pattern` items provide `#minecraft:pattern_item/<name>`.
    let name = item::item_id(stack).name();
    let base = name
        .strip_prefix("minecraft:")?
        .strip_suffix("_banner_pattern")?;
    Some(
        patterns
            .tags
            .get(&format!("minecraft:pattern_item/{base}"))
            .cloned()
            .unwrap_or_default(),
    )
}

fn in_tag(stack: &ItemStack, tag: &str) -> bool {
    game_data().is_some_and(|d| d.item_in_tag(stack.item, tag))
}

/// `LoomMenu.isPatternItem`.
pub fn is_pattern_item(stack: &ItemStack) -> bool {
    in_tag(stack, "minecraft:loom_patterns") && provided_patterns(stack).is_some()
}

/// `LoomMenu.isDyeItem`.
pub fn is_dye_item(stack: &ItemStack) -> bool {
    in_tag(stack, "minecraft:loom_dyes") && dye_color(stack).is_some()
}

/// `item instanceof BannerItem`.
pub fn is_banner_item(stack: &ItemStack) -> bool {
    item::item_id(stack)
        .block()
        .is_some_and(|b| b.info().kind == BlockKind::BannerBlock)
}

/// Number of layers of a `banner_patterns` value.
fn layer_count(value: Option<&[u8]>) -> usize {
    value
        .and_then(|v| Reader::new(v).varint().ok())
        .and_then(|n| usize::try_from(n).ok())
        .unwrap_or(0)
}

/// `BannerPatternLayers.Builder().addAll(layers).add(pattern, color)`.
pub fn add_layer(stack: &mut ItemStack, pattern: u32, color: i32) {
    let old = stack
        .component(components::BANNER_PATTERNS)
        .map(<[u8]>::to_vec);
    let (count, rest) = match &old {
        Some(v) => {
            let mut r = Reader::new(v);
            let n = r.varint().unwrap_or(0);
            (n, r.rest().to_vec())
        }
        None => (0, Vec::new()),
    };
    let mut out = Vec::with_capacity(rest.len() + 8);
    write_varint(&mut out, count.saturating_add(1));
    out.extend_from_slice(&rest);
    write_varint(
        &mut out,
        i32::try_from(pattern).unwrap_or(0).saturating_add(1),
    );
    write_varint(&mut out, color);
    stack
        .patch_mut()
        .set(components::BANNER_PATTERNS, out.into_boxed_slice());
}

impl ContainerView<'_> {
    fn station_local(&self, i: usize) -> ItemStack {
        self.menu.local.get(i).cloned().unwrap_or_default()
    }

    fn station_set_local(&mut self, i: usize, stack: ItemStack) {
        if let Some(s) = self.menu.local.get_mut(i) {
            *s = stack;
        }
    }

    fn selected(&self) -> i32 {
        self.menu.data.first().copied().unwrap_or(-1)
    }

    fn set_selected(&mut self, value: i32) {
        if let Some(d) = self.menu.data.first_mut() {
            *d = value;
        }
    }

    /// `Slot.mayPlace` of the loom's and cartography table's inputs.
    pub(super) fn station_may_place(kind: MenuKind, i: usize, stack: &ItemStack) -> bool {
        match (kind, i) {
            (MenuKind::Loom, 0) => is_banner_item(stack),
            (MenuKind::Loom, 1) => is_dye_item(stack),
            (MenuKind::Loom, 2) => is_pattern_item(stack),
            (MenuKind::CartographyTable, 0) => stack.component(components::MAP_ID).is_some(),
            (MenuKind::CartographyTable, 1) => {
                let id = item::item_id(stack);
                id == items::PAPER || id == items::MAP || id == items::GLASS_PANE
            }
            _ => false,
        }
    }

    // --- Loom ------------------------------------------------------------------------------

    /// `LoomMenu.getSelectablePatterns`.
    fn selectable_patterns(pattern: &ItemStack) -> Vec<u32> {
        if pattern.is_empty() {
            return game_data()
                .and_then(|d| {
                    d.banner_patterns
                        .tags
                        .get("minecraft:no_item_required")
                        .cloned()
                })
                .unwrap_or_default();
        }
        provided_patterns(pattern).unwrap_or_default()
    }

    /// `LoomMenu.slotsChanged`.
    pub(super) fn loom_changed(&mut self) {
        let banner = self.station_local(0);
        let dye = self.station_local(1);
        let pattern = self.station_local(2);
        if banner.is_empty() || dye.is_empty() {
            self.station_set_local(3, ItemStack::EMPTY);
            self.menu.recipes.clear();
            self.set_selected(-1);
            return;
        }
        let selected = self.selected();
        let valid = usize::try_from(selected).is_ok_and(|i| i < self.menu.recipes.len());
        let previous = std::mem::take(&mut self.menu.recipes);
        let patterns = Self::selectable_patterns(&pattern);
        self.menu.recipes = patterns
            .iter()
            .filter_map(|&p| usize::try_from(p).ok())
            .collect();
        let display = if patterns.len() == 1 {
            self.set_selected(0);
            patterns.first().copied()
        } else if !valid {
            self.set_selected(-1);
            None
        } else {
            let value = usize::try_from(selected)
                .ok()
                .and_then(|i| previous.get(i))
                .copied();
            match value.and_then(|v| self.menu.recipes.iter().position(|&p| p == v)) {
                Some(index) => {
                    self.set_selected(i32::try_from(index).unwrap_or(-1));
                    value.and_then(|v| u32::try_from(v).ok())
                }
                None => {
                    self.set_selected(-1);
                    None
                }
            }
        };
        match display {
            Some(pattern) => {
                if layer_count(banner.component(components::BANNER_PATTERNS)) >= MAX_PATTERNS {
                    self.set_selected(-1);
                    self.station_set_local(3, ItemStack::EMPTY);
                } else {
                    self.loom_setup_result(pattern);
                }
            }
            None => self.station_set_local(3, ItemStack::EMPTY),
        }
    }

    /// `LoomMenu.setupResultSlot`.
    fn loom_setup_result(&mut self, pattern: u32) {
        let banner = self.station_local(0);
        let dye = self.station_local(1);
        let mut result = ItemStack::EMPTY;
        if !banner.is_empty()
            && !dye.is_empty()
            && let Some(color) = dye_color(&dye)
        {
            result = banner.with_count(1);
            add_layer(&mut result, pattern, color);
        }
        if !item::matches(&result, &self.station_local(3)) {
            self.station_set_local(3, result);
        }
    }

    /// `LoomMenu.clickMenuButton`.
    pub(super) fn loom_button(&mut self, id: i32) -> bool {
        let Some(&pattern) = usize::try_from(id)
            .ok()
            .and_then(|i| self.menu.recipes.get(i))
        else {
            return false;
        };
        self.set_selected(id);
        self.loom_setup_result(u32::try_from(pattern).unwrap_or(0));
        true
    }

    /// The loom result slot's `onTake`.
    pub(super) fn loom_take(&mut self) {
        for i in 0..2 {
            let mut s = self.station_local(i);
            s.shrink(1);
            self.station_set_local(i, s);
        }
        if self.station_local(0).is_empty() || self.station_local(1).is_empty() {
            self.set_selected(-1);
        }
        self.loom_changed();
        self.take_sound("ui.loom.take_result");
    }

    fn take_sound(&mut self, sound: &str) {
        let time = self.level.game_time();
        if self.menu.last_sound != time {
            let pos = self.menu.pos;
            let sound = crate::block::sound_id(sound);
            self.level
                .play_block_sound(None, pos, sound, crate::level::SOUND_BLOCKS, 1.0, 1.0);
            self.menu.last_sound = time;
        }
    }

    /// `LoomMenu.quickMoveStack`.
    pub(super) fn loom_quick_move(
        &mut self,
        index: usize,
        ctx: ClickContext,
        drops: &mut Vec<Drop>,
    ) -> ItemStack {
        let original = self.item(index).clone();
        if original.is_empty() {
            return ItemStack::EMPTY;
        }
        let mut stack = original.clone();
        let moved = if index == 3 {
            super::move_to(self, &mut stack, 4, 40, true)
        } else if index > 3 {
            if is_banner_item(&stack) {
                super::move_to(self, &mut stack, 0, 1, false)
            } else if is_dye_item(&stack) {
                super::move_to(self, &mut stack, 1, 2, false)
            } else if is_pattern_item(&stack) {
                super::move_to(self, &mut stack, 2, 3, false)
            } else if index < 31 {
                super::move_to(self, &mut stack, 31, 40, false)
            } else {
                super::move_to(self, &mut stack, 4, 31, false)
            }
        } else {
            super::move_to(self, &mut stack, 4, 40, false)
        };
        if !moved {
            return ItemStack::EMPTY;
        }
        super::finish_quick_move(self, index, stack, &original, ctx, drops)
    }

    // --- Cartography table ---------------------------------------------------------------

    /// `CartographyTableMenu.slotsChanged`.
    pub(super) fn cartography_changed(&mut self) {
        let map = self.station_local(0);
        let additional = self.station_local(1);
        let result = self.station_local(2);
        if !result.is_empty() && (map.is_empty() || additional.is_empty()) {
            self.station_set_local(2, ItemStack::EMPTY);
            return;
        }
        if map.is_empty() || additional.is_empty() {
            return;
        }
        // `setupResultSlot`.
        let Some(info) = crate::map::info(self.level, &map) else {
            return;
        };
        let extra = item::item_id(&additional);
        let extendable = in_tag(&map, "minecraft:extendable_maps");
        let new = if extra == items::PAPER && extendable && !info.locked && info.scale < 4 {
            let mut r = map.with_count(1);
            crate::map::set_post_processing(&mut r, crate::map::PostProcessing::Scale);
            r
        } else if extra == items::GLASS_PANE && !info.locked {
            let mut r = map.with_count(1);
            crate::map::set_post_processing(&mut r, crate::map::PostProcessing::Lock);
            r
        } else if extra == items::MAP {
            map.with_count(2)
        } else {
            self.station_set_local(2, ItemStack::EMPTY);
            return;
        };
        if !item::matches(&new, &result) {
            self.station_set_local(2, new);
        }
    }

    /// The cartography result slot's `onTake`: the inputs shrink, the map is post-processed.
    pub(super) fn cartography_take(&mut self, taken: &mut ItemStack) {
        for i in 0..2 {
            let mut s = self.station_local(i);
            s.shrink(1);
            self.station_set_local(i, s);
        }
        self.cartography_changed();
        crate::map::on_crafted(self.level, taken);
        self.take_sound("ui.cartography_table.take_result");
    }

    /// `CartographyTableMenu.quickMoveStack`.
    pub(super) fn cartography_quick_move(
        &mut self,
        index: usize,
        ctx: ClickContext,
        drops: &mut Vec<Drop>,
    ) -> ItemStack {
        let original = self.item(index).clone();
        if original.is_empty() {
            return ItemStack::EMPTY;
        }
        let mut stack = original.clone();
        let moved = if index == 2 {
            crate::map::on_crafted(self.level, &mut stack);
            super::move_to(self, &mut stack, 3, 39, true)
        } else if index > 2 {
            let id = item::item_id(&stack);
            if stack.component(components::MAP_ID).is_some() {
                super::move_to(self, &mut stack, 0, 1, false)
            } else if id != items::PAPER && id != items::MAP && id != items::GLASS_PANE {
                if index < 30 {
                    super::move_to(self, &mut stack, 30, 39, false)
                } else {
                    super::move_to(self, &mut stack, 3, 30, false)
                }
            } else {
                super::move_to(self, &mut stack, 1, 2, false)
            }
        } else {
            super::move_to(self, &mut stack, 3, 39, false)
        };
        if !moved {
            return ItemStack::EMPTY;
        }
        super::finish_quick_move(self, index, stack, &original, ctx, drops)
    }

    // --- Crafter ---------------------------------------------------------------------------

    /// `CrafterSlot.mayPlace`'s disabled check.
    pub(super) fn crafter_disabled(&self, i: usize) -> bool {
        match self.menu.storage {
            Storage::Block(pos) => match self.level.block_entity(pos) {
                Some(BlockEntity::Crafter(c)) => c.is_disabled(i),
                _ => false,
            },
            _ => false,
        }
    }

    /// `CrafterMenu.refreshRecipeResult` when the grid changed.
    pub(super) fn crafter_changed(&mut self) {
        let Storage::Block(pos) = self.menu.storage else {
            return;
        };
        let Some(BlockEntity::Crafter(c)) = self.level.block_entity(pos) else {
            return;
        };
        if self.menu.crafter_grid.as_slice() == &c.items[..] {
            return;
        }
        self.menu.crafter_grid = c.items.to_vec();
        let input = c.craft_input();
        let result = game_data()
            .and_then(|d| {
                d.recipes
                    .crafting_for_in(&input, &|m| {
                        crate::map::info(self.level, m).map(|i| i.scale)
                    })
                    .map(|r| d.recipes.assemble(r, &input))
            })
            .unwrap_or_default();
        if let Some(slot) = self.menu.local.first_mut() {
            *slot = result;
        }
    }

    /// `handleContainerSlotStateChanged`: a crafter slot is enabled or disabled.
    pub fn crafter_slot_state(&mut self, slot: i32, enabled: bool) {
        let (MenuKind::Crafter, Storage::Block(pos)) = (self.menu.kind, self.menu.storage) else {
            return;
        };
        let Ok(slot) = usize::try_from(slot) else {
            return;
        };
        let changed = match self.level.block_entity_mut(pos) {
            Some(BlockEntity::Crafter(c)) => c.set_slot_state(slot, enabled),
            _ => false,
        };
        if changed {
            crate::block::container::set_changed(self.level, pos);
            self.level.block_entities.touch(pos.chunk());
        }
    }

    // --- Lectern ---------------------------------------------------------------------------

    /// `LecternMenu.clickMenuButton`.
    pub(super) fn lectern_button(&mut self, id: i32, drops: &mut Vec<Drop>) -> bool {
        let Storage::Block(pos) = self.menu.storage else {
            return false;
        };
        let page = |view: &Self| match view.level.block_entity(pos) {
            Some(BlockEntity::Lectern(l)) => l.page,
            _ => 0,
        };
        match id {
            100.. => {
                crate::block::lectern::set_page(self.level, pos, id - 100);
                true
            }
            1 => {
                let p = page(self);
                crate::block::lectern::set_page(self.level, pos, p - 1);
                true
            }
            2 => {
                let p = page(self);
                crate::block::lectern::set_page(self.level, pos, p + 1);
                true
            }
            3 => {
                if !self.menu.may_build {
                    return false;
                }
                let mut book = crate::block::lectern::remove_book(self.level, pos);
                crate::block::container::set_changed(self.level, pos);
                if !self.inv.add(None, &mut book, false) {
                    drops.push(Drop {
                        stack: book,
                        thrown: false,
                    });
                }
                true
            }
            _ => false,
        }
    }
}

//! Utility items: Java item cool-downs as Bedrock cool-down categories, Java maps as Bedrock
//! map textures and decorations, and Bedrock book editing as Java `edit_book`.
//!
//! The map decoration table follows GeyserMC's `mappings/map_decorations.json` (MIT,
//! Copyright (c) 2019-2026 GeyserMC).

use std::time::{Duration, Instant};

use vanillite_protocol::packets::play::clientbound as pcb;
use vanillite_protocol::packets::play::serverbound as psb;

use super::Session;
use super::inventory::MENU;
use crate::packets::{self as bp, BookAction, MapDecoration, MapUpdate, id};

/// Maps kept per session; Java sends the data of the maps the player carries.
const MAX_MAPS: usize = 256;
/// Most pages of a Java book (`WritableBookContent.MAX_PAGES`).
const MAX_PAGES: usize = 100;
/// Most characters of a Java page and title (`edit_book` limits).
const PAGE_CHARS: usize = 1024;
const TITLE_CHARS: usize = 32;
/// Book edits are sent to Java at most this often (the last one wins); signing is immediate.
const BOOK_SEND_DELAY: Duration = Duration::from_millis(500);

/// `MapColor` base colours by id (`MapColor.java`).
const MAP_COLORS: [u32; 62] = [
    0x000000, 0x7FB238, 0xF7E9A3, 0xC7C7C7, 0xFF0000, 0xA0A0FF, 0xA7A7A7, 0x007C00, 0xFFFFFF,
    0xA4A8B8, 0x976D4D, 0x707070, 0x4040FF, 0x8F7748, 0xFFFCF5, 0xD87F33, 0xB24CD8, 0x6699D8,
    0xE5E533, 0x7FCC19, 0xF27FA5, 0x4C4C4C, 0x999999, 0x4C7F99, 0x7F3FB2, 0x334CB2, 0x664C33,
    0x667F33, 0x993333, 0x191919, 0xFAEE4D, 0x5CDBD5, 0x4A80FF, 0x00D93A, 0x815631, 0x700200,
    0xD1B1A1, 0x9F5224, 0x95576C, 0x706C8A, 0xBA8524, 0x677535, 0xA04D4E, 0x392923, 0x876B62,
    0x575C5C, 0x7A4958, 0x4C3E5C, 0x4C3223, 0x4C522A, 0x8E3C2E, 0x251610, 0xBD3031, 0x943F61,
    0x5C191D, 0x167E86, 0x3A8E8C, 0x562C3E, 0x14B485, 0x646464, 0xD8AF93, 0x7FA796,
];

/// `MapColor.Brightness` modifiers: LOW, NORMAL, HIGH, LOWEST.
const BRIGHTNESS: [u32; 4] = [180, 220, 255, 135];

/// A packed Java map colour (`id << 2 | brightness`) as Bedrock's ARGB pixel; id 0 is
/// transparent.
pub(super) fn map_pixel(packed: u8) -> u32 {
    let base = usize::from(packed >> 2);
    let Some(&rgb) = MAP_COLORS.get(base).filter(|_| base != 0) else {
        return 0;
    };
    let m = BRIGHTNESS
        .get(usize::from(packed & 3))
        .copied()
        .unwrap_or(255);
    let channel = |shift: u32| (rgb >> shift & 255).saturating_mul(m) / 255;
    0xFF00_0000 | channel(16) << 16 | channel(8) << 8 | channel(0)
}

/// The Bedrock image and ARGB colour of a Java map decoration type.
fn decoration_image(java: i32) -> Option<(u8, u32)> {
    let name = u16::try_from(java)
        .ok()
        .and_then(|i| vanillite_data::registry::MAP_DECORATION_TYPE.name(i))?;
    let name = name.strip_prefix("minecraft:").unwrap_or(name);
    if let Some(color) = name.strip_prefix("banner_") {
        let argb: u32 = match color {
            "orange" => 0xFFF9801D,
            "magenta" => 0xFFC74EBD,
            "light_blue" => 0xFF3AB3DA,
            "yellow" => 0xFFFED83D,
            "lime" => 0xFF80C71F,
            "pink" => 0xFFF38BAA,
            "gray" => 0xFF474F52,
            "light_gray" => 0xFF9D9D97,
            "cyan" => 0xFF169C9C,
            "purple" => 0xFF8932B8,
            "blue" => 0xFF3C44AA,
            "brown" => 0xFF835432,
            "green" => 0xFF5E7C16,
            "red" => 0xFFB02E26,
            "black" => 0xFF1D1D21,
            _ => 0xFFFFFFFF,
        };
        return Some((13, argb));
    }
    let image = match name {
        "player" => 0,
        "red_marker" => 2,
        "blue_marker" => 3,
        "red_x" => 4,
        "target_x" => return Some((4, 0xFF00_0000)),
        "target_point" => 5,
        "player_off_map" => 6,
        "frame" => 1,
        "player_off_limits" => 13,
        "mansion" => 14,
        "monument" => 15,
        "village_desert" => 17,
        "village_plains" => 18,
        "village_savanna" => 19,
        "village_snowy" => 20,
        "village_taiga" => 21,
        "jungle_temple" => 22,
        "swamp_hut" => 23,
        "trial_chambers" => 24,
        "abandoned_camp" => 25,
        "ancient_city" => 26,
        "mineshaft" => 27,
        "desert_pyramid" => 28,
        "ocean_ruin_warm" => 29,
        _ => return None,
    };
    Some((image, 0xFFFF_FFFF))
}

/// The last known state of one Java map.
#[derive(Debug, Clone)]
pub(super) struct MapCache {
    /// 128 × 128 packed Java colours, row by row.
    colors: Vec<u8>,
    scale: u8,
    locked: bool,
    decorations: Vec<MapDecoration>,
}

impl Default for MapCache {
    fn default() -> Self {
        Self {
            colors: vec![0; 128 * 128],
            scale: 0,
            locked: false,
            decorations: Vec::new(),
        }
    }
}

/// The book the Bedrock client edits: its hotbar slot and pages, and the edit to send.
#[derive(Debug, Clone, Default)]
pub(super) struct BookEdit {
    slot: Option<u8>,
    pages: Vec<String>,
    pending: Option<(psb::EditBook, Instant)>,
}

/// Applies one Bedrock book action to the pages (`BedrockBookEditTranslator`'s rules); trailing
/// empty pages are dropped. `false` for an out-of-range page.
pub(super) fn apply_book_action(pages: &mut Vec<String>, action: &BookAction) -> bool {
    let index = |p: i32| usize::try_from(p).ok().filter(|&p| p < MAX_PAGES);
    let clip = |t: &str| -> String {
        t.chars()
            .filter(|c| !c.is_control() || *c == '\n')
            .take(PAGE_CHARS)
            .collect()
    };
    match action {
        BookAction::ReplacePage { page, text } | BookAction::AddPage { page, text } => {
            let Some(p) = index(*page) else {
                return false;
            };
            let replace = matches!(action, BookAction::ReplacePage { .. });
            if replace && let Some(slot) = pages.get_mut(p) {
                *slot = clip(text);
            } else {
                while pages.len() < p {
                    pages.push(String::new());
                }
                pages.insert(p, clip(text));
                pages.truncate(MAX_PAGES);
            }
        }
        BookAction::DeletePage { page } => {
            let Some(p) = index(*page) else {
                return false;
            };
            if p < pages.len() {
                pages.remove(p);
            }
        }
        BookAction::SwapPages { a, b } => {
            let (Some(a), Some(b)) = (index(*a), index(*b)) else {
                return false;
            };
            if a < pages.len() && b < pages.len() {
                pages.swap(a, b);
            }
        }
        BookAction::Sign { .. } => {}
    }
    while pages.last().is_some_and(String::is_empty) {
        pages.pop();
    }
    true
}

impl Session {
    /// `cooldown`: Bedrock knows the categories by the item name.
    pub(super) fn java_cooldown(&mut self, c: &pcb::Cooldown) {
        let category = c.group.strip_prefix("minecraft:").unwrap_or(&c.group);
        self.send(
            id::PLAYER_START_ITEM_COOLDOWN,
            &bp::start_item_cooldown(category, c.duration.max(0)),
        );
    }

    /// `map_item_data`: the colours go into the session's copy of the map; the changed
    /// rectangle and the decorations go to the client once it is in the world.
    pub(super) fn java_map_data(&mut self, m: pcb::MapItemData) {
        if !self.maps.contains_key(&m.map_id) && self.maps.len() >= MAX_MAPS {
            self.maps.clear();
        }
        let map = self.maps.entry(m.map_id).or_default();
        map.scale = u8::try_from(m.scale.clamp(0, 4)).unwrap_or(0);
        map.locked = m.locked;
        if let Some(list) = &m.decorations {
            map.decorations = list
                .iter()
                .filter_map(|d| {
                    let (image, color) = decoration_image(d.kind)?;
                    Some(MapDecoration {
                        image,
                        rotation: u8::try_from(d.rot & 15).unwrap_or(0),
                        x: d.x.to_ne_bytes()[0],
                        y: d.y.to_ne_bytes()[0],
                        label: d.name.as_ref().map(|n| n.plain()).unwrap_or_default(),
                        color,
                    })
                })
                .collect();
        }
        let mut texture = None;
        if let Some(p) = &m.patch {
            let (x0, y0) = (usize::from(p.start_x), usize::from(p.start_y));
            let (w, h) = (usize::from(p.width), usize::from(p.height));
            if w > 0
                && x0.saturating_add(w) <= 128
                && y0.saturating_add(h) <= 128
                && p.colors.len() == w.saturating_mul(h)
            {
                let rows = map.colors.as_chunks_mut::<128>().0.iter_mut().skip(y0);
                for (dst, src) in rows.zip(p.colors.chunks_exact(w)) {
                    if let Some(dst) = dst.get_mut(x0..x0.saturating_add(w)) {
                        dst.copy_from_slice(src);
                    }
                }
                texture = Some((p.start_x, p.start_y, p.width, p.height));
            }
        }
        let decorations = m.decorations.is_some();
        self.send_map(m.map_id, decorations, texture);
    }

    /// Sends a map's decorations and/or a rectangle of its texture.
    fn send_map(&mut self, map_id: i32, decorations: bool, texture: Option<(u8, u8, u8, u8)>) {
        let Some(dimension) = self
            .world
            .as_ref()
            .filter(|w| w.spawned)
            .and_then(|w| u8::try_from(w.layout.dimension).ok())
        else {
            return;
        };
        let Some(map) = self.maps.get(&map_id) else {
            return;
        };
        let pixels: Vec<u32> = match texture {
            Some((x, y, w, h)) => {
                let (x, y, w, h) = (
                    usize::from(x),
                    usize::from(y),
                    usize::from(w),
                    usize::from(h),
                );
                let mut out = Vec::with_capacity(w.saturating_mul(h));
                for row in map.colors.as_chunks::<128>().0.iter().skip(y).take(h) {
                    let line = row.get(x..x.saturating_add(w)).unwrap_or_default();
                    out.extend(line.iter().map(|&c| map_pixel(c)));
                }
                out
            }
            None => Vec::new(),
        };
        let update = MapUpdate {
            dimension,
            locked: map.locked,
            scale: map.scale,
            decorations: decorations.then_some(map.decorations.as_slice()),
            texture: texture.map(|(x, y, w, h)| (x, y, w, h, pixels.as_slice())),
        };
        let body = bp::map_item_data(i64::from(map_id), &update);
        self.send(id::CLIENTBOUND_MAP_ITEM_DATA, &body);
    }

    /// `map_info_request`: the client shows a map it has no data for; the whole map from the
    /// session's copy.
    pub(super) fn map_info_request(&mut self, body: &[u8]) -> crate::io::Result<()> {
        let map_id = bp::read_map_info_request(body)?;
        if let Ok(map_id) = i32::try_from(map_id) {
            self.send_map(map_id, true, Some((0, 0, 128, 128)));
        }
        Ok(())
    }

    /// `book_edit`: the pages of the writable book in a hotbar slot change; the Java server
    /// gets the whole book (`edit_book`), signed with the title on `Sign`.
    pub(super) fn book_edit(&mut self, body: &[u8], now: Instant) -> crate::io::Result<()> {
        let (slot, action) = bp::read_book_edit(body)?;
        let Some(slot) = u8::try_from(slot).ok().filter(|&s| s < 9) else {
            return Ok(());
        };
        let menu = usize::from(slot).saturating_add(36);
        let Some(stack) = self.inv.slots.get(menu).filter(|_| menu < MENU) else {
            return Ok(());
        };
        let writable = vanillite_data::item::ids::WRITABLE_BOOK.0;
        if stack.stack.item != writable {
            return Ok(());
        }
        if self.book.slot != Some(slot) {
            self.book.slot = Some(slot);
            self.book.pages = crate::items::writable_pages(&stack.stack);
        }
        if !apply_book_action(&mut self.book.pages, &action) {
            return Ok(());
        }
        let title = match &action {
            BookAction::Sign { title } => {
                let title: String = title
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(TITLE_CHARS)
                    .collect();
                Some(title)
            }
            _ => None,
        };
        let signed = title.is_some();
        let edit = psb::EditBook {
            slot: i32::from(slot),
            pages: self.book.pages.clone(),
            title,
        };
        if signed {
            self.book = BookEdit::default();
            self.java_send(&edit);
        } else {
            let since = self.book.pending.as_ref().map_or(now, |(_, t)| *t);
            self.book.pending = Some((edit, since));
        }
        Ok(())
    }

    /// Sends the pending book edit once it is old enough.
    pub(super) fn flush_book(&mut self, now: Instant) {
        let due = self
            .book
            .pending
            .as_ref()
            .is_some_and(|(_, t)| now.saturating_duration_since(*t) >= BOOK_SEND_DELAY);
        if due && let Some((edit, _)) = self.book.pending.take() {
            self.java_send(&edit);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_colors_follow_vanilla_brightness() {
        // GRASS (1) at LOW, NORMAL, HIGH; id 0 is transparent.
        assert_eq!(map_pixel(4), 0xFF00_0000 | 89 << 16 | 125 << 8 | 39);
        assert_eq!(map_pixel(6), 0xFF00_0000 | 127 << 16 | 178 << 8 | 56);
        assert_eq!(map_pixel(0), 0);
        assert_eq!(map_pixel(3), 0);
        assert_eq!(decoration_image(-1), None);
    }

    #[test]
    fn book_actions_edit_pages_like_geyser() {
        let mut pages = Vec::new();
        assert!(apply_book_action(
            &mut pages,
            &BookAction::ReplacePage {
                page: 2,
                text: "c".into()
            }
        ));
        assert_eq!(pages, ["", "", "c"]);
        apply_book_action(&mut pages, &BookAction::SwapPages { a: 0, b: 2 });
        assert_eq!(pages, ["c"]);
        apply_book_action(
            &mut pages,
            &BookAction::AddPage {
                page: 0,
                text: "a".into(),
            },
        );
        assert_eq!(pages, ["a", "c"]);
        apply_book_action(&mut pages, &BookAction::DeletePage { page: 0 });
        assert_eq!(pages, ["c"]);
        assert!(!apply_book_action(
            &mut pages,
            &BookAction::DeletePage { page: 100 }
        ));
    }
}

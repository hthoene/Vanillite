//! Item cool-downs of a player (`ItemCooldowns`, `ServerItemCooldowns`): a cool-down group (the
//! `use_cooldown` group or the item id) cannot be used until its end tick; the client is told
//! when a cool-down starts and ends.

use vanillite_protocol::codec::Reader;
use vanillite_protocol::components::ids;
use vanillite_protocol::packets::play::clientbound::Cooldown;

use super::sink::PacketSink;
use crate::item::{self, ItemStack};

/// `ItemCooldowns`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ItemCooldowns {
    /// Group, start and end tick, in insertion order.
    entries: Vec<(Box<str>, i32, i32)>,
    tick_count: i32,
}

/// The `use_cooldown` component: seconds and the optional group.
fn use_cooldown(stack: &ItemStack) -> Option<(f32, Option<String>)> {
    let value = stack.component(ids::USE_COOLDOWN)?;
    let mut r = Reader::new(value);
    let seconds = r.f32().ok()?;
    let group = match r.u8().ok()? {
        0 => None,
        _ => Some(r.string(32767).ok()?.to_owned()),
    };
    Some((seconds, group))
}

/// `getCooldownGroup`: the `use_cooldown` group, else the item id.
pub fn group(stack: &ItemStack) -> String {
    use_cooldown(stack)
        .and_then(|(_, g)| g)
        .unwrap_or_else(|| item::item_id(stack).name().to_owned())
}

/// `UseCooldown.ticks()` of a stack with the `use_cooldown` component.
pub fn use_cooldown_ticks(stack: &ItemStack) -> Option<i32> {
    #[allow(clippy::as_conversions, clippy::cast_possible_truncation)]
    use_cooldown(stack).map(|(seconds, _)| (seconds * 20.0) as i32)
}

impl ItemCooldowns {
    /// `isOnCooldown(item)`.
    pub fn is_on_cooldown(&self, stack: &ItemStack) -> bool {
        let group = group(stack);
        self.entries
            .iter()
            .find(|(g, _, _)| **g == *group)
            .is_some_and(|&(_, start, end)| end > start && end > self.tick_count)
    }

    /// `addCooldown(item, time)`.
    pub fn add(&mut self, stack: &ItemStack, ticks: i32, sink: &dyn PacketSink) {
        self.add_group(&group(stack), ticks, sink);
    }

    /// `addCooldown(group, time)`.
    pub fn add_group(&mut self, group: &str, ticks: i32, sink: &dyn PacketSink) {
        let end = self.tick_count.saturating_add(ticks);
        match self.entries.iter_mut().find(|(g, _, _)| **g == *group) {
            Some(e) => {
                e.1 = self.tick_count;
                e.2 = end;
            }
            None => self.entries.push((Box::from(group), self.tick_count, end)),
        }
        sink.send(&Cooldown {
            group: group.to_owned(),
            duration: ticks,
        });
    }

    /// `UseCooldown.apply`: the cool-down of a used stack's `use_cooldown` component.
    pub fn apply_use_cooldown(&mut self, used: &ItemStack, sink: &dyn PacketSink) {
        if let Some(ticks) = use_cooldown_ticks(used)
            && ticks > 0
        {
            self.add(used, ticks, sink);
        }
    }

    /// `tick()`: ended cool-downs are removed and the client is told.
    pub fn tick(&mut self, sink: &dyn PacketSink) {
        self.tick_count = self.tick_count.wrapping_add(1);
        if self.entries.is_empty() {
            return;
        }
        let now = self.tick_count;
        self.entries.retain(|(group, _, end)| {
            let keep = *end > now;
            if !keep {
                sink.send(&Cooldown {
                    group: group.to_string(),
                    duration: 0,
                });
            }
            keep
        });
    }
}

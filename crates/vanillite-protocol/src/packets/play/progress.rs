//! Play packets of statistics and advancements: `award_stats`, `update_advancements`,
//! `select_advancements_tab` and `seen_advancements`.

use crate::Packet;
use crate::codec::{
    Decode, DecodeLimited, Encode, Error, Reader, Result, decode_list_limited, write_len,
    write_varint,
};
use crate::text::Component;
use crate::types::Identifier;

/// Component quota of advancement titles and descriptions (`TRUSTED_STREAM_CODEC`).
const COMPONENT_MAX: usize = 2_097_152;
/// Entries of the lists and maps of these packets.
const MAX_ENTRIES: usize = 1 << 20;
/// Characters of a criterion name.
const NAME_MAX: usize = 32767;

/// One statistic of `award_stats` (`Stat.STREAM_CODEC`): the `minecraft:stat_type` id, the id
/// in that type's registry, and the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatValue {
    pub stat_type: i32,
    pub value_id: i32,
    pub count: i32,
}

impl Encode for StatValue {
    fn encode(&self, out: &mut Vec<u8>) {
        write_varint(out, self.stat_type);
        write_varint(out, self.value_id);
        write_varint(out, self.count);
    }
}

impl Decode for StatValue {
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        Ok(Self {
            stat_type: r.varint()?,
            value_id: r.varint()?,
            count: r.varint()?,
        })
    }
}

/// `award_stats` (`ClientboundAwardStatsPacket`): the statistics that changed.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Play, dir = Clientbound, name = "award_stats")]
pub struct AwardStats {
    #[field(max_len = MAX_ENTRIES)]
    pub stats: Vec<StatValue>,
}

/// `DisplayInfo` on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvancementDisplay {
    pub title: Component,
    pub description: Component,
    /// The icon (`ItemStackTemplate`): item id and count, without component changes.
    pub icon_item: i32,
    pub icon_count: i32,
    /// `AdvancementType` id.
    pub frame: i32,
    pub background: Option<Identifier>,
    pub show_toast: bool,
    pub hidden: bool,
}

impl Encode for AdvancementDisplay {
    fn encode(&self, out: &mut Vec<u8>) {
        self.title.encode(out);
        self.description.encode(out);
        write_varint(out, self.icon_item);
        write_varint(out, self.icon_count);
        // An empty `DataComponentPatch`.
        out.extend_from_slice(&[0, 0]);
        write_varint(out, self.frame);
        let flags = i32::from(self.background.is_some())
            | (i32::from(self.show_toast) << 1)
            | (i32::from(self.hidden) << 2);
        flags.encode(out);
        if let Some(background) = &self.background {
            background.encode(out);
        }
    }
}

impl Decode for AdvancementDisplay {
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        let title = Component::decode_limited(r, COMPONENT_MAX)?;
        let description = Component::decode_limited(r, COMPONENT_MAX)?;
        let icon_item = r.varint()?;
        let icon_count = r.varint()?;
        if r.varint()? != 0 || r.varint()? != 0 {
            return Err(Error::Invalid("advancement icon components"));
        }
        let frame = r.varint()?;
        let flags = r.i32()?;
        let background = if flags & 1 != 0 {
            Some(Identifier::decode(r)?)
        } else {
            None
        };
        Ok(Self {
            title,
            description,
            icon_item,
            icon_count,
            frame,
            background,
            show_toast: flags & 2 != 0,
            hidden: flags & 4 != 0,
        })
    }
}

/// `ClientboundUpdateAdvancementsPacket.PositionedAdvancement`: an `AdvancementHolder` with its
/// position in the tab.
#[derive(Debug, Clone, PartialEq)]
pub struct AdvancementEntry {
    pub id: Identifier,
    pub parent: Option<Identifier>,
    pub display: Option<AdvancementDisplay>,
    /// `AdvancementRequirements`: all of the lists, any of each list.
    pub requirements: Vec<Vec<String>>,
    pub sends_telemetry_event: bool,
    pub x: f32,
    pub y: f32,
}

impl Encode for AdvancementEntry {
    fn encode(&self, out: &mut Vec<u8>) {
        self.id.encode(out);
        self.parent.encode(out);
        self.display.encode(out);
        write_len(out, self.requirements.len());
        for list in &self.requirements {
            list.encode(out);
        }
        self.sends_telemetry_event.encode(out);
        self.x.encode(out);
        self.y.encode(out);
    }
}

impl Decode for AdvancementEntry {
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        let id = Identifier::decode(r)?;
        let parent = Option::<Identifier>::decode(r)?;
        let display = Option::<AdvancementDisplay>::decode(r)?;
        let count = r.len_prefix("requirements", MAX_ENTRIES, 1)?;
        let mut requirements = Vec::with_capacity(count);
        for _ in 0..count {
            requirements.push(decode_list_limited(r, MAX_ENTRIES, NAME_MAX)?);
        }
        Ok(Self {
            id,
            parent,
            display,
            requirements,
            sends_telemetry_event: bool::decode(r)?,
            x: r.f32()?,
            y: r.f32()?,
        })
    }
}

/// The progress of one advancement: every criterion with the epoch milliseconds it was
/// obtained at (`AdvancementProgress.STREAM_CODEC`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvancementProgress {
    pub id: Identifier,
    pub criteria: Vec<(String, Option<i64>)>,
}

impl Encode for AdvancementProgress {
    fn encode(&self, out: &mut Vec<u8>) {
        self.id.encode(out);
        write_len(out, self.criteria.len());
        for (name, obtained) in &self.criteria {
            name.encode(out);
            obtained.encode(out);
        }
    }
}

impl Decode for AdvancementProgress {
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        let id = Identifier::decode(r)?;
        let count = r.len_prefix("criteria", MAX_ENTRIES, 2)?;
        let mut criteria = Vec::with_capacity(count);
        for _ in 0..count {
            criteria.push((
                String::decode_limited(r, NAME_MAX)?,
                Option::<i64>::decode(r)?,
            ));
        }
        Ok(Self { id, criteria })
    }
}

/// `update_advancements` (`ClientboundUpdateAdvancementsPacket`).
#[derive(Debug, Clone, PartialEq, Packet)]
#[packet(state = Play, dir = Clientbound, name = "update_advancements")]
pub struct UpdateAdvancements {
    pub reset: bool,
    #[field(max_len = MAX_ENTRIES)]
    pub added: Vec<AdvancementEntry>,
    #[field(max_len = MAX_ENTRIES)]
    pub removed: Vec<Identifier>,
    #[field(max_len = MAX_ENTRIES)]
    pub progress: Vec<AdvancementProgress>,
    pub show_advancements: bool,
}

/// `select_advancements_tab`: the tab to open, or none.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Play, dir = Clientbound, name = "select_advancements_tab")]
pub struct SelectAdvancementsTab {
    pub tab: Option<Identifier>,
}

crate::varint_enum! {
    /// `ServerboundSeenAdvancementsPacket.Action`.
    pub enum SeenAdvancementsAction {
        OpenedTab,
        ClosedScreen,
    }
}

/// `seen_advancements`: the client opened a tab or closed the advancements screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeenAdvancements {
    pub action: SeenAdvancementsAction,
    /// The tab of `OpenedTab`.
    pub tab: Option<Identifier>,
}

impl Encode for SeenAdvancements {
    fn encode(&self, out: &mut Vec<u8>) {
        self.action.encode(out);
        if self.action == SeenAdvancementsAction::OpenedTab
            && let Some(tab) = &self.tab
        {
            tab.encode(out);
        }
    }
}

impl Decode for SeenAdvancements {
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        let action = SeenAdvancementsAction::decode(r)?;
        let tab = match action {
            SeenAdvancementsAction::OpenedTab => Some(Identifier::decode(r)?),
            SeenAdvancementsAction::ClosedScreen => None,
        };
        Ok(Self { action, tab })
    }
}

/// `seen_advancements` as a packet.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Play, dir = Serverbound, name = "seen_advancements")]
pub struct SeenAdvancementsPacket(pub SeenAdvancements);

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    fn round_trip<T: Encode + Decode + PartialEq + std::fmt::Debug>(v: &T) -> Vec<u8> {
        let mut out = Vec::new();
        v.encode(&mut out);
        let mut r = Reader::new(&out);
        assert_eq!(&T::decode(&mut r).expect("decode"), v);
        assert_eq!(r.remaining(), 0);
        out
    }

    #[test]
    fn update_advancements_round_trip() {
        round_trip(&UpdateAdvancements {
            reset: true,
            added: vec![AdvancementEntry {
                id: Identifier::vanilla("story/root"),
                parent: None,
                display: Some(AdvancementDisplay {
                    title: Component::translatable("advancements.story.root.title", vec![]),
                    description: Component::text("d"),
                    icon_item: 27,
                    icon_count: 1,
                    frame: 0,
                    background: Some(Identifier::vanilla("gui/advancements/backgrounds/stone")),
                    show_toast: false,
                    hidden: false,
                }),
                requirements: vec![vec!["crafting_table".to_owned()]],
                sends_telemetry_event: true,
                x: 0.0,
                y: 1.5,
            }],
            removed: vec![Identifier::vanilla("a")],
            progress: vec![AdvancementProgress {
                id: Identifier::vanilla("story/root"),
                criteria: vec![("crafting_table".to_owned(), Some(1_700_000_000_000))],
            }],
            show_advancements: false,
        });
    }

    #[test]
    fn seen_advancements_round_trip() {
        let closed = round_trip(&SeenAdvancementsPacket(SeenAdvancements {
            action: SeenAdvancementsAction::ClosedScreen,
            tab: None,
        }));
        assert_eq!(closed, vec![1]);
        round_trip(&SeenAdvancementsPacket(SeenAdvancements {
            action: SeenAdvancementsAction::OpenedTab,
            tab: Some(Identifier::vanilla("story/root")),
        }));
        round_trip(&AwardStats {
            stats: vec![StatValue {
                stat_type: 8,
                value_id: 1,
                count: 20,
            }],
        });
    }
}

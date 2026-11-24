//! Clientbound play packets of the chat and admin commands: teams, titles, dialogs and the
//! camera.

use crate::Packet;
use crate::codec::{Decode, DecodeLimited, Encode, Error, Reader, Result, write_varint};
use crate::text::Component;
use crate::types::Nbt;

/// Component quota of team and title components (`ComponentSerialization.TRUSTED_STREAM_CODEC`
/// on the client side reads with the default NBT quota).
const COMPONENT_MAX: usize = 2_097_152;
/// Members per team packet.
const MAX_MEMBERS: usize = 1 << 16;

/// `ClientboundSetPlayerTeamPacket.Parameters`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamParameters {
    pub display_name: Component,
    pub prefix: Component,
    pub suffix: Component,
    /// `Team.Visibility` id.
    pub nametag_visibility: i32,
    /// `Team.CollisionRule` id.
    pub collision_rule: i32,
    /// `TeamColor` id, `None` without a colour.
    pub color: Option<i32>,
    /// Bit 0 friendly fire, bit 1 see friendly invisibles.
    pub options: u8,
}

impl Encode for TeamParameters {
    fn encode(&self, out: &mut Vec<u8>) {
        self.display_name.encode(out);
        self.prefix.encode(out);
        self.suffix.encode(out);
        write_varint(out, self.nametag_visibility);
        write_varint(out, self.collision_rule);
        match self.color {
            Some(c) => {
                out.push(1);
                write_varint(out, c);
            }
            None => out.push(0),
        }
        out.push(self.options);
    }
}

impl Decode for TeamParameters {
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        Ok(Self {
            display_name: Component::decode_limited(r, COMPONENT_MAX)?,
            prefix: Component::decode_limited(r, COMPONENT_MAX)?,
            suffix: Component::decode_limited(r, COMPONENT_MAX)?,
            nametag_visibility: r.varint()?,
            collision_rule: r.varint()?,
            color: if bool::decode(r)? {
                Some(r.varint()?)
            } else {
                None
            },
            options: r.u8()?,
        })
    }
}

/// The method of `set_player_team` with its data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TeamAction {
    /// 0: a new team with its members.
    Add(Box<TeamParameters>, Vec<String>),
    /// 1: the team is gone.
    Remove,
    /// 2: the team's options changed.
    Change(Box<TeamParameters>),
    /// 3: members joined.
    Join(Vec<String>),
    /// 4: members left.
    Leave(Vec<String>),
}

fn write_members(out: &mut Vec<u8>, members: &[String]) {
    members.encode(out);
}

fn read_members(r: &mut Reader<'_>) -> Result<Vec<String>> {
    crate::codec::decode_list_limited(r, MAX_MEMBERS, 32767)
}

impl Encode for TeamAction {
    fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Self::Add(p, members) => {
                out.push(0);
                p.encode(out);
                write_members(out, members);
            }
            Self::Remove => out.push(1),
            Self::Change(p) => {
                out.push(2);
                p.encode(out);
            }
            Self::Join(members) => {
                out.push(3);
                write_members(out, members);
            }
            Self::Leave(members) => {
                out.push(4);
                write_members(out, members);
            }
        }
    }
}

impl Decode for TeamAction {
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        Ok(match r.i8()? {
            0 => {
                let p = TeamParameters::decode(r)?;
                Self::Add(Box::new(p), read_members(r)?)
            }
            1 => Self::Remove,
            2 => Self::Change(Box::new(TeamParameters::decode(r)?)),
            3 => Self::Join(read_members(r)?),
            4 => Self::Leave(read_members(r)?),
            _ => return Err(Error::Invalid("team method")),
        })
    }
}

/// `set_player_team`.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Play, dir = Clientbound, name = "set_player_team")]
pub struct SetPlayerTeam {
    #[field(max_len = 32767)]
    pub name: String,
    pub action: TeamAction,
}

/// `set_title_text`.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Play, dir = Clientbound, name = "set_title_text")]
pub struct SetTitleText {
    #[field(max_len = 2_097_152)]
    pub text: Component,
}

/// `set_subtitle_text`.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Play, dir = Clientbound, name = "set_subtitle_text")]
pub struct SetSubtitleText {
    #[field(max_len = 2_097_152)]
    pub text: Component,
}

/// `set_action_bar_text`.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Play, dir = Clientbound, name = "set_action_bar_text")]
pub struct SetActionBarText {
    #[field(max_len = 2_097_152)]
    pub text: Component,
}

/// `set_titles_animation`: ticks.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Play, dir = Clientbound, name = "set_titles_animation")]
pub struct SetTitlesAnimation {
    pub fade_in: i32,
    pub stay: i32,
    pub fade_out: i32,
}

/// `clear_titles`: `reset` also resets the times.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Play, dir = Clientbound, name = "clear_titles")]
pub struct ClearTitles {
    pub reset: bool,
}

/// `set_camera`.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Play, dir = Clientbound, name = "set_camera")]
pub struct SetCamera {
    #[field(varint)]
    pub camera_id: i32,
}

/// A dialog (`Holder<Dialog>`): a registry entry by network id or an inline definition.
#[derive(Debug, Clone, PartialEq)]
pub enum DialogRef {
    /// A registered dialog, sent as `id + 1`.
    Registered(i32),
    /// An inline dialog as NBT, sent after a 0.
    Direct(Nbt),
}

impl Encode for DialogRef {
    fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Self::Registered(id) => write_varint(out, id.saturating_add(1)),
            Self::Direct(nbt) => {
                out.push(0);
                nbt.encode(out);
            }
        }
    }
}

impl Decode for DialogRef {
    fn decode(r: &mut Reader<'_>) -> Result<Self> {
        Ok(match r.varint()? {
            0 => Self::Direct(Nbt::decode_limited(r, COMPONENT_MAX)?),
            id => Self::Registered(id.saturating_sub(1)),
        })
    }
}

/// `show_dialog` (play).
#[derive(Debug, Clone, PartialEq, Packet)]
#[packet(state = Play, dir = Clientbound, name = "show_dialog")]
pub struct ShowDialog {
    pub dialog: DialogRef,
}

/// `clear_dialog` (play).
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(state = Play, dir = Clientbound, name = "clear_dialog")]
pub struct ClearDialog;

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    fn round_trip<T: Encode + Decode + PartialEq + std::fmt::Debug>(v: &T) {
        let mut out = Vec::new();
        v.encode(&mut out);
        let mut r = Reader::new(&out);
        assert_eq!(&T::decode(&mut r).expect("decode"), v);
        assert_eq!(r.remaining(), 0);
    }

    #[test]
    fn team_actions_round_trip() {
        let p = TeamParameters {
            display_name: Component::text("Red"),
            prefix: Component::text("["),
            suffix: Component::empty(),
            nametag_visibility: 2,
            collision_rule: 1,
            color: Some(12),
            options: 3,
        };
        round_trip(&TeamAction::Add(Box::new(p.clone()), vec!["Alice".into()]));
        round_trip(&TeamAction::Change(Box::new(TeamParameters {
            color: None,
            ..p
        })));
        round_trip(&TeamAction::Remove);
        round_trip(&TeamAction::Join(vec!["a".into(), "b".into()]));
        round_trip(&TeamAction::Leave(vec![]));
        let mut out = Vec::new();
        TeamAction::Remove.encode(&mut out);
        assert_eq!(out, [1]);
    }

    #[test]
    fn dialog_refs() {
        let mut out = Vec::new();
        DialogRef::Registered(4).encode(&mut out);
        assert_eq!(out, [5]);
        round_trip(&DialogRef::Registered(0));
    }
}

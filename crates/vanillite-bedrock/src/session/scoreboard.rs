//! The Java scoreboard sidebar as a Bedrock sidebar objective, plus entity teleports and the own
//! movement speed.

use rustc_hash::FxHashMap;
use vanillite_protocol::codec::{Decode, Reader as JReader};
use vanillite_protocol::packets::play::clientbound::{self as pcb, relative};

use super::Session;
use crate::packets::{self as bp, id};

/// Most objectives and scores per objective kept (the server's own limits are far below).
const MAX_OBJECTIVES: usize = 256;
const MAX_SCORES: usize = 1024;
/// Java `DisplaySlot.SIDEBAR`.
const SIDEBAR: i32 = 1;

#[derive(Debug, Default)]
struct Objective {
    display: String,
    /// Owner -> score, shown name and Bedrock entry id.
    scores: FxHashMap<String, (i32, Option<String>, i64)>,
}

/// The Java objectives and which one the sidebar shows.
#[derive(Debug, Default)]
pub(super) struct Scoreboard {
    objectives: FxHashMap<String, Objective>,
    sidebar: Option<String>,
    next_entry: i64,
}

impl Session {
    /// Shows the sidebar objective again (after a removal or a new objective).
    fn redraw_sidebar(&mut self, previous: Option<&str>) {
        if let Some(old) = previous {
            self.send(id::REMOVE_OBJECTIVE, &bp::remove_objective(old));
        }
        let board = &self.scoreboard;
        let Some((name, o)) = board
            .sidebar
            .as_deref()
            .and_then(|n| Some((n, board.objectives.get(n)?)))
        else {
            return;
        };
        let display = bp::set_display_objective("sidebar", name, &o.display);
        let scores = bp::set_score(
            o.scores
                .iter()
                .map(|(owner, (score, shown, entry))| {
                    (*entry, name, *score, shown.as_deref().unwrap_or(owner))
                })
                .collect::<Vec<_>>()
                .into_iter(),
        );
        self.send(id::SET_DISPLAY_OBJECTIVE, &display);
        self.send(id::SET_SCORE, &scores);
    }

    /// `set_objective`, `set_display_objective`, `set_score` and `reset_score`.
    pub(super) fn java_scoreboard(&mut self, packet: i32, r: &mut JReader<'_>) {
        use vanillite_protocol::ids::play::clientbound as ids;
        let shown = self.scoreboard.sidebar.clone();
        let board = &mut self.scoreboard;
        let redraw = match packet {
            ids::SET_OBJECTIVE => {
                let Ok(p) = pcb::SetObjective::decode(r) else {
                    return;
                };
                let sidebar = shown.as_deref() == Some(p.name.as_str());
                match p.action {
                    pcb::ObjectiveAction::Add(params) | pcb::ObjectiveAction::Change(params) => {
                        if board.objectives.len() >= MAX_OBJECTIVES
                            && !board.objectives.contains_key(&p.name)
                        {
                            return;
                        }
                        board.objectives.entry(p.name).or_default().display =
                            params.display_name.plain();
                    }
                    pcb::ObjectiveAction::Remove => {
                        board.objectives.remove(&p.name);
                        if sidebar {
                            board.sidebar = None;
                        }
                    }
                }
                sidebar
            }
            ids::SET_DISPLAY_OBJECTIVE => {
                let Ok(d) = pcb::SetDisplayObjective::decode(r) else {
                    return;
                };
                if d.slot != SIDEBAR {
                    return;
                }
                board.sidebar = (!d.objective.is_empty()).then_some(d.objective);
                true
            }
            ids::SET_SCORE => {
                let Ok(s) = pcb::SetScore::decode(r) else {
                    return;
                };
                let next = board.next_entry.saturating_add(1);
                let Some(o) = board.objectives.get_mut(&s.objective) else {
                    return;
                };
                if o.scores.len() >= MAX_SCORES && !o.scores.contains_key(&s.owner) {
                    return;
                }
                let shown_name = s.display.map(|c| c.plain());
                let entry = match o.scores.get(&s.owner) {
                    Some(e) => e.2,
                    None => {
                        board.next_entry = next;
                        next
                    }
                };
                o.scores
                    .insert(s.owner.clone(), (s.score, shown_name.clone(), entry));
                if shown.as_deref() == Some(s.objective.as_str()) {
                    let name = shown_name.as_deref().unwrap_or(&s.owner);
                    let body =
                        bp::set_score([(entry, s.objective.as_str(), s.score, name)].into_iter());
                    self.send(id::SET_SCORE, &body);
                }
                false
            }
            ids::RESET_SCORE => {
                let Ok(s) = pcb::ResetScore::decode(r) else {
                    return;
                };
                let mut hit = false;
                for (name, o) in &mut board.objectives {
                    if s.objective.as_ref().is_none_or(|n| n == name)
                        && o.scores.remove(&s.owner).is_some()
                    {
                        hit |= shown.as_deref() == Some(name.as_str());
                    }
                }
                hit
            }
            _ => false,
        };
        if redraw {
            self.redraw_sidebar(shown.as_deref());
        }
    }

    /// `teleport_entity`.
    pub(super) fn java_teleport_entity(&mut self, r: &mut JReader<'_>) {
        let Ok(t) = pcb::TeleportEntity::decode(r) else {
            return;
        };
        let Some(e) = self
            .world
            .as_mut()
            .and_then(|w| w.entities.get_mut(&t.entity_id))
        else {
            return;
        };
        let rel = |bit: i32| t.relatives & bit != 0;
        let p = &t.change.position;
        for (i, (v, bit)) in [(p.x, relative::X), (p.y, relative::Y), (p.z, relative::Z)]
            .into_iter()
            .enumerate()
        {
            if let Some(c) = e.pos.get_mut(i) {
                *c = if rel(bit) { *c + v } else { v };
            }
        }
        e.yaw = if rel(relative::Y_ROT) {
            e.yaw + t.change.y_rot
        } else {
            t.change.y_rot
        };
        e.pitch = if rel(relative::X_ROT) {
            e.pitch + t.change.x_rot
        } else {
            t.change.x_rot
        };
        e.head_yaw = e.yaw;
        self.send_entity_move(t.entity_id, t.on_ground, true);
    }

    /// `update_attributes` of the own player: the movement speed without the sprint bonus
    /// (Bedrock adds it itself).
    pub(super) fn java_attributes(&mut self, r: &mut JReader<'_>) {
        let Some(w) = &self.world else {
            return;
        };
        let (own, java_id) = (w.entity_id, w.java_id);
        if r.varint().ok() != Some(java_id) {
            return;
        }
        let speed_id = vanillite_data::registry::Registry::by_name("minecraft:attribute")
            .and_then(|a| a.id("minecraft:movement_speed"))
            .map(i32::from);
        let Ok(n) = r.varint() else {
            return;
        };
        for _ in 0..n.clamp(0, 64) {
            let (Ok(attribute), Ok(base), Ok(modifiers)) = (r.varint(), r.f64(), r.varint()) else {
                return;
            };
            let (mut add, mut base_mul, mut total_mul) = (0.0f64, 0.0f64, 1.0f64);
            for _ in 0..modifiers.clamp(0, 256) {
                let (Ok(modifier), Ok(amount), Ok(op)) = (r.string(32767), r.f64(), r.u8()) else {
                    return;
                };
                if modifier == "minecraft:sprinting" {
                    continue;
                }
                match op {
                    0 => add += amount,
                    1 => base_mul += amount,
                    _ => total_mul *= 1.0 + amount,
                }
            }
            if Some(attribute) == speed_id {
                let value = (base + add) * (1.0 + base_mul) * total_mul;
                let value = crate::num::f32_of(value.clamp(0.0, 1024.0));
                self.send(
                    id::UPDATE_ATTRIBUTES,
                    &bp::update_movement_speed(super::rid(own), value, 0),
                );
            }
        }
    }
}

impl Session {
    /// `block_entity_data`: signs and banners.
    pub(super) fn java_block_entity(&mut self, r: &mut JReader<'_>) {
        let Ok(d) = pcb::BlockEntityData::decode(r) else {
            return;
        };
        let pos = [d.pos.x, d.pos.y, d.pos.z];
        let kind = u16::try_from(d.kind)
            .ok()
            .and_then(|k| vanillite_data::registry::BLOCK_ENTITY_TYPE.name(k));
        let fallback = match kind {
            Some("minecraft:sign") => "minecraft:oak_sign",
            Some("minecraft:hanging_sign") => "minecraft:oak_hanging_sign",
            _ => "",
        };
        let state = self.recent_blocks.get(&pos).copied().or_else(|| {
            vanillite_data::block::BlockId::by_name(fallback)
                .map(|b| u32::from(b.default_state().0))
        });
        let Some(state) = state else {
            return;
        };
        let mut input = d.nbt.as_slice();
        let mut budget = vanillite_nbt::Budget::network();
        let Ok(Some(vanillite_nbt::Tag::Compound(nbt))) =
            vanillite_nbt::read::read_network(&mut input, &mut budget)
        else {
            return;
        };
        if let Some(actor) = crate::blockentity::translate(pos, state, Some(&nbt)) {
            self.send(id::BLOCK_ACTOR_DATA, &bp::block_actor_data(pos, &actor));
        }
    }

    /// `block_actor_data` from the client: the text of the sign the server let it edit.
    pub(super) fn sign_edited(&mut self, body: &[u8]) -> crate::io::Result<()> {
        let mut r = crate::io::Reader::new(body);
        let pos = [r.vari32()?, r.vari32()?, r.vari32()?];
        let Some((sign, front)) = self.editing_sign.filter(|(p, _)| *p == pos) else {
            return Ok(());
        };
        let texts = crate::nbt::sign_texts(&mut r)?;
        let Some(text) = texts.get(usize::from(!front)).cloned().flatten() else {
            return Ok(());
        };
        self.editing_sign = None;
        let mut lines: [String; 4] = Default::default();
        for (line, part) in lines.iter_mut().zip(text.split('\n')) {
            *line = part.chars().filter(|c| !c.is_control()).take(384).collect();
        }
        self.java_send(
            &vanillite_protocol::packets::play::serverbound::SignUpdate {
                pos: vanillite_protocol::types::BlockPos {
                    x: sign[0],
                    y: sign[1],
                    z: sign[2],
                },
                lines,
                front,
            },
        );
        Ok(())
    }
}

//! Gameplay translation: block breaking and placing, entities with their data and equipment,
//! health, hunger, experience, effects, death and respawn, dimension and server changes, time,
//! weather, sounds, titles and boss bars.

use std::sync::Arc;

use rustc_hash::FxHashMap;
use vanillite_protocol::codec::{Decode, Reader as JReader};
use vanillite_protocol::item::ItemStack;
use vanillite_protocol::packets::play::clientbound::{self as pcb, GameMode};
use vanillite_protocol::packets::play::serverbound::{self as psb, DigAction, Hand};
use vanillite_protocol::types::BlockPos;

use super::{EYE, Entity, Kind, Session, f3, rid, uid};
use crate::data::{Blocks, WATERLOGGED};
use crate::entity::{self, shared};
use crate::num::{angle_deg, f32_of};
use crate::packets::{
    self as bp, Transaction, action, actor_event, id, level_event, meta, move_mode,
};

/// Runtime ids of boss bar actors start here (far above Java entity ids).
const BOSS_BASE: i64 = 0x7f00_0000;

fn java_pos(p: [i32; 3]) -> BlockPos {
    BlockPos {
        x: p[0],
        y: p[1],
        z: p[2],
    }
}

fn block_center(p: BlockPos) -> [f32; 3] {
    [p.x, p.y, p.z].map(|v| f32_of(f64::from(v) + 0.5))
}

/// Bedrock runtime id of a Java block state (without the water layer).
fn bedrock_block(blocks: &Blocks, state: i32) -> u32 {
    let e = blocks.entry(u32::try_from(state).unwrap_or(0));
    u32::from(e & !WATERLOGGED)
}

impl Session {
    /// Bedrock runtime id of a Java entity id (the own player keeps the id of its first join).
    pub(super) fn actor(&self, java: i32) -> u64 {
        match &self.world {
            Some(w) if java == w.java_id => rid(w.entity_id),
            _ => rid(java),
        }
    }

    fn own_runtime(&self) -> u64 {
        self.world.as_ref().map_or(0, |w| rid(w.entity_id))
    }

    fn next_sequence(&mut self) -> i32 {
        let Some(w) = &mut self.world else {
            return 0;
        };
        w.sequence = w.sequence.wrapping_add(1).max(1);
        w.sequence
    }

    fn dig(&mut self, action: DigAction, pos: [i32; 3], face: i32) {
        let sequence = self.next_sequence();
        self.java_send(&psb::PlayerActionPacket {
            action,
            pos: java_pos(pos),
            direction: u8::try_from(face.clamp(0, 5)).unwrap_or(0),
            sequence,
        });
    }

    // ---- Bedrock -> Java ----------------------------------------------------------------

    /// Server authoritative block breaking (`player_auth_input` block actions).
    pub(super) fn block_actions(&mut self, actions: &[bp::BlockAction]) {
        let creative = self
            .world
            .as_ref()
            .is_some_and(|w| w.game_mode == GameMode::Creative);
        for a in actions {
            let breaking = self.world.as_ref().and_then(|w| w.breaking);
            match a.action {
                action::START_BREAK => {
                    if let Some(w) = &mut self.world {
                        w.breaking = Some(a.pos);
                    }
                    self.dig(DigAction::StartDestroyBlock, a.pos, a.face);
                }
                action::CONTINUE_DESTROY_BLOCK | action::CRACK_BREAK => {
                    if breaking != Some(a.pos) && !creative {
                        if let Some(old) = breaking {
                            self.dig(DigAction::AbortDestroyBlock, old, a.face);
                        }
                        if let Some(w) = &mut self.world {
                            w.breaking = Some(a.pos);
                        }
                        self.dig(DigAction::StartDestroyBlock, a.pos, a.face);
                    }
                }
                action::ABORT_BREAK => {
                    if let Some(w) = &mut self.world {
                        w.breaking = None;
                    }
                    if !creative {
                        self.dig(DigAction::AbortDestroyBlock, a.pos, a.face);
                    }
                }
                action::PREDICT_DESTROY_BLOCK => {
                    if let Some(w) = &mut self.world {
                        w.breaking = None;
                    }
                    if creative {
                        // Creative breaks on the start already; only a new block needs one.
                        if breaking != Some(a.pos) {
                            self.dig(DigAction::StartDestroyBlock, a.pos, a.face);
                        }
                    } else {
                        self.dig(DigAction::StopDestroyBlock, a.pos, a.face);
                    }
                }
                _ => {}
            }
        }
    }

    /// An item use: placing, using an item, breaking (legacy) or hitting an entity.
    pub(super) fn transaction(&mut self, t: Transaction) {
        match t {
            Transaction::UseItem {
                action,
                pos,
                face,
                hotbar,
                click,
            } => {
                if let Ok(h) = u8::try_from(hotbar) {
                    self.select_hotbar(h);
                }
                let (yaw, pitch) = self.world.as_ref().map_or((0.0, 0.0), |w| (w.yaw, w.pitch));
                match action {
                    0 if (0..6).contains(&face) => {
                        let sequence = self.next_sequence();
                        self.java_send(&psb::UseItemOn {
                            hand: Hand::MainHand,
                            pos: java_pos(pos),
                            face,
                            cursor_x: click[0].clamp(0.0, 1.0),
                            cursor_y: click[1].clamp(0.0, 1.0),
                            cursor_z: click[2].clamp(0.0, 1.0),
                            inside: false,
                            world_border_hit: false,
                            sequence,
                        });
                    }
                    0 | 1 => {
                        let sequence = self.next_sequence();
                        self.java_send(&psb::UseItem {
                            hand: Hand::MainHand,
                            sequence,
                            y_rot: yaw,
                            x_rot: pitch,
                        });
                    }
                    2 => {
                        let creative = self
                            .world
                            .as_ref()
                            .is_some_and(|w| w.game_mode == GameMode::Creative);
                        if creative {
                            self.dig(DigAction::StartDestroyBlock, pos, face);
                        }
                    }
                    _ => {}
                }
            }
            Transaction::UseItemOnEntity {
                target,
                action,
                hotbar,
            } => {
                if let Ok(h) = u8::try_from(hotbar) {
                    self.select_hotbar(h);
                }
                let Some(java) = self.java_entity(target) else {
                    return;
                };
                if action == 0 {
                    // Use on the entity: mount, trade, feed, leash, ...
                    let sneaking = self.world.as_ref().is_some_and(|w| w.sneaking);
                    self.java_send(&psb::Interact {
                        entity_id: java,
                        hand: Hand::MainHand,
                        location: vanillite_protocol::types::LpVec3::default(),
                        using_secondary_action: sneaking,
                    });
                } else if action == 1 {
                    // `attack` (Java 26.3): the target's entity id.
                    let mut body = Vec::with_capacity(8);
                    vanillite_protocol::codec::write_varint(
                        &mut body,
                        vanillite_protocol::ids::play::serverbound::ATTACK,
                    );
                    vanillite_protocol::codec::write_varint(&mut body, java);
                    self.java_send_raw(body);
                }
            }
            Transaction::Release { .. } => {
                self.dig(DigAction::ReleaseUseItem, [0; 3], 0);
            }
            Transaction::Other => {}
        }
    }

    /// The Java entity id of a Bedrock runtime id.
    fn java_entity(&self, runtime: u64) -> Option<i32> {
        let w = self.world.as_ref()?;
        let java = i32::from_ne_bytes(u32::try_from(runtime.checked_sub(1)?).ok()?.to_ne_bytes());
        w.entities.contains_key(&java).then_some(java)
    }

    /// `interact`: opening the inventory needs the server's answer.
    pub(super) fn interact(&mut self, body: &[u8]) -> crate::io::Result<()> {
        let (kind, _target) = bp::read_interact(body)?;
        if kind == 3 {
            self.leave_vehicle();
        }
        if kind == 6 {
            let Some(w) = &self.world else {
                return Ok(());
            };
            let pos = [w.pos[0], w.pos[1], w.pos[2]].map(crate::num::floor_i32);
            let own = uid(w.entity_id);
            self.send(id::CONTAINER_OPEN, &bp::container_open(0, -1, pos, own));
            self.inventory_open = true;
        }
        Ok(())
    }

    /// `container_close` from the client.
    pub(super) fn container_close(&mut self, body: &[u8]) -> crate::io::Result<()> {
        let window = bp::read_container_close(body)?;
        if window == 0 {
            self.inventory_open = false;
            self.java_send(&psb::ContainerClose { container_id: 0 });
        }
        self.send(id::CONTAINER_CLOSE, &bp::container_close(window, -1, false));
        Ok(())
    }

    /// `player_action`: respawn and the legacy actions.
    pub(super) fn player_action(&mut self, body: &[u8]) -> crate::io::Result<()> {
        let (act, _pos, _face) = bp::read_player_action(body)?;
        if act == action::RESPAWN {
            self.request_respawn();
        }
        Ok(())
    }

    /// `respawn` from the client (state 2: ready, after the death screen).
    pub(super) fn bedrock_respawn(&mut self, body: &[u8]) -> crate::io::Result<()> {
        if bp::read_respawn_state(body)? == 2 {
            self.request_respawn();
        }
        Ok(())
    }

    fn request_respawn(&mut self) {
        let Some(w) = &self.world else {
            return;
        };
        if !w.dead {
            return;
        }
        let eye = [w.pos[0], w.pos[1] + EYE, w.pos[2]];
        let own = rid(w.entity_id);
        self.send(id::RESPAWN, &bp::respawn(f3(eye), 1, own));
        self.java_send(&psb::ClientCommand {
            action: psb::ClientCommandAction::PerformRespawn,
        });
    }

    /// `block_pick_request`.
    pub(super) fn pick_block(&mut self, body: &[u8]) -> crate::io::Result<()> {
        let pos = bp::read_block_pick(body)?;
        self.java_send(&psb::PickItemFromBlock {
            pos: java_pos(pos),
            include_data: false,
        });
        Ok(())
    }

    // ---- Java -> Bedrock: blocks --------------------------------------------------------

    pub(super) fn java_section_update(&mut self, s: &pcb::SectionBlocksUpdate) {
        let section = s.section;
        let sx = i32::try_from(section.wrapping_shr(42)).unwrap_or(0);
        let sy = i32::try_from(section.wrapping_shl(44).wrapping_shr(44)).unwrap_or(0);
        let sz = i32::try_from(section.wrapping_shl(22).wrapping_shr(42)).unwrap_or(0);
        for &v in &s.changes {
            let state = u32::try_from(v.wrapping_shr(12)).unwrap_or(0);
            let local = i32::try_from(v & 0xfff).unwrap_or(0);
            let pos = [
                sx.wrapping_mul(16).wrapping_add(local.wrapping_shr(8) & 15),
                sy.wrapping_mul(16).wrapping_add(local & 15),
                sz.wrapping_mul(16).wrapping_add(local.wrapping_shr(4) & 15),
            ];
            self.block_update(pos, state);
        }
    }

    pub(super) fn java_block_destruction(&mut self, d: &pcb::BlockDestruction) {
        let pos = block_center(d.pos);
        if d.progress <= 9 {
            let known = self
                .world
                .as_mut()
                .is_some_and(|w| w.cracking.insert(d.id, d.progress).is_some());
            if !known {
                // Bedrock animates on its own: about one second for a full crack.
                self.send(
                    id::LEVEL_EVENT,
                    &bp::level_event(level_event::START_BLOCK_CRACKING, pos, 65_535 / 20),
                );
            }
        } else {
            if let Some(w) = &mut self.world {
                w.cracking.remove(&d.id);
            }
            self.send(
                id::LEVEL_EVENT,
                &bp::level_event(level_event::STOP_BLOCK_CRACKING, pos, 0),
            );
        }
    }

    pub(super) fn java_level_event(&mut self, e: &pcb::LevelEvent) {
        // 2001: a block was destroyed (particles and sound of the state in `data`).
        if e.event == 2001 {
            let block = bedrock_block(self.version.palette.blocks(), e.data);
            let pos = block_center(e.pos);
            self.send(
                id::LEVEL_EVENT,
                &bp::level_event(
                    level_event::PARTICLES_DESTROY_BLOCK,
                    pos,
                    i32::try_from(block).unwrap_or(0),
                ),
            );
            self.send(
                id::LEVEL_SOUND_EVENT,
                &bp::level_sound_event("break", pos, i32::try_from(block).unwrap_or(0)),
            );
        }
    }

    pub(super) fn java_sound(&mut self, s: &pcb::Sound) {
        let Some(name) = u16::try_from(s.sound.0)
            .ok()
            .and_then(|i| {
                vanillite_data::registry::Registry::by_name("minecraft:sound_event")?.name(i)
            })
            .and_then(crate::data::sound_name)
        else {
            return;
        };
        let pos = [s.x, s.y, s.z].map(|v| f32_of(f64::from(v) / 8.0));
        self.send(
            id::PLAY_SOUND,
            &bp::play_sound(name, pos, s.volume, s.pitch, self.version),
        );
    }

    // ---- Java -> Bedrock: entities ------------------------------------------------------

    pub(super) fn add_entity(&mut self, e: &pcb::AddEntity) {
        let Some(w) = &mut self.world else {
            return;
        };
        let pos = [e.position.x, e.position.y, e.position.z];
        let mut entity = Entity {
            pos,
            yaw: angle_deg(e.y_rot),
            pitch: angle_deg(e.x_rot),
            head_yaw: angle_deg(e.y_head_rot),
            kind: Kind::Actor,
            java_type: u16::try_from(e.entity_type).unwrap_or(u16::MAX),
            ..Entity::default()
        };
        let kind = u16::try_from(e.entity_type)
            .ok()
            .filter(|&k| usize::from(k) < vanillite_data::entity::ENTITY_TYPES.len())
            .map(vanillite_data::entity::EntityTypeId);
        if kind == Some(vanillite_data::entity::ids::PLAYER) {
            let name = w.names.get(&e.uuid.0).cloned().unwrap_or_default();
            entity.kind = Kind::Player;
            let mode = super::bedrock_game_mode(w.game_mode);
            w.entities.insert(e.entity_id, entity.clone());
            let eye = [pos[0], pos[1] + EYE, pos[2]];
            let body = bp::add_player(
                &bp::AddPlayer {
                    uuid: e.uuid.0,
                    name: &name,
                    entity_id: uid(e.entity_id),
                    pos: f3(eye),
                    pitch: entity.pitch,
                    yaw: entity.yaw,
                    head_yaw: entity.head_yaw,
                    game_mode: if mode == 6 { 0 } else { mode },
                    sneaking: false,
                },
                self.version,
            );
            self.send(id::ADD_PLAYER, &body);
            return;
        }
        let Some(java_name) = kind.map(|k| k.name()) else {
            return;
        };
        if java_name == "minecraft:item" {
            // The item arrives with the entity data.
            entity.kind = Kind::PendingItem;
            w.entities.insert(e.entity_id, entity);
            return;
        }
        let Some(name) = entity::bedrock_identifier(java_name, &self.shared.entity_types) else {
            return;
        };
        w.entities.insert(e.entity_id, entity.clone());
        let v = e.movement.0;
        let mut m = bp::Metadata::default().long(meta::FLAGS, base_flags(0, false));
        match java_name {
            "minecraft:falling_block" => {
                let block = bedrock_block(self.version.palette.blocks(), e.data);
                m = m.int(meta::VARIANT, i32::try_from(block).unwrap_or(0));
            }
            "minecraft:tnt" => {
                m = bp::Metadata::default()
                    .long(
                        meta::FLAGS,
                        base_flags(0, false) | 1i64.wrapping_shl(meta::FLAG_IGNITED),
                    )
                    .int(meta::FUSE, 80);
            }
            _ => {}
        }
        let body = bp::add_actor(
            self.version,
            uid(e.entity_id),
            &name,
            f3(pos),
            f3([v.x, v.y, v.z]),
            entity.pitch,
            entity.yaw,
            entity.head_yaw,
            &m.float(meta::SCALE, 1.0),
        );
        self.send(id::ADD_ACTOR, &body);
    }

    pub(super) fn remove_entities(&mut self, ids: &[i32]) {
        for &e in ids {
            if self
                .world
                .as_ref()
                .is_some_and(|w| w.passengers.contains_key(&e))
            {
                self.java_passengers(e, Vec::new());
            }
            let known = self
                .world
                .as_mut()
                .and_then(|w| w.entities.remove(&e))
                .is_some_and(|e| e.kind != Kind::PendingItem);
            if known {
                self.send(id::REMOVE_ACTOR, &bp::remove_actor(uid(e)));
            }
        }
    }

    pub(super) fn java_entity_data(&mut self, d: &pcb::SetEntityData) {
        let own = self
            .world
            .as_ref()
            .is_some_and(|w| w.java_id == d.entity_id);
        let Some(e) = self
            .world
            .as_ref()
            .and_then(|w| w.entities.get(&d.entity_id))
            .cloned()
            .or_else(|| {
                own.then_some(Entity {
                    kind: Kind::Player,
                    ..Entity::default()
                })
            })
        else {
            return;
        };
        let java_name = if e.kind == Kind::Player {
            "minecraft:player"
        } else {
            java_type_name(e.java_type)
        };
        let tnt = java_name == "minecraft:tnt";
        let data = entity::read_data(&d.items.0, java_name);
        let runtime = self.actor(d.entity_id);
        if e.kind == Kind::PendingItem {
            let Some(item) = data.item.filter(|i| !i.is_empty()) else {
                return;
            };
            if let Some(w) = &mut self.world
                && let Some(ent) = w.entities.get_mut(&d.entity_id)
            {
                ent.kind = Kind::Item;
            }
            let mut stack = Vec::new();
            self.version
                .palette
                .items()
                .write_instance(&mut stack, &item, 0);
            let body = bp::add_item_actor(uid(d.entity_id), &stack, f3(e.pos), [0.0; 3]);
            self.send(id::ADD_ITEM_ACTOR, &body);
            return;
        }
        let mut next = e.clone();
        if let Some(f) = data.flags {
            next.flags = f;
        }
        if let Some(n) = data.custom_name {
            next.name = n;
        }
        if let Some(b) = data.baby {
            next.baby = b;
        }
        if let Some(w) = data.wool {
            next.wool = w;
        }
        let changed = next.flags != e.flags
            || next.name != e.name
            || next.baby != e.baby
            || next.wool != e.wool;
        if let Some(w) = &mut self.world
            && let Some(ent) = w.entities.get_mut(&d.entity_id)
        {
            *ent = next.clone();
        }
        if !changed && data.fuse.is_none() && data.effect_color.is_none() && data.villager.is_none()
        {
            return;
        }
        let player = e.kind == Kind::Player;
        let fl = actor_flags(&next, player, tnt);
        let mut m = bp::Metadata::default().long(meta::FLAGS, fl);
        if own {
            // The own player: only the flags Bedrock shows to itself.
            self.send(id::SET_ACTOR_DATA, &bp::set_actor_data(runtime, &m));
            return;
        }
        let villager = data.villager.map(entity::villager_variants);
        if let Some((profession, _)) = villager {
            m = m.int(meta::VARIANT, profession);
        }
        if java_name == "minecraft:sheep" {
            m = m.byte(meta::COLOR, next.wool & 0x0f);
        }
        if !player && let Some(n) = &next.name {
            m = m.string(meta::NAME, n);
        }
        if let Some(c) = data.effect_color {
            m = m.int(meta::EFFECT_COLOR, c);
        }
        if let Some((_, region)) = villager {
            m = m.int(meta::MARK_VARIANT, region);
        }
        if let Some(fuse) = data.fuse {
            m = m.int(meta::FUSE, fuse);
        }
        self.send(id::SET_ACTOR_DATA, &bp::set_actor_data(runtime, &m));
    }

    /// Sends the flags of a known entity again (after a change outside its entity data).
    pub(super) fn send_actor_flags(&mut self, java: i32) {
        let Some(e) = self
            .world
            .as_ref()
            .and_then(|w| w.entities.get(&java))
            .filter(|e| matches!(e.kind, Kind::Actor | Kind::Player))
            .cloned()
        else {
            return;
        };
        let tnt = java_type_name(e.java_type) == "minecraft:tnt";
        let m =
            bp::Metadata::default().long(meta::FLAGS, actor_flags(&e, e.kind == Kind::Player, tnt));
        let runtime = self.actor(java);
        self.send(id::SET_ACTOR_DATA, &bp::set_actor_data(runtime, &m));
    }

    pub(super) fn java_equipment(&mut self, e: &pcb::SetEquipment) {
        if self
            .world
            .as_ref()
            .is_some_and(|w| w.java_id == e.entity_id)
        {
            return;
        }
        if !self
            .world
            .as_ref()
            .is_some_and(|w| w.entities.contains_key(&e.entity_id))
        {
            return;
        }
        // 7: the saddle slot (Bedrock shows it as a flag).
        if let Some((_, saddle)) = e.slots.iter().rev().find(|(slot, _)| *slot == 7) {
            let saddled = !saddle.is_empty();
            let changed = self
                .world
                .as_mut()
                .and_then(|w| w.entities.get_mut(&e.entity_id))
                .is_some_and(|ent| std::mem::replace(&mut ent.saddled, saddled) != saddled);
            if changed {
                self.send_actor_flags(e.entity_id);
            }
        }
        let runtime = self.actor(e.entity_id);
        let items = self.version.palette.items();
        let eq = self.equipment.entry(e.entity_id).or_default();
        for (slot, stack) in &e.slots {
            if let Some(s) = eq.get_mut(usize::from(*slot)) {
                *s = stack.clone();
            }
        }
        let eq = eq.clone();
        let one = |s: &ItemStack| {
            let mut out = Vec::new();
            items.write_instance(&mut out, s, 0);
            out
        };
        let empty = ItemStack::EMPTY;
        let get = |i: usize| eq.get(i).unwrap_or(&empty);
        self.send(
            id::MOB_EQUIPMENT,
            &bp::mob_equipment(runtime, &one(get(0)), 0, 0),
        );
        self.send(
            id::MOB_EQUIPMENT,
            &bp::mob_equipment(runtime, &one(get(1)), 1, 119),
        );
        let mut armour = Vec::new();
        for i in [5usize, 4, 3, 2, 6] {
            armour.extend_from_slice(&one(get(i)));
        }
        self.send(
            id::MOB_ARMOUR_EQUIPMENT,
            &bp::mob_armour_equipment(runtime, &armour),
        );
    }

    pub(super) fn java_motion(&mut self, m: &pcb::SetEntityMotion) {
        let known = self.world.as_ref().is_some_and(|w| {
            w.java_id == m.entity_id
                || w.entities
                    .get(&m.entity_id)
                    .is_some_and(|e| e.kind != Kind::PendingItem)
        });
        if known {
            let v = m.movement.0;
            let runtime = self.actor(m.entity_id);
            self.send(
                id::SET_ACTOR_MOTION,
                &bp::set_actor_motion(runtime, f3([v.x, v.y, v.z])),
            );
        }
    }

    pub(super) fn java_take_item(&mut self, t: &pcb::TakeItemEntity) {
        let known = self.world.as_ref().is_some_and(|w| {
            w.entities
                .get(&t.item_id)
                .is_some_and(|e| e.kind == Kind::Item)
        });
        if known {
            let (item, taker) = (self.actor(t.item_id), self.actor(t.player_id));
            self.send(id::TAKE_ITEM_ACTOR, &bp::take_item_actor(item, taker));
        }
    }

    pub(super) fn java_swing(&mut self, s: &pcb::SwingAnimation) {
        let runtime = self.actor(s.entity_id);
        if self
            .world
            .as_ref()
            .is_some_and(|w| w.java_id != s.entity_id)
        {
            self.send(id::ANIMATE, &bp::animate(1, runtime, self.version));
        }
    }

    pub(super) fn java_hurt(&mut self, entity_id: i32) {
        let runtime = self.actor(entity_id);
        self.send(
            id::ACTOR_EVENT,
            &bp::actor_event(runtime, actor_event::HURT, 0),
        );
    }

    pub(super) fn java_entity_event(&mut self, e: &pcb::EntityEvent) {
        // 3: a living entity died.
        if e.event == 3 {
            let runtime = self.actor(e.entity_id);
            self.send(
                id::ACTOR_EVENT,
                &bp::actor_event(runtime, actor_event::DEATH, 0),
            );
        }
    }

    // ---- Java -> Bedrock: the own player ------------------------------------------------

    pub(super) fn java_health(&mut self, h: &pcb::SetHealth) {
        let own = self.own_runtime();
        let health = h.health.clamp(0.0, 1024.0);
        let food = f32_of(f64::from(h.food.clamp(0, 20)));
        let attributes = [
            bp::Attribute {
                name: "minecraft:health",
                min: 0.0,
                max: 20f32.max(health),
                value: health.ceil(),
                default: 20.0,
            },
            bp::Attribute {
                name: "minecraft:player.hunger",
                min: 0.0,
                max: 20.0,
                value: food,
                default: 20.0,
            },
            bp::Attribute {
                name: "minecraft:player.saturation",
                min: 0.0,
                max: 20.0,
                value: h.saturation.clamp(0.0, 20.0),
                default: 5.0,
            },
        ];
        self.send(
            id::UPDATE_ATTRIBUTES,
            &bp::update_attributes(own, &attributes),
        );
        let Some(w) = &mut self.world else {
            return;
        };
        if health <= 0.0 && !w.dead {
            w.dead = true;
            // The death screen; its button sends `respawn` (state 2).
            self.send(id::RESPAWN, &bp::respawn([0.0; 3], 0, own));
        } else if health > 0.0 {
            w.dead = false;
        }
    }

    pub(super) fn java_combat_kill(&mut self, k: &pcb::PlayerCombatKill) {
        if self
            .world
            .as_ref()
            .is_some_and(|w| w.java_id == k.player_id)
        {
            let text = k.message.plain();
            self.send(id::DEATH_INFO, &bp::death_info(&text));
        }
    }

    pub(super) fn java_experience(&mut self, x: &pcb::SetExperience) {
        let own = self.own_runtime();
        let attributes = [
            bp::Attribute {
                name: "minecraft:player.level",
                min: 0.0,
                max: 24_791.0,
                value: f32_of(f64::from(x.level.max(0))),
                default: 0.0,
            },
            bp::Attribute {
                name: "minecraft:player.experience",
                min: 0.0,
                max: 1.0,
                value: x.progress.clamp(0.0, 1.0),
                default: 0.0,
            },
        ];
        self.send(
            id::UPDATE_ATTRIBUTES,
            &bp::update_attributes(own, &attributes),
        );
    }

    pub(super) fn java_time(&mut self, t: &pcb::SetTime) {
        let Some(index) = self
            .world_clocks
            .iter()
            .position(|c| c == "minecraft:overworld")
            .and_then(|i| i32::try_from(i).ok())
        else {
            return;
        };
        let Some(clock) = t.clocks.iter().find(|c| c.clock == index) else {
            return;
        };
        let ticks = i32::try_from(clock.total_ticks.rem_euclid(1 << 30)).unwrap_or(0);
        self.send(id::SET_TIME, &bp::set_time(ticks));
        let running = clock.rate > 0.0;
        let Some(w) = &mut self.world else {
            return;
        };
        if w.daylight != Some(running) {
            w.daylight = Some(running);
            self.send(
                id::GAME_RULES_CHANGED,
                &bp::game_rule_changed("dodaylightcycle", running),
            );
        }
    }

    /// Weather and game mode (`game_event`).
    pub(super) fn java_game_event(&mut self, e: &pcb::GameEvent) {
        use vanillite_protocol::packets::play::clientbound::GameEvent as E;
        let pos = [0.0f32; 3];
        let level = |v: f32| crate::num::f32_to_i32(v.clamp(0.0, 1.0) * 65_535.0);
        match e.event {
            E::CHANGE_GAME_MODE => {
                let mode = match crate::num::f32_to_u8(e.param) {
                    1 => GameMode::Creative,
                    2 => GameMode::Adventure,
                    3 => GameMode::Spectator,
                    _ => GameMode::Survival,
                };
                if let Some(w) = &mut self.world {
                    w.game_mode = mode;
                }
                self.send(
                    id::SET_PLAYER_GAME_TYPE,
                    &bp::set_player_game_type(super::bedrock_game_mode(mode)),
                );
                self.send_abilities();
            }
            1 => self.send(
                id::LEVEL_EVENT,
                &bp::level_event(level_event::START_RAIN, pos, 65_535),
            ),
            2 => {
                self.send(
                    id::LEVEL_EVENT,
                    &bp::level_event(level_event::STOP_RAIN, pos, 0),
                );
                self.send(
                    id::LEVEL_EVENT,
                    &bp::level_event(level_event::STOP_THUNDER, pos, 0),
                );
            }
            7 => {
                let (event, data) = if e.param > 0.0 {
                    (level_event::START_RAIN, level(e.param))
                } else {
                    (level_event::STOP_RAIN, 0)
                };
                self.send(id::LEVEL_EVENT, &bp::level_event(event, pos, data));
            }
            8 => {
                let (event, data) = if e.param > 0.0 {
                    (level_event::START_THUNDER, level(e.param))
                } else {
                    (level_event::STOP_THUNDER, 0)
                };
                self.send(id::LEVEL_EVENT, &bp::level_event(event, pos, data));
            }
            _ => {}
        }
    }

    pub(super) fn java_spawn_position(&mut self, s: &pcb::SetDefaultSpawnPosition) {
        let Some(w) = &self.world else {
            return;
        };
        let p = s.pos.pos;
        let dim = w.layout.dimension;
        self.send(
            id::SET_SPAWN_POSITION,
            &bp::set_spawn_position([p.x, p.y, p.z], dim),
        );
    }

    /// `respawn`: after death, or a new dimension.
    pub(super) fn java_respawn(&mut self, r: &pcb::Respawn) {
        let dim_type = usize::try_from(r.spawn.dimension_type)
            .ok()
            .and_then(|i| self.dimension_types.get(i))
            .cloned()
            .unwrap_or_else(|| r.spawn.dimension.as_str().to_owned());
        let layout = super::layout_for(r.spawn.dimension.as_str(), &dim_type);
        self.reset_world(layout, r.spawn.game_mode, false);
    }

    /// A new world on the Java side (respawn, dimension change or a server switch through a
    /// proxy): forget the entities, switch the Bedrock dimension when the position is known.
    pub(super) fn reset_world(
        &mut self,
        layout: crate::chunk::Layout,
        mode: GameMode,
        switch: bool,
    ) {
        let Some(w) = &mut self.world else {
            return;
        };
        let ids: Vec<i32> = w
            .entities
            .iter()
            .filter(|(_, e)| e.kind != Kind::PendingItem)
            .map(|(&id, _)| id)
            .collect();
        w.entities.clear();
        w.cracking.clear();
        w.passengers.clear();
        w.vehicle = None;
        w.predicted_vehicle = None;
        w.merchant = None;
        let bosses: Vec<i64> = w.bosses.drain().map(|(_, b)| b.0).collect();
        let changed = w.layout.dimension != layout.dimension || switch;
        if changed {
            w.pending_dimension = Some((
                layout.dimension,
                switch && w.layout.dimension == layout.dimension,
            ));
        }
        w.layout = layout;
        let mode_changed = w.game_mode != mode;
        w.game_mode = mode;
        w.dead = false;
        w.breaking = None;
        self.equipment.clear();
        for e in ids {
            self.send(id::REMOVE_ACTOR, &bp::remove_actor(uid(e)));
        }
        for b in bosses {
            self.send(
                id::BOSS_EVENT,
                &bp::boss_event(b, 2, "", 0.0, 0, self.version),
            );
            self.send(id::REMOVE_ACTOR, &bp::remove_actor(b));
        }
        if mode_changed {
            self.send(
                id::SET_PLAYER_GAME_TYPE,
                &bp::set_player_game_type(super::bedrock_game_mode(mode)),
            );
            self.send_abilities();
        }
    }

    /// The dimension switch of [`Self::reset_world`], once the new position is known.
    pub(super) fn finish_dimension_change(&mut self, target: [f64; 3]) {
        let Some(w) = &mut self.world else {
            return;
        };
        let Some((dim, same)) = w.pending_dimension.take() else {
            return;
        };
        let eye = f3([target[0], target[1] + EYE, target[2]]);
        if same {
            // Bedrock drops its chunks only on a dimension change: pass through another one.
            let other = if dim == 0 { 1 } else { 0 };
            self.send(
                id::CHANGE_DIMENSION,
                &bp::change_dimension(other, eye, false),
            );
        }
        self.send(id::CHANGE_DIMENSION, &bp::change_dimension(dim, eye, false));
        self.send(id::PLAY_STATUS, &bp::play_status(bp::status::PLAYER_SPAWN));
    }

    // ---- Java -> Bedrock: packets without a typed decoder -------------------------------

    /// Titles, boss bars and effects.
    pub(super) fn java_other(&mut self, packet: i32, body: &[u8]) {
        use vanillite_protocol::ids::play::clientbound as ids;
        let mut r = JReader::new(body);
        if r.varint().is_err() {
            return;
        }
        let text = |r: &mut JReader<'_>| entity::component(r).map(|c| c.plain()).ok();
        match packet {
            ids::SET_TITLE_TEXT => {
                if let Some(t) = text(&mut r) {
                    self.send(id::SET_TITLE, &bp::set_title(2, &t, [0; 3]));
                }
            }
            ids::SET_SUBTITLE_TEXT => {
                if let Some(t) = text(&mut r) {
                    self.send(id::SET_TITLE, &bp::set_title(3, &t, [0; 3]));
                }
            }
            ids::SET_ACTION_BAR_TEXT => {
                if let Some(t) = text(&mut r) {
                    self.send(id::SET_TITLE, &bp::set_title(4, &t, [0; 3]));
                }
            }
            ids::SET_TITLES_ANIMATION => {
                if let (Ok(a), Ok(b), Ok(c)) = (r.i32(), r.i32(), r.i32()) {
                    self.send(id::SET_TITLE, &bp::set_title(5, "", [a, b, c]));
                }
            }
            ids::CLEAR_TITLES => {
                let reset = r.u8().is_ok_and(|v| v != 0);
                self.send(id::SET_TITLE, &bp::set_title(i32::from(reset), "", [0; 3]));
            }
            ids::BOSS_EVENT => self.java_boss(&mut r),
            ids::SET_PASSENGERS => {
                if let Ok(p) = pcb::SetPassengers::decode(&mut r) {
                    self.java_passengers(p.vehicle, p.passengers.0);
                }
            }
            ids::MOVE_VEHICLE => {
                if let Ok(m) = pcb::MoveVehicleBack::decode(&mut r) {
                    self.java_move_vehicle(&m);
                }
            }
            ids::LEVEL_PARTICLES => {
                if let Ok(p) = pcb::LevelParticles::decode(&mut r) {
                    self.java_particles(&p);
                }
            }
            ids::RESOURCE_PACK_PUSH => {
                if let Ok(p) = pcb::ResourcePackPush::decode(&mut r) {
                    use vanillite_protocol::packets::common::ResourcePackAction as A;
                    for action in [A::Accepted, A::SuccessfullyLoaded] {
                        self.java_send(&psb::ResourcePack { id: p.id, action });
                    }
                }
            }
            ids::EXPLODE => self.java_explode(&mut r),
            ids::SOUND_ENTITY => self.java_entity_sound(&mut r),
            ids::OPEN_SCREEN => self.java_open_screen(&mut r),
            ids::MERCHANT_OFFERS => self.java_merchant_offers(&mut r),
            ids::CONTAINER_CLOSE => {
                if let Ok(container) = r.varint() {
                    self.java_container_closed(container);
                }
            }
            _ => {}
        }
    }

    /// `update_mob_effect` (`Some((amplifier, duration, flags))`) or `remove_mob_effect`.
    pub(super) fn java_mob_effect(
        &mut self,
        entity: i32,
        effect: i32,
        update: Option<(i32, i32, u8)>,
    ) {
        let Some(bedrock) = u16::try_from(effect)
            .ok()
            .and_then(|e| {
                vanillite_data::registry::Registry::by_name("minecraft:mob_effect")?.name(e)
            })
            .and_then(entity::effect_id)
        else {
            return;
        };
        let runtime = self.actor(entity);
        let body = match update {
            None => bp::mob_effect(runtime, 3, bedrock, 0, false, 0),
            Some((amplifier, duration, flags)) => {
                bp::mob_effect(runtime, 1, bedrock, amplifier, flags & 2 != 0, duration)
            }
        };
        self.send(id::MOB_EFFECT, &body);
    }

    /// The Bedrock dimension the player is in.
    fn dimension(&self) -> u8 {
        self.world
            .as_ref()
            .and_then(|w| u8::try_from(w.layout.dimension).ok())
            .unwrap_or(0)
    }

    /// `level_particles`: vanilla Bedrock particle effects, at most 8 per packet spread over the
    /// packet's distribution box.
    fn java_particles(&mut self, p: &pcb::LevelParticles) {
        let Some(name) = u16::try_from(p.particle.kind)
            .ok()
            .and_then(|k| vanillite_data::registry::PARTICLE_TYPE.name(k))
            .and_then(crate::data::particle_name)
        else {
            return;
        };
        let dimension = self.dimension();
        let n = p.count.clamp(1, 8);
        let dist = [p.x_dist, p.y_dist, p.z_dist].map(|d| d.clamp(0.0, 16.0));
        let mut seed = u32::try_from((p.x.to_bits() ^ p.z.to_bits()) & 0xffff_ffff).unwrap_or(0);
        for i in 0..n {
            let mut pos = [p.x, p.y, p.z].map(f32_of);
            if p.count > 0 && i > 0 {
                for (v, d) in pos.iter_mut().zip(dist) {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    let unit = f32_of(f64::from(seed >> 8) / f64::from(1u32 << 24)) * 2.0 - 1.0;
                    *v += unit * d;
                }
            }
            self.send(
                id::SPAWN_PARTICLE_EFFECT,
                &bp::spawn_particle_effect(dimension, pos, name),
            );
        }
    }

    /// `explode`: the explosion's particle and sound, and the knockback of the own player.
    fn java_explode(&mut self, r: &mut JReader<'_>) {
        let (Ok(x), Ok(y), Ok(z), Ok(_radius), Ok(_blocks)) =
            (r.f64(), r.f64(), r.f64(), r.f32(), r.i32())
        else {
            return;
        };
        let knockback = match r.u8() {
            Ok(0) => None,
            Ok(_) => match (r.f64(), r.f64(), r.f64()) {
                (Ok(a), Ok(b), Ok(c)) => Some([a, b, c]),
                _ => return,
            },
            Err(_) => return,
        };
        let pos = [x, y, z].map(f32_of);
        if let Ok(p) = vanillite_protocol::packets::play::clientbound::Particle::decode(r)
            && let Some(name) = u16::try_from(p.kind)
                .ok()
                .and_then(|k| vanillite_data::registry::PARTICLE_TYPE.name(k))
                .and_then(crate::data::particle_name)
        {
            let dimension = self.dimension();
            self.send(
                id::SPAWN_PARTICLE_EFFECT,
                &bp::spawn_particle_effect(dimension, pos, name),
            );
        }
        self.send(
            id::LEVEL_SOUND_EVENT,
            &bp::level_sound_event("explode", pos, -1),
        );
        if let Some(k) = knockback
            && let Some(w) = &self.world
        {
            let own = rid(w.entity_id);
            self.send(id::SET_ACTOR_MOTION, &bp::set_actor_motion(own, f3(k)));
        }
    }

    /// `sound_entity`: a sound at an entity.
    fn java_entity_sound(&mut self, r: &mut JReader<'_>) {
        let (Ok(sound), Ok(_source), Ok(entity), Ok(volume), Ok(pitch)) = (
            pcb::SoundEvent::decode(r),
            r.varint(),
            r.varint(),
            r.f32(),
            r.f32(),
        ) else {
            return;
        };
        let Some(pos) = self.world.as_ref().and_then(|w| {
            if entity == w.java_id {
                Some(w.pos)
            } else {
                w.entities.get(&entity).map(|e| e.pos)
            }
        }) else {
            return;
        };
        let Some(name) = u16::try_from(sound.0)
            .ok()
            .and_then(|i| {
                vanillite_data::registry::Registry::by_name("minecraft:sound_event")?.name(i)
            })
            .and_then(crate::data::sound_name)
        else {
            return;
        };
        self.send(
            id::PLAY_SOUND,
            &bp::play_sound(name, f3(pos), volume, pitch, self.version),
        );
    }

    fn java_boss(&mut self, r: &mut JReader<'_>) {
        let (Ok(uuid), Ok(op)) = (r.u128(), r.varint()) else {
            return;
        };
        let Some(w) = &mut self.world else {
            return;
        };
        match op {
            0 => {
                let Ok(title) = entity::component(r).map(|c| c.plain()) else {
                    return;
                };
                let (Ok(health), Ok(colour)) = (r.f32(), r.varint()) else {
                    return;
                };
                if w.bosses.len() >= 16 && !w.bosses.contains_key(&uuid) {
                    return;
                }
                w.next_boss = w.next_boss.wrapping_add(1);
                let entity = BOSS_BASE.saturating_add(i64::from(w.next_boss & 0xffff));
                let colour = u8::try_from(colour.clamp(0, 6)).unwrap_or(0);
                w.bosses
                    .insert(uuid, (entity, title.clone(), health, colour));
                // An invisible, tiny actor carries the bar (Bedrock shows bars of actors).
                let below = [w.pos[0], w.pos[1] - 10.0, w.pos[2]];
                let m = bp::Metadata::default()
                    .long(meta::FLAGS, 1i64.wrapping_shl(meta::FLAG_INVISIBLE))
                    .float(meta::SCALE, 0.0)
                    .float(meta::WIDTH, 0.0)
                    .float(meta::HEIGHT, 0.0);
                let body = bp::add_actor(
                    self.version,
                    entity,
                    "minecraft:creeper",
                    f3(below),
                    [0.0; 3],
                    0.0,
                    0.0,
                    0.0,
                    &m,
                );
                self.send(id::ADD_ACTOR, &body);
                self.send(
                    id::BOSS_EVENT,
                    &bp::boss_event(entity, 0, &title, health, colour, self.version),
                );
            }
            1 => {
                if let Some((entity, ..)) = w.bosses.remove(&uuid) {
                    self.send(
                        id::BOSS_EVENT,
                        &bp::boss_event(entity, 2, "", 0.0, 0, self.version),
                    );
                    self.send(id::REMOVE_ACTOR, &bp::remove_actor(entity));
                }
            }
            2 | 3 => {
                let Some(b) = w.bosses.get_mut(&uuid) else {
                    return;
                };
                let event = if op == 2 {
                    let Ok(h) = r.f32() else {
                        return;
                    };
                    b.2 = h;
                    4
                } else {
                    let Ok(t) = entity::component(r).map(|c| c.plain()) else {
                        return;
                    };
                    b.1 = t;
                    5
                };
                let (entity, title, health, colour) = b.clone();
                self.send(
                    id::BOSS_EVENT,
                    &bp::boss_event(entity, event, &title, health, colour, self.version),
                );
            }
            _ => {}
        }
    }

    // ---- Skins ------------------------------------------------------------------------

    /// Loads the Java skin of a listed player in the background (once per texture).
    pub(super) fn fetch_java_skin(
        &mut self,
        uuid: u128,
        properties: &[vanillite_protocol::types::Property],
    ) {
        let Some((url, slim)) = crate::skin::java_texture(properties) else {
            return;
        };
        let cached = self.shared.java_skins.lock().ok().and_then(|mut cache| {
            if let Some(s) = cache.get(&url) {
                return Some(s.clone());
            }
            if cache.len() >= 4096 {
                cache.clear();
            }
            cache.insert(url.clone(), None);
            None
        });
        let tx = self.events_tx.clone();
        match cached {
            Some(Some(skin)) => {
                let _ = tx.try_send(super::Event::Skin(uuid, skin));
            }
            // Being loaded for another player: this one keeps the plain skin.
            Some(None) => {}
            None => {
                let shared = Arc::clone(&self.shared);
                tokio::task::spawn_blocking(move || {
                    let skin = crate::skin::fetch_java(&url, slim).map(Arc::new);
                    if let Ok(mut cache) = shared.java_skins.lock() {
                        match &skin {
                            Some(s) => {
                                cache.insert(url, Some(Arc::clone(s)));
                            }
                            None => {
                                cache.remove(&url);
                            }
                        }
                    }
                    if let Some(skin) = skin {
                        let _ = tx.try_send(super::Event::Skin(uuid, skin));
                    }
                });
            }
        }
    }

    /// A loaded Java skin.
    pub(super) fn apply_skin(&mut self, uuid: u128, skin: &crate::skin::Skin) {
        if self
            .world
            .as_ref()
            .is_some_and(|w| w.listed.contains(&uuid))
        {
            self.send(id::PLAYER_SKIN, &bp::player_skin(uuid, skin, self.version));
        }
    }

    /// Sends the own player's teleport after a respawn or dimension change.
    pub(super) fn send_own_position(&mut self, target: [f64; 3]) {
        let Some(w) = &self.world else {
            return;
        };
        let (yaw, pitch, rid) = (w.yaw, w.pitch, rid(w.entity_id));
        let eye = [target[0], target[1] + EYE, target[2]];
        self.send(
            id::MOVE_PLAYER,
            &bp::move_player(rid, f3(eye), pitch, yaw, yaw, move_mode::TELEPORT, false, 0),
        );
    }
}

/// The Java name of an entity type id ("" when unknown).
pub(super) fn java_type_name(id: u16) -> &'static str {
    vanillite_data::registry::Registry::by_name("minecraft:entity_type")
        .and_then(|r| r.name(id))
        .unwrap_or("")
}

/// All Bedrock flags of a known entity.
fn actor_flags(e: &Entity, player: bool, tnt: bool) -> i64 {
    let mut fl = base_flags(e.flags, player);
    for (on, flag) in [
        (e.baby, meta::FLAG_BABY),
        (tnt, meta::FLAG_IGNITED),
        (e.saddled, meta::FLAG_SADDLED),
        (e.wool & 0x10 != 0, meta::FLAG_SHEARED),
        (e.riding, meta::FLAG_RIDING),
    ] {
        if on {
            fl |= 1i64.wrapping_shl(flag);
        }
    }
    fl
}

/// The Bedrock flags of an entity from its Java shared flags.
fn base_flags(java: u8, player: bool) -> i64 {
    let mut f = bp::entity_flags(java & shared::CROUCHING != 0, java & shared::SPRINTING != 0);
    if !player {
        f &= !(1i64.wrapping_shl(meta::FLAG_ALWAYS_SHOW_NAME)
            | 1i64.wrapping_shl(meta::FLAG_SHOW_NAME));
    }
    for (bit, flag) in [
        (shared::ON_FIRE, meta::FLAG_ON_FIRE),
        (shared::INVISIBLE, meta::FLAG_INVISIBLE),
        (shared::SWIMMING, meta::FLAG_SWIMMING),
        (shared::FALL_FLYING, meta::FLAG_GLIDING),
    ] {
        if java & bit != 0 {
            f |= 1i64.wrapping_shl(flag);
        }
    }
    f
}

/// Per-entity Java equipment (mainhand, offhand, feet, legs, chest, head, body).
pub(super) type Equipment = FxHashMap<i32, [ItemStack; 7]>;

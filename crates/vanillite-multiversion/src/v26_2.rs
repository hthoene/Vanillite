//! Step 26.2 (protocol 776) ↔ 26.3 (protocol 777).
//!
//! Ids come from the generated tables ([`crate::generated::v26_2`]); this module rewrites the
//! packets whose fields changed and the ones that carry registry ids. Everything else only gets
//! its packet id rewritten. The wire formats on both sides are this step's own knowledge: it
//! never uses the typed packets of `vanillite-protocol`, which follow the current version.

use std::sync::{Arc, Mutex, PoisonError};

use crate::chain::{ClientboundStep, ServerboundStep};
use crate::chunk::{self, Kind};
use crate::common::{self, Registries};
use crate::entity_data::{self, Tables};
use crate::generated::v26_2 as g;
use crate::io;
use crate::items::{self, Maps};
use crate::table::{PacketTable, Remap};
use crate::{Result, State};
use g::new_configuration_clientbound as ncc;
use g::new_play_clientbound as npc;
use g::new_play_serverbound as nps;
use g::old_configuration_serverbound as ocs;
use g::old_play_serverbound as ops;
use vanillite_protocol::Error;
use vanillite_protocol::codec::{Reader, write_varint};
use vanillite_protocol::components::Ref;

/// Opens both halves of the step for one connection.
///
/// `deep`: older steps follow, so item stacks keep only the components they translate
/// ([`crate::portable`]).
pub(crate) fn open(deep: bool) -> (Box<dyn ClientboundStep>, Box<dyn ServerboundStep>) {
    let shared = Arc::new(Mutex::new(Shared::default()));
    (
        Box::new(Clientbound {
            shared: Arc::clone(&shared),
            regs: Registries::default(),
            refs: Vec::new(),
            deep,
        }),
        Box::new(Serverbound {
            shared,
            pos: [0.0; 3],
            rot: [0.0; 2],
            interacted: false,
        }),
    )
}

/// State one half learns and the other needs.
#[derive(Debug, Default)]
struct Shared {
    /// Teleports sent and not yet confirmed: id, position, rotation, relative flags.
    teleports: Vec<(i32, [f64; 3], [f32; 2], i32)>,
}

/// The step's static registry tables, by registry name.
struct StaticMaps {
    /// Only the portable components survive (older steps follow).
    deep: bool,
}

impl Maps for StaticMaps {
    fn registry(&self, name: &str) -> Option<&'static Remap> {
        static_remap(name)
    }

    fn component(&self, t: u16) -> Option<u16> {
        if self.deep
            && !g::NEW_DATA_COMPONENT_TYPES
                .get(usize::from(t))
                .is_some_and(|n| crate::portable::portable(n))
        {
            return None;
        }
        // `swing_animation` was renamed `attack_animation` with the same codec.
        if i32::from(t) == g::NEW_DATA_COMPONENT_TYPE_ATTACK_ANIMATION {
            return u16::try_from(g::OLD_DATA_COMPONENT_TYPE_SWING_ANIMATION).ok();
        }
        g::DATA_COMPONENT_TYPE
            .older(i32::from(t))
            .and_then(|t| u16::try_from(t).ok())
    }

    fn convert(&self, t: u16, value: &[u8], out: &mut Vec<u8>) -> Option<Result<()>> {
        (i32::from(t) == g::NEW_DATA_COMPONENT_TYPE_POT_DECORATIONS)
            .then(|| pot_decorations(value, out))
    }
}

/// `pot_decorations`: 26.3 has four optional item stack templates (back, left, right, front),
/// 26.2 a list of item ids where brick is a plain side.
fn pot_decorations(value: &[u8], out: &mut Vec<u8>) -> Result<()> {
    let mut r = Reader::new(value);
    let mut ids = [g::OLD_ITEM_BRICK; 4];
    for id in &mut ids {
        if r.u8()? != 0 {
            *id = g::ITEM.older(r.varint()?).unwrap_or(g::OLD_ITEM_BRICK);
            r.varint()?;
            let mut skip = Vec::new();
            items::patch_to_old(
                &mut r,
                &mut skip,
                &StaticMaps { deep: false },
                &mut Vec::new(),
            )?;
        }
    }
    let len = ids
        .iter()
        .rposition(|&i| i != g::OLD_ITEM_BRICK)
        .map_or(0, |i| i.saturating_add(1));
    io::write_len(out, len);
    for id in ids.iter().take(len) {
        write_varint(out, *id);
    }
    Ok(())
}

/// A component type of a 26.2 client in 26.3 numbering.
fn component_to_new(t: i32) -> Option<i32> {
    if t == g::OLD_DATA_COMPONENT_TYPE_SWING_ANIMATION {
        return Some(g::NEW_DATA_COMPONENT_TYPE_ATTACK_ANIMATION);
    }
    g::DATA_COMPONENT_TYPE.newer(t)
}

fn static_remap(name: &str) -> Option<&'static Remap> {
    Some(match name {
        "minecraft:block" => &g::BLOCK,
        "minecraft:item" => &g::ITEM,
        "minecraft:entity_type" => &g::ENTITY_TYPE,
        "minecraft:sound_event" => &g::SOUND_EVENT,
        "minecraft:particle_type" => &g::PARTICLE_TYPE,
        "minecraft:data_component_type" => &g::DATA_COMPONENT_TYPE,
        "minecraft:command_argument_type" => &g::COMMAND_ARGUMENT_TYPE,
        "minecraft:map_decoration_type" => &g::MAP_DECORATION_TYPE,
        "minecraft:custom_stat" => &g::CUSTOM_STAT,
        _ => return None,
    })
}

/// Index of a data component type of the newer version.
fn new_component(name: &str) -> u16 {
    g::NEW_DATA_COMPONENT_TYPES
        .iter()
        .position(|e| *e == name)
        .and_then(|i| u16::try_from(i).ok())
        .unwrap_or(u16::MAX)
}

fn tables(maps: &dyn Maps) -> Tables<'_> {
    Tables {
        maps,
        block_state: &g::BLOCK_STATE,
        particle_type: &g::PARTICLE_TYPE,
        particle_names: g::NEW_PARTICLE_TYPES,
        profile_component: new_component("minecraft:profile"),
        painting_component: new_component("minecraft:painting/variant"),
    }
}

fn packets(state: State, clientbound: bool) -> Option<&'static PacketTable> {
    Some(match (state, clientbound) {
        (State::Login, true) => &g::LOGIN_CLIENTBOUND,
        (State::Login, false) => &g::LOGIN_SERVERBOUND,
        (State::Configuration, true) => &g::CONFIGURATION_CLIENTBOUND,
        (State::Configuration, false) => &g::CONFIGURATION_SERVERBOUND,
        (State::Play, true) => &g::PLAY_CLIENTBOUND,
        (State::Play, false) => &g::PLAY_SERVERBOUND,
        (State::Handshake | State::Status, _) => return None,
    })
}

/// Global palette sizes: block states on both sides.
fn block_kind() -> Kind {
    Kind {
        entries: 4096,
        max_indirect: 8,
        global_new: chunk::global_bits(g::BLOCK_STATE.to_old.len()),
        global_old: chunk::global_bits(g::BLOCK_STATE.to_new.len()),
    }
}

#[derive(Debug)]
struct Clientbound {
    shared: Arc<Mutex<Shared>>,
    /// Synchronised registries as sent in this connection's configuration.
    regs: Registries,
    refs: Vec<Ref>,
    deep: bool,
}

impl Clientbound {
    fn maps(&self) -> StaticMaps {
        StaticMaps { deep: self.deep }
    }

    fn biome_kind(&self) -> Kind {
        self.regs.biome_kind()
    }

    fn biome_map(&self) -> impl Fn(i32) -> i32 + '_ {
        self.regs.biome_map()
    }
}

fn block_state_to_old(id: i32) -> i32 {
    g::BLOCK_STATE.older(id).unwrap_or(0)
}

impl ClientboundStep for Clientbound {
    fn translate(
        &mut self,
        state: State,
        body: &[u8],
        out: &mut Vec<u8>,
        _ctx: &crate::chain::Ctx<'_>,
    ) -> Result<bool> {
        let Some(table) = packets(state, true) else {
            out.extend_from_slice(body);
            return Ok(true);
        };
        let mut r = Reader::new(body);
        let id = r.varint()?;
        if state == State::Play && id == npc::SWING_ANIMATION {
            // 26.2 shows swings through `animate`: 0 main hand, 3 off hand.
            let entity = r.varint()?;
            let hand = r.varint()?;
            write_varint(out, g::old_play_clientbound::ANIMATE);
            write_varint(out, entity);
            out.push(if hand == 1 { 3 } else { 0 });
            return Ok(true);
        }
        let Some(old_id) = table.older(id) else {
            // A packet the older version does not have.
            return Ok(false);
        };
        write_varint(out, old_id);
        match state {
            State::Configuration => self.configuration(id, &mut r, out),
            State::Play => self.play(id, &mut r, out),
            _ => {
                out.extend_from_slice(r.take_rest());
                Ok(true)
            }
        }
    }

    fn stateless(&self, state: State, id: i32) -> bool {
        // Packets that change or read per-connection state; all others translate the same for
        // every client of 26.2 (chunk biomes use the server's registries, the same for all).
        !(state != State::Play
            || id == npc::PLAYER_POSITION
            || id == npc::LOGIN
            || id == npc::RESPAWN
            || id == npc::UPDATE_TAGS
            || id == npc::START_CONFIGURATION
            || id == npc::RECIPE_BOOK_ADD
            || id == npc::UPDATE_RECIPES
            || id == npc::PLACE_GHOST_RECIPE
            || id == npc::UPDATE_ADVANCEMENTS)
    }

    fn entity_type_older(&self, new: i32) -> Option<i32> {
        g::ENTITY_TYPE.older(new)
    }
}

impl Clientbound {
    fn configuration(&mut self, id: i32, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<bool> {
        match id {
            ncc::SELECT_KNOWN_PACKS => {
                common::known_packs(r, out, g::NEXT_PROTOCOL, g::PROTOCOL)?;
            }
            ncc::REGISTRY_DATA => return self.regs.registry_data(&g::TABLES, r, out),
            ncc::UPDATE_TAGS => self.regs.tags(&g::TABLES, r, out)?,
            _ => out.extend_from_slice(r.take_rest()),
        }
        Ok(true)
    }

    fn play(&mut self, id: i32, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<bool> {
        let maps = self.maps();
        match id {
            npc::LEVEL_CHUNK_WITH_LIGHT => {
                io::bytes(r, out, 8)?;
                chunk::heightmaps(r, out)?;
                let len = io::len(r, 2_097_152, 1)?;
                let data = r.take(len)?;
                let mut sections = Vec::with_capacity(len);
                chunk::sections(
                    data,
                    &mut sections,
                    block_kind(),
                    self.biome_kind(),
                    &block_state_to_old,
                    &self.biome_map(),
                )?;
                io::write_len(out, sections.len());
                out.extend_from_slice(&sections);
                // Block entities: packed xz, y, type, NBT.
                let n = io::len(r, 1 << 16, 4)?;
                io::write_len(out, n);
                for _ in 0..n {
                    io::bytes(r, out, 3)?;
                    io::varint(r, out)?;
                    io::nbt(r, out)?;
                }
                light_data(r, out)?;
            }
            npc::LIGHT_UPDATE => {
                io::varint(r, out)?;
                io::varint(r, out)?;
                light_data(r, out)?;
            }
            npc::LOGIN => {
                io::bytes(r, out, 5)?;
                let n = io::len(r, 1024, 1)?;
                io::write_len(out, n);
                for _ in 0..n {
                    io::string(r, out)?;
                }
                for _ in 0..3 {
                    io::varint(r, out)?;
                }
                io::bytes(r, out, 3)?;
                spawn_info(r, out)?;
                out.extend_from_slice(r.take_rest());
            }
            npc::RESPAWN => {
                spawn_info(r, out)?;
                out.extend_from_slice(r.take_rest());
            }
            npc::ANIMATE => {
                io::varint(r, out)?;
                // 26.3: 0 wake up, 1 crit, 2 magic crit; 26.2: 2, 4, 5 (0 and 3 were swings).
                let action = match r.u8()? {
                    0 => 2,
                    1 => 4,
                    2 => 5,
                    _ => return Ok(false),
                };
                out.push(action);
            }
            npc::ENTITY_EVENT => {
                let head = r.take(4)?;
                let event = r.u8()?;
                // 71/72: boat sink and launch, new in 26.3.
                if event == 71 || event == 72 {
                    return Ok(false);
                }
                out.extend_from_slice(head);
                out.push(event);
            }
            npc::EXPLODE => {
                io::bytes(r, out, 24 + 4 + 4)?;
                if io::boolean(r, out)? {
                    io::bytes(r, out, 24)?;
                }
                let maps = self.maps();
                let t = tables(&maps);
                if !entity_data::particle(r, out, &t, &mut self.refs)? {
                    // No such particle in 26.2: the plain explosion.
                    write_varint(out, g::PARTICLE_TYPE.older(0).unwrap_or(0));
                }
                let mut sound_bytes = Vec::new();
                common::sound(&g::TABLES, r, &mut sound_bytes)?;
                let n = io::len(r, 1024, 2)?;
                let mut list = Vec::new();
                let mut kept = 0usize;
                for _ in 0..n {
                    let mut entry = Vec::new();
                    let ok = entity_data::particle(r, &mut entry, &t, &mut self.refs)?;
                    io::bytes(r, &mut entry, 8)?;
                    io::varint(r, &mut entry)?;
                    if ok {
                        list.extend_from_slice(&entry);
                        kept = kept.saturating_add(1);
                    }
                }
                let play_sound = r.u8()? != 0;
                if play_sound {
                    out.extend_from_slice(&sound_bytes);
                } else {
                    write_varint(
                        out,
                        g::OLD_SOUND_EVENT_INTENTIONALLY_EMPTY.saturating_add(1),
                    );
                }
                io::write_len(out, kept);
                out.extend_from_slice(&list);
            }
            npc::UPDATE_ADVANCEMENTS => self.advancements(r, out)?,
            npc::MERCHANT_OFFERS => {
                let maps = self.maps();
                io::varint(r, out)?;
                let n = io::len(r, 1024, 4)?;
                io::write_len(out, n);
                for _ in 0..n {
                    item_cost(r, out, &maps, &mut self.refs)?;
                    items::stack_to_old(r, out, &maps, &mut self.refs)?;
                    if io::boolean(r, out)? {
                        item_cost(r, out, &maps, &mut self.refs)?;
                    }
                    io::bytes(r, out, 1 + 4 * 6)?;
                }
                out.extend_from_slice(r.take_rest());
            }
            npc::RECIPE_BOOK_ADD => {
                let n = io::len(r, 1 << 16, 3)?;
                io::write_len(out, n);
                for _ in 0..n {
                    self.display_entry(r, out)?;
                    io::bytes(r, out, 1)?;
                }
                io::bytes(r, out, 1)?;
            }
            npc::UPDATE_RECIPES => {
                let n = io::len(r, 1 << 16, 2)?;
                io::write_len(out, n);
                for _ in 0..n {
                    io::string(r, out)?;
                    let m = io::len(r, 1 << 16, 1)?;
                    let mut ids = Vec::with_capacity(m);
                    for _ in 0..m {
                        if let Some(id) = g::ITEM.older(r.varint()?) {
                            ids.push(id);
                        }
                    }
                    io::write_len(out, ids.len());
                    for id in ids {
                        write_varint(out, id);
                    }
                }
                let n = io::len(r, 1 << 16, 2)?;
                io::write_len(out, n);
                for _ in 0..n {
                    item_set(r, out)?;
                    self.slot_display(r, out, SLOT_DEPTH)?;
                }
            }
            npc::PLACE_GHOST_RECIPE => {
                io::varint(r, out)?;
                self.recipe_display(r, out)?;
            }
            npc::CHUNKS_BIOMES => {
                let n = io::len(r, 1024, 9)?;
                io::write_len(out, n);
                for _ in 0..n {
                    io::bytes(r, out, 8)?;
                    let len = io::len(r, 2_097_152, 1)?;
                    let data = r.take(len)?;
                    let mut sections = Vec::with_capacity(len);
                    chunk::biome_sections(
                        data,
                        &mut sections,
                        self.biome_kind(),
                        &self.biome_map(),
                    )?;
                    io::write_len(out, sections.len());
                    out.extend_from_slice(&sections);
                }
            }
            npc::BLOCK_UPDATE => {
                io::bytes(r, out, 8)?;
                write_varint(out, block_state_to_old(r.varint()?));
            }
            npc::SECTION_BLOCKS_UPDATE => {
                io::bytes(r, out, 8)?;
                let n = io::len(r, 4096, 1)?;
                io::write_len(out, n);
                for _ in 0..n {
                    let v = r.varlong()?;
                    let state = i32::try_from(v >> 12).unwrap_or(0);
                    let packed = (i64::from(block_state_to_old(state)) << 12) | (v & 0xfff);
                    vanillite_protocol::codec::write_varlong(out, packed);
                }
            }
            npc::BLOCK_EVENT => {
                io::bytes(r, out, 10)?;
                write_varint(out, g::BLOCK.older(r.varint()?).unwrap_or(0));
            }
            npc::LEVEL_EVENT => {
                let mut event = r.i32()?;
                match event {
                    // Splash sounds (26.2 plays them with the particles), teleport trails and
                    // mining progress (26.2 clients draw their own): new in 26.3.
                    1053 | 1054 | 2015..=2020 => return Ok(false),
                    // Destroy particles without sound: the nearest 26.2 event.
                    2014 => event = 2001,
                    _ => {}
                }
                out.extend_from_slice(&event.to_be_bytes());
                io::bytes(r, out, 8)?;
                let data = r.i32()?;
                // 2001: block destroyed (particles and sound of a block state).
                let data = if event == 2001 {
                    block_state_to_old(data)
                } else {
                    data
                };
                out.extend_from_slice(&data.to_be_bytes());
                io::bytes(r, out, 1)?;
            }
            npc::ADD_ENTITY => {
                io::varint(r, out)?;
                io::bytes(r, out, 16)?;
                let kind = r.varint()?;
                let Some(old) = g::ENTITY_TYPE.older(kind) else {
                    // An entity type the client cannot show; later packets about it are ignored
                    // by the client.
                    return Ok(false);
                };
                write_varint(out, old);
                io::bytes(r, out, 24)?;
                common::lp_vec3(r, out)?;
                io::bytes(r, out, 3)?;
                let data = r.varint()?;
                let falling_block = g::NEW_ENTITY_TYPES
                    .get(usize::try_from(kind).unwrap_or(usize::MAX))
                    == Some(&"minecraft:falling_block");
                write_varint(
                    out,
                    if falling_block {
                        block_state_to_old(data)
                    } else {
                        data
                    },
                );
            }
            npc::MOVE_ENTITY_POS | npc::MOVE_ENTITY_POS_ROT => {
                // 26.3: properties (on ground, step count) and a VecDelta; 26.2: one delta.
                io::varint(r, out)?;
                let props = r.varint()?;
                let on_ground = props & 1 != 0;
                let steps = u32::from_ne_bytes(props.to_ne_bytes()) >> 1;
                let mut delta = [0i32; 3];
                let mut add = |r: &mut Reader<'_>| -> Result<()> {
                    for d in &mut delta {
                        *d = d.saturating_add(i32::from(r.i16()?));
                    }
                    Ok(())
                };
                if steps == 0 {
                    add(r)?;
                } else {
                    for _ in 0..steps.min(1024) {
                        r.varint()?;
                        add(r)?;
                    }
                }
                for d in delta {
                    let d = i16::try_from(d.clamp(i32::from(i16::MIN), i32::from(i16::MAX)))
                        .unwrap_or(0);
                    out.extend_from_slice(&d.to_be_bytes());
                }
                if id == npc::MOVE_ENTITY_POS_ROT {
                    io::bytes(r, out, 2)?;
                }
                out.push(u8::from(on_ground));
            }
            npc::MOVE_ENTITY_ROT => {
                // 26.3: id, on ground, yaw, pitch; 26.2: id, yaw, pitch, on ground.
                io::varint(r, out)?;
                let on_ground = r.u8()?;
                io::bytes(r, out, 2)?;
                out.push(on_ground);
            }
            npc::ENTITY_POSITION_SYNC => {
                // 26.3: a position path; 26.2: PositionMoveRotation (position, movement, rot).
                io::varint(r, out)?;
                let pos = match r.varint()? {
                    0 => r.take(24)?,
                    1 => {
                        let n = io::len(r, 1024, 25)?;
                        let mut last = r.rest().get(..24).ok_or(Error::UnexpectedEnd)?;
                        for _ in 0..n {
                            last = r.take(24)?;
                            r.varint()?;
                        }
                        last
                    }
                    _ => return Err(Error::Invalid("position path")),
                };
                out.extend_from_slice(pos);
                out.extend_from_slice(&[0; 24]);
                io::bytes(r, out, 9)?;
            }
            npc::PLAYER_POSITION => {
                let teleport = io::varint(r, out)?;
                let mut pos = [0.0f64; 3];
                for p in &mut pos {
                    *p = r.f64()?;
                    out.extend_from_slice(&p.to_be_bytes());
                }
                io::bytes(r, out, 24)?;
                let mut rot = [0.0f32; 2];
                for v in &mut rot {
                    *v = r.f32()?;
                    out.extend_from_slice(&v.to_be_bytes());
                }
                let relatives = r.i32()?;
                out.extend_from_slice(&relatives.to_be_bytes());
                let mut shared = self.shared.lock().unwrap_or_else(PoisonError::into_inner);
                if shared.teleports.len() >= 64 {
                    shared.teleports.remove(0);
                }
                shared.teleports.push((teleport, pos, rot, relatives));
            }
            npc::SET_ENTITY_DATA => {
                io::varint(r, out)?;
                entity_data::entity_data(r, out, &tables(&maps))?;
            }
            npc::SET_EQUIPMENT => {
                io::varint(r, out)?;
                loop {
                    let slot = r.u8()?;
                    out.push(slot);
                    items::stack_to_old(r, out, &maps, &mut self.refs)?;
                    if slot & 0x80 == 0 {
                        break;
                    }
                }
            }
            npc::CONTAINER_SET_CONTENT => {
                io::varint(r, out)?;
                io::varint(r, out)?;
                let n = io::len(r, 1024, 1)?;
                io::write_len(out, n);
                for _ in 0..n {
                    items::stack_to_old(r, out, &maps, &mut self.refs)?;
                }
                items::stack_to_old(r, out, &maps, &mut self.refs)?;
            }
            npc::CONTAINER_SET_SLOT => {
                io::varint(r, out)?;
                io::varint(r, out)?;
                io::bytes(r, out, 2)?;
                items::stack_to_old(r, out, &maps, &mut self.refs)?;
            }
            npc::SET_CURSOR_ITEM => items::stack_to_old(r, out, &maps, &mut self.refs)?,
            npc::SET_PLAYER_INVENTORY => {
                io::varint(r, out)?;
                items::stack_to_old(r, out, &maps, &mut self.refs)?;
            }
            npc::SOUND | npc::SOUND_ENTITY => {
                common::sound(&g::TABLES, r, out)?;
                out.extend_from_slice(r.take_rest());
            }
            npc::LEVEL_PARTICLES => return self.level_particles(r, out),
            npc::UPDATE_TAGS => self.regs.tags(&g::TABLES, r, out)?,
            npc::AWARD_STATS => common::award_stats(&g::TABLES, r, out)?,
            npc::COMMANDS => common::commands(&g::TABLES, r, out)?,
            _ => out.extend_from_slice(r.take_rest()),
        }
        Ok(true)
    }

    /// `level_particles`: 26.3 sends the particle first, per-axis speeds, a VarInt count and a
    /// randomization type; 26.2 the particle last, one speed and an int count.
    fn level_particles(&mut self, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<bool> {
        let maps = self.maps();
        let mut particle = Vec::new();
        if !entity_data::particle(r, &mut particle, &tables(&maps), &mut self.refs)? {
            return Ok(false);
        }
        let flags = r.take(2)?;
        let pos = r.take(24)?;
        let mut dist = [r.f32()?, r.f32()?, r.f32()?];
        let speed = [r.f32()?, r.f32()?, r.f32()?];
        let count = r.varint()?;
        r.varint()?;
        // Count 0: one particle moving by dist * speed per axis; exact with speed 1.
        let max_speed = if count == 0 {
            for (d, s) in dist.iter_mut().zip(speed) {
                *d *= s;
            }
            1.0
        } else {
            // Same speeds are exact; different ones (or the new spread modes) approximate.
            speed.iter().copied().fold(0.0f32, f32::max)
        };
        out.extend_from_slice(flags);
        out.extend_from_slice(pos);
        for d in dist {
            out.extend_from_slice(&d.to_be_bytes());
        }
        out.extend_from_slice(&max_speed.to_be_bytes());
        out.extend_from_slice(&count.to_be_bytes());
        out.extend_from_slice(&particle);
        Ok(true)
    }

    /// `update_advancements`: 26.3 moved the display position from the display info to the
    /// entry.
    fn advancements(&mut self, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<()> {
        let maps = self.maps();
        io::bytes(r, out, 1)?;
        let n = io::len(r, 1 << 16, 2)?;
        io::write_len(out, n);
        for _ in 0..n {
            io::string(r, out)?;
            if io::boolean(r, out)? {
                io::string(r, out)?;
            }
            let mut display = Vec::new();
            let has_display = io::boolean(r, &mut display)?;
            if has_display {
                io::nbt(r, &mut display)?;
                io::nbt(r, &mut display)?;
                items::template_to_old(r, &mut display, &maps, &mut self.refs)?;
                io::varint(r, &mut display)?;
                let flags = r.i32()?;
                display.extend_from_slice(&flags.to_be_bytes());
                if flags & 1 != 0 {
                    io::string(r, &mut display)?;
                }
            }
            let mut rest = Vec::new();
            // Requirements: a list of lists of strings; then the telemetry flag.
            let groups = io::len(r, 1 << 16, 1)?;
            io::write_len(&mut rest, groups);
            for _ in 0..groups {
                let k = io::len(r, 1 << 16, 1)?;
                io::write_len(&mut rest, k);
                for _ in 0..k {
                    io::string(r, &mut rest)?;
                }
            }
            io::bytes(r, &mut rest, 1)?;
            let position = r.take(8)?;
            out.extend_from_slice(&display);
            if has_display {
                out.extend_from_slice(position);
            }
            out.extend_from_slice(&rest);
        }
        out.extend_from_slice(r.take_rest());
        Ok(())
    }
}

/// Nesting limit of slot displays.
const SLOT_DEPTH: u8 = 16;

/// `minecraft:slot_display` ids (the same in 26.2 and 26.3).
mod slot {
    pub const ITEM: i32 = 4;
    pub const TAG: i32 = 6;
    pub const COMPOSITE: i32 = 10;
}

/// An item `HolderSet` (`Ingredient`): a tag name or a direct list of items.
fn item_set(r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<()> {
    let n = r.varint()?;
    let Some(n) = n.checked_sub(1) else {
        write_varint(out, 0);
        return io::string(r, out);
    };
    let n = usize::try_from(n).map_err(|_| Error::NegativeLength)?;
    if n > 1 << 16 || n > r.remaining() {
        return Err(Error::Invalid("item set"));
    }
    let mut ids = Vec::with_capacity(n);
    for _ in 0..n {
        if let Some(id) = g::ITEM.older(r.varint()?) {
            ids.push(id);
        }
    }
    io::write_len(out, ids.len().saturating_add(1));
    for id in ids {
        write_varint(out, id);
    }
    Ok(())
}

impl Clientbound {
    /// `SlotDisplay`: 26.3 sends `tag` as an item holder set, 26.2 as a tag name; a direct list
    /// becomes a `composite` of `item`s.
    fn slot_display(&mut self, r: &mut Reader<'_>, out: &mut Vec<u8>, depth: u8) -> Result<()> {
        let depth = depth
            .checked_sub(1)
            .ok_or(Error::Invalid("slot display depth"))?;
        let kind = r.varint()?;
        match kind {
            // empty, any_fuel
            0 | 1 => write_varint(out, kind),
            // with_any_potion
            2 => {
                write_varint(out, kind);
                self.slot_display(r, out, depth)?;
            }
            // only_with_component
            3 => {
                let mut inner = Vec::new();
                self.slot_display(r, &mut inner, depth)?;
                let component =
                    u16::try_from(r.varint()?).map_err(|_| Error::Invalid("component type"))?;
                match self.maps().component(component) {
                    Some(c) => {
                        write_varint(out, kind);
                        out.extend_from_slice(&inner);
                        write_varint(out, i32::from(c));
                    }
                    None => out.extend_from_slice(&inner),
                }
            }
            slot::ITEM => {
                write_varint(out, kind);
                write_varint(out, g::ITEM.older(r.varint()?).unwrap_or(1));
            }
            // item_stack
            5 => {
                write_varint(out, kind);
                let maps = self.maps();
                items::template_to_old(r, out, &maps, &mut self.refs)?;
            }
            slot::TAG => {
                let n = r.varint()?;
                let Some(n) = n.checked_sub(1) else {
                    write_varint(out, slot::TAG);
                    return io::string(r, out);
                };
                let n = usize::try_from(n).map_err(|_| Error::NegativeLength)?;
                if n > 1 << 16 || n > r.remaining() {
                    return Err(Error::Invalid("item set"));
                }
                let mut ids = Vec::with_capacity(n);
                for _ in 0..n {
                    if let Some(id) = g::ITEM.older(r.varint()?) {
                        ids.push(id);
                    }
                }
                write_varint(out, slot::COMPOSITE);
                io::write_len(out, ids.len());
                for id in ids {
                    write_varint(out, slot::ITEM);
                    write_varint(out, id);
                }
            }
            // dyed, with_remainder
            7 | 9 => {
                write_varint(out, kind);
                self.slot_display(r, out, depth)?;
                self.slot_display(r, out, depth)?;
            }
            // smithing_trim: base, material, `trim_pattern` holder
            8 => {
                let mut base = Vec::new();
                self.slot_display(r, &mut base, depth)?;
                let mut material = Vec::new();
                self.slot_display(r, &mut material, depth)?;
                let pattern = r
                    .varint()?
                    .checked_sub(1)
                    .filter(|i| *i >= 0)
                    .ok_or(Error::Invalid("inline trim pattern"))?;
                match self.regs.older("minecraft:trim_pattern", pattern) {
                    Some(p) => {
                        write_varint(out, kind);
                        out.extend_from_slice(&base);
                        out.extend_from_slice(&material);
                        write_varint(out, p.saturating_add(1));
                    }
                    None => out.extend_from_slice(&base),
                }
            }
            slot::COMPOSITE => {
                write_varint(out, kind);
                let n = io::len(r, 1 << 16, 1)?;
                io::write_len(out, n);
                for _ in 0..n {
                    self.slot_display(r, out, depth)?;
                }
            }
            _ => return Err(Error::Invalid("slot display type")),
        }
        Ok(())
    }

    fn slot_displays(&mut self, r: &mut Reader<'_>, out: &mut Vec<u8>, n: usize) -> Result<()> {
        for _ in 0..n {
            self.slot_display(r, out, SLOT_DEPTH)?;
        }
        Ok(())
    }

    /// `RecipeDisplay`: the same five types in 26.2 and 26.3.
    fn recipe_display(&mut self, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<()> {
        match io::varint(r, out)? {
            0 => {
                let n = io::len(r, 1 << 16, 1)?;
                io::write_len(out, n);
                self.slot_displays(r, out, n)?;
                self.slot_displays(r, out, 2)
            }
            1 => {
                io::varint(r, out)?;
                io::varint(r, out)?;
                let n = io::len(r, 1 << 16, 1)?;
                io::write_len(out, n);
                self.slot_displays(r, out, n)?;
                self.slot_displays(r, out, 2)
            }
            2 => {
                self.slot_displays(r, out, 4)?;
                io::varint(r, out)?;
                io::bytes(r, out, 4)
            }
            3 => self.slot_displays(r, out, 3),
            4 => self.slot_displays(r, out, 5),
            _ => Err(Error::Invalid("recipe display type")),
        }
    }

    /// `RecipeDisplayEntry`: id, display, group, book category, crafting requirements.
    fn display_entry(&mut self, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<()> {
        io::varint(r, out)?;
        self.recipe_display(r, out)?;
        io::varint(r, out)?;
        io::varint(r, out)?;
        if io::boolean(r, out)? {
            let n = io::len(r, 1 << 16, 1)?;
            io::write_len(out, n);
            for _ in 0..n {
                item_set(r, out)?;
            }
        }
        Ok(())
    }
}

/// Light data: the four section masks are byte arrays in 26.3 and long arrays in 26.2.
fn light_data(r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<()> {
    for _ in 0..4 {
        let n = io::len(r, 1 << 16, 1)?;
        let bytes = r.take(n)?;
        io::write_len(out, n.div_ceil(8));
        for chunk in bytes.chunks(8) {
            let mut word = [0u8; 8];
            for (w, b) in word.iter_mut().zip(chunk) {
                *w = *b;
            }
            out.extend_from_slice(&u64::from_le_bytes(word).to_be_bytes());
        }
    }
    out.extend_from_slice(r.take_rest());
    Ok(())
}

/// `CommonPlayerSpawnInfo`: the previous game mode was a byte (-1 none) in 26.2 and is an
/// optional VarInt (0 none, else id + 1) in 26.3; the game mode a VarInt of one byte.
fn spawn_info(r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<()> {
    io::varint(r, out)?;
    io::string(r, out)?;
    io::bytes(r, out, 8)?;
    let mode = r.varint()?;
    out.push(u8::try_from(mode).map_err(|_| Error::Invalid("game mode"))?);
    let previous = r.varint()?;
    let previous = if previous == 0 {
        -1i8
    } else {
        i8::try_from(previous.saturating_sub(1)).map_err(|_| Error::Invalid("game mode"))?
    };
    out.extend_from_slice(&previous.to_be_bytes());
    Ok(())
}

/// `ItemCost`: item id, count, exact component predicate.
fn item_cost(
    r: &mut Reader<'_>,
    out: &mut Vec<u8>,
    maps: &dyn Maps,
    refs: &mut Vec<Ref>,
) -> Result<()> {
    write_varint(out, g::ITEM.older(r.varint()?).unwrap_or(1));
    io::varint(r, out)?;
    items::exact_to_old(r, out, maps, refs)
}

#[derive(Debug)]
struct Serverbound {
    shared: Arc<Mutex<Shared>>,
    /// The client's last reported position and rotation (for teleport confirmations).
    pos: [f64; 3],
    rot: [f32; 2],
    /// The client interacted or dropped an item in this tick: its swing is not a punch.
    interacted: bool,
}

impl ServerboundStep for Serverbound {
    fn translate(
        &mut self,
        state: State,
        body: &[u8],
        out: &mut Vec<u8>,
        _extra: &mut Vec<Vec<u8>>,
    ) -> Result<bool> {
        let Some(table) = packets(state, false) else {
            out.extend_from_slice(body);
            return Ok(true);
        };
        let mut r = Reader::new(body);
        let id = r.varint()?;
        if state == State::Play && id == ops::SWING {
            // 26.3 clients send `punch` only for attack swings of the main hand; the server
            // shows interaction swings itself.
            if r.varint()? != 0 || self.interacted {
                return Ok(false);
            }
            write_varint(out, nps::PUNCH);
            return Ok(true);
        }
        let new_id = table
            .newer(id)
            .ok_or(Error::Invalid("packet unknown to the server's version"))?;
        write_varint(out, new_id);
        match state {
            State::Configuration if id == ocs::SELECT_KNOWN_PACKS => {
                common::known_packs(&mut r, out, g::PROTOCOL, g::NEXT_PROTOCOL)?;
            }
            State::Play => return self.play(id, &mut r, out),
            _ => out.extend_from_slice(r.take_rest()),
        }
        Ok(true)
    }
}

impl Serverbound {
    fn play(&mut self, id: i32, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<bool> {
        match id {
            ops::CLIENT_TICK_END => self.interacted = false,
            ops::USE_ITEM | ops::USE_ITEM_ON => self.interacted = true,
            ops::INTERACT => {
                // Entity id, then the action: 1 is an attack (its swing is a punch).
                let mut peek = r.clone();
                peek.varint()?;
                if peek.varint()? != 1 {
                    self.interacted = true;
                }
            }
            _ => {}
        }
        match id {
            ops::PLAYER_ACTION => {
                // 26.3 inserted CHANGE_DESTROY_DIRECTION at 1.
                let action = r.varint()?;
                if action == 3 || action == 4 {
                    self.interacted = true;
                }
                write_varint(
                    out,
                    if action == 0 {
                        0
                    } else {
                        action.saturating_add(1)
                    },
                );
                out.extend_from_slice(r.take_rest());
            }
            ops::SIGN_UPDATE => {
                // 26.2: position, front flag, 4 lines; 26.3: position, 4 lines, slot.
                io::bytes(r, out, 8)?;
                let front = r.u8()?;
                for _ in 0..4 {
                    io::string(r, out)?;
                }
                out.push(front);
            }
            ops::MOVE_PLAYER_POS | ops::MOVE_PLAYER_POS_ROT => {
                for p in &mut self.pos {
                    *p = r.f64()?;
                    out.extend_from_slice(&p.to_be_bytes());
                }
                if id == ops::MOVE_PLAYER_POS_ROT {
                    self.rotation(r, out)?;
                }
                out.extend_from_slice(r.take_rest());
            }
            ops::MOVE_PLAYER_ROT => {
                self.rotation(r, out)?;
                out.extend_from_slice(r.take_rest());
            }
            ops::ACCEPT_TELEPORTATION => {
                // 26.3 also wants the position and rotation the client reached.
                let teleport = io::varint(r, out)?;
                let found = {
                    let mut shared = self.shared.lock().unwrap_or_else(PoisonError::into_inner);
                    let i = shared.teleports.iter().position(|t| t.0 == teleport);
                    i.map(|i| shared.teleports.remove(i))
                };
                let (pos, rot) = match found {
                    Some((_, pos, rot, rel)) => {
                        let mut p = pos;
                        for (i, v) in p.iter_mut().enumerate() {
                            if rel & (1 << i) != 0 {
                                *v += self.pos.get(i).copied().unwrap_or(0.0);
                            }
                        }
                        let mut o = rot;
                        // Relative bits 3 (y rot) and 4 (x rot).
                        if rel & (1 << 3) != 0 {
                            o[0] += self.rot[0];
                        }
                        if rel & (1 << 4) != 0 {
                            o[1] += self.rot[1];
                        }
                        (p, o)
                    }
                    None => (self.pos, self.rot),
                };
                self.pos = pos;
                self.rot = rot;
                for v in pos {
                    out.extend_from_slice(&v.to_be_bytes());
                }
                for v in rot {
                    out.extend_from_slice(&v.to_be_bytes());
                }
            }
            ops::SET_CREATIVE_MODE_SLOT => {
                io::bytes(r, out, 2)?;
                items::untrusted_stack_to_new(r, out, &g::ITEM, &component_to_new)?;
            }
            _ => out.extend_from_slice(r.take_rest()),
        }
        Ok(true)
    }

    fn rotation(&mut self, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<()> {
        for v in &mut self.rot {
            *v = r.f32()?;
            out.extend_from_slice(&v.to_be_bytes());
        }
        Ok(())
    }
}

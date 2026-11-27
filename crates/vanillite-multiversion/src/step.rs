//! The step code shared by every step below the newest one (1.20.5 … 26.1 ↔ their successor).
//!
//! Ids come from the step's generated [`Tables`]. The wire formats are this module's knowledge:
//! each packet whose format changed somewhere between 1.20.5 and 26.2 is read in the format of
//! the step's newer version and written in the format of its older version (the `V*` protocol
//! numbers mark where a format changed). Packets that only carry registry ids get them
//! rewritten; everything else only gets its packet id rewritten. Packets whose Java shape
//! changed in the step ([`Tables::changed`]) and that are not handled here are dropped rather
//! than sent in a form the client cannot read.

use std::sync::{Arc, Mutex, PoisonError};

use crate::chain::{ClientboundStep, Ctx, ServerboundStep};
use crate::chunk::{self, Kind};
use crate::common::{self, Registries};
use crate::generated::{Cb, Sb};
use crate::io;
use crate::portable::Items;
use crate::recipe::{self, OldBook};
use crate::table::{PacketTable, Tables};
use crate::{Result, State};
use vanillite_protocol::Error;
use vanillite_protocol::codec::{Reader, write_str, write_varint, write_varlong};

/// 1.21 (enchantments, paintings and chat types synchronised; attribute modifiers by id).
pub(crate) const V1_21: i32 = 767;
/// 1.21.2 (container ids as VarInt, new player positions and input, recipe book, sea level).
pub(crate) const V1_21_2: i32 = 768;
/// 1.21.4 (particles always shown, held slot as VarInt, player loaded, model data lists).
pub(crate) const V1_21_4: i32 = 769;
/// 1.21.5 (hashed stacks, heightmap lists, palettes without length, team enums, chat index).
pub(crate) const V1_21_5: i32 = 770;
/// 1.21.6 (difficulty as VarInt, shift through player input only).
pub(crate) const V1_21_6: i32 = 771;
/// 1.21.9 (velocities as `LpVec3`, respawn data, explosion particles).
pub(crate) const V1_21_9: i32 = 773;
/// 26.1 (fluid counts in sections, world clocks, attack packet, item templates).
pub(crate) const V26_1: i32 = 775;
/// 26.2 (online mode, session id, team colours).
pub(crate) const V26_2: i32 = 776;

/// Teleport id of the teleports a step makes up for rotations (`player_rotation` before 1.21.2,
/// relative ones before 1.21.9). Vanilla counts its ids up from 0, so this one is never the
/// server's; the client's acceptance of it is not passed on.
const SYNTHETIC_TELEPORT: i32 = -0x564c;

/// The id of the newest version's item `name` in the version of protocol `protocol`, through
/// every step's item table.
fn newest_item_to(protocol: i32, name: &str) -> Option<i32> {
    let name = name.strip_prefix("minecraft:").unwrap_or(name);
    let id = vanillite_data::registry::ITEM.id(&format!("minecraft:{name}"))?;
    let mut id = crate::generated::v26_2::ITEM.older(i32::from(id))?;
    for t in crate::STEPS.iter().rev().filter(|t| t.protocol >= protocol) {
        id = t.older("minecraft:item", id)?;
    }
    Some(id)
}

/// The newest version's id of entry `id` of static registry `registry` in the numbering of the
/// version of protocol `protocol`.
pub(crate) fn to_newest(registry: &str, protocol: i32, id: i32) -> Option<u16> {
    let mut id = id;
    for t in crate::STEPS.iter().filter(|t| t.protocol >= protocol) {
        id = t.newer(registry, id)?;
    }
    u16::try_from(crate::generated::v26_2::TABLES.newer(registry, id)?).ok()
}

/// Opens both halves of a step for one connection.
pub(crate) fn open(t: &'static Tables) -> (Box<dyn ClientboundStep>, Box<dyn ServerboundStep>) {
    let shared = Arc::new(Mutex::new(Registries::default()));
    (
        Box::new(Clientbound {
            t,
            regs: Arc::clone(&shared),
            book: OldBook::default(),
        }),
        Box::new(Serverbound {
            t,
            regs: shared,
            rot: [0.0; 2],
            input: 0,
            interact_at: None,
        }),
    )
}

fn packets(t: &Tables, state: State, clientbound: bool) -> Option<&'static PacketTable> {
    let i: usize = match state {
        State::Login => 0,
        State::Configuration => 2,
        State::Play => 4,
        State::Handshake | State::Status => return None,
    };
    t.packets
        .get(i.saturating_add(usize::from(!clientbound)))
        .copied()
}

fn changed(t: &Tables, state: State, clientbound: bool) -> &'static [i32] {
    let i: usize = match state {
        State::Login => 0,
        State::Configuration => 2,
        State::Play => 4,
        State::Handshake | State::Status => return &[],
    };
    t.changed
        .get(i.saturating_add(usize::from(!clientbound)))
        .copied()
        .unwrap_or(&[])
}

/// Block state sizes of both versions.
fn block_kind(t: &Tables) -> Kind {
    Kind {
        entries: 4096,
        max_indirect: 8,
        global_new: chunk::global_bits(t.block_state.to_old.len()),
        global_old: chunk::global_bits(t.block_state.to_new.len()),
    }
}

#[derive(Debug)]
struct Clientbound {
    t: &'static Tables,
    /// Shared with the serverbound half (creative items name synchronised entries).
    regs: Arc<Mutex<Registries>>,
    /// The recipes of a client before 1.21.2 (step 1.21.1 ↔ 1.21.2 only).
    book: OldBook,
}

impl ClientboundStep for Clientbound {
    fn translate(
        &mut self,
        state: State,
        body: &[u8],
        out: &mut Vec<u8>,
        ctx: &Ctx<'_>,
    ) -> Result<bool> {
        let t = self.t;
        let Some(table) = packets(t, state, true) else {
            out.extend_from_slice(body);
            return Ok(true);
        };
        let mut r = Reader::new(body);
        let id = r.varint()?;
        let mut regs = self.regs.lock().unwrap_or_else(PoisonError::into_inner);
        let mut c = Cx {
            t,
            regs: &mut regs,
            ctx,
            state,
            book: &mut self.book,
        };
        if state == State::Login {
            let Some(old) = table.older(id) else {
                return Ok(false);
            };
            write_varint(out, old);
            return c.login(id, &mut r, out);
        }
        let kind = t
            .clientbound(state, id)
            .ok_or(Error::Invalid("packet id"))?;
        // Packets the older version only has in another form.
        if state == State::Play
            && let Some(done) = c.converted(kind, &mut r, out)?
        {
            return Ok(done);
        }
        let Some(old) = table.older(id) else {
            return Ok(false);
        };
        write_varint(out, old);
        let handled = match state {
            State::Configuration => c.configuration(kind, &mut r, out)?,
            _ => c.play(kind, &mut r, out)?,
        };
        match handled {
            Some(keep) => Ok(keep),
            None if changed(t, state, true).contains(&id) => Ok(false),
            None => {
                out.extend_from_slice(r.take_rest());
                Ok(true)
            }
        }
    }

    fn stateless(&self, state: State, id: i32) -> bool {
        // Only packets that change the connection's registries depend on more than the body.
        state == State::Play
            && !matches!(
                self.t.clientbound(state, id),
                Some(
                    Cb::Login
                        | Cb::Respawn
                        | Cb::UpdateTags
                        | Cb::StartConfiguration
                        | Cb::UpdateRecipes
                        | Cb::RecipeBookAdd
                        | Cb::RecipeBookRemove
                        | Cb::RecipeBookSettings
                        | Cb::PlaceGhostRecipe
                ) | None
            )
    }

    fn entity_type_older(&self, new: i32) -> Option<i32> {
        self.t.older("minecraft:entity_type", new)
    }
}

/// One clientbound translation.
struct Cx<'a, 'c> {
    t: &'static Tables,
    regs: &'a mut Registries,
    ctx: &'a Ctx<'c>,
    state: State,
    book: &'a mut OldBook,
}

impl Cx<'_, '_> {
    fn next(&self) -> i32 {
        self.t.next_protocol
    }

    fn old(&self) -> i32 {
        self.t.protocol
    }

    /// Whether the format changed at `v` in this step.
    fn at(&self, v: i32) -> bool {
        self.t.next_protocol >= v && self.t.protocol < v
    }

    fn items(&self) -> Items<'_> {
        Items {
            t: self.t,
            regs: self.regs,
            clientbound: true,
            depth: 0,
        }
    }

    fn old_id(&self, kind: Cb) -> Result<i32> {
        self.t
            .old_clientbound(self.state, kind)
            .ok_or(Error::Invalid("packet the older version lacks"))
    }

    fn block_state(&self, id: i32) -> i32 {
        self.t.block_state.older(id).unwrap_or(0)
    }

    fn login(&mut self, id: i32, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<bool> {
        // 2: `login_finished` (`game_profile` before 1.21.2): profile, then a strict error
        // handling flag (1.20.5–1.21.1) and the session id (26.2).
        if id == 2 {
            io::bytes(r, out, 16)?;
            io::string(r, out)?;
            let n = io::len(r, 16, 2)?;
            io::write_len(out, n);
            for _ in 0..n {
                io::string(r, out)?;
                io::string(r, out)?;
                if io::boolean(r, out)? {
                    io::string(r, out)?;
                }
            }
            if self.next() < V1_21_2 {
                io::bytes(r, out, 1)?;
            } else if self.old() < V1_21_2 {
                out.push(0);
            }
            if self.at(V26_2) {
                r.skip(16)?;
            }
            out.extend_from_slice(r.take_rest());
            return Ok(true);
        }
        out.extend_from_slice(r.take_rest());
        Ok(true)
    }

    fn configuration(
        &mut self,
        kind: Cb,
        r: &mut Reader<'_>,
        out: &mut Vec<u8>,
    ) -> Result<Option<bool>> {
        match kind {
            Cb::SelectKnownPacks => {
                common::known_packs(r, out, self.t.next_protocol, self.t.protocol)?
            }
            Cb::RegistryData => return self.regs.registry_data(self.t, r, out).map(Some),
            Cb::UpdateTags => self.regs.tags(self.t, r, out)?,
            _ => return Ok(None),
        }
        Ok(Some(true))
    }

    /// Packets of the newer version that the older one receives as another packet. `None`:
    /// not one of them.
    fn converted(
        &mut self,
        kind: Cb,
        r: &mut Reader<'_>,
        out: &mut Vec<u8>,
    ) -> Result<Option<bool>> {
        match kind {
            Cb::UpdateRecipes if self.at(V1_21_2) => {
                write_varint(out, self.old_id(Cb::UpdateRecipes)?);
                let items = Items {
                    t: self.t,
                    regs: self.regs,
                    clientbound: true,
                    depth: 0,
                };
                self.book.update_recipes(&items, r, out)?;
                self.regs.cutter.recipes = std::mem::take(&mut self.book.cutter);
            }
            Cb::RecipeBookAdd if self.at(V1_21_2) => {
                write_varint(out, self.old_id(Cb::UpdateRecipes)?);
                let mut unlock = Vec::new();
                write_varint(&mut unlock, self.old_id(Cb::Recipe)?);
                let items = Items {
                    t: self.t,
                    regs: self.regs,
                    clientbound: true,
                    depth: 0,
                };
                self.book.add(&items, r, out, &mut unlock)?;
                self.ctx.emit(unlock);
            }
            Cb::RecipeBookRemove if self.at(V1_21_2) => {
                write_varint(out, self.old_id(Cb::Recipe)?);
                self.book.remove(r, out)?;
            }
            Cb::RecipeBookSettings if self.at(V1_21_2) => {
                write_varint(out, self.old_id(Cb::Recipe)?);
                self.book.settings(r, out)?;
            }
            Cb::PlaceGhostRecipe if self.at(V1_21_2) => {
                write_varint(out, self.old_id(Cb::PlaceGhostRecipe)?);
                return self.book.ghost(r, out).map(Some);
            }
            Cb::SetCursorItem if self.old() < V1_21_2 => {
                // `container_set_slot` of container -1, slot -1.
                write_varint(out, self.old_id(Cb::ContainerSetSlot)?);
                out.push(0xff);
                write_varint(out, 0);
                out.extend_from_slice(&(-1i16).to_be_bytes());
                self.items().stack(r, out)?;
            }
            Cb::SetPlayerInventory if self.old() < V1_21_2 => {
                // `container_set_slot` of container -2: a player inventory slot.
                write_varint(out, self.old_id(Cb::ContainerSetSlot)?);
                let slot = r.varint()?;
                out.push(0xfe);
                write_varint(out, 0);
                let slot = i16::try_from(slot).map_err(|_| Error::Invalid("slot"))?;
                out.extend_from_slice(&slot.to_be_bytes());
                self.items().stack(r, out)?;
            }
            Cb::TeleportEntity if self.at(V1_21_2) => {
                return self.teleport_entity(r, out).map(Some);
            }
            Cb::PlayerRotation if self.at(V1_21_2) => {
                // No such packet before 1.21.2: a teleport by zero blocks with the rotation.
                write_varint(out, self.old_id(Cb::PlayerPosition)?);
                out.extend_from_slice(&[0; 24]);
                io::bytes(r, out, 8)?;
                out.push(7);
                write_varint(out, SYNTHETIC_TELEPORT);
            }
            Cb::PlayerRotation if self.at(V1_21_9) => {
                let rot = r.take(8)?;
                let relative = [r.u8()? != 0, r.u8()? != 0];
                if relative == [false, false] {
                    write_varint(out, self.old_id(Cb::PlayerRotation)?);
                    out.extend_from_slice(rot);
                    return Ok(Some(true));
                }
                // Relative rotations (1.21.9): a teleport by zero blocks and zero velocity.
                write_varint(out, self.old_id(Cb::PlayerPosition)?);
                write_varint(out, SYNTHETIC_TELEPORT);
                out.extend_from_slice(&[0; 48]);
                out.extend_from_slice(rot);
                let flags =
                    0b1110_0111 | (i32::from(relative[0]) << 3) | (i32::from(relative[1]) << 4);
                out.extend_from_slice(&flags.to_be_bytes());
            }
            Cb::Cooldown if self.at(V1_21_2) => {
                // Cooldown groups (1.21.2) → items: the default group is the item's name.
                let group = r.string(io::MAX_STRING)?;
                let Some(item) = newest_item_to(self.t.protocol, group) else {
                    return Ok(Some(false));
                };
                write_varint(out, self.old_id(Cb::Cooldown)?);
                write_varint(out, item);
                io::varint(r, out)?;
            }
            Cb::EntityPositionSync if self.old() < V1_21_2 => {
                write_varint(out, self.old_id(Cb::TeleportEntity)?);
                io::varint(r, out)?;
                let pmr = r.take(48)?;
                let rot = [r.f32()?, r.f32()?];
                let on_ground = r.u8()?;
                out.extend_from_slice(pmr.get(..24).ok_or(Error::UnexpectedEnd)?);
                out.push(angle(rot[0]));
                out.push(angle(rot[1]));
                out.push(on_ground);
            }
            Cb::AddEntity if self.old() < V1_21_5 => {
                // Experience orbs were added with their own packet before 1.21.5.
                let mut peek = r.clone();
                peek.varint()?;
                peek.skip(16)?;
                let kind = peek.varint()?;
                let orb = self
                    .t
                    .new_entity_types
                    .get(usize::try_from(kind).unwrap_or(usize::MAX))
                    == Some(&"minecraft:experience_orb");
                if !orb {
                    return Ok(None);
                }
                write_varint(out, self.old_id(Cb::AddExperienceOrb)?);
                io::varint(r, out)?;
                r.skip(16)?;
                r.varint()?;
                io::bytes(r, out, 24)?;
                out.extend_from_slice(&1i16.to_be_bytes());
            }
            _ => return Ok(None),
        }
        Ok(Some(true))
    }

    /// The play packets this step rewrites. `None`: copy the rest unless the shape changed.
    #[allow(clippy::too_many_lines)]
    fn play(&mut self, kind: Cb, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<Option<bool>> {
        let t = self.t;
        match kind {
            Cb::LevelChunkWithLight => {
                io::bytes(r, out, 8)?;
                self.heightmaps(r, out)?;
                let len = io::len(r, 2_097_152, 1)?;
                let data = r.take(len)?;
                let mut sections = Vec::with_capacity(len.saturating_add(64));
                self.sections(data, &mut sections, false)?;
                io::write_len(out, sections.len());
                out.extend_from_slice(&sections);
                let n = io::len(r, 1 << 16, 4)?;
                io::write_len(out, n);
                for _ in 0..n {
                    io::bytes(r, out, 3)?;
                    let kind = r.varint()?;
                    write_varint(
                        out,
                        t.older("minecraft:block_entity_type", kind).unwrap_or(0),
                    );
                    io::nbt(r, out)?;
                }
                out.extend_from_slice(r.take_rest());
            }
            Cb::ChunksBiomes => {
                let n = io::len(r, 1024, 9)?;
                io::write_len(out, n);
                for _ in 0..n {
                    io::bytes(r, out, 8)?;
                    let len = io::len(r, 2_097_152, 1)?;
                    let data = r.take(len)?;
                    let mut sections = Vec::with_capacity(len.saturating_add(16));
                    self.sections(data, &mut sections, true)?;
                    io::write_len(out, sections.len());
                    out.extend_from_slice(&sections);
                }
            }
            Cb::Login => {
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
                self.spawn_info(r, out)?;
                if self.at(V26_2) {
                    r.skip(1)?;
                }
                out.extend_from_slice(r.take_rest());
            }
            Cb::Respawn => {
                self.spawn_info(r, out)?;
                out.extend_from_slice(r.take_rest());
            }
            Cb::AddEntity => {
                io::varint(r, out)?;
                io::bytes(r, out, 16)?;
                let kind = r.varint()?;
                let Some(old) = t.older("minecraft:entity_type", kind) else {
                    return Ok(Some(false));
                };
                write_varint(out, old);
                io::bytes(r, out, 24)?;
                // Velocity: an `LpVec3` after the position from 1.21.9 on, three shorts at the
                // end before.
                let mut velocity = None;
                if self.next() >= V1_21_9 {
                    let start = r.rest();
                    let v = common::read_lp_vec3(r)?;
                    if self.old() >= V1_21_9 {
                        let len = start.len().saturating_sub(r.remaining());
                        out.extend_from_slice(start.get(..len).ok_or(Error::UnexpectedEnd)?);
                    } else {
                        velocity = Some(v);
                    }
                }
                io::bytes(r, out, 3)?;
                let data = r.varint()?;
                let falling_block = t
                    .new_entity_types
                    .get(usize::try_from(kind).unwrap_or(usize::MAX))
                    == Some(&"minecraft:falling_block");
                write_varint(
                    out,
                    if falling_block {
                        self.block_state(data)
                    } else {
                        data
                    },
                );
                match velocity {
                    Some(v) => common::write_velocity_shorts(out, v),
                    None => out.extend_from_slice(r.take_rest()),
                }
            }
            Cb::SetEntityMotion => {
                io::varint(r, out)?;
                if self.at(V1_21_9) {
                    let v = common::read_lp_vec3(r)?;
                    common::write_velocity_shorts(out, v);
                } else {
                    out.extend_from_slice(r.take_rest());
                }
            }
            Cb::PlayerPosition if self.at(V1_21_2) => {
                // Teleport id, position, movement, rotation, relative flags → position,
                // rotation, flags byte (movement deltas did not exist), teleport id.
                let teleport = r.varint()?;
                io::bytes(r, out, 24)?;
                r.skip(24)?;
                io::bytes(r, out, 8)?;
                let relatives = r.i32()?;
                out.push(u8::try_from(relatives & 0x1f).unwrap_or(0));
                write_varint(out, teleport);
            }
            Cb::SetEntityData => {
                let entity = io::varint(r, out)?;
                let Some(kind) = self.ctx.entity_type(entity) else {
                    return Ok(Some(false));
                };
                if !self.entity_data(kind, r, out)? {
                    return Ok(Some(false));
                }
            }
            Cb::SetEquipment => {
                io::varint(r, out)?;
                loop {
                    let slot = r.u8()?;
                    out.push(slot);
                    self.items().stack(r, out)?;
                    if slot & 0x80 == 0 {
                        break;
                    }
                }
            }
            Cb::ContainerSetContent => {
                let cutter = self.cutter_open(r)?;
                self.container_id(r, out)?;
                io::varint(r, out)?;
                let n = io::len(r, 1024, 1)?;
                io::write_len(out, n);
                for i in 0..n {
                    let at = out.len();
                    self.items().stack(r, out)?;
                    if cutter && i == 0 {
                        self.regs
                            .cutter
                            .set_input(out.get(at..).unwrap_or_default());
                    }
                }
                self.items().stack(r, out)?;
            }
            Cb::ContainerSetSlot => {
                let cutter = self.cutter_open(r)?;
                self.container_id(r, out)?;
                io::varint(r, out)?;
                let slot = r.clone().take(2)?;
                io::bytes(r, out, 2)?;
                let at = out.len();
                self.items().stack(r, out)?;
                if cutter && slot == [0, 0] {
                    self.regs
                        .cutter
                        .set_input(out.get(at..).unwrap_or_default());
                }
            }
            Cb::ContainerClose
            | Cb::ContainerSetData
            | Cb::HorseScreenOpen
            | Cb::MountScreenOpen => {
                if kind == Cb::ContainerClose && self.at(V1_21_2) {
                    self.regs.cutter.container = None;
                }
                self.container_id(r, out)?;
                out.extend_from_slice(r.take_rest());
            }
            Cb::OpenScreen => {
                let container = io::varint(r, out)?;
                let menu = r.varint()?;
                if self.at(V1_21_2) {
                    let newest = to_newest("minecraft:menu", self.next(), menu);
                    let cutter = newest.and_then(|m| vanillite_data::registry::MENU.name(m));
                    self.regs.cutter.container =
                        (cutter == Some("minecraft:stonecutter")).then_some(container);
                    self.regs.cutter.input = None;
                }
                let Some(menu) = t.older("minecraft:menu", menu) else {
                    return Ok(Some(false));
                };
                write_varint(out, menu);
                out.extend_from_slice(r.take_rest());
            }
            Cb::MerchantOffers => {
                io::varint(r, out)?;
                let n = io::len(r, 1024, 4)?;
                io::write_len(out, n);
                for _ in 0..n {
                    self.item_cost(r, out)?;
                    self.items().stack(r, out)?;
                    if io::boolean(r, out)? {
                        self.item_cost(r, out)?;
                    }
                    io::bytes(r, out, 1 + 4 * 6)?;
                }
                out.extend_from_slice(r.take_rest());
            }
            Cb::SetHeldSlot | Cb::SetCarriedItem if self.at(V1_21_4) => {
                let slot = r.varint()?;
                out.push(u8::try_from(slot).map_err(|_| Error::Invalid("slot"))?);
            }
            Cb::UpdateAttributes => {
                io::varint(r, out)?;
                let n = io::len(r, 1024, 10)?;
                let mut list = Vec::new();
                let mut kept = 0usize;
                for _ in 0..n {
                    let attribute = r.varint()?;
                    let mut entry = Vec::new();
                    io::bytes(r, &mut entry, 8)?;
                    let m = io::len(r, 1024, 10)?;
                    io::write_len(&mut entry, m);
                    for _ in 0..m {
                        if self.at(V1_21) {
                            let id = r.string(io::MAX_STRING)?;
                            entry.extend_from_slice(&modifier_uuid(id));
                        } else if self.next() >= V1_21 {
                            io::string(r, &mut entry)?;
                        } else {
                            io::bytes(r, &mut entry, 16)?;
                        }
                        io::bytes(r, &mut entry, 8)?;
                        io::varint(r, &mut entry)?;
                    }
                    if let Some(old) = t.older("minecraft:attribute", attribute) {
                        write_varint(&mut list, old);
                        list.extend_from_slice(&entry);
                        kept = kept.saturating_add(1);
                    }
                }
                io::write_len(out, kept);
                out.extend_from_slice(&list);
            }
            Cb::UpdateMobEffect | Cb::RemoveMobEffect => {
                io::varint(r, out)?;
                let effect = r.varint()?;
                let Some(effect) = t.older("minecraft:mob_effect", effect) else {
                    return Ok(Some(false));
                };
                write_varint(out, effect);
                out.extend_from_slice(r.take_rest());
            }
            Cb::Sound | Cb::SoundEntity => {
                common::sound(t, r, out)?;
                out.extend_from_slice(r.take_rest());
            }
            Cb::LevelParticles => {
                io::bytes(r, out, 1)?;
                if self.next() >= V1_21_4 {
                    let always = r.u8()?;
                    if self.old() >= V1_21_4 {
                        out.push(always);
                    }
                }
                io::bytes(r, out, 24 + 16 + 4)?;
                if !self.particle(r, out)? {
                    return Ok(Some(false));
                }
            }
            Cb::Explode => return self.explode(r, out).map(Some),
            Cb::LevelEvent => {
                let event = r.i32()?;
                out.extend_from_slice(&event.to_be_bytes());
                io::bytes(r, out, 8)?;
                let data = r.i32()?;
                let data = if event == 2001 {
                    self.block_state(data)
                } else {
                    data
                };
                out.extend_from_slice(&data.to_be_bytes());
                io::bytes(r, out, 1)?;
            }
            Cb::BlockUpdate => {
                io::bytes(r, out, 8)?;
                write_varint(out, self.block_state(r.varint()?));
            }
            Cb::SectionBlocksUpdate => {
                io::bytes(r, out, 8)?;
                let n = io::len(r, 4096, 1)?;
                io::write_len(out, n);
                for _ in 0..n {
                    let v = r.varlong()?;
                    let state = i32::try_from(v >> 12).unwrap_or(0);
                    write_varlong(
                        out,
                        (i64::from(self.block_state(state)) << 12) | (v & 0xfff),
                    );
                }
            }
            Cb::BlockEvent => {
                io::bytes(r, out, 10)?;
                write_varint(out, t.older("minecraft:block", r.varint()?).unwrap_or(0));
            }
            Cb::BlockEntityData => {
                io::bytes(r, out, 8)?;
                let kind = r.varint()?;
                let Some(kind) = t.older("minecraft:block_entity_type", kind) else {
                    return Ok(Some(false));
                };
                write_varint(out, kind);
                out.extend_from_slice(r.take_rest());
            }
            Cb::UpdateTags => self.regs.tags(t, r, out)?,
            Cb::AwardStats => common::award_stats(t, r, out)?,
            Cb::Commands => common::commands(t, r, out)?,
            Cb::SetDefaultSpawnPosition if self.at(V1_21_9) => {
                // Respawn data: dimension, position, yaw, pitch → position, angle.
                r.string(io::MAX_STRING)?;
                io::bytes(r, out, 12)?;
            }
            Cb::SetTime => return self.set_time(r, out).map(Some),
            Cb::PlayerInfoUpdate => self.player_info(r, out)?,
            Cb::PlayerChat => return self.player_chat(r, out).map(Some),
            Cb::DisguisedChat => {
                io::component(r, out, self.at(V1_21_5))?;
                if !self.chat_type(r, out)? {
                    return Ok(Some(false));
                }
                self.chat_names(r, out)?;
            }
            Cb::SystemChat | Cb::SetTitleText | Cb::SetSubtitleText | Cb::SetActionBarText
                if self.at(V1_21_5) =>
            {
                io::component(r, out, true)?;
                out.extend_from_slice(r.take_rest());
            }
            Cb::TabList if self.at(V1_21_5) => {
                io::component(r, out, true)?;
                io::component(r, out, true)?;
            }
            Cb::SetPlayerTeam => self.team(r, out)?,
            Cb::ChangeDifficulty if self.at(V1_21_6) => {
                let d = r.varint()?;
                out.push(u8::try_from(d).map_err(|_| Error::Invalid("difficulty"))?);
                io::bytes(r, out, 1)?;
            }
            Cb::DamageEvent => {
                io::varint(r, out)?;
                let kind = r.varint()?;
                let Some(kind) = self.regs.older("minecraft:damage_type", kind) else {
                    return Ok(Some(false));
                };
                write_varint(out, kind);
                out.extend_from_slice(r.take_rest());
            }
            Cb::MapItemData => {
                io::varint(r, out)?;
                io::bytes(r, out, 2)?;
                if io::boolean(r, out)? {
                    let n = io::len(r, 1024, 5)?;
                    let mut list = Vec::new();
                    let mut kept = 0usize;
                    for _ in 0..n {
                        let kind = r.varint()?;
                        let mut d = Vec::new();
                        io::bytes(r, &mut d, 3)?;
                        if io::boolean(r, &mut d)? {
                            io::nbt(r, &mut d)?;
                        }
                        if let Some(kind) = t.older("minecraft:map_decoration_type", kind) {
                            write_varint(&mut list, kind);
                            list.extend_from_slice(&d);
                            kept = kept.saturating_add(1);
                        }
                    }
                    io::write_len(out, kept);
                    out.extend_from_slice(&list);
                }
                out.extend_from_slice(r.take_rest());
            }
            // 1.21.4 wrote the same doubles and floats through `Vec3.STREAM_CODEC`.
            Cb::MoveVehicle => out.extend_from_slice(r.take_rest()),
            Cb::ProjectilePower if self.at(V1_21) => return Ok(Some(false)),
            Cb::UpdateAdvancements => self.advancements(r, out)?,
            Cb::UpdateRecipes if self.old() >= V1_21_2 => self.items().update_recipes(r, out)?,
            Cb::UpdateRecipes => self.items().old_recipes(r, out)?,
            Cb::RecipeBookAdd => self.items().recipe_book_add(r, out)?,
            Cb::PlaceGhostRecipe if self.old() >= V1_21_2 => {
                self.items().place_ghost_recipe(r, out)?;
            }
            _ => return Ok(None),
        }
        Ok(Some(true))
    }

    /// `update_advancements`: the display icon is a stack template from 26.1 on and a stack
    /// before; 1.21.5 added whether to show the advancements screen.
    fn advancements(&self, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<()> {
        io::bytes(r, out, 1)?;
        let n = io::len(r, 1 << 16, 2)?;
        io::write_len(out, n);
        for _ in 0..n {
            io::string(r, out)?;
            if io::boolean(r, out)? {
                io::string(r, out)?;
            }
            if io::boolean(r, out)? {
                io::nbt(r, out)?;
                io::nbt(r, out)?;
                self.items().particle_item(r, out)?;
                io::varint(r, out)?;
                let flags = r.i32()?;
                out.extend_from_slice(&flags.to_be_bytes());
                if flags & 1 != 0 {
                    io::string(r, out)?;
                }
                io::bytes(r, out, 8)?;
            }
            let groups = io::len(r, 1 << 16, 1)?;
            io::write_len(out, groups);
            for _ in 0..groups {
                let k = io::len(r, 1 << 16, 1)?;
                io::write_len(out, k);
                for _ in 0..k {
                    io::string(r, out)?;
                }
            }
            io::bytes(r, out, 1)?;
        }
        let rest = r.take_rest();
        if self.at(V1_21_5) {
            let len = rest.len().checked_sub(1).ok_or(Error::UnexpectedEnd)?;
            out.extend_from_slice(rest.get(..len).ok_or(Error::UnexpectedEnd)?);
        } else {
            out.extend_from_slice(rest);
        }
        Ok(())
    }

    /// Container ids: an unsigned (or, for slots, signed) byte before 1.21.2, a VarInt since.
    /// Whether the packet's container (next in `r`) is the open stonecutter of a client before
    /// 1.21.2.
    fn cutter_open(&self, r: &Reader<'_>) -> Result<bool> {
        if !self.at(V1_21_2) || self.regs.cutter.container.is_none() {
            return Ok(false);
        }
        Ok(self.regs.cutter.container == Some(r.clone().varint()?))
    }

    fn container_id(&self, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<()> {
        if self.at(V1_21_2) {
            let id = r.varint()?;
            out.push(u8::from_ne_bytes(
                i8::try_from(id).unwrap_or(0).to_ne_bytes(),
            ));
        } else if self.next() >= V1_21_2 {
            io::varint(r, out)?;
        } else {
            io::bytes(r, out, 1)?;
        }
        Ok(())
    }

    /// `CommonPlayerSpawnInfo`: the dimension type is a synchronised registry id; 1.21.2 added
    /// the sea level.
    fn spawn_info(&self, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<()> {
        let dim = r.varint()?;
        write_varint(
            out,
            self.regs
                .older("minecraft:dimension_type", dim)
                .unwrap_or(0),
        );
        io::string(r, out)?;
        io::bytes(r, out, 8 + 1 + 1 + 1 + 1)?;
        if io::boolean(r, out)? {
            io::string(r, out)?;
            io::bytes(r, out, 8)?;
        }
        io::varint(r, out)?;
        if self.at(V1_21_2) {
            r.varint()?;
        } else if self.next() >= V1_21_2 {
            io::varint(r, out)?;
        }
        Ok(())
    }

    /// Heightmaps: a list of (type, long array) from 1.21.5 on, NBT before.
    fn heightmaps(&self, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<()> {
        if !self.at(V1_21_5) {
            if self.next() >= V1_21_5 {
                return chunk::heightmaps(r, out);
            }
            return io::nbt(r, out);
        }
        // Network NBT: a nameless root compound with one long array per type.
        const NAMES: [&str; 6] = [
            "WORLD_SURFACE_WG",
            "WORLD_SURFACE",
            "OCEAN_FLOOR_WG",
            "OCEAN_FLOOR",
            "MOTION_BLOCKING",
            "MOTION_BLOCKING_NO_LEAVES",
        ];
        out.push(10);
        let n = io::len(r, 16, 2)?;
        for _ in 0..n {
            let kind = r.varint()?;
            let longs = io::len(r, 1024, 8)?;
            let data = r.take(longs.saturating_mul(8))?;
            let Some(name) = usize::try_from(kind).ok().and_then(|k| NAMES.get(k)) else {
                continue;
            };
            out.push(12);
            out.extend_from_slice(&u16::try_from(name.len()).unwrap_or(0).to_be_bytes());
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(&i32::try_from(longs).unwrap_or(0).to_be_bytes());
            out.extend_from_slice(data);
        }
        out.push(0);
        Ok(())
    }

    /// Chunk sections (or only biome containers): counts, block states, biomes.
    fn sections(&self, data: &[u8], out: &mut Vec<u8>, biomes_only: bool) -> Result<()> {
        let mut r = Reader::new(data);
        let format = chunk::Format {
            len_in: self.next() < V1_21_5,
            len_out: self.old() < V1_21_5,
        };
        let biome_kind = self.regs.biome_kind();
        let biome_map = self.regs.biome_map();
        let block_map = |id| self.block_state(id);
        while r.remaining() > 0 {
            if !biomes_only {
                io::bytes(&mut r, out, 2)?;
                // 26.1 added the fluid count.
                if self.next() >= V26_1 {
                    let fluids = r.take(2)?;
                    if self.old() >= V26_1 {
                        out.extend_from_slice(fluids);
                    }
                }
                chunk::container_in(&mut r, out, block_kind(self.t), &block_map, format)?;
            }
            chunk::container_in(&mut r, out, biome_kind, &biome_map, format)?;
        }
        Ok(())
    }

    /// `ItemCost`: item id, count, a component list (exact predicate).
    fn item_cost(&self, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<()> {
        write_varint(
            out,
            self.t.older("minecraft:item", r.varint()?).unwrap_or(1),
        );
        io::varint(r, out)?;
        self.items().components(r, out)
    }

    /// Entity data items, by the generated per-type layouts of both versions.
    fn entity_data(&self, kind: i32, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<bool> {
        let t = self.t;
        let layout = usize::try_from(kind)
            .ok()
            .and_then(|k| t.entity_data.get(k))
            .copied()
            .unwrap_or(&[]);
        for _ in 0..256 {
            let index = r.u8()?;
            if index == 0xff {
                out.push(0xff);
                return Ok(true);
            }
            let serializer = r.varint()?;
            let new_name = usize::try_from(serializer)
                .ok()
                .and_then(|s| t.new_serializers.get(s))
                .copied()
                .ok_or(Error::Invalid("entity data serializer"))?;
            let target = layout
                .get(usize::from(index))
                .copied()
                .filter(|&m| m != u16::MAX);
            let mut value = Vec::new();
            let old_name = target.and_then(|m| {
                t.old_serializers
                    .get(usize::from(u8::try_from(m & 0xff).unwrap_or(0)))
                    .copied()
            });
            let keep = self.data_value(new_name, old_name.unwrap_or(""), r, &mut value)?;
            if keep && let (Some(m), Some(old_name)) = (target, old_name) {
                let old_serializer = t
                    .old_serializers
                    .iter()
                    .position(|s| *s == old_name)
                    .and_then(|i| i32::try_from(i).ok())
                    .unwrap_or(0);
                out.push(u8::try_from(m >> 8).unwrap_or(0xff));
                write_varint(out, old_serializer);
                out.extend_from_slice(&value);
            }
        }
        Err(Error::Invalid("entity data without end"))
    }

    /// One entity data value of serializer `new` (newer version) for serializer `old` (older
    /// version; empty if it has no such item). `false` leaves the item out.
    fn data_value(
        &self,
        new: &str,
        old: &str,
        r: &mut Reader<'_>,
        out: &mut Vec<u8>,
    ) -> Result<bool> {
        let same = new == old
            || matches!(
                (new, old),
                ("optional_living_entity_reference", "optional_uuid")
            );
        match new {
            "byte" | "boolean" => io::bytes(r, out, 1)?,
            "int"
            | "direction"
            | "pose"
            | "optional_unsigned_int"
            | "sniffer_state"
            | "armadillo_state"
            | "copper_golem_state"
            | "weathering_copper_state"
            | "dye_color" => {
                io::varint(r, out)?;
            }
            "humanoid_arm" => {
                let arm = r.varint()?;
                if old == "byte" {
                    out.push(u8::try_from(arm).unwrap_or(1));
                    return Ok(true);
                }
                write_varint(out, arm);
            }
            "long" => {
                write_varlong(out, r.varlong()?);
            }
            "float" => io::bytes(r, out, 4)?,
            "string" => io::string(r, out)?,
            "component" | "compound_tag" => io::nbt(r, out)?,
            "optional_component" => {
                if io::boolean(r, out)? {
                    io::nbt(r, out)?;
                }
            }
            "item_stack" => self.items().stack(r, out)?,
            "rotations" | "vector3" => io::bytes(r, out, 12)?,
            "quaternion" => io::bytes(r, out, 16)?,
            "block_pos" => io::bytes(r, out, 8)?,
            "optional_block_pos" => {
                if io::boolean(r, out)? {
                    io::bytes(r, out, 8)?;
                }
            }
            "optional_uuid" | "optional_living_entity_reference" => {
                if io::boolean(r, out)? {
                    io::bytes(r, out, 16)?;
                }
            }
            "block_state" | "optional_block_state" => {
                write_varint(out, self.block_state(r.varint()?));
            }
            "particle" => return Ok(self.particle(r, out)? && same),
            "particles" => {
                let n = io::len(r, 1024, 1)?;
                let mut list = Vec::new();
                let mut kept = 0usize;
                for _ in 0..n {
                    if self.particle(r, &mut list)? {
                        kept = kept.saturating_add(1);
                    }
                }
                io::write_len(out, kept);
                out.extend_from_slice(&list);
            }
            "villager_data" => {
                let kind = r.varint()?;
                let profession = r.varint()?;
                write_varint(
                    out,
                    self.t.older("minecraft:villager_type", kind).unwrap_or(0),
                );
                write_varint(
                    out,
                    self.t
                        .older("minecraft:villager_profession", profession)
                        .unwrap_or(0),
                );
                io::varint(r, out)?;
            }
            "optional_global_pos" => {
                if io::boolean(r, out)? {
                    io::string(r, out)?;
                    io::bytes(r, out, 8)?;
                }
            }
            "resolvable_profile" => {
                let start = r.rest();
                skip_profile(r)?;
                let len = start.len().saturating_sub(r.remaining());
                out.extend_from_slice(start.get(..len).ok_or(Error::UnexpectedEnd)?);
            }
            variant if variant.ends_with("_variant") => {
                // A synchronised registry entry: `holder` (id + 1, 0 inline) for wolves
                // (1.21–1.21.4) and paintings (1.21 on), else the registry id.
                let registry = format!("minecraft:{variant}");
                let holder = |p: i32| match variant {
                    "painting_variant" => p >= V1_21,
                    "wolf_variant" => (V1_21..V1_21_5).contains(&p),
                    _ => false,
                };
                let raw = r.varint()?;
                let id = if holder(self.next()) {
                    if raw == 0 {
                        return Err(Error::Invalid("inline variant"));
                    }
                    raw.saturating_sub(1)
                } else {
                    raw
                };
                let Some(old) = self.regs.older(&registry, id) else {
                    return Ok(false);
                };
                write_varint(
                    out,
                    if holder(self.old()) {
                        old.saturating_add(1)
                    } else {
                        old
                    },
                );
            }
            _ => return Err(Error::Invalid("entity data serializer")),
        }
        Ok(same)
    }

    /// `ParticleTypes.STREAM_CODEC`: a type id and its options. `false` if the older version
    /// lacks the type (the options are consumed, nothing is written).
    fn particle(&self, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<bool> {
        let id = r.varint()?;
        let name = usize::try_from(id)
            .ok()
            .and_then(|i| self.t.new_particle_types.get(i))
            .copied()
            .ok_or(Error::Invalid("particle type"))?;
        let mut options = Vec::new();
        self.particle_options(name, r, &mut options)?;
        match self.t.older("minecraft:particle_type", id) {
            Some(old) => {
                write_varint(out, old);
                out.extend_from_slice(&options);
                Ok(true)
            }
            None => Ok(false),
        }
    }

    fn particle_options(&self, name: &str, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<()> {
        let (new, old) = (self.next(), self.old());
        // Options some particles gained in 1.21.9.
        let gained = |out: &mut Vec<u8>, r: &mut Reader<'_>, size: usize| -> Result<()> {
            if new >= V1_21_9 {
                let v = r.take(size)?;
                if old >= V1_21_9 {
                    out.extend_from_slice(v);
                }
            }
            Ok(())
        };
        match name.trim_start_matches("minecraft:") {
            "block" | "block_marker" | "falling_dust" | "dust_pillar" | "block_crumble" => {
                write_varint(out, self.block_state(r.varint()?));
            }
            "dust" => {
                self.dust_color(r, out)?;
                io::bytes(r, out, 4)?;
            }
            "dust_color_transition" => {
                self.dust_color(r, out)?;
                self.dust_color(r, out)?;
                io::bytes(r, out, 4)?;
            }
            "entity_effect" | "tinted_leaves" => io::bytes(r, out, 4)?,
            "flash" => gained(out, r, 4)?,
            "effect" | "instant_effect" => gained(out, r, 8)?,
            "dragon_breath" => gained(out, r, 4)?,
            "sculk_charge" => io::bytes(r, out, 4)?,
            "shriek" => {
                io::varint(r, out)?;
            }
            "item" => self.items().particle_item(r, out)?,
            "vibration" => {
                let kind = io::varint(r, out)?;
                if kind == 0 {
                    io::bytes(r, out, 8)?;
                } else {
                    io::varint(r, out)?;
                    io::bytes(r, out, 4)?;
                }
                io::varint(r, out)?;
            }
            "trail" => {
                io::bytes(r, out, 24 + 4)?;
                // 1.21.4 added the duration.
                if new >= V1_21_4 {
                    let d = r.take(4)?;
                    if old >= V1_21_4 {
                        out.extend_from_slice(d);
                    }
                }
            }
            "geyser" | "geyser_plume" => io::bytes(r, out, 4)?,
            "geyser_base" | "geyser_poof" => io::bytes(r, out, 8)?,
            _ => {}
        }
        Ok(())
    }

    /// A dust colour: an RGB int from 1.21.2 on, three floats before.
    fn dust_color(&self, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<()> {
        if self.at(V1_21_2) {
            let rgb = r.i32()?;
            for shift in [16, 8, 0] {
                let c = f32::from(u8::try_from((rgb >> shift) & 0xff).unwrap_or(0)) / 255.0;
                out.extend_from_slice(&c.to_be_bytes());
            }
            Ok(())
        } else if self.next() >= V1_21_2 {
            io::bytes(r, out, 4)
        } else {
            io::bytes(r, out, 12)
        }
    }

    /// `explode`: one format until 1.21.1 (blocks, knockback, two particles), a smaller one
    /// from 1.21.2 on, extended by radius, block count and particle list in 1.21.9.
    fn explode(&self, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<bool> {
        let center = r.take(24)?;
        out.extend_from_slice(center);
        if self.next() >= V1_21_9 {
            let extra = r.take(8)?;
            if self.old() >= V1_21_9 {
                out.extend_from_slice(extra);
            }
        }
        let knockback = if io::boolean(r, &mut Vec::new())? {
            Some(r.take(24)?)
        } else {
            None
        };
        let mut particle = Vec::new();
        if !self.particle(r, &mut particle)? {
            return Ok(false);
        }
        let mut sound = Vec::new();
        common::sound(self.t, r, &mut sound)?;
        if self.old() >= V1_21_2 {
            match knockback {
                Some(k) => {
                    out.push(1);
                    out.extend_from_slice(k);
                }
                None => out.push(0),
            }
            out.extend_from_slice(&particle);
            out.extend_from_slice(&sound);
            out.extend_from_slice(r.take_rest());
            return Ok(true);
        }
        // Until 1.21.1: power, affected blocks, knockback (floats), block interaction, small and
        // large particle, sound. Power 4 (TNT) only selects the particle; blocks come as updates.
        out.extend_from_slice(&4.0f32.to_be_bytes());
        out.push(0);
        let mut k = [0.0f32; 3];
        if let Some(bytes) = knockback {
            let mut kr = Reader::new(bytes);
            for v in &mut k {
                #[allow(clippy::as_conversions)] // Knockback is small.
                {
                    *v = kr.f64()? as f32;
                }
            }
        }
        for v in k {
            out.extend_from_slice(&v.to_be_bytes());
        }
        write_varint(out, 0);
        out.extend_from_slice(&particle);
        out.extend_from_slice(&particle);
        out.extend_from_slice(&sound);
        Ok(true)
    }

    /// `set_time`: game time and day time (with a negative day time for a stopped clock until
    /// 1.21.1, a tick flag from 1.21.2 on, world clocks from 26.1 on).
    fn set_time(&self, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<bool> {
        io::bytes(r, out, 8)?;
        let (day, ticking) = if self.next() >= V26_1 {
            if self.old() >= V26_1 {
                out.extend_from_slice(r.take_rest());
                return Ok(true);
            }
            // The overworld clock, else the first.
            let clocks = self.regs.dropped("minecraft:world_clock").unwrap_or(&[]);
            let n = io::len(r, 64, 7)?;
            let mut found = None;
            for _ in 0..n {
                let clock = r.varint()?;
                let ticks = r.varlong()?;
                r.skip(4)?;
                let rate = r.f32()?;
                let overworld = usize::try_from(clock)
                    .ok()
                    .and_then(|c| clocks.get(c))
                    .is_some_and(|c| c == "minecraft:overworld");
                if found.is_none() || overworld {
                    found = Some((ticks, rate > 0.0));
                }
            }
            let Some(found) = found else {
                return Ok(false);
            };
            found
        } else {
            let day = r.i64()?;
            let ticking = if self.next() >= V1_21_2 {
                r.u8()? != 0
            } else {
                day >= 0
            };
            (day, ticking)
        };
        if self.old() >= V1_21_2 {
            out.extend_from_slice(&day.to_be_bytes());
            out.push(u8::from(ticking));
        } else {
            let day = match (ticking, day) {
                (true, d) => d.saturating_abs(),
                (false, 0) => -1,
                (false, d) => d.saturating_abs().saturating_neg(),
            };
            out.extend_from_slice(&day.to_be_bytes());
        }
        Ok(true)
    }

    /// `player_info_update`: the actions the older version lacks (list order 1.21.2, hat
    /// 1.21.4) are left out of the set and the entries.
    fn player_info(&self, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<()> {
        const LIST_ORDER: u8 = 1 << 6;
        const HAT: u8 = 1 << 7;
        let actions = r.u8()?;
        let mut drop = 0u8;
        if self.old() < V1_21_2 {
            drop |= LIST_ORDER;
        }
        if self.old() < V1_21_4 {
            drop |= HAT;
        }
        out.push(actions & !drop);
        let n = io::len(r, 4096, 16)?;
        io::write_len(out, n);
        for _ in 0..n {
            io::bytes(r, out, 16)?;
            for bit in 0..8u8 {
                if actions & (1 << bit) == 0 {
                    continue;
                }
                let mut sink = Vec::new();
                let o: &mut Vec<u8> = if drop & (1 << bit) != 0 {
                    &mut sink
                } else {
                    out
                };
                match bit {
                    // Add player: name, properties.
                    0 => {
                        io::string(r, o)?;
                        let p = io::len(r, 16, 2)?;
                        io::write_len(o, p);
                        for _ in 0..p {
                            io::string(r, o)?;
                            io::string(r, o)?;
                            if io::boolean(r, o)? {
                                io::string(r, o)?;
                            }
                        }
                    }
                    // Initialize chat: optional session (id, expiry, key, signature).
                    1 => {
                        if io::boolean(r, o)? {
                            io::bytes(r, o, 24)?;
                            for _ in 0..2 {
                                let len = io::len(r, 4096, 1)?;
                                io::write_len(o, len);
                                io::bytes(r, o, len)?;
                            }
                        }
                    }
                    // Game mode, latency, list order.
                    2 | 4 | 6 => {
                        io::varint(r, o)?;
                    }
                    // Listed, hat.
                    3 | 7 => io::bytes(r, o, 1)?,
                    // Display name.
                    _ => {
                        if io::boolean(r, o)? {
                            io::nbt(r, o)?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// A chat type: a registry id until 1.20.5, a holder (id + 1, 0 inline) from 1.21 on.
    fn chat_type(&self, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<bool> {
        let raw = r.varint()?;
        let id = if self.next() >= V1_21 {
            if raw == 0 {
                return Ok(false);
            }
            raw.saturating_sub(1)
        } else {
            raw
        };
        let Some(old) = self.regs.older("minecraft:chat_type", id) else {
            return Ok(false);
        };
        write_varint(
            out,
            if self.old() >= V1_21 {
                old.saturating_add(1)
            } else {
                old
            },
        );
        Ok(true)
    }

    /// `teleport_entity` for a client before 1.21.2 (absolute position and angles, no relative
    /// flags): relative moves of all three axes become a `move_entity_pos(_rot)` when they are
    /// short enough, and only other relative teleports are left out.
    fn teleport_entity(&self, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<bool> {
        let entity = r.varint()?;
        let pos = [r.f64()?, r.f64()?, r.f64()?];
        r.skip(24)?;
        let rot = [r.f32()?, r.f32()?];
        let relatives = r.i32()?;
        let on_ground = r.u8()?;
        if relatives & 7 == 0 {
            write_varint(out, self.old_id(Cb::TeleportEntity)?);
            write_varint(out, entity);
            for c in pos {
                out.extend_from_slice(&c.to_be_bytes());
            }
            out.push(angle(rot[0]));
            out.push(angle(rot[1]));
            out.push(on_ground);
            return Ok(true);
        }
        let mut delta = [0i16; 3];
        for (d, c) in delta.iter_mut().zip(pos) {
            let scaled = (c * 4096.0).round();
            if relatives & 7 != 7 || !(-32768.0..=32767.0).contains(&scaled) {
                return Ok(false);
            }
            #[allow(clippy::as_conversions)] // In range, checked above.
            let v = scaled as i16;
            *d = v;
        }
        let rotation = match relatives & 0x18 {
            0 => true,
            0x18 if rot == [0.0, 0.0] => false,
            _ => return Ok(false),
        };
        write_varint(
            out,
            self.old_id(if rotation {
                Cb::MoveEntityPosRot
            } else {
                Cb::MoveEntityPos
            })?,
        );
        write_varint(out, entity);
        for d in delta {
            out.extend_from_slice(&d.to_be_bytes());
        }
        if rotation {
            out.push(angle(rot[0]));
            out.push(angle(rot[1]));
        }
        out.push(on_ground);
        Ok(true)
    }

    /// `player_chat`: 1.21.5 added the global index in front; the chat type follows the
    /// synchronised registry.
    fn player_chat(&self, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<bool> {
        if self.at(V1_21_5) {
            r.varint()?;
        } else if self.next() >= V1_21_5 {
            io::varint(r, out)?;
        }
        io::bytes(r, out, 16)?;
        io::varint(r, out)?;
        if io::boolean(r, out)? {
            io::bytes(r, out, 256)?;
        }
        // Body: content, timestamp, salt, last seen (id + 1, signature if id is 0).
        io::string(r, out)?;
        io::bytes(r, out, 16)?;
        let n = io::len(r, 20, 1)?;
        io::write_len(out, n);
        for _ in 0..n {
            if io::varint(r, out)? == 0 {
                io::bytes(r, out, 256)?;
            }
        }
        if io::boolean(r, out)? {
            io::component(r, out, self.at(V1_21_5))?;
        }
        // Filter mask: type, and a bit set for partially filtered.
        if io::varint(r, out)? == 2 {
            let longs = io::len(r, 1024, 8)?;
            io::write_len(out, longs);
            io::bytes(r, out, longs.saturating_mul(8))?;
        }
        if !self.chat_type(r, out)? {
            return Ok(false);
        }
        self.chat_names(r, out)?;
        Ok(true)
    }

    /// The sender name and optional target name of a chat type binding; their hover and click
    /// events change shape in 1.21.5.
    fn chat_names(&self, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<()> {
        if !self.at(V1_21_5) {
            out.extend_from_slice(r.take_rest());
            return Ok(());
        }
        io::component(r, out, true)?;
        if io::boolean(r, out)? {
            io::component(r, out, true)?;
        }
        Ok(())
    }

    /// `set_player_team`: parameters changed in 1.21.5 (visibility and collision as enums
    /// instead of names) and 26.2 (new order, optional team colour).
    fn team(&self, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<()> {
        const VISIBILITY: [&str; 4] = ["always", "never", "hideForOtherTeams", "hideForOwnTeam"];
        const COLLISION: [&str; 4] = ["always", "never", "pushOtherTeams", "pushOwnTeam"];
        io::string(r, out)?;
        let method = r.u8()?;
        out.push(method);
        // 0 create and 2 update carry parameters.
        if method == 0 || method == 2 {
            let (name, options, visibility, collision, color, prefix, suffix);
            let mut nr = Vec::new();
            if self.next() >= V26_2 {
                name = nbt_slice(r)?;
                prefix = nbt_slice(r)?;
                suffix = nbt_slice(r)?;
                visibility = Some(r.varint()?);
                collision = Some(r.varint()?);
                color = if r.u8()? != 0 { r.varint()? } else { 21 };
                options = r.u8()?;
            } else {
                name = nbt_slice(r)?;
                options = r.u8()?;
                if self.next() >= V1_21_5 {
                    visibility = Some(r.varint()?);
                    collision = Some(r.varint()?);
                } else {
                    io::string(r, &mut nr)?;
                    io::string(r, &mut nr)?;
                    visibility = None;
                    collision = None;
                }
                color = r.varint()?;
                prefix = nbt_slice(r)?;
                suffix = nbt_slice(r)?;
            }
            let enum_name = |names: &[&'static str; 4], v: i32| {
                usize::try_from(v)
                    .ok()
                    .and_then(|i| names.get(i))
                    .copied()
                    .unwrap_or("always")
            };
            out.extend_from_slice(name);
            if self.old() >= V26_2 {
                out.extend_from_slice(prefix);
                out.extend_from_slice(suffix);
                write_varint(out, visibility.unwrap_or(0));
                write_varint(out, collision.unwrap_or(0));
                if (0..16).contains(&color) {
                    out.push(1);
                    write_varint(out, color);
                } else {
                    out.push(0);
                }
                out.push(options);
            } else {
                out.push(options);
                match (visibility, collision) {
                    (Some(v), Some(c)) if self.old() >= V1_21_5 => {
                        write_varint(out, v);
                        write_varint(out, c);
                    }
                    (Some(v), Some(c)) => {
                        write_str(out, enum_name(&VISIBILITY, v));
                        write_str(out, enum_name(&COLLISION, c));
                    }
                    _ => out.extend_from_slice(&nr),
                }
                write_varint(out, color);
                out.extend_from_slice(prefix);
                out.extend_from_slice(suffix);
            }
        }
        out.extend_from_slice(r.take_rest());
        Ok(())
    }
}

/// The bytes of one network NBT tag.
fn nbt_slice<'a>(r: &mut Reader<'a>) -> Result<&'a [u8]> {
    let start = r.rest();
    let mut sink = Vec::new();
    io::nbt(r, &mut sink)?;
    start
        .get(..start.len().saturating_sub(r.remaining()))
        .ok_or(Error::UnexpectedEnd)
}

/// Skips a `ResolvableProfile` (1.21.9): a full profile or a partial one, then a skin patch.
fn skip_profile(r: &mut Reader<'_>) -> Result<()> {
    let mut sink = Vec::new();
    let full = r.u8()? != 0;
    if full {
        r.skip(16)?;
        r.string(io::MAX_STRING)?;
    } else {
        if r.u8()? != 0 {
            r.string(io::MAX_STRING)?;
        }
        if r.u8()? != 0 {
            r.skip(16)?;
        }
    }
    let p = io::len(r, 16, 2)?;
    for _ in 0..p {
        io::string(r, &mut sink)?;
        io::string(r, &mut sink)?;
        if r.u8()? != 0 {
            r.string(io::MAX_STRING)?;
        }
    }
    // Skin patch: optional body, cape, elytra (identifiers), optional model.
    for _ in 0..3 {
        if r.u8()? != 0 {
            r.string(io::MAX_STRING)?;
        }
    }
    if r.u8()? != 0 {
        r.varint()?;
    }
    Ok(())
}

/// Degrees as a byte angle.
fn angle(deg: f32) -> u8 {
    let v = (deg * 256.0 / 360.0).rem_euclid(256.0).floor();
    #[allow(clippy::as_conversions)] // 0..256 after rem_euclid.
    let v = v as u32;
    u8::try_from(v & 0xff).unwrap_or(0)
}

/// The UUID older clients identify an attribute modifier by (1.21 named them): the one the
/// client itself uses for sprinting, else one derived from the name.
fn modifier_uuid(id: &str) -> [u8; 16] {
    if id == "minecraft:sprinting" {
        return 0x662a_6b8d_da3e_4c1c_8813_96ea_6097_278d_u128.to_be_bytes();
    }
    // FNV-1a, 128 bit: stable and different per name.
    let mut h: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
    for b in id.bytes() {
        h ^= u128::from(b);
        h = h.wrapping_mul(0x0000_0000_0100_0000_0000_0000_0000_013b);
    }
    h.to_be_bytes()
}

#[derive(Debug)]
struct Serverbound {
    t: &'static Tables,
    regs: Arc<Mutex<Registries>>,
    /// The client's last rotation (for `use_item`, which carries it from 1.21 on).
    rot: [f32; 2],
    /// The last player input flags sent to the newer version (1.21.2 format).
    input: u8,
    /// An `interact_at` waiting for its `interact` (before 26.1): entity and hand.
    interact_at: Option<(i32, i32)>,
}

impl ServerboundStep for Serverbound {
    fn translate(
        &mut self,
        state: State,
        body: &[u8],
        out: &mut Vec<u8>,
        extra: &mut Vec<Vec<u8>>,
    ) -> Result<bool> {
        let t = self.t;
        let Some(table) = packets(t, state, false) else {
            out.extend_from_slice(body);
            return Ok(true);
        };
        let mut r = Reader::new(body);
        let id = r.varint()?;
        if state == State::Login {
            write_varint(
                out,
                table
                    .newer(id)
                    .ok_or(Error::Invalid("packet unknown to the server's version"))?,
            );
            out.extend_from_slice(r.take_rest());
            return Ok(true);
        }
        let kind = t
            .serverbound(state, id)
            .ok_or(Error::Invalid("packet id"))?;
        if state == State::Play
            && let Some(keep) = self.converted(kind, &mut r, out)?
        {
            return Ok(keep);
        }
        let Some(new_id) = table.newer(id) else {
            // A packet the newer version does not have.
            return Ok(false);
        };
        write_varint(out, new_id);
        let handled = self.packet(state, kind, &mut r, out, extra)?;
        match handled {
            Some(keep) => Ok(keep),
            None if changed(t, state, false).contains(&id) => Ok(false),
            None => {
                out.extend_from_slice(r.take_rest());
                Ok(true)
            }
        }
    }
}

impl Serverbound {
    fn old(&self) -> i32 {
        self.t.protocol
    }

    fn at(&self, v: i32) -> bool {
        self.t.next_protocol >= v && self.t.protocol < v
    }

    fn new_id(&self, kind: Sb) -> Result<i32> {
        self.t
            .new_serverbound(State::Play, kind)
            .ok_or(Error::Invalid("packet the newer version lacks"))
    }

    /// A packet the newer version has for this one's kind.
    fn body(&self, kind: Sb) -> Result<Vec<u8>> {
        let mut b = Vec::with_capacity(8);
        write_varint(&mut b, self.new_id(kind)?);
        Ok(b)
    }

    /// Packets the newer version receives as another packet.
    fn converted(
        &mut self,
        kind: Sb,
        r: &mut Reader<'_>,
        out: &mut Vec<u8>,
    ) -> Result<Option<bool>> {
        if !self.at(V26_1) || kind != Sb::Interact {
            return Ok(None);
        }
        // Before 26.1: entity, action (0 interact, 1 attack, 2 interact at), [hand | position
        // and hand], sneaking. From 26.1: `attack` (entity), `interact` (entity, hand,
        // position as `LpVec3`, sneaking).
        let entity = r.varint()?;
        match r.varint()? {
            1 => {
                write_varint(out, self.new_id(Sb::Attack)?);
                write_varint(out, entity);
                Ok(Some(true))
            }
            0 => {
                let hand = r.varint()?;
                let sneaking = r.u8()?;
                if self.interact_at.take() == Some((entity, hand)) {
                    // Already sent with its position.
                    return Ok(Some(false));
                }
                write_varint(out, self.new_id(Sb::Interact)?);
                write_varint(out, entity);
                write_varint(out, hand);
                out.push(0);
                out.push(sneaking);
                Ok(Some(true))
            }
            2 => {
                let pos = [r.f32()?, r.f32()?, r.f32()?];
                let hand = r.varint()?;
                let sneaking = r.u8()?;
                self.interact_at = Some((entity, hand));
                write_varint(out, self.new_id(Sb::Interact)?);
                write_varint(out, entity);
                write_varint(out, hand);
                write_lp_vec3(out, pos.map(f64::from));
                out.push(sneaking);
                Ok(Some(true))
            }
            _ => Err(Error::Invalid("interact action")),
        }
    }

    #[allow(clippy::too_many_lines)]
    fn packet(
        &mut self,
        state: State,
        kind: Sb,
        r: &mut Reader<'_>,
        out: &mut Vec<u8>,
        extra: &mut Vec<Vec<u8>>,
    ) -> Result<Option<bool>> {
        let t = self.t;
        match kind {
            Sb::SelectKnownPacks => common::known_packs(r, out, t.protocol, t.next_protocol)?,
            Sb::ClientInformation => {
                out.extend_from_slice(r.take_rest());
                // 1.21.2 added the particle status: all.
                if self.at(V1_21_2) {
                    write_varint(out, 0);
                }
            }
            _ if state != State::Play => return Ok(None),
            Sb::MovePlayerPos
            | Sb::MovePlayerPosRot
            | Sb::MovePlayerRot
            | Sb::MovePlayerStatusOnly => {
                let rest = r.take_rest();
                let rot_at = match kind {
                    Sb::MovePlayerPosRot => Some(24),
                    Sb::MovePlayerRot => Some(0),
                    _ => None,
                };
                if let Some(at) = rot_at {
                    let mut rr = Reader::new(rest.get(at..).unwrap_or_default());
                    self.rot = [rr.f32()?, rr.f32()?];
                }
                out.extend_from_slice(rest);
                // The server expects the end of each client tick (1.21.2); older clients send
                // at most one movement per tick.
                if self.at(V1_21_2) {
                    extra.push(self.body(Sb::ClientTickEnd)?);
                }
            }
            Sb::AcceptTeleportation => {
                if r.clone().varint()? == SYNTHETIC_TELEPORT {
                    return Ok(Some(false));
                }
                out.extend_from_slice(r.take_rest());
                // 1.21.4: the client reports when it can play; older ones can once teleported.
                if self.at(V1_21_4) {
                    extra.push(self.body(Sb::PlayerLoaded)?);
                }
            }
            Sb::PlayerInput if self.at(V1_21_2) => {
                // Sideways and forward impulse, jump and shift flags → the key flags.
                let xxa = r.f32()?;
                let zza = r.f32()?;
                let flags = r.u8()?;
                let mut input = 0u8;
                if zza > 0.0 {
                    input |= 1;
                }
                if zza < 0.0 {
                    input |= 2;
                }
                if xxa > 0.0 {
                    input |= 4;
                }
                if xxa < 0.0 {
                    input |= 8;
                }
                if flags & 1 != 0 {
                    input |= 16;
                }
                if flags & 2 != 0 {
                    input |= 32;
                }
                self.input = input;
                out.push(input);
            }
            Sb::PlayerCommand if self.at(V1_21_2) => {
                // Entity, action, data. Shift (0 press, 1 release) also goes into the player
                // input from 1.21.2 on.
                io::varint(r, out)?;
                let action = r.varint()?;
                write_varint(out, action);
                out.extend_from_slice(r.take_rest());
                if action == 0 || action == 1 {
                    self.input = if action == 0 {
                        self.input | 32
                    } else {
                        self.input & !32
                    };
                    let mut b = self.body(Sb::PlayerInput)?;
                    b.push(self.input);
                    extra.push(b);
                }
            }
            Sb::PlayerCommand if self.at(V1_21_6) => {
                // Entity, action, data: 1.21.6 dropped the shift actions (shift comes with the
                // player input).
                io::varint(r, out)?;
                let action = r.varint()?;
                if action < 2 {
                    return Ok(Some(false));
                }
                write_varint(out, action.saturating_sub(2));
                out.extend_from_slice(r.take_rest());
            }
            Sb::UseItem if self.at(V1_21) => {
                out.extend_from_slice(r.take_rest());
                for v in self.rot {
                    out.extend_from_slice(&v.to_be_bytes());
                }
            }
            Sb::UseItemOn if self.at(V1_21_2) => {
                // Hand, position, face, cursor, inside; 1.21.2 added the world border hit.
                io::varint(r, out)?;
                io::bytes(r, out, 8)?;
                io::varint(r, out)?;
                io::bytes(r, out, 12 + 1)?;
                out.push(0);
                out.extend_from_slice(r.take_rest());
            }
            Sb::ContainerClick => self.container_click(r, out)?,
            Sb::PlaceRecipe if self.at(V1_21_2) => {
                // Container byte, recipe name, shift → container, display id, shift.
                let container = r.u8()?;
                let Some(id) = recipe::display_id(r.string(io::MAX_STRING)?) else {
                    return Ok(Some(false));
                };
                write_varint(out, i32::from(container));
                write_varint(out, id);
                out.extend_from_slice(r.take_rest());
            }
            Sb::RecipeBookSeenRecipe if self.at(V1_21_2) => {
                let Some(id) = recipe::display_id(r.string(io::MAX_STRING)?) else {
                    return Ok(Some(false));
                };
                write_varint(out, id);
            }
            Sb::ContainerClose if self.at(V1_21_2) => {
                self.regs
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .cutter
                    .container = None;
                write_varint(out, i32::from(r.u8()?));
            }
            Sb::ContainerButtonClick if self.at(V1_21_2) => {
                // Container id and button as bytes before 1.21.2; a stonecutter's button is the
                // client's recipe index.
                let container = i32::from(r.u8()?);
                let button = i32::from(r.u8()?);
                let regs = self.regs.lock().unwrap_or_else(PoisonError::into_inner);
                write_varint(out, container);
                write_varint(out, regs.cutter.server_index(container, button));
            }
            Sb::SetCreativeModeSlot => {
                io::bytes(r, out, 2)?;
                let regs = self.regs.lock().unwrap_or_else(PoisonError::into_inner);
                let items = Items {
                    t,
                    regs: &regs,
                    clientbound: false,
                    depth: 0,
                };
                if items.client_stack(r, out).is_err() {
                    return Ok(Some(false));
                }
            }
            Sb::MoveVehicle if self.at(V1_21_4) => {
                out.extend_from_slice(r.take_rest());
                out.push(0);
            }
            Sb::ChangeDifficulty if self.at(V1_21_6) => {
                write_varint(out, i32::from(r.u8()?));
            }
            Sb::Chat | Sb::ChatCommandSigned if self.at(V1_21_5) => {
                // The last seen update gained a checksum byte (0: not checked).
                out.extend_from_slice(r.take_rest());
                out.push(0);
            }
            Sb::SetBeacon => {
                for _ in 0..2 {
                    if io::boolean(r, out)? {
                        let effect = r.varint()?;
                        write_varint(
                            out,
                            t.newer("minecraft:mob_effect", effect)
                                .ok_or(Error::Invalid("mob effect"))?,
                        );
                    }
                }
            }
            Sb::PlayerAction
            | Sb::ClientCommand
            | Sb::SetJigsawBlock
            | Sb::SetStructureBlock
            | Sb::CookieResponse
            | Sb::CustomClickAction
            | Sb::SeenAdvancements => {
                // Only identifiers were renamed (1.21.11) or actions appended (`client_command`
                // gained `request_gamerule_values` in 26.1).
                out.extend_from_slice(r.take_rest());
            }
            _ => return Ok(None),
        }
        Ok(Some(true))
    }

    /// `container_click`: container id (a byte before 1.21.2), state, slot, button, click type,
    /// changed slots and carried item (full stacks before 1.21.5, hashed stacks from then on).
    fn container_click(&mut self, r: &mut Reader<'_>, out: &mut Vec<u8>) -> Result<()> {
        let container = r.clone().u8().map(i32::from)?;
        if self.at(V1_21_2) {
            write_varint(out, i32::from(r.u8()?));
        } else if self.old() >= V1_21_2 {
            io::varint(r, out)?;
        } else {
            io::bytes(r, out, 1)?;
        }
        io::varint(r, out)?;
        io::bytes(r, out, 3)?;
        io::varint(r, out)?;
        let regs = self.regs.lock().unwrap_or_else(PoisonError::into_inner);
        let items = Items {
            t: self.t,
            regs: &regs,
            clientbound: false,
            depth: 0,
        };
        let stack = |r: &mut Reader<'_>, out: &mut Vec<u8>| -> Result<()> {
            if self.old() >= V1_21_5 {
                items.hashed_stack(r, out)
            } else {
                items.click_stack(r, out)
            }
        };
        let n = io::len(r, 128, 3)?;
        io::write_len(out, n);
        let mut input = None;
        for _ in 0..n {
            let slot = r.clone().take(2)?;
            io::bytes(r, out, 2)?;
            if slot == [0, 0] && self.old() < V1_21_2 {
                let mut peek = r.clone();
                input = Some(match peek.varint()? {
                    count if count > 0 => Some(peek.varint()?),
                    _ => None,
                });
            }
            stack(r, out)?;
        }
        stack(r, out)?;
        drop(regs);
        if let Some(item) = input
            && self.at(V1_21_2)
        {
            let mut regs = self.regs.lock().unwrap_or_else(PoisonError::into_inner);
            if regs.cutter.container == Some(container) {
                regs.cutter.input = item;
            }
        }
        Ok(())
    }
}

/// Writes an `LpVec3` (1.21.9): `Vec3` components scaled into 15 bits each.
fn write_lp_vec3(out: &mut Vec<u8>, v: [f64; 3]) {
    let max = v.iter().fold(0.0f64, |m, c| m.max(c.abs()));
    if !max.is_finite() || max < 3.051_944_088_384_301e-5 {
        out.push(0);
        return;
    }
    #[allow(clippy::as_conversions)] // A positive, finite ceiling below 2^34.
    let scale = max.ceil().min(1.7e10) as u64;
    let big = scale & 3 != scale;
    let low = if big { (scale & 3) | 4 } else { scale };
    #[allow(clippy::as_conversions)] // Exact for these magnitudes.
    let s = scale as f64;
    let pack = |c: f64| -> u64 {
        let q = ((c / s) * 0.5 + 0.5) * 32766.0;
        #[allow(clippy::as_conversions)] // 0..=32766 after the clamp.
        let q = q.round().clamp(0.0, 32766.0) as u64;
        q
    };
    let [x, y, z] = v;
    let packed = low | (pack(x) << 3) | (pack(y) << 18) | (pack(z) << 33);
    let bytes = packed.to_le_bytes();
    out.extend_from_slice(bytes.get(..2).unwrap_or_default());
    let high = u32::try_from((packed >> 16) & 0xffff_ffff).unwrap_or(0);
    out.extend_from_slice(&high.to_be_bytes());
    if big {
        write_varint(out, i32::try_from(scale >> 2).unwrap_or(i32::MAX));
    }
}

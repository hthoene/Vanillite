//! [`Server`] and [`Ctx`] functions added with ABI 1.6: typed entity values, passengers,
//! attributes, mob targets, navigation and goals; biomes, light, chunks, the world border,
//! explosions and ray traces; plugin menus; item components and item persistent data.

use std::ffi::c_void;

use vanillite_abi::{self as abi, Status, status};

use super::{Ctx, Server, Task, TaskId, collect_with, drop_task, run_task};
use crate::ffi::read_string;
use crate::game::{Goal, GoalBox};
use crate::types::{BlockBox, BlockPos, DataValue, Error, Text, Uuid, Vec3, World};

/// A world border ([`Ctx::border`]). Since ABI 1.6.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WorldBorder {
    /// Centre X.
    pub center_x: f64,
    /// Centre Z.
    pub center_z: f64,
    /// The diameter now.
    pub diameter: f64,
    /// The diameter it moves to (equal to `diameter` for none).
    pub target: f64,
    /// Ticks until it reaches `target`.
    pub lerp_ticks: i64,
    /// Damage per block outside the safe zone per second.
    pub damage_per_block: f64,
    /// Blocks outside the border without damage.
    pub safe_zone: f64,
    /// Warning distance in blocks.
    pub warning_blocks: i32,
    /// Warning time in seconds.
    pub warning_time: i32,
}

impl WorldBorder {
    fn from_abi(b: abi::BorderInfo) -> WorldBorder {
        WorldBorder {
            center_x: b.center_x,
            center_z: b.center_z,
            diameter: b.diameter,
            target: b.target,
            lerp_ticks: b.lerp_ticks,
            damage_per_block: b.damage_per_block,
            safe_zone: b.safe_zone,
            warning_blocks: b.warning_blocks,
            warning_time: b.warning_time,
        }
    }

    fn to_abi(self) -> abi::BorderInfo {
        abi::BorderInfo {
            size: abi::size_of::<abi::BorderInfo>(),
            center_x: self.center_x,
            center_z: self.center_z,
            diameter: self.diameter,
            target: self.target,
            lerp_ticks: self.lerp_ticks,
            damage_per_block: self.damage_per_block,
            safe_zone: self.safe_zone,
            warning_blocks: self.warning_blocks,
            warning_time: self.warning_time,
        }
    }
}

/// What a ray hit ([`Ctx::raytrace`]). Since ABI 1.6.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RayHit {
    /// A block: the point, the block and the face hit (0 down .. 5 east).
    Block(Vec3, BlockPos, u32),
    /// An entity at a point.
    Entity(Vec3, Uuid),
}

fn status_err(s: Status) -> Error {
    Error::check(s).err().unwrap_or(Error::InvalidArgument)
}

fn utf8(bytes: Vec<u8>) -> Result<String, Error> {
    String::from_utf8(bytes).map_err(|_| Error::InvalidArgument)
}

impl Server {
    /// Runs `task` on an entity's owner after `delay` ticks, then every `period` ticks (0:
    /// once); when the entity is gone, `retired` runs once instead (Folia's retired callback).
    /// Since ABI 1.6.
    pub fn schedule_entity(
        &self,
        world: World,
        entity: Uuid,
        delay: u64,
        period: u64,
        task: impl FnMut(&mut Ctx<'_>) + Send + 'static,
        retired: impl FnOnce(&mut Ctx<'_>) + Send + 'static,
    ) -> Result<TaskId, Error> {
        let mut retired = Some(retired);
        let mut task = task;
        let both: Box<Task> = Box::new(Box::new(move |ctx: &mut Ctx<'_>| {
            if ctx.retiring {
                if let Some(r) = retired.take() {
                    r(ctx);
                }
            } else {
                task(ctx);
            }
        }));
        let spec = abi::TaskSpec {
            size: abi::size_of::<abi::TaskSpec>(),
            owner: abi::Owner {
                kind: abi::owner::ENTITY,
                world: world.0,
                pos: BlockPos::default().to_abi(),
                entity: entity.to_abi(),
            },
            delay,
            period,
            run: run_task,
            drop: Some(drop_task),
            user: Box::into_raw(both).cast(),
            retired: Some(run_retired),
        };
        let mut id = 0;
        // SAFETY: valid host function and spec. On failure the host has called `drop`.
        Error::check(unsafe { (self.api().schedule)(self.host.id, &spec, &mut id) })?;
        Ok(TaskId(id))
    }

    /// A component of an item (SNBT) as SNBT, defaults of its type included; `None` when it
    /// has none. Since ABI 1.6.
    pub fn item_component(&self, item: &str, component: &str) -> Result<Option<String>, Error> {
        let (api, id) = (self.api(), self.host.id);
        // SAFETY: valid host function, strings and sink.
        let r = collect_with(|sink| unsafe {
            (api.item_component)(id, abi::Str::new(item), abi::Str::new(component), sink)
        });
        match r {
            Ok(bytes) => utf8(bytes).map(Some),
            Err(Error::NotFound) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// The item (SNBT) with a component set to an SNBT value (`None` removes it); the value
    /// is checked as Vanilla reads it. Since ABI 1.6.
    pub fn item_with_component(
        &self,
        item: &str,
        component: &str,
        value: Option<&str>,
    ) -> Result<String, Error> {
        let (api, id) = (self.api(), self.host.id);
        // SAFETY: valid host function, strings and sink.
        utf8(collect_with(|sink| unsafe {
            (api.item_set_component)(
                id,
                abi::Str::new(item),
                abi::Str::new(component),
                abi::Str::new(value.unwrap_or("")),
                sink,
            )
        })?)
    }

    /// The item with its custom name (`minecraft:custom_name`) set. Since ABI 1.6.
    pub fn item_with_name(&self, item: &str, name: &Text) -> Result<String, Error> {
        self.item_with_component(item, "minecraft:custom_name", Some(&text_snbt(name)))
    }

    /// The item with its lore lines set. Since ABI 1.6.
    pub fn item_with_lore(&self, item: &str, lines: &[Text]) -> Result<String, Error> {
        let list: Vec<String> = lines.iter().map(text_snbt).collect();
        self.item_with_component(item, "minecraft:lore", Some(&format!("[{}]", list.join(","))))
    }

    /// The item with its damage set. Since ABI 1.6.
    pub fn item_with_damage(&self, item: &str, damage: i32) -> Result<String, Error> {
        self.item_with_component(item, "minecraft:damage", Some(&damage.to_string()))
    }

    /// The item unbreakable (or breakable again). Since ABI 1.6.
    pub fn item_with_unbreakable(&self, item: &str, unbreakable: bool) -> Result<String, Error> {
        self.item_with_component(item, "minecraft:unbreakable", unbreakable.then_some("{}"))
    }

    /// The item with its enchantments set, e.g. `[("minecraft:sharpness", 5)]`. Since
    /// ABI 1.6.
    pub fn item_with_enchantments(
        &self,
        item: &str,
        enchantments: &[(&str, i32)],
    ) -> Result<String, Error> {
        let map: Vec<String> = enchantments
            .iter()
            .map(|(e, level)| format!("\"{}\":{level}", e.replace('"', "")))
            .collect();
        self.item_with_component(
            item,
            "minecraft:enchantments",
            Some(&format!("{{{}}}", map.join(","))),
        )
    }

    /// A persistent data value of an item (SNBT), stored as Paper stores it
    /// (`minecraft:custom_data` → `PublicBukkitValues`); `None` when it has none. Since
    /// ABI 1.6.
    pub fn item_data(&self, item: &str, key: &str) -> Result<Option<DataValue>, Error> {
        let (api, id) = (self.api(), self.host.id);
        let mut kind = 0;
        // SAFETY: valid host function, strings, output and sink.
        let r = collect_with(|sink| unsafe {
            (api.item_data_get)(
                id,
                abi::Str::new(item),
                abi::Str::new(key),
                &mut kind,
                sink,
            )
        });
        match r {
            Ok(bytes) => Ok(Some(
                DataValue::decode(kind, &bytes).ok_or(Error::InvalidArgument)?,
            )),
            Err(Error::NotFound) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// The item (SNBT) with a persistent data value set. Since ABI 1.6.
    pub fn item_with_data(&self, item: &str, key: &str, value: &DataValue) -> Result<String, Error> {
        let (api, id) = (self.api(), self.host.id);
        let (kind, bytes) = value.encode();
        // SAFETY: valid host function, strings, bytes and sink.
        utf8(collect_with(|sink| unsafe {
            (api.item_data_set)(
                id,
                abi::Str::new(item),
                abi::Str::new(key),
                kind,
                abi::Bytes::new(&bytes),
                sink,
            )
        })?)
    }

    /// The item (SNBT) without a persistent data value. Since ABI 1.6.
    pub fn item_without_data(&self, item: &str, key: &str) -> Result<String, Error> {
        let (api, id) = (self.api(), self.host.id);
        // SAFETY: valid host function, strings and sink.
        utf8(collect_with(|sink| unsafe {
            (api.item_data_remove)(id, abi::Str::new(item), abi::Str::new(key), sink)
        })?)
    }

    /// The persistent data keys of an item. Since ABI 1.6.
    pub fn item_data_keys(&self, item: &str) -> Result<Vec<String>, Error> {
        let (api, id) = (self.api(), self.host.id);
        // SAFETY: valid host function, string and sink.
        let text = utf8(collect_with(|sink| unsafe {
            (api.item_data_keys)(id, abi::Str::new(item), sink)
        })?)?;
        Ok(text
            .lines()
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect())
    }
}

/// A text as the SNBT of a text component.
fn text_snbt(text: &Text) -> String {
    match text {
        Text::Plain(s) => format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")),
        // JSON objects and strings are valid SNBT.
        Text::Json(s) => s.clone(),
    }
}

unsafe extern "C" fn run_retired(user: *mut c_void, ctx: *mut abi::Context) -> Status {
    crate::ffi::guard(|| {
        // SAFETY: `user` is the `Box<Task>` of `schedule_entity`.
        let task = unsafe { &mut *user.cast::<Task>() };
        let mut ctx = Ctx::new(ctx);
        ctx.retiring = true;
        task(&mut ctx);
        status::OK
    })
}

impl Ctx<'_> {
    /// A property ([`abi::entity_prop`]) of an entity (players too) of the owned world. Since
    /// ABI 1.6.
    pub fn entity_prop(&mut self, world: World, entity: Uuid, prop: u32) -> Result<f64, Error> {
        let mut out = 0.0;
        // SAFETY: valid host function, context of this call and output.
        Error::check(unsafe {
            (self.api().entity_get)(self.raw, world.0, entity.to_abi(), prop, &mut out)
        })?;
        Ok(out)
    }

    /// Changes a property of an entity of the owned world. Since ABI 1.6.
    pub fn set_entity_prop(
        &mut self,
        world: World,
        entity: Uuid,
        prop: u32,
        value: f64,
    ) -> Result<(), Error> {
        // SAFETY: valid host function and context of this call.
        Error::check(unsafe {
            (self.api().entity_set)(self.raw, world.0, entity.to_abi(), prop, value)
        })
    }

    /// An entity's health. Since ABI 1.6.
    pub fn health(&mut self, world: World, entity: Uuid) -> Result<f64, Error> {
        self.entity_prop(world, entity, abi::entity_prop::HEALTH)
    }

    /// Sets an entity's health. Since ABI 1.6.
    pub fn set_health(&mut self, world: World, entity: Uuid, health: f64) -> Result<(), Error> {
        self.set_entity_prop(world, entity, abi::entity_prop::HEALTH, health)
    }

    /// Switches a flag property ([`abi::entity_prop`], e.g. `GLOWING`, `NO_AI`, `SILENT`).
    /// Since ABI 1.6.
    pub fn set_entity_flag(
        &mut self,
        world: World,
        entity: Uuid,
        prop: u32,
        on: bool,
    ) -> Result<(), Error> {
        self.set_entity_prop(world, entity, prop, if on { 1.0 } else { 0.0 })
    }

    /// An entity's custom name as JSON; `None` without one. Since ABI 1.6.
    pub fn entity_name(&mut self, world: World, entity: Uuid) -> Result<Option<String>, Error> {
        let (api, raw) = (self.api(), self.raw);
        // SAFETY: valid host function, context of this call and buffer.
        match read_string(|out| unsafe { (api.entity_name)(raw, world.0, entity.to_abi(), out) }) {
            Ok(name) => Ok(Some(name)),
            Err(status::NOT_FOUND) => Ok(None),
            Err(s) => Err(status_err(s)),
        }
    }

    /// Sets (`None`: removes) an entity's custom name. Since ABI 1.6.
    pub fn set_entity_name(
        &mut self,
        world: World,
        entity: Uuid,
        name: Option<Text>,
    ) -> Result<(), Error> {
        let name = name.unwrap_or(Text::Plain(String::new()));
        let (s, format) = name.parts();
        // SAFETY: valid host function, context of this call and string.
        Error::check(unsafe {
            (self.api().entity_set_name)(
                self.raw,
                world.0,
                entity.to_abi(),
                abi::Str::new(s),
                format,
            )
        })
    }

    /// The passengers of an entity, the controlling one first. Since ABI 1.6.
    pub fn passengers(&mut self, world: World, entity: Uuid) -> Result<Vec<Uuid>, Error> {
        let mut buf = vec![abi::Uuid::NIL; 8];
        loop {
            let mut count = 0;
            // SAFETY: valid host function, context of this call and a buffer of `buf.len()`
            // entries.
            Error::check(unsafe {
                (self.api().entity_passengers)(
                    self.raw,
                    world.0,
                    entity.to_abi(),
                    buf.as_mut_ptr(),
                    buf.len(),
                    &mut count,
                )
            })?;
            if count <= buf.len() {
                return Ok(buf.iter().take(count).map(|u| Uuid::from_abi(*u)).collect());
            }
            buf.resize(count, abi::Uuid::NIL);
        }
    }

    /// The vehicle of an entity; `None` when it rides nothing. Since ABI 1.6.
    pub fn vehicle(&mut self, world: World, entity: Uuid) -> Result<Option<Uuid>, Error> {
        let mut out = abi::Uuid::NIL;
        // SAFETY: valid host function, context of this call and output.
        match Error::check(unsafe {
            (self.api().entity_vehicle)(self.raw, world.0, entity.to_abi(), &mut out)
        }) {
            Ok(()) => Ok(Some(Uuid::from_abi(out))),
            Err(Error::NotFound) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Mounts `passenger` on `vehicle` as `/ride mount`. Since ABI 1.6.
    pub fn mount(&mut self, world: World, passenger: Uuid, vehicle: Uuid) -> Result<(), Error> {
        // SAFETY: valid host function and context of this call.
        Error::check(unsafe {
            (self.api().entity_mount)(self.raw, world.0, passenger.to_abi(), vehicle.to_abi())
        })
    }

    /// Dismounts an entity as `/ride dismount`. Since ABI 1.6.
    pub fn dismount(&mut self, world: World, passenger: Uuid) -> Result<(), Error> {
        // SAFETY: valid host function and context of this call.
        Error::check(unsafe { (self.api().entity_dismount)(self.raw, world.0, passenger.to_abi()) })
    }

    /// An attribute (e.g. `minecraft:movement_speed`) of a living entity: its base and its
    /// value. Since ABI 1.6.
    pub fn attribute(
        &mut self,
        world: World,
        entity: Uuid,
        attribute: &str,
    ) -> Result<(f64, f64), Error> {
        let (mut base, mut value) = (0.0, 0.0);
        // SAFETY: valid host function, context of this call, string and outputs.
        Error::check(unsafe {
            (self.api().entity_attribute)(
                self.raw,
                world.0,
                entity.to_abi(),
                abi::Str::new(attribute),
                &mut base,
                &mut value,
            )
        })?;
        Ok((base, value))
    }

    /// Sets an attribute's base value. Since ABI 1.6.
    pub fn set_attribute_base(
        &mut self,
        world: World,
        entity: Uuid,
        attribute: &str,
        value: f64,
    ) -> Result<(), Error> {
        // SAFETY: valid host function, context of this call and string.
        Error::check(unsafe {
            (self.api().entity_attribute_set_base)(
                self.raw,
                world.0,
                entity.to_abi(),
                abi::Str::new(attribute),
                value,
            )
        })
    }

    /// Adds an attribute modifier (operation from [`abi::attribute_op`]); `id` without a
    /// namespace uses the plugin's id. Since ABI 1.6.
    pub fn add_attribute_modifier(
        &mut self,
        world: World,
        entity: Uuid,
        attribute: &str,
        id: &str,
        amount: f64,
        operation: u32,
    ) -> Result<(), Error> {
        self.attribute_modifier(world, entity, attribute, id, amount, operation, true)
    }

    /// Removes an attribute modifier. Since ABI 1.6.
    pub fn remove_attribute_modifier(
        &mut self,
        world: World,
        entity: Uuid,
        attribute: &str,
        id: &str,
    ) -> Result<(), Error> {
        self.attribute_modifier(world, entity, attribute, id, 0.0, 0, false)
    }

    #[allow(clippy::too_many_arguments)]
    fn attribute_modifier(
        &mut self,
        world: World,
        entity: Uuid,
        attribute: &str,
        id: &str,
        amount: f64,
        operation: u32,
        add: bool,
    ) -> Result<(), Error> {
        // SAFETY: valid host function, context of this call and strings.
        Error::check(unsafe {
            (self.api().entity_attribute_modifier)(
                self.raw,
                world.0,
                entity.to_abi(),
                abi::Str::new(attribute),
                abi::Str::new(id),
                amount,
                operation,
                u32::from(add),
            )
        })
    }

    /// The target of a mob. Since ABI 1.6.
    pub fn mob_target(&mut self, world: World, mob: Uuid) -> Result<Option<Uuid>, Error> {
        let mut out = abi::Uuid::NIL;
        // SAFETY: valid host function, context of this call and output.
        match Error::check(unsafe {
            (self.api().mob_target)(self.raw, world.0, mob.to_abi(), &mut out)
        }) {
            Ok(()) => Ok(Some(Uuid::from_abi(out))),
            Err(Error::NotFound) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Sets (`None`: forgets) the target of a mob. Since ABI 1.6.
    pub fn set_mob_target(
        &mut self,
        world: World,
        mob: Uuid,
        target: Option<Uuid>,
    ) -> Result<(), Error> {
        let target = target.map_or(abi::Uuid::NIL, Uuid::to_abi);
        // SAFETY: valid host function and context of this call.
        Error::check(unsafe { (self.api().mob_set_target)(self.raw, world.0, mob.to_abi(), target) })
    }

    /// Lets a mob walk to a position by its path finding (speed modifier 1 normal; 0 stops).
    /// Since ABI 1.6.
    pub fn navigate(&mut self, world: World, mob: Uuid, to: Vec3, speed: f64) -> Result<(), Error> {
        // SAFETY: valid host function and context of this call.
        Error::check(unsafe {
            (self.api().mob_navigate)(self.raw, world.0, mob.to_abi(), to.to_abi(), speed)
        })
    }

    /// Adds a plugin goal to one mob with a priority (lower first) and the
    /// [`abi::goal_flag`] bits it controls; `target` puts it into the target selector.
    /// Returns its id. Since ABI 1.6.
    pub fn add_goal(
        &mut self,
        world: World,
        mob: Uuid,
        priority: i32,
        flags: u32,
        target: bool,
        goal: impl Goal,
    ) -> Result<u64, Error> {
        let user: Box<GoalBox> = Box::new(std::sync::Mutex::new(Box::new(goal)));
        let spec = abi::GoalSpec {
            size: abi::size_of::<abi::GoalSpec>(),
            priority,
            flags,
            target: u32::from(target),
            can_use: crate::game::goal_can_use,
            can_continue: Some(crate::game::goal_can_continue),
            start: Some(crate::game::goal_start),
            stop: Some(crate::game::goal_stop),
            tick: crate::game::goal_tick,
            drop: Some(crate::game::goal_drop),
            user: Box::into_raw(user).cast(),
        };
        let mut id = 0;
        // SAFETY: valid host function, context of this call, spec and output. On failure the
        // host has called `drop`.
        Error::check(unsafe {
            (self.api().mob_goal_add)(self.raw, world.0, mob.to_abi(), &spec, &mut id)
        })?;
        Ok(id)
    }

    /// Removes a plugin goal. Since ABI 1.6.
    pub fn remove_goal(&mut self, world: World, mob: Uuid, goal: u64) -> Result<(), Error> {
        // SAFETY: valid host function and context of this call.
        Error::check(unsafe { (self.api().mob_goal_remove)(self.raw, world.0, mob.to_abi(), goal) })
    }

    /// Removes a mob's Vanilla goals ([`abi::goal_clear`] bits). Since ABI 1.6.
    pub fn clear_goals(&mut self, world: World, mob: Uuid, which: u32) -> Result<(), Error> {
        // SAFETY: valid host function and context of this call.
        Error::check(unsafe { (self.api().mob_goals_clear)(self.raw, world.0, mob.to_abi(), which) })
    }

    /// The biome at a position, e.g. `minecraft:plains`. Since ABI 1.6.
    pub fn biome(&mut self, world: World, pos: BlockPos) -> Result<String, Error> {
        let (api, raw) = (self.api(), self.raw);
        // SAFETY: valid host function, context of this call and buffer.
        read_string(|out| unsafe { (api.biome_get)(raw, world.0, pos.to_abi(), out) })
            .map_err(status_err)
    }

    /// Sets the biome of a box as `/fillbiome`. Since ABI 1.6.
    pub fn set_biome(&mut self, world: World, area: BlockBox, biome: &str) -> Result<(), Error> {
        let area = area.to_abi();
        // SAFETY: valid host function, context of this call, record and string.
        Error::check(unsafe {
            (self.api().biome_set)(self.raw, world.0, &area, abi::Str::new(biome))
        })
    }

    /// The sky and block light (0 to 15) at a position. Since ABI 1.6.
    pub fn light(&mut self, world: World, pos: BlockPos) -> Result<(u32, u32), Error> {
        let (mut sky, mut block) = (0, 0);
        // SAFETY: valid host function, context of this call and outputs.
        Error::check(unsafe {
            (self.api().light_get)(self.raw, world.0, pos.to_abi(), &mut sky, &mut block)
        })?;
        Ok((sky, block))
    }

    /// How far a chunk is loaded ([`abi::chunk_state`]). Since ABI 1.6.
    pub fn chunk_state(&mut self, world: World, x: i32, z: i32) -> Result<u32, Error> {
        let mut out = 0;
        // SAFETY: valid host function, context of this call and output.
        Error::check(unsafe { (self.api().chunk_state)(self.raw, world.0, x, z, &mut out) })?;
        Ok(out)
    }

    /// Force-loads a chunk or ends it, as `/forceload` (saved with the world). Since ABI 1.6.
    pub fn force_load(&mut self, world: World, x: i32, z: i32, on: bool) -> Result<(), Error> {
        // SAFETY: valid host function and context of this call.
        Error::check(unsafe {
            (self.api().chunk_force_load)(self.raw, world.0, x, z, u32::from(on))
        })
    }

    /// Adds or removes the plugin's ticket on a chunk (keeps it loaded and ticking; not saved,
    /// dropped when the plugin is disabled). Since ABI 1.6.
    pub fn chunk_ticket(&mut self, world: World, x: i32, z: i32, add: bool) -> Result<(), Error> {
        // SAFETY: valid host function and context of this call.
        Error::check(unsafe { (self.api().chunk_ticket)(self.raw, world.0, x, z, u32::from(add)) })
    }

    /// The block state ids of a whole chunk column, a snapshot any thread may read (index
    /// `((y - min_y) * 16 + z) * 16 + x`); positions outside loaded chunks read
    /// [`crate::NO_BLOCK`]. Since ABI 1.6.
    pub fn chunk_snapshot(
        &mut self,
        world: World,
        x: i32,
        z: i32,
        min_y: i32,
        height: u32,
    ) -> Result<Vec<u32>, Error> {
        let top = min_y + i32::try_from(height).map_err(|_| Error::InvalidArgument)? - 1;
        let area = BlockBox::new(
            BlockPos::new(x * 16, min_y, z * 16),
            BlockPos::new(x * 16 + 15, top, z * 16 + 15),
        );
        self.blocks(world, area)
    }

    /// The world border. Since ABI 1.6.
    pub fn border(&mut self, world: World) -> Result<WorldBorder, Error> {
        let mut out = WorldBorder {
            center_x: 0.0,
            center_z: 0.0,
            diameter: 0.0,
            target: 0.0,
            lerp_ticks: 0,
            damage_per_block: 0.0,
            safe_zone: 0.0,
            warning_blocks: 0,
            warning_time: 0,
        }
        .to_abi();
        // SAFETY: valid host function, context of this call and output.
        Error::check(unsafe { (self.api().border_get)(self.raw, world.0, &mut out) })?;
        Ok(WorldBorder::from_abi(out))
    }

    /// Changes the world border as `/worldborder`. Since ABI 1.6.
    pub fn set_border(&mut self, world: World, border: WorldBorder) -> Result<(), Error> {
        let border = border.to_abi();
        // SAFETY: valid host function, context of this call and record.
        Error::check(unsafe { (self.api().border_set)(self.raw, world.0, &border) })
    }

    /// An explosion as TNT makes it (power 4 is TNT; flags from [`abi::explode_flag`]). Since
    /// ABI 1.6.
    pub fn explode(
        &mut self,
        world: World,
        center: Vec3,
        power: f32,
        flags: u32,
        source: Option<Uuid>,
    ) -> Result<(), Error> {
        let source = source.map_or(abi::Uuid::NIL, Uuid::to_abi);
        // SAFETY: valid host function and context of this call.
        Error::check(unsafe {
            (self.api().explode)(self.raw, world.0, center.to_abi(), power, flags, source)
        })
    }

    /// The nearest hit of a ray from `from` to `to` (at most 1024 blocks; flags from
    /// [`abi::raytrace_flag`]), ignoring one entity. Since ABI 1.6.
    pub fn raytrace(
        &mut self,
        world: World,
        from: Vec3,
        to: Vec3,
        flags: u32,
        ignore: Option<Uuid>,
    ) -> Result<Option<RayHit>, Error> {
        let mut out = abi::RayHit {
            size: abi::size_of::<abi::RayHit>(),
            kind: abi::ray_hit::MISS,
            pos: from.to_abi(),
            block: BlockPos::default().to_abi(),
            face: 0,
            entity: abi::Uuid::NIL,
        };
        let ignore = ignore.map_or(abi::Uuid::NIL, Uuid::to_abi);
        // SAFETY: valid host function, context of this call and output.
        Error::check(unsafe {
            (self.api().raytrace)(
                self.raw,
                world.0,
                from.to_abi(),
                to.to_abi(),
                flags,
                ignore,
                &mut out,
            )
        })?;
        let pos = Vec3::from_abi(out.pos);
        Ok(match out.kind {
            abi::ray_hit::BLOCK => Some(RayHit::Block(pos, BlockPos::from_abi(out.block), out.face)),
            abi::ray_hit::ENTITY => Some(RayHit::Entity(pos, Uuid::from_abi(out.entity))),
            _ => None,
        })
    }

    /// Opens a plugin menu (`minecraft:generic_9x1` to `generic_9x6`, `generic_3x3`,
    /// `crafter_3x3`, `hopper`, `shulker_box`) for a player with a title and items (SNBT list
    /// of stacks with `Slot`, `""` for none); returns the container id. Clicks fire
    /// [`abi::event::INVENTORY_CLICK`] with [`abi::menu_flag::PLUGIN`]. Since ABI 1.6.
    pub fn open_menu(
        &mut self,
        world: World,
        player: Uuid,
        menu_type: &str,
        title: impl Into<Text>,
        items: &str,
    ) -> Result<u32, Error> {
        let title = title.into();
        let (t, format) = title.parts();
        let spec = abi::MenuSpec {
            size: abi::size_of::<abi::MenuSpec>(),
            menu_type: abi::Str::new(menu_type),
            title: abi::Str::new(t),
            format,
            items: abi::Str::new(items),
        };
        let mut id = 0;
        // SAFETY: valid host function, context of this call, record and output.
        Error::check(unsafe {
            (self.api().menu_open)(self.raw, world.0, player.to_abi(), &spec, &mut id)
        })?;
        Ok(id)
    }

    /// The item in a slot of a player's open menu as SNBT (`{}` for none). Since ABI 1.6.
    pub fn menu_item(&mut self, world: World, player: Uuid, slot: u32) -> Result<String, Error> {
        let (api, raw) = (self.api(), self.raw);
        // SAFETY: valid host function, context of this call and sink.
        utf8(collect_with(|sink| unsafe {
            (api.menu_item)(raw, world.0, player.to_abi(), slot, sink)
        })?)
    }

    /// Puts an item (SNBT, `{}` for none) into a slot of a player's open menu. Since ABI 1.6.
    pub fn set_menu_item(
        &mut self,
        world: World,
        player: Uuid,
        slot: u32,
        item: &str,
    ) -> Result<(), Error> {
        // SAFETY: valid host function, context of this call and string.
        Error::check(unsafe {
            (self.api().menu_set_item)(self.raw, world.0, player.to_abi(), slot, abi::Str::new(item))
        })
    }

    /// Closes a player's open menu. Since ABI 1.6.
    pub fn close_menu(&mut self, world: World, player: Uuid) -> Result<(), Error> {
        // SAFETY: valid host function and context of this call.
        Error::check(unsafe { (self.api().menu_close)(self.raw, world.0, player.to_abi()) })
    }
}

//! The host functions added with ABI 1.6: typed entity values, passengers, attributes, mob
//! targets, navigation and goals; biomes, light, chunk tickets, the world border, explosions
//! and ray traces; chunk populators; plugin menus; item components and item persistent data.

use std::sync::PoisonError;

use vanillite_abi::{self as abi, Status, status};
use vanillite_nbt::{Compound, Tag};

use super::{
    arg, block_box, block_pos, ctx, data_key, data_tag, done, guard, name_arg, on_world, opt_arg,
    snbt, tag_data, text_arg, valid_pos, with, write, write_sink, write_snbt, write_str,
};
use crate::{Menu, PluginGoal};

fn uuid_opt(u: abi::Uuid) -> Option<u128> {
    (u.to_u128() != 0).then(|| u.to_u128())
}

fn vec(v: abi::Vec3) -> [f64; 3] {
    [v.x, v.y, v.z]
}

pub(crate) unsafe extern "C" fn entity_get(
    raw: *mut abi::Context,
    world: u32,
    entity: abi::Uuid,
    prop: u32,
    out: *mut f64,
) -> Status {
    on_world(raw, world, |w| match w.entity_get(entity.to_u128(), prop) {
        Ok(v) => write(out, v),
        Err(s) => s,
    })
}

pub(crate) unsafe extern "C" fn entity_set(
    raw: *mut abi::Context,
    world: u32,
    entity: abi::Uuid,
    prop: u32,
    value: f64,
) -> Status {
    on_world(raw, world, |w| {
        if !value.is_finite() {
            return status::INVALID_ARGUMENT;
        }
        done(w.entity_set(entity.to_u128(), prop, value))
    })
}

pub(crate) unsafe extern "C" fn entity_name(
    raw: *mut abi::Context,
    world: u32,
    entity: abi::Uuid,
    out: *mut abi::StrBuf,
) -> Status {
    on_world(raw, world, |w| match w.entity_name(entity.to_u128()) {
        Ok(Some(name)) => write_str(out, &name),
        Ok(None) => status::NOT_FOUND,
        Err(s) => s,
    })
}

pub(crate) unsafe extern "C" fn entity_set_name(
    raw: *mut abi::Context,
    world: u32,
    entity: abi::Uuid,
    name: abi::Str,
    format: u32,
) -> Status {
    on_world(raw, world, |w| {
        let name = match text_arg(name, format) {
            Ok(t) if !t.as_str().is_empty() => Some(t),
            Ok(_) => None,
            Err(s) => return s,
        };
        done(w.set_entity_name(entity.to_u128(), name))
    })
}

/// Writes up to `cap` UUIDs and the total count.
fn write_uuids(found: &[u128], out: *mut abi::Uuid, cap: usize, count: *mut usize) -> Status {
    if cap > 0 && out.is_null() {
        return status::INVALID_ARGUMENT;
    }
    for (i, uuid) in found.iter().take(cap).enumerate() {
        // SAFETY: the plugin passes `cap` writable entries.
        unsafe { out.add(i).write(abi::Uuid::from_u128(*uuid)) };
    }
    write(count, found.len())
}

pub(crate) unsafe extern "C" fn entity_passengers(
    raw: *mut abi::Context,
    world: u32,
    entity: abi::Uuid,
    out: *mut abi::Uuid,
    cap: usize,
    count: *mut usize,
) -> Status {
    on_world(raw, world, |w| match w.passengers(entity.to_u128()) {
        Ok(found) => write_uuids(&found, out, cap, count),
        Err(s) => s,
    })
}

pub(crate) unsafe extern "C" fn entity_vehicle(
    raw: *mut abi::Context,
    world: u32,
    entity: abi::Uuid,
    out: *mut abi::Uuid,
) -> Status {
    on_world(raw, world, |w| match w.vehicle(entity.to_u128()) {
        Ok(Some(v)) => write(out, abi::Uuid::from_u128(v)),
        Ok(None) => status::NOT_FOUND,
        Err(s) => s,
    })
}

pub(crate) unsafe extern "C" fn entity_mount(
    raw: *mut abi::Context,
    world: u32,
    passenger: abi::Uuid,
    vehicle: abi::Uuid,
) -> Status {
    on_world(raw, world, |w| {
        done(w.mount(passenger.to_u128(), vehicle.to_u128()))
    })
}

pub(crate) unsafe extern "C" fn entity_dismount(
    raw: *mut abi::Context,
    world: u32,
    passenger: abi::Uuid,
) -> Status {
    on_world(raw, world, |w| done(w.dismount(passenger.to_u128())))
}

pub(crate) unsafe extern "C" fn entity_attribute(
    raw: *mut abi::Context,
    world: u32,
    entity: abi::Uuid,
    attribute: abi::Str,
    base: *mut f64,
    value: *mut f64,
) -> Status {
    on_world(raw, world, |w| {
        let Ok(attribute) = name_arg(attribute) else {
            return status::INVALID_ARGUMENT;
        };
        if base.is_null() || value.is_null() {
            return status::INVALID_ARGUMENT;
        }
        match w.attribute(entity.to_u128(), attribute) {
            Ok((b, v)) => match write(base, b) {
                status::OK => write(value, v),
                s => s,
            },
            Err(s) => s,
        }
    })
}

pub(crate) unsafe extern "C" fn entity_attribute_set_base(
    raw: *mut abi::Context,
    world: u32,
    entity: abi::Uuid,
    attribute: abi::Str,
    value: f64,
) -> Status {
    on_world(raw, world, |w| {
        let Ok(attribute) = name_arg(attribute) else {
            return status::INVALID_ARGUMENT;
        };
        if !value.is_finite() {
            return status::INVALID_ARGUMENT;
        }
        done(w.set_attribute_base(entity.to_u128(), attribute, value))
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) unsafe extern "C" fn entity_attribute_modifier(
    raw: *mut abi::Context,
    world: u32,
    entity: abi::Uuid,
    attribute: abi::Str,
    id: abi::Str,
    amount: f64,
    operation: u32,
    add: u32,
) -> Status {
    guard(|| {
        // SAFETY: the plugin passes the context of the running callback.
        let Some(plugin) = (unsafe { ctx(raw) }).map(|c| c.plugin) else {
            return status::INVALID_ARGUMENT;
        };
        let Some(id) = data_key(plugin, id) else {
            return status::INVALID_ARGUMENT;
        };
        on_world(raw, world, |w| {
            let Ok(attribute) = name_arg(attribute) else {
                return status::INVALID_ARGUMENT;
            };
            let change = if add != 0 {
                if !amount.is_finite() || operation > abi::attribute_op::ADD_MULTIPLIED_TOTAL {
                    return status::INVALID_ARGUMENT;
                }
                Some((amount, operation))
            } else {
                None
            };
            done(w.attribute_modifier(entity.to_u128(), attribute, &id, change))
        })
    })
}

pub(crate) unsafe extern "C" fn mob_target(
    raw: *mut abi::Context,
    world: u32,
    mob: abi::Uuid,
    out: *mut abi::Uuid,
) -> Status {
    on_world(raw, world, |w| match w.mob_target(mob.to_u128()) {
        Ok(Some(t)) => write(out, abi::Uuid::from_u128(t)),
        Ok(None) => status::NOT_FOUND,
        Err(s) => s,
    })
}

pub(crate) unsafe extern "C" fn mob_set_target(
    raw: *mut abi::Context,
    world: u32,
    mob: abi::Uuid,
    target: abi::Uuid,
) -> Status {
    on_world(raw, world, |w| {
        done(w.set_mob_target(mob.to_u128(), uuid_opt(target)))
    })
}

pub(crate) unsafe extern "C" fn mob_navigate(
    raw: *mut abi::Context,
    world: u32,
    mob: abi::Uuid,
    to: abi::Vec3,
    speed: f64,
) -> Status {
    on_world(raw, world, |w| {
        let to = vec(to);
        if !valid_pos(to) || !speed.is_finite() || speed > 100.0 {
            return status::INVALID_ARGUMENT;
        }
        done(w.navigate(mob.to_u128(), to, speed))
    })
}

pub(crate) unsafe extern "C" fn mob_goal_add(
    raw: *mut abi::Context,
    world: u32,
    mob: abi::Uuid,
    spec: *const abi::GoalSpec,
    out: *mut u64,
) -> Status {
    guard(|| {
        // SAFETY: the plugin passes a valid spec or null.
        let Some(spec) = (unsafe { spec.as_ref() }) else {
            return status::INVALID_ARGUMENT;
        };
        let drop_user = || {
            if let Some(drop) = spec.drop {
                // SAFETY: the plugin's drop function for its own user data.
                unsafe { drop(spec.user) };
            }
        };
        if (spec.size as usize) < size_of::<abi::GoalSpec>()
            || spec.flags & !0xF != 0
            || out.is_null()
        {
            drop_user();
            return status::INVALID_ARGUMENT;
        }
        // SAFETY: the plugin passes the context of the running callback.
        let Some(plugin) = (unsafe { ctx(raw) }).and_then(|c| {
            crate::Inner::resolve(c.plugin.abi_id).map(|(_, p)| p)
        }) else {
            drop_user();
            return status::INVALID_ARGUMENT;
        };
        // From here the goal owns the user data: dropping it frees them.
        let goal = PluginGoal::new(plugin, *spec);
        on_world(raw, world, |w| match w.add_goal(mob.to_u128(), goal) {
            Ok(id) => write(out, id),
            Err(s) => s,
        })
    })
}

pub(crate) unsafe extern "C" fn mob_goal_remove(
    raw: *mut abi::Context,
    world: u32,
    mob: abi::Uuid,
    goal: u64,
) -> Status {
    on_world(raw, world, |w| done(w.remove_goal(mob.to_u128(), goal)))
}

pub(crate) unsafe extern "C" fn mob_goals_clear(
    raw: *mut abi::Context,
    world: u32,
    mob: abi::Uuid,
    which: u32,
) -> Status {
    on_world(raw, world, |w| {
        if which & !(abi::goal_clear::GOALS | abi::goal_clear::TARGETS) != 0 {
            return status::INVALID_ARGUMENT;
        }
        done(w.clear_goals(mob.to_u128(), which))
    })
}

pub(crate) unsafe extern "C" fn biome_get(
    raw: *mut abi::Context,
    world: u32,
    pos: abi::BlockPos,
    out: *mut abi::StrBuf,
) -> Status {
    on_world(raw, world, |w| match w.biome(block_pos(pos)) {
        Ok(b) => write_str(out, &b),
        Err(s) => s,
    })
}

pub(crate) unsafe extern "C" fn biome_set(
    raw: *mut abi::Context,
    world: u32,
    area: *const abi::BlockBox,
    biome: abi::Str,
) -> Status {
    on_world(raw, world, |w| {
        let (Some((area, _)), Ok(biome)) = (block_box(area), name_arg(biome)) else {
            return status::INVALID_ARGUMENT;
        };
        done(w.set_biome(area, biome))
    })
}

pub(crate) unsafe extern "C" fn light_get(
    raw: *mut abi::Context,
    world: u32,
    pos: abi::BlockPos,
    sky: *mut u32,
    block: *mut u32,
) -> Status {
    on_world(raw, world, |w| {
        if sky.is_null() || block.is_null() {
            return status::INVALID_ARGUMENT;
        }
        match w.light(block_pos(pos)) {
            Ok((s, b)) => match write(sky, s) {
                status::OK => write(block, b),
                st => st,
            },
            Err(s) => s,
        }
    })
}

pub(crate) unsafe extern "C" fn chunk_state(
    raw: *mut abi::Context,
    world: u32,
    x: i32,
    z: i32,
    out: *mut u32,
) -> Status {
    on_world(raw, world, |w| match w.chunk_state(x, z) {
        Ok(state) => write(out, state),
        Err(s) => s,
    })
}

/// Whether chunk coordinates are inside the world's coordinate limits.
fn valid_chunk(x: i32, z: i32) -> bool {
    x.unsigned_abs() < 1_875_000 && z.unsigned_abs() < 1_875_000
}

pub(crate) unsafe extern "C" fn chunk_force_load(
    raw: *mut abi::Context,
    world: u32,
    x: i32,
    z: i32,
    on: u32,
) -> Status {
    on_world(raw, world, |w| {
        if !valid_chunk(x, z) {
            return status::INVALID_ARGUMENT;
        }
        done(w.force_load(x, z, on != 0))
    })
}

pub(crate) unsafe extern "C" fn chunk_ticket(
    raw: *mut abi::Context,
    world: u32,
    x: i32,
    z: i32,
    add: u32,
) -> Status {
    guard(|| {
        // SAFETY: the plugin passes the context of the running callback.
        let Some(plugin) = (unsafe { ctx(raw) }).map(|c| c.plugin.id().to_owned()) else {
            return status::INVALID_ARGUMENT;
        };
        on_world(raw, world, |w| {
            if !valid_chunk(x, z) {
                return status::INVALID_ARGUMENT;
            }
            done(w.chunk_ticket(&plugin, x, z, add != 0))
        })
    })
}

pub(crate) unsafe extern "C" fn border_get(
    raw: *mut abi::Context,
    world: u32,
    out: *mut abi::BorderInfo,
) -> Status {
    on_world(raw, world, |w| match w.border() {
        Ok(b) => write(
            out,
            abi::BorderInfo {
                size: abi::size_of::<abi::BorderInfo>(),
                ..b
            },
        ),
        Err(s) => s,
    })
}

pub(crate) unsafe extern "C" fn border_set(
    raw: *mut abi::Context,
    world: u32,
    border: *const abi::BorderInfo,
) -> Status {
    on_world(raw, world, |w| {
        // SAFETY: the plugin passes a valid record or null.
        let Some(b) = (unsafe { border.as_ref() }) else {
            return status::INVALID_ARGUMENT;
        };
        let finite = [
            b.center_x,
            b.center_z,
            b.diameter,
            b.target,
            b.damage_per_block,
            b.safe_zone,
        ]
        .iter()
        .all(|v| v.is_finite());
        if (b.size as usize) < size_of::<abi::BorderInfo>() || !finite || b.lerp_ticks < 0 {
            return status::INVALID_ARGUMENT;
        }
        done(w.set_border(b))
    })
}

pub(crate) unsafe extern "C" fn explode(
    raw: *mut abi::Context,
    world: u32,
    center: abi::Vec3,
    power: f32,
    flags: u32,
    source: abi::Uuid,
) -> Status {
    on_world(raw, world, |w| {
        let center = vec(center);
        let known = abi::explode_flag::BREAK_BLOCKS | abi::explode_flag::FIRE;
        if !valid_pos(center) || !(0.0..=128.0).contains(&power) || flags & !known != 0 {
            return status::INVALID_ARGUMENT;
        }
        done(w.explode(center, power, flags, uuid_opt(source)))
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) unsafe extern "C" fn raytrace(
    raw: *mut abi::Context,
    world: u32,
    from: abi::Vec3,
    to: abi::Vec3,
    flags: u32,
    ignore: abi::Uuid,
    out: *mut abi::RayHit,
) -> Status {
    on_world(raw, world, |w| {
        let (from, to) = (vec(from), vec(to));
        let distance = from
            .iter()
            .zip(&to)
            .map(|(a, b)| (a - b) * (a - b))
            .sum::<f64>()
            .sqrt();
        let known = abi::raytrace_flag::BLOCKS
            | abi::raytrace_flag::FLUIDS
            | abi::raytrace_flag::ENTITIES;
        // SEC: a ray crosses at most 1024 blocks.
        if !valid_pos(from) || !valid_pos(to) || distance > 1024.0 || flags & !known != 0 {
            return status::INVALID_ARGUMENT;
        }
        match w.raytrace(from, to, flags, uuid_opt(ignore)) {
            Ok(hit) => write(
                out,
                abi::RayHit {
                    size: abi::size_of::<abi::RayHit>(),
                    ..hit
                },
            ),
            Err(s) => s,
        }
    })
}

pub(crate) unsafe extern "C" fn provide_chunk_populator(
    id: abi::PluginId,
    table: *const abi::ChunkPopulatorProvider,
) -> Status {
    with(id, |inner, plugin| {
        if inner.is_reloading(plugin) {
            log::warn!(
                "[{}] a new chunk populator takes effect after a restart",
                plugin.id()
            );
            return status::OK;
        }
        if !inner.may_register(plugin) {
            return status::WRONG_PHASE;
        }
        // SAFETY: the plugin passes a valid table or null.
        let Some(table) = (unsafe { table.as_ref() }) else {
            return status::INVALID_ARGUMENT;
        };
        if (table.size as usize) < size_of::<abi::ChunkPopulatorProvider>()
            || !matches!(
                table.stage,
                abi::gen_stage::AFTER_SURFACE | abi::gen_stage::AFTER_FEATURES
            )
        {
            return status::INVALID_ARGUMENT;
        }
        if table.data_version != 0 && table.data_version != inner.settings.data_version {
            log::error!(
                "Plugin {}: its chunk populator is built for data version {}, this server has {}",
                plugin.id(),
                table.data_version,
                inner.settings.data_version
            );
            return status::UNSUPPORTED;
        }
        inner
            .populators
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((plugin.local, *table));
        status::OK
    })
}

/// The longest menu title in bytes.
const MAX_TITLE: usize = 32 * 1024;

pub(crate) unsafe extern "C" fn menu_open(
    raw: *mut abi::Context,
    world: u32,
    player: abi::Uuid,
    spec: *const abi::MenuSpec,
    out: *mut u32,
) -> Status {
    on_world(raw, world, |w| {
        // SAFETY: the plugin passes a valid record or null.
        let Some(spec) = (unsafe { spec.as_ref() }) else {
            return status::INVALID_ARGUMENT;
        };
        if (spec.size as usize) < size_of::<abi::MenuSpec>() || out.is_null() {
            return status::INVALID_ARGUMENT;
        }
        let (Ok(menu_type), Ok(title), Ok(items)) = (
            name_arg(spec.menu_type),
            text_arg(spec.title, spec.format),
            arg(spec.items),
        ) else {
            return status::INVALID_ARGUMENT;
        };
        if title.as_str().len() > MAX_TITLE || items.len() > abi::limits::MAX_SNBT as usize {
            return status::INVALID_ARGUMENT;
        }
        let menu = Menu {
            menu_type,
            title,
            items,
        };
        match w.open_menu(player.to_u128(), &menu) {
            Ok(id) => write(out, id),
            Err(s) => s,
        }
    })
}

pub(crate) unsafe extern "C" fn menu_item(
    raw: *mut abi::Context,
    world: u32,
    player: abi::Uuid,
    slot: u32,
    sink: *const abi::ByteSink,
) -> Status {
    on_world(raw, world, |w| match w.menu_item(player.to_u128(), slot) {
        Ok(item) => write_snbt(sink, item),
        Err(s) => s,
    })
}

pub(crate) unsafe extern "C" fn menu_set_item(
    raw: *mut abi::Context,
    world: u32,
    player: abi::Uuid,
    slot: u32,
    item: abi::Str,
) -> Status {
    on_world(raw, world, |w| match snbt(item) {
        Ok(item) => done(w.set_menu_item(player.to_u128(), slot, &item)),
        Err(s) => s,
    })
}

pub(crate) unsafe extern "C" fn menu_close(
    raw: *mut abi::Context,
    world: u32,
    player: abi::Uuid,
) -> Status {
    on_world(raw, world, |w| done(w.close_menu(player.to_u128())))
}

pub(crate) unsafe extern "C" fn item_component(
    id: abi::PluginId,
    item: abi::Str,
    component: abi::Str,
    sink: *const abi::ByteSink,
) -> Status {
    with(id, |inner, _| {
        let (Ok(item), Ok(component)) = (snbt(item), name_arg(component)) else {
            return status::INVALID_ARGUMENT;
        };
        let Some(server) = inner.server() else {
            return status::WRONG_PHASE;
        };
        match server.item_component(&item, component) {
            Ok(Some(tag)) => write_sink(sink, vanillite_nbt::snbt::to_string(&tag).as_bytes()),
            Ok(None) => status::NOT_FOUND,
            Err(s) => s,
        }
    })
}

pub(crate) unsafe extern "C" fn item_set_component(
    id: abi::PluginId,
    item: abi::Str,
    component: abi::Str,
    value: abi::Str,
    sink: *const abi::ByteSink,
) -> Status {
    with(id, |inner, _| {
        let (Ok(item), Ok(component), Ok(value)) =
            (snbt(item), name_arg(component), opt_arg(value))
        else {
            return status::INVALID_ARGUMENT;
        };
        if value.is_some_and(|v| v.len() > abi::limits::MAX_SNBT as usize) {
            return status::INVALID_ARGUMENT;
        }
        let value = match value.map(vanillite_nbt::snbt::parse) {
            Some(Ok(tag)) => Some(tag),
            Some(Err(_)) => return status::INVALID_ARGUMENT,
            None => None,
        };
        let Some(server) = inner.server() else {
            return status::WRONG_PHASE;
        };
        match server.item_set_component(&item, component, value.as_ref()) {
            Ok(item) => write_snbt(sink, item),
            Err(s) => s,
        }
    })
}

/// The persistent data of an item as Paper stores it: `components."minecraft:custom_data"
/// .PublicBukkitValues`.
const CUSTOM_DATA: &str = "minecraft:custom_data";
const PUBLIC_VALUES: &str = "PublicBukkitValues";

fn item_values(item: &Compound) -> Option<&Compound> {
    match item
        .get("components")?
        .as_compound()?
        .get(CUSTOM_DATA)?
        .as_compound()?
        .get(PUBLIC_VALUES)?
    {
        Tag::Compound(c) => Some(c),
        _ => None,
    }
}

/// Changes an item's persistent data and drops empty containers again.
fn edit_values(item: &mut Compound, f: impl FnOnce(&mut Compound) -> Status) -> Status {
    let mut components = match item.remove("components") {
        Some(Tag::Compound(c)) => c,
        _ => Compound::new(),
    };
    let mut custom = match components.remove(CUSTOM_DATA) {
        Some(Tag::Compound(c)) => c,
        _ => Compound::new(),
    };
    let mut values = match custom.remove(PUBLIC_VALUES) {
        Some(Tag::Compound(c)) => c,
        _ => Compound::new(),
    };
    let s = f(&mut values);
    if !values.is_empty() {
        custom.insert(PUBLIC_VALUES.to_owned(), Tag::Compound(values));
    }
    if !custom.is_empty() {
        components.insert(CUSTOM_DATA.to_owned(), Tag::Compound(custom));
    }
    if !components.is_empty() {
        item.insert("components".to_owned(), Tag::Compound(components));
    }
    s
}

pub(crate) unsafe extern "C" fn item_data_get(
    id: abi::PluginId,
    item: abi::Str,
    key: abi::Str,
    kind: *mut u32,
    sink: *const abi::ByteSink,
) -> Status {
    with(id, |_, plugin| {
        let (Ok(item), Some(key)) = (snbt(item), data_key(plugin, key)) else {
            return status::INVALID_ARGUMENT;
        };
        let Some(tag) = item_values(&item).and_then(|v| v.get(&key)) else {
            return status::NOT_FOUND;
        };
        let Some((t, bytes)) = tag_data(tag) else {
            return status::UNSUPPORTED;
        };
        match write(kind, t) {
            status::OK => write_sink(sink, &bytes),
            s => s,
        }
    })
}

pub(crate) unsafe extern "C" fn item_data_set(
    id: abi::PluginId,
    item: abi::Str,
    key: abi::Str,
    kind: u32,
    value: abi::Bytes,
    sink: *const abi::ByteSink,
) -> Status {
    with(id, |_, plugin| {
        let (Ok(mut item), Some(key)) = (snbt(item), data_key(plugin, key)) else {
            return status::INVALID_ARGUMENT;
        };
        if value.len > abi::data_limits::MAX_VALUE as usize {
            return status::INVALID_ARGUMENT;
        }
        // SAFETY: the plugin passes `len` readable bytes.
        let Some(tag) = (unsafe { super::bytes(value) }).and_then(|b| data_tag(kind, b)) else {
            return status::INVALID_ARGUMENT;
        };
        let s = edit_values(&mut item, |values| {
            if values.get(&key).is_none() && values.len() >= abi::data_limits::MAX_KEYS as usize {
                return status::INVALID_ARGUMENT;
            }
            values.insert(key, tag);
            status::OK
        });
        match s {
            status::OK => write_snbt(sink, item),
            s => s,
        }
    })
}

pub(crate) unsafe extern "C" fn item_data_remove(
    id: abi::PluginId,
    item: abi::Str,
    key: abi::Str,
    sink: *const abi::ByteSink,
) -> Status {
    with(id, |_, plugin| {
        let (Ok(mut item), Some(key)) = (snbt(item), data_key(plugin, key)) else {
            return status::INVALID_ARGUMENT;
        };
        edit_values(&mut item, |values| {
            values.remove(&key);
            status::OK
        });
        write_snbt(sink, item)
    })
}

pub(crate) unsafe extern "C" fn item_data_keys(
    id: abi::PluginId,
    item: abi::Str,
    sink: *const abi::ByteSink,
) -> Status {
    with(id, |_, _| {
        let Ok(item) = snbt(item) else {
            return status::INVALID_ARGUMENT;
        };
        let keys = item_values(&item)
            .map(|v| v.iter().map(|(k, _)| k).collect::<Vec<_>>().join("\n"))
            .unwrap_or_default();
        write_sink(sink, keys.as_bytes())
    })
}

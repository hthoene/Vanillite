//! Entity data (`set_entity_data`) and particles between versions with the same serializers
//! and particle option codecs: block states, items and particle types inside are rewritten.

use crate::Result;
use crate::io;
use crate::items::{self, Maps};
use crate::table::Remap;
use vanillite_protocol::Error;
use vanillite_protocol::codec::{Reader, write_varint};
use vanillite_protocol::components::{Ref, registry_refs};

/// The id tables entity data and particles need.
pub(crate) struct Tables<'a> {
    pub maps: &'a dyn Maps,
    pub block_state: &'static Remap,
    pub particle_type: &'static Remap,
    /// Particle type names of the newer version, by id (to find the option codec).
    pub particle_names: &'static [&'static str],
    /// Data component type ids of the newer version used to parse shared codecs.
    pub profile_component: u16,
    pub painting_component: u16,
}

/// `EntityDataSerializers` ids (registration order).
mod serializer {
    pub const BYTE: i32 = 0;
    pub const INT: i32 = 1;
    pub const LONG: i32 = 2;
    pub const FLOAT: i32 = 3;
    pub const STRING: i32 = 4;
    pub const COMPONENT: i32 = 5;
    pub const OPTIONAL_COMPONENT: i32 = 6;
    pub const ITEM_STACK: i32 = 7;
    pub const BOOLEAN: i32 = 8;
    pub const ROTATIONS: i32 = 9;
    pub const BLOCK_POS: i32 = 10;
    pub const OPTIONAL_BLOCK_POS: i32 = 11;
    pub const DIRECTION: i32 = 12;
    pub const OPTIONAL_LIVING_ENTITY_REFERENCE: i32 = 13;
    pub const BLOCK_STATE: i32 = 14;
    pub const OPTIONAL_BLOCK_STATE: i32 = 15;
    pub const PARTICLE: i32 = 16;
    pub const PARTICLES: i32 = 17;
    pub const VILLAGER_DATA: i32 = 18;
    pub const OPTIONAL_UNSIGNED_INT: i32 = 19;
    pub const POSE: i32 = 20;
    /// Cat, cat sound, cow, cow sound, wolf, wolf sound, frog, pig, pig sound, chicken, chicken
    /// sound, zombie nautilus variants: registry ids.
    pub const FIRST_VARIANT: i32 = 21;
    pub const LAST_VARIANT: i32 = 32;
    pub const OPTIONAL_GLOBAL_POS: i32 = 33;
    pub const PAINTING_VARIANT: i32 = 34;
    /// Sniffer, armadillo, copper golem, weathering copper states.
    pub const FIRST_STATE: i32 = 35;
    pub const LAST_STATE: i32 = 38;
    pub const VECTOR3: i32 = 39;
    pub const QUATERNION: i32 = 40;
    pub const RESOLVABLE_PROFILE: i32 = 41;
    pub const HUMANOID_ARM: i32 = 42;
}

/// Serializers the older version (26.2) knows: all before `DYE_COLOR` (43).
const OLD_SERIALIZERS: i32 = 43;

/// Rewrites the items of `set_entity_data` (after the entity id) to the older version. Items of
/// a serializer the older version lacks, or with a value it cannot show, are left out.
pub(crate) fn entity_data(r: &mut Reader<'_>, out: &mut Vec<u8>, t: &Tables<'_>) -> Result<()> {
    let mut refs = Vec::new();
    for _ in 0..256 {
        let index = r.u8()?;
        if index == 0xff {
            out.push(0xff);
            return Ok(());
        }
        let kind = r.varint()?;
        let mut value = Vec::new();
        let keep = value_to_old(kind, r, &mut value, t, &mut refs)?;
        if keep && kind < OLD_SERIALIZERS {
            out.push(index);
            write_varint(out, kind);
            out.extend_from_slice(&value);
        }
    }
    Err(Error::Invalid("entity data without end"))
}

fn value_to_old(
    kind: i32,
    r: &mut Reader<'_>,
    out: &mut Vec<u8>,
    t: &Tables<'_>,
    refs: &mut Vec<Ref>,
) -> Result<bool> {
    use serializer as s;
    match kind {
        s::BYTE | s::BOOLEAN => io::bytes(r, out, 1)?,
        s::INT
        | s::DIRECTION
        | s::POSE
        | s::OPTIONAL_UNSIGNED_INT
        | s::HUMANOID_ARM
        | s::FIRST_VARIANT..=s::LAST_VARIANT
        | s::FIRST_STATE..=s::LAST_STATE => {
            io::varint(r, out)?;
        }
        s::LONG => {
            write_varint_long(out, r.varlong()?);
        }
        s::FLOAT => io::bytes(r, out, 4)?,
        s::STRING => io::string(r, out)?,
        s::COMPONENT => io::nbt(r, out)?,
        s::OPTIONAL_COMPONENT => {
            if io::boolean(r, out)? {
                io::nbt(r, out)?;
            }
        }
        s::ITEM_STACK => items::stack_to_old(r, out, t.maps, refs)?,
        s::ROTATIONS => io::bytes(r, out, 12)?,
        s::BLOCK_POS => io::bytes(r, out, 8)?,
        s::OPTIONAL_BLOCK_POS => {
            if io::boolean(r, out)? {
                io::bytes(r, out, 8)?;
            }
        }
        s::OPTIONAL_LIVING_ENTITY_REFERENCE => {
            if io::boolean(r, out)? {
                io::bytes(r, out, 16)?;
            }
        }
        s::BLOCK_STATE | s::OPTIONAL_BLOCK_STATE => {
            // The optional form writes 0 (air) for none.
            let state = r.varint()?;
            write_varint(out, t.block_state.older(state).unwrap_or(0));
        }
        s::PARTICLE => return particle(r, out, t, refs),
        s::PARTICLES => {
            let n = io::len(r, 1024, 1)?;
            let mut list = Vec::new();
            let mut kept = 0usize;
            for _ in 0..n {
                if particle(r, &mut list, t, refs)? {
                    kept = kept.saturating_add(1);
                }
            }
            io::write_len(out, kept);
            out.extend_from_slice(&list);
        }
        s::VILLAGER_DATA => {
            for _ in 0..3 {
                io::varint(r, out)?;
            }
        }
        s::OPTIONAL_GLOBAL_POS => {
            if io::boolean(r, out)? {
                io::string(r, out)?;
                io::bytes(r, out, 8)?;
            }
        }
        // Same codecs as the `painting/variant` and `profile` data components.
        s::PAINTING_VARIANT => shared_codec(t.painting_component, r, out, t, refs)?,
        s::RESOLVABLE_PROFILE => shared_codec(t.profile_component, r, out, t, refs)?,
        s::VECTOR3 => io::bytes(r, out, 12)?,
        s::QUATERNION => io::bytes(r, out, 16)?,
        // DYE_COLOR (26.3 only) and anything newer: a VarInt enum; dropped by the caller.
        43 => {
            io::varint(r, out)?;
            return Ok(false);
        }
        _ => return Err(Error::Invalid("entity data serializer")),
    }
    Ok(true)
}

fn write_varint_long(out: &mut Vec<u8>, v: i64) {
    vanillite_protocol::codec::write_varlong(out, v);
}

/// A value with the codec of a data component: parsed and rewritten like one.
fn shared_codec(
    component: u16,
    r: &mut Reader<'_>,
    out: &mut Vec<u8>,
    t: &Tables<'_>,
    refs: &mut Vec<Ref>,
) -> Result<()> {
    refs.clear();
    let data = r.rest();
    let len = registry_refs(component, data, refs)?;
    let mut value = Vec::new();
    // Rewritten as the value of a patch-less component: only plain ids can occur.
    let slice = data.get(..len).ok_or(Error::UnexpectedEnd)?;
    let mut pos = 0usize;
    for rf in refs.iter() {
        if let Ref::Id {
            registry,
            start,
            end,
            id,
            plus_one,
        } = *rf
        {
            value.extend_from_slice(slice.get(pos..start).ok_or(Error::UnexpectedEnd)?);
            let raw = if plus_one { id.saturating_sub(1) } else { id };
            let new = match t.maps.registry(registry) {
                Some(remap) => remap.older(raw).unwrap_or(0),
                None => raw,
            };
            write_varint(
                &mut value,
                if plus_one { new.saturating_add(1) } else { new },
            );
            pos = end;
        }
    }
    value.extend_from_slice(slice.get(pos..).ok_or(Error::UnexpectedEnd)?);
    out.extend_from_slice(&value);
    r.skip(len)
}

/// `ParticleTypes.STREAM_CODEC`: a type id and its options. `false` if the older version lacks
/// the type (nothing is written).
pub(crate) fn particle(
    r: &mut Reader<'_>,
    out: &mut Vec<u8>,
    t: &Tables<'_>,
    refs: &mut Vec<Ref>,
) -> Result<bool> {
    let id = r.varint()?;
    let name = usize::try_from(id)
        .ok()
        .and_then(|i| t.particle_names.get(i))
        .copied()
        .ok_or(Error::Invalid("particle type"))?;
    let old = t.particle_type.older(id);
    let mut options = Vec::new();
    particle_options(name, r, &mut options, t, refs)?;
    match old {
        Some(old) => {
            write_varint(out, old);
            out.extend_from_slice(&options);
            Ok(true)
        }
        None => Ok(false),
    }
}

/// The options of a particle type (26.3 `ParticleTypes`).
fn particle_options(
    name: &str,
    r: &mut Reader<'_>,
    out: &mut Vec<u8>,
    t: &Tables<'_>,
    refs: &mut Vec<Ref>,
) -> Result<()> {
    match name.trim_start_matches("minecraft:") {
        "block" | "block_marker" | "falling_dust" | "dust_pillar" | "block_crumble" => {
            let state = r.varint()?;
            write_varint(out, t.block_state.older(state).unwrap_or(0));
        }
        // GeyserParticleOptions: water blocks.
        "geyser" | "geyser_plume" => io::bytes(r, out, 4)?,
        // GeyserBaseParticleOptions: water blocks, burst impulse.
        "geyser_base" | "geyser_poof" => io::bytes(r, out, 8)?,
        // PowerParticleOption, SculkChargeParticleOptions.
        "dragon_breath" | "sculk_charge" => io::bytes(r, out, 4)?,
        // DustParticleOptions, SpellParticleOption: color, float.
        "dust" | "effect" | "instant_effect" => io::bytes(r, out, 8)?,
        "dust_color_transition" => io::bytes(r, out, 12)?,
        // ColorParticleOption.
        "entity_effect" | "tinted_leaves" | "flash" => io::bytes(r, out, 4)?,
        "item" => items::template_to_old(r, out, t.maps, refs)?,
        "vibration" => {
            // PositionSource: type id, then a block position or an entity id and a y offset.
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
            io::varint(r, out)?;
        }
        "shriek" => {
            io::varint(r, out)?;
        }
        _ => {}
    }
    Ok(())
}

//! Mooshrooms (`MushroomCow`): cows of the mushroom fields that give stew and suspicious
//! stew, turn into cows when sheared and switch colour when lightning strikes.

use vanillite_base::Vec3;
use vanillite_base::random::{LegacyRandom, RandomSource};
use vanillite_nbt::{Compound, List, Tag};
use vanillite_protocol::packets::play::clientbound::SoundSource;
use vanillite_world::entity::Rotation;

use super::{st, st_mut};
use crate::item::ItemStack;
use crate::mob::MobData;
use crate::mob::animal::{Interaction, animal_interact, fill_held, hurt_held, sound_at, use_item};
use crate::mob::effects::id as effect;
use crate::mob::fam::{Hurt, Interact};
use crate::mob::goals::{Goal as MobGoal, TemptItems};
use crate::mob::kinds::Kind;
use crate::survival::Effect;

/// `AbstractCow.registerGoals`.
pub(super) fn register_goals(m: &mut MobData) {
    let g = &mut m.goals;
    g.add(0, MobGoal::Float);
    g.add(1, MobGoal::panic(2.0));
    g.add(2, MobGoal::breed(1.0));
    g.add(3, MobGoal::tempt(1.25, TemptItems::Food));
    g.add(
        4,
        MobGoal::FollowParent {
            speed: 1.25,
            parent: None,
            recalc: 0,
        },
    );
    g.add(5, MobGoal::stroll(1.0));
    g.add(6, MobGoal::look_at_player(6.0));
    g.add(7, MobGoal::random_look());
    m.nav.can_float = true;
}

/// The suspicious stew effect of a flower item (`SuspiciousEffectHolder`), in ticks.
pub(crate) fn flower_effect(name: &str) -> Option<(u16, i32)> {
    let name = name.strip_prefix("minecraft:").unwrap_or(name);
    Some(match name {
        "dandelion" | "golden_dandelion" | "blue_orchid" => (effect::SATURATION, 7),
        "torchflower" | "poppy" | "cornflower" => {
            if name == "cornflower" {
                (effect::JUMP_BOOST, 100)
            } else {
                (effect::NIGHT_VISION, 100)
            }
        }
        "allium" => (effect::FIRE_RESISTANCE, 60),
        "azure_bluet" | "open_eyeblossom" => (effect::BLINDNESS, 220),
        "red_tulip" | "orange_tulip" | "white_tulip" | "pink_tulip" => (effect::WEAKNESS, 140),
        "oxeye_daisy" => (effect::REGENERATION, 140),
        "wither_rose" => (effect::WITHER, 140),
        "lily_of_the_valley" => (effect::POISON, 220),
        "closed_eyeblossom" => (effect::NAUSEA, 140),
        _ => return None,
    })
}

/// The `suspicious_stew_effects` component value of one effect.
fn stew_component(effect: u16, duration: i32) -> Box<[u8]> {
    let mut out = Vec::new();
    vanillite_protocol::codec::write_varint(&mut out, 1);
    vanillite_protocol::codec::write_varint(&mut out, i32::from(effect));
    vanillite_protocol::codec::write_varint(&mut out, duration);
    out.into()
}

/// `MushroomCow.mobInteract`.
pub(super) fn interact(ix: &mut Interact<'_, '_>) -> Interaction {
    let name = crate::item::item_id(ix.stack).name();
    let Some(m) = ix.ents.mob(ix.target) else {
        return Interaction::Pass;
    };
    let (baby, variant, stew) = (m.baby, st(m).variant, st(m).stew_effects);
    if name == "minecraft:bowl" && !baby {
        let (mut stack, sound) = match stew {
            Some((effect, duration)) => {
                let mut s = crate::mob::equipment::stack("minecraft:suspicious_stew");
                if let Some(id) = vanillite_data::registry::DATA_COMPONENT_TYPE
                    .id("minecraft:suspicious_stew_effects")
                {
                    s.patch_mut().set(id, stew_component(effect, duration));
                }
                (s, "minecraft:entity.mooshroom.suspicious_milk")
            }
            None => (
                crate::mob::equipment::stack("minecraft:mushroom_stew"),
                "minecraft:entity.mooshroom.milk",
            ),
        };
        if let Some(m) = ix.ents.mob_mut(ix.target) {
            st_mut(m).stew_effects = None;
        }
        stack.count = 1;
        fill_held(ix.ents, ix.env, ix.player, ix.hand, stack);
        sound_at(ix.ents, ix.env, ix.target, sound, SoundSource::Neutral, 1.0);
        return Interaction::Success;
    }
    if name == "minecraft:shears" && !baby {
        shear(ix);
        hurt_held(ix.ents, ix.env, ix.player, ix.hand, 1);
        return Interaction::Success;
    }
    if variant == 1
        && !baby
        && let Some(effect) = flower_effect(name)
    {
        if stew.is_none() {
            use_item(ix.ents, ix.player, ix.hand);
            if let Some(m) = ix.ents.mob_mut(ix.target) {
                st_mut(m).stew_effects = Some(effect);
            }
            sound_at(
                ix.ents,
                ix.env,
                ix.target,
                "minecraft:entity.mooshroom.eat",
                SoundSource::Neutral,
                1.0,
            );
        }
        return Interaction::Success;
    }
    // `AbstractCow.mobInteract`: milk.
    if name == "minecraft:bucket" && !baby {
        if let Some(pos) = ix.ents.store.get(ix.player).map(|e| e.pos()) {
            let seed = crate::survival::sound_seed();
            crate::survival::play_sound(
                ix.env.out,
                None,
                pos,
                "minecraft:entity.cow.milk",
                SoundSource::Players,
                1.0,
                1.0,
                seed,
            );
        }
        fill_held(
            ix.ents,
            ix.env,
            ix.player,
            ix.hand,
            crate::mob::equipment::stack("minecraft:milk_bucket"),
        );
        return Interaction::Success;
    }
    animal_interact(ix.ents, ix.env, ix.player, ix.target, ix.hand, ix.stack)
}

/// `MushroomCow.shear`: the mooshroom turns into a cow and drops five mushrooms.
fn shear(ix: &mut Interact<'_, '_>) {
    sound_at(
        ix.ents,
        ix.env,
        ix.target,
        "minecraft:entity.mooshroom.shear",
        SoundSource::Players,
        1.0,
    );
    let (Some(m), Some(e)) = (ix.ents.mob(ix.target), ix.ents.store.get(ix.target)) else {
        return;
    };
    let (pos, rot, height) = (e.pos(), e.rot(), f64::from(e.dims().height));
    // `convertTo(COW, ConversionParams.single(...))`: the cow keeps the name, age, health and
    // flags.
    let seed = crate::mob::new_entity_seed();
    let mut cow = MobData::new(Kind::Cow, seed);
    cow.custom_name = m.custom_name.clone();
    cow.no_ai = m.no_ai;
    cow.silent = m.silent;
    cow.persistent = m.persistent;
    cow.health = m.health.min(cow.max_health());
    cow.age = m.age;
    cow.baby = m.baby;
    cow.y_body_rot = m.y_body_rot;
    let mushroom = if st(m).variant == 1 {
        "minecraft:brown_mushroom"
    } else {
        "minecraft:red_mushroom"
    };
    if let Some(m) = ix.ents.mob_mut(ix.target) {
        m.removed = true;
    }
    ix.cx.discarded.push(ix.target);
    let uuid = crate::mob::new_uuid();
    crate::mob::spawn(
        ix.ents,
        ix.level.network_ids(),
        uuid,
        pos,
        Rotation {
            y_head_rot: rot.y_rot,
            ..rot
        },
        cow,
    );
    // `shearing/mooshroom`: five mushrooms of its colour, one item entity each.
    for _ in 0..5 {
        let (uuid, y_rot, velocity) = crate::mob::new_item_entity();
        let stack: ItemStack = crate::mob::equipment::stack(mushroom);
        ix.env.out.push(Effect::DropItem {
            uuid,
            pos: pos + Vec3::new(0.0, height, 0.0),
            velocity,
            y_rot,
            pickup_delay: 0,
            stack,
        });
    }
}

/// `MushroomCow.thunderHit`: red and brown swap.
pub(super) fn thunder_hit(h: &mut Hurt<'_, '_>) {
    let Some(m) = h.ents.mob_mut(h.target) else {
        return;
    };
    let s = st_mut(m);
    s.variant = if s.variant == 1 { 0 } else { 1 };
    let silent = m.silent;
    let seed = crate::survival::sound_seed();
    let pos = h.ents.store.get(h.target).map(|e| e.pos());
    if let Some(mut e) = h.ents.store.get_mut(h.target) {
        e.mark_data_dirty();
    }
    if let Some(pos) = pos
        && !silent
    {
        crate::survival::play_sound(
            h.env.out,
            None,
            pos,
            "minecraft:entity.mooshroom.convert",
            SoundSource::Neutral,
            2.0,
            1.0,
            seed,
        );
    }
}

/// `MushroomCow.getOffspringVariant`: 1 in 1024 babies of same-coloured parents mutate.
pub(super) fn offspring(
    parent: &MobData,
    partner: &MobData,
    child: &mut MobData,
    random: &mut LegacyRandom,
) {
    let (a, b) = (st(parent).variant, st(partner).variant);
    let v = if a == b && random.next_int_bounded(1024) == 0 {
        if a == 1 { 0 } else { 1 }
    } else if random.next_boolean() {
        a
    } else {
        b
    };
    st_mut(child).variant = v;
}

/// `addAdditionalSaveData`: `Type` and `stew_effects`.
pub(super) fn save(m: &MobData, out: &mut Compound) {
    let s = st(m);
    out.insert("Type", if s.variant == 1 { "brown" } else { "red" });
    if let Some((effect, duration)) = s.stew_effects
        && let Some(name) = crate::mob::effects::id::name(effect)
    {
        let mut c = Compound::new();
        c.insert("id", name);
        c.insert("duration", duration);
        out.insert("stew_effects", List(vec![Tag::Compound(c)]));
    }
}

/// `readAdditionalSaveData`.
pub(super) fn load(m: &mut MobData, tag: &Compound) {
    let s = st_mut(m);
    s.variant = i32::from(tag.get("Type").and_then(Tag::as_str) == Some("brown"));
    s.stew_effects = match tag.get("stew_effects") {
        Some(Tag::List(l)) => l.0.iter().find_map(|t| {
            let Tag::Compound(c) = t else {
                return None;
            };
            let effect = c.get("id").and_then(Tag::as_str).and_then(effect::of)?;
            Some((effect, super::int(c, "duration").unwrap_or(160)))
        }),
        _ => None,
    };
}

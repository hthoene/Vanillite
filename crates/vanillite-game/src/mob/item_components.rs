//! `EntityType.createDefaultStackConfig` for mobs made from an item (spawn eggs, mob buckets):
//! the item's entity components (`applyImplicitComponents` of the entity classes: variants,
//! sound variants, collars, colours) and then its `entity_data` (`updateCustomEntityTag`).

use vanillite_nbt::Tag;
use vanillite_world::entity::EntityId;

use crate::entities::Entities;
use crate::item::ItemStack;
use crate::mob::{Kind, MobData};

/// `appendDefaultStackConfig` after the mob was created and `finalizeSpawn` ran: its
/// components, then the `entity_data` of the item.
pub fn apply_spawn_item(ents: &mut Entities, id: EntityId, stack: &ItemStack) {
    apply_components(ents, id, stack);
    crate::decoration::stand::apply_entity_data_component(ents, id, stack);
}

/// `Entity.applyComponentsFromItemStack` of a mob (the custom name is set by the callers):
/// the setters of the components, nothing else of the mob changes.
pub fn apply_components(ents: &mut Entities, id: EntityId, stack: &ItemStack) {
    let Some(m) = ents.mob_mut(id) else {
        return;
    };
    let mut changed = false;
    for name in components(m.kind) {
        let Some(cid) = vanillite_data::registry::DATA_COMPONENT_TYPE.id(name) else {
            continue;
        };
        if let Some(wire) = stack.component(cid) {
            set(m, name, cid, wire);
            changed = true;
        }
    }
    if changed && let Some(mut e) = ents.store.get_mut(id) {
        e.mark_data_dirty();
    }
}

/// The components `applyImplicitComponents` of the kind's class reads, in its order.
fn components(kind: Kind) -> &'static [&'static str] {
    match kind {
        Kind::Pig => &["minecraft:pig/variant", "minecraft:pig/sound_variant"],
        Kind::Cow => &["minecraft:cow/variant", "minecraft:cow/sound_variant"],
        Kind::Chicken => &[
            "minecraft:chicken/variant",
            "minecraft:chicken/sound_variant",
        ],
        Kind::Cat => &[
            "minecraft:cat/variant",
            "minecraft:cat/sound_variant",
            "minecraft:cat/collar",
        ],
        Kind::Wolf => &[
            "minecraft:wolf/variant",
            "minecraft:wolf/sound_variant",
            "minecraft:wolf/collar",
        ],
        Kind::Frog => &["minecraft:frog/variant"],
        Kind::ZombieNautilus => &["minecraft:zombie_nautilus/variant"],
        Kind::Horse => &["minecraft:horse/variant"],
        Kind::Llama | Kind::TraderLlama => &["minecraft:llama/variant"],
        Kind::Axolotl => &["minecraft:axolotl/variant"],
        Kind::Parrot => &["minecraft:parrot/variant"],
        Kind::Rabbit => &["minecraft:rabbit/variant"],
        Kind::Fox => &["minecraft:fox/variant"],
        Kind::Mooshroom => &["minecraft:mooshroom/variant"],
        Kind::Salmon => &["minecraft:salmon/size"],
        Kind::TropicalFish => &[
            "minecraft:tropical_fish/pattern",
            "minecraft:tropical_fish/base_color",
            "minecraft:tropical_fish/pattern_color",
        ],
        Kind::Villager | Kind::ZombieVillager => &["minecraft:villager/variant"],
        Kind::Sheep => &["minecraft:sheep/color"],
        Kind::Shulker => &["minecraft:shulker/color"],
        Kind::SulfurCube => &["minecraft:sulfur_cube_content"],
        _ => &[],
    }
}

/// The registry entry of a registry component, without the `minecraft:` namespace.
fn entry_name(cid: u16, wire: &[u8]) -> Option<String> {
    match vanillite_protocol::components_nbt::to_nbt(
        cid,
        wire,
        &vanillite_protocol::item::InstalledRegistries,
    ) {
        Ok(Tag::String(s)) => Some(s.strip_prefix("minecraft:").unwrap_or(&s).to_owned()),
        _ => None,
    }
}

/// The index of the registry entry of the component in `names`.
fn index_in(names: &[&str], cid: u16, wire: &[u8]) -> Option<usize> {
    let name = entry_name(cid, wire)?;
    names.iter().position(|n| *n == name)
}

/// The setter of the component (`setVariant`, `setSoundVariant`, `setCollarColor`, ...).
fn set(m: &mut MobData, name: &str, cid: u16, wire: &[u8]) {
    use super::fam::{brainy, classic, companion, illager, mount, undead, village, wildlife};
    let Ok(id) = vanillite_protocol::codec::Reader::new(wire).varint() else {
        return;
    };
    let colour = u8::try_from(id.clamp(0, 15)).unwrap_or(0);
    let short = name.strip_prefix("minecraft:").unwrap_or(name);
    let index = |names: &[&str]| index_in(names, cid, wire).and_then(|i| i32::try_from(i).ok());
    match short {
        "pig/variant" | "cow/variant" | "chicken/variant" => {
            if let Some(i) = index(&["cold", "temperate", "warm"]) {
                m.variant = i;
            }
        }
        "pig/sound_variant" | "cow/sound_variant" | "chicken/sound_variant" => {
            if let Some(i) = index(super::kinds::sound_variant_names(m.kind)) {
                m.sound_variant = u8::try_from(i).unwrap_or(0);
            }
        }
        "cat/variant" => {
            if let Some(i) = index(&companion::feline::CAT_VARIANTS) {
                m.variant = i;
            }
        }
        "cat/sound_variant" => {
            if let Some(i) = index(&companion::feline::CAT_SOUNDS) {
                companion::st_mut(m).sound_variant = u8::try_from(i).unwrap_or(0);
            }
        }
        "wolf/variant" => {
            if let Some(i) = index(&companion::wolf::VARIANT_NAMES) {
                m.variant = i;
            }
        }
        "wolf/sound_variant" => {
            if let Some(i) = index(&companion::wolf::SOUND_NAMES) {
                companion::st_mut(m).sound_variant = u8::try_from(i).unwrap_or(0);
            }
        }
        "cat/collar" | "wolf/collar" => companion::st_mut(m).collar = colour,
        "frog/variant" => {
            if let Some(i) = index(&brainy::frog::VARIANTS) {
                brainy::state_mut(m).variant = i;
            }
        }
        "zombie_nautilus/variant" => {
            if let Some(i) = index(&["temperate", "warm"]) {
                mount::nautilus::nautilus_mut(m).variant = i;
            }
        }
        // `Horse.setVariant`: the colour, the markings stay.
        "horse/variant" => m.variant = m.variant & 0xFF00 | id & 0xFF,
        "llama/variant" => m.variant = id.clamp(0, 3),
        "parrot/variant" => m.variant = id.clamp(0, 4),
        "axolotl/variant" => brainy::state_mut(m).variant = id.clamp(0, 4),
        "rabbit/variant" => {
            m.variant = companion::rabbit::clamp_type(id);
            if m.variant == companion::rabbit::EVIL {
                companion::rabbit::apply_evil(m);
            }
        }
        "fox/variant" => m.variant = i32::from(id == 1),
        "mooshroom/variant" => wildlife::st_mut(m).variant = i32::from(id == 1),
        "salmon/size" => wildlife::st_mut(m).variant = id.clamp(0, 2),
        // `TropicalFish.setPattern`, `setBaseColor`, `setPatternColor` on the packed variant.
        "tropical_fish/pattern" => {
            let s = wildlife::st_mut(m);
            s.variant = s.variant & !0xFFFF | id & 0xFFFF;
        }
        "tropical_fish/base_color" => {
            let s = wildlife::st_mut(m);
            s.variant = s.variant & !0xFF_0000 | i32::from(colour) << 16;
        }
        "tropical_fish/pattern_color" => {
            let s = wildlife::st_mut(m);
            s.variant = s.variant & 0xFF_FFFF | i32::from(colour) << 24;
        }
        "sheep/color" => classic::sheep_mut(m).color = colour,
        "shulker/color" => illager::st_mut(m).color = colour,
        // `setItemSlotAndDropWhenKilled(BODY, content.create())`.
        "sulfur_cube_content" => {
            let mut r = vanillite_protocol::codec::Reader::new(wire);
            if let Ok(block) = ItemStack::decode_template(&mut r) {
                let body = crate::item::EquipmentSlot::Body;
                m.equipment.set(body, block);
                m.equipment.set_guaranteed_drop(body);
            }
        }
        // `setVillagerData(getVillagerData().withType(variant))`, `setVillagerDataFinalized`.
        "villager/variant" => {
            let Some(ty) = entry_name(cid, wire).and_then(|n| {
                vanillite_data::registry::VILLAGER_TYPE.id(&format!("minecraft:{n}"))
            }) else {
                return;
            };
            if m.kind == Kind::ZombieVillager {
                let z = undead::zombies::zs_mut(m);
                z.villager_type = ty;
                z.data_finalized = true;
            } else {
                let v = village::villager::vs_mut(m);
                v.villager_type = ty;
                v.data_finalized = true;
            }
        }
        _ => {}
    }
}

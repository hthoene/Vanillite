//! What a block entity's saved NBT looks like after a template loaded data into it
//! (`BlockEntity.loadWithComponents` then `saveWithoutMetadata`): Vanillite keeps block
//! entities as NBT, so the fields Vanilla fills with defaults on load and writes back on save,
//! and the forms its codecs write (plain text components as strings), are applied here.
//!
//! Covered are the block entity types of the templates of the ported structures; other types
//! keep the template's data as it is.

use vanillite_nbt::{Compound, List, Tag};

/// Normalises `nbt` (a block entity of type `kind` after the template's data was loaded into
/// it) to what `saveWithoutMetadata` would write.
pub fn normalize_loaded(kind: &str, nbt: &mut Compound) {
    match kind {
        "minecraft:sign" | "minecraft:hanging_sign" => sign(nbt),
        "minecraft:brewing_stand" => brewing_stand(nbt),
        "minecraft:furnace" | "minecraft:smoker" | "minecraft:blast_furnace" => furnace(nbt),
        "minecraft:sculk_sensor" | "minecraft:calibrated_sculk_sensor" => {
            default(nbt, "last_vibration_frequency", 0_i32);
            vibration_listener(nbt);
        }
        "minecraft:sculk_shrieker" => {
            default(nbt, "warning_level", 0_i32);
            vibration_listener(nbt);
        }
        "minecraft:vault" => vault(nbt),
        _ => {}
    }
}

/// `VaultConfig.CODEC` leaves out the fields that hold their default (`lenientOptionalFieldOf`).
fn vault(nbt: &mut Compound) {
    let Some(Tag::Compound(mut config)) = nbt.remove("config") else {
        return;
    };
    if config.get("loot_table").and_then(Tag::as_str)
        == Some("minecraft:chests/trial_chambers/reward")
    {
        config.remove("loot_table");
    }
    for (name, default) in [("activation_range", 4.0), ("deactivation_range", 4.5)] {
        if matches!(config.get(name), Some(Tag::Double(v)) if *v == default) {
            config.remove(name);
        }
    }
    nbt.insert("config", config);
}

/// Inserts `value` under `name` unless the key is present (`input.getXOr(name, default)` then
/// `output.putX(name, value)`).
fn default(nbt: &mut Compound, name: &str, value: impl Into<Tag>) {
    if nbt.get(name).is_none() {
        nbt.insert(name, value);
    }
}

/// `ContainerHelper.saveAllItems(output, items)`: the list is always written.
fn items(nbt: &mut Compound) {
    default(nbt, "Items", List::default());
}

/// `BrewingStandBlockEntity.loadAdditional`/`saveAdditional`.
fn brewing_stand(nbt: &mut Compound) {
    default(nbt, "BrewTime", 0_i32);
    default(nbt, "total_brew_time", 400_i32);
    items(nbt);
    default(nbt, "Fuel", 0_i32);
    default(nbt, "total_fuel", 20_i32);
    default(nbt, "speed_multiplier", 1.0_f32);
}

/// `AbstractFurnaceBlockEntity.loadAdditional`/`saveAdditional`.
fn furnace(nbt: &mut Compound) {
    default(nbt, "cooking_time_spent", 0_i32);
    default(nbt, "cooking_total_time", 0_i32);
    default(nbt, "lit_time_remaining", 0_i32);
    default(nbt, "lit_total_time", 0_i32);
    default(nbt, "speed_multiplier", 1.0_f32);
    items(nbt);
    default(nbt, "RecipesUsed", Compound::new());
}

/// The `listener` of `SculkSensorBlockEntity` and `SculkShriekerBlockEntity`
/// (`VibrationSystem.Data.CODEC`): a fresh `VibrationSystem.Data` when the template has none
/// (no event, a selector without candidate, no delay). A template's own listener is kept.
fn vibration_listener(nbt: &mut Compound) {
    default(
        nbt,
        "listener",
        Compound::new()
            .with("selector", Compound::new().with("tick", -1_i64))
            .with("event_delay", 0_i32),
    );
}

/// `SignBlockEntity.loadAdditional`/`saveAdditional`: both texts through `SignText.CODEC`,
/// `is_waxed` always, `allow_op_features` only when set.
fn sign(nbt: &mut Compound) {
    if matches!(nbt.get("allow_op_features"), Some(Tag::Byte(0))) {
        nbt.remove("allow_op_features");
    }
    for side in ["front_text", "back_text"] {
        let text = match nbt.remove(side) {
            Some(Tag::Compound(text)) => sign_text(text),
            _ => sign_text(Compound::new()),
        };
        nbt.insert(side, text);
    }
    default(nbt, "is_waxed", false);
}

/// `SignText.CODEC` round trip: four messages, `filtered_messages` only when they differ,
/// `color` and `has_glowing_text` always.
fn sign_text(mut text: Compound) -> Compound {
    let empty = || List(vec![Tag::String(String::new()); 4]);
    let messages = match text.remove("messages") {
        Some(Tag::List(list)) if list.0.len() == 4 => collapse_components(list),
        _ => empty(),
    };
    let filtered = match text.remove("filtered_messages") {
        Some(Tag::List(list)) if list.0.len() == 4 => Some(collapse_components(list)),
        _ => None,
    };
    let mut out = Compound::new().with("messages", messages.clone());
    if let Some(filtered) = filtered
        && filtered != messages
    {
        out.insert("filtered_messages", filtered);
    }
    out.insert(
        "color",
        text.remove("color")
            .unwrap_or_else(|| Tag::String("black".into())),
    );
    out.insert(
        "has_glowing_text",
        text.remove("has_glowing_text").unwrap_or(Tag::Byte(0)),
    );
    out
}

fn collapse_components(list: List) -> List {
    List(list.0.into_iter().map(collapse_component).collect())
}

/// `ComponentSerialization.CODEC` encoding: a literal text without style or siblings is written
/// as a plain string (`Component.tryCollapseToString`).
fn collapse_component(tag: Tag) -> Tag {
    if let Tag::Compound(compound) = &tag {
        let plain = compound
            .iter()
            .all(|(key, value)| key == "text" || (key == "type" && value.as_str() == Some("text")));
        if plain && let Some(Tag::String(text)) = compound.get("text") {
            return Tag::String(text.clone());
        }
    }
    tag
}

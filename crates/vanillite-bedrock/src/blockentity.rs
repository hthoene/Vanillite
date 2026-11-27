//! Java block entities as Bedrock block actor data: sign texts, bed and banner colours (one
//! Bedrock block for all colours, the colour lives in the block actor), banner patterns, the
//! halves of double chests, skull rotations and the plants of flower pots (Bedrock keeps them in
//! the block actor; Java pots are blocks without a block entity).

use vanillite_data::block::BlockState;
use vanillite_nbt::{Compound as JCompound, Tag};

use crate::nbt::Compound;

/// Java `DyeColor` names in id order (Bedrock beds use the same ids, banners `15 - id`).
const DYES: [&str; 16] = [
    "white",
    "orange",
    "magenta",
    "light_blue",
    "yellow",
    "lime",
    "pink",
    "gray",
    "light_gray",
    "cyan",
    "purple",
    "blue",
    "brown",
    "green",
    "red",
    "black",
];

/// `DyeColor.getTextColor` per dye id.
const TEXT_COLORS: [u32; 16] = [
    0xFF_FFFF, 0xFF_681F, 0xFF_00FF, 0x9A_C0CD, 0xFF_FF00, 0xBF_FF00, 0xFF_69B4, 0x80_8080,
    0xD3_D3D3, 0x00_FFFF, 0xA0_20F0, 0x00_00FF, 0x8B_4513, 0x00_FF00, 0xFF_0000, 0x00_0000,
];

/// Java banner pattern -> Bedrock pattern code.
const PATTERNS: &[(&str, &str)] = &[
    ("base", "b"),
    ("square_bottom_left", "bl"),
    ("square_bottom_right", "br"),
    ("square_top_left", "tl"),
    ("square_top_right", "tr"),
    ("stripe_bottom", "bs"),
    ("stripe_top", "ts"),
    ("stripe_left", "ls"),
    ("stripe_right", "rs"),
    ("stripe_center", "cs"),
    ("stripe_middle", "ms"),
    ("stripe_downright", "drs"),
    ("stripe_downleft", "dls"),
    ("small_stripes", "ss"),
    ("cross", "cr"),
    ("straight_cross", "sc"),
    ("triangle_bottom", "bt"),
    ("triangle_top", "tt"),
    ("triangles_bottom", "bts"),
    ("triangles_top", "tts"),
    ("diagonal_left", "ld"),
    ("diagonal_up_right", "rd"),
    ("diagonal_up_left", "lud"),
    ("diagonal_right", "rud"),
    ("circle", "mc"),
    ("rhombus", "mr"),
    ("half_vertical", "vh"),
    ("half_horizontal", "hh"),
    ("half_vertical_right", "vhr"),
    ("half_horizontal_bottom", "hhb"),
    ("border", "bo"),
    ("curly_border", "cbo"),
    ("gradient", "gra"),
    ("gradient_up", "gru"),
    ("bricks", "bri"),
    ("globe", "glb"),
    ("creeper", "cre"),
    ("skull", "sku"),
    ("flower", "flo"),
    ("mojang", "moj"),
    ("piglin", "pig"),
    ("flow", "flw"),
    ("guster", "gus"),
];

/// Bedrock `SkullType` by Java skull block name prefix (floor and wall variants).
const SKULLS: [(&str, u8); 7] = [
    ("skeleton_", 0),
    ("wither_skeleton_", 1),
    ("zombie_", 2),
    ("player_", 3),
    ("creeper_", 4),
    ("dragon_", 5),
    ("piglin_", 6),
];

/// The Bedrock skull type of a Java skull or head block (floor or wall).
fn skull_type(block: &str) -> Option<u8> {
    let name = block.strip_prefix("minecraft:")?;
    let base = name
        .strip_suffix("wall_skull")
        .or_else(|| name.strip_suffix("wall_head"))
        .or_else(|| name.strip_suffix("skull"))
        .or_else(|| name.strip_suffix("head"))?;
    // The longest prefix wins (wither_skeleton_ before skeleton_).
    SKULLS
        .iter()
        .filter(|(p, _)| *p == base)
        .map(|(_, t)| *t)
        .next()
}

/// Whether a Java block is a flower pot (empty or with a plant).
fn is_pot(block: &str) -> bool {
    block == "minecraft:flower_pot" || block.starts_with("minecraft:potted_")
}

/// The Java banner pattern (`minecraft:creeper`) of a Bedrock pattern code (`cre`).
pub fn java_pattern(code: &str) -> Option<String> {
    PATTERNS
        .iter()
        .find(|(_, b)| *b == code)
        .map(|(j, _)| format!("minecraft:{j}"))
}

/// The dye id of a Java dye item (`minecraft:red_dye` -> 14).
pub fn dye_of_item(item: &str) -> Option<usize> {
    let name = item.strip_prefix("minecraft:")?.strip_suffix("_dye")?;
    dye(name)
}

/// Most banner layers sent.
const MAX_LAYERS: usize = 16;

fn dye(name: &str) -> Option<usize> {
    DYES.iter().position(|d| *d == name)
}

/// The dye of a coloured block name (`red_bed` -> red).
fn dye_prefix(block: &str) -> Option<usize> {
    let name = block.strip_prefix("minecraft:").unwrap_or(block);
    DYES.iter()
        .position(|d| name.starts_with(d) && name.as_bytes().get(d.len()) == Some(&b'_'))
}

/// The plain text of a text component in NBT form.
fn plain(tag: &Tag, out: &mut String, depth: u8) {
    let Some(depth) = depth.checked_sub(1) else {
        return;
    };
    match tag {
        Tag::String(s) => out.push_str(s),
        Tag::List(l) => l.0.iter().for_each(|t| plain(t, out, depth)),
        Tag::Compound(c) => {
            match (c.get("text"), c.get("translate")) {
                (Some(Tag::String(s)), _) => out.push_str(s),
                (_, Some(Tag::String(key))) => {
                    out.push_str(c.get("fallback").and_then(Tag::as_str).unwrap_or(key));
                }
                _ => {}
            }
            if let Some(extra) = c.get("extra") {
                plain(extra, out, depth);
            }
        }
        _ => {}
    }
}

/// A Bedrock sign side from a Java `front_text`/`back_text`.
fn sign_side(side: Option<&JCompound>) -> Compound {
    let mut text = String::new();
    let mut color = 15usize;
    let mut glowing = false;
    if let Some(side) = side {
        if let Some(Tag::List(lines)) = side.get("messages") {
            for (i, line) in lines.0.iter().take(4).enumerate() {
                if i > 0 {
                    text.push('\n');
                }
                plain(line, &mut text, 8);
            }
        }
        color = side
            .get("color")
            .and_then(Tag::as_str)
            .and_then(dye)
            .unwrap_or(15);
        glowing = side
            .get("has_glowing_text")
            .and_then(Tag::as_i64)
            .is_some_and(|v| v != 0);
    }
    let argb = TEXT_COLORS.get(color).copied().unwrap_or(0) | 0xFF00_0000;
    let mut c = Compound::default();
    c.string("Text", &text)
        .int("SignTextColor", i32::from_ne_bytes(argb.to_ne_bytes()))
        .byte("IgnoreLighting", u8::from(glowing))
        .byte("HideGlowOutline", 0)
        .byte("PersistFormatting", 1)
        .string("TextOwner", "");
    c
}

/// The partner of a double chest half: its offset (x, z) and whether this half leads.
fn chest_pair(state: BlockState) -> Option<([i32; 2], bool)> {
    let kind = state.value("type")?;
    let facing = state.value("facing")?;
    // `ChestBlock.getConnectedDirection`: clockwise of the facing for the left half.
    let clockwise = match facing {
        "north" => [1, 0],
        "east" => [0, 1],
        "south" => [-1, 0],
        _ => [0, -1],
    };
    match kind {
        "left" => Some((clockwise, true)),
        "right" => Some((clockwise.map(|v: i32| v.saturating_neg()), false)),
        _ => None,
    }
}

/// The Bedrock block actor data (a root compound) for a Java block entity at `pos` with block
/// state `state` and its client NBT (`None` when only the block changed).
pub fn translate(
    pos: [i32; 3],
    state: u32,
    nbt: Option<&JCompound>,
    palette: &crate::data::Palette,
) -> Option<Vec<u8>> {
    let state = BlockState(u16::try_from(state).ok()?);
    if !state.is_valid() {
        return None;
    }
    let name = state.block().name();
    let [x, y, z] = pos;
    let mut c = Compound::default();
    if name.ends_with("_sign") {
        let id = if name.ends_with("hanging_sign") {
            "HangingSign"
        } else {
            "Sign"
        };
        let nbt = nbt?;
        let side = |k: &str| nbt.get(k).and_then(Tag::as_compound);
        c.string("id", id)
            .compound("FrontText", &sign_side(side("front_text")))
            .compound("BackText", &sign_side(side("back_text")))
            .byte(
                "IsWaxed",
                u8::from(
                    nbt.get("is_waxed")
                        .and_then(Tag::as_i64)
                        .is_some_and(|v| v != 0),
                ),
            );
    } else if name.ends_with("_bed") {
        let color = dye_prefix(name)?;
        c.string("id", "Bed")
            .byte("color", u8::try_from(color).unwrap_or(14));
    } else if name.ends_with("_banner") {
        let base = dye_prefix(name)?;
        let mut layers = Vec::new();
        if let Some(Tag::List(list)) = nbt.and_then(|n| n.get("patterns")) {
            for layer in list.0.iter().filter_map(Tag::as_compound).take(MAX_LAYERS) {
                let pattern = layer
                    .get("pattern")
                    .and_then(Tag::as_str)
                    .map(|p| p.strip_prefix("minecraft:").unwrap_or(p))
                    .and_then(|p| PATTERNS.iter().find(|(j, _)| *j == p))
                    .map(|(_, b)| *b);
                let color = layer.get("color").and_then(Tag::as_str).and_then(dye);
                if let (Some(pattern), Some(color)) = (pattern, color) {
                    let mut l = Compound::default();
                    l.string("Pattern", pattern).int(
                        "Color",
                        15i32.saturating_sub(i32::try_from(color).unwrap_or(0)),
                    );
                    layers.push(l);
                }
            }
        }
        c.string("id", "Banner")
            .int(
                "Base",
                15i32.saturating_sub(i32::try_from(base).unwrap_or(0)),
            )
            .int("Type", 0)
            .compounds("Patterns", &layers);
    } else if name == "minecraft:beacon" {
        let effect = |k: &str| {
            nbt.and_then(|n| n.get(k))
                .and_then(Tag::as_str)
                .and_then(crate::entity::effect_id)
                .unwrap_or(0)
        };
        c.string("id", "Beacon")
            .int("primary", effect("primary_effect"))
            .int("secondary", effect("secondary_effect"));
    } else if name == "minecraft:chest" || name == "minecraft:trapped_chest" {
        let ([dx, dz], lead) = chest_pair(state)?;
        c.string("id", "Chest")
            .int("pairx", x.saturating_add(dx))
            .int("pairz", z.saturating_add(dz))
            .byte("pairlead", u8::from(lead));
    } else if let Some(kind) = skull_type(name) {
        // Floor skulls turn in 16 steps; wall skulls face their wall (the block state).
        let rotation = state
            .value("rotation")
            .and_then(|r| r.parse::<u8>().ok())
            .map_or(0.0, |r| f32::from(r) * 22.5);
        c.string("id", "Skull")
            .float("Rotation", rotation)
            .byte("SkullType", kind)
            .byte("MouthMoving", 0)
            .int("MouthTickCount", 0)
            .byte("DoingAnimation", 0);
    } else if is_pot(name) {
        c.string("id", "FlowerPot");
        if let Some(plant) = palette.potted_plant(u32::from(state.0)) {
            c.raw_compound("PlantBlock", plant);
        }
    } else {
        return None;
    }
    c.int("x", x).int("y", y).int("z", z).byte("isMovable", 1);
    Some(c.root())
}

/// Whether the Bedrock block actor data of a Java block state needs the block entity's NBT
/// (signs, banner patterns).
pub fn needs_nbt(state: u32) -> bool {
    u16::try_from(state).is_ok_and(|s| {
        let s = BlockState(s);
        s.is_valid() && {
            let name = s.block().name();
            name.ends_with("_sign") || name.ends_with("_banner") || name == "minecraft:beacon"
        }
    })
}

/// Whether a Java block state has Bedrock block actor data from its state alone (beds,
/// banners, double chests, skulls, flower pots): a block update must send it.
pub fn from_state(state: u32) -> bool {
    let Ok(s) = u16::try_from(state) else {
        return false;
    };
    let s = BlockState(s);
    if !s.is_valid() {
        return false;
    }
    let name = s.block().name();
    name.ends_with("_bed")
        || name.ends_with("_banner")
        || is_pot(name)
        || skull_type(name).is_some()
        || ((name == "minecraft:chest" || name == "minecraft:trapped_chest")
            && chest_pair(s).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_come_from_block_names() {
        assert_eq!(dye_prefix("minecraft:light_blue_bed"), Some(3));
        assert_eq!(dye_prefix("minecraft:blue_bed"), Some(11));
        assert_eq!(dye_prefix("minecraft:light_gray_wall_banner"), Some(8));
    }

    #[test]
    fn skulls_and_pots() {
        assert_eq!(skull_type("minecraft:wither_skeleton_wall_skull"), Some(1));
        assert_eq!(skull_type("minecraft:skeleton_skull"), Some(0));
        assert_eq!(skull_type("minecraft:piglin_head"), Some(6));
        assert_eq!(skull_type("minecraft:piston_head"), None);
        let pot = vanillite_data::block::BlockId::by_name("minecraft:potted_poppy")
            .map(|b| u32::from(b.default_state().0))
            .unwrap_or(0);
        let plant = crate::data::V26_60.potted_plant(pot).unwrap_or_default();
        assert!(plant.windows(5).any(|w| w == b"poppy"));
        let actor = translate([0, 64, 0], pot, None, &crate::data::V26_60).unwrap_or_default();
        assert!(actor.windows(10).any(|w| w == b"PlantBlock"));
    }

    #[test]
    fn sign_text_is_plain() {
        let line = Tag::Compound(JCompound::default().with("text", "Hello ").with(
            "extra",
            Tag::List(vanillite_nbt::List(vec![Tag::from("world")])),
        ));
        let mut out = String::new();
        plain(&line, &mut out, 8);
        assert_eq!(out, "Hello world");
    }
}

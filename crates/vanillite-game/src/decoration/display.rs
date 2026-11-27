//! Display entities (`Display.BlockDisplay`, `ItemDisplay`, `TextDisplay`) and interactions
//! (`Interaction`). Displays only carry data: the client interpolates them. Interactions keep
//! the last player that attacked or used them (`execute on attacker` / `on target`).

use vanillite_data::block::BlockState;
use vanillite_data::entity::{EntityTypeId, ids};
use vanillite_nbt::{Compound, List, Tag};
use vanillite_protocol::packets::play::clientbound::SoundSource;
use vanillite_world::entity::EntityId;

use super::{DecorData, flag, float, int};
use crate::entities::Entities;
use crate::item::ItemStack;
use crate::mob::animal::Interaction as Result;
use crate::player::encode::{DataWriter, serializer};
use crate::survival::player::Env;

/// Whether `kind` is a display entity.
#[inline]
pub fn is_display_kind(kind: EntityTypeId) -> bool {
    kind == ids::BLOCK_DISPLAY || kind == ids::ITEM_DISPLAY || kind == ids::TEXT_DISPLAY
}

/// A player action on an interaction (`Interaction.PlayerAction`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlayerAction {
    pub player: u128,
    pub timestamp: i64,
}

/// An interaction entity.
#[derive(Clone, Debug, PartialEq)]
pub struct Interaction {
    pub width: f32,
    pub height: f32,
    pub response: bool,
    pub attack: Option<PlayerAction>,
    pub interaction: Option<PlayerAction>,
}

impl Default for Interaction {
    fn default() -> Interaction {
        Interaction {
            width: 1.0,
            height: 1.0,
            response: false,
            attack: None,
            interaction: None,
        }
    }
}

/// `Transformation`: translation, left rotation, scale, right rotation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transformation {
    pub translation: [f32; 3],
    pub left_rotation: [f32; 4],
    pub scale: [f32; 3],
    pub right_rotation: [f32; 4],
}

impl Transformation {
    /// `Transformation.IDENTITY`.
    pub const IDENTITY: Transformation = Transformation {
        translation: [0.0; 3],
        left_rotation: [0.0, 0.0, 0.0, 1.0],
        scale: [1.0; 3],
        right_rotation: [0.0, 0.0, 0.0, 1.0],
    };
}

/// What a display shows.
#[derive(Clone, Debug, PartialEq)]
pub enum Shown {
    Block(BlockState),
    Item {
        item: ItemStack,
        /// `ItemDisplayContext` id.
        context: u8,
    },
    Text {
        /// The text component as NBT.
        text: Tag,
        line_width: i32,
        background: i32,
        opacity: i8,
        /// `FLAG_SHADOW`, `FLAG_SEE_THROUGH`, `FLAG_USE_DEFAULT_BACKGROUND`, alignment.
        flags: u8,
    },
}

/// A display entity.
#[derive(Clone, Debug, PartialEq)]
pub struct Display {
    pub transformation: Transformation,
    /// `interpolation_duration`.
    pub interpolation_duration: i32,
    /// `start_interpolation`.
    pub start_interpolation: i32,
    /// `teleport_duration` (0–59).
    pub teleport_duration: i32,
    /// `BillboardConstraints` id.
    pub billboard: u8,
    /// Packed `Brightness`, `-1` for none.
    pub brightness: i32,
    pub view_range: f32,
    pub shadow_radius: f32,
    pub shadow_strength: f32,
    pub width: f32,
    pub height: f32,
    pub glow_color: i32,
    pub shown: Shown,
}

const BILLBOARDS: [&str; 4] = ["fixed", "vertical", "horizontal", "center"];
const ITEM_CONTEXTS: [&str; 10] = [
    "none",
    "thirdperson_lefthand",
    "thirdperson_righthand",
    "firstperson_lefthand",
    "firstperson_righthand",
    "head",
    "gui",
    "ground",
    "fixed",
    "on_shelf",
];

/// The stored keys of the displays.
pub const KEYS: &[&str] = &[
    "transformation",
    "interpolation_duration",
    "start_interpolation",
    "teleport_duration",
    "billboard",
    "view_range",
    "shadow_radius",
    "shadow_strength",
    "width",
    "height",
    "glow_color_override",
    "brightness",
    "block_state",
    "item",
    "item_display",
    "text",
    "line_width",
    "text_opacity",
    "background",
    "shadow",
    "see_through",
    "default_background",
    "alignment",
];

fn floats<const N: usize>(tag: Option<&Tag>) -> Option<[f32; N]> {
    let list: Vec<f32> = match tag? {
        Tag::List(List(v)) => v
            .iter()
            .map(|t| match t {
                Tag::Float(f) => Some(*f),
                #[allow(clippy::cast_possible_truncation)]
                Tag::Double(d) => Some(*d as f32),
                t => t.as_i64().map(|v| {
                    #[allow(clippy::cast_precision_loss)]
                    let f = v as f32;
                    f
                }),
            })
            .collect::<Option<_>>()?,
        Tag::IntArray(v) => v
            .iter()
            .map(|&i| {
                #[allow(clippy::cast_precision_loss)]
                let f = i as f32;
                f
            })
            .collect(),
        _ => return None,
    };
    list.try_into().ok()
}

fn float_list(v: &[f32]) -> Tag {
    Tag::List(List(v.iter().map(|&f| Tag::Float(f)).collect()))
}

/// `ExtraCodecs.QUATERNIONF`: four floats, or an axis and angle.
fn quaternion(tag: Option<&Tag>) -> Option<[f32; 4]> {
    if let Some(q) = floats::<4>(tag) {
        return Some(normalized(q));
    }
    let Some(Tag::Compound(c)) = tag else {
        return None;
    };
    let angle = float(c, "angle")?;
    // `new Quaternionf(new AxisAngle4f(angle, axis))`: the axis as given.
    let [x, y, z] = floats::<3>(c.get("axis"))?;
    let (s, c) = ((angle * 0.5).sin(), (angle * 0.5).cos());
    Some([x * s, y * s, z * s, c])
}

fn normalized(q: [f32; 4]) -> [f32; 4] {
    let inv = 1.0 / (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
    [q[0] * inv, q[1] * inv, q[2] * inv, q[3] * inv]
}

/// `Transformation.EXTENDED_CODEC`: the four parts, or a 4×4 matrix (row major).
fn read_transformation(tag: Option<&Tag>) -> Option<Transformation> {
    match tag? {
        Tag::Compound(c) => Some(Transformation {
            translation: floats(c.get("translation"))?,
            left_rotation: quaternion(c.get("left_rotation"))?,
            scale: floats(c.get("scale"))?,
            right_rotation: quaternion(c.get("right_rotation"))?,
        }),
        t => floats::<16>(Some(t)).map(super::matrix::decompose),
    }
}

fn write_transformation(t: &Transformation) -> Tag {
    let mut c = Compound::new();
    c.insert("translation", float_list(&t.translation));
    c.insert("left_rotation", float_list(&t.left_rotation));
    c.insert("scale", float_list(&t.scale));
    c.insert("right_rotation", float_list(&t.right_rotation));
    Tag::Compound(c)
}

/// `Display.readAdditionalSaveData` and the kind's.
pub fn load(kind: EntityTypeId, tag: &Compound) -> Option<Display> {
    let shown = if kind == ids::BLOCK_DISPLAY {
        Shown::Block(
            tag.get("block_state")
                .and_then(crate::level::block_entity::read_state_codec)
                .unwrap_or(BlockState::AIR),
        )
    } else if kind == ids::ITEM_DISPLAY {
        Shown::Item {
            item: match tag.get("item") {
                Some(Tag::Compound(c)) => crate::inventory::stack_from_nbt(c),
                _ => ItemStack::EMPTY,
            },
            context: name_index(tag.get("item_display"), &ITEM_CONTEXTS).unwrap_or(0),
        }
    } else if kind == ids::TEXT_DISPLAY {
        let mut flags = 0u8;
        for (key, bit) in [
            ("shadow", 1u8),
            ("see_through", 2),
            ("default_background", 4),
        ] {
            if flag(tag, key) {
                flags |= bit;
            }
        }
        match tag.get("alignment").and_then(Tag::as_str) {
            Some("left") => flags |= 8,
            Some("right") => flags |= 16,
            _ => {}
        }
        Shown::Text {
            // `ComponentUtils.resolve` of selectors and scores needs a command source; the
            // text is kept as stored.
            text: tag
                .get("text")
                .cloned()
                .unwrap_or_else(|| Tag::String(String::new())),
            line_width: int(tag, "line_width").unwrap_or(200),
            background: int(tag, "background").unwrap_or(0x4000_0000),
            #[allow(clippy::cast_possible_truncation)]
            opacity: int(tag, "text_opacity").map_or(-1, |v| v as i8),
            flags,
        }
    } else {
        return None;
    };
    let brightness = match tag.get("brightness") {
        Some(Tag::Compound(c)) => match (int(c, "block"), int(c, "sky")) {
            (Some(b), Some(s)) if (0..=15).contains(&b) && (0..=15).contains(&s) => {
                b << 4 | s << 20
            }
            _ => -1,
        },
        _ => -1,
    };
    Some(Display {
        transformation: read_transformation(tag.get("transformation"))
            .unwrap_or(Transformation::IDENTITY),
        interpolation_duration: int(tag, "interpolation_duration").unwrap_or(0),
        start_interpolation: int(tag, "start_interpolation").unwrap_or(0),
        teleport_duration: int(tag, "teleport_duration").unwrap_or(0).clamp(0, 59),
        billboard: name_index(tag.get("billboard"), &BILLBOARDS).unwrap_or(0),
        brightness,
        view_range: float(tag, "view_range").unwrap_or(1.0),
        shadow_radius: float(tag, "shadow_radius").unwrap_or(0.0),
        shadow_strength: float(tag, "shadow_strength").unwrap_or(1.0),
        width: float(tag, "width").unwrap_or(0.0),
        height: float(tag, "height").unwrap_or(0.0),
        glow_color: int(tag, "glow_color_override").unwrap_or(-1),
        shown,
    })
}

fn name_index(tag: Option<&Tag>, names: &[&str]) -> Option<u8> {
    let s = tag?.as_str()?;
    let s = s.strip_prefix("minecraft:").unwrap_or(s);
    names
        .iter()
        .position(|n| *n == s)
        .and_then(|i| u8::try_from(i).ok())
}

/// `Display.addAdditionalSaveData` and the kind's.
pub fn save(d: &Display, out: &mut Compound) {
    out.insert("transformation", write_transformation(&d.transformation));
    out.insert(
        "billboard",
        BILLBOARDS
            .get(usize::from(d.billboard))
            .copied()
            .unwrap_or("fixed"),
    );
    out.insert("interpolation_duration", d.interpolation_duration);
    out.insert("teleport_duration", d.teleport_duration);
    out.insert("view_range", d.view_range);
    out.insert("shadow_radius", d.shadow_radius);
    out.insert("shadow_strength", d.shadow_strength);
    out.insert("width", d.width);
    out.insert("height", d.height);
    out.insert("glow_color_override", d.glow_color);
    if d.brightness == -1 {
        out.remove("brightness");
    } else {
        let mut b = Compound::new();
        b.insert("block", d.brightness >> 4 & 15);
        b.insert("sky", d.brightness >> 20 & 15);
        out.insert("brightness", b);
    }
    match &d.shown {
        Shown::Block(state) => {
            out.insert(
                "block_state",
                crate::level::block_entity::write_state_codec(*state),
            );
        }
        Shown::Item { item, context } => {
            if item.is_empty() {
                out.remove("item");
            } else {
                out.insert("item", crate::inventory::stack_to_nbt(item));
            }
            out.insert(
                "item_display",
                ITEM_CONTEXTS
                    .get(usize::from(*context))
                    .copied()
                    .unwrap_or("none"),
            );
        }
        Shown::Text {
            text,
            line_width,
            background,
            opacity,
            flags,
        } => {
            out.insert("text", text.clone());
            out.insert("line_width", *line_width);
            out.insert("background", *background);
            out.insert("text_opacity", *opacity);
            out.insert("shadow", flags & 1 != 0);
            out.insert("see_through", flags & 2 != 0);
            out.insert("default_background", flags & 4 != 0);
            out.insert(
                "alignment",
                if flags & 8 != 0 {
                    "left"
                } else if flags & 16 != 0 {
                    "right"
                } else {
                    "center"
                },
            );
        }
    }
}

fn vec_item(w: &mut DataWriter<'_>, id: u8, v: &[f32], default: &[f32]) {
    let same = v
        .iter()
        .zip(default)
        .all(|(a, b)| a.to_bits() == b.to_bits());
    let ser = if v.len() == 3 {
        serializer::VECTOR3
    } else {
        serializer::QUATERNION
    };
    w.raw(id, ser, same, |o| {
        for f in v {
            o.extend_from_slice(&f.to_be_bytes());
        }
    });
}

/// The synched data of a display (ids 8–22, then the kind's from 23).
pub fn entity_data(w: &mut DataWriter<'_>, d: &Display) {
    let t = &d.transformation;
    let id = Transformation::IDENTITY;
    // `setTransformationInterpolationDelay(ticks, true)`: every data update sends it.
    w.int(8, d.start_interpolation, 0);
    w.int(9, d.interpolation_duration, 0);
    w.int(10, d.teleport_duration, 0);
    vec_item(w, 11, &t.translation, &id.translation);
    vec_item(w, 12, &t.scale, &id.scale);
    vec_item(w, 13, &t.left_rotation, &id.left_rotation);
    vec_item(w, 14, &t.right_rotation, &id.right_rotation);
    w.byte(15, d.billboard, 0);
    w.int(16, d.brightness, -1);
    w.float(17, d.view_range, 1.0);
    w.float(18, d.shadow_radius, 0.0);
    w.float(19, d.shadow_strength, 1.0);
    w.float(20, d.width, 0.0);
    w.float(21, d.height, 0.0);
    w.int(22, d.glow_color, -1);
    match &d.shown {
        Shown::Block(state) => w.varint(23, serializer::BLOCK_STATE, i32::from(state.0), 0),
        Shown::Item { item, context } => {
            w.stack(23, item);
            w.byte(24, *context, 0);
        }
        Shown::Text {
            text,
            line_width,
            background,
            opacity,
            flags,
        } => {
            let empty = matches!(text, Tag::String(s) if s.is_empty());
            w.raw(23, serializer::COMPONENT, empty, |o| {
                let _ = vanillite_nbt::write::write_network(text, o);
            });
            w.int(24, *line_width, 200);
            w.int(25, *background, 0x4000_0000);
            #[allow(clippy::cast_sign_loss)]
            w.byte(26, *opacity as u8, 0xFF);
            w.byte(27, *flags, 0);
        }
    }
}

/// `Interaction.readAdditionalSaveData`.
pub fn load_interaction(tag: &Compound) -> Interaction {
    let action = |key: &str| match tag.get(key) {
        Some(Tag::Compound(c)) => Some(PlayerAction {
            player: crate::mob::nbt::uuid_key_of(c, "player")?,
            timestamp: c.get("timestamp").and_then(Tag::as_i64)?,
        }),
        _ => None,
    };
    Interaction {
        width: float(tag, "width").unwrap_or(1.0),
        height: float(tag, "height").unwrap_or(1.0),
        response: flag(tag, "response"),
        attack: action("attack"),
        interaction: action("interaction"),
    }
}

/// `Interaction.addAdditionalSaveData`.
pub fn save_interaction(i: &Interaction, out: &mut Compound) {
    out.insert("width", i.width);
    out.insert("height", i.height);
    for (key, a) in [("attack", i.attack), ("interaction", i.interaction)] {
        match a {
            Some(a) => {
                let mut c = Compound::new();
                c.insert("player", crate::mob::nbt::uuid_tag(a.player));
                c.insert("timestamp", a.timestamp);
                out.insert(key, c);
            }
            None => {
                out.remove(key);
            }
        }
    }
    out.insert("response", i.response);
}

/// The synched data of an interaction.
pub fn interaction_data(w: &mut DataWriter<'_>, i: &Interaction) {
    w.float(8, i.width, 1.0);
    w.float(9, i.height, 1.0);
    w.boolean(10, i.response, false);
}

/// `Interaction.skipAttackInteraction`: the attack is recorded; without a response the attack
/// ends here. Returns whether it ends.
pub fn attack_interaction(
    ents: &mut Entities,
    env: &mut Env<'_>,
    player: EntityId,
    id: EntityId,
) -> bool {
    let Some(uuid) = ents.store.get(player).map(|e| e.uuid()) else {
        return true;
    };
    let time = env.game_time;
    let Some(DecorData::Interaction(i)) = ents.decor_mut(id).map(|d| &mut d.data) else {
        return true;
    };
    i.attack = Some(PlayerAction {
        player: uuid,
        timestamp: time,
    });
    let response = i.response;
    // `PLAYER_HURT_ENTITY` with generic damage of 1.
    let kind = crate::advancements::trigger::TriggerKind::PLAYER_HURT_ENTITY;
    if crate::advancements::view::listens(ents, player, kind)
        && let Some(target) = crate::advancements::view::snapshot(ents, id)
    {
        let source = crate::survival::damage::DamageSource::of(env.types.generic);
        let damage =
            crate::advancements::view::damage_event(ents, env.types, &source, 1.0, 1.0, false);
        if let Some(p) = ents.player_mut(player) {
            p.advancements
                .trigger(crate::advancements::trigger::Event::PlayerHurtEntity(
                    target, damage,
                ));
        }
    }
    if response {
        // The attack goes on and deals no damage (`ATTACK_NODAMAGE`).
        if let Some(p) = ents.store.get(player).map(|e| e.pos()) {
            crate::survival::play_sound(
                env.out,
                None,
                p,
                "minecraft:entity.player.attack.nodamage",
                SoundSource::Players,
                1.0,
                1.0,
                crate::survival::sound_seed(),
            );
        }
        if let Some(p) = ents.player_mut(player) {
            p.survival.attack_ticker = 0;
        }
    }
    true
}

/// `Interaction.interact`: the use is recorded and consumed.
pub fn interact_interaction(
    ents: &mut Entities,
    env: &Env<'_>,
    player: EntityId,
    id: EntityId,
) -> Result {
    let Some(uuid) = ents.store.get(player).map(|e| e.uuid()) else {
        return Result::Pass;
    };
    if let Some(DecorData::Interaction(i)) = ents.decor_mut(id).map(|d| &mut d.data) {
        i.interaction = Some(PlayerAction {
            player: uuid,
            timestamp: env.game_time,
        });
    }
    Result::Consume
}

/// `getLastAttacker` (`execute on attacker`) of an interaction: the player who attacked it.
pub fn last_attacker(ents: &Entities, id: EntityId) -> Option<EntityId> {
    match &ents.decor(id)?.data {
        DecorData::Interaction(i) => ents.player_by_uuid(i.attack?.player),
        _ => None,
    }
}

/// `getTarget` (`execute on target`) of an interaction: the player who used it.
pub fn target(ents: &Entities, id: EntityId) -> Option<EntityId> {
    match &ents.decor(id)?.data {
        DecorData::Interaction(i) => ents.player_by_uuid(i.interaction?.player),
        _ => None,
    }
}

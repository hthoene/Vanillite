//! Skulls and banners (`SkullBlockEntity`, `BannerBlockEntity`): what the client draws, kept
//! as component values (`profile`, `note_block_sound`, `banner_patterns`, `custom_name`).

use vanillite_data::block::BlockId;
use vanillite_nbt::{Compound, Tag};
use vanillite_protocol::components::ids;
use vanillite_protocol::text::Component;

use crate::level::block_entity::{encode_name, load_component, save_component};

/// The data of a `SkullBlockEntity`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Skull {
    /// `profile` (a `minecraft:profile` value).
    pub profile: Option<Box<[u8]>>,
    /// `note_block_sound`.
    pub note_block_sound: Option<String>,
    /// `custom_name`.
    pub custom_name: Option<Component>,
}

impl Skull {
    /// `loadAdditional`.
    pub fn load(&mut self, nbt: &Compound) {
        self.profile = load_component(nbt, "profile", ids::PROFILE);
        self.note_block_sound = match nbt.get("note_block_sound") {
            Some(Tag::String(s)) if valid_identifier(s) => Some(s.clone()),
            _ => None,
        };
        self.custom_name = nbt.get("custom_name").map(Component::from_nbt);
    }

    /// `saveAdditional` (also the update tag, `saveCustomOnly`).
    pub fn save(&self, out: &mut Compound) {
        if let Some(profile) = &self.profile {
            save_component(out, "profile", ids::PROFILE, profile);
        }
        if let Some(sound) = &self.note_block_sound {
            out.insert("note_block_sound", Tag::String(sound.clone()));
        }
        if let Some(name) = &self.custom_name {
            out.insert("custom_name", name.to_nbt());
        }
    }

    /// `collectImplicitComponents`.
    pub fn component(&self, id: u16) -> Option<Box<[u8]>> {
        if id == ids::PROFILE {
            self.profile.clone()
        } else if id == ids::NOTE_BLOCK_SOUND {
            let sound = self.note_block_sound.as_ref()?;
            let mut out = Vec::new();
            vanillite_protocol::codec::write_str(&mut out, sound);
            Some(out.into_boxed_slice())
        } else if id == ids::CUSTOM_NAME {
            encode_name(self.custom_name.as_ref())
        } else {
            None
        }
    }

    /// `applyImplicitComponents`.
    pub fn apply(&mut self, stack: &crate::item::ItemStack) {
        self.profile = stack.component(ids::PROFILE).map(Box::from);
        self.note_block_sound = stack.component(ids::NOTE_BLOCK_SOUND).and_then(|v| {
            let mut r = vanillite_protocol::codec::Reader::new(v);
            r.string(32767).ok().map(str::to_owned)
        });
        self.custom_name = crate::level::block_entity::decode_name(stack);
    }
}

/// The data of a `BannerBlockEntity` (the base colour is the block's).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Banner {
    /// `patterns` (a `minecraft:banner_patterns` value; `None` for no layers).
    pub patterns: Option<Box<[u8]>>,
    /// `CustomName`.
    pub custom_name: Option<Component>,
}

impl Banner {
    /// `loadAdditional`.
    pub fn load(&mut self, nbt: &Compound) {
        self.custom_name = nbt.get("CustomName").map(Component::from_nbt);
        self.patterns =
            load_component(nbt, "patterns", ids::BANNER_PATTERNS).filter(|v| !is_empty_list(v));
    }

    /// `saveAdditional` (also the update tag, `saveWithoutMetadata`).
    pub fn save(&self, out: &mut Compound) {
        if let Some(patterns) = &self.patterns {
            save_component(out, "patterns", ids::BANNER_PATTERNS, patterns);
        }
        if let Some(name) = &self.custom_name {
            out.insert("CustomName", name.to_nbt());
        }
    }

    /// `collectImplicitComponents`: patterns are always set (empty layers included).
    pub fn component(&self, id: u16) -> Option<Box<[u8]>> {
        if id == ids::BANNER_PATTERNS {
            Some(self.patterns.clone().unwrap_or_else(|| Box::new([0])))
        } else if id == ids::CUSTOM_NAME {
            encode_name(self.custom_name.as_ref())
        } else {
            None
        }
    }

    /// `applyImplicitComponents`.
    pub fn apply(&mut self, stack: &crate::item::ItemStack) {
        self.patterns = stack
            .component(ids::BANNER_PATTERNS)
            .filter(|v| !is_empty_list(v))
            .map(Box::from);
        self.custom_name = crate::level::block_entity::decode_name(stack);
    }
}

/// Whether a list value has no entries (a single zero length).
fn is_empty_list(value: &[u8]) -> bool {
    value == [0]
}

/// `Identifier` syntax (namespace and path characters).
fn valid_identifier(s: &str) -> bool {
    let (ns, path) = s.split_once(':').unwrap_or(("minecraft", s));
    !path.is_empty()
        && ns
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' | b'.'))
        && path
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' | b'.' | b'/'))
}

/// Whether a block has a `SkullBlockEntity`.
pub fn is_skull(block: BlockId) -> bool {
    block_entity_type(block) == "minecraft:skull"
}

/// Whether a block has a `BannerBlockEntity`.
pub fn is_banner(block: BlockId) -> bool {
    block_entity_type(block) == "minecraft:banner"
}

/// The `minecraft:block_entity_type` name of a block (empty without one).
pub fn block_entity_type(block: BlockId) -> &'static str {
    let id = block.info().block_entity;
    if id == u16::MAX {
        return "";
    }
    vanillite_data::registry::BLOCK_ENTITY_TYPE
        .name(id)
        .unwrap_or_default()
}

//! Data component values in saved data (Vanilla's persistent codecs).
//!
//! Placeholder: the converter of every component type replaces this module.

use vanillite_nbt::Tag;

/// The persistent (NBT) form of a component value; `None` if it cannot be converted.
pub fn component_to_nbt(type_id: u16, wire: &[u8]) -> Option<Tag> {
    let _ = (type_id, wire);
    None
}

/// The wire value of a component from its persistent (NBT) form; `None` if invalid.
pub fn component_from_nbt(type_id: u16, tag: &Tag) -> Option<Box<[u8]>> {
    let _ = (type_id, tag);
    None
}

//! Java items <-> Bedrock items (generated `items.bin`, see `data/README.md`) and the Bedrock
//! item stack formats.

use std::sync::OnceLock;

use vanillite_data::item::ItemId;
use vanillite_protocol::item::ItemStack;

use crate::io::{self, Reader, Result};

static ITEMS: &[u8] = include_bytes!("../data/items.bin");
static CREATIVE_JAVA: &[u8] = include_bytes!("../data/creative_java.bin");
/// Body of `creative_content`.
pub static CREATIVE: &[u8] = include_bytes!("../data/creative.bin");

/// The item tables of this build.
#[derive(Debug)]
pub struct Items {
    /// Bedrock item id and metadata per Java item id.
    to_bedrock: Vec<(i16, u16)>,
    /// Java item per creative item network id - 1.
    creative: Vec<u16>,
    /// Bedrock block runtime id per Java item (0 for items that place no block).
    block: Vec<u32>,
    /// `minecraft:damage` component type id.
    damage_component: Option<u16>,
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    b.get(i..i.saturating_add(4))
        .and_then(|b| b.try_into().ok())
        .map_or(0, u32::from_le_bytes)
}

impl Items {
    /// The tables (built on first use).
    pub fn get() -> &'static Items {
        static TABLE: OnceLock<Items> = OnceLock::new();
        TABLE.get_or_init(|| {
            let count = usize::try_from(u32_at(ITEMS, 0)).unwrap_or(0);
            let to_bedrock: Vec<(i16, u16)> = ITEMS
                .get(4..)
                .unwrap_or_default()
                .as_chunks::<4>()
                .0
                .iter()
                .take(count)
                .map(|c| {
                    (
                        i16::from_le_bytes([c[0], c[1]]),
                        u16::from_le_bytes([c[2], c[3]]),
                    )
                })
                .collect();
            let n = usize::try_from(u32_at(CREATIVE_JAVA, 0)).unwrap_or(0);
            let creative = CREATIVE_JAVA
                .get(4..)
                .unwrap_or_default()
                .as_chunks::<2>()
                .0
                .iter()
                .take(n)
                .map(|c| u16::from_le_bytes(*c))
                .collect();
            let blocks = crate::data::Blocks::get();
            let block = (0..to_bedrock.len())
                .map(|java| {
                    u16::try_from(java)
                        .ok()
                        .filter(|&j| usize::from(j) < vanillite_data::item::ITEMS.len())
                        .and_then(|j| ItemId(j).block())
                        .map_or(0, |b| {
                            let e = blocks.entry(u32::from(b.default_state().0));
                            u32::from(e & !crate::data::WATERLOGGED)
                        })
                })
                .collect();
            let damage_component =
                vanillite_data::registry::Registry::by_name("minecraft:data_component_type")
                    .and_then(|r| r.id("minecraft:damage"));
            Items {
                to_bedrock,
                creative,
                block,
                damage_component,
            }
        })
    }

    /// Bedrock item id and metadata of a Java item (0 = air for unknown items).
    #[inline]
    pub fn bedrock(&self, java: u16) -> (i16, u16) {
        self.to_bedrock
            .get(usize::from(java))
            .copied()
            .unwrap_or((0, 0))
    }

    /// The Java item of a creative item network id.
    pub fn creative(&self, net_id: u32) -> Option<u16> {
        let i = usize::try_from(net_id.checked_sub(1)?).ok()?;
        self.creative.get(i).copied()
    }

    /// Number of Java items.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.to_bedrock.len()
    }

    /// Damage of a stack (`minecraft:damage`).
    fn damage(&self, stack: &ItemStack) -> Option<i32> {
        let raw = stack.component(self.damage_component?)?;
        vanillite_protocol::codec::Reader::new(raw).varint().ok()
    }

    /// The user data of a stack (NBT with the damage of tools).
    fn write_user_data(&self, out: &mut Vec<u8>, stack: &ItemStack) {
        match self.damage(stack).filter(|&d| d > 0) {
            Some(d) => {
                let mut user = Vec::with_capacity(24);
                user.extend_from_slice(&(-1i16).to_le_bytes());
                user.push(1);
                user.extend_from_slice(&[0x0a, 0, 0, 3, 6, 0]);
                user.extend_from_slice(b"Damage");
                user.extend_from_slice(&d.to_le_bytes());
                user.push(0);
                user.extend_from_slice(&[0; 8]);
                io::write_bytes(out, &user);
            }
            None => io::write_bytes(out, &[0; 10]),
        }
    }

    /// Bedrock id, metadata and block runtime id of a stack; `None` for empty stacks.
    fn parts(&self, stack: &ItemStack) -> Option<(i16, u16, u32, u16)> {
        if stack.is_empty() {
            return None;
        }
        let (id, meta) = self.bedrock(stack.item);
        if id == 0 {
            return None;
        }
        let block = self
            .block
            .get(usize::from(stack.item))
            .copied()
            .unwrap_or(0);
        let count = u16::try_from(stack.count.clamp(0, 255)).unwrap_or(0);
        Some((id, meta, block, count))
    }

    /// Writes a stack as `ItemInstance` (inventories, equipment) with its stack network id.
    pub fn write_instance(&self, out: &mut Vec<u8>, stack: &ItemStack, net_id: i32) {
        let Some((id, meta, block, count)) = self.parts(stack) else {
            out.extend_from_slice(&[0; 8]);
            return;
        };
        out.extend_from_slice(&id.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        io::write_varu32(out, u32::from(meta));
        if net_id != 0 {
            out.push(1);
            io::write_vari32(out, net_id);
        } else {
            out.push(0);
        }
        io::write_varu32(out, block);
        self.write_user_data(out, stack);
    }
}

/// An `ItemInstance` as read from the client (user data skipped).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Instance {
    pub id: i16,
    pub count: u16,
    pub meta: u32,
    pub net_id: i32,
}

/// Reads an `ItemInstance`.
pub fn read_instance(r: &mut Reader<'_>) -> Result<Instance> {
    let [a, b] = r.array::<2>()?;
    let id = i16::from_le_bytes([a, b]);
    if id == 0 {
        // Air: count, metadata, no stack id, block runtime id, empty user data.
        let _ = r.array::<2>()?;
        r.varu32()?;
        if r.bool()? {
            r.vari32()?;
        }
        r.varu32()?;
        r.bytes("item user data", 0)?;
        return Ok(Instance::default());
    }
    let count = u16::from_le_bytes(r.array::<2>()?);
    let meta = r.varu32()?;
    let net_id = if r.bool()? { r.vari32()? } else { 0 };
    let _block = r.varu32()?;
    r.bytes("item user data", 1 << 16)?;
    Ok(Instance {
        id,
        count,
        meta,
        net_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_java_item_maps_to_a_bedrock_item() {
        let items = Items::get();
        assert_eq!(items.len(), vanillite_data::item::ITEMS.len());
        for java in 1..items.len() {
            let java = u16::try_from(java).unwrap_or(0);
            let (id, _) = items.bedrock(java);
            assert_ne!(id, 0, "{} has no Bedrock item", ItemId(java).name());
        }
        // Round trips of plain items, block items carry their block.
        let stone = ItemId::by_name("minecraft:stone").map_or(0, |i| i.0);
        let (id, _) = items.bedrock(stone);
        assert_ne!(items.block.get(usize::from(stone)), Some(&0));
        assert!(!items.creative.is_empty());
        assert!(items.creative(1).is_some());
        let mut out = Vec::new();
        items.write_instance(&mut out, &ItemStack::new(stone, 5), 7);
        let i = read_instance(&mut Reader::new(&out)).unwrap_or_default();
        assert_eq!((i.id, i.count, i.net_id), (id, 5, 7));
    }
}

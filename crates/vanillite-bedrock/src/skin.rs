//! Player skins on the Bedrock side.
//!
//! Bedrock players keep the skin their client sent (shown to other Bedrock players). Java players
//! get a plain built-in skin for now; Java skins are not fetched yet.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::Value;

use crate::io;
use crate::login::field;

/// A skin as `player_list` carries it.
#[derive(Debug, Clone)]
pub struct Skin {
    pub id: String,
    pub resource_patch: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
    pub geometry: Vec<u8>,
    pub geometry_version: Vec<u8>,
    pub slim: bool,
}

/// The classic humanoid model.
const DEFAULT_PATCH: &str = r#"{"geometry":{"default":"geometry.humanoid.custom"}}"#;

impl Skin {
    /// A plain 64x64 skin: skin-coloured head and arms, a shirt and trousers.
    pub fn plain() -> Self {
        let mut data = vec![0u8; 64 * 64 * 4];
        // Texture regions of the 64x64 layout (x, y, width, height, RGBA).
        let regions: [(usize, usize, usize, usize, [u8; 4]); 6] = [
            (0, 0, 32, 16, [0xc6, 0x8e, 0x6a, 0xff]),   // head
            (16, 16, 24, 16, [0x2f, 0x8f, 0xa8, 0xff]), // body
            (40, 16, 16, 16, [0xc6, 0x8e, 0x6a, 0xff]), // right arm
            (32, 48, 16, 16, [0xc6, 0x8e, 0x6a, 0xff]), // left arm
            (0, 16, 16, 16, [0x3b, 0x3b, 0x8f, 0xff]),  // right leg
            (16, 48, 16, 16, [0x3b, 0x3b, 0x8f, 0xff]), // left leg
        ];
        for (x0, y0, w, h, rgba) in regions {
            for y in y0..y0.saturating_add(h) {
                for x in x0..x0.saturating_add(w) {
                    let i = y.saturating_mul(64).saturating_add(x).saturating_mul(4);
                    if let Some(px) = data.get_mut(i..i.saturating_add(4)) {
                        px.copy_from_slice(&rgba);
                    }
                }
            }
        }
        Self {
            id: "vanillite.plain".into(),
            resource_patch: DEFAULT_PATCH.as_bytes().to_vec(),
            width: 64,
            height: 64,
            data,
            geometry: Vec::new(),
            geometry_version: b"0.0.0".to_vec(),
            slim: false,
        }
    }

    /// The skin of a Bedrock client's login data, if it is well-formed.
    pub fn from_client_data(c: &Value) -> Option<Self> {
        let decode = |k: &str| {
            STANDARD
                .decode(field(c, k).as_str().unwrap_or_default())
                .ok()
        };
        let width = u32::try_from(field(c, "SkinImageWidth").as_u64()?).ok()?;
        let height = u32::try_from(field(c, "SkinImageHeight").as_u64()?).ok()?;
        let data = decode("SkinData")?;
        let expected = usize::try_from(width.checked_mul(height)?.checked_mul(4)?).ok()?;
        if data.len() != expected || expected > 512 * 512 * 4 {
            return None;
        }
        let resource_patch = decode("SkinResourcePatch").unwrap_or_else(|| DEFAULT_PATCH.into());
        let slim = field(c, "ArmSize").as_str() == Some("slim");
        Some(Self {
            id: field(c, "SkinId")
                .as_str()
                .unwrap_or("custom")
                .chars()
                .take(128)
                .collect(),
            resource_patch,
            width,
            height,
            data,
            geometry: decode("SkinGeometryData").unwrap_or_default(),
            geometry_version: decode("SkinGeometryDataEngineVersion").unwrap_or_default(),
            slim,
        })
    }

    /// Writes the network form.
    pub fn write(&self, out: &mut Vec<u8>) {
        io::write_str(out, &self.id);
        io::write_bytes(out, &self.resource_patch);
        out.extend_from_slice(&self.width.to_le_bytes());
        out.extend_from_slice(&self.height.to_le_bytes());
        io::write_bytes(out, &self.data);
        io::write_varu32(out, 0); // animations
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        io::write_bytes(out, &[]); // cape
        io::write_bytes(out, &self.geometry);
        io::write_bytes(out, &self.geometry_version);
        io::write_bytes(out, &[]); // animation data
        io::write_str(out, ""); // cape id
        io::write_str(out, &self.id); // full id
        out.push(if self.slim { 0 } else { 1 });
        out.extend_from_slice(&[0; 4]); // skin colour
        io::write_varu32(out, 0); // persona pieces
        io::write_varu32(out, 0); // piece tints
        out.extend_from_slice(&[0, 0, 0, 0, 0]); // premium, persona, cape on classic, primary, override
        io::write_str(out, "True"); // trusted
        io::write_str(out, ""); // profile hash
    }
}

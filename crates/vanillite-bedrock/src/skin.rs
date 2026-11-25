//! Player skins on the Bedrock side.
//!
//! Bedrock players keep the skin their client sent (shown to other Bedrock players). Java players
//! start with a plain built-in skin; their Mojang skin is fetched from the texture server named
//! in their signed `textures` property and replaces it once loaded.
//!
//! Java players see Bedrock players with the default skin: Java clients only show skins signed by
//! Mojang, and a Bedrock skin has no such signature (uploading it to a signing service would send
//! player data to a third party), so the Java side keeps the default Steve/Alex look.

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
/// The slim model.
const SLIM_PATCH: &str = r#"{"geometry":{"default":"geometry.humanoid.customSlim"}}"#;

/// Where Java skins may be loaded from.
const TEXTURE_HOST: &str = "http://textures.minecraft.net/texture/";

/// The skin URL and model (slim) of a Java profile's `textures` property.
pub fn java_texture(properties: &[vanillite_protocol::types::Property]) -> Option<(String, bool)> {
    let p = properties.iter().find(|p| p.name == "textures")?;
    let json = STANDARD.decode(p.value.as_bytes()).ok()?;
    let v: Value = serde_json::from_slice(&json).ok()?;
    let skin = field(field(&v, "textures"), "SKIN");
    let url = skin.get("url")?.as_str()?;
    let hash = url
        .strip_prefix(TEXTURE_HOST)
        .or_else(|| url.strip_prefix("https://textures.minecraft.net/texture/"))?;
    if hash.is_empty() || hash.len() > 128 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let slim = field(field(skin, "metadata"), "model").as_str() == Some("slim");
    Some((format!("{TEXTURE_HOST}{hash}"), slim))
}

/// Loads a Java skin (blocking): 64x64 PNG from the texture server.
pub fn fetch_java(url: &str, slim: bool) -> Option<Skin> {
    if !url.starts_with(TEXTURE_HOST) {
        return None;
    }
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(5)))
        .build()
        .new_agent();
    let png = agent
        .get(url)
        .call()
        .ok()?
        .body_mut()
        .with_config()
        .limit(64 << 10)
        .read_to_vec()
        .ok()?;
    Skin::from_java_png(&png, slim, url.rsplit('/').next().unwrap_or("java"))
}

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

    /// A Java skin image (64x64) as a Bedrock skin.
    pub fn from_java_png(png: &[u8], slim: bool, id: &str) -> Option<Self> {
        let (w, h, data) = crate::png::decode_rgba(png)?;
        if (w, h) != (64, 64) {
            return None;
        }
        Some(Self {
            id: format!("vanillite.java.{id}"),
            resource_patch: if slim { SLIM_PATCH } else { DEFAULT_PATCH }
                .as_bytes()
                .to_vec(),
            width: w,
            height: h,
            data,
            geometry: Vec::new(),
            geometry_version: b"0.0.0".to_vec(),
            slim,
        })
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
    pub fn write(&self, out: &mut Vec<u8>, v: &crate::version::Version) {
        io::write_str(out, &self.id);
        if v.before_26_60() {
            io::write_str(out, ""); // PlayFab id
        }
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

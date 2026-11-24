//! Player data files: `<world>/playerdata/<uuid>.dat`, gzip-compressed named NBT in Vanilla's
//! layout, so worlds stay interchangeable with Vanilla.
//!
//! Only the fields the server keeps so far are read and written; unknown fields of an existing
//! file are kept when it is saved again.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use vanillite_nbt::{Budget, Compound, List, Tag};

/// Largest uncompressed player file that is read (untrusted file, SEC-26).
const MAX_FILE_BYTES: usize = 16 << 20;

/// The persisted state of one player.
#[derive(Clone, Debug, PartialEq)]
pub struct PlayerData {
    /// `Pos`.
    pub pos: [f64; 3],
    /// `Rotation`: yaw, pitch.
    pub rotation: [f32; 2],
    /// `OnGround`.
    pub on_ground: bool,
    /// `Dimension`, e.g. `minecraft:overworld`.
    pub dimension: String,
    /// `playerGameType`.
    pub game_mode: i32,
    /// Fields of the file this server does not handle yet.
    pub other: Compound,
}

/// Player data files of one world.
#[derive(Clone, Debug)]
pub struct PlayerDataStore {
    dir: PathBuf,
}

impl PlayerDataStore {
    /// The store in `<level_dir>/playerdata`.
    pub fn new(level_dir: &Path) -> PlayerDataStore {
        PlayerDataStore {
            dir: level_dir.join("playerdata"),
        }
    }

    fn path(&self, uuid: u128) -> PathBuf {
        self.dir.join(format!("{}.dat", uuid_string(uuid)))
    }

    /// Loads a player's data. `Ok(None)` if the player has none; a damaged file is an error.
    pub fn load(&self, uuid: u128) -> io::Result<Option<PlayerData>> {
        let compressed = match fs::read(self.path(uuid)) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let raw = gunzip(&compressed)?;
        let root = vanillite_nbt::read::read_named(&mut raw.as_slice(), &mut Budget::file())
            .map_err(io::Error::other)?;
        Ok(Some(PlayerData::from_nbt(root)))
    }

    /// Saves a player's data like Vanilla: written to a temporary file, then moved in place.
    pub fn save(&self, uuid: u128, data: &PlayerData) -> io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let mut raw = Vec::new();
        vanillite_nbt::write::write_named("", &data.to_nbt(), &mut raw)
            .map_err(io::Error::other)?;
        let mut compressor = libdeflater::Compressor::new(libdeflater::CompressionLvl::default());
        let mut out = vec![0; compressor.gzip_compress_bound(raw.len())];
        let n = compressor
            .gzip_compress(&raw, &mut out)
            .map_err(io::Error::other)?;
        out.truncate(n);
        let path = self.path(uuid);
        let tmp = path.with_extension("dat_tmp");
        fs::write(&tmp, &out)?;
        fs::rename(&tmp, &path)
    }
}

fn gunzip(data: &[u8]) -> io::Result<Vec<u8>> {
    // ISIZE, the last four bytes, is the uncompressed size modulo 2^32.
    let size = data
        .last_chunk::<4>()
        .map(|b| u32::from_le_bytes(*b))
        .and_then(|n| usize::try_from(n).ok())
        .filter(|&n| n <= MAX_FILE_BYTES)
        .ok_or_else(|| io::Error::other("player data file too large or truncated"))?;
    let mut out = vec![0; size];
    let n = libdeflater::Decompressor::new()
        .gzip_decompress(data, &mut out)
        .map_err(io::Error::other)?;
    out.truncate(n);
    Ok(out)
}

/// UUID in Java's `UUID.toString()` form.
pub fn uuid_string(v: u128) -> String {
    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        v >> 96,
        (v >> 80) & 0xffff,
        (v >> 64) & 0xffff,
        (v >> 48) & 0xffff,
        v & 0xffff_ffff_ffff
    )
}

fn number(tag: Option<&Tag>) -> Option<f64> {
    match tag? {
        Tag::Double(v) => Some(*v),
        Tag::Float(v) => Some(f64::from(*v)),
        _ => None,
    }
}

impl PlayerData {
    fn from_nbt(mut root: Compound) -> PlayerData {
        let list = |root: &mut Compound, name: &str| match root.remove(name) {
            Some(Tag::List(List(items))) => items,
            _ => Vec::new(),
        };
        let pos_list = list(&mut root, "Pos");
        let mut pos = [0.0; 3];
        for (slot, tag) in pos.iter_mut().zip(&pos_list) {
            *slot = number(Some(tag)).filter(|v| v.is_finite()).unwrap_or(0.0);
        }
        let rot_list = list(&mut root, "Rotation");
        let mut rotation = [0.0f32; 2];
        for (slot, tag) in rotation.iter_mut().zip(&rot_list) {
            *slot = match tag {
                Tag::Float(v) if v.is_finite() => *v,
                _ => 0.0,
            };
        }
        let on_ground = root
            .remove("OnGround")
            .and_then(|t| t.as_i64())
            .is_some_and(|v| v != 0);
        let dimension = root
            .remove("Dimension")
            .and_then(|t| t.as_str().map(str::to_owned))
            .unwrap_or_else(|| "minecraft:overworld".to_owned());
        let game_mode = root
            .remove("playerGameType")
            .and_then(|t| t.as_i64())
            .and_then(|v| i32::try_from(v).ok())
            .unwrap_or(-1);
        PlayerData {
            pos,
            rotation,
            on_ground,
            dimension,
            game_mode,
            other: root,
        }
    }

    fn to_nbt(&self) -> Compound {
        let mut root = self.other.clone();
        root.insert(
            "Pos",
            List(self.pos.iter().map(|&v| Tag::Double(v)).collect()),
        );
        root.insert(
            "Rotation",
            List(self.rotation.iter().map(|&v| Tag::Float(v)).collect()),
        );
        root.insert("OnGround", self.on_ground);
        root.insert("Dimension", self.dimension.as_str());
        root.insert("playerGameType", self.game_mode);
        root
    }
}

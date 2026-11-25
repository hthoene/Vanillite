//! Map data (`MapItemSavedData`, `MapIndex`) as far as items and the cartography table need it:
//! map ids, scale, lock state and copies for scaling and locking (`MapItem
//! .onCraftedPostProcess`). The colours, banners and frames of a map are kept as stored.
//!
//! All worlds share one [`Maps`] store (map ids are world-wide); the server loads entries from
//! `data/minecraft/maps/<id>.dat` on first use and writes the changed ones with every save.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use vanillite_nbt::{Compound, Tag};
use vanillite_protocol::codec::{Reader, write_varint};
use vanillite_protocol::components::ids as components;

use crate::item::ItemStack;
use crate::level::Level;

/// `MapItemSavedData.MAX_SCALE`.
pub const MAX_SCALE: i8 = 4;

/// The saved data id of the map index (`maps/last_id`).
pub const INDEX_ID: &str = "minecraft:maps/last_id";

/// The saved data id of map `id`.
pub fn data_id(id: i32) -> String {
    format!("minecraft:maps/{id}")
}

/// One map (`MapItemSavedData`).
#[derive(Clone, Debug, PartialEq)]
pub struct MapData {
    /// `dimension`.
    pub dimension: String,
    /// `xCenter`.
    pub x_center: i32,
    /// `zCenter`.
    pub z_center: i32,
    /// `scale` (0–4).
    pub scale: i8,
    /// `trackingPosition`.
    pub tracking_position: bool,
    /// `unlimitedTracking`.
    pub unlimited_tracking: bool,
    /// `locked`.
    pub locked: bool,
    /// `colors` (128 × 128).
    pub colors: Vec<i8>,
    /// `banners` and `frames` as stored.
    pub markers: Compound,
}

impl MapData {
    /// Reads the `data` compound (`MapItemSavedData.CODEC`).
    pub fn load(c: &Compound) -> Option<MapData> {
        let int = |k: &str| match c.get(k) {
            Some(Tag::Int(v)) => Some(*v),
            _ => None,
        };
        let flag = |k: &str, default: bool| match c.get(k) {
            Some(Tag::Byte(v)) => *v != 0,
            _ => default,
        };
        let dimension = match c.get("dimension") {
            Some(Tag::String(s)) => s.clone(),
            _ => return None,
        };
        let colors = match c.get("colors") {
            Some(Tag::ByteArray(b)) if b.len() == 16384 => b.clone(),
            _ => vec![0; 16384],
        };
        let mut markers = Compound::new();
        for key in ["banners", "frames"] {
            if let Some(tag) = c.get(key) {
                markers.insert(key, tag.clone());
            }
        }
        Some(MapData {
            dimension,
            x_center: int("xCenter")?,
            z_center: int("zCenter")?,
            scale: match c.get("scale") {
                Some(Tag::Byte(v)) => (*v).clamp(0, MAX_SCALE),
                _ => 0,
            },
            tracking_position: flag("trackingPosition", true),
            unlimited_tracking: flag("unlimitedTracking", false),
            locked: flag("locked", false),
            colors,
            markers,
        })
    }

    /// The `data` compound.
    pub fn save(&self) -> Compound {
        let mut c = Compound::new();
        c.insert("dimension", Tag::String(self.dimension.clone()));
        c.insert("xCenter", Tag::Int(self.x_center));
        c.insert("zCenter", Tag::Int(self.z_center));
        c.insert("scale", Tag::Byte(self.scale));
        c.insert("colors", Tag::ByteArray(self.colors.clone()));
        c.insert(
            "trackingPosition",
            Tag::Byte(i8::from(self.tracking_position)),
        );
        c.insert(
            "unlimitedTracking",
            Tag::Byte(i8::from(self.unlimited_tracking)),
        );
        c.insert("locked", Tag::Byte(i8::from(self.locked)));
        for key in ["banners", "frames"] {
            let list = self
                .markers
                .get(key)
                .cloned()
                .unwrap_or(Tag::List(vanillite_nbt::List::new()));
            c.insert(key, list);
        }
        c
    }

    /// `MapItemSavedData.createFresh(originX, originZ, scale, …)`.
    pub fn fresh(
        origin_x: f64,
        origin_z: f64,
        scale: i8,
        tracking_position: bool,
        unlimited_tracking: bool,
        dimension: String,
    ) -> MapData {
        let size = 128i32 << scale.clamp(0, MAX_SCALE);
        let area_x = ((origin_x + 64.0) / f64::from(size)).floor() as i32;
        let area_z = ((origin_z + 64.0) / f64::from(size)).floor() as i32;
        MapData {
            dimension,
            x_center: area_x * size + size / 2 - 64,
            z_center: area_z * size + size / 2 - 64,
            scale,
            tracking_position,
            unlimited_tracking,
            locked: false,
            colors: vec![0; 16384],
            markers: Compound::new(),
        }
    }

    /// `scaled()`: a fresh map one scale up around the same centre.
    pub fn scaled(&self) -> MapData {
        MapData::fresh(
            f64::from(self.x_center),
            f64::from(self.z_center),
            (self.scale + 1).clamp(0, MAX_SCALE),
            self.tracking_position,
            self.unlimited_tracking,
            self.dimension.clone(),
        )
    }

    /// `locked()`: a locked copy with the same colours and markers.
    pub fn locked(&self) -> MapData {
        MapData {
            locked: true,
            ..self.clone()
        }
    }
}

/// Loads the stored `data` compound of a saved data id.
pub type Loader = Box<dyn Fn(&str) -> Option<Compound> + Send + Sync>;

#[derive(Default)]
struct Inner {
    /// Loaded maps (`None`: no data for that id).
    maps: HashMap<i32, Option<MapData>>,
    /// `MapIndex.lastMapId` once read.
    last_id: Option<i32>,
    dirty: Vec<i32>,
    index_dirty: bool,
}

/// The maps of the server.
pub struct Maps {
    loader: Loader,
    inner: Mutex<Inner>,
}

impl std::fmt::Debug for Maps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Maps").finish_non_exhaustive()
    }
}

impl Maps {
    /// A store that reads stored data with `loader`.
    pub fn new(loader: Loader) -> Maps {
        Maps {
            loader,
            inner: Mutex::new(Inner::default()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `getMapData(id)`.
    pub fn get(&self, id: i32) -> Option<MapData> {
        let mut inner = self.lock();
        if let Some(entry) = inner.maps.get(&id) {
            return entry.clone();
        }
        let data = (self.loader)(&data_id(id)).and_then(|c| MapData::load(&c));
        inner.maps.insert(id, data.clone());
        data
    }

    /// `setMapData(id, data)`.
    pub fn set(&self, id: i32, data: MapData) {
        let mut inner = self.lock();
        inner.maps.insert(id, Some(data));
        if !inner.dirty.contains(&id) {
            inner.dirty.push(id);
        }
    }

    /// `getFreeMapId()`.
    pub fn free_id(&self) -> i32 {
        let mut inner = self.lock();
        let last = match inner.last_id {
            Some(v) => v,
            None => (self.loader)(INDEX_ID)
                .and_then(|c| match c.get("map") {
                    Some(Tag::Int(v)) => Some(*v),
                    _ => None,
                })
                .unwrap_or(-1),
        };
        let id = last.saturating_add(1);
        inner.last_id = Some(id);
        inner.index_dirty = true;
        id
    }

    /// The saved data to write: changed maps and the index.
    pub fn take_dirty(&self) -> Vec<(String, Compound)> {
        let mut inner = self.lock();
        let mut out = Vec::new();
        for id in std::mem::take(&mut inner.dirty) {
            if let Some(Some(data)) = inner.maps.get(&id) {
                out.push((data_id(id), data.save()));
            }
        }
        if std::mem::take(&mut inner.index_dirty)
            && let Some(last) = inner.last_id
        {
            let mut c = Compound::new();
            c.insert("map", Tag::Int(last));
            out.push((INDEX_ID.to_owned(), c));
        }
        out
    }
}

/// `MapPostProcessing`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PostProcessing {
    /// `LOCK`.
    Lock,
    /// `SCALE`.
    Scale,
}

/// The `map_id` of a stack.
pub fn map_id(stack: &ItemStack) -> Option<i32> {
    stack
        .component(components::MAP_ID)
        .and_then(|v| Reader::new(v).varint().ok())
}

/// Sets the `map_id` of a stack.
pub fn set_map_id(stack: &mut ItemStack, id: i32) {
    let mut out = Vec::with_capacity(5);
    write_varint(&mut out, id);
    stack
        .patch_mut()
        .set(components::MAP_ID, out.into_boxed_slice());
}

/// Sets `map_post_processing`.
pub fn set_post_processing(stack: &mut ItemStack, value: PostProcessing) {
    let id = match value {
        PostProcessing::Lock => 0,
        PostProcessing::Scale => 1,
    };
    stack
        .patch_mut()
        .set(components::MAP_POST_PROCESSING, Box::new([id]));
}

/// Scale and lock state of a map (what the cartography table reads).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MapInfo {
    /// `scale`.
    pub scale: i8,
    /// `locked`.
    pub locked: bool,
}

/// `MapItem.getSavedData(stack, level)` as [`MapInfo`].
pub fn info(level: &Level, stack: &ItemStack) -> Option<MapInfo> {
    let data = level.maps()?.get(map_id(stack)?)?;
    Some(MapInfo {
        scale: data.scale,
        locked: data.locked,
    })
}

/// `MapItem.onCraftedPostProcess`: a stack with `map_post_processing` becomes a scaled or
/// locked copy of its map under a new id.
pub fn on_crafted(level: &mut Level, stack: &mut ItemStack) {
    let Some(value) = stack
        .component(components::MAP_POST_PROCESSING)
        .map(<[u8]>::to_vec)
    else {
        return;
    };
    stack.patch_mut().unset(components::MAP_POST_PROCESSING);
    let Some(maps) = level.maps() else {
        return;
    };
    let Some(original) = map_id(stack).and_then(|id| maps.get(id)) else {
        return;
    };
    let data = match value.first() {
        Some(0) => original.locked(),
        Some(1) => original.scaled(),
        _ => return,
    };
    let id = maps.free_id();
    maps.set(id, data);
    set_map_id(stack, id);
}

//! The world border of one dimension (`WorldBorder`): centre, size with its lerp, damage,
//! warnings; saved as the dimension's saved data `minecraft:world_border`.

use vanillite_base::{BlockPos, Vec3};
use vanillite_nbt::{Compound, Tag};

/// `WorldBorder.MAX_SIZE` (`5.999997E7F`).
pub const MAX_SIZE: f64 = 59_999_968.0;
/// `WorldBorder.MAX_CENTER_COORDINATE`.
pub const MAX_CENTER_COORDINATE: f64 = 29_999_984.0;

/// `WorldBorder.Settings`: what is saved.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    pub center_x: f64,
    pub center_z: f64,
    pub damage_per_block: f64,
    pub safe_zone: f64,
    pub warning_blocks: i32,
    pub warning_time: i32,
    pub size: f64,
    pub lerp_time: i64,
    pub lerp_target: f64,
}

impl Default for Settings {
    /// `Settings.DEFAULT`.
    fn default() -> Self {
        Settings {
            center_x: 0.0,
            center_z: 0.0,
            damage_per_block: 0.2,
            safe_zone: 5.0,
            warning_blocks: 5,
            warning_time: 300,
            size: MAX_SIZE,
            lerp_time: 0,
            lerp_target: 0.0,
        }
    }
}

/// A numeric tag as a double (`Codec.DOUBLE` reads any number).
#[allow(clippy::cast_precision_loss, clippy::as_conversions)]
fn as_f64(tag: &Tag) -> Option<f64> {
    Some(match *tag {
        Tag::Byte(v) => f64::from(v),
        Tag::Short(v) => f64::from(v),
        Tag::Int(v) => f64::from(v),
        Tag::Long(v) => v as f64,
        Tag::Float(v) => f64::from(v),
        Tag::Double(v) => v,
        _ => return None,
    })
}

impl Settings {
    /// `Settings.CODEC`; missing or invalid fields keep their defaults.
    pub fn from_nbt(data: &Compound) -> Settings {
        let d = Settings::default();
        let double = |key: &str, default: f64| data.get(key).and_then(as_f64).unwrap_or(default);
        let int = |key: &str, default: i32| {
            data.get(key)
                .and_then(Tag::as_i64)
                .and_then(|v| i32::try_from(v).ok())
                .unwrap_or(default)
        };
        let center = |key: &str| {
            let v = double(key, 0.0);
            if (-MAX_CENTER_COORDINATE..=MAX_CENTER_COORDINATE).contains(&v) {
                v
            } else {
                0.0
            }
        };
        Settings {
            center_x: center("center_x"),
            center_z: center("center_z"),
            damage_per_block: double("damage_per_block", d.damage_per_block),
            safe_zone: double("safe_zone", d.safe_zone),
            warning_blocks: int("warning_blocks", d.warning_blocks),
            warning_time: int("warning_time", d.warning_time),
            size: double("size", d.size),
            lerp_time: data.get("lerp_time").and_then(Tag::as_i64).unwrap_or(0),
            lerp_target: double("lerp_target", d.lerp_target),
        }
    }

    /// The saved compound.
    pub fn to_nbt(&self) -> Compound {
        Compound::with_capacity(9)
            .with("center_x", self.center_x)
            .with("center_z", self.center_z)
            .with("damage_per_block", self.damage_per_block)
            .with("safe_zone", self.safe_zone)
            .with("warning_blocks", self.warning_blocks)
            .with("warning_time", self.warning_time)
            .with("size", self.size)
            .with("lerp_time", self.lerp_time)
            .with("lerp_target", self.lerp_target)
    }
}

/// `BorderStatus`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Growing,
    Shrinking,
    Stationary,
}

/// `BorderExtent`: a static size or a size moving between two sizes.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Extent {
    Static(f64),
    Moving {
        from: f64,
        to: f64,
        lerp_begin: i64,
        lerp_end: i64,
        lerp_duration: f64,
        lerp_progress: i64,
        size: f64,
        previous_size: f64,
    },
}

/// `WorldBorder`.
#[derive(Clone, Debug, PartialEq)]
pub struct WorldBorder {
    damage_per_block: f64,
    safe_zone: f64,
    warning_time: i32,
    warning_blocks: i32,
    center_x: f64,
    center_z: f64,
    absolute_max_size: i32,
    extent: Extent,
    /// `SavedData.isDirty`.
    dirty: bool,
}

impl Default for WorldBorder {
    fn default() -> Self {
        WorldBorder::new(Settings::default(), 0)
    }
}

/// `MovingBorderExtent.calculateSize`.
fn moving_size(from: f64, to: f64, duration: f64, progress: i64) -> f64 {
    #[allow(clippy::cast_precision_loss, clippy::as_conversions)]
    let t = (duration - progress as f64) / duration;
    if t < 1.0 { from + t * (to - from) } else { to }
}

impl WorldBorder {
    /// A border with `settings` applied at `game_time` (`applyInitialSettings`).
    pub fn new(settings: Settings, game_time: i64) -> WorldBorder {
        let mut border = WorldBorder {
            damage_per_block: settings.damage_per_block,
            safe_zone: settings.safe_zone,
            warning_time: settings.warning_time,
            warning_blocks: settings.warning_blocks,
            center_x: settings.center_x,
            center_z: settings.center_z,
            absolute_max_size: 29_999_984,
            extent: Extent::Static(settings.size),
            dirty: false,
        };
        if settings.lerp_time > 0 {
            border.lerp_size_between(
                settings.size,
                settings.lerp_target,
                settings.lerp_time,
                game_time,
            );
        }
        border.dirty = false;
        border
    }

    /// `Settings(WorldBorder)`.
    pub fn settings(&self) -> Settings {
        Settings {
            center_x: self.center_x,
            center_z: self.center_z,
            damage_per_block: self.damage_per_block,
            safe_zone: self.safe_zone,
            warning_blocks: self.warning_blocks,
            warning_time: self.warning_time,
            size: self.size(),
            lerp_time: self.lerp_time(),
            lerp_target: self.lerp_target(),
        }
    }

    /// The settings to save when they changed since the last call.
    pub fn take_dirty(&mut self) -> Option<Settings> {
        std::mem::take(&mut self.dirty).then(|| self.settings())
    }

    /// `getCenterX`.
    pub fn center_x(&self) -> f64 {
        self.center_x
    }

    /// `getCenterZ`.
    pub fn center_z(&self) -> f64 {
        self.center_z
    }

    /// `getSize`.
    pub fn size(&self) -> f64 {
        match self.extent {
            Extent::Static(size) | Extent::Moving { size, .. } => size,
        }
    }

    /// `getLerpTime`: the ticks left of a moving border.
    pub fn lerp_time(&self) -> i64 {
        match self.extent {
            Extent::Static(_) => 0,
            Extent::Moving { lerp_progress, .. } => lerp_progress,
        }
    }

    /// `getLerpTarget`.
    pub fn lerp_target(&self) -> f64 {
        match self.extent {
            Extent::Static(size) => size,
            Extent::Moving { to, .. } => to,
        }
    }

    /// `getLerpSpeed`.
    pub fn lerp_speed(&self) -> f64 {
        match self.extent {
            Extent::Static(_) => 0.0,
            #[allow(clippy::cast_precision_loss, clippy::as_conversions)]
            Extent::Moving {
                from,
                to,
                lerp_begin,
                lerp_end,
                ..
            } => (from - to).abs() / lerp_end.saturating_sub(lerp_begin) as f64,
        }
    }

    /// `getStatus`.
    pub fn status(&self) -> Status {
        match self.extent {
            Extent::Static(_) => Status::Stationary,
            Extent::Moving { from, to, .. } if to < from => Status::Shrinking,
            Extent::Moving { .. } => Status::Growing,
        }
    }

    /// `getAbsoluteMaxSize`.
    pub fn absolute_max_size(&self) -> i32 {
        self.absolute_max_size
    }

    /// `setAbsoluteMaxSize` (`max-world-size`).
    pub fn set_absolute_max_size(&mut self, size: i32) {
        self.absolute_max_size = size;
    }

    /// `getDamagePerBlock`.
    pub fn damage_per_block(&self) -> f64 {
        self.damage_per_block
    }

    /// `getSafeZone`.
    pub fn safe_zone(&self) -> f64 {
        self.safe_zone
    }

    /// `getWarningTime` in ticks.
    pub fn warning_time(&self) -> i32 {
        self.warning_time
    }

    /// `getWarningBlocks`.
    pub fn warning_blocks(&self) -> i32 {
        self.warning_blocks
    }

    /// The size the edges use: a moving border's previous size (`getMinX(0.0F)` lerps from it).
    fn edge_size(&self) -> f64 {
        match self.extent {
            Extent::Static(size) => size,
            Extent::Moving { previous_size, .. } => previous_size,
        }
    }

    /// The clamped edge on one axis: `center + sign * size / 2`.
    fn edge(&self, center: f64, sign: f64) -> f64 {
        let max = f64::from(self.absolute_max_size);
        (center + sign * self.edge_size() / 2.0).clamp(-max, max)
    }

    /// `getMinX`.
    pub fn min_x(&self) -> f64 {
        self.edge(self.center_x, -1.0)
    }

    /// `getMaxX`.
    pub fn max_x(&self) -> f64 {
        self.edge(self.center_x, 1.0)
    }

    /// `getMinZ`.
    pub fn min_z(&self) -> f64 {
        self.edge(self.center_z, -1.0)
    }

    /// `getMaxZ`.
    pub fn max_z(&self) -> f64 {
        self.edge(self.center_z, 1.0)
    }

    /// `isWithinBounds(x, z, margin)`.
    pub fn is_within_bounds_margin(&self, x: f64, z: f64, margin: f64) -> bool {
        x >= self.min_x() - margin
            && x < self.max_x() + margin
            && z >= self.min_z() - margin
            && z < self.max_z() + margin
    }

    /// `isWithinBounds(x, z)`.
    pub fn is_within_bounds(&self, x: f64, z: f64) -> bool {
        self.is_within_bounds_margin(x, z, 0.0)
    }

    /// `isWithinBounds(AABB)` of a box `[min_x, max_x] × [min_z, max_z]`.
    pub fn is_box_within_bounds(&self, min_x: f64, min_z: f64, max_x: f64, max_z: f64) -> bool {
        let epsilon = f64::from(1.0e-5_f32);
        self.is_within_bounds(min_x, min_z)
            && self.is_within_bounds(max_x - epsilon, max_z - epsilon)
    }

    /// `isWithinBounds(BlockPos)`.
    pub fn is_block_within_bounds(&self, pos: BlockPos) -> bool {
        self.is_within_bounds(f64::from(pos.x), f64::from(pos.z))
    }

    /// `isWithinBounds(ChunkPos)`: both corner blocks of the chunk.
    pub fn is_chunk_within_bounds(&self, pos: vanillite_base::ChunkPos) -> bool {
        let (x, z) = (f64::from(pos.x * 16), f64::from(pos.z * 16));
        self.is_within_bounds(x, z) && self.is_within_bounds(x + 15.0, z + 15.0)
    }

    /// `clampVec3ToBound(x, y, z)`.
    pub fn clamp_vec(&self, x: f64, y: f64, z: f64) -> Vec3 {
        let epsilon = f64::from(1.0e-5_f32);
        Vec3::new(
            x.max(self.min_x()).min(self.max_x() - epsilon),
            y,
            z.max(self.min_z()).min(self.max_z() - epsilon),
        )
    }

    /// `clampToBounds(x, y, z)`.
    pub fn clamp_to_bounds(&self, x: f64, y: f64, z: f64) -> BlockPos {
        let v = self.clamp_vec(x, y, z);
        BlockPos::containing(v.x, v.y, v.z)
    }

    /// `isInsideCloseToBorder(entity, box)`: an entity at `x`, `z` with a box of
    /// `x_size` × `z_size` is near enough to the border to collide with it.
    pub fn is_inside_close_to_border(&self, x: f64, z: f64, x_size: f64, z_size: f64) -> bool {
        let bb_max = x_size.abs().max(z_size.abs()).max(1.0);
        self.distance_to_border(x, z) < bb_max * 2.0 && self.is_within_bounds_margin(x, z, bb_max)
    }

    /// `getCollisionShape` of a static border as the block box it keeps entities in:
    /// `[floor(minX), ceil(maxX)] × [floor(minZ), ceil(maxZ)]`.
    pub fn collision_box(&self) -> (f64, f64, f64, f64) {
        (
            self.min_x().floor(),
            self.min_z().floor(),
            self.max_x().ceil(),
            self.max_z().ceil(),
        )
    }

    /// `getDistanceToBorder(x, z)`.
    pub fn distance_to_border(&self, x: f64, z: f64) -> f64 {
        let from_north = z - self.min_z();
        let from_south = self.max_z() - z;
        let from_west = x - self.min_x();
        let from_east = self.max_x() - x;
        from_west.min(from_east).min(from_north).min(from_south)
    }

    /// `setCenter`.
    pub fn set_center(&mut self, x: f64, z: f64) {
        self.center_x = x;
        self.center_z = z;
        self.dirty = true;
    }

    /// `setSize`.
    pub fn set_size(&mut self, size: f64) {
        self.extent = Extent::Static(size);
        self.dirty = true;
    }

    /// `lerpSizeBetween`.
    pub fn lerp_size_between(&mut self, from: f64, to: f64, ticks: i64, game_time: i64) {
        self.extent = if from == to {
            Extent::Static(to)
        } else {
            #[allow(clippy::cast_precision_loss, clippy::as_conversions)]
            let lerp_duration = ticks as f64;
            let size = moving_size(from, to, lerp_duration, ticks);
            Extent::Moving {
                from,
                to,
                lerp_begin: game_time,
                lerp_end: game_time.saturating_add(ticks),
                lerp_duration,
                lerp_progress: ticks,
                size,
                previous_size: size,
            }
        };
        self.dirty = true;
    }

    /// `setSafeZone`.
    pub fn set_safe_zone(&mut self, safe_zone: f64) {
        self.safe_zone = safe_zone;
        self.dirty = true;
    }

    /// `setDamagePerBlock`.
    pub fn set_damage_per_block(&mut self, damage: f64) {
        self.damage_per_block = damage;
        self.dirty = true;
    }

    /// `setWarningTime`.
    pub fn set_warning_time(&mut self, ticks: i32) {
        self.warning_time = ticks;
        self.dirty = true;
    }

    /// `setWarningBlocks`.
    pub fn set_warning_blocks(&mut self, blocks: i32) {
        self.warning_blocks = blocks;
        self.dirty = true;
    }

    /// `tick`: a moving border advances one tick and stops at its target.
    pub fn tick(&mut self) {
        let Extent::Moving {
            from,
            to,
            lerp_duration,
            lerp_progress,
            size,
            previous_size,
            ..
        } = &mut self.extent
        else {
            return;
        };
        *lerp_progress = lerp_progress.saturating_sub(1);
        *previous_size = *size;
        *size = moving_size(*from, *to, *lerp_duration, *lerp_progress);
        self.dirty = true;
        if *lerp_progress <= 0 {
            self.extent = Extent::Static(*to);
        }
    }

    /// `LivingEntity.baseTick`'s `outOfBorder` damage of a player whose box is outside the
    /// border: `None` inside the border and its safe zone.
    pub fn damage(&self, x: f64, z: f64, half_width: f64) -> Option<f32> {
        if self.is_box_within_bounds(
            x - half_width,
            z - half_width,
            x + half_width,
            z + half_width,
        ) {
            return None;
        }
        let distance = self.distance_to_border(x, z) + self.safe_zone;
        if distance >= 0.0 || self.damage_per_block <= 0.0 {
            return None;
        }
        let damage = vanillite_base::math::floor(-distance * self.damage_per_block).max(1);
        #[allow(clippy::cast_precision_loss, clippy::as_conversions)]
        Some(damage as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lerps_to_the_target_and_stops() {
        let mut border = WorldBorder::default();
        border.set_size(100.0);
        border.lerp_size_between(100.0, 50.0, 10, 0);
        assert_eq!(border.status(), Status::Shrinking);
        assert_eq!(border.size(), 100.0);
        for _ in 0..5 {
            border.tick();
        }
        assert_eq!(border.size(), 75.0);
        assert_eq!(border.lerp_time(), 5);
        for _ in 0..5 {
            border.tick();
        }
        assert_eq!(border.size(), 50.0);
        assert_eq!(border.status(), Status::Stationary);
    }

    #[test]
    fn damages_outside_the_safe_zone() {
        let mut border = WorldBorder::default();
        border.set_size(10.0);
        assert_eq!(border.damage(0.0, 0.0, 0.3), None);
        // The first 5 blocks past the edge are the safe zone.
        assert_eq!(border.damage(10.0, 0.0, 0.3), None);
        assert_eq!(border.damage(15.0, 0.0, 0.3), Some(1.0));
        assert_eq!(border.damage(55.0, 0.0, 0.3), Some(9.0));
    }

    #[test]
    fn settings_round_trip() {
        let mut border = WorldBorder::default();
        border.set_center(3.5, -2.0);
        border.lerp_size_between(200.0, 20.0, 100, 7);
        let settings = border.take_dirty();
        assert!(settings.is_some());
        assert_eq!(border.take_dirty(), None);
        let read = Settings::from_nbt(&settings.unwrap_or_default().to_nbt());
        let again = WorldBorder::new(read, 7);
        assert_eq!(again.center_x(), 3.5);
        assert_eq!(again.lerp_target(), 20.0);
        assert_eq!(again.lerp_time(), 100);
    }
}

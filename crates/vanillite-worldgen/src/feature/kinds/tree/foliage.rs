//! Foliage placers (`foliageplacers/`): grow a tree's leaves around its foliage attachments.

use serde_json::Value;
use vanillite_base::math::floor_f;
use vanillite_base::random::{DecorationRandom, RandomSource};
use vanillite_base::{BlockPos, Direction};

use super::{
    Axis, HORIZONTAL, TreeWriter, clockwise, dist_manhattan, has_property, is_water_source,
    relative, try_set_bool, with_axis,
};
use crate::Error;
use crate::carver::providers::IntProvider;
use crate::feature::{field, float_field, int_field};

/// `FoliagePlacer.FoliageAttachment`: where foliage grows and how it is shaped there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FoliageAttachment {
    pub pos: BlockPos,
    pub radius_offset_xz: i32,
    pub foliage_height_offset: i32,
    pub size_x: i32,
    pub size_z: i32,
}

impl FoliageAttachment {
    /// `new FoliageAttachment(pos, radiusOffsetXZ, doubleTrunk)`.
    pub const fn new(pos: BlockPos, radius_offset_xz: i32, double_trunk: bool) -> Self {
        let size = if double_trunk { 2 } else { 1 };
        Self {
            pos,
            radius_offset_xz,
            foliage_height_offset: 0,
            size_x: size,
            size_z: size,
        }
    }

    /// `doubleTrunk()`.
    pub const fn double_trunk(&self) -> bool {
        self.size_x == 2 && self.size_z == 2
    }
}

/// `FoliagePlacer`: radius and offset providers plus the placer type.
#[derive(Clone, Debug)]
pub struct FoliagePlacer {
    radius: IntProvider,
    offset: IntProvider,
    kind: FoliageKind,
}

#[derive(Clone, Debug)]
enum FoliageKind {
    /// `BlobFoliagePlacer`.
    Blob { height: i32 },
    /// `BushFoliagePlacer`.
    Bush { height: i32 },
    /// `FancyFoliagePlacer`.
    Fancy { height: i32 },
    /// `SpruceFoliagePlacer`.
    Spruce { trunk_height: IntProvider },
    /// `PineFoliagePlacer`.
    Pine { height: IntProvider },
    /// `AcaciaFoliagePlacer`.
    Acacia,
    /// `MegaJungleFoliagePlacer` (`jungle_foliage_placer`).
    MegaJungle { height: i32 },
    /// `MegaPineFoliagePlacer`.
    MegaPine { crown_height: IntProvider },
    /// `DarkOakFoliagePlacer`.
    DarkOak,
    /// `RandomSpreadFoliagePlacer`.
    RandomSpread {
        foliage_height: IntProvider,
        leaf_placement_attempts: i32,
    },
    /// `CherryFoliagePlacer`.
    Cherry {
        height: IntProvider,
        wide_bottom_layer_hole_chance: f32,
        corner_hole_chance: f32,
        hanging_leaves_chance: f32,
        hanging_leaves_extension_chance: f32,
    },
    /// `PoplarFoliagePlacer`.
    Poplar {
        height: IntProvider,
        side_hole_chance: f32,
    },
}

fn int_in(json: &Value, name: &str, min: i32, max: i32) -> Result<i32, Error> {
    let value = int_field(json, name)?;
    if (min..=max).contains(&value) {
        Ok(value)
    } else {
        Err(Error::new(format!("{name} {value} outside [{min}, {max}]")))
    }
}

fn int_provider(json: &Value, name: &str, min: i32, max: i32) -> Result<IntProvider, Error> {
    IntProvider::from_json_in_range(field(json, name)?, min, max)
}

fn chance(json: &Value, name: &str) -> Result<f32, Error> {
    let value = float_field(json, name)?;
    if (0.0..=1.0).contains(&value) {
        Ok(value)
    } else {
        Err(Error::new(format!("{name} {value} outside [0, 1]")))
    }
}

impl FoliagePlacer {
    /// Parses `FoliagePlacer.CODEC`.
    pub fn parse(json: &Value) -> Result<Self, Error> {
        let kind = json
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::new("foliage placer without type"))?;
        let kind = match kind.strip_prefix("minecraft:").unwrap_or(kind) {
            "blob_foliage_placer" => FoliageKind::Blob {
                height: int_in(json, "height", 0, 16)?,
            },
            "bush_foliage_placer" => FoliageKind::Bush {
                height: int_in(json, "height", 0, 16)?,
            },
            "fancy_foliage_placer" => FoliageKind::Fancy {
                height: int_in(json, "height", 0, 16)?,
            },
            "spruce_foliage_placer" => FoliageKind::Spruce {
                trunk_height: int_provider(json, "trunk_height", 0, 24)?,
            },
            "pine_foliage_placer" => FoliageKind::Pine {
                height: int_provider(json, "height", 0, 24)?,
            },
            "acacia_foliage_placer" => FoliageKind::Acacia,
            "jungle_foliage_placer" => FoliageKind::MegaJungle {
                height: int_in(json, "height", 0, 16)?,
            },
            "mega_pine_foliage_placer" => FoliageKind::MegaPine {
                crown_height: int_provider(json, "crown_height", 0, 24)?,
            },
            "dark_oak_foliage_placer" => FoliageKind::DarkOak,
            "random_spread_foliage_placer" => FoliageKind::RandomSpread {
                foliage_height: int_provider(json, "foliage_height", 1, 512)?,
                leaf_placement_attempts: int_in(json, "leaf_placement_attempts", 0, 256)?,
            },
            "cherry_foliage_placer" => FoliageKind::Cherry {
                height: int_provider(json, "height", 4, 16)?,
                wide_bottom_layer_hole_chance: chance(json, "wide_bottom_layer_hole_chance")?,
                corner_hole_chance: chance(json, "corner_hole_chance")?,
                hanging_leaves_chance: chance(json, "hanging_leaves_chance")?,
                hanging_leaves_extension_chance: chance(json, "hanging_leaves_extension_chance")?,
            },
            "poplar_foliage_placer" => FoliageKind::Poplar {
                height: int_provider(json, "height", 5, 16)?,
                side_hole_chance: chance(json, "side_hole_chance")?,
            },
            other => return Err(Error::new(format!("unknown foliage placer type {other}"))),
        };
        Ok(Self {
            radius: int_provider(json, "radius", 0, 16)?,
            offset: int_provider(json, "offset", 0, 16)?,
            kind,
        })
    }

    /// `foliageHeight(random, treeHeight, tree)`.
    pub fn foliage_height(&self, random: &mut DecorationRandom, tree_height: i32) -> i32 {
        match &self.kind {
            FoliageKind::Blob { height }
            | FoliageKind::Bush { height }
            | FoliageKind::Fancy { height }
            | FoliageKind::MegaJungle { height } => *height,
            FoliageKind::Spruce { trunk_height } => {
                4.max(tree_height - trunk_height.sample(random))
            }
            FoliageKind::Pine { height }
            | FoliageKind::Cherry { height, .. }
            | FoliageKind::Poplar { height, .. } => height.sample(random),
            FoliageKind::MegaPine { crown_height } => crown_height.sample(random),
            FoliageKind::RandomSpread { foliage_height, .. } => foliage_height.sample(random),
            FoliageKind::Acacia => 0,
            FoliageKind::DarkOak => 4,
        }
    }

    /// `foliageRadius(random, trunkHeight)`.
    pub fn foliage_radius(&self, random: &mut DecorationRandom, trunk_height: i32) -> i32 {
        let radius = self.radius.sample(random);
        match self.kind {
            FoliageKind::Pine { .. } => radius + random.next_int_bounded((trunk_height + 1).max(1)),
            _ => radius,
        }
    }

    /// `createFoliage(level, foliageSetter, random, tree, treeHeight, attachment,
    /// foliageHeight, leafRadius)`: samples the offset, then grows the foliage.
    pub fn create_foliage(
        &self,
        w: &mut TreeWriter<'_, '_, '_>,
        _tree_height: i32,
        attachment: &FoliageAttachment,
        foliage_height: i32,
        leaf_radius: i32,
    ) {
        let offset = self.offset.sample(w.random());
        let a = attachment;
        let double_trunk = a.double_trunk();
        match &self.kind {
            FoliageKind::Blob { .. } => {
                let height = foliage_height + a.foliage_height_offset;
                let mut yo = offset;
                while yo >= offset - height {
                    let radius = (leaf_radius + a.radius_offset_xz - 1 - yo / 2).max(0);
                    self.place_leaves_row(w, a.pos, radius, yo, double_trunk);
                    yo -= 1;
                }
            }
            FoliageKind::Bush { .. } => {
                let height = foliage_height + a.foliage_height_offset;
                let mut yo = offset;
                while yo >= offset - height {
                    let radius = leaf_radius + a.radius_offset_xz - 1 - yo;
                    self.place_leaves_row(w, a.pos, radius, yo, double_trunk);
                    yo -= 1;
                }
            }
            FoliageKind::Fancy { .. } => {
                let mut yo = offset;
                while yo >= offset - foliage_height {
                    let radius =
                        leaf_radius + i32::from(yo != offset && yo != offset - foliage_height);
                    self.place_leaves_row(w, a.pos, radius, yo, double_trunk);
                    yo -= 1;
                }
            }
            FoliageKind::Spruce { .. } => {
                let mut radius = w.random().next_int_bounded(2);
                let mut max_radius = 1;
                let mut min_radius = 0;
                let height = foliage_height + a.foliage_height_offset;
                let mut yo = offset;
                while yo >= -height {
                    self.place_leaves_row(w, a.pos, radius, yo, double_trunk);
                    if radius >= max_radius {
                        radius = min_radius;
                        min_radius = 1;
                        max_radius = (max_radius + 1).min(leaf_radius + a.radius_offset_xz);
                    } else {
                        radius += 1;
                    }
                    yo -= 1;
                }
            }
            FoliageKind::Pine { .. } => {
                let mut radius = 0;
                let height = foliage_height + a.foliage_height_offset;
                let mut yo = offset;
                while yo >= offset - height {
                    self.place_leaves_row(w, a.pos, radius, yo, double_trunk);
                    if radius >= 1 && yo == offset - height + 1 {
                        radius -= 1;
                    } else if radius < leaf_radius + a.radius_offset_xz {
                        radius += 1;
                    }
                    yo -= 1;
                }
            }
            FoliageKind::Acacia => {
                let pos = a.pos.offset(0, offset, 0);
                let height = foliage_height + a.foliage_height_offset;
                self.place_leaves_row(
                    w,
                    pos,
                    leaf_radius + a.radius_offset_xz,
                    -1 - height,
                    double_trunk,
                );
                self.place_leaves_row(w, pos, leaf_radius - 1, -height, double_trunk);
                self.place_leaves_row(
                    w,
                    pos,
                    leaf_radius + a.radius_offset_xz - 1,
                    0,
                    double_trunk,
                );
            }
            FoliageKind::MegaJungle { .. } => {
                let leaf_height = if double_trunk {
                    foliage_height
                } else {
                    1 + w.random().next_int_bounded(2)
                } + a.foliage_height_offset;
                let mut yo = offset;
                while yo >= offset - leaf_height {
                    let radius = leaf_radius + a.radius_offset_xz + 1 - yo;
                    self.place_leaves_row(w, a.pos, radius, yo, double_trunk);
                    yo -= 1;
                }
            }
            FoliageKind::MegaPine { .. } => {
                let mut prev_radius = 0;
                let height = foliage_height + a.foliage_height_offset;
                for yy in a.pos.y - height + offset..=a.pos.y + offset {
                    let yo = a.pos.y - yy;
                    let smooth_radius = leaf_radius
                        + a.radius_offset_xz
                        + floor_f(yo as f32 / height as f32 * 3.5_f32);
                    let jagged_radius = if yo > 0 && smooth_radius == prev_radius && yy & 1 == 0 {
                        smooth_radius + 1
                    } else {
                        smooth_radius
                    };
                    let pos = BlockPos::new(a.pos.x, yy, a.pos.z);
                    self.place_leaves_row(w, pos, jagged_radius, 0, double_trunk);
                    prev_radius = smooth_radius;
                }
            }
            FoliageKind::DarkOak => {
                let pos = a.pos.offset(0, offset, 0);
                if double_trunk {
                    self.place_leaves_row(w, pos, leaf_radius + 2, -1, true);
                    self.place_leaves_row(w, pos, leaf_radius + 3, 0, true);
                    self.place_leaves_row(w, pos, leaf_radius + 2, 1, true);
                    if w.random().next_boolean() {
                        self.place_leaves_row(w, pos, leaf_radius, 2, true);
                    }
                } else {
                    self.place_leaves_row(w, pos, leaf_radius + 2, -1, false);
                    self.place_leaves_row(w, pos, leaf_radius + 1, 0, false);
                }
            }
            FoliageKind::RandomSpread {
                leaf_placement_attempts,
                ..
            } => {
                for _ in 0..*leaf_placement_attempts {
                    let random = w.random();
                    let dx =
                        random.next_int_bounded(leaf_radius) - random.next_int_bounded(leaf_radius);
                    let dy = random.next_int_bounded(foliage_height)
                        - random.next_int_bounded(foliage_height);
                    let dz =
                        random.next_int_bounded(leaf_radius) - random.next_int_bounded(leaf_radius);
                    try_place_leaf(w, a.pos.offset(dx, dy, dz));
                }
            }
            FoliageKind::Cherry {
                hanging_leaves_chance,
                hanging_leaves_extension_chance,
                ..
            } => {
                let pos = a.pos.offset(0, offset, 0);
                let radius = leaf_radius + a.radius_offset_xz - 1;
                let height = foliage_height + a.foliage_height_offset;
                self.place_leaves_row(w, pos, radius - 2, height - 3, double_trunk);
                self.place_leaves_row(w, pos, radius - 1, height - 4, double_trunk);
                let mut y = height - 5;
                while y >= 0 {
                    self.place_leaves_row(w, pos, radius, y, double_trunk);
                    y -= 1;
                }
                self.place_leaves_row_with_hanging_leaves_below(
                    w,
                    pos,
                    radius,
                    -1,
                    double_trunk,
                    *hanging_leaves_chance,
                    *hanging_leaves_extension_chance,
                );
                self.place_leaves_row_with_hanging_leaves_below(
                    w,
                    pos,
                    radius - 1,
                    -2,
                    double_trunk,
                    *hanging_leaves_chance,
                    *hanging_leaves_extension_chance,
                );
            }
            FoliageKind::Poplar {
                side_hole_chance, ..
            } => {
                let poplar = Poplar {
                    side_hole_chance: *side_hole_chance,
                    flip: false,
                    foliage_height: 0,
                };
                poplar.create(w, a, foliage_height, leaf_radius, offset);
            }
        }
    }

    /// `shouldSkipLocation(random, dx, y, dz, currentRadius, doubleTrunk)` (absolute dx, dz).
    fn should_skip_location(
        &self,
        random: &mut DecorationRandom,
        dx: i32,
        y: i32,
        dz: i32,
        radius: i32,
        double_trunk: bool,
    ) -> bool {
        match &self.kind {
            FoliageKind::Blob { .. } => {
                dx == radius && dz == radius && (random.next_int_bounded(2) == 0 || y == 0)
            }
            FoliageKind::Bush { .. } => {
                dx == radius && dz == radius && random.next_int_bounded(2) == 0
            }
            FoliageKind::Fancy { .. } => {
                let x = dx as f32 + 0.5;
                let z = dz as f32 + 0.5;
                x * x + z * z > (radius * radius) as f32
            }
            FoliageKind::Spruce { .. } | FoliageKind::Pine { .. } => {
                dx == radius && dz == radius && radius > 0
            }
            FoliageKind::Acacia => {
                if y == 0 {
                    (dx > 1 || dz > 1) && dx != 0 && dz != 0
                } else {
                    dx == radius && dz == radius && radius > 0
                }
            }
            FoliageKind::MegaJungle { .. } | FoliageKind::MegaPine { .. } => {
                dx + dz >= 7 || dx * dx + dz * dz > radius * radius
            }
            FoliageKind::DarkOak => {
                if y == -1 && !double_trunk {
                    dx == radius && dz == radius
                } else {
                    y == 1 && dx + dz > radius * 2 - 2
                }
            }
            FoliageKind::RandomSpread { .. } | FoliageKind::Poplar { .. } => false,
            FoliageKind::Cherry {
                wide_bottom_layer_hole_chance,
                corner_hole_chance,
                ..
            } => {
                if y == -1
                    && (dx == radius || dz == radius)
                    && random.next_float() < *wide_bottom_layer_hole_chance
                {
                    return true;
                }
                let corner = dx == radius && dz == radius;
                if radius > 2 {
                    corner || dx + dz > radius * 2 - 2 && random.next_float() < *corner_hole_chance
                } else {
                    corner && random.next_float() < *corner_hole_chance
                }
            }
        }
    }

    /// `shouldSkipLocationSigned(random, dx, y, dz, currentRadius, doubleTrunk)`.
    fn should_skip_location_signed(
        &self,
        random: &mut DecorationRandom,
        dx: i32,
        y: i32,
        dz: i32,
        radius: i32,
        double_trunk: bool,
    ) -> bool {
        if matches!(self.kind, FoliageKind::DarkOak)
            && y == 0
            && double_trunk
            && (dx == -radius || dx >= radius)
            && (dz == -radius || dz >= radius)
        {
            return true;
        }
        let (min_dx, min_dz) = if double_trunk {
            (dx.abs().min((dx - 1).abs()), dz.abs().min((dz - 1).abs()))
        } else {
            (dx.abs(), dz.abs())
        };
        self.should_skip_location(random, min_dx, y, min_dz, radius, double_trunk)
    }

    /// `placeLeavesRow(level, foliageSetter, random, tree, origin, currentRadius, y,
    /// doubleTrunk)`.
    fn place_leaves_row(
        &self,
        w: &mut TreeWriter<'_, '_, '_>,
        origin: BlockPos,
        radius: i32,
        y: i32,
        double_trunk: bool,
    ) {
        let offset = i32::from(double_trunk);
        for dx in -radius..=radius + offset {
            for dz in -radius..=radius + offset {
                if !self.should_skip_location_signed(w.random(), dx, y, dz, radius, double_trunk) {
                    try_place_leaf(w, origin.offset(dx, y, dz));
                }
            }
        }
    }

    /// `placeLeavesRowWithHangingLeavesBelow`.
    #[allow(clippy::too_many_arguments)]
    fn place_leaves_row_with_hanging_leaves_below(
        &self,
        w: &mut TreeWriter<'_, '_, '_>,
        origin: BlockPos,
        radius: i32,
        y: i32,
        double_trunk: bool,
        hanging_leaves_chance: f32,
        hanging_leaves_extension_chance: f32,
    ) {
        self.place_leaves_row(w, origin, radius, y, double_trunk);
        let offset = i32::from(double_trunk);
        let log_pos = origin.offset(0, -1, 0);
        for along_edge in HORIZONTAL {
            let to_edge = clockwise(along_edge);
            let positive = matches!(to_edge, Direction::South | Direction::East);
            let offset_to_edge = if positive { radius + offset } else { radius };
            let mut pos = relative(
                relative(origin.offset(0, y - 1, 0), to_edge, offset_to_edge),
                along_edge,
                -radius,
            );
            let mut offset_along_edge = -radius;
            while offset_along_edge < radius + offset {
                let leaves_above = w.is_foliage_set(pos.offset(0, 1, 0));
                if leaves_above && try_place_extension(w, hanging_leaves_chance, log_pos, pos) {
                    try_place_extension(
                        w,
                        hanging_leaves_extension_chance,
                        log_pos,
                        pos.offset(0, -1, 0),
                    );
                }
                offset_along_edge += 1;
                pos = pos.relative(along_edge);
            }
        }
    }
}

/// `FoliagePlacer.tryPlaceExtension`.
fn try_place_extension(
    w: &mut TreeWriter<'_, '_, '_>,
    chance: f32,
    log_pos: BlockPos,
    pos: BlockPos,
) -> bool {
    if dist_manhattan(pos, log_pos) >= 7 {
        return false;
    }
    if w.random().next_float() > chance {
        return false;
    }
    try_place_leaf(w, pos)
}

/// `FoliagePlacer.tryPlaceLeaf`.
fn try_place_leaf(w: &mut TreeWriter<'_, '_, '_>, pos: BlockPos) -> bool {
    let current = w.level().block(pos);
    let persistent = current.value("persistent") == Some("true");
    if persistent || !w.valid_tree_pos(pos) {
        return false;
    }
    let mut state = w.foliage_state(pos);
    if has_property(state, "waterlogged") {
        state = try_set_bool(state, "waterlogged", is_water_source(w.level().fluid(pos)));
    }
    w.set_foliage(pos, state);
    true
}

/// The parts of `PoplarFoliagePlacer` that depend on one `createFoliage` call.
struct Poplar {
    side_hole_chance: f32,
    flip: bool,
    foliage_height: i32,
}

impl Poplar {
    /// `PoplarFoliagePlacer.createFoliage` (after the offset was sampled).
    fn create(
        mut self,
        w: &mut TreeWriter<'_, '_, '_>,
        a: &FoliageAttachment,
        foliage_height: i32,
        leaf_radius: i32,
        offset: i32,
    ) {
        let double_trunk = a.double_trunk();
        let pos = a.pos.offset(0, offset, 0);
        let radius = leaf_radius + a.radius_offset_xz - 1;
        self.flip = w.random().next_boolean();
        let height = foliage_height + a.foliage_height_offset;
        self.foliage_height = height;
        self.place_leaves_row(w, pos, radius - 2, height - 1, double_trunk);
        self.place_leaves_row(w, pos, radius - 1, height - 2, double_trunk);
        self.place_leaves_row(w, pos, radius - 1, height - 3, double_trunk);
        let mut y = height - 4;
        while y >= 1 {
            self.place_leaves_row(w, pos, radius, y, double_trunk);
            y -= 1;
        }
        self.replace_leaves_with_log(w, pos, radius, height - 4, double_trunk);
        self.place_leaves_row(w, pos, radius - 1, 0, double_trunk);
        self.place_leaves_row(w, pos, (radius - 2).clamp(1, 2), -1, double_trunk);
    }

    /// `shouldRowBePartialRhombusShape`.
    fn partial_rhombus_row(&self, y: i32) -> bool {
        self.foliage_height - 1 == y || self.foliage_height - 2 == y
    }

    /// `getCornerBlocksToCutForRhombusShape`.
    fn corner_blocks_to_cut(&self, dx: i32, dz: i32, radius: i32, partial: bool) -> i32 {
        let small_corner = if self.flip {
            dx > 0 && dz > 0 || dz < 0 && dx < 0
        } else {
            dx > 0 && dz < 0 || dz > 0 && dx < 0
        };
        if small_corner {
            radius - 1
        } else if partial {
            radius + 1
        } else {
            radius
        }
    }

    /// `PoplarFoliagePlacer.placeLeavesRow`.
    fn place_leaves_row(
        &self,
        w: &mut TreeWriter<'_, '_, '_>,
        origin: BlockPos,
        radius: i32,
        y: i32,
        double_trunk: bool,
    ) {
        let offset = i32::from(double_trunk);
        for dx in -radius..=radius + offset {
            for dz in -radius..=radius + offset {
                if !self.should_skip_location(w.random(), dx, y, dz, radius) {
                    try_place_leaf(w, origin.offset(dx, y, dz));
                }
            }
        }
    }

    /// `PoplarFoliagePlacer.shouldSkipLocation`.
    fn should_skip_location(
        &self,
        random: &mut DecorationRandom,
        dx: i32,
        y: i32,
        dz: i32,
        radius: i32,
    ) -> bool {
        let partial = self.partial_rhombus_row(y);
        let corner_cut = self.corner_blocks_to_cut(dx, dz, radius, partial);
        let (abs_dx, abs_dz) = (dx.abs(), dz.abs());
        let edge = abs_dx == radius || abs_dz == radius;
        if partial && edge {
            return true;
        }
        let side_removal = i32::from(random.next_float() <= self.side_hole_chance);
        !within_rhombus(radius, abs_dx, abs_dz, corner_cut, side_removal)
    }

    /// `PoplarFoliagePlacer.replaceLeavesWithLog`.
    fn replace_leaves_with_log(
        &self,
        w: &mut TreeWriter<'_, '_, '_>,
        origin: BlockPos,
        radius: i32,
        y: i32,
        double_trunk: bool,
    ) {
        let offset = i32::from(double_trunk);
        for dx in -radius..=radius + offset {
            for dz in -radius..=radius + offset {
                let (abs_dx, abs_dz) = (dx.abs(), dz.abs());
                let corner_cut =
                    self.corner_blocks_to_cut(dx, dz, radius, self.partial_rhombus_row(y));
                if within_rhombus(radius, abs_dx, abs_dz, corner_cut, 2)
                    && (abs_dz == 0 && radius - abs_dx >= 4 || abs_dx == 0 && radius - abs_dz >= 4)
                {
                    let pos = origin.offset(dx, y, dz);
                    let axis = if abs_dz == 0 { Axis::X } else { Axis::Z };
                    // `tryPlaceLog`: the foliage provider is sampled for the comparison.
                    let current = w.level().block(pos);
                    if current == w.foliage_state(pos) {
                        let log = with_axis(w.trunk_state(pos), axis);
                        w.set_foliage(pos, log);
                    }
                }
            }
        }
    }
}

/// `PoplarFoliagePlacer.isWithinRhombusShape`.
fn within_rhombus(
    radius: i32,
    abs_dx: i32,
    abs_dz: i32,
    corner_cut: i32,
    side_removal: i32,
) -> bool {
    abs_dx + abs_dz <= radius * 2 - (corner_cut + side_removal)
}

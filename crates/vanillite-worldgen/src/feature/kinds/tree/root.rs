//! Root placers (`rootplacers/`): Vanilla has one, the mangrove root placer.

use serde_json::Value;
use vanillite_base::random::{DecorationRandom, RandomSource};
use vanillite_base::{BlockPos, Direction};
use vanillite_data::block::BlockState;

use super::{HORIZONTAL, TreeWriter, dist_manhattan, has_property, is_water, try_set_bool};
use crate::Error;
use crate::blocks::BlockSet;
use crate::carver::providers::IntProvider;
use crate::feature::region::WorldGenRegion;
use crate::feature::{BlockStateProvider, FeatureParser, field, float_field, int_field};

/// `AboveRootPlacement`.
#[derive(Clone, Debug)]
struct AboveRootPlacement {
    above_root_provider: BlockStateProvider,
    above_root_placement_chance: f32,
}

/// `MangroveRootPlacement`.
#[derive(Clone, Debug)]
struct MangroveRootPlacement {
    can_grow_through: BlockSet,
    muddy_roots_in: BlockSet,
    muddy_roots_provider: BlockStateProvider,
    max_root_width: i32,
    max_root_length: i32,
    random_skew_chance: f32,
}

/// `RootPlacer` (only `MangroveRootPlacer` exists).
#[derive(Clone, Debug)]
pub struct RootPlacer {
    trunk_offset_y: IntProvider,
    root_provider: BlockStateProvider,
    above_root_placement: Option<AboveRootPlacement>,
    mangrove: MangroveRootPlacement,
}

fn int_in(json: &Value, name: &str, min: i32, max: i32) -> Result<i32, Error> {
    let value = int_field(json, name)?;
    if (min..=max).contains(&value) {
        Ok(value)
    } else {
        Err(Error::new(format!("{name} {value} outside [{min}, {max}]")))
    }
}

fn chance(json: &Value, name: &str) -> Result<f32, Error> {
    let value = float_field(json, name)?;
    if (0.0..=1.0).contains(&value) {
        Ok(value)
    } else {
        Err(Error::new(format!("{name} {value} outside [0, 1]")))
    }
}

/// Up to two candidate root positions (`potentialRootPositions`).
type Candidates = ([BlockPos; 2], usize);

impl RootPlacer {
    /// Parses `RootPlacer.CODEC`.
    pub fn parse(json: &Value, parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        let kind = json
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::new("root placer without type"))?;
        if kind.strip_prefix("minecraft:").unwrap_or(kind) != "mangrove_root_placer" {
            return Err(Error::new(format!("unknown root placer type {kind}")));
        }
        let above_root_placement = match json.get("above_root_placement") {
            Some(above) => Some(AboveRootPlacement {
                above_root_provider: parser.state_provider(field(above, "above_root_provider")?)?,
                above_root_placement_chance: chance(above, "above_root_placement_chance")?,
            }),
            None => None,
        };
        let placement = field(json, "mangrove_root_placement")?;
        let mangrove = MangroveRootPlacement {
            can_grow_through: parser.block_set(field(placement, "can_grow_through")?)?,
            muddy_roots_in: parser.block_set(field(placement, "muddy_roots_in")?)?,
            muddy_roots_provider: parser
                .state_provider(field(placement, "muddy_roots_provider")?)?,
            max_root_width: int_in(placement, "max_root_width", 1, 12)?,
            max_root_length: int_in(placement, "max_root_length", 1, 64)?,
            random_skew_chance: chance(placement, "random_skew_chance")?,
        };
        Ok(Self {
            trunk_offset_y: IntProvider::from_json(field(json, "trunk_offset_y")?)?,
            root_provider: parser.state_provider(field(json, "root_provider")?)?,
            above_root_placement,
            mangrove,
        })
    }

    /// `getTrunkOrigin(origin, random)`.
    pub fn trunk_origin(&self, origin: BlockPos, random: &mut DecorationRandom) -> BlockPos {
        origin.offset(0, self.trunk_offset_y.sample(random), 0)
    }

    /// `MangroveRootPlacer.placeRoots`.
    pub fn place_roots(
        &self,
        w: &mut TreeWriter<'_, '_, '_>,
        origin: BlockPos,
        trunk_origin: BlockPos,
    ) -> bool {
        let mut column = origin;
        while column.y < trunk_origin.y {
            if !self.can_place_root(w, column) {
                return false;
            }
            column = column.offset(0, 1, 0);
        }
        let mut positions = vec![trunk_origin.offset(0, -1, 0)];
        let mut in_direction = Vec::new();
        for dir in HORIZONTAL {
            let pos = trunk_origin.relative(dir);
            in_direction.clear();
            if !self.simulate_roots(w, pos, dir, trunk_origin, &mut in_direction, 0) {
                return false;
            }
            positions.extend_from_slice(&in_direction);
            positions.push(trunk_origin.relative(dir));
        }
        for pos in positions {
            self.place_root(w, pos);
        }
        true
    }

    /// `MangroveRootPlacer.simulateRoots`.
    fn simulate_roots(
        &self,
        w: &mut TreeWriter<'_, '_, '_>,
        root_pos: BlockPos,
        dir: Direction,
        root_origin: BlockPos,
        positions: &mut Vec<BlockPos>,
        layer: i32,
    ) -> bool {
        let max_length = self.mangrove.max_root_length;
        if layer == max_length || positions.len() > max_length as usize {
            return false;
        }
        let (candidates, count) =
            self.potential_root_positions(root_pos, dir, w.random(), root_origin);
        for &pos in &candidates[..count] {
            if self.can_place_root(w, pos) {
                positions.push(pos);
                if !self.simulate_roots(w, pos, dir, root_origin, positions, layer + 1) {
                    return false;
                }
            }
        }
        true
    }

    /// `MangroveRootPlacer.potentialRootPositions`.
    fn potential_root_positions(
        &self,
        pos: BlockPos,
        prev_dir: Direction,
        random: &mut DecorationRandom,
        root_origin: BlockPos,
    ) -> Candidates {
        let below = pos.offset(0, -1, 0);
        let next_to = pos.relative(prev_dir);
        let width = dist_manhattan(pos, root_origin);
        let max_width = self.mangrove.max_root_width;
        let skew = self.mangrove.random_skew_chance;
        if width > max_width - 3 && width <= max_width {
            if random.next_float() < skew {
                ([below, next_to.offset(0, -1, 0)], 2)
            } else {
                ([below, below], 1)
            }
        } else if width > max_width || random.next_float() < skew {
            ([below, below], 1)
        } else if random.next_boolean() {
            ([next_to, next_to], 1)
        } else {
            ([below, below], 1)
        }
    }

    /// `MangroveRootPlacer.canPlaceRoot`.
    fn can_place_root(&self, w: &TreeWriter<'_, '_, '_>, pos: BlockPos) -> bool {
        w.valid_tree_pos(pos)
            || self
                .mangrove
                .can_grow_through
                .contains(w.level().block(pos))
    }

    /// `MangroveRootPlacer.placeRoot` (falling back to `RootPlacer.placeRoot`).
    fn place_root(&self, w: &mut TreeWriter<'_, '_, '_>, pos: BlockPos) {
        if self.mangrove.muddy_roots_in.contains(w.level().block(pos)) {
            let state =
                self.mangrove
                    .muddy_roots_provider
                    .get_state(w.ctx.level, w.ctx.random, pos);
            let state = waterlogged(w.level(), pos, state);
            w.set_root(pos, state);
        } else if self.can_place_root(w, pos) {
            let state = self.root_provider.get_state(w.ctx.level, w.ctx.random, pos);
            let state = waterlogged(w.level(), pos, state);
            w.set_root(pos, state);
            if let Some(above) = &self.above_root_placement {
                let above_pos = pos.offset(0, 1, 0);
                if w.random().next_float() < above.above_root_placement_chance
                    && w.level().block(above_pos).is_air()
                {
                    let state =
                        above
                            .above_root_provider
                            .get_state(w.ctx.level, w.ctx.random, above_pos);
                    let state = waterlogged(w.level(), above_pos, state);
                    w.set_root(above_pos, state);
                }
            }
        }
    }
}

/// `RootPlacer.getPotentiallyWaterloggedState`.
fn waterlogged(level: &WorldGenRegion<'_>, pos: BlockPos, state: BlockState) -> BlockState {
    if has_property(state, "waterlogged") {
        try_set_bool(state, "waterlogged", is_water(level.fluid(pos)))
    } else {
        state
    }
}

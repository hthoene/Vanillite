//! `SculkPatchFeature` with the parts of `SculkSpreader`, `SculkBehaviour`, `SculkBlock`,
//! `SculkVeinBlock`, `MultifaceBlock` and `MultifaceSpreader` that world generation runs.
//!
//! Sounds, particles (`levelEvent`) and entity pushing have no effect during generation and are
//! left out. The world generation spreader never merges cursors, so the cursor bookkeeping of
//! `updateCursors` reduces to dropping discharged and far away cursors.

use serde_json::Value;
use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Direction};
use vanillite_data::block::{BlockState, flags as state_flags, ids};

use super::{
    axis, direction_name, is, is_face_full, is_source, is_still_water, shuffle, shuffled_directions,
};
use crate::Error;
use crate::behaviour::{SupportType, can_be_replaced, is_face_sturdy};
use crate::blocks::BlockSet;
use crate::feature::region::{WorldGenRegion, flags};
use crate::feature::{FeatureContext, FeatureParser, FeatureType, int_field};
use crate::mth::{floor_f, min_f, square_f};

/// `SculkSpreader.createWorldGenSpreader()`: growth spawn cost.
const GROWTH_SPAWN_COST: i32 = 50;
/// No growth radius.
const NO_GROWTH_RADIUS: i32 = 1;
/// Charge decay rate.
const CHARGE_DECAY_RATE: i32 = 5;
/// Additional decay rate.
const ADDITIONAL_DECAY_RATE: i32 = 10;
/// `MAX_CURSORS`.
const MAX_CURSORS: usize = 32;
/// `MAX_CHARGE`.
const MAX_CHARGE: i32 = 1000;

/// The block tags the spreader reads.
#[derive(Debug)]
struct Tags {
    /// `#sculk_replaceable_world_gen` (the world generation spreader's replaceable blocks).
    replaceable_world_gen: BlockSet,
    /// `#sculk_replaceable` (`SculkVeinBlock.hasSubstrateAccess`).
    replaceable: BlockSet,
    /// `#sculk_growth_inhibitors`.
    growth_inhibitors: BlockSet,
    /// `#fire`.
    fire: BlockSet,
}

/// `sculk_patch`.
#[derive(Debug)]
pub struct SculkPatchFeature {
    charge_count: i32,
    amount_per_charge: i32,
    spread_attempts: i32,
    growth_rounds: i32,
    spread_rounds: i32,
    tags: Tags,
}

/// An int field checked against a range.
fn int_in(json: &Value, name: &str, min: i32, max: i32) -> Result<i32, Error> {
    let value = int_field(json, name)?;
    if !(min..=max).contains(&value) {
        return Err(Error::new(format!(
            "{name} {value} outside of [{min}, {max}]"
        )));
    }
    Ok(value)
}

impl FeatureType for SculkPatchFeature {
    fn parse(json: &Value, parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        Ok(Self {
            charge_count: int_in(json, "charge_count", 1, 32)?,
            amount_per_charge: int_in(json, "amount_per_charge", 1, 500)?,
            spread_attempts: int_in(json, "spread_attempts", 1, 64)?,
            growth_rounds: int_in(json, "growth_rounds", 0, 8)?,
            spread_rounds: int_in(json, "spread_rounds", 0, 8)?,
            tags: Tags {
                replaceable_world_gen: parser.block_tag("minecraft:sculk_replaceable_world_gen"),
                replaceable: parser.block_tag("minecraft:sculk_replaceable"),
                growth_inhibitors: parser.block_tag("minecraft:sculk_growth_inhibitors"),
                fire: parser.block_tag("minecraft:fire"),
            },
        })
    }

    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        let level = &mut *ctx.level;
        let random = &mut *ctx.random;
        if !patch_can_spread_from(level, origin) {
            return false;
        }
        let spreader = Spreader { tags: &self.tags };
        let mut cursors: Vec<ChargeCursor> = Vec::with_capacity(MAX_CURSORS);
        for round in 0..self.spread_rounds + self.growth_rounds {
            for _ in 0..self.charge_count {
                add_cursors(&mut cursors, origin, self.amount_per_charge);
            }
            let spread_veins = round < self.spread_rounds;
            for _ in 0..self.spread_attempts {
                spreader.update_cursors(&mut cursors, level, origin, random, spread_veins);
            }
            cursors.clear();
        }
        true
    }
}

/// `SculkPatchFeature.canSpreadFrom`.
fn patch_can_spread_from(level: &WorldGenRegion<'_>, origin: BlockPos) -> bool {
    let start = level.block(origin);
    if Behaviour::of(start).is_sculk() {
        return true;
    }
    if !start.is_air() && (!is(start, ids::WATER) || !is_source(start.fluid_state())) {
        return false;
    }
    Direction::ALL.iter().any(|&d| {
        level
            .block(origin.relative(d))
            .has(state_flags::COLLISION_FULL_BLOCK)
    })
}

/// `SculkSpreader.addCursors`.
fn add_cursors(cursors: &mut Vec<ChargeCursor>, pos: BlockPos, charge: i32) {
    let mut charge = charge;
    while charge > 0 {
        let current = charge.min(MAX_CHARGE);
        if cursors.len() < MAX_CURSORS {
            cursors.push(ChargeCursor::new(pos, current));
        }
        charge -= current;
    }
}

/// `SculkSpreader.ChargeCursor`.
#[derive(Clone, Copy, Debug)]
struct ChargeCursor {
    pos: BlockPos,
    charge: i32,
    update_delay: i32,
    decay_delay: i32,
    /// `facings` as a bit set by `Direction` ordinal; `None` = `null`.
    facings: Option<u8>,
}

impl ChargeCursor {
    fn new(pos: BlockPos, charge: i32) -> Self {
        Self {
            pos,
            charge,
            update_delay: 0,
            decay_delay: 1,
            facings: None,
        }
    }
}

/// `SculkSpreader` as `createWorldGenSpreader` configures it.
struct Spreader<'t> {
    tags: &'t Tags,
}

/// The `SculkBehaviour` of a block: `SculkBlock`, `SculkVeinBlock` or `SculkBehaviour.DEFAULT`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Behaviour {
    Default,
    Sculk,
    Vein,
}

impl Behaviour {
    /// `ChargeCursor.getBlockBehaviour(state)`.
    fn of(state: BlockState) -> Self {
        if is(state, ids::SCULK) {
            Self::Sculk
        } else if is(state, ids::SCULK_VEIN) {
            Self::Vein
        } else {
            Self::Default
        }
    }

    /// `state.getBlock() instanceof SculkBehaviour`.
    fn is_sculk(self) -> bool {
        self != Self::Default
    }

    /// `canChangeBlockStateOnSpread()`.
    fn can_change_block_state_on_spread(self) -> bool {
        self != Self::Sculk
    }

    /// `updateDecayDelay(age)`.
    fn update_decay_delay(self, age: i32) -> i32 {
        match self {
            Self::Default => (age - 1).max(0),
            Self::Sculk | Self::Vein => 1,
        }
    }
}

/// Which spreader of `SculkVeinBlock`: the full one or the same space one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VeinSpreader {
    /// `getSpreader()`: `SAME_POSITION`, `SAME_PLANE`, `WRAP_AROUND`.
    Full,
    /// `getSameSpaceSpreader()`: `SAME_POSITION` only.
    SameSpace,
}

/// `MultifaceSpreader.SpreadPos`.
#[derive(Clone, Copy, Debug)]
struct SpreadPos {
    pos: BlockPos,
    face: Direction,
}

/// `ChargeCursor.NON_CORNER_NEIGHBOURS` in `betweenClosed` order.
const NON_CORNER_NEIGHBOURS: [[i32; 3]; 18] = {
    let mut out = [[0; 3]; 18];
    let mut n = 0;
    let mut z = -1;
    while z <= 1 {
        let mut y = -1;
        while y <= 1 {
            let mut x = -1;
            while x <= 1 {
                if (x == 0 || y == 0 || z == 0) && !(x == 0 && y == 0 && z == 0) {
                    out[n] = [x, y, z];
                    n += 1;
                }
                x += 1;
            }
            y += 1;
        }
        z += 1;
    }
    out
};

impl Spreader<'_> {
    /// `updateCursors(level, originPos, random, spreadVeins)`.
    fn update_cursors<R: RandomSource>(
        &self,
        cursors: &mut Vec<ChargeCursor>,
        level: &mut WorldGenRegion<'_>,
        origin: BlockPos,
        random: &mut R,
        spread_veins: bool,
    ) {
        cursors.retain_mut(|cursor| {
            if is_pos_unreasonable(cursor.pos, origin) {
                return false;
            }
            self.update(cursor, level, origin, random, spread_veins);
            cursor.charge > 0
        });
    }

    /// `ChargeCursor.update`.
    fn update<R: RandomSource>(
        &self,
        cursor: &mut ChargeCursor,
        level: &mut WorldGenRegion<'_>,
        origin: BlockPos,
        random: &mut R,
        spread_veins: bool,
    ) {
        // `shouldUpdate`: always for world generation while charged.
        if cursor.charge <= 0 {
            return;
        }
        if cursor.update_delay > 0 {
            cursor.update_delay -= 1;
            return;
        }
        let mut current = level.block(cursor.pos);
        let mut behaviour = Behaviour::of(current);
        if spread_veins
            && self.attempt_spread_vein(behaviour, level, cursor.pos, current, cursor.facings)
            && behaviour.can_change_block_state_on_spread()
        {
            current = level.block(cursor.pos);
            behaviour = Behaviour::of(current);
        }
        cursor.charge =
            self.attempt_use_charge(behaviour, cursor, level, origin, random, spread_veins);
        if cursor.charge <= 0 {
            self.on_discharged(behaviour, level, current, cursor.pos);
            return;
        }
        match self.valid_movement_pos(level, cursor.pos, random, origin) {
            Some(transfer) => {
                self.on_discharged(behaviour, level, current, cursor.pos);
                cursor.pos = transfer;
                current = level.block(transfer);
            }
            None => {
                self.on_discharged(behaviour, level, current, cursor.pos);
                cursor.charge = 0;
                return;
            }
        }
        if Behaviour::of(current).is_sculk() {
            cursor.facings = Some(available_faces(current));
        }
        cursor.decay_delay = behaviour.update_decay_delay(cursor.decay_delay);
        // `getSculkSpreadDelay()`
        cursor.update_delay = 1;
    }

    /// `SculkBehaviour.attemptSpreadVein`.
    fn attempt_spread_vein(
        &self,
        behaviour: Behaviour,
        level: &mut WorldGenRegion<'_>,
        pos: BlockPos,
        state: BlockState,
        facings: Option<u8>,
    ) -> bool {
        if behaviour == Behaviour::Default {
            match facings {
                None => {
                    let current = level.block(pos);
                    return self.spread_all(VeinSpreader::SameSpace, current, level, pos, true) > 0;
                }
                Some(faces) if faces != 0 => {
                    if !state.is_air() && !is_still_water(state.fluid_state()) {
                        return false;
                    }
                    return regrow(level, pos, state, faces);
                }
                Some(_) => {}
            }
        }
        self.spread_all(VeinSpreader::Full, state, level, pos, true) > 0
    }

    /// `SculkBehaviour.attemptUseCharge`.
    fn attempt_use_charge<R: RandomSource>(
        &self,
        behaviour: Behaviour,
        cursor: &ChargeCursor,
        level: &mut WorldGenRegion<'_>,
        origin: BlockPos,
        random: &mut R,
        spread_veins: bool,
    ) -> i32 {
        match behaviour {
            Behaviour::Default => {
                if cursor.decay_delay > 0 {
                    cursor.charge
                } else {
                    0
                }
            }
            Behaviour::Sculk => self.sculk_use_charge(cursor, level, origin, random),
            Behaviour::Vein => {
                if spread_veins && self.attempt_place_sculk(level, cursor.pos, random) {
                    cursor.charge - 1
                } else if random.next_int_bounded(CHARGE_DECAY_RATE) == 0 {
                    floor_f(cursor.charge as f32 * 0.5)
                } else {
                    cursor.charge
                }
            }
        }
    }

    /// `SculkBlock.attemptUseCharge`.
    fn sculk_use_charge<R: RandomSource>(
        &self,
        cursor: &ChargeCursor,
        level: &mut WorldGenRegion<'_>,
        origin: BlockPos,
        random: &mut R,
    ) -> i32 {
        let charge = cursor.charge;
        if charge == 0 || random.next_int_bounded(CHARGE_DECAY_RATE) != 0 {
            return charge;
        }
        let pos = cursor.pos;
        let close_to_catalyst =
            pos.dist_sqr(origin) < f64::from(NO_GROWTH_RADIUS * NO_GROWTH_RADIUS);
        if !close_to_catalyst && self.can_place_growth(level, pos) {
            if random.next_int_bounded(GROWTH_SPAWN_COST) < charge {
                let growth_pos = pos.relative(Direction::Up);
                let state = random_growth_state(level, growth_pos, random);
                level.set_block_and_update(growth_pos, state);
            }
            return 0.max(charge - GROWTH_SPAWN_COST);
        }
        if random.next_int_bounded(ADDITIONAL_DECAY_RATE) != 0 {
            charge
        } else if close_to_catalyst {
            charge - 1
        } else {
            charge - decay_penalty(pos, origin, charge)
        }
    }

    /// `SculkBlock.canPlaceGrowth`.
    fn can_place_growth(&self, level: &WorldGenRegion<'_>, pos: BlockPos) -> bool {
        let above = level.block(pos.relative(Direction::Up));
        if !(above.is_air() || is(above, ids::WATER) && is_still_water(above.fluid_state())) {
            return false;
        }
        // `findBlocksIn(pos - (4, 0, 4), pos + (4, 2, 4)).filterState(inhibitor).atMostMatched(2)`
        let mut matched = 0;
        for y in pos.y..=pos.y + 2 {
            if level.is_outside_build_height(y) {
                continue;
            }
            for z in pos.z - 4..=pos.z + 4 {
                for x in pos.x - 4..=pos.x + 4 {
                    if self
                        .tags
                        .growth_inhibitors
                        .contains(level.block(BlockPos::new(x, y, z)))
                    {
                        matched += 1;
                        if matched > 2 {
                            return false;
                        }
                    }
                }
            }
        }
        true
    }

    /// `SculkVeinBlock.attemptPlaceSculk`.
    fn attempt_place_sculk<R: RandomSource>(
        &self,
        level: &mut WorldGenRegion<'_>,
        pos: BlockPos,
        random: &mut R,
    ) -> bool {
        let state = level.block(pos);
        for support in shuffled_directions(random) {
            if !has_face(state, support) {
                continue;
            }
            let support_pos = pos.relative(support);
            if !self
                .tags
                .replaceable_world_gen
                .contains(level.block(support_pos))
            {
                continue;
            }
            let sculk = ids::SCULK.default_state();
            level.set_block_and_update(support_pos, sculk);
            self.spread_all(VeinSpreader::Full, sculk, level, support_pos, true);
            let skip = support.opposite();
            for direction in Direction::ALL {
                if direction == skip {
                    continue;
                }
                let vein_pos = support_pos.relative(direction);
                let vein = level.block(vein_pos);
                if is(vein, ids::SCULK_VEIN) {
                    vein_on_discharged(level, vein, vein_pos);
                }
            }
            return true;
        }
        false
    }

    /// `SculkBehaviour.onDischarged`: only veins react.
    fn on_discharged(
        &self,
        behaviour: Behaviour,
        level: &mut WorldGenRegion<'_>,
        state: BlockState,
        pos: BlockPos,
    ) {
        if behaviour == Behaviour::Vein {
            vein_on_discharged(level, state, pos);
        }
    }

    /// `ChargeCursor.getValidMovementPos`.
    fn valid_movement_pos<R: RandomSource>(
        &self,
        level: &WorldGenRegion<'_>,
        pos: BlockPos,
        random: &mut R,
        origin: BlockPos,
    ) -> Option<BlockPos> {
        let mut offsets = NON_CORNER_NEIGHBOURS;
        shuffle(&mut offsets, random);
        let mut sculk_position = pos;
        for [dx, dy, dz] in offsets {
            let neighbour = pos.offset(dx, dy, dz);
            // `canMoveToPos` for world generation
            let (ox, oz) = (origin.x - neighbour.x, origin.z - neighbour.z);
            if ox * ox + oz * oz > 144 {
                continue;
            }
            let transferee = level.block(neighbour);
            if Behaviour::of(transferee).is_sculk()
                && is_movement_unobstructed(level, pos, neighbour)
            {
                sculk_position = neighbour;
                if self.has_substrate_access(level, transferee, neighbour) {
                    break;
                }
            }
        }
        (sculk_position != pos).then_some(sculk_position)
    }

    /// `SculkVeinBlock.hasSubstrateAccess`.
    fn has_substrate_access(
        &self,
        level: &WorldGenRegion<'_>,
        state: BlockState,
        pos: BlockPos,
    ) -> bool {
        is(state, ids::SCULK_VEIN)
            && Direction::ALL.iter().any(|&d| {
                has_face(state, d) && self.tags.replaceable.contains(level.block(pos.relative(d)))
            })
    }

    /// `MultifaceSpreader.spreadAll(state, level, pos, postProcess)` of a sculk vein spreader.
    fn spread_all(
        &self,
        spreader: VeinSpreader,
        state: BlockState,
        level: &mut WorldGenRegion<'_>,
        pos: BlockPos,
        post_process: bool,
    ) -> i64 {
        let mut count = 0;
        for face in Direction::ALL {
            if !can_spread_from(state, face) {
                continue;
            }
            for spread_direction in Direction::ALL {
                let spread =
                    self.spread_pos_toward(spreader, state, level, pos, face, spread_direction);
                if let Some(spread) = spread
                    && place_vein(level, spread, post_process)
                {
                    count += 1;
                }
            }
        }
        count
    }

    /// `getSpreadFromFaceTowardDirection` with the config's `canSpreadInto`.
    fn spread_pos_toward(
        &self,
        spreader: VeinSpreader,
        state: BlockState,
        level: &WorldGenRegion<'_>,
        pos: BlockPos,
        starting_face: Direction,
        spread_direction: Direction,
    ) -> Option<SpreadPos> {
        if axis(spread_direction) == axis(starting_face) {
            return None;
        }
        if !(is_other_block_valid_as_source(state)
            || has_face(state, starting_face) && !has_face(state, spread_direction))
        {
            return None;
        }
        let same_position = SpreadPos {
            pos,
            face: spread_direction,
        };
        if self.can_spread_into(level, pos, same_position) {
            return Some(same_position);
        }
        if spreader == VeinSpreader::SameSpace {
            return None;
        }
        let same_plane = SpreadPos {
            pos: pos.relative(spread_direction),
            face: starting_face,
        };
        if self.can_spread_into(level, pos, same_plane) {
            return Some(same_plane);
        }
        let wrap_around = SpreadPos {
            pos: pos.relative(spread_direction).relative(starting_face),
            face: spread_direction.opposite(),
        };
        self.can_spread_into(level, pos, wrap_around)
            .then_some(wrap_around)
    }

    /// `DefaultSpreaderConfig.canSpreadInto`.
    fn can_spread_into(
        &self,
        level: &WorldGenRegion<'_>,
        source: BlockPos,
        spread: SpreadPos,
    ) -> bool {
        let existing = level.block(spread.pos);
        self.state_can_be_replaced(level, source, spread.pos, spread.face, existing)
            && is_valid_state_for_placement(level, existing, spread.pos, spread.face)
    }

    /// `SculkVeinSpreaderConfig.stateCanBeReplaced`.
    fn state_can_be_replaced(
        &self,
        level: &WorldGenRegion<'_>,
        source: BlockPos,
        placement: BlockPos,
        direction: Direction,
        existing: BlockState,
    ) -> bool {
        let against = level.block(placement.relative(direction));
        if is(against, ids::SCULK)
            || is(against, ids::SCULK_CATALYST)
            || is(against, ids::MOVING_PISTON)
        {
            return false;
        }
        let manhattan = (source.x - placement.x).abs()
            + (source.y - placement.y).abs()
            + (source.z - placement.z).abs();
        if manhattan == 2 {
            let neighbour = source.relative(direction.opposite());
            if is_face_sturdy(level.block(neighbour), direction, SupportType::Full) {
                return false;
            }
        }
        let fluid = existing.fluid_state();
        if !fluid.is_empty() && !is_still_water(fluid) {
            return false;
        }
        if self.tags.fire.contains(existing) {
            return false;
        }
        can_be_replaced(existing)
            || existing.is_air()
            || is(existing, ids::SCULK_VEIN)
            || is(existing, ids::WATER) && is_source(fluid)
    }
}

/// `ChargeCursor.isPosUnreasonable`: farther than 1024 blocks (chessboard) from the origin.
fn is_pos_unreasonable(pos: BlockPos, origin: BlockPos) -> bool {
    let distance = (pos.x - origin.x)
        .abs()
        .max((pos.y - origin.y).abs())
        .max((pos.z - origin.z).abs());
    distance > 1024
}

/// `SculkBlock.getDecayPenalty`.
fn decay_penalty(pos: BlockPos, origin: BlockPos, charge: i32) -> i32 {
    let outer_distance_squared =
        square_f(pos.dist_sqr(origin).sqrt() as f32 - NO_GROWTH_RADIUS as f32);
    let max_reach_squared = (24 - NO_GROWTH_RADIUS) * (24 - NO_GROWTH_RADIUS);
    let distance_factor = min_f(1.0, outer_distance_squared / max_reach_squared as f32);
    1.max((charge as f32 * distance_factor * 0.5) as i32)
}

/// `SculkBlock.getRandomGrowthState` for world generation.
fn random_growth_state<R: RandomSource>(
    level: &WorldGenRegion<'_>,
    pos: BlockPos,
    random: &mut R,
) -> BlockState {
    let state = if random.next_int_bounded(11) == 0 {
        let shrieker = ids::SCULK_SHRIEKER.default_state();
        shrieker.with("can_summon", "true").unwrap_or(shrieker)
    } else {
        ids::SCULK_SENSOR.default_state()
    };
    if !level.fluid(pos).is_empty()
        && let Some(waterlogged) = state.with("waterlogged", "true")
    {
        return waterlogged;
    }
    state
}

/// `ChargeCursor.isMovementUnobstructed`.
fn is_movement_unobstructed(level: &WorldGenRegion<'_>, from: BlockPos, to: BlockPos) -> bool {
    let (dx, dy, dz) = (to.x - from.x, to.y - from.y, to.z - from.z);
    if dx.abs() + dy.abs() + dz.abs() == 1 {
        return true;
    }
    let direction_x = if dx < 0 {
        Direction::West
    } else {
        Direction::East
    };
    let direction_y = if dy < 0 {
        Direction::Down
    } else {
        Direction::Up
    };
    let direction_z = if dz < 0 {
        Direction::North
    } else {
        Direction::South
    };
    let unobstructed = |direction: Direction| {
        let test = from.relative(direction);
        !is_face_sturdy(level.block(test), direction.opposite(), SupportType::Full)
    };
    if dx == 0 {
        unobstructed(direction_y) || unobstructed(direction_z)
    } else if dy == 0 {
        unobstructed(direction_x) || unobstructed(direction_z)
    } else {
        unobstructed(direction_x) || unobstructed(direction_y)
    }
}

/// `MultifaceBlock.hasFace(state, face)`: the boolean face property (`PipeBlock` properties) is
/// `true`.
fn has_face(state: BlockState, face: Direction) -> bool {
    state.value(direction_name(face)) == Some("true")
}

/// `MultifaceBlock.hasAnyFace`.
fn has_any_face(state: BlockState) -> bool {
    Direction::ALL.iter().any(|&d| has_face(state, d))
}

/// `MultifaceBlock.availableFaces` of a sculk block: the vein's faces (none for others).
fn available_faces(state: BlockState) -> u8 {
    if !is(state, ids::SCULK_VEIN) {
        return 0;
    }
    Direction::ALL
        .iter()
        .filter(|&&d| has_face(state, d))
        .fold(0, |mask, &d| mask | 1 << d as u8)
}

/// `SculkVeinSpreaderConfig.isOtherBlockValidAsSource`.
fn is_other_block_valid_as_source(state: BlockState) -> bool {
    !is(state, ids::SCULK_VEIN)
}

/// `SpreadConfig.canSpreadFrom`.
fn can_spread_from(state: BlockState, face: Direction) -> bool {
    is_other_block_valid_as_source(state) || has_face(state, face)
}

/// `MultifaceBlock.canAttachTo(level, direction, neighbourPos, neighbourState)`: the neighbour's
/// support or collision shape has a full face towards the block.
fn can_attach_to(direction: Direction, neighbour: BlockState) -> bool {
    let face = direction.opposite();
    is_face_sturdy(neighbour, face, SupportType::Full)
        || is_face_full(neighbour.collision_shape(), face)
}

/// `MultifaceBlock.canAttachTo(level, pos, direction)`.
fn can_attach_to_at(level: &WorldGenRegion<'_>, pos: BlockPos, direction: Direction) -> bool {
    can_attach_to(direction, level.block(pos.relative(direction)))
}

/// `MultifaceBlock.isValidStateForPlacement` of the sculk vein (every face is supported).
fn is_valid_state_for_placement(
    level: &WorldGenRegion<'_>,
    old: BlockState,
    pos: BlockPos,
    direction: Direction,
) -> bool {
    if is(old, ids::SCULK_VEIN) && has_face(old, direction) {
        return false;
    }
    can_attach_to_at(level, pos, direction)
}

/// `MultifaceBlock.getStateForPlacement(oldState, level, pos, direction)` of the sculk vein.
fn vein_state_for_placement(
    level: &WorldGenRegion<'_>,
    old: BlockState,
    pos: BlockPos,
    direction: Direction,
) -> Option<BlockState> {
    if !is_valid_state_for_placement(level, old, pos, direction) {
        return None;
    }
    let fluid = old.fluid_state();
    let state = if is(old, ids::SCULK_VEIN) {
        old
    } else if is_source(fluid) && is_still_water(fluid) {
        let vein = ids::SCULK_VEIN.default_state();
        vein.with("waterlogged", "true").unwrap_or(vein)
    } else {
        ids::SCULK_VEIN.default_state()
    };
    state.with(direction_name(direction), "true")
}

/// `MultifaceSpreader.spreadToFace` with `SpreadConfig.placeBlock`.
fn place_vein(level: &mut WorldGenRegion<'_>, spread: SpreadPos, post_process: bool) -> bool {
    let old = level.block(spread.pos);
    let Some(state) = vein_state_for_placement(level, old, spread.pos, spread.face) else {
        return false;
    };
    if post_process {
        level.mark_post_processing(spread.pos);
    }
    level.set_block(spread.pos, state, flags::CLIENTS)
}

/// `SculkVeinBlock.regrow(level, pos, existing, faces)`.
fn regrow(level: &mut WorldGenRegion<'_>, pos: BlockPos, existing: BlockState, faces: u8) -> bool {
    let mut state = ids::SCULK_VEIN.default_state();
    let mut has_at_least_one_face = false;
    for face in Direction::ALL {
        if faces & (1 << face as u8) == 0 || !can_attach_to_at(level, pos, face) {
            continue;
        }
        if let Some(s) = state.with(direction_name(face), "true") {
            state = s;
            has_at_least_one_face = true;
        }
    }
    if !has_at_least_one_face {
        return false;
    }
    if !existing.fluid_state().is_empty() {
        state = state.with("waterlogged", "true").unwrap_or(state);
    }
    level.set_block_and_update(pos, state);
    true
}

/// `SculkVeinBlock.onDischarged`: drops faces resting on sculk; a vein without faces becomes
/// air or water.
fn vein_on_discharged(level: &mut WorldGenRegion<'_>, state: BlockState, pos: BlockPos) {
    if !is(state, ids::SCULK_VEIN) {
        return;
    }
    let mut state = state;
    for direction in Direction::ALL {
        if has_face(state, direction) && is(level.block(pos.relative(direction)), ids::SCULK) {
            state = state
                .with(direction_name(direction), "false")
                .unwrap_or(state);
        }
    }
    if !has_any_face(state) {
        state = if level.fluid(pos).is_empty() {
            BlockState::AIR
        } else {
            ids::WATER.default_state()
        };
    }
    level.set_block_and_update(pos, state);
}

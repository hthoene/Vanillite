//! The sculk catalyst (`SculkCatalystBlock`, `SculkCatalystBlockEntity`) and the level's sculk
//! spreading (`SculkSpreader.createLevelSpreader`, `SculkBehaviour`, `SculkBlock`,
//! `SculkVeinBlock` and their `MultifaceSpreader`).
//!
//! A mob dying near a catalyst gives its experience to the catalyst instead of dropping it:
//! the catalyst blooms and charge cursors start at the death position, spreading sculk and
//! veins and growing sensors and shriekers. The death reaches the catalyst through
//! [`on_mob_death`] (decided during the mob's death, applied right after its tick), not through
//! the generic game event dispatch.
//!
//! World generation has its own copy of the spreader (`sculk_patch` in vanillite-worldgen)
//! with the world generation configuration and no side effects.

use vanillite_abi::event::BLOCK_SPREAD;
use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Direction, Vec3};
use vanillite_data::block::{BlockId, BlockState, ids};
use vanillite_nbt::{Compound, List, Tag};
use vanillite_worldgen::behaviour::{StateExt, can_be_replaced, tags};

use super::{BlockBehaviour, is_face_sturdy, sound_id};
use crate::level::game_event::{containing, events};
use crate::level::{BlockEntity, Level, SOUND_BLOCKS, update};

/// `CatalystListener.getListenerRadius()`.
pub const LISTENER_RADIUS: i32 = 8;
/// `CatalystListener.PULSE_TICKS`.
const PULSE_TICKS: i32 = 8;
/// `createLevelSpreader()`: growth spawn cost.
const GROWTH_SPAWN_COST: i32 = 10;
/// No growth radius.
const NO_GROWTH_RADIUS: i32 = 4;
/// Charge decay rate.
const CHARGE_DECAY_RATE: i32 = 10;
/// Additional decay rate.
const ADDITIONAL_DECAY_RATE: i32 = 5;
/// `MAX_CURSORS`.
const MAX_CURSORS: usize = 32;
/// `MAX_CHARGE`.
const MAX_CHARGE: i32 = 1000;
/// `LevelEvent.PARTICLES_SCULK_CHARGE`.
const CHARGE_EVENT: i32 = 3006;

/// `SculkSpreader.ChargeCursor`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChargeCursor {
    pub pos: BlockPos,
    pub charge: i32,
    pub update_delay: i32,
    pub decay_delay: i32,
    /// `facings` as a bit set by `Direction` ordinal; `None` = `null`.
    pub facings: Option<u8>,
}

impl ChargeCursor {
    fn new(pos: BlockPos, charge: i32) -> ChargeCursor {
        ChargeCursor {
            pos,
            charge,
            update_delay: 0,
            decay_delay: 1,
            facings: None,
        }
    }
}

/// The data of a `SculkCatalystBlockEntity`: its spreader's cursors.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Catalyst {
    pub cursors: Vec<ChargeCursor>,
}

impl Catalyst {
    /// `SculkSpreader.addCursors(startPos, charge)`.
    pub fn add_cursors(&mut self, pos: BlockPos, charge: i32) {
        let mut charge = charge;
        while charge > 0 {
            let current = charge.min(MAX_CHARGE);
            if self.cursors.len() < MAX_CURSORS {
                self.cursors.push(ChargeCursor::new(pos, current));
            }
            charge -= current;
        }
    }

    /// `SculkSpreader.load`: at most 32 cursors.
    pub fn load(&mut self, nbt: &Compound) {
        self.cursors.clear();
        let Some(Tag::List(list)) = nbt.get("cursors") else {
            return;
        };
        for t in list.0.iter().take(MAX_CURSORS) {
            let Tag::Compound(c) = t else { continue };
            let Some(Tag::IntArray(p)) = c.get("pos") else {
                continue;
            };
            let [x, y, z] = p.as_slice() else { continue };
            let int = |k: &str, d: i32| match c.get(k) {
                Some(Tag::Int(v)) => *v,
                _ => d,
            };
            let facings = match c.get("facings") {
                Some(Tag::List(l)) => Some(l.0.iter().fold(0u8, |mask, t| match t {
                    Tag::String(name) => mask | direction_by_name(name).map_or(0, |d| 1 << d as u8),
                    _ => mask,
                })),
                _ => None,
            };
            self.cursors.push(ChargeCursor {
                pos: BlockPos::new(*x, *y, *z),
                charge: int("charge", 0).clamp(0, MAX_CHARGE),
                decay_delay: int("decay_delay", 1).clamp(0, 1),
                update_delay: int("update_delay", 0).max(0),
                facings,
            });
        }
    }

    /// `SculkSpreader.save`.
    pub fn save(&self, nbt: &mut Compound) {
        let list = self
            .cursors
            .iter()
            .map(|c| {
                let mut t = Compound::new();
                t.insert("pos", Tag::IntArray(vec![c.pos.x, c.pos.y, c.pos.z]));
                t.insert("charge", Tag::Int(c.charge));
                t.insert("decay_delay", Tag::Int(c.decay_delay));
                t.insert("update_delay", Tag::Int(c.update_delay));
                if let Some(f) = c.facings {
                    t.insert(
                        "facings",
                        Tag::List(List(
                            Direction::ALL
                                .iter()
                                .filter(|&&d| f & (1 << d as u8) != 0)
                                .map(|&d| Tag::String(direction_name(d).to_owned()))
                                .collect(),
                        )),
                    );
                }
                Tag::Compound(t)
            })
            .collect();
        nbt.insert("cursors", Tag::List(List(list)));
    }
}

/// Fills the sculk catalyst behaviours.
pub(super) fn fill(block: BlockId, b: &mut BlockBehaviour) {
    if block == ids::SCULK_CATALYST {
        b.tick = Some(tick);
    }
}

/// `SculkCatalystBlock.tick`: the bloom ends.
fn tick(level: &mut Level, state: BlockState, pos: BlockPos) {
    if state.value("bloom") == Some("true") {
        level.set_block_and_update(pos, state.set("bloom", "false"));
    }
}

/// Whether a catalyst listens at `position` (a mob dying there gives its experience to it).
pub fn catalyst_near(level: &Level, position: Vec3) -> bool {
    let mut found = Vec::new();
    nearest_catalysts(level, position, &mut found);
    !found.is_empty()
}

/// [`catalyst_near`] from the blocks alone, for a player's death in the player tick: a
/// catalyst block within the listener radius (every catalyst block has its listener).
pub fn catalyst_block_near(
    blocks: &dyn crate::survival::blocks::BlockAccess,
    position: Vec3,
) -> bool {
    let c = containing(position);
    let r = LISTENER_RADIUS;
    (-r..=r).any(|dx| {
        (-r..=r).any(|dy| {
            (-r..=r).any(|dz| {
                dx * dx + dy * dy + dz * dz <= r * r
                    && blocks
                        .block_state(BlockPos::new(c.x + dx, c.y + dy, c.z + dz))
                        .is_some_and(|s| s.is(ids::SCULK_CATALYST))
            })
        })
    })
}

fn nearest_catalysts(level: &Level, position: Vec3, out: &mut Vec<(BlockPos, f64)>) {
    let radius = crate::level::game_event::notification_radius(events().entity_die);
    level.block_listeners_near(
        position,
        radius,
        |e| matches!(e, BlockEntity::SculkCatalyst(_)),
        out,
    );
}

/// `CatalystListener.handleGameEvent(ENTITY_DIE)` of the nearest catalyst (`BY_DISTANCE`: the
/// first one consumes the experience): `experience` charge where the mob died, and the bloom.
/// Returns whether a catalyst took it.
pub fn on_mob_death(level: &mut Level, position: Vec3, experience: i32) -> bool {
    let mut found = Vec::new();
    nearest_catalysts(level, position, &mut found);
    let Some(&(pos, _)) = found.first() else {
        return false;
    };
    if experience > 0
        && let Some(BlockEntity::SculkCatalyst(c)) = level.block_entity_mut(pos)
    {
        // `BlockPos.containing(sourcePosition.relative(UP, 0.5))`.
        let start = containing(Vec3::new(position.x, position.y + 0.5, position.z));
        c.add_cursors(start, experience);
    }
    bloom(level, pos);
    true
}

/// `CatalystListener.bloom`.
fn bloom(level: &mut Level, pos: BlockPos) {
    let state = level.block(pos);
    if !state.is(ids::SCULK_CATALYST) {
        return;
    }
    level.set_block_and_update(pos, state.set("bloom", "true"));
    level.schedule_block_tick(pos, state.block(), PULSE_TICKS);
    level.send_particles(
        crate::level::particles::simple("minecraft:sculk_soul"),
        vanillite_base::Vec3::new(
            f64::from(pos.x) + 0.5,
            f64::from(pos.y) + 1.15,
            f64::from(pos.z) + 0.5,
        ),
        2,
        vanillite_base::Vec3::new(0.2, 0.0, 0.2),
        0.0,
    );
    let pitch = 0.6 + level.random().next_float() * 0.4;
    let sound = sound_id("block.sculk_catalyst.bloom");
    level.play_block_sound(None, pos, sound, SOUND_BLOCKS, 2.0, pitch);
}

/// The block entity ticker (`serverTick`): `updateCursors(level, pos, random, true)`.
pub fn tick_block_entity(level: &mut Level, pos: BlockPos, _: BlockState) {
    let mut cursors = match level.block_entity_mut(pos) {
        Some(BlockEntity::SculkCatalyst(c)) if !c.cursors.is_empty() => {
            std::mem::take(&mut c.cursors)
        }
        _ => return,
    };
    update_cursors(level, &mut cursors, pos, true);
    if let Some(BlockEntity::SculkCatalyst(c)) = level.block_entity_mut(pos) {
        c.cursors = cursors;
    }
}

/// `SculkSpreader.updateCursors` of the level spreader: cursors on the same position merge
/// while their charges fit.
pub fn update_cursors(
    level: &mut Level,
    cursors: &mut Vec<ChargeCursor>,
    origin: BlockPos,
    spread_veins: bool,
) {
    if cursors.is_empty() {
        return;
    }
    let mut processed: Vec<ChargeCursor> = Vec::with_capacity(cursors.len());
    // Position → index into `processed` of the mergeable cursor there.
    let mut mergeable: Vec<(BlockPos, usize)> = Vec::new();
    let mut charges: Vec<(BlockPos, i32)> = Vec::new();
    for mut cursor in cursors.drain(..) {
        if is_pos_unreasonable(cursor.pos, origin) {
            continue;
        }
        update(level, &mut cursor, origin, spread_veins);
        if cursor.charge <= 0 {
            level.level_event(None, CHARGE_EVENT, cursor.pos, 0);
            continue;
        }
        let pos = cursor.pos;
        match charges.iter_mut().find(|(p, _)| *p == pos) {
            Some((_, c)) => *c += cursor.charge,
            None => charges.push((pos, cursor.charge)),
        }
        match mergeable.iter_mut().find(|(p, _)| *p == pos) {
            None => {
                mergeable.push((pos, processed.len()));
                processed.push(cursor);
            }
            Some((_, existing)) => {
                let e = &mut processed[*existing];
                if cursor.charge + e.charge <= MAX_CHARGE {
                    // `mergeWith`.
                    e.charge += cursor.charge;
                    e.update_delay = e.update_delay.min(cursor.update_delay);
                } else {
                    let smaller = cursor.charge < e.charge;
                    processed.push(cursor);
                    if smaller {
                        *existing = processed.len() - 1;
                    }
                }
            }
        }
    }
    for (pos, charge) in charges {
        let faces = mergeable
            .iter()
            .find(|(p, _)| *p == pos)
            .and_then(|&(_, i)| processed[i].facings);
        if charge > 0
            && let Some(faces) = faces
        {
            let particles = ((charge as f64).ln_1p() / f64::from(2.3_f32)) as i32 + 1;
            level.level_event(None, CHARGE_EVENT, pos, (particles << 6) + i32::from(faces));
        }
    }
    *cursors = processed;
}

/// The `SculkBehaviour` of a block: `SculkBlock`, `SculkVeinBlock` or `SculkBehaviour.DEFAULT`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Behaviour {
    Default,
    Sculk,
    Vein,
}

impl Behaviour {
    fn of(state: BlockState) -> Behaviour {
        if state.is(ids::SCULK) {
            Behaviour::Sculk
        } else if state.is(ids::SCULK_VEIN) {
            Behaviour::Vein
        } else {
            Behaviour::Default
        }
    }

    fn is_sculk(self) -> bool {
        self != Behaviour::Default
    }

    fn update_decay_delay(self, age: i32) -> i32 {
        match self {
            Behaviour::Default => (age - 1).max(0),
            Behaviour::Sculk | Behaviour::Vein => 1,
        }
    }
}

/// `ChargeCursor.update`.
fn update(level: &mut Level, cursor: &mut ChargeCursor, origin: BlockPos, spread_veins: bool) {
    // `shouldUpdate`: charged and the origin's chunk ticks blocks.
    if cursor.charge <= 0 || !level.is_block_ticking(origin.chunk()) {
        return;
    }
    if cursor.update_delay > 0 {
        cursor.update_delay -= 1;
        return;
    }
    let mut current = level.block(cursor.pos);
    let mut behaviour = Behaviour::of(current);
    if spread_veins && attempt_spread_vein(level, behaviour, cursor.pos, current, cursor.facings) {
        if behaviour != Behaviour::Sculk {
            current = level.block(cursor.pos);
            behaviour = Behaviour::of(current);
        }
        let sound = sound_id("block.sculk.spread");
        level.play_block_sound(None, cursor.pos, sound, SOUND_BLOCKS, 1.0, 1.0);
    }
    cursor.charge = attempt_use_charge(level, behaviour, cursor, origin, spread_veins);
    if cursor.charge <= 0 {
        on_discharged(level, behaviour, current, cursor.pos);
        return;
    }
    if let Some(transfer) = valid_movement_pos(level, cursor.pos) {
        on_discharged(level, behaviour, current, cursor.pos);
        cursor.pos = transfer;
        current = level.block(transfer);
    }
    if Behaviour::of(current).is_sculk() {
        cursor.facings = Some(available_faces(current));
    }
    cursor.decay_delay = behaviour.update_decay_delay(cursor.decay_delay);
    cursor.update_delay = 1;
}

/// `SculkBehaviour.attemptSpreadVein`.
fn attempt_spread_vein(
    level: &mut Level,
    behaviour: Behaviour,
    pos: BlockPos,
    state: BlockState,
    facings: Option<u8>,
) -> bool {
    if behaviour == Behaviour::Default {
        match facings {
            None => {
                let current = level.block(pos);
                return spread_all(level, true, current, pos) > 0;
            }
            Some(faces) if faces != 0 => {
                if !state.is_air() && !is_water(state) {
                    return false;
                }
                return regrow(level, pos, state, faces);
            }
            Some(_) => {}
        }
    }
    spread_all(level, false, state, pos) > 0
}

/// `getFluidState().is(Fluids.WATER)`: still (source) water only, not flowing water.
pub(super) fn is_water(state: BlockState) -> bool {
    crate::mob::is_water(state) && state.fluid_state().info().source
}

/// `SculkBehaviour.attemptUseCharge`.
fn attempt_use_charge(
    level: &mut Level,
    behaviour: Behaviour,
    cursor: &ChargeCursor,
    origin: BlockPos,
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
        Behaviour::Sculk => sculk_use_charge(level, cursor, origin),
        Behaviour::Vein => {
            if spread_veins && attempt_place_sculk(level, cursor.pos) {
                cursor.charge - 1
            } else if level.random().next_int_bounded(CHARGE_DECAY_RATE) == 0 {
                (cursor.charge as f32 * 0.5).floor() as i32
            } else {
                cursor.charge
            }
        }
    }
}

/// `SculkBlock.attemptUseCharge`.
fn sculk_use_charge(level: &mut Level, cursor: &ChargeCursor, origin: BlockPos) -> i32 {
    let charge = cursor.charge;
    if charge == 0 || level.random().next_int_bounded(CHARGE_DECAY_RATE) != 0 {
        return charge;
    }
    let pos = cursor.pos;
    let close_to_catalyst = pos.dist_sqr(origin) < f64::from(NO_GROWTH_RADIUS * NO_GROWTH_RADIUS);
    if !close_to_catalyst && can_place_growth(level, pos) {
        if level.random().next_int_bounded(GROWTH_SPAWN_COST) < charge {
            let growth_pos = pos.offset(0, 1, 0);
            let state = random_growth_state(level, growth_pos);
            if super::hook_events::allows(level, BLOCK_SPREAD, growth_pos, state) {
                level.set_block_and_update(growth_pos, state);
                let sound = if state.is(ids::SCULK_SHRIEKER) {
                    "block.sculk_shrieker.place"
                } else {
                    "block.sculk_sensor.place"
                };
                level.play_block_sound(None, pos, sound_id(sound), SOUND_BLOCKS, 1.0, 1.0);
            }
        }
        return 0.max(charge - GROWTH_SPAWN_COST);
    }
    if level.random().next_int_bounded(ADDITIONAL_DECAY_RATE) != 0 {
        charge
    } else if close_to_catalyst {
        charge - 1
    } else {
        charge - decay_penalty(pos, origin, charge)
    }
}

/// `SculkBlock.canPlaceGrowth`: air or still water above, at most two growth inhibitors
/// around.
fn can_place_growth(level: &Level, pos: BlockPos) -> bool {
    let above = level.block(pos.offset(0, 1, 0));
    let water_above = above.is(ids::WATER) && is_water(above);
    if !(above.is_air() || water_above) {
        return false;
    }
    let inhibitors = &tags().blocks.sculk_growth_inhibitors;
    let mut matched = 0;
    for y in pos.y..=pos.y + 2 {
        if !level.in_height(y) {
            continue;
        }
        for z in pos.z - 4..=pos.z + 4 {
            for x in pos.x - 4..=pos.x + 4 {
                if inhibitors.contains(level.block(BlockPos::new(x, y, z))) {
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

/// `SculkBlock.getRandomGrowthState` of the level: shriekers cannot summon.
fn random_growth_state(level: &mut Level, pos: BlockPos) -> BlockState {
    let state = if level.random().next_int_bounded(11) == 0 {
        ids::SCULK_SHRIEKER
            .default_state()
            .set("can_summon", "false")
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

/// `SculkBlock.getDecayPenalty`.
fn decay_penalty(pos: BlockPos, origin: BlockPos, charge: i32) -> i32 {
    let outer = (pos.dist_sqr(origin).sqrt() as f32 - NO_GROWTH_RADIUS as f32).powi(2);
    let max_reach = ((24 - NO_GROWTH_RADIUS) * (24 - NO_GROWTH_RADIUS)) as f32;
    let factor = (outer / max_reach).min(1.0);
    1.max((charge as f32 * factor * 0.5) as i32)
}

/// `SculkVeinBlock.attemptPlaceSculk`: a face on a replaceable block turns it into sculk.
fn attempt_place_sculk(level: &mut Level, pos: BlockPos) -> bool {
    let state = level.block(pos);
    let replaceable = &tags().blocks.sculk_replaceable;
    for support in shuffled_directions(level) {
        if !has_face(state, support) {
            continue;
        }
        let support_pos = pos.relative(support);
        if !replaceable.contains(level.block(support_pos)) {
            continue;
        }
        let sculk = ids::SCULK.default_state();
        if !super::hook_events::allows(level, BLOCK_SPREAD, support_pos, sculk) {
            return false;
        }
        level.set_block_and_update(support_pos, sculk);
        let sound = sound_id("block.sculk.spread");
        level.play_block_sound(None, support_pos, sound, SOUND_BLOCKS, 1.0, 1.0);
        spread_all(level, false, sculk, support_pos);
        let skip = support.opposite();
        for direction in Direction::ALL {
            if direction == skip {
                continue;
            }
            let vein_pos = support_pos.relative(direction);
            let vein = level.block(vein_pos);
            if vein.is(ids::SCULK_VEIN) {
                vein_on_discharged(level, vein, vein_pos);
            }
        }
        return true;
    }
    false
}

/// `SculkBehaviour.onDischarged`: only veins react.
fn on_discharged(level: &mut Level, behaviour: Behaviour, state: BlockState, pos: BlockPos) {
    if behaviour == Behaviour::Vein {
        vein_on_discharged(level, state, pos);
    }
}

/// `ChargeCursor.getRandomizedNonCornerNeighbourOffsets` order source.
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

/// `Util.shuffle` with the level random.
fn shuffle<T>(items: &mut [T], level: &mut Level) {
    for i in (2..=items.len()).rev() {
        let swap_to = level.random().next_int_bounded(i as i32) as usize;
        items.swap(i - 1, swap_to);
    }
}

/// `Direction.allShuffled(random)`.
fn shuffled_directions(level: &mut Level) -> [Direction; 6] {
    let mut directions = Direction::ALL;
    shuffle(&mut directions, level);
    directions
}

/// `ChargeCursor.getValidMovementPos` of the level spreader.
fn valid_movement_pos(level: &mut Level, pos: BlockPos) -> Option<BlockPos> {
    let mut offsets = NON_CORNER_NEIGHBOURS;
    shuffle(&mut offsets, level);
    let mut sculk_position = pos;
    for [dx, dy, dz] in offsets {
        let neighbour = pos.offset(dx, dy, dz);
        let transferee = level.block(neighbour);
        if Behaviour::of(transferee).is_sculk() && is_movement_unobstructed(level, pos, neighbour) {
            sculk_position = neighbour;
            if has_substrate_access(level, transferee, neighbour) {
                break;
            }
        }
    }
    (sculk_position != pos).then_some(sculk_position)
}

/// `SculkVeinBlock.hasSubstrateAccess`.
fn has_substrate_access(level: &Level, state: BlockState, pos: BlockPos) -> bool {
    let replaceable = &tags().blocks.sculk_replaceable;
    state.is(ids::SCULK_VEIN)
        && Direction::ALL
            .iter()
            .any(|&d| has_face(state, d) && replaceable.contains(level.block(pos.relative(d))))
}

/// `ChargeCursor.isMovementUnobstructed`.
fn is_movement_unobstructed(level: &Level, from: BlockPos, to: BlockPos) -> bool {
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
        !is_face_sturdy(level.block(test), direction.opposite())
    };
    if dx == 0 {
        unobstructed(direction_y) || unobstructed(direction_z)
    } else if dy == 0 {
        unobstructed(direction_x) || unobstructed(direction_z)
    } else {
        unobstructed(direction_x) || unobstructed(direction_y)
    }
}

/// `ChargeCursor.isPosUnreasonable`.
fn is_pos_unreasonable(pos: BlockPos, origin: BlockPos) -> bool {
    let d = (pos.x - origin.x)
        .abs()
        .max((pos.y - origin.y).abs())
        .max((pos.z - origin.z).abs());
    d > 1024
}

/// The serialized name of a direction.
pub(super) fn direction_name(direction: Direction) -> &'static str {
    match direction {
        Direction::Down => "down",
        Direction::Up => "up",
        Direction::North => "north",
        Direction::South => "south",
        Direction::West => "west",
        Direction::East => "east",
    }
}

fn direction_by_name(name: &str) -> Option<Direction> {
    Direction::ALL
        .into_iter()
        .find(|&d| direction_name(d) == name)
}

/// `MultifaceBlock.hasFace`.
pub(super) fn has_face(state: BlockState, face: Direction) -> bool {
    state.value(direction_name(face)) == Some("true")
}

/// `MultifaceBlock.hasAnyFace`.
fn has_any_face(state: BlockState) -> bool {
    Direction::ALL.iter().any(|&d| has_face(state, d))
}

/// `MultifaceBlock.availableFaces` of a sculk block (only veins have faces).
fn available_faces(state: BlockState) -> u8 {
    if !state.is(ids::SCULK_VEIN) {
        return 0;
    }
    Direction::ALL
        .iter()
        .filter(|&&d| has_face(state, d))
        .fold(0, |mask, &d| mask | 1 << d as u8)
}

/// `MultifaceBlock.canAttachTo(level, pos, direction)`.
pub(super) fn can_attach_to(level: &Level, pos: BlockPos, direction: Direction) -> bool {
    let neighbour = level.block(pos.relative(direction));
    let face = direction.opposite();
    is_face_sturdy(neighbour, face)
        || super::place::util::face_full(neighbour.collision_shape(), face)
}

/// `MultifaceSpreader.spreadAll(state, level, pos, postProcess = false)` of the vein spreader
/// (`same_space`: only `SAME_POSITION`).
fn spread_all(level: &mut Level, same_space: bool, state: BlockState, pos: BlockPos) -> i64 {
    let mut count = 0;
    for face in Direction::ALL {
        if !(!state.is(ids::SCULK_VEIN) || has_face(state, face)) {
            continue;
        }
        for spread in Direction::ALL {
            if let Some((target, dir)) = spread_pos(level, same_space, state, pos, face, spread)
                && place_vein(level, target, dir)
            {
                count += 1;
            }
        }
    }
    count
}

fn axis(d: Direction) -> u8 {
    match d {
        Direction::Down | Direction::Up => 1,
        Direction::North | Direction::South => 2,
        Direction::West | Direction::East => 0,
    }
}

/// `getSpreadFromFaceTowardDirection` with `canSpreadInto`.
fn spread_pos(
    level: &Level,
    same_space: bool,
    state: BlockState,
    pos: BlockPos,
    starting_face: Direction,
    spread_direction: Direction,
) -> Option<(BlockPos, Direction)> {
    if axis(spread_direction) == axis(starting_face) {
        return None;
    }
    let other_source = !state.is(ids::SCULK_VEIN);
    if !(other_source || has_face(state, starting_face) && !has_face(state, spread_direction)) {
        return None;
    }
    let same_position = (pos, spread_direction);
    if can_spread_into(level, pos, same_position) {
        return Some(same_position);
    }
    if same_space {
        return None;
    }
    let same_plane = (pos.relative(spread_direction), starting_face);
    if can_spread_into(level, pos, same_plane) {
        return Some(same_plane);
    }
    let wrap_around = (
        pos.relative(spread_direction).relative(starting_face),
        spread_direction.opposite(),
    );
    can_spread_into(level, pos, wrap_around).then_some(wrap_around)
}

/// `DefaultSpreaderConfig.canSpreadInto`.
fn can_spread_into(level: &Level, source: BlockPos, (pos, face): (BlockPos, Direction)) -> bool {
    let existing = level.block(pos);
    state_can_be_replaced(level, source, pos, face, existing)
        && is_valid_state_for_placement(level, existing, pos, face)
}

/// `SculkVeinSpreaderConfig.stateCanBeReplaced`.
fn state_can_be_replaced(
    level: &Level,
    source: BlockPos,
    placement: BlockPos,
    direction: Direction,
    existing: BlockState,
) -> bool {
    let against = level.block(placement.relative(direction));
    if against.is(ids::SCULK) || against.is(ids::SCULK_CATALYST) || against.is(ids::MOVING_PISTON) {
        return false;
    }
    let manhattan = (source.x - placement.x).abs()
        + (source.y - placement.y).abs()
        + (source.z - placement.z).abs();
    if manhattan == 2 {
        let neighbour = source.relative(direction.opposite());
        if is_face_sturdy(level.block(neighbour), direction) {
            return false;
        }
    }
    let fluid = existing.fluid_state();
    if !fluid.is_empty() && !is_water(existing) {
        return false;
    }
    if tags().blocks.fire.contains(existing) {
        return false;
    }
    can_be_replaced(existing)
        || existing.is_air()
        || existing.is(ids::SCULK_VEIN)
        || existing.is(ids::WATER) && fluid.info().source
}

/// `MultifaceBlock.isValidStateForPlacement` of the vein.
fn is_valid_state_for_placement(
    level: &Level,
    old: BlockState,
    pos: BlockPos,
    direction: Direction,
) -> bool {
    if old.is(ids::SCULK_VEIN) && has_face(old, direction) {
        return false;
    }
    can_attach_to(level, pos, direction)
}

/// `MultifaceSpreader.spreadToFace` with `placeBlock` (`setBlock(pos, state, 2)`).
fn place_vein(level: &mut Level, pos: BlockPos, face: Direction) -> bool {
    let old = level.block(pos);
    if !is_valid_state_for_placement(level, old, pos, face) {
        return false;
    }
    let fluid = old.fluid_state();
    let base = if old.is(ids::SCULK_VEIN) {
        old
    } else if fluid.info().source && is_water(old) {
        ids::SCULK_VEIN.default_state().set("waterlogged", "true")
    } else {
        ids::SCULK_VEIN.default_state()
    };
    let Some(state) = base.with(direction_name(face), "true") else {
        return false;
    };
    super::hook_events::set_block(level, BLOCK_SPREAD, pos, state, update::CLIENTS)
}

/// `SculkVeinBlock.regrow`.
fn regrow(level: &mut Level, pos: BlockPos, existing: BlockState, faces: u8) -> bool {
    let mut state = ids::SCULK_VEIN.default_state();
    let mut any = false;
    for face in Direction::ALL {
        if faces & (1 << face as u8) == 0 || !can_attach_to(level, pos, face) {
            continue;
        }
        if let Some(s) = state.with(direction_name(face), "true") {
            state = s;
            any = true;
        }
    }
    if !any {
        return false;
    }
    if !existing.fluid_state().is_empty() {
        state = state.set("waterlogged", "true");
    }
    level.set_block_and_update(pos, state);
    true
}

/// `SculkVeinBlock.onDischarged`.
fn vein_on_discharged(level: &mut Level, state: BlockState, pos: BlockPos) {
    if !state.is(ids::SCULK_VEIN) {
        return;
    }
    let mut state = state;
    for direction in Direction::ALL {
        if has_face(state, direction) && level.block(pos.relative(direction)).is(ids::SCULK) {
            state = state.set(direction_name(direction), "false");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charge_splits_into_cursors_of_at_most_1000() {
        let mut c = Catalyst::default();
        c.add_cursors(BlockPos::new(1, 2, 3), 2500);
        let charges: Vec<i32> = c.cursors.iter().map(|c| c.charge).collect();
        assert_eq!(charges, vec![1000, 1000, 500]);
        for _ in 0..40 {
            c.add_cursors(BlockPos::ZERO, 5);
        }
        assert_eq!(c.cursors.len(), MAX_CURSORS);
    }

    #[test]
    fn cursors_round_trip_through_nbt() {
        let mut c = Catalyst::default();
        c.add_cursors(BlockPos::new(-4, 60, 9), 37);
        c.cursors[0].facings = Some(0b10_0001);
        c.cursors[0].update_delay = 1;
        let mut nbt = Compound::new();
        c.save(&mut nbt);
        let mut back = Catalyst::default();
        back.load(&nbt);
        assert_eq!(back, c);
    }

    #[test]
    fn decay_penalty_grows_with_distance() {
        let origin = BlockPos::ZERO;
        assert_eq!(decay_penalty(BlockPos::new(5, 0, 0), origin, 100), 1);
        assert!(decay_penalty(BlockPos::new(20, 0, 0), origin, 100) > 10);
        assert_eq!(decay_penalty(BlockPos::new(40, 0, 0), origin, 100), 50);
    }
}

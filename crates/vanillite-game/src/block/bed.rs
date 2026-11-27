//! Beds and respawn anchors as places to sleep and respawn: the dimension's bed rules
//! (`BedRule`), sleep heights, where a sleeper or a respawning player stands up
//! (`AbstractBedBlock.findStandUpPosition`, `RespawnAnchorBlock.findStandUpPosition`,
//! `DismountHelper.findSafeDismountLocation`), and the block side of using them
//! (`AbstractBedBlock.useWithoutItem`, `RespawnAnchorBlock.useItemOn`/`useWithoutItem`).
//!
//! What needs the player (sleeping, setting the respawn point) runs after the player's packets
//! as a [`PlayerBlockUse`] ([`crate::survival::sleep`]).

use serde_json::Value;
use vanillite_base::{Aabb, BlockPos, Direction, Vec3};
use vanillite_data::block::{BlockId, BlockKind, BlockState, ids};
use vanillite_protocol::text::Component;
use vanillite_world::entity::{Dimensions, EntityId};
use vanillite_worldgen::behaviour::StateExt;

use super::{BlockBehaviour, Interaction, ItemInteraction, UseContext, sound_id};
use crate::item::{self, ItemStack};
use crate::level::explosion::{BlockInteraction, BlockResistance, ExplosionStyle};
use crate::level::game_event::{GameEventContext, events};
use crate::level::{Level, SOUND_BLOCKS};

/// `BedRule.Rule`: when a bed rule holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum When {
    /// `always`.
    Always,
    /// `when_dark`: `Level.isDarkOutside`.
    WhenDark,
    /// `never`.
    Never,
}

impl When {
    fn parse(v: Option<&Value>) -> Option<When> {
        match v?.as_str()? {
            "always" => Some(When::Always),
            "when_dark" => Some(When::WhenDark),
            "never" => Some(When::Never),
            _ => None,
        }
    }

    /// `Rule.test(level)` with `dark` as `isDarkOutside`.
    pub fn test(self, dark: bool) -> bool {
        match self {
            When::Always => true,
            When::WhenDark => dark,
            When::Never => false,
        }
    }
}

/// `BedRule`: the `gameplay/bed_rule` (`gameplay/straw_bed_rule`) attribute of a dimension.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BedRule {
    /// `can_sleep`.
    pub can_sleep: When,
    /// `can_set_spawn`.
    pub can_set_spawn: When,
    /// `destroy_on_use`: the bed explodes (straw beds break) when used.
    pub destroy_on_use: bool,
    /// `destroy_on_leave`: the bed breaks when its sleeper leaves (straw beds).
    pub destroy_on_leave: bool,
    /// `error_message`: shown when sleeping is not possible.
    pub error_message: Option<Component>,
}

impl BedRule {
    /// `BedRule.CAN_SLEEP_WHEN_DARK`, the default of `gameplay/bed_rule`.
    pub fn can_sleep_when_dark() -> BedRule {
        BedRule {
            can_sleep: When::WhenDark,
            can_set_spawn: When::Always,
            destroy_on_use: false,
            destroy_on_leave: false,
            error_message: Some(Component::translatable(
                "block.minecraft.bed.no_sleep",
                vec![],
            )),
        }
    }

    /// `BedRule.DESTROY_ON_LEAVE`, the default of `gameplay/straw_bed_rule`.
    pub fn destroy_on_leave() -> BedRule {
        BedRule {
            can_sleep: When::WhenDark,
            can_set_spawn: When::Never,
            destroy_on_use: false,
            destroy_on_leave: true,
            error_message: Some(Component::translatable(
                "block.minecraft.bed.no_sleep",
                vec![],
            )),
        }
    }

    /// `BedRule.CODEC`; `None` when a required field is missing.
    pub fn from_json(v: &Value) -> Option<BedRule> {
        let flag = |key: &str| v.get(key).and_then(Value::as_bool).unwrap_or(false);
        Some(BedRule {
            can_sleep: When::parse(v.get("can_sleep"))?,
            can_set_spawn: When::parse(v.get("can_set_spawn"))?,
            destroy_on_use: flag("destroy_on_use"),
            destroy_on_leave: flag("destroy_on_leave"),
            error_message: v.get("error_message").map(Component::from_json),
        })
    }
}

/// `Level.isDarkOutside`: no fixed time (a sky) and a sky darkened by 4 or more.
pub fn is_dark_outside(level: &Level) -> bool {
    level.settings.has_sky_light && level.spawner_env.env.sky_darken >= 4
}

/// `instanceof AbstractBedBlock`.
pub fn is_bed(state: BlockState) -> bool {
    matches!(
        state.block().info().kind,
        BlockKind::BedBlock | BlockKind::StrawBedBlock
    )
}

/// A straw bed (`StrawBedBlock`).
pub fn is_straw_bed(state: BlockState) -> bool {
    state.block().info().kind == BlockKind::StrawBedBlock
}

/// `getBedRule(level, pos)`: the dimension's rule for this kind of bed.
pub fn bed_rule(level: &Level, state: BlockState) -> &BedRule {
    if is_straw_bed(state) {
        &level.settings.straw_bed_rule
    } else {
        &level.settings.bed_rule
    }
}

/// The bed's `facing`.
pub fn facing(state: BlockState) -> Direction {
    state.direction("facing").unwrap_or(Direction::North)
}

fn is_head(state: BlockState) -> bool {
    state.value("part") == Some("head")
}

/// `getConnectedDirection`: from this half towards the other.
pub fn connected_direction(state: BlockState) -> Direction {
    let facing = facing(state);
    if is_head(state) {
        facing.opposite()
    } else {
        facing
    }
}

fn relative(pos: BlockPos, d: Direction) -> BlockPos {
    let [x, y, z] = d.normal();
    pos.offset(x, y, z)
}

/// `AbstractBedBlock.getSleepHeight`: the top of the bed's shape (a straw bed's head lies on
/// its foot).
pub fn sleep_height(level: &Level, state: BlockState, pos: BlockPos) -> Option<f64> {
    if !is_bed(state) {
        return None;
    }
    let shape_top = |s: BlockState| s.outline_shape().iter().map(|b| b[4]).reduce(f64::max);
    if is_straw_bed(state) && is_head(state) {
        let laying = level.block(relative(pos, connected_direction(state)));
        if laying.block() != state.block() || is_head(laying) {
            return None;
        }
        return shape_top(laying);
    }
    shape_top(state)
}

/// `NodeEvaluator.isBurningBlock`.
fn is_burning(state: BlockState) -> bool {
    let block = state.block();
    block == ids::FIRE
        || block == ids::SOUL_FIRE
        || block == ids::LAVA
        || block == ids::MAGMA_BLOCK
        || block == ids::LAVA_CAULDRON
        || ((block == ids::CAMPFIRE || block == ids::SOUL_CAMPFIRE) && state.flag("lit"))
}

/// `EntityType.isBlockDangerous` of an entity that is not fire immune (players, villagers).
fn is_dangerous(state: BlockState) -> bool {
    let block = state.block();
    is_burning(state)
        || block == ids::WITHER_ROSE
        || block == ids::SWEET_BERRY_BUSH
        || block == ids::CACTUS
        || block == ids::POWDER_SNOW
}

fn in_tag(state: BlockState, tag: &str, fallback: impl FnOnce(&str) -> bool) -> bool {
    match crate::player::play::game_data() {
        Some(d) if d.block_tags.contains_key(tag) => d.block_in_tag(state.block().0, tag),
        _ => fallback(state.block().name()),
    }
}

/// `DismountHelper.nonClimbableShape`'s top: none for climbable blocks and open trapdoors.
fn non_climbable_top(state: BlockState) -> Option<f64> {
    let climbable = in_tag(state, "minecraft:climbable", |n| {
        n.ends_with("ladder") || n.ends_with("vines") || n == "minecraft:scaffolding"
    });
    let open_trapdoor = state.block().info().kind == BlockKind::TrapDoorBlock && state.flag("open");
    if climbable || open_trapdoor {
        return None;
    }
    state
        .collision_shape()
        .iter()
        .map(|b| b[4])
        .reduce(f64::max)
}

/// `DismountHelper.findSafeDismountLocation(type, level, pos, checkDangerous)` for an entity
/// of `dims` that is not fire immune; `player` adds the `#invalid_spawn_inside` check.
pub fn safe_dismount(
    level: &Level,
    pos: BlockPos,
    check_dangerous: bool,
    dims: Dimensions,
    player: bool,
) -> Option<Vec3> {
    let state = level.block(pos);
    if check_dangerous && is_dangerous(state) {
        return None;
    }
    let below = level.block(pos.offset(0, -1, 0));
    // `getBlockFloorHeight`.
    let floor = match non_climbable_top(state) {
        Some(h) => h,
        None => match non_climbable_top(below) {
            Some(h) if h >= 1.0 => h - 1.0,
            _ => f64::NEG_INFINITY,
        },
    };
    if !(floor.is_finite() && floor < 1.0) {
        return None;
    }
    if check_dangerous && floor <= 0.0 && is_dangerous(below) {
        return None;
    }
    let at = Vec3::new(
        f64::from(pos.x) + 0.5,
        f64::from(pos.y) + floor,
        f64::from(pos.z) + 0.5,
    );
    let half = f64::from(dims.width) / 2.0;
    let aabb = Aabb::new(
        at.x - half,
        at.y,
        at.z - half,
        at.x + half,
        at.y + f64::from(dims.height),
        at.z + half,
    );
    let mut scratch = Vec::new();
    if !crate::mob::physics::no_collision(&level.view(), &aabb, &mut scratch) {
        return None;
    }
    if player {
        let invalid = |s: BlockState| {
            in_tag(s, "minecraft:invalid_spawn_inside", |n| {
                n == "minecraft:end_portal" || n == "minecraft:end_gateway"
            })
        };
        if invalid(state) || invalid(level.block(pos.offset(0, 1, 0))) {
            return None;
        }
    }
    level
        .border
        .is_box_within_bounds(aabb.min_x, aabb.min_z, aabb.max_x, aabb.max_z)
        .then_some(at)
}

fn dir_xz(d: Direction) -> (i32, i32) {
    let [x, _, z] = d.normal();
    (x, z)
}

/// `AbstractBedBlock.findStandUpPosition` (the bunk bed search included) for an entity of
/// `dims` facing `yaw`.
pub fn stand_up_position(
    level: &Level,
    bed: BlockPos,
    forward: Direction,
    yaw: f32,
    dims: Dimensions,
    player: bool,
) -> Option<Vec3> {
    let right = match forward {
        Direction::North => Direction::East,
        Direction::East => Direction::South,
        Direction::South => Direction::West,
        _ => Direction::North,
    };
    // `right.isFacingAngle(yaw)`.
    let (rx, rz) = dir_xz(right);
    let a = yaw * crate::mob::DEG_TO_RAD;
    let facing = f64::from(rx) * f64::from(-a.sin()) + f64::from(rz) * f64::from(a.cos()) > 0.0;
    let side = if facing { right.opposite() } else { right };
    let (fx, fz) = dir_xz(forward);
    let (sx, sz) = dir_xz(side);
    let surround: [(i32, i32); 10] = [
        (sx, sz),
        (sx - fx, sz - fz),
        (sx - fx * 2, sz - fz * 2),
        (-fx * 2, -fz * 2),
        (-sx - fx * 2, -sz - fz * 2),
        (-sx - fx, -sz - fz),
        (-sx, -sz),
        (-sx + fx, -sz + fz),
        (fx, fz),
        (sx + fx, sz + fz),
    ];
    let above: [(i32, i32); 2] = [(0, 0), (-fx, -fz)];
    let at = |base: BlockPos, offsets: &[(i32, i32)], safe: bool| {
        offsets
            .iter()
            .find_map(|&(x, z)| safe_dismount(level, base.offset(x, 0, z), safe, dims, player))
    };
    if !is_bed(level.block(bed.offset(0, -1, 0))) {
        // `bedStandUpOffsets`: around the bed, then on it.
        let stand = |safe: bool| at(bed, &surround, safe).or_else(|| at(bed, &above, safe));
        return stand(true).or_else(|| stand(false));
    }
    // A bunk bed: around it, around the one below, on it.
    let below = bed.offset(0, -1, 0);
    at(bed, &surround, true)
        .or_else(|| at(below, &surround, true))
        .or_else(|| at(bed, &above, true))
        .or_else(|| at(bed, &surround, false))
        .or_else(|| at(below, &surround, false))
        .or_else(|| at(bed, &above, false))
}

/// `RespawnAnchorBlock.RESPAWN_OFFSETS`: around the anchor, below, above, then on top.
const ANCHOR_OFFSETS: [(i32, i32, i32); 25] = {
    const H: [(i32, i32); 8] = [
        (0, -1),
        (-1, 0),
        (0, 1),
        (1, 0),
        (-1, -1),
        (1, -1),
        (-1, 1),
        (1, 1),
    ];
    let mut out = [(0, 1, 0); 25];
    let mut i = 0;
    while i < 8 {
        out[i] = (H[i].0, 0, H[i].1);
        out[i + 8] = (H[i].0, -1, H[i].1);
        out[i + 16] = (H[i].0, 1, H[i].1);
        i += 1;
    }
    out
};

/// `RespawnAnchorBlock.findStandUpPosition` for a player.
pub fn anchor_stand_up_position(level: &Level, pos: BlockPos, dims: Dimensions) -> Option<Vec3> {
    let find = |safe: bool| {
        ANCHOR_OFFSETS
            .iter()
            .find_map(|&(x, y, z)| safe_dismount(level, pos.offset(x, y, z), safe, dims, true))
    };
    find(true).or_else(|| find(false))
}

/// What a player's click on a bed or anchor asks of the player's side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayerBlockUse {
    /// `AbstractBedBlock.useWithoutItem` on the bed whose head is at the position.
    Bed,
    /// A charged, working respawn anchor sets the player's respawn point.
    AnchorSpawn,
}

/// Fills the bed and respawn anchor behaviours.
pub(super) fn fill(block: BlockId, b: &mut BlockBehaviour) {
    if matches!(
        block.info().kind,
        BlockKind::BedBlock | BlockKind::StrawBedBlock
    ) {
        b.use_without_item = Some(bed_use_without_item);
    } else if block == ids::RESPAWN_ANCHOR {
        b.use_item_on = Some(anchor_use_item_on);
        b.use_without_item = Some(anchor_use_without_item);
    }
}

/// `AbstractBedBlock.useWithoutItem`: a whole bed passes the click to the player's side.
fn bed_use_without_item(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    ctx: &UseContext,
) -> Interaction {
    let other = level.block(relative(pos, connected_direction(state)));
    if other.block() != state.block() || is_head(other) == is_head(state) {
        return Interaction::Consume;
    }
    let head = if is_head(state) {
        pos
    } else {
        relative(pos, facing(state))
    };
    level.player_block_use(ctx.player, head, PlayerBlockUse::Bed);
    Interaction::Success
}

fn charges(state: BlockState) -> i32 {
    state.int("charges").unwrap_or(0)
}

/// `RespawnAnchorBlock.charge`: one more charge, with its game event and sound.
pub fn charge_anchor(
    level: &mut Level,
    source: Option<EntityId>,
    pos: BlockPos,
    state: BlockState,
) {
    let charged = state.set_int("charges", charges(state) + 1);
    level.set_block_and_update(pos, charged);
    let context = GameEventContext::with_state(source, charged);
    level.game_event_block(events().block_change, pos, context);
    level.play_block_sound(
        None,
        pos,
        sound_id("block.respawn_anchor.charge"),
        SOUND_BLOCKS,
        1.0,
        1.0,
    );
}

/// `RespawnAnchorBlock.useItemOn`: glowstone charges the anchor.
fn anchor_use_item_on(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    ctx: &UseContext,
    stack: &mut ItemStack,
) -> ItemInteraction {
    let fuel = vanillite_data::item::ids::GLOWSTONE;
    let chargeable = charges(state) < 4;
    if item::item_id(stack) == fuel && chargeable {
        charge_anchor(level, Some(ctx.player), pos, state);
        stack.shrink(1);
        ItemInteraction::Done(Interaction::Success)
    } else if ctx.main_hand && ctx.off_hand_item == fuel.0 && chargeable {
        ItemInteraction::Pass
    } else {
        ItemInteraction::TryWithEmptyHand
    }
}

/// `RespawnAnchorBlock.useWithoutItem`: a charged anchor explodes where it does not work and
/// sets the respawn point where it does.
fn anchor_use_without_item(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    ctx: &UseContext,
) -> Interaction {
    if charges(state) == 0 {
        return Interaction::Pass;
    }
    if !level.settings.respawn_anchor_works {
        explode_anchor(level, pos);
        return Interaction::Success;
    }
    if ctx.respawn_here == Some(pos) {
        return Interaction::Consume;
    }
    level.player_block_use(ctx.player, pos, PlayerBlockUse::AnchorSpawn);
    Interaction::Success
}

/// `FluidState.is(WATER)`, source or flowing with its amount.
fn water_that_would_flow(level: &Level, pos: BlockPos) -> bool {
    let fluid = level.fluid(pos);
    if !super::fluid::is_water(fluid) {
        return false;
    }
    if fluid.info().source {
        return true;
    }
    fluid.info().amount >= 2 && !super::fluid::is_water(level.fluid(pos.offset(0, -1, 0)))
}

/// `RespawnAnchorBlock.explode`: the anchor goes and a fiery explosion of 5 takes its place;
/// water next to it (or above) shields the blocks.
pub fn explode_anchor(level: &mut Level, pos: BlockPos) {
    level.remove_block(pos, false);
    let in_water = [
        Direction::North,
        Direction::South,
        Direction::West,
        Direction::East,
    ]
    .into_iter()
    .any(|d| water_that_would_flow(level, relative(pos, d)))
        || super::fluid::is_water(level.fluid(pos.offset(0, 1, 0)));
    explode_respawn_point(
        level,
        pos,
        if in_water {
            BlockResistance::WaterAt(pos)
        } else {
            BlockResistance::Default
        },
    );
}

/// `BedBlock.destroyOnUse`: both halves go and the bed explodes.
pub fn explode_bed(level: &mut Level, head: BlockPos, state: BlockState) {
    level.remove_block(head, false);
    let foot = relative(head, facing(state).opposite());
    if level.block(foot).block() == state.block() {
        level.remove_block(foot, false);
    }
    explode_respawn_point(level, head, BlockResistance::Default);
}

/// `level.explode(null, badRespawnPointExplosion(center), calculator, center, 5, true,
/// ExplosionInteraction.BLOCK)`.
fn explode_respawn_point(level: &mut Level, pos: BlockPos, resistance: BlockResistance) {
    let center = Vec3::new(
        f64::from(pos.x) + 0.5,
        f64::from(pos.y) + 0.5,
        f64::from(pos.z) + 0.5,
    );
    let interaction = if level.settings.block_explosion_drop_decay {
        BlockInteraction::DestroyWithDecay
    } else {
        BlockInteraction::Destroy
    };
    level.explode_with(
        None,
        None,
        center,
        5.0,
        interaction,
        ExplosionStyle {
            resistance,
            fire: true,
            bad_respawn_point: true,
            ..ExplosionStyle::default()
        },
    );
}

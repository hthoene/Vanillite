//! Copper golems (`CopperGolem`): they oxidize over game days (waxing with honeycomb stops it,
//! axes scrape it back), turn into a statue when fully oxidized, lose their antenna item to
//! shears and sort items between chests with their brain (in [`super::ai`]).

use vanillite_base::BlockPos;
use vanillite_base::random::RandomSource;
use vanillite_nbt::{Compound, Tag};

use super::State;
use crate::item::{EquipmentSlot, ItemStack};
use crate::mob::animal::{self, Interaction};
use crate::mob::fam::{Hurt, Interact};
use crate::mob::{MobData, Tick, WorldAction};
use crate::player::encode::{DataWriter, serializer};

/// `WEATHERING_TICK_FROM` and `WEATHERING_TICK_TO`.
const WEATHERING_TICKS: (i32, i32) = (504_000, 552_000);
/// `IGNORE_WEATHERING_TICK`: waxed.
pub const WAXED: i64 = -2;
/// `UNSET_WEATHERING_TICK`.
pub const UNSET: i64 = -1;
/// `TURN_TO_STATUE_CHANCE`.
const TURN_TO_STATUE_CHANCE: f32 = 0.0058;

/// `WeatheringCopper.WeatherState`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Weather {
    #[default]
    Unaffected,
    Exposed,
    Weathered,
    Oxidized,
}

impl Weather {
    pub fn next(self) -> Weather {
        match self {
            Weather::Unaffected => Weather::Exposed,
            Weather::Exposed => Weather::Weathered,
            _ => Weather::Oxidized,
        }
    }

    pub fn previous(self) -> Weather {
        match self {
            Weather::Oxidized => Weather::Weathered,
            Weather::Weathered => Weather::Exposed,
            _ => Weather::Unaffected,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Weather::Unaffected => "unaffected",
            Weather::Exposed => "exposed",
            Weather::Weathered => "weathered",
            Weather::Oxidized => "oxidized",
        }
    }

    fn of(name: &str) -> Weather {
        match name.strip_prefix("minecraft:").unwrap_or(name) {
            "exposed" => Weather::Exposed,
            "weathered" => Weather::Weathered,
            "oxidized" => Weather::Oxidized,
            _ => Weather::Unaffected,
        }
    }
}

/// `CopperGolemState`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GolemState {
    #[default]
    Idle,
    GettingItem,
    GettingNoItem,
    DroppingItem,
    DroppingNoItem,
}

/// The copper golem part of a mob.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CopperGolem {
    pub weather: Weather,
    pub state: GolemState,
    /// `nextWeatheringTick`.
    pub next_weathering_tick: i64,
    /// `TRANSPORT_ITEMS_COOLDOWN_TICKS` set in the constructor.
    pub transport_cooldown: i32,
    /// `openedChestPos` with the container whose lid it opened.
    pub opened_chest: Option<(BlockPos, crate::block::container::ContainerRef)>,
    /// `lastLightningBoltUUID` (0: none).
    pub last_lightning_bolt: u128,
}

pub fn golem(m: &MobData) -> CopperGolem {
    match super::state(m) {
        State::CopperGolem(g) => g.clone(),
        _ => CopperGolem::default(),
    }
}

pub fn golem_mut(m: &mut MobData) -> &mut CopperGolem {
    let s = super::state_mut(m);
    if !matches!(s, State::CopperGolem(_)) {
        *s = State::CopperGolem(CopperGolem {
            next_weathering_tick: UNSET,
            ..CopperGolem::default()
        });
    }
    match s {
        State::CopperGolem(g) => g,
        _ => unreachable!("set above"),
    }
}

/// `CopperGolem.thunderHit` after the plain hit: the first strike of the bolt with the UUID
/// `bolt` takes one oxidation stage off and restarts the weathering timer.
pub fn thunder_hit(
    ents: &mut crate::entities::Entities,
    id: vanillite_world::entity::EntityId,
    bolt: u128,
) {
    let Some(m) = ents.mob_mut(id) else {
        return;
    };
    let g = golem_mut(m);
    if g.last_lightning_bolt == bolt {
        return;
    }
    g.last_lightning_bolt = bolt;
    if g.weather != Weather::Unaffected {
        g.next_weathering_tick = UNSET;
        g.weather = g.weather.previous();
        if let Some(mut e) = ents.store.get_mut(id) {
            e.mark_data_dirty();
        }
    }
}

pub fn init(m: &mut MobData) {
    golem_mut(m);
    // `setRequiredPathLength(48)`, `setCanOpenDoors(true)`, `setPersistenceRequired`.
    m.nav.can_open_doors = true;
    m.persistent = true;
    let cooldown = m.random.next_int_between_inclusive(60, 99);
    golem_mut(m).transport_cooldown = cooldown;
    super::ai::copper_golem_brain(m);
}

/// The sounds by oxidation (`CopperGolemOxidationLevels`).
pub fn sound(m: &MobData, what: &str) -> &'static str {
    match (golem(m).weather, what) {
        (Weather::Weathered, "hurt") => "minecraft:entity.copper_golem_weathered.hurt",
        (Weather::Weathered, "death") => "minecraft:entity.copper_golem_weathered.death",
        (Weather::Weathered, _) => "minecraft:entity.copper_golem_weathered.step",
        (Weather::Oxidized, "hurt") => "minecraft:entity.copper_golem_oxidized.hurt",
        (Weather::Oxidized, "death") => "minecraft:entity.copper_golem_oxidized.death",
        (Weather::Oxidized, _) => "minecraft:entity.copper_golem_oxidized.step",
        (_, "hurt") => "minecraft:entity.copper_golem.hurt",
        (_, "death") => "minecraft:entity.copper_golem.death",
        _ => "minecraft:entity.copper_golem.step",
    }
}

/// `finalizeSpawn`: the spawn sound.
pub fn finalize(m: &mut MobData) {
    // The sound plays when the golem is added; the spawn sound of built golems comes from
    // the block that builds them.
    golem_mut(m);
}

/// `CopperGolem.tick`: weathering on the server.
pub fn after_tick(t: &mut Tick<'_, '_>) {
    let now = t.game_time();
    let g = golem(t.mob_ref());
    if g.next_weathering_tick == WAXED {
        return;
    }
    if g.next_weathering_tick == UNSET {
        let add = t
            .env
            .random
            .next_int_between_inclusive(WEATHERING_TICKS.0, WEATHERING_TICKS.1);
        golem_mut(t.mob()).next_weathering_tick = now + i64::from(add);
        return;
    }
    let oxidized = g.weather == Weather::Oxidized;
    if now >= g.next_weathering_tick && !oxidized {
        let new = g.weather.next();
        let next = if new == Weather::Oxidized {
            0
        } else {
            let add = t
                .env
                .random
                .next_int_between_inclusive(WEATHERING_TICKS.0, WEATHERING_TICKS.1);
            g.next_weathering_tick + i64::from(add)
        };
        let gm = golem_mut(t.mob());
        gm.weather = new;
        gm.next_weathering_tick = next;
        t.core_mut().mark_data_dirty();
    }
    if oxidized && can_turn_to_statue(t) {
        turn_to_statue(t);
    }
}

/// `canTurnToStatue`: standing in air, 0.58 % per tick.
fn can_turn_to_statue(t: &mut Tick<'_, '_>) -> bool {
    let pos = t.block_pos();
    t.level.block(pos).is_air() && t.env.random.next_float() <= TURN_TO_STATUE_CHANCE
}

/// `turnToStatue`: an oxidized statue in a random pose facing the golem's way; the golem is
/// gone (the statue's block entity does not keep the golem yet).
fn turn_to_statue(t: &mut Tick<'_, '_>) {
    let pos = t.block_pos();
    let Some(block) = vanillite_data::registry::BLOCK.id("minecraft:oxidized_copper_golem_statue")
    else {
        return;
    };
    let mut state = vanillite_data::block::BlockId(block).default_state();
    const POSES: [&str; 4] = ["standing", "sitting", "running", "star"];
    let pose = POSES[usize::try_from(t.mob().random.next_int_bounded(4)).unwrap_or(0)];
    if let Some(s) = state.with("copper_golem_pose", pose) {
        state = s;
    }
    let facing = facing_of(t.core().rot().y_rot);
    if let Some(s) = state.with("facing", facing) {
        state = s;
    }
    t.cx.actions
        .push(WorldAction::SetBlock(pos, state, crate::level::update::ALL));
    // `dropPreservedEquipment`.
    let pos_v = t.pos();
    for slot in crate::mob::equipment::SLOTS {
        let m = t.mob();
        if m.equipment.drop_chance(slot) > 1.0 && !m.equipment.get(slot).is_empty() {
            let stack = std::mem::take(&mut m.equipment.items[usize::from(slot.ordinal())]);
            crate::mob::equipment::spawn_at_location(t, stack, 0.0);
        }
    }
    let _ = pos_v;
    super::make_sound(t, "minecraft:entity.copper_golem_become_statue");
    let m = t.mob();
    m.removed = true;
    m.dead = true;
}

/// `Direction.fromYRot`.
fn facing_of(y_rot: f32) -> &'static str {
    let i = ((f64::from(y_rot) / 90.0 + 0.5).floor() as i32).rem_euclid(4);
    match i {
        0 => "south",
        1 => "west",
        2 => "north",
        _ => "east",
    }
}

/// `customServerAiStep`: the brain.
pub fn custom_server_ai_step(t: &mut Tick<'_, '_>) {
    super::ai::tick(t);
}

/// `actuallyHurt`: the golem goes idle.
pub fn after_hurt(h: &mut Hurt<'_, '_>) {
    if let Some(m) = h.ents.mob_mut(h.target) {
        golem_mut(m).state = GolemState::Idle;
    }
    if let Some(mut e) = h.ents.store.get_mut(h.target) {
        e.mark_data_dirty();
    }
}

/// `CopperGolem.mobInteract`: taking its item, shearing the antenna, waxing, scraping.
pub fn interact(ix: &mut Interact<'_, '_>) -> Interaction {
    let Some(m) = ix.ents.mob(ix.target) else {
        return Interaction::Pass;
    };
    let held = m.equipment.get(EquipmentSlot::MainHand).clone();
    let g = golem(m);
    let pos = ix.ents.store.get(ix.target).map(|e| e.pos());
    let Some(pos) = pos else {
        return Interaction::Pass;
    };
    if ix.stack.is_empty() && !held.is_empty() {
        let player_pos = ix.ents.store.get(ix.player).map_or(pos, |e| e.pos());
        if let Some(m) = ix.ents.mob_mut(ix.target) {
            m.equipment
                .set(EquipmentSlot::MainHand, ItemStack::default());
        }
        super::allay::throw_item(
            ix.ents,
            ix.env,
            ix.target,
            held,
            player_pos,
            vanillite_base::Vec3::new(0.3, 0.3, 0.3),
            0.3,
        );
        return Interaction::Success;
    }
    let name = crate::item::item_id(ix.stack).name();
    let in_tag = |tag: &str| {
        crate::player::play::game_data().is_some_and(|d| d.item_in_tag(ix.stack.item, tag))
    };
    let block = vanillite_base::BlockPos::containing(pos.x, pos.y, pos.z);
    if name == "minecraft:shears" && ready_for_shearing(m) {
        let antenna = ix
            .ents
            .mob_mut(ix.target)
            .map(|m| {
                std::mem::take(&mut m.equipment.items[usize::from(EquipmentSlot::Saddle.ordinal())])
            })
            .unwrap_or_default();
        crate::mob::combat::mob_sound(
            ix.ents,
            ix.env,
            ix.target,
            "minecraft:entity.copper_golem.shear",
        );
        drop_above(ix, pos, antenna);
        crate::mob::animal::entity_game_event(
            ix.ents,
            ix.env,
            ix.target,
            crate::level::game_event::events().shear,
            Some(ix.player),
        );
        animal::hurt_held(ix.ents, ix.env, ix.player, ix.hand, 1);
        return Interaction::Success;
    }
    if name == "minecraft:honeycomb" && g.next_weathering_tick != WAXED {
        level_event(ix, 3003, block);
        block_sound(ix, block, "minecraft:item.honeycomb.wax_on");
        if let Some(m) = ix.ents.mob_mut(ix.target) {
            golem_mut(m).next_weathering_tick = WAXED;
        }
        animal::use_item(ix.ents, ix.player, ix.hand);
        return Interaction::Success;
    }
    if in_tag("minecraft:axes") && g.next_weathering_tick == WAXED {
        crate::mob::combat::mob_sound(ix.ents, ix.env, ix.target, "minecraft:item.axe.scrape");
        level_event(ix, 3004, block);
        if let Some(m) = ix.ents.mob_mut(ix.target) {
            golem_mut(m).next_weathering_tick = UNSET;
        }
        animal::hurt_held(ix.ents, ix.env, ix.player, ix.hand, 1);
        return Interaction::Success;
    }
    if in_tag("minecraft:axes") && g.weather != Weather::Unaffected {
        crate::mob::combat::mob_sound(ix.ents, ix.env, ix.target, "minecraft:item.axe.scrape");
        level_event(ix, 3005, block);
        if let Some(m) = ix.ents.mob_mut(ix.target) {
            let gm = golem_mut(m);
            gm.next_weathering_tick = UNSET;
            gm.weather = g.weather.previous();
        }
        if let Some(mut e) = ix.ents.store.get_mut(ix.target) {
            e.mark_data_dirty();
        }
        animal::hurt_held(ix.ents, ix.env, ix.player, ix.hand, 1);
        return Interaction::Success;
    }
    Interaction::Pass
}

/// `readyForShearing`: the antenna slot holds `#shearable_from_copper_golem`.
fn ready_for_shearing(m: &MobData) -> bool {
    let s = m.equipment.get(EquipmentSlot::Saddle);
    !s.is_empty()
        && crate::player::play::game_data()
            .is_some_and(|d| d.item_in_tag(s.item, "minecraft:shearable_from_copper_golem"))
}

fn drop_above(ix: &mut Interact<'_, '_>, pos: vanillite_base::Vec3, stack: ItemStack) {
    if stack.is_empty() {
        return;
    }
    let (uuid, y_rot, velocity) = crate::mob::new_item_entity();
    ix.env.out.push(crate::survival::Effect::DropItem {
        uuid,
        pos: vanillite_base::Vec3::new(pos.x, pos.y + 1.5, pos.z),
        velocity,
        y_rot,
        pickup_delay: 10,
        stack,
    });
}

fn level_event(ix: &mut Interact<'_, '_>, event: i32, pos: BlockPos) {
    ix.cx.actions.push(WorldAction::LevelEvent(event, pos, 0));
}

fn block_sound(ix: &mut Interact<'_, '_>, pos: BlockPos, sound: &str) {
    let seed = crate::survival::sound_seed();
    crate::survival::play_sound(
        ix.env.out,
        None,
        vanillite_base::Vec3::new(
            f64::from(pos.x) + 0.5,
            f64::from(pos.y) + 0.5,
            f64::from(pos.z) + 0.5,
        ),
        sound,
        vanillite_protocol::packets::play::clientbound::SoundSource::Blocks,
        1.0,
        1.0,
        seed,
    );
}

/// The synched data: weather state 16, golem state 17.
pub fn encode(w: &mut DataWriter<'_>, m: &MobData) {
    let g = golem(m);
    w.varint(16, serializer::WEATHERING_COPPER_STATE, g.weather as i32, 0);
    w.varint(17, serializer::COPPER_GOLEM_STATE, g.state as i32, 0);
}

pub fn save(m: &MobData, out: &mut Compound) {
    let g = golem(m);
    out.insert("next_weather_age", g.next_weathering_tick);
    out.insert("weather_state", g.weather.name());
    super::ai::save_brain(m, out);
}

pub fn load(m: &mut MobData, tag: &Compound) {
    let next = super::long(tag, "next_weather_age").unwrap_or(UNSET);
    let weather = tag
        .get("weather_state")
        .and_then(Tag::as_str)
        .map_or(Weather::Unaffected, Weather::of);
    let g = golem_mut(m);
    g.next_weathering_tick = next;
    g.weather = weather;
    super::ai::load_brain(m, tag);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mob::Kind;

    #[test]
    fn weather_steps() {
        assert_eq!(Weather::Unaffected.next(), Weather::Exposed);
        assert_eq!(Weather::Oxidized.next(), Weather::Oxidized);
        assert_eq!(Weather::Exposed.previous(), Weather::Unaffected);
        assert_eq!(Weather::of("weathered"), Weather::Weathered);
    }

    #[test]
    fn golem_round_trip_and_persistence() {
        let mut m = MobData::new(Kind::CopperGolem, 1);
        assert!(m.persistent);
        assert_eq!(golem(&m).next_weathering_tick, UNSET);
        let g = golem_mut(&mut m);
        g.weather = Weather::Weathered;
        g.next_weathering_tick = WAXED;
        let mut out = Compound::new();
        save(&m, &mut out);
        let mut n = MobData::new(Kind::CopperGolem, 2);
        load(&mut n, &out);
        assert_eq!(golem(&n).weather, Weather::Weathered);
        assert_eq!(golem(&n).next_weathering_tick, WAXED);
    }

    #[test]
    fn statue_facing() {
        assert_eq!(facing_of(0.0), "south");
        assert_eq!(facing_of(90.0), "west");
        assert_eq!(facing_of(180.0), "north");
        assert_eq!(facing_of(-90.0), "east");
    }
}

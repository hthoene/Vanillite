//! Resolving entity selectors (Vanilla's `EntitySelector.findEntities`). Only players exist as
//! entities so far, so every selector selects players. Players of other worlds are known by
//! name, UUID and game mode only; selectors with position options are limited to the source's
//! world like in Vanilla.

use vanillite_base::Vec3;
use vanillite_command::selector::{AdvancementCheck, EntitySelector, Filter, Order};
use vanillite_game::entities::Entities;
use vanillite_world::entity::EntityId;

use super::source::CommandSource;
use crate::global::Global;
use crate::world::WorldId;

/// A selected player.
#[derive(Clone, Debug)]
pub(crate) struct Target {
    /// UUID.
    pub(crate) uuid: u128,
    /// Name.
    pub(crate) name: String,
    /// The world the player is in.
    pub(crate) world: WorldId,
    /// The entity, when the player is in the source's world.
    pub(crate) entity: Option<EntityId>,
    /// Position and rotation, when the player is in the source's world.
    pub(crate) place: Option<(Vec3, f32, f32)>,
    /// Game mode.
    pub(crate) game_mode: vanillite_config::GameMode,
}

/// All online players in player list order; those in this world with their entity.
pub(crate) fn online_players(entities: &Entities, global: &Global, world: WorldId) -> Vec<Target> {
    let local: Vec<(u128, EntityId, Vec3, f32, f32)> = entities
        .players
        .iter()
        .filter_map(|(id, _)| {
            let e = entities.store.get(id)?;
            Some((e.uuid(), id, e.pos(), e.rot().y_rot, e.rot().x_rot))
        })
        .collect();
    global
        .players
        .snapshot()
        .into_iter()
        .map(|p| {
            let here = local
                .iter()
                .find(|l| l.0 == p.uuid.0)
                .filter(|_| p.world == world);
            Target {
                uuid: p.uuid.0,
                name: p.name,
                world: p.world,
                entity: here.map(|l| l.1),
                place: here.map(|l| (l.2, l.3, l.4)),
                game_mode: p.info.game_mode,
            }
        })
        .collect()
}

/// `Mth.wrapDegrees`.
fn wrap_degrees(v: f64) -> f64 {
    let w = v % 360.0;
    if w >= 180.0 {
        w - 360.0
    } else if w < -180.0 {
        w + 360.0
    } else {
        w
    }
}

fn rotation_matches(range: &vanillite_command::bounds::FloatRange, rot: f32) -> bool {
    let min = wrap_degrees(f64::from(range.min.unwrap_or(0.0)));
    let max = wrap_degrees(f64::from(range.max.unwrap_or(359.0)));
    let rot = wrap_degrees(f64::from(rot));
    if min > max {
        rot >= min || rot <= max
    } else {
        rot >= min && rot <= max
    }
}

/// Whether a player passes the selector's filters. `game_mode` is the player's mode.
fn matches(selector: &EntitySelector, target: &Target, origin: Vec3) -> bool {
    if let Some(range) = &selector.distance {
        let Some((pos, _, _)) = target.place else {
            return false;
        };
        let d = pos - origin;
        if !range.matches((d.x * d.x + d.y * d.y + d.z * d.z).sqrt()) {
            return false;
        }
    }
    if selector.delta.iter().any(Option::is_some) {
        let Some((pos, _, _)) = target.place else {
            return false;
        };
        let [dx, dy, dz] = selector.delta.map(|d| d.unwrap_or(0.0));
        let (min, max) = (
            Vec3::new(
                origin.x.min(origin.x + dx),
                origin.y.min(origin.y + dy),
                origin.z.min(origin.z + dz),
            ),
            Vec3::new(
                origin.x.max(origin.x + dx) + 1.0,
                origin.y.max(origin.y + dy) + 1.0,
                origin.z.max(origin.z + dz) + 1.0,
            ),
        );
        let half = 0.3;
        let inside = pos.x + half > min.x
            && pos.x - half < max.x
            && pos.y + 1.8 > min.y
            && pos.y < max.y
            && pos.z + half > min.z
            && pos.z - half < max.z;
        if !inside {
            return false;
        }
    }
    if let Some(range) = &selector.level
        && !range.matches(0)
    {
        return false;
    }
    if let Some(range) = &selector.x_rotation
        && !target
            .place
            .is_some_and(|(_, _, x)| rotation_matches(range, x))
    {
        return false;
    }
    if let Some(range) = &selector.y_rotation
        && !target
            .place
            .is_some_and(|(_, y, _)| rotation_matches(range, y))
    {
        return false;
    }
    selector.filters.iter().all(|filter| match filter {
        Filter::Alive => true,
        Filter::Name { name, inverted } => (target.name == *name) != *inverted,
        Filter::GameMode { mode, inverted } => {
            (super::exec::config_game_mode(*mode) == target.game_mode) != *inverted
        }
        // No teams yet: everyone is in no team.
        Filter::Team { team, inverted } => team.is_empty() != *inverted,
        Filter::Type { id, inverted } => (id == "minecraft:player") != *inverted,
        Filter::TypeTag { inverted, .. } => *inverted,
        // No entity tags yet.
        Filter::Tag { tag, inverted } => tag.is_empty() != *inverted,
        // Player NBT is not serialised yet: only `{}` matches.
        Filter::Nbt { nbt, inverted } => nbt.is_empty() != *inverted,
        // No scoreboard yet: no objective exists.
        Filter::Scores(scores) => scores.is_empty(),
        // No advancements yet: nothing is done.
        Filter::Advancements(checks) => checks.iter().all(|(_, check)| match check {
            AdvancementCheck::Done(done) => !done,
            AdvancementCheck::Criteria(criteria) => criteria.iter().all(|(_, done)| !done),
        }),
        // No predicates yet.
        Filter::Predicate { inverted, .. } => *inverted,
    })
}

/// `EntitySelector.findEntities` for players. The caller checks counts and the selector
/// permission.
pub(crate) fn select(
    selector: &EntitySelector,
    source: &CommandSource,
    entities: &Entities,
    global: &Global,
) -> Vec<Target> {
    let players = online_players(entities, global, source.world);
    if let Some(name) = &selector.player_name {
        return players
            .into_iter()
            .filter(|p| p.name.eq_ignore_ascii_case(name))
            .take(1)
            .collect();
    }
    if let Some(uuid) = selector.uuid {
        return players
            .into_iter()
            .filter(|p| p.uuid == uuid)
            .take(1)
            .collect();
    }
    let origin = Vec3::new(
        selector.position[0].unwrap_or(source.pos.x),
        selector.position[1].unwrap_or(source.pos.y),
        selector.position[2].unwrap_or(source.pos.z),
    );
    if selector.current_entity {
        let Some(own) = source.uuid() else {
            return Vec::new();
        };
        return players
            .into_iter()
            .filter(|p| p.uuid == own)
            .filter(|p| matches(selector, p, origin))
            .collect();
    }
    let mut found: Vec<Target> = players
        .into_iter()
        .filter(|p| !selector.world_limited || p.world == source.world)
        .filter(|p| matches(selector, p, origin))
        .collect();
    let distance = |t: &Target| {
        t.place.map_or(f64::INFINITY, |(pos, _, _)| {
            let d = pos - origin;
            d.x * d.x + d.y * d.y + d.z * d.z
        })
    };
    match selector.order {
        Order::Arbitrary => {}
        Order::Nearest => found.sort_by(|a, b| distance(a).total_cmp(&distance(b))),
        Order::Furthest => found.sort_by(|a, b| distance(b).total_cmp(&distance(a))),
        Order::Random => shuffle(&mut found),
    }
    found.truncate(usize::try_from(selector.max_results).unwrap_or(usize::MAX));
    found
}

/// A Fisher-Yates shuffle with a time-seeded xorshift (`@r` needs no cryptographic quality).
fn shuffle<T>(items: &mut [T]) {
    let mut state = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0x9E37_79B9, |d| d.subsec_nanos() as u64 | 1);
    for i in (1..items.len()).rev() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let j = (state % (i as u64 + 1)) as usize;
        items.swap(i, j);
    }
}

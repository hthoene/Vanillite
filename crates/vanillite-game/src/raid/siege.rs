//! Zombie sieges (`VillageSiege`): at midnight one night in ten, twenty zombies come to a
//! village a player is in, from 32 blocks away.

use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Vec3};
use vanillite_chunk::HeightmapKind;

use crate::level::Level;
use crate::mob::fam::SpawnReason;
use crate::mob::spawn::{self, SpawnTable};
use crate::mob::{Environment, Kind, MobData};
use crate::survival::rules::Difficulty;

/// `VillageSiege.State`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SiegeState {
    SiegeTonight,
    #[default]
    SiegeDone,
}

/// The state of a world's `VillageSiege`.
#[derive(Clone, Copy, Debug, Default)]
pub struct VillageSiege {
    pub has_setup_siege: bool,
    pub state: SiegeState,
    pub zombies_to_spawn: i32,
    pub next_spawn_time: i32,
    pub spawn: BlockPos,
}

/// What the siege needs of this tick.
#[derive(Debug)]
pub struct SiegeInput<'a> {
    pub spawn_enemies: bool,
    pub difficulty: Difficulty,
    pub env: Environment,
    /// The day time is at the `roll_village_siege` marker (18000 of the overworld day).
    pub at_roll_marker: bool,
    pub table: Option<&'a SpawnTable>,
}

/// `VillageSiege.tick`.
pub fn tick(level: &mut Level, siege: &mut VillageSiege, input: &SiegeInput<'_>) {
    if input.env.is_bright_outside() || !input.spawn_enemies {
        siege.state = SiegeState::SiegeDone;
        siege.has_setup_siege = false;
        return;
    }
    if input.at_roll_marker {
        siege.state = if level.random().next_int_bounded(10) == 0 {
            SiegeState::SiegeTonight
        } else {
            SiegeState::SiegeDone
        };
    }
    if siege.state == SiegeState::SiegeDone {
        return;
    }
    if !siege.has_setup_siege {
        if !try_to_setup(level, siege, input) {
            return;
        }
        siege.has_setup_siege = true;
    }
    if siege.next_spawn_time > 0 {
        siege.next_spawn_time -= 1;
        return;
    }
    siege.next_spawn_time = 2;
    if siege.zombies_to_spawn > 0 {
        try_spawn(level, siege, input);
        siege.zombies_to_spawn -= 1;
    } else {
        siege.state = SiegeState::SiegeDone;
    }
}

/// `tryToSetupSiege`: the first non-spectator player in a village (whose biome has sieges).
fn try_to_setup(level: &mut Level, siege: &mut VillageSiege, input: &SiegeInput<'_>) -> bool {
    let players: Vec<Vec3> = level
        .entities
        .players
        .iter()
        .filter(|(_, p)| p.game_mode != vanillite_config::GameMode::Spectator)
        .filter_map(|(id, _)| level.entities.store.get(id).map(|e| e.pos()))
        .collect();
    for p in players {
        let center = BlockPos::containing(p.x, p.y, p.z);
        let no_sieges = input
            .table
            .and_then(|t| t.biome(level.biome(center)))
            .is_some_and(|b| b.is("minecraft:without_zombie_sieges"));
        if !level.entities.poi.is_village(center) || no_sieges {
            continue;
        }
        for _ in 0..10 {
            let angle = level.random().next_float() * std::f32::consts::TAU;
            let x = center.x
                + vanillite_base::math::floor(f64::from(
                    vanillite_base::math::cos(f64::from(angle)) * 32.0,
                ));
            let z = center.z
                + vanillite_base::math::floor(f64::from(
                    vanillite_base::math::sin(f64::from(angle)) * 32.0,
                ));
            siege.spawn = BlockPos::new(x, center.y, z);
            if find_random_spawn_pos(level, siege.spawn, input).is_some() {
                siege.next_spawn_time = 0;
                siege.zombies_to_spawn = 20;
                break;
            }
        }
        return true;
    }
    false
}

/// `trySpawn`: a zombie finalized with `EVENT` at the siege point.
fn try_spawn(level: &mut Level, siege: &VillageSiege, input: &SiegeInput<'_>) {
    let Some(pos) = find_random_spawn_pos(level, siege.spawn, input) else {
        return;
    };
    let mut zombie = MobData::new(Kind::Zombie, crate::mob::new_entity_seed());
    // `finalizeSpawn` runs before the zombie is placed (at the origin).
    let extra = spawn::with_level_random(level, |level, random| {
        spawn::finalize(
            level,
            input.table,
            &input.env,
            &mut zombie,
            BlockPos::ZERO,
            input.difficulty,
            0,
            SpawnReason::Event,
            random,
        )
    });
    let y_rot = level.random().next_float() * 360.0;
    spawn::add_mob_with(level, zombie, pos, y_rot, extra);
}

/// `findRandomSpawnPos`: up to ten tries within 8 blocks, in the village, where a zombie may
/// spawn.
fn find_random_spawn_pos(level: &mut Level, pos: BlockPos, input: &SiegeInput<'_>) -> Option<Vec3> {
    for _ in 0..10 {
        let r = level.random();
        let x = pos.x + r.next_int_bounded(16) - 8;
        let z = pos.z + r.next_int_bounded(16) - 8;
        let y = spawn::surface_y(level, HeightmapKind::WorldSurface, x, z);
        let at = BlockPos::new(x, y, z);
        if !level.entities.poi.is_village(at) {
            continue;
        }
        let ok = spawn::with_level_random(level, |level, random| {
            crate::mob::fam::check_spawn_rules(
                Kind::Zombie,
                level,
                &input.env,
                at,
                random,
                SpawnReason::Event,
            )
        });
        if ok {
            return Some(Vec3::new(
                f64::from(x) + 0.5,
                f64::from(y),
                f64::from(z) + 0.5,
            ));
        }
    }
    None
}

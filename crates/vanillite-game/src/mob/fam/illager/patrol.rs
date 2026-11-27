//! Pillager patrols (`PatrolSpawner`): every ten to eleven minutes of daylight a patrol may
//! appear 24 to 47 blocks from a random player.

use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, ChunkPos, Vec3};
use vanillite_chunk::HeightmapKind;
use vanillite_data::entity::ids;

use super::super::super::kinds::Kind;
use super::super::super::spawn::{self, SpawnTable};
use super::super::super::{Environment, MobData};
use super::super::SpawnReason;
use super::st_mut;
use crate::level::Level;
use crate::survival::rules::Difficulty;

/// The state of the patrol spawner of a world.
#[derive(Clone, Copy, Debug, Default)]
pub struct PatrolSpawner {
    pub next_tick: i32,
}

/// What the patrol spawner needs to know about the world this tick.
pub struct PatrolInput<'a> {
    /// `spawnEnemies` and the `spawn_patrols` game rule.
    pub enabled: bool,
    pub difficulty: Difficulty,
    pub env: Environment,
    /// The players: position and whether they are spectators.
    pub players: &'a [(Vec3, bool)],
    pub table: Option<&'a SpawnTable>,
    /// `EnvironmentAttributes.CAN_PILLAGER_PATROL_SPAWN` at a position.
    pub can_patrol_spawn: &'a dyn Fn(&Level, BlockPos) -> bool,
}

impl std::fmt::Debug for PatrolInput<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PatrolInput")
            .field("enabled", &self.enabled)
            .finish_non_exhaustive()
    }
}

/// `PatrolSpawner.tick`; returns the number of pillagers spawned.
pub fn patrol_spawner_tick(
    level: &mut Level,
    spawner: &mut PatrolSpawner,
    input: &PatrolInput<'_>,
) -> u32 {
    if !input.enabled {
        return 0;
    }
    spawner.next_tick -= 1;
    if spawner.next_tick > 0 {
        return 0;
    }
    spawner.next_tick += 12000 + level.random().next_int_bounded(1200);
    if !input.env.is_bright_outside() {
        return 0;
    }
    if level.random().next_int_bounded(5) != 0 {
        return 0;
    }
    let n = i32::try_from(input.players.len()).unwrap_or(i32::MAX);
    if n < 1 {
        return 0;
    }
    let pick = usize::try_from(level.random().next_int_bounded(n)).unwrap_or(0);
    let (player, spectator) = input.players[pick];
    if spectator {
        return 0;
    }
    let pp = BlockPos::containing(player.x, player.y, player.z);
    // `isCloseToVillage(pos, 2)`.
    if level.entities.poi.is_close_to_village(pp, 2) {
        return 0;
    }
    let r = level.random();
    let x = (24 + r.next_int_bounded(24)) * if r.next_boolean() { -1 } else { 1 };
    let z = (24 + r.next_int_bounded(24)) * if r.next_boolean() { -1 } else { 1 };
    let mut spawn_pos = BlockPos::new(pp.x + x, pp.y, pp.z + z);
    let loaded = (((spawn_pos.x - 10) >> 4)..=((spawn_pos.x + 10) >> 4)).all(|cx| {
        (((spawn_pos.z - 10) >> 4)..=((spawn_pos.z + 10) >> 4))
            .all(|cz| level.chunks.chunk(ChunkPos::new(cx, cz)).is_some())
    });
    if !loaded || !(input.can_patrol_spawn)(level, spawn_pos) {
        return 0;
    }
    #[allow(clippy::cast_possible_truncation)]
    let group = crate::level::difficulty::at(level, spawn_pos)
        .effective
        .ceil() as i32
        + 1;
    let mut spawned = 0;
    for i in 0..group {
        let y = spawn::surface_y(
            level,
            HeightmapKind::MotionBlockingNoLeaves,
            spawn_pos.x,
            spawn_pos.z,
        );
        spawn_pos = BlockPos::new(spawn_pos.x, y, spawn_pos.z);
        let ok = spawn_member(level, input, spawn_pos, i == 0);
        if ok {
            spawned += 1;
        } else if i == 0 {
            break;
        }
        let r = level.random();
        let dx = r.next_int_bounded(5) - r.next_int_bounded(5);
        let dz = r.next_int_bounded(5) - r.next_int_bounded(5);
        spawn_pos = BlockPos::new(spawn_pos.x + dx, spawn_pos.y, spawn_pos.z + dz);
    }
    spawned
}

/// `spawnPatrolMember`.
fn spawn_member(level: &mut Level, input: &PatrolInput<'_>, pos: BlockPos, leader: bool) -> bool {
    let state = level.block(pos);
    if !spawn::valid_empty_spawn_block(state, ids::PILLAGER) {
        return false;
    }
    let mut random = vanillite_base::random::LegacyRandom::new(level.random().next_long());
    if !super::super::check_spawn_rules(
        Kind::Pillager,
        level,
        &input.env,
        pos,
        &mut random,
        SpawnReason::Patrol,
    ) {
        return false;
    }
    let seed = crate::mob::new_entity_seed();
    let mut mob = MobData::new(Kind::Pillager, seed);
    if leader {
        // `setPatrolLeader(true)` and `findPatrolTarget()` before the position is set: the
        // target is picked around the origin, as Vanilla does.
        let r = &mut mob.random;
        let (dx, dz) = (
            -500 + r.next_int_bounded(1000),
            -500 + r.next_int_bounded(1000),
        );
        let s = st_mut(&mut mob);
        s.patrol_leader = true;
        s.patrolling = true;
        s.patrol_target = Some(BlockPos::new(dx, 0, dz));
    }
    let at = Vec3::new(f64::from(pos.x), f64::from(pos.y), f64::from(pos.z));
    let extra = spawn::with_level_random(level, |level, random| {
        spawn::finalize(
            level,
            input.table,
            &input.env,
            &mut mob,
            pos,
            input.difficulty,
            0,
            SpawnReason::Patrol,
            random,
        )
    });
    let y_rot = level.random().next_float() * 360.0;
    spawn::add_mob_with(level, mob, at, y_rot, extra, SpawnReason::Patrol);
    true
}

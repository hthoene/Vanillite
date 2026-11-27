//! What blocks do when they break besides their loot: `spawnAfterBreak` (experience of ores,
//! spawners and the sculk family, silverfish from infested blocks), `IceBlock.playerDestroy`
//! (water stays) and the `playerWillDestroy` of unstable TNT and piston heads.

use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Vec3};
use vanillite_data::block::{BlockState, ids};
use vanillite_worldgen::behaviour::{PosExt, StateExt};

use crate::item::ItemStack;
use crate::level::Level;

/// The experience a block drops when it breaks (`DropExperienceBlock.xpRange` and the like).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Experience {
    /// `UniformInt.of(min, max)`.
    Uniform(i32, i32),
    /// `ConstantInt.of(value)`.
    Constant(i32),
    /// `SpawnerBlock`: `15 + nextInt(15) + nextInt(15)`, not changed by enchantments.
    Spawner,
}

fn experience(state: BlockState) -> Option<Experience> {
    use Experience::{Constant, Spawner, Uniform};
    let b = state.block();
    Some(if b == ids::COAL_ORE || b == ids::DEEPSLATE_COAL_ORE {
        Uniform(0, 2)
    } else if b == ids::NETHER_GOLD_ORE {
        Uniform(0, 1)
    } else if b == ids::LAPIS_ORE || b == ids::DEEPSLATE_LAPIS_ORE || b == ids::NETHER_QUARTZ_ORE {
        Uniform(2, 5)
    } else if b == ids::DIAMOND_ORE
        || b == ids::DEEPSLATE_DIAMOND_ORE
        || b == ids::EMERALD_ORE
        || b == ids::DEEPSLATE_EMERALD_ORE
    {
        Uniform(3, 7)
    } else if b == ids::REDSTONE_ORE || b == ids::DEEPSLATE_REDSTONE_ORE {
        Uniform(1, 5)
    } else if b == ids::SCULK {
        Constant(1)
    } else if b == ids::SCULK_SENSOR
        || b == ids::CALIBRATED_SCULK_SENSOR
        || b == ids::SCULK_SHRIEKER
        || b == ids::SCULK_CATALYST
    {
        Constant(5)
    } else if b == ids::SPAWNER {
        Spawner
    } else {
        // Gold, iron and copper ores drop `ConstantInt.of(0)`: nothing.
        return None;
    })
}

/// `InfestedBlock`.
fn is_infested(state: BlockState) -> bool {
    state
        .block()
        .name()
        .trim_start_matches("minecraft:")
        .starts_with("infested_")
}

/// `BlockState.spawnAfterBreak(level, pos, tool, dropExperience)`.
pub fn spawn_after_break(
    level: &mut Level,
    state: BlockState,
    pos: BlockPos,
    tool: &ItemStack,
    drop_experience: bool,
) {
    if is_infested(state) {
        // `InfestedBlock.spawnAfterBreak`: experience does not matter.
        if level.settings.block_drops
            && !has_enchantment_tag(tool, "minecraft:prevents_infested_spawns")
        {
            spawn_infestation(level, pos);
        }
        return;
    }
    if !drop_experience {
        return;
    }
    let Some(xp) = experience(state) else {
        return;
    };
    let amount = match xp {
        Experience::Spawner => {
            let r = level.random();
            15 + r.next_int_bounded(15) + r.next_int_bounded(15)
        }
        Experience::Uniform(min, max) => {
            let sample = level.random().next_int_bounded(max - min + 1) + min;
            process_block_experience(level, tool, sample)
        }
        Experience::Constant(v) => process_block_experience(level, tool, v),
    };
    if amount > 0 {
        pop_experience(level, pos, amount);
    }
}

/// `EnchantmentHelper.processBlockExperience`: silk touch sets it to 0.
fn process_block_experience(level: &mut Level, tool: &ItemStack, amount: i32) -> i32 {
    let Some(data) = crate::player::play::game_data() else {
        return amount;
    };
    data.enchants.block_experience(tool, level.random(), amount)
}

/// `Block.popExperience`.
fn pop_experience(level: &mut Level, pos: BlockPos, amount: i32) {
    if level.settings.block_drops {
        let at = Vec3::new(
            f64::from(pos.x) + 0.5,
            f64::from(pos.y) + 0.5,
            f64::from(pos.z) + 0.5,
        );
        crate::experience_orb::award(level, at, amount);
    }
}

/// `EnchantmentHelper.hasTag(stack, tag)`.
fn has_enchantment_tag(stack: &ItemStack, tag: &str) -> bool {
    let Some(data) = crate::player::play::game_data() else {
        return false;
    };
    let Some(members) = data
        .registry_tags
        .get("minecraft:enchantment")
        .and_then(|t| t.get(tag))
    else {
        return false;
    };
    crate::item::enchantments(stack)
        .into_iter()
        .any(|(e, _)| members.contains(&e))
}

/// `InfestedBlock.spawnInfestation`: a silverfish at the bottom centre of the block, facing
/// south, without `finalizeSpawn`.
fn spawn_infestation(level: &mut Level, pos: BlockPos) {
    let kind = crate::mob::Kind::Silverfish;
    let mob = crate::mob::MobData::new(kind, crate::mob::new_entity_seed());
    let at = Vec3::new(
        f64::from(pos.x) + 0.5,
        f64::from(pos.y),
        f64::from(pos.z) + 0.5,
    );
    crate::mob::spawn::add_mob(level, mob, at, 0.0, crate::mob::fam::SpawnReason::Triggered);
}

/// `IceBlock.playerDestroy` after the drops: without a melting-preventing enchantment, the ice
/// leaves water above a block in `#ice_melts_when_destroyed_above` or a liquid (nothing where
/// water evaporates).
pub fn ice_player_destroy(level: &mut Level, state: BlockState, pos: BlockPos, tool: &ItemStack) {
    if !state.is(ids::ICE) || has_enchantment_tag(tool, "minecraft:prevents_ice_melting") {
        return;
    }
    if level.settings.water_evaporates {
        level.remove_block(pos, false);
        return;
    }
    let below = level.block(pos.below());
    if super::place::tags::ICE_MELTS_WHEN_DESTROYED_ABOVE.contains(below.block())
        || super::is_liquid(below)
        || below.is(ids::BUBBLE_COLUMN)
    {
        level.set_block_and_update(pos, ids::WATER.default_state());
    }
}

/// The `playerWillDestroy` of TNT and piston heads (`creative` is `instabuild` /
/// `preventsBlockDrops`).
pub fn player_will_destroy(level: &mut Level, state: BlockState, pos: BlockPos, creative: bool) {
    if state.is(ids::TNT) {
        // `TntBlock.playerWillDestroy`: unstable TNT primes without a source.
        if !creative && state.flag("unstable") {
            super::tnt::prime(level, pos, None);
        }
    } else if state.is(ids::PISTON_HEAD) {
        super::piston::head_player_will_destroy(level, pos, state, creative);
    }
}

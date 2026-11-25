//! Fire (`FireBlock`, `SoulFireBlock`, `BaseFireBlock`): the scheduled fire tick that ages,
//! burns out and spreads fire, and lava setting its surroundings on fire (`LavaFluid.randomTick`).
//! Placement, survival and shape updates of fire are shared block behaviour
//! (`vanillite_worldgen::behaviour`); the portal check of `onPlace` is in [`super::portal`].

use std::sync::OnceLock;

use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Direction};
use vanillite_config::GameMode;
use vanillite_data::block::{BLOCK_COUNT, BlockId, BlockState, flags, ids};
use vanillite_data::fluid::FluidState;
use vanillite_worldgen::behaviour::{PosExt, StateExt, fire_state_with_age, tags};

use crate::level::{Level, update};

/// Which `#infiniburn_*` tag a dimension's `infiniburn` names: blocks fire burns on forever.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Infiniburn {
    #[default]
    Overworld,
    Nether,
    End,
}

impl Infiniburn {
    /// The dimension type's `infiniburn` tag (`#minecraft:infiniburn_overworld`, ...).
    pub fn from_tag(tag: &str) -> Infiniburn {
        match tag.trim_start_matches('#') {
            "minecraft:infiniburn_nether" => Infiniburn::Nether,
            "minecraft:infiniburn_end" => Infiniburn::End,
            _ => Infiniburn::Overworld,
        }
    }

    fn contains(self, state: BlockState) -> bool {
        let blocks = &tags().blocks;
        match self {
            Infiniburn::Overworld => blocks.infiniburn_overworld.contains(state),
            Infiniburn::Nether => blocks.infiniburn_nether.contains(state),
            Infiniburn::End => blocks.infiniburn_end.contains(state),
        }
    }
}

/// `FireBlock.bootStrap`: ignite and burn odds by block name.
const ODDS: &[(&str, u8, u8)] = &[
    ("oak_planks", 5, 20),
    ("spruce_planks", 5, 20),
    ("birch_planks", 5, 20),
    ("jungle_planks", 5, 20),
    ("acacia_planks", 5, 20),
    ("cherry_planks", 5, 20),
    ("dark_oak_planks", 5, 20),
    ("pale_oak_planks", 5, 20),
    ("poplar_planks", 5, 20),
    ("mangrove_planks", 5, 20),
    ("bamboo_planks", 5, 20),
    ("bamboo_mosaic", 5, 20),
    ("oak_slab", 5, 20),
    ("spruce_slab", 5, 20),
    ("birch_slab", 5, 20),
    ("jungle_slab", 5, 20),
    ("acacia_slab", 5, 20),
    ("cherry_slab", 5, 20),
    ("dark_oak_slab", 5, 20),
    ("pale_oak_slab", 5, 20),
    ("poplar_slab", 5, 20),
    ("mangrove_slab", 5, 20),
    ("bamboo_slab", 5, 20),
    ("bamboo_mosaic_slab", 5, 20),
    ("oak_fence_gate", 5, 20),
    ("spruce_fence_gate", 5, 20),
    ("birch_fence_gate", 5, 20),
    ("jungle_fence_gate", 5, 20),
    ("acacia_fence_gate", 5, 20),
    ("cherry_fence_gate", 5, 20),
    ("dark_oak_fence_gate", 5, 20),
    ("pale_oak_fence_gate", 5, 20),
    ("poplar_fence_gate", 5, 20),
    ("mangrove_fence_gate", 5, 20),
    ("bamboo_fence_gate", 5, 20),
    ("oak_fence", 5, 20),
    ("spruce_fence", 5, 20),
    ("birch_fence", 5, 20),
    ("jungle_fence", 5, 20),
    ("acacia_fence", 5, 20),
    ("cherry_fence", 5, 20),
    ("dark_oak_fence", 5, 20),
    ("pale_oak_fence", 5, 20),
    ("poplar_fence", 5, 20),
    ("mangrove_fence", 5, 20),
    ("bamboo_fence", 5, 20),
    ("oak_stairs", 5, 20),
    ("birch_stairs", 5, 20),
    ("spruce_stairs", 5, 20),
    ("jungle_stairs", 5, 20),
    ("acacia_stairs", 5, 20),
    ("cherry_stairs", 5, 20),
    ("dark_oak_stairs", 5, 20),
    ("pale_oak_stairs", 5, 20),
    ("poplar_stairs", 5, 20),
    ("mangrove_stairs", 5, 20),
    ("bamboo_stairs", 5, 20),
    ("bamboo_mosaic_stairs", 5, 20),
    ("oak_log", 5, 5),
    ("spruce_log", 5, 5),
    ("birch_log", 5, 5),
    ("jungle_log", 5, 5),
    ("acacia_log", 5, 5),
    ("cherry_log", 5, 5),
    ("pale_oak_log", 5, 5),
    ("poplar_log", 5, 5),
    ("dark_oak_log", 5, 5),
    ("mangrove_log", 5, 5),
    ("bamboo_block", 5, 5),
    ("stripped_oak_log", 5, 5),
    ("stripped_spruce_log", 5, 5),
    ("stripped_birch_log", 5, 5),
    ("stripped_jungle_log", 5, 5),
    ("stripped_acacia_log", 5, 5),
    ("stripped_cherry_log", 5, 5),
    ("stripped_dark_oak_log", 5, 5),
    ("stripped_pale_oak_log", 5, 5),
    ("stripped_poplar_log", 5, 5),
    ("stripped_mangrove_log", 5, 5),
    ("stripped_bamboo_block", 5, 5),
    ("stripped_oak_wood", 5, 5),
    ("stripped_spruce_wood", 5, 5),
    ("stripped_birch_wood", 5, 5),
    ("stripped_jungle_wood", 5, 5),
    ("stripped_acacia_wood", 5, 5),
    ("stripped_cherry_wood", 5, 5),
    ("stripped_dark_oak_wood", 5, 5),
    ("stripped_pale_oak_wood", 5, 5),
    ("stripped_poplar_wood", 5, 5),
    ("stripped_mangrove_wood", 5, 5),
    ("oak_wood", 5, 5),
    ("spruce_wood", 5, 5),
    ("birch_wood", 5, 5),
    ("jungle_wood", 5, 5),
    ("acacia_wood", 5, 5),
    ("cherry_wood", 5, 5),
    ("pale_oak_wood", 5, 5),
    ("poplar_wood", 5, 5),
    ("dark_oak_wood", 5, 5),
    ("mangrove_wood", 5, 5),
    ("mangrove_roots", 5, 20),
    ("oak_leaves", 30, 60),
    ("spruce_leaves", 30, 60),
    ("birch_leaves", 30, 60),
    ("jungle_leaves", 30, 60),
    ("acacia_leaves", 30, 60),
    ("cherry_leaves", 30, 60),
    ("dark_oak_leaves", 30, 60),
    ("pale_oak_leaves", 30, 60),
    ("mangrove_leaves", 30, 60),
    ("red_poplar_leaves", 30, 60),
    ("orange_poplar_leaves", 30, 60),
    ("yellow_poplar_leaves", 30, 60),
    ("bookshelf", 30, 20),
    ("tnt", 15, 100),
    ("short_grass", 60, 100),
    ("fern", 60, 100),
    ("dead_bush", 60, 100),
    ("short_dry_grass", 60, 100),
    ("tall_dry_grass", 60, 100),
    ("sunflower", 60, 100),
    ("lilac", 60, 100),
    ("rose_bush", 60, 100),
    ("peony", 60, 100),
    ("red_shrub", 60, 100),
    ("tall_grass", 60, 100),
    ("large_fern", 60, 100),
    ("dandelion", 60, 100),
    ("golden_dandelion", 60, 100),
    ("poppy", 60, 100),
    ("open_eyeblossom", 60, 100),
    ("closed_eyeblossom", 60, 100),
    ("blue_orchid", 60, 100),
    ("allium", 60, 100),
    ("azure_bluet", 60, 100),
    ("red_tulip", 60, 100),
    ("orange_tulip", 60, 100),
    ("white_tulip", 60, 100),
    ("pink_tulip", 60, 100),
    ("oxeye_daisy", 60, 100),
    ("cornflower", 60, 100),
    ("lily_of_the_valley", 60, 100),
    ("torchflower", 60, 100),
    ("pitcher_plant", 60, 100),
    ("wither_rose", 60, 100),
    ("pink_petals", 60, 100),
    ("wildflowers", 60, 100),
    ("leaf_litter", 60, 100),
    ("cactus_flower", 60, 100),
    ("vine", 15, 100),
    ("coal_block", 5, 5),
    ("hay_block", 60, 20),
    ("target", 15, 20),
    ("pale_moss_block", 5, 100),
    ("pale_moss_carpet", 5, 100),
    ("pale_hanging_moss", 5, 100),
    ("dried_kelp_block", 30, 60),
    ("bamboo", 60, 60),
    ("scaffolding", 60, 60),
    ("lectern", 30, 20),
    ("composter", 5, 20),
    ("sweet_berry_bush", 60, 100),
    ("beehive", 5, 20),
    ("bee_nest", 30, 20),
    ("azalea_leaves", 30, 60),
    ("flowering_azalea_leaves", 30, 60),
    ("cave_vines", 15, 60),
    ("cave_vines_plant", 15, 60),
    ("spore_blossom", 60, 100),
    ("azalea", 30, 60),
    ("flowering_azalea", 30, 60),
    ("big_dripleaf", 60, 100),
    ("big_dripleaf_stem", 60, 100),
    ("small_dripleaf", 60, 100),
    ("hanging_roots", 30, 60),
    ("glow_lichen", 15, 100),
    ("firefly_bush", 60, 100),
    ("bush", 60, 100),
    ("acacia_shelf", 30, 20),
    ("bamboo_shelf", 30, 20),
    ("birch_shelf", 30, 20),
    ("cherry_shelf", 30, 20),
    ("dark_oak_shelf", 30, 20),
    ("jungle_shelf", 30, 20),
    ("mangrove_shelf", 30, 20),
    ("poplar_shelf", 30, 20),
    ("oak_shelf", 30, 20),
    ("pale_oak_shelf", 30, 20),
    ("spruce_shelf", 30, 20),
    ("straw_bed", 30, 60),
];

/// The dye colours of `Blocks.WOOL`, `WOOL_STAIRS`, `WOOL_SLAB` and `CARPET`.
const COLORS: [&str; 16] = [
    "white",
    "orange",
    "magenta",
    "light_blue",
    "yellow",
    "lime",
    "pink",
    "gray",
    "light_gray",
    "cyan",
    "purple",
    "blue",
    "brown",
    "green",
    "red",
    "black",
];

/// Ignite and burn odds by block id.
fn odds_table() -> &'static [(u8, u8)] {
    static TABLE: OnceLock<Box<[(u8, u8)]>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = vec![(0, 0); BLOCK_COUNT].into_boxed_slice();
        for &(name, ignite, burn) in ODDS {
            if let Some(block) = BlockId::by_name(&format!("minecraft:{name}")) {
                table[usize::from(block.0)] = (ignite, burn);
            }
        }
        // The colour loops at the end of `bootStrap`.
        for color in COLORS {
            for (suffix, ignite, burn) in [
                ("wool", 30, 60),
                ("wool_stairs", 30, 60),
                ("wool_slab", 30, 60),
                ("carpet", 60, 20),
            ] {
                if let Some(block) = BlockId::by_name(&format!("minecraft:{color}_{suffix}")) {
                    table[usize::from(block.0)] = (ignite, burn);
                }
            }
        }
        table
    })
}

fn waterlogged(state: BlockState) -> bool {
    state.value("waterlogged") == Some("true")
}

/// `FireBlock.getBurnOdds(state)`.
fn burn_odds(state: BlockState) -> i32 {
    if waterlogged(state) {
        0
    } else {
        i32::from(odds_table()[usize::from(state.block().0)].1)
    }
}

/// `FireBlock.getIgniteOdds(state)`.
fn ignite_odds(state: BlockState) -> i32 {
    if waterlogged(state) {
        0
    } else {
        i32::from(odds_table()[usize::from(state.block().0)].0)
    }
}

/// `FireBlock.canBurn`.
fn can_burn(state: BlockState) -> bool {
    ignite_odds(state) > 0
}

/// `FireBlock.getFireTickDelay`.
fn fire_tick_delay(level: &mut Level) -> i32 {
    30 + level.random().next_int_bounded(10)
}

/// `ServerLevel.canSpreadFireAround`: the `fire_spread_radius_around_player` rule, -1 for
/// everywhere, else a player (not a spectator) closer than the radius.
pub fn can_spread_fire_around(level: &Level, pos: BlockPos) -> bool {
    let radius = level.settings.fire_spread_radius;
    if radius == -1 {
        return true;
    }
    let (x, y, z) = (f64::from(pos.x), f64::from(pos.y), f64::from(pos.z));
    level.entities.players.iter().any(|(id, player)| {
        player.game_mode != GameMode::Spectator
            && level.entities.store.get(id).is_some_and(|e| {
                let p = e.pos();
                ((p.x - x).powi(2) + (p.y - y).powi(2) + (p.z - z).powi(2)).sqrt()
                    < f64::from(radius)
            })
    })
}

/// `FireBlock.isNearRain`.
fn is_near_rain(level: &Level, pos: BlockPos) -> bool {
    level.is_raining_at(pos)
        || level.is_raining_at(pos.relative(Direction::West))
        || level.is_raining_at(pos.relative(Direction::East))
        || level.is_raining_at(pos.relative(Direction::North))
        || level.is_raining_at(pos.relative(Direction::South))
}

/// `FireBlock.isValidFireLocation`.
fn is_valid_fire_location(level: &Level, pos: BlockPos) -> bool {
    Direction::ALL
        .into_iter()
        .any(|d| can_burn(level.block(pos.relative(d))))
}

/// `FireBlock.getIgniteOdds(level, pos)`: the best odds of the neighbours of an empty block.
fn ignite_odds_at(level: &Level, pos: BlockPos) -> i32 {
    if !level.block(pos).is_air() {
        return 0;
    }
    Direction::ALL
        .into_iter()
        .map(|d| ignite_odds(level.block(pos.relative(d))))
        .fold(0, i32::max)
}

fn sturdy_below(level: &Level, pos: BlockPos) -> bool {
    super::is_face_sturdy(level.block(pos.below()), Direction::Up)
}

/// `FireBlock.tick`.
pub(super) fn tick(level: &mut Level, mut state: BlockState, pos: BlockPos) {
    let delay = fire_tick_delay(level);
    level.schedule_block_tick(pos, ids::FIRE, delay);
    if !can_spread_fire_around(level, pos) {
        return;
    }
    if !level.can_survive(state, pos) {
        level.remove_block(pos, false);
    }
    let below = level.block(pos.below());
    let infiniburn = level.settings.infiniburn.contains(below);
    let age = state.int("age").unwrap_or(0);
    if !infiniburn
        && level.is_raining()
        && is_near_rain(level, pos)
        && level.random().next_float() < 0.2 + age as f32 * 0.03
    {
        level.remove_block(pos, false);
        return;
    }
    let new_age = (age + level.random().next_int_bounded(3) / 2).min(15);
    if age != new_age {
        state = state.set_int("age", new_age);
        level.set_block(
            pos,
            state,
            update::SKIP_BLOCK_ENTITY_SIDEEFFECTS | update::INVISIBLE,
        );
    }
    if !infiniburn {
        if !is_valid_fire_location(level, pos) {
            if !sturdy_below(level, pos) || age > 3 {
                level.remove_block(pos, false);
            }
            return;
        }
        if age == 15
            && level.random().next_int_bounded(4) == 0
            && !can_burn(level.block(pos.below()))
        {
            level.remove_block(pos, false);
            return;
        }
    }
    // `EnvironmentAttributes.INCREASED_FIRE_BURNOUT`: no dimension of the Vanilla pack sets it.
    let increased_burnout = false;
    let extra = if increased_burnout { -50 } else { 0 };
    check_burn_out(level, pos.relative(Direction::East), 300 + extra, age);
    check_burn_out(level, pos.relative(Direction::West), 300 + extra, age);
    check_burn_out(level, pos.below(), 250 + extra, age);
    check_burn_out(level, pos.above(), 250 + extra, age);
    check_burn_out(level, pos.relative(Direction::North), 300 + extra, age);
    check_burn_out(level, pos.relative(Direction::South), 300 + extra, age);
    let difficulty = level.settings.difficulty.id();
    for xx in -1..=1 {
        for zz in -1..=1 {
            for yy in -1..=4 {
                if xx == 0 && yy == 0 && zz == 0 {
                    continue;
                }
                let rate = if yy > 1 { 100 + (yy - 1) * 100 } else { 100 };
                let test = pos.offset(xx, yy, zz);
                let ignite = ignite_odds_at(level, test);
                if ignite <= 0 {
                    continue;
                }
                let mut odds = (ignite + 40 + difficulty * 7) / (age + 30);
                if increased_burnout {
                    odds /= 2;
                }
                if odds > 0
                    && level.random().next_int_bounded(rate) <= odds
                    && (!level.is_raining() || !is_near_rain(level, test))
                {
                    let spread_age = (age + level.random().next_int_bounded(5) / 4).min(15);
                    let fire = fire_state_with_age(&*level, test, spread_age);
                    level.set_block_and_update(test, fire);
                }
            }
        }
    }
}

/// `FireBlock.checkBurnOut`: the block at `pos` burns away or catches fire; TNT primes.
fn check_burn_out(level: &mut Level, pos: BlockPos, chance: i32, age: i32) {
    let old = level.block(pos);
    let odds = burn_odds(old);
    if level.random().next_int_bounded(chance) >= odds {
        return;
    }
    if level.random().next_int_bounded(age + 10) < 5 && !level.is_raining_at(pos) {
        let new_age = (age + level.random().next_int_bounded(5) / 4).min(15);
        let fire = fire_state_with_age(&*level, pos, new_age);
        level.set_block_and_update(pos, fire);
    } else {
        level.remove_block(pos, false);
    }
    if old.is(ids::TNT) {
        super::tnt::prime(level, pos, None);
    }
}

/// `LavaFluid.randomTick`: lava sets flammable blocks around and above it on fire.
pub fn lava_random_tick(level: &mut Level, pos: BlockPos, _fluid: FluidState) {
    if !can_spread_fire_around(level, pos) {
        return;
    }
    let passes = level.random().next_int_bounded(3);
    if passes > 0 {
        let mut test = pos;
        for _ in 0..passes {
            let dx = level.random().next_int_bounded(3) - 1;
            let dz = level.random().next_int_bounded(3) - 1;
            test = test.offset(dx, 1, dz);
            if !lava_loaded(level, test) {
                return;
            }
            let state = level.block(test);
            if state.is_air() {
                if Direction::ALL
                    .into_iter()
                    .any(|d| ignited_by_lava(level, test.relative(d)))
                {
                    let fire = base_fire_state(level, test);
                    level.set_block_and_update(test, fire);
                    return;
                }
            } else if tags().blocks.blocks_lava_fire_spread.contains(state) {
                return;
            }
        }
    } else {
        for _ in 0..3 {
            let dx = level.random().next_int_bounded(3) - 1;
            let dz = level.random().next_int_bounded(3) - 1;
            let test = pos.offset(dx, 0, dz);
            if !lava_loaded(level, test) {
                return;
            }
            if level.block(test.above()).is_air() && ignited_by_lava(level, test) {
                // Vanilla computes the fire for `test`, not for the block above it.
                let fire = base_fire_state(level, test);
                level.set_block_and_update(test.above(), fire);
            }
        }
    }
}

/// `Level.isLoaded(pos)`: inside the build height and in a loaded chunk.
fn lava_loaded(level: &Level, pos: BlockPos) -> bool {
    level.in_height(pos.y) && level.is_loaded(pos)
}

/// `LavaFluid.isFlammable`.
fn ignited_by_lava(level: &Level, pos: BlockPos) -> bool {
    if level.in_height(pos.y) && !level.is_loaded(pos) {
        return false;
    }
    level.block(pos).has(flags::IGNITED_BY_LAVA)
}

/// `BaseFireBlock.getState(level, pos)`.
fn base_fire_state(level: &Level, pos: BlockPos) -> BlockState {
    let age = 0;
    fire_state_with_age(level, pos, age)
}

//! End and nether features: islands, spikes, gateways, platforms, deltas, basalt columns.

mod chorus;
mod column;
mod delta;
mod spike;

use std::sync::LazyLock;

use serde_json::Value;
use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, ChunkPos};
use vanillite_chunk::HeightmapKind;
use vanillite_data::block::{BlockId, BlockState, ids};
use vanillite_nbt::Tag;

pub use chorus::ChorusPlantFeature;
pub use column::SteppedColumnClusterFeature;
pub use delta::DeltaFeature;
pub use spike::{EndSpike, EndSpikeFeature, spikes_for_seed};

use super::vegetation::util::{HORIZONTAL, parse_block_pos, shuffle, with};
use crate::Error;
use crate::behaviour::can_survive;
use crate::feature::region::WorldGenRegion;
use crate::feature::{FeatureContext, FeatureParser, FeatureType, bool_or, field};

/// `BuiltInLootTables.SPAWN_BONUS_CHEST`.
const SPAWN_BONUS_CHEST: &str = "minecraft:chests/spawn_bonus_chest";

/// `bonus_chest`: a chest with the spawn bonus loot table and torches on a random free
/// surface position of the chunk.
#[derive(Debug)]
pub struct BonusChestFeature;

impl FeatureType for BonusChestFeature {
    fn parse(_json: &Value, _parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        Ok(Self)
    }

    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        let chunk = origin.chunk();
        let mut xs: [i32; 16] = std::array::from_fn(|i| chunk.min_block_x() + i as i32);
        shuffle(&mut xs, ctx.random);
        let mut zs: [i32; 16] = std::array::from_fn(|i| chunk.min_block_z() + i as i32);
        shuffle(&mut zs, ctx.random);
        for x in xs {
            for z in zs {
                let chest = ctx.level.heightmap_pos(
                    HeightmapKind::MotionBlockingNoLeaves,
                    BlockPos::new(x, 0, z),
                );
                let state = ctx.level.block(chest);
                if !(state.is_air() || state.collision_shape().is_empty()) {
                    continue;
                }
                ctx.level.set_block(chest, ids::CHEST.default_state(), 2);
                // `RandomizableContainer.setBlockEntityLootTable`: the seed is drawn only when
                // the chest's block entity exists.
                if let Some(entity) = ctx.level.block_entity_mut(chest) {
                    let seed = ctx.random.next_long();
                    entity.insert("LootTable", Tag::String(SPAWN_BONUS_CHEST.into()));
                    if seed != 0 {
                        entity.insert("LootTableSeed", Tag::Long(seed));
                    }
                }
                let torch = ids::TORCH.default_state();
                for direction in HORIZONTAL {
                    let pos = chest.relative(direction);
                    if can_survive(ctx.level, torch, pos) {
                        ctx.level.set_block(pos, torch, 2);
                    }
                }
                return true;
            }
        }
        false
    }
}

/// `end_gateway`: a gateway block in a bedrock frame, optionally with a known exit.
#[derive(Debug)]
pub struct EndGatewayFeature {
    exit: Option<BlockPos>,
    exact: bool,
}

impl FeatureType for EndGatewayFeature {
    fn parse(json: &Value, _parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        Ok(Self {
            exit: json.get("exit").map(parse_block_pos).transpose()?,
            exact: field(json, "exact")?
                .as_bool()
                .ok_or_else(|| Error::new("exact is not a boolean"))?,
        })
    }

    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        let air = BlockState::AIR;
        let bedrock = ids::BEDROCK.default_state();
        // `BlockPos.betweenClosed`: x fastest, then y, then z.
        for z in origin.z - 1..=origin.z + 1 {
            for y in origin.y - 2..=origin.y + 2 {
                for x in origin.x - 1..=origin.x + 1 {
                    let pos = BlockPos::new(x, y, z);
                    let same_x = x == origin.x;
                    let same_y = y == origin.y;
                    let same_z = z == origin.z;
                    let end = (y - origin.y).abs() == 2;
                    let state = if same_x && same_y && same_z {
                        ctx.level
                            .set_block_and_update(pos, ids::END_GATEWAY.default_state());
                        if let Some(exit) = self.exit {
                            self.set_exit(ctx.level, pos, exit);
                        }
                        continue;
                    } else if same_y {
                        air
                    } else if (end && same_x && same_z) || ((same_x || same_z) && !end) {
                        bedrock
                    } else {
                        air
                    };
                    ctx.level.set_block_and_update(pos, state);
                }
            }
        }
        true
    }
}

impl EndGatewayFeature {
    /// `TheEndGatewayBlockEntity.setExitPosition(exit, exact)` as the block entity saves it.
    fn set_exit(&self, level: &mut WorldGenRegion<'_>, pos: BlockPos, exit: BlockPos) {
        if let Some(entity) = level.block_entity_mut(pos) {
            entity.insert("exit_portal", Tag::IntArray(vec![exit.x, exit.y, exit.z]));
            if self.exact {
                entity.insert("ExactTeleport", Tag::Byte(1));
            } else {
                entity.remove("ExactTeleport");
            }
        }
    }
}

/// `end_island`: a small floating cone of end stone.
#[derive(Debug)]
pub struct EndIslandFeature;

impl FeatureType for EndIslandFeature {
    fn parse(_json: &Value, _parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        Ok(Self)
    }

    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        let end_stone = ids::END_STONE.default_state();
        let mut size = ctx.random.next_int_bounded(3) as f32 + 4.0;
        let mut y = 0;
        while size > 0.5 {
            let from = (-size).floor() as i32;
            let to = size.ceil() as i32;
            for x in from..=to {
                for z in from..=to {
                    if ((x * x + z * z) as f32) <= (size + 1.0) * (size + 1.0) {
                        ctx.level
                            .set_block_and_update(origin.offset(x, y, z), end_stone);
                    }
                }
            }
            size -= ctx.random.next_int_bounded(2) as f32 + 0.5;
            y -= 1;
        }
        true
    }
}

/// `end_platform`: the obsidian platform players arrive on in the End.
#[derive(Debug)]
pub struct EndPlatformFeature;

impl FeatureType for EndPlatformFeature {
    fn parse(_json: &Value, _parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        Ok(Self)
    }

    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        // `createEndPlatform(level, origin, false)`.
        for dz in -2..=2 {
            for dx in -2..=2 {
                for dy in -1..3 {
                    let pos = origin.offset(dx, dy, dz);
                    let block = if dy == -1 { ids::OBSIDIAN } else { ids::AIR };
                    if ctx.level.block(pos).block() != block {
                        ctx.level.set_block_and_update(pos, block.default_state());
                    }
                }
            }
        }
        true
    }
}

/// The wall torches around the podium pillar, by horizontal direction (`HORIZONTAL` order).
static PODIUM_TORCHES: LazyLock<[BlockState; 4]> = LazyLock::new(|| {
    let torch = ids::WALL_TORCH.default_state();
    [
        with(torch, "facing", "north"),
        with(torch, "facing", "east"),
        with(torch, "facing", "south"),
        with(torch, "facing", "west"),
    ]
});

/// `end_podium`: the exit portal frame; `active` fills it with end portal blocks.
#[derive(Debug)]
pub struct EndPodiumFeature {
    active: bool,
}

impl FeatureType for EndPodiumFeature {
    fn parse(json: &Value, _parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        Ok(Self {
            active: bool_or(json, "active", false),
        })
    }

    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        let bedrock = ids::BEDROCK.default_state();
        // `BlockPos.betweenClosed`: x fastest, then y, then z.
        for z in origin.z - 4..=origin.z + 4 {
            for y in origin.y - 1..=origin.y + 32 {
                for x in origin.x - 4..=origin.x + 4 {
                    let pos = BlockPos::new(x, y, z);
                    let dist = pos.dist_sqr(origin);
                    let inside_rim = dist < 2.5 * 2.5;
                    if !inside_rim && dist >= 3.5 * 3.5 {
                        continue;
                    }
                    if y < origin.y {
                        if inside_rim {
                            ctx.level.set_block_and_update(pos, bedrock);
                        } else {
                            self.set_or_drop(ctx.level, pos, ids::END_STONE);
                        }
                    } else if y > origin.y {
                        self.set_or_drop(ctx.level, pos, ids::AIR);
                    } else if !inside_rim {
                        ctx.level.set_block_and_update(pos, bedrock);
                    } else if self.active {
                        drop_previous_and_set_block(ctx.level, pos, ids::END_PORTAL);
                    } else {
                        ctx.level.set_block_and_update(pos, BlockState::AIR);
                    }
                }
            }
        }
        for y in 0..4 {
            ctx.level
                .set_block_and_update(origin.offset(0, y, 0), bedrock);
        }
        let pillar_center = origin.offset(0, 2, 0);
        for (direction, &torch) in HORIZONTAL.iter().zip(PODIUM_TORCHES.iter()) {
            ctx.level
                .set_block_and_update(pillar_center.relative(*direction), torch);
        }
        true
    }
}

impl EndPodiumFeature {
    /// `dropPreviousAndSetBlock` when active, else a plain `setBlock`.
    fn set_or_drop(&self, level: &mut WorldGenRegion<'_>, pos: BlockPos, block: BlockId) {
        if self.active {
            drop_previous_and_set_block(level, pos, block);
        } else {
            level.set_block_and_update(pos, block.default_state());
        }
    }
}

/// `EndPodiumFeature.dropPreviousAndSetBlock`: item drops are entities and are left out.
fn drop_previous_and_set_block(level: &mut WorldGenRegion<'_>, pos: BlockPos, block: BlockId) {
    if level.block(pos).block() != block {
        level.destroy_block(pos);
        level.set_block_and_update(pos, block.default_state());
    }
}

/// `VoidStartPlatformFeature.PLATFORM_OFFSET`.
const PLATFORM_OFFSET: BlockPos = BlockPos::new(8, 3, 8);
/// `VoidStartPlatformFeature.PLATFORM_RADIUS`.
const PLATFORM_RADIUS: i32 = 16;

/// `void_start_platform`: the stone platform of the void superflat preset.
#[derive(Debug)]
pub struct VoidStartPlatformFeature;

impl FeatureType for VoidStartPlatformFeature {
    fn parse(_json: &Value, _parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        Ok(Self)
    }

    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        let chunk = origin.chunk();
        let platform_chunk: ChunkPos = PLATFORM_OFFSET.chunk();
        if checkerboard_distance(chunk.x, chunk.z, platform_chunk.x, platform_chunk.z) > 1 {
            return true;
        }
        let platform = BlockPos::new(
            PLATFORM_OFFSET.x,
            origin.y + PLATFORM_OFFSET.y,
            PLATFORM_OFFSET.z,
        );
        for z in chunk.min_block_z()..=chunk.min_block_z() + 15 {
            for x in chunk.min_block_x()..=chunk.min_block_x() + 15 {
                if checkerboard_distance(platform.x, platform.z, x, z) <= PLATFORM_RADIUS {
                    let pos = BlockPos::new(x, platform.y, z);
                    let block = if pos == platform {
                        ids::COBBLESTONE
                    } else {
                        ids::STONE
                    };
                    ctx.level.set_block(pos, block.default_state(), 2);
                }
            }
        }
        true
    }
}

/// `VoidStartPlatformFeature.checkerboardDistance`.
fn checkerboard_distance(xa: i32, za: i32, xb: i32, zb: i32) -> i32 {
    (xa - xb).abs().max((za - zb).abs())
}

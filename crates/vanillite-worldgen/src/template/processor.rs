//! Structure processors (`StructureProcessor`, registry `worldgen/structure_processor`) and
//! processor lists (`StructureProcessorList`, registry `worldgen/processor_list`).

use std::sync::Arc;

use rustc_hash::FxHashMap;
use serde_json::Value;
use vanillite_base::BlockPos;
use vanillite_base::random::{LegacyRandom, PositionalRandomFactory, RandomSource};
use vanillite_chunk::HeightmapKind;
use vanillite_data::block::{BlockId, BlockState, ids};
use vanillite_datapack::LoadedPacks;

use super::rule::ProcessorRule;
use super::settings::{StructurePlaceSettings, TemplateRandom};
use super::structure::{StructureBlockInfo, is_shape_full_block};
use super::{parse_state_string, qualified_str};
use crate::Error;
use crate::blocks::parse_block_state;
use crate::blocks::{BlockSet, parse_block_set, qualify};
use crate::carver::providers::IntProvider;
use crate::feature::field;
use crate::feature::region::WorldGenRegion;

/// A structure processor: changes or drops the blocks of a template before they are placed.
#[derive(Clone, Debug)]
pub enum StructureProcessor {
    /// `blackstone_replace` (`BlackstoneReplaceProcessor`).
    BlackstoneReplace,
    /// `block_age` (`BlockAgeProcessor`); the block tags it tests are resolved at parse time.
    BlockAge {
        mossiness: f32,
        stairs: BlockSet,
        slabs: BlockSet,
        walls: BlockSet,
    },
    /// `block_ignore` (`BlockIgnoreProcessor`).
    BlockIgnore(Vec<BlockId>),
    /// `block_rot` (`BlockRotProcessor`).
    BlockRot {
        rottable_blocks: Option<BlockSet>,
        integrity: f32,
    },
    /// `capped` (`CappedProcessor`): applies the delegate to at most `limit` random blocks of
    /// the whole piece.
    Capped {
        delegate: Box<StructureProcessor>,
        limit: IntProvider,
    },
    /// `gravity` (`GravityProcessor`).
    Gravity {
        heightmap: HeightmapKind,
        offset: i32,
    },
    /// `jigsaw_replacement` (`JigsawReplacementProcessor`).
    JigsawReplacement,
    /// `lava_submerged_block` (`LavaSubmergedBlockProcessor`).
    LavaSubmergedBlock,
    /// `nop` (`NopProcessor`).
    Nop,
    /// `protected_blocks` (`ProtectedBlockProcessor`).
    ProtectedBlocks(BlockSet),
    /// `rule` (`RuleProcessor`).
    Rule(Vec<ProcessorRule>),
}

/// A processor list (`StructureProcessorList`).
#[derive(Clone, Debug, Default)]
pub struct ProcessorList {
    pub processors: Vec<StructureProcessor>,
}

/// `Heightmap.Types` by serialized name.
pub fn heightmap_by_name(name: &str) -> Result<HeightmapKind, Error> {
    Ok(match name {
        "WORLD_SURFACE_WG" => HeightmapKind::WorldSurfaceWg,
        "WORLD_SURFACE" => HeightmapKind::WorldSurface,
        "OCEAN_FLOOR_WG" => HeightmapKind::OceanFloorWg,
        "OCEAN_FLOOR" => HeightmapKind::OceanFloor,
        "MOTION_BLOCKING" => HeightmapKind::MotionBlocking,
        "MOTION_BLOCKING_NO_LEAVES" => HeightmapKind::MotionBlockingNoLeaves,
        other => return Err(Error::new(format!("unknown heightmap type {other}"))),
    })
}

impl StructureProcessor {
    /// Parses a processor (`StructureProcessorType.SINGLE_CODEC`, dispatched on
    /// `processor_type`).
    pub fn parse(packs: &LoadedPacks, json: &Value) -> Result<Self, Error> {
        let kind = qualified_str(json, "processor_type")?;
        Ok(match kind.as_str() {
            "minecraft:blackstone_replace" => Self::BlackstoneReplace,
            "minecraft:block_age" => Self::BlockAge {
                mossiness: crate::feature::float_field(json, "mossiness")?,
                stairs: BlockSet::from_tag(packs, "minecraft:stairs"),
                slabs: BlockSet::from_tag(packs, "minecraft:slabs"),
                walls: BlockSet::from_tag(packs, "minecraft:walls"),
            },
            "minecraft:block_ignore" => Self::BlockIgnore(
                field(json, "blocks")?
                    .as_array()
                    .ok_or_else(|| Error::new("block_ignore blocks is not a list"))?
                    .iter()
                    .map(|s| parse_block_state(s).map(BlockState::block))
                    .collect::<Result<_, _>>()?,
            ),
            "minecraft:block_rot" => {
                let integrity = crate::feature::float_field(json, "integrity")?;
                if !(0.0..=1.0).contains(&integrity) {
                    return Err(Error::new(format!("integrity {integrity} outside [0, 1]")));
                }
                Self::BlockRot {
                    rottable_blocks: json
                        .get("rottable_blocks")
                        .map(|set| parse_block_set(packs, set))
                        .transpose()?,
                    integrity,
                }
            }
            "minecraft:capped" => Self::Capped {
                delegate: Box::new(Self::parse(packs, field(json, "delegate")?)?),
                limit: IntProvider::from_json_in_range(field(json, "limit")?, 1, i32::MAX)?,
            },
            "minecraft:gravity" => Self::Gravity {
                heightmap: match json.get("heightmap").and_then(Value::as_str) {
                    Some(name) => heightmap_by_name(name)?,
                    None => HeightmapKind::WorldSurfaceWg,
                },
                offset: crate::feature::int_or(json, "offset", 0),
            },
            "minecraft:jigsaw_replacement" => Self::JigsawReplacement,
            "minecraft:lava_submerged_block" => Self::LavaSubmergedBlock,
            "minecraft:nop" => Self::Nop,
            "minecraft:protected_blocks" => {
                Self::ProtectedBlocks(parse_block_set(packs, field(json, "value")?)?)
            }
            "minecraft:rule" => Self::Rule(
                field(json, "rules")?
                    .as_array()
                    .ok_or_else(|| Error::new("rule processor rules is not a list"))?
                    .iter()
                    .map(|r| ProcessorRule::parse(packs, r))
                    .collect::<Result<_, _>>()?,
            ),
            other => return Err(Error::new(format!("unknown structure processor {other}"))),
        })
    }

    /// `evaluatesEntirePieceState()`.
    pub fn evaluates_entire_piece_state(&self) -> bool {
        matches!(self, Self::Capped { .. })
    }

    /// `processBlock(level, targetPosition, referencePos, templateRelativePos,
    /// processedBlockInfo, settings)`; `None` drops the block. `random` is the generator passed
    /// to `placeInWorld` (what [`StructurePlaceSettings::random`] may hand out).
    #[allow(clippy::too_many_arguments)]
    pub fn process_block(
        &self,
        level: &WorldGenRegion<'_>,
        _target: BlockPos,
        reference: BlockPos,
        template_relative: BlockPos,
        info: StructureBlockInfo,
        settings: &StructurePlaceSettings<'_>,
        random: &mut dyn TemplateRandom,
    ) -> Option<StructureBlockInfo> {
        match self {
            Self::Nop | Self::Capped { .. } => Some(info),
            Self::BlackstoneReplace => Some(blackstone_replace(info)),
            Self::BlockAge {
                mossiness,
                stairs,
                slabs,
                walls,
            } => {
                let mut random = settings.random(info.pos, random);
                let age = BlockAge {
                    mossiness: *mossiness,
                    random: &mut random,
                };
                let state = info.state;
                let block = state.block();
                let new_state = if block == ids::STONE_BRICKS
                    || block == ids::STONE
                    || block == ids::CHISELED_STONE_BRICKS
                {
                    age.full_stone_block()
                } else if stairs.contains(state) {
                    age.stairs(state)
                } else if slabs.contains(state) {
                    age.slab(state)
                } else if walls.contains(state) {
                    age.wall(state)
                } else if block == ids::OBSIDIAN {
                    age.obsidian()
                } else {
                    None
                };
                Some(match new_state {
                    Some(state) => StructureBlockInfo { state, ..info },
                    None => info,
                })
            }
            Self::BlockIgnore(blocks) => (!blocks.contains(&info.state.block())).then_some(info),
            Self::BlockRot {
                rottable_blocks,
                integrity,
            } => {
                let mut random = settings.random(info.pos, random);
                let rottable = rottable_blocks
                    .as_ref()
                    .is_none_or(|set| set.contains(info.state));
                // `!(nextFloat() <= integrity)`; the integrity is never NaN.
                if rottable && random.next_float() > *integrity {
                    None
                } else {
                    Some(info)
                }
            }
            Self::Gravity { heightmap, offset } => {
                // Generation never runs on a `ServerLevel`, so the `_WG` types stay.
                let pos = info.pos;
                let height = level.height_at(*heightmap, pos.x, pos.z) + offset;
                Some(StructureBlockInfo {
                    pos: BlockPos::new(pos.x, height + template_relative.y, pos.z),
                    ..info
                })
            }
            Self::JigsawReplacement => {
                if info.state.block() != ids::JIGSAW {
                    return Some(info);
                }
                let Some(nbt) = &info.nbt else {
                    return Some(info);
                };
                let final_state = match nbt.get("final_state") {
                    Some(vanillite_nbt::Tag::String(s)) => s.as_str(),
                    _ => "minecraft:air",
                };
                let state = parse_state_string(final_state)?;
                (state.block() != ids::STRUCTURE_VOID).then_some(StructureBlockInfo {
                    pos: info.pos,
                    state,
                    nbt: None,
                })
            }
            Self::LavaSubmergedBlock => {
                let was_lava = level.block(info.pos).block() == ids::LAVA;
                if was_lava && !is_shape_full_block(info.state) {
                    Some(StructureBlockInfo {
                        state: ids::LAVA.default_state(),
                        ..info
                    })
                } else {
                    Some(info)
                }
            }
            Self::ProtectedBlocks(set) => (!set.contains(level.block(info.pos))).then_some(info),
            Self::Rule(rules) => {
                let pos = info.pos;
                let mut random =
                    LegacyRandom::new(vanillite_base::math::get_seed(pos.x, pos.y, pos.z));
                for rule in rules {
                    if rule.test(
                        level,
                        info.state,
                        template_relative,
                        pos,
                        reference,
                        &mut random,
                    ) {
                        return Some(StructureBlockInfo {
                            pos,
                            state: rule.output_state,
                            nbt: rule.block_entity_modifier.apply(&mut random, info.nbt),
                        });
                    }
                }
                Some(info)
            }
        }
    }

    /// `finalizeProcessing(level, position, referencePos, originalBlockInfoList,
    /// processedBlockInfoList, settings)`: only `capped` does anything. `originals` are the
    /// template relative positions of the processed blocks.
    #[allow(clippy::too_many_arguments)]
    pub fn finalize_processing(
        &self,
        level: &WorldGenRegion<'_>,
        position: BlockPos,
        reference: BlockPos,
        originals: &[BlockPos],
        processed: &mut [StructureBlockInfo],
        settings: &StructurePlaceSettings<'_>,
        random: &mut dyn TemplateRandom,
    ) {
        let Self::Capped { delegate, limit } = self else {
            return;
        };
        if limit.max_inclusive() == 0 || processed.is_empty() {
            return;
        }
        if originals.len() != processed.len() {
            // Vanilla logs that the lists are out of sync and skips the processing.
            return;
        }
        let mut capped_random = LegacyRandom::new(level.seed())
            .fork_positional()
            .at(position.x, position.y, position.z);
        let max_to_replace = limit.sample(&mut capped_random).min(processed.len() as i32);
        if max_to_replace < 1 {
            return;
        }
        // `Util.toShuffledList(IntStream.range(0, size), random)`.
        let mut indices: Vec<usize> = (0..processed.len()).collect();
        for i in (2..=indices.len()).rev() {
            let swap_to = capped_random.next_int_bounded(i as i32) as usize;
            indices.swap(i - 1, swap_to);
        }
        let mut replaced = 0;
        for index in indices {
            if replaced >= max_to_replace {
                break;
            }
            let original = processed[index].clone();
            let altered = delegate.process_block(
                level,
                position,
                reference,
                originals[index],
                original.clone(),
                settings,
                random,
            );
            if let Some(altered) = altered
                && !original.same_as(&altered)
            {
                replaced += 1;
                processed[index] = altered;
            }
        }
    }
}

/// The random choices of `BlockAgeProcessor`.
struct BlockAge<'a> {
    mossiness: f32,
    random: &'a mut dyn TemplateRandom,
}

impl BlockAge<'_> {
    fn full_stone_block(self) -> Option<BlockState> {
        if self.random.next_float() >= 0.5 {
            return None;
        }
        let non_mossy = [
            ids::CRACKED_STONE_BRICKS.default_state(),
            random_facing_stairs(self.random, ids::STONE_BRICK_STAIRS),
        ];
        let mossy = [
            ids::MOSSY_STONE_BRICKS.default_state(),
            random_facing_stairs(self.random, ids::MOSSY_STONE_BRICK_STAIRS),
        ];
        Some(self.pick(&non_mossy, &mossy))
    }

    fn stairs(self, state: BlockState) -> Option<BlockState> {
        if self.random.next_float() >= 0.5 {
            return None;
        }
        let non_mossy = [
            ids::STONE_SLAB.default_state(),
            ids::STONE_BRICK_SLAB.default_state(),
        ];
        let mossy = [
            with_properties_of(ids::MOSSY_STONE_BRICK_STAIRS, state),
            ids::MOSSY_STONE_BRICK_SLAB.default_state(),
        ];
        Some(self.pick(&non_mossy, &mossy))
    }

    fn slab(self, state: BlockState) -> Option<BlockState> {
        (self.random.next_float() < self.mossiness)
            .then(|| with_properties_of(ids::MOSSY_STONE_BRICK_SLAB, state))
    }

    fn wall(self, state: BlockState) -> Option<BlockState> {
        (self.random.next_float() < self.mossiness)
            .then(|| with_properties_of(ids::MOSSY_STONE_BRICK_WALL, state))
    }

    fn obsidian(self) -> Option<BlockState> {
        (self.random.next_float() < 0.15).then(|| ids::CRYING_OBSIDIAN.default_state())
    }

    /// `getRandomBlock(random, nonMossyBlocks, mossyBlocks)`.
    fn pick(self, non_mossy: &[BlockState; 2], mossy: &[BlockState; 2]) -> BlockState {
        let blocks = if self.random.next_float() < self.mossiness {
            mossy
        } else {
            non_mossy
        };
        blocks[self.random.next_int(2) as usize]
    }
}

/// `BlockAgeProcessor.getRandomFacingStairs`: a random horizontal facing, then a random half.
fn random_facing_stairs(random: &mut dyn TemplateRandom, stairs: BlockId) -> BlockState {
    const FACINGS: [&str; 4] = ["north", "east", "south", "west"];
    const HALVES: [&str; 2] = ["top", "bottom"];
    let state = stairs.default_state();
    let facing = FACINGS[random.next_int(4) as usize];
    let state = state.with("facing", facing).unwrap_or(state);
    let half = HALVES[random.next_int(2) as usize];
    state.with("half", half).unwrap_or(state)
}

/// `Block.withPropertiesOf(source)`: the block's default state with every property of `source`
/// that it also has.
pub fn with_properties_of(block: BlockId, source: BlockState) -> BlockState {
    source
        .properties()
        .fold(block.default_state(), |state, (name, value)| {
            state.with(name, value).unwrap_or(state)
        })
}

/// `BlackstoneReplaceProcessor.processBlock`.
fn blackstone_replace(info: StructureBlockInfo) -> StructureBlockInfo {
    let block = info.state.block();
    let replacement = match block {
        b if b == ids::COBBLESTONE || b == ids::MOSSY_COBBLESTONE => ids::BLACKSTONE,
        b if b == ids::STONE => ids::POLISHED_BLACKSTONE,
        b if b == ids::STONE_BRICKS || b == ids::MOSSY_STONE_BRICKS => {
            ids::POLISHED_BLACKSTONE_BRICKS
        }
        b if b == ids::COBBLESTONE_STAIRS || b == ids::MOSSY_COBBLESTONE_STAIRS => {
            ids::BLACKSTONE_STAIRS
        }
        b if b == ids::STONE_STAIRS => ids::POLISHED_BLACKSTONE_STAIRS,
        b if b == ids::STONE_BRICK_STAIRS || b == ids::MOSSY_STONE_BRICK_STAIRS => {
            ids::POLISHED_BLACKSTONE_BRICK_STAIRS
        }
        b if b == ids::COBBLESTONE_SLAB || b == ids::MOSSY_COBBLESTONE_SLAB => ids::BLACKSTONE_SLAB,
        b if b == ids::SMOOTH_STONE_SLAB || b == ids::STONE_SLAB => ids::POLISHED_BLACKSTONE_SLAB,
        b if b == ids::STONE_BRICK_SLAB || b == ids::MOSSY_STONE_BRICK_SLAB => {
            ids::POLISHED_BLACKSTONE_BRICK_SLAB
        }
        b if b == ids::STONE_BRICK_WALL || b == ids::MOSSY_STONE_BRICK_WALL => {
            ids::POLISHED_BLACKSTONE_BRICK_WALL
        }
        b if b == ids::COBBLESTONE_WALL || b == ids::MOSSY_COBBLESTONE_WALL => ids::BLACKSTONE_WALL,
        b if b == ids::CHISELED_STONE_BRICKS => ids::CHISELED_POLISHED_BLACKSTONE,
        b if b == ids::CRACKED_STONE_BRICKS => ids::CRACKED_POLISHED_BLACKSTONE_BRICKS,
        b if b == ids::IRON_BARS => ids::IRON_CHAIN,
        _ => return info,
    };
    let old = info.state;
    let mut state = replacement.default_state();
    for property in ["facing", "half", "type"] {
        if let Some(value) = old.value(property) {
            state = state.with(property, value).unwrap_or(state);
        }
    }
    StructureBlockInfo { state, ..info }
}

/// Parses and caches processor lists (`StructureProcessorType.LIST_CODEC`): a registry id of
/// `worldgen/processor_list`, `{"processors": [...]}` or a bare list. Named lists are parsed
/// once and shared.
#[derive(Debug, Default)]
pub struct ProcessorLists {
    named: FxHashMap<String, Arc<ProcessorList>>,
}

impl ProcessorLists {
    /// An empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// A `Holder<StructureProcessorList>`.
    pub fn get(&mut self, packs: &LoadedPacks, json: &Value) -> Result<Arc<ProcessorList>, Error> {
        let Some(name) = json.as_str() else {
            return Ok(Arc::new(parse_direct(packs, json)?));
        };
        let name = qualify(name);
        if let Some(list) = self.named.get(&name) {
            return Ok(Arc::clone(list));
        }
        let entry = packs
            .registry("minecraft:worldgen/processor_list")
            .and_then(|r| r.get(&name))
            .ok_or_else(|| Error::new(format!("unknown processor list {name}")))?;
        let definition: Value = serde_json::from_str(entry.json.get())
            .map_err(|e| Error::new(format!("processor list {name}: {e}")))?;
        let list = Arc::new(
            parse_direct(packs, &definition)
                .map_err(|e| Error::new(format!("processor list {name}: {e}")))?,
        );
        self.named.insert(name, Arc::clone(&list));
        Ok(list)
    }
}

/// `StructureProcessorType.DIRECT_CODEC`: `{"processors": [...]}` or `[...]`.
fn parse_direct(packs: &LoadedPacks, json: &Value) -> Result<ProcessorList, Error> {
    let items = json
        .get("processors")
        .unwrap_or(json)
        .as_array()
        .ok_or_else(|| Error::new("processor list is not a list"))?;
    Ok(ProcessorList {
        processors: items
            .iter()
            .map(|p| StructureProcessor::parse(packs, p))
            .collect::<Result<_, _>>()?,
    })
}

//! Anti-Xray (`anticheat.anti-xray`): hidden blocks that touch no transparent block are sent to
//! clients as other blocks. The masking depends only on the chunk and its neighbours, never on
//! the player, so a masked chunk packet is encoded once and shared like an unmasked one. After a
//! block change the hidden blocks around it are sent with their real state.

use std::sync::Arc;

use vanillite_base::hash::{FxHashMap, FxHashSet};
use vanillite_base::{BlockPos, ChunkPos, SectionPos};
use vanillite_chunk::{Chunk, ContainerData, PalettedContainer, Section, bits};
use vanillite_config::{AntiXrayMode, AntiXrayRules};
use vanillite_data::block::{BlockId, BlockState, STATE_COUNT, flags};
use vanillite_world::chunks::Chunks;

/// The prepared Anti-Xray settings of one world; `None` of [`AntiXray::new`] when disabled.
#[derive(Debug)]
pub struct AntiXray {
    /// The rules this was built from, to notice changes on reload.
    rules: AntiXrayRules,
    /// One bit per block state: masked when it touches no transparent block.
    masked: Box<[u64]>,
    /// The blocks sent instead: the base stone (hide) or the hidden blocks (obfuscate).
    fakes: Box<[u16]>,
    /// Sent instead below y 0 in hide mode (deepslate in the overworld).
    deep: Option<u16>,
    /// Blocks at and above this height are sent as they are.
    max_y: i32,
    /// Radius around changed blocks within which masked blocks are sent with their real state.
    radius: i32,
}

impl AntiXray {
    /// The settings of a world from its rules; `None` when the engine mode is `disabled` or no
    /// listed block exists.
    pub fn new(rules: &AntiXrayRules, dimension: &str) -> Option<AntiXray> {
        let state = |name: &str| BlockId::by_name(name).map(|b| b.default_state().0);
        let mut masked = vec![0u64; STATE_COUNT.div_ceil(64)].into_boxed_slice();
        let mut mark = |names: &[String]| {
            for name in names {
                let Some(block) = BlockId::by_name(name) else {
                    log::warn!("anticheat.anti-xray: unknown block {name:?} is ignored");
                    continue;
                };
                let info = block.info();
                for id in info.first_state..info.first_state.saturating_add(info.state_count) {
                    if let Some(word) = masked.get_mut(usize::from(id) >> 6) {
                        *word |= 1 << (id & 63);
                    }
                }
            }
        };
        let (fakes, deep) = match rules.engine_mode {
            AntiXrayMode::Disabled => return None,
            AntiXrayMode::Hide => {
                mark(&rules.hidden_blocks);
                let (base, deep) = match dimension {
                    "minecraft:the_nether" => ("minecraft:netherrack", None),
                    "minecraft:the_end" => ("minecraft:end_stone", None),
                    "minecraft:overworld" => ("minecraft:stone", Some("minecraft:deepslate")),
                    _ => ("minecraft:stone", None),
                };
                (vec![state(base)?], deep.and_then(state))
            }
            AntiXrayMode::Obfuscate => {
                mark(&rules.hidden_blocks);
                mark(&rules.replacement_blocks);
                let fakes: Vec<u16> = rules
                    .hidden_blocks
                    .iter()
                    .filter_map(|n| state(n))
                    .collect();
                (fakes, None)
            }
        };
        if fakes.is_empty() || masked.iter().all(|&w| w == 0) {
            return None;
        }
        Some(AntiXray {
            rules: rules.clone(),
            masked,
            fakes: fakes.into_boxed_slice(),
            deep,
            max_y: rules.max_block_height,
            radius: i32::from(rules.update_radius),
        })
    }

    /// Whether this was built from `rules`.
    pub fn built_from(&self, rules: &AntiXrayRules) -> bool {
        self.rules == *rules
    }

    /// Whether `state` is masked when it touches no transparent block.
    #[inline]
    fn is_masked(&self, state: u16) -> bool {
        self.masked
            .get(usize::from(state) >> 6)
            .is_some_and(|w| w & (1 << (state & 63)) != 0)
    }

    /// The sections of `chunk` as clients get them: sections with masked blocks are new,
    /// all others the chunk's own. `None` when nothing is masked. `chunks` gives the
    /// neighbouring chunks; blocks at the border of a chunk that is not loaded count as covered.
    pub fn mask(&self, chunks: &Chunks, pos: ChunkPos, chunk: &Chunk) -> Option<Vec<Arc<Section>>> {
        let neighbour = |dx: i32, dz: i32| {
            chunks
                .chunk(ChunkPos::new(
                    pos.x.wrapping_add(dx),
                    pos.z.wrapping_add(dz),
                ))
                .map(|c| &c.chunk)
        };
        let around = Around {
            west: neighbour(-1, 0),
            east: neighbour(1, 0),
            north: neighbour(0, -1),
            south: neighbour(0, 1),
        };
        let sections = chunk.sections();
        // Sections below `max-block-height` and the one above them, decoded on first use.
        let mut decoded: Vec<Option<Box<Blocks>>> = Vec::new();
        decoded.resize_with(sections.len(), || None);
        let mut values: Box<Blocks> = Box::new([0; 4096]);
        let mut changes: Vec<(usize, u16)> = Vec::new();
        // The fakes depend on the chunk position only, never on the real blocks.
        let mut random = (i64::from(pos.x) as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ (i64::from(pos.z) as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
        let mut out: Option<Vec<Arc<Section>>> = None;
        for (index, section) in sections.iter().enumerate() {
            let base_y = chunk
                .min_y()
                .saturating_add(i32::try_from(index).unwrap_or(i32::MAX).saturating_mul(16));
            if base_y >= self.max_y {
                break;
            }
            if !section.blocks().may_contain(|s| self.is_masked(s)) {
                continue;
            }
            for i in [index.wrapping_sub(1), index, index + 1] {
                if let (Some(slot), Some(section)) = (decoded.get_mut(i), sections.get(i))
                    && slot.is_none()
                {
                    *slot = Some(decode(section));
                }
            }
            let Some(Some(real)) = decoded.get(index) else {
                continue;
            };
            let below = index
                .checked_sub(1)
                .and_then(|i| decoded.get(i))
                .and_then(Option::as_deref);
            let above = decoded.get(index + 1).and_then(Option::as_deref);
            changes.clear();
            for (i, &state) in real.iter().enumerate() {
                if !self.is_masked(state) {
                    continue;
                }
                let (x, ly, z) = (i & 15, i >> 8, (i >> 4) & 15);
                let y = base_y.saturating_add(ly as i32);
                if y >= self.max_y {
                    break;
                }
                // The six neighbours: in this section, the sections below and above, or the
                // neighbouring chunks.
                let opaque =
                    |s: Option<u16>| s.is_none_or(|s| BlockState(s).has(flags::SOLID_RENDER));
                let covered = opaque(match ly {
                    0 => below.map(|b| b[i | 0xF00]),
                    _ => Some(real[i - 0x100]),
                }) && opaque(match ly {
                    15 => above.map(|a| a[i & 0xFF]),
                    _ => Some(real[i + 0x100]),
                }) && opaque(match z {
                    0 => around.north.and_then(|c| c.block(x as u32, y, 15)),
                    _ => Some(real[i - 0x10]),
                }) && opaque(match z {
                    15 => around.south.and_then(|c| c.block(x as u32, y, 0)),
                    _ => Some(real[i + 0x10]),
                }) && opaque(match x {
                    0 => around.west.and_then(|c| c.block(15, y, z as u32)),
                    _ => Some(real[i - 1]),
                }) && opaque(match x {
                    15 => around.east.and_then(|c| c.block(0, y, z as u32)),
                    _ => Some(real[i + 1]),
                });
                if !covered {
                    continue;
                }
                let fake = match (self.deep, self.fakes.len()) {
                    (Some(deep), _) if y < 0 => deep,
                    (_, 1) => self.fakes[0],
                    (_, len) => {
                        random = splitmix(random);
                        self.fakes[(random % len as u64) as usize]
                    }
                };
                changes.push((i, fake));
            }
            if changes.is_empty() {
                continue;
            }
            // Few changes (hide): set them in a copy; many (obfuscate): build the section anew.
            let masked = if changes.len() <= 256 {
                let mut copy = (**section).clone();
                for &(i, fake) in &changes {
                    copy.set_block(
                        (i & 15) as u32,
                        (i >> 8) as u32,
                        ((i >> 4) & 15) as u32,
                        fake,
                    );
                }
                Some(copy)
            } else {
                values.copy_from_slice(&real[..]);
                for &(i, fake) in &changes {
                    values[i] = fake;
                }
                encode(&values, section)
            };
            if let Some(masked) = masked {
                let sections = out.get_or_insert_with(|| sections.to_vec());
                sections[index] = Arc::new(masked);
            }
        }
        out
    }

    /// Adds the masked blocks within the update radius of `changes` to the block updates of
    /// the tick (`changed`), so clients get their real state.
    pub(crate) fn reveal(
        &self,
        chunks: &Chunks,
        changes: &[BlockPos],
        changed: &mut FxHashMap<SectionPos, Vec<BlockPos>>,
    ) {
        let mut seen: FxHashSet<BlockPos> = changed.values().flatten().copied().collect();
        let r = self.radius;
        for &center in changes {
            if center.y.saturating_sub(r) >= self.max_y {
                continue;
            }
            for dy in -r..=r {
                let y = center.y.saturating_add(dy);
                if y >= self.max_y {
                    break;
                }
                for dz in -r..=r {
                    for dx in -r..=r {
                        let pos =
                            BlockPos::new(center.x.wrapping_add(dx), y, center.z.wrapping_add(dz));
                        if chunks.block(pos).is_some_and(|s| self.is_masked(s)) && seen.insert(pos)
                        {
                            changed.entry(pos.section()).or_default().push(pos);
                        }
                    }
                }
            }
        }
    }
}

/// The four horizontal neighbours of a chunk, `None` when not loaded.
struct Around<'a> {
    west: Option<&'a Chunk>,
    east: Option<&'a Chunk>,
    north: Option<&'a Chunk>,
    south: Option<&'a Chunk>,
}

/// The block states of a section in index order (`y << 8 | z << 4 | x`).
type Blocks = [u16; 4096];

/// The block states of a section.
fn decode(section: &Section) -> Box<Blocks> {
    let mut out: Box<Blocks> = Box::new([0; 4096]);
    match section.blocks().data() {
        ContainerData::Single(value) => out.fill(*value),
        ContainerData::Indirect {
            bits,
            palette,
            data,
        } => bits::for_each(data, *bits, 4096, |i, p| {
            out[i] = palette.get(p as usize).copied().unwrap_or(0);
        }),
        ContainerData::Direct { bits, data } => {
            bits::for_each(data, *bits, 4096, |i, v| out[i] = v as u16);
        }
    }
    out
}

/// A section with the block states `values` and the biomes of `like`.
fn encode(values: &Blocks, like: &Section) -> Option<Section> {
    let mut palette: Vec<u16> = Vec::new();
    let mut index_of: FxHashMap<u16, u32> = FxHashMap::default();
    let mut indices: Box<[u32; 4096]> = Box::new([0; 4096]);
    let mut last = None;
    for (slot, &value) in indices.iter_mut().zip(values.iter()) {
        *slot = match last {
            Some((v, i)) if v == value => i,
            _ => {
                let next = palette.len() as u32;
                let i = *index_of.entry(value).or_insert_with(|| {
                    palette.push(value);
                    next
                });
                last = Some((value, i));
                i
            }
        };
    }
    let blocks = match palette.len() {
        0 | 1 => PalettedContainer::new(palette.first().copied().unwrap_or(0)),
        len @ 2..=256 => {
            let bits = (usize::BITS - (len - 1).leading_zeros()).max(4) as u8;
            let data = bits::pack(4096, bits, |i| indices[i]);
            PalettedContainer::from_parts(bits, &palette, &data).ok()?
        }
        _ => {
            let bits = vanillite_chunk::facts::block_global_bits();
            let data = bits::pack(4096, bits, |i| u32::from(values[i]));
            PalettedContainer::from_parts(bits, &[], &data).ok()?
        }
    };
    Some(Section::from_containers(blocks, like.biomes().clone()))
}

/// SplitMix64 step.
fn splitmix(state: u64) -> u64 {
    let mut z = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use vanillite_base::hash::FxHashMap;
    use vanillite_base::{BlockPos, ChunkPos};
    use vanillite_chunk::slots::{
        FlatGenerator, LightEngine, Neighbours, NullCodec, NullStore, WorldInfo,
    };
    use vanillite_chunk::{ProtoChunk, Section, wire};
    use vanillite_config::{Profile, RuleSet, WorldRules};
    use vanillite_data::block::{BlockId, BlockState};
    use vanillite_world::chunks::{ChunkSettings, ChunkSlots, Chunks, InlineExecutor};

    use crate::world_setup::ChunkPackets;

    struct NoLight;

    impl LightEngine for NoLight {
        fn initialize_light(&self, _: &mut [ProtoChunk]) {}
        fn light_chunks(&self, _: &mut [ProtoChunk], _: &mut Neighbours<'_>) {}
        fn relight(&self, _: &[ChunkPos], _: &mut Neighbours<'_>) {}
    }

    /// Full classic superflat chunks around the origin (bedrock, two dirt layers, grass).
    fn flat_chunks() -> Chunks {
        vanillite_chunk::facts::install_vanilla(&[], &[], 1);
        let info = WorldInfo {
            dimension: "minecraft:overworld".to_owned(),
            dir: std::env::temp_dir(),
            min_y: -64,
            height: 384,
            seed: 0,
            has_sky_light: true,
            biomes: Arc::from(["minecraft:plains".to_owned()]),
            default_biome: 0,
            air: BlockState::AIR.0,
        };
        let slots = ChunkSlots {
            store: Arc::new(NullStore),
            codec: Arc::new(NullCodec),
            generator: Arc::new(FlatGenerator::classic(&info)),
            light: Arc::new(NoLight),
            executor: Arc::new(InlineExecutor),
        };
        let settings = ChunkSettings {
            view_distance: 3,
            simulation_distance: 3,
            ..ChunkSettings::default()
        };
        let mut chunks = Chunks::new(info, settings, slots);
        let level = chunks.player_ticket_level();
        chunks.add_ticket(ChunkPos::new(0, 0), level);
        for _ in 0..4 {
            chunks.tick();
        }
        chunks
    }

    fn state(name: &str) -> u16 {
        BlockId::by_name(name).expect(name).default_state().0
    }

    /// The sections of the chunk packet of `pos`.
    fn sent_blocks(packets: &ChunkPackets, chunks: &Chunks, pos: ChunkPos) -> Vec<Section> {
        let block_entities = vanillite_game::level::BlockEntities::default();
        let packet = vanillite_game::player::ChunkSource::chunk_packet(
            &packets.source(chunks, &block_entities),
            pos,
        )
        .expect("chunk packet");
        let mut body = packet.body();
        wire::read_varint(&mut body).expect("id");
        wire::read_array::<8>(&mut body).expect("position");
        for _ in 0..wire::read_varint(&mut body).expect("heightmaps") {
            wire::read_varint(&mut body).expect("kind");
            let longs = wire::read_varint(&mut body).expect("length");
            for _ in 0..longs {
                wire::read_array::<8>(&mut body).expect("heightmap");
            }
        }
        wire::read_varint(&mut body).expect("data size");
        let count = chunks.chunk(pos).expect("chunk").chunk.sections().len();
        (0..count)
            .map(|_| Section::read(&mut body).expect("section"))
            .collect()
    }

    fn block_of(sections: &[Section], pos: BlockPos) -> u16 {
        let index = usize::try_from((pos.y + 64) >> 4).expect("index");
        sections[index].block(
            (pos.x & 15) as u32,
            (pos.y & 15) as u32,
            (pos.z & 15) as u32,
        )
    }

    #[test]
    fn hidden_ores_are_masked_and_revealed_after_mining_nearby() {
        let mut chunks = flat_chunks();
        let diamond = state("minecraft:diamond_ore");
        let covered = BlockPos::new(5, -63, 5);
        let near = BlockPos::new(5, -63, 7);
        let exposed = BlockPos::new(8, -61, 8);
        for pos in [covered, near, exposed] {
            chunks.set_block(pos, diamond).expect("set ore");
        }
        let mut packets = ChunkPackets::default();
        let rules = WorldRules::for_profile(Profile::Paper);
        packets.set_anti_xray(&rules.anti_xray, "minecraft:overworld");
        let mut changed = FxHashMap::default();
        packets.invalidate(&mut chunks, &[], &mut changed);

        let sent = sent_blocks(&packets, &chunks, ChunkPos::new(0, 0));
        assert_eq!(block_of(&sent, covered), state("minecraft:deepslate"));
        assert_eq!(block_of(&sent, near), state("minecraft:deepslate"));
        assert_eq!(block_of(&sent, exposed), diamond, "exposed ores stay");

        // Mine the dirt above the covered ore: the ores within the update radius get their real
        // state with the tick's block updates.
        changed.clear();
        let above = BlockPos::new(5, -62, 5);
        chunks.set_block(above, BlockState::AIR.0).expect("mine");
        packets.invalidate(&mut chunks, &[], &mut changed);
        let updates: Vec<BlockPos> = changed.values().flatten().copied().collect();
        assert!(updates.contains(&covered));
        assert!(updates.contains(&near));
        // A fresh packet shows the now exposed ore and still masks the covered one.
        let sent = sent_blocks(&packets, &chunks, ChunkPos::new(0, 0));
        assert_eq!(block_of(&sent, covered), diamond);
        assert_eq!(block_of(&sent, near), state("minecraft:deepslate"));
    }

    #[test]
    fn vanilla_profile_sends_real_blocks() {
        let mut chunks = flat_chunks();
        let diamond = state("minecraft:diamond_ore");
        let covered = BlockPos::new(5, -63, 5);
        chunks.set_block(covered, diamond).expect("set ore");
        let mut packets = ChunkPackets::default();
        let rules = WorldRules::for_profile(Profile::Vanilla);
        packets.set_anti_xray(&rules.anti_xray, "minecraft:overworld");
        let mut changed = FxHashMap::default();
        packets.invalidate(&mut chunks, &[], &mut changed);
        let sent = sent_blocks(&packets, &chunks, ChunkPos::new(0, 0));
        assert_eq!(block_of(&sent, covered), diamond);
        assert!(changed.values().flatten().all(|p| *p == covered));
    }
}

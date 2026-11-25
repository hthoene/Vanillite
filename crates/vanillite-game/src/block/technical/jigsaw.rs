//! `JigsawBlock`: the connection points of jigsaw structures and `jigsaw_generate`.

use vanillite_base::{BlockPos, Direction};
use vanillite_data::block::{BlockId, BlockState};
use vanillite_nbt::{Compound, Tag};
use vanillite_protocol::packets::play::serverbound::{JigsawGenerate, SetJigsawBlock};
use vanillite_worldgen::template::structure::jigsaw_front;

use super::Editor;
use super::structure_block::{int_or, string_or, try_parse_identifier};
use crate::block::BlockBehaviour;
use crate::level::{BlockEntity, Level};

/// `JigsawBlockEntity.EMPTY_ID` (and `Pools.EMPTY`).
const EMPTY: &str = "minecraft:empty";

/// The most levels a jigsaw block generates: the edit screen's slider ends at 20, larger
/// values from modified clients would only cost the server time.
const MAX_LEVELS: i32 = 20;

/// `JigsawBlockEntity.JointType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Joint {
    Rollable,
    Aligned,
}

impl Joint {
    fn name(self) -> &'static str {
        match self {
            Joint::Rollable => "rollable",
            Joint::Aligned => "aligned",
        }
    }

    fn by_name(name: &str) -> Option<Joint> {
        match name {
            "rollable" => Some(Joint::Rollable),
            "aligned" => Some(Joint::Aligned),
            _ => None,
        }
    }

    /// `StructureTemplate.getDefaultJointType(state)`: aligned for horizontal fronts.
    fn default_for(state: BlockState) -> Joint {
        match jigsaw_front(state) {
            Some(Direction::Up | Direction::Down) => Joint::Rollable,
            _ => Joint::Aligned,
        }
    }
}

/// `JigsawBlockEntity`.
#[derive(Clone, Debug, PartialEq)]
pub struct Jigsaw {
    /// `name`.
    pub name: String,
    /// `target`.
    pub target: String,
    /// `pool`.
    pub pool: String,
    /// `finalState`.
    pub final_state: String,
    /// `joint`.
    pub joint: Joint,
    /// `placementPriority`.
    pub placement_priority: i32,
    /// `selectionPriority`.
    pub selection_priority: i32,
    /// The joint of the block's orientation, what loading data without a joint gives.
    state_joint: Joint,
}

impl Default for Jigsaw {
    fn default() -> Self {
        Self {
            name: EMPTY.to_owned(),
            target: EMPTY.to_owned(),
            pool: EMPTY.to_owned(),
            final_state: "minecraft:air".to_owned(),
            joint: Joint::Rollable,
            placement_priority: 0,
            selection_priority: 0,
            state_joint: Joint::Rollable,
        }
    }
}

/// `Identifier.CODEC` with a default for missing or invalid values.
fn identifier_or(nbt: &Compound, key: &str) -> String {
    nbt.get(key)
        .and_then(Tag::as_str)
        .and_then(try_parse_identifier)
        .unwrap_or_else(|| EMPTY.to_owned())
}

impl Jigsaw {
    /// `new JigsawBlockEntity(pos, state)`.
    pub fn new(state: BlockState) -> Self {
        Self {
            state_joint: Joint::default_for(state),
            ..Self::default()
        }
    }

    /// `loadAdditional`.
    pub fn load(&mut self, nbt: &Compound) {
        self.name = identifier_or(nbt, "name");
        self.target = identifier_or(nbt, "target");
        self.pool = identifier_or(nbt, "pool");
        self.final_state = string_or(nbt, "final_state", "minecraft:air").to_owned();
        self.joint = nbt
            .get("joint")
            .and_then(Tag::as_str)
            .and_then(Joint::by_name)
            .unwrap_or(self.state_joint);
        self.placement_priority = int_or(nbt, "placement_priority", 0);
        self.selection_priority = int_or(nbt, "selection_priority", 0);
    }

    /// `saveAdditional`.
    pub fn save(&self, nbt: &mut Compound) {
        nbt.insert("name", Tag::String(self.name.clone()));
        nbt.insert("target", Tag::String(self.target.clone()));
        nbt.insert("pool", Tag::String(self.pool.clone()));
        nbt.insert("final_state", Tag::String(self.final_state.clone()));
        nbt.insert("joint", Tag::String(self.joint.name().to_owned()));
        nbt.insert("placement_priority", Tag::Int(self.placement_priority));
        nbt.insert("selection_priority", Tag::Int(self.selection_priority));
    }

    /// `getUpdateTag`: `saveCustomOnly`.
    pub fn update_tag(&self) -> Option<Compound> {
        let mut nbt = Compound::new();
        self.save(&mut nbt);
        nbt.sort_java_hash_order();
        Some(nbt)
    }
}

/// The behaviour of the block (rotation and mirroring are world generation's).
pub(super) fn fill(_block: BlockId, _b: &mut BlockBehaviour) {}

/// `handleSetJigsawBlock`.
pub fn handle(level: &mut Level, p: SetJigsawBlock, editor: &Editor<'_>) {
    if !editor.allowed {
        return;
    }
    let pos = BlockPos::new(p.pos.x, p.pos.y, p.pos.z);
    let Some(BlockEntity::Jigsaw(jigsaw)) = level.block_entity_mut(pos) else {
        return;
    };
    jigsaw.name = p.name.as_str().to_owned();
    jigsaw.target = p.target.as_str().to_owned();
    jigsaw.pool = p.pool.as_str().to_owned();
    jigsaw.final_state = p.final_state;
    // `JointType.CODEC.byName(name, ALIGNED)`.
    jigsaw.joint = Joint::by_name(&p.joint).unwrap_or(Joint::Aligned);
    jigsaw.placement_priority = p.placement_priority;
    jigsaw.selection_priority = p.selection_priority;
    level.block_entity_changed(pos);
    level.send_block_updated(pos);
}

/// `handleJigsawGenerate`: `JigsawBlockEntity.generate`, `JigsawPlacement.generateJigsaw` of
/// the block's pool from its target in front of it. Worlds without the noise generator and
/// pools that do not exist generate nothing.
pub fn handle_generate(level: &mut Level, p: JigsawGenerate, editor: &Editor<'_>) {
    use vanillite_worldgen::place as wgp;
    if !editor.allowed {
        return;
    }
    let pos = BlockPos::new(p.pos.x, p.pos.y, p.pos.z);
    let Some(BlockEntity::Jigsaw(jigsaw)) = level.block_entity(pos) else {
        return;
    };
    let (pool, target) = (jigsaw.pool.clone(), jigsaw.target.clone());
    let Some(front) = jigsaw_front(level.block(pos)) else {
        return;
    };
    let start = pos.relative(front);
    let (Some(packs), Some(generation)) = (level.data_packs().cloned(), level.generation.as_ref())
    else {
        return;
    };
    let generator = std::sync::Arc::clone(&generation.terrain);
    if !wgp::has_entry(&packs, "minecraft:worldgen/template_pool", &pool) {
        return;
    }
    let height = vanillite_worldgen::structure::LevelHeight {
        min_y: level.min_y(),
        height: level.max_y() - level.min_y() + 1,
    };
    let levels = p.levels.clamp(0, MAX_LEVELS);
    let pieces = match wgp::jigsaw_pieces(&generator, &packs, &pool, &target, levels, start, height)
    {
        Ok(Some(pieces)) => pieces,
        Ok(None) => return,
        Err(e) => {
            log::warn!("jigsaw generate {pool}: {e}");
            return;
        }
    };
    let mut pieces = pieces;
    let (mut min, mut max) = (start.chunk(), start.chunk());
    for piece in &pieces {
        let bb = piece.bounding_box();
        min = vanillite_base::ChunkPos::new(min.x.min(bb.min_x() >> 4), min.z.min(bb.min_z() >> 4));
        max = vanillite_base::ChunkPos::new(max.x.max(bb.max_x() >> 4), max.z.max(bb.max_z() >> 4));
    }
    // `level.getRandom()`.
    let seed = vanillite_base::random::RandomSource::next_long(level.random());
    let mut random = vanillite_base::random::DecorationRandom::new(seed);
    let mut area = crate::level::place::PlaceArea::copy(level, &generator, min, max);
    area.run(&generator, |region| {
        wgp::place_jigsaw(
            &generator,
            &mut pieces,
            region,
            &mut random,
            start,
            p.keep_jigsaws,
        );
    });
    area.apply(level);
}

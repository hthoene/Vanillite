//! Pool elements (`StructurePoolElement` and its subclasses `SinglePoolElement`,
//! `LegacySinglePoolElement`, `ListPoolElement`, `FeaturePoolElement`, `EmptyPoolElement`) and
//! the jigsaw blocks they expose to `JigsawPlacement`.

use std::cmp::Reverse;
use std::sync::Arc;

use serde_json::Value;
use vanillite_base::math::get_seed;
use vanillite_base::random::{LegacyRandom, RandomSource};
use vanillite_base::{BlockPos, Direction};
use vanillite_data::block::{BlockState, ids};
use vanillite_nbt::{Compound, List, Tag};

use super::pools::{PoolId, PoolsBuilder, Symbol};
use crate::Error;
use crate::blocks::qualify;
use crate::feature::{FeatureContext, PlacedFeatureId, field, place_placed};
use crate::structure::StructureParser;
use crate::structure::piece::PieceContext;
use crate::structure::pool::Projection;
use crate::template::structure::JointType;
use crate::template::transform::{direction_by_name, transform};
use crate::template::{
    BoundingBox, LiquidSettings, Mirror, Rotation, StructurePlaceSettings, StructureProcessor,
    StructureTemplate,
};

/// A jigsaw block of a pool element as `getShuffledJigsawBlocks` returns it
/// (`StructureTemplate.JigsawBlockInfo`): placed and rotated, with its names resolved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Jigsaw {
    /// `pos()`.
    pub pos: BlockPos,
    /// `JigsawBlock.getFrontFacing(state())`.
    pub front: Direction,
    /// `JigsawBlock.getTopFacing(state())`.
    pub top: Direction,
    /// `jointType() == ROLLABLE`.
    pub rollable: bool,
    /// `name()`: `None` only for the jigsaw of a feature element.
    pub name: Option<Symbol>,
    /// `target()`.
    pub target: Symbol,
    /// `pool()`.
    pub pool: PoolId,
    /// `placementPriority()`.
    pub placement_priority: i32,
    /// `selectionPriority()`.
    pub selection_priority: i32,
}

impl Jigsaw {
    /// `JigsawBlock.canAttach(source, target)`.
    pub fn can_attach(&self, target: &Jigsaw) -> bool {
        self.front == target.front.opposite()
            && (self.rollable || self.top == target.top)
            && target.name.is_none_or(|name| name == self.target)
    }

    /// The jigsaw rotated by `rotation` around the origin and moved to `position`
    /// (`getJigsaws(position, rotation)`; a jigsaw block rotates both its front and top).
    fn placed(&self, position: BlockPos, rotation: Rotation) -> Self {
        let pos = transform(self.pos, Mirror::None, rotation, BlockPos::ZERO);
        Self {
            pos: pos.offset(position.x, position.y, position.z),
            front: rotation.rotate(self.front),
            top: rotation.rotate(self.top),
            ..*self
        }
    }
}

/// `Util.shuffle(list, random)`.
pub fn shuffle<T, R: RandomSource + ?Sized>(list: &mut [T], random: &mut R) {
    for i in (2..=list.len()).rev() {
        let swap_to = random.next_int_bounded(i as i32) as usize;
        list.swap(i - 1, swap_to);
    }
}

/// `StructurePoolElement`: what a template pool holds.
#[derive(Debug)]
pub enum PoolElement {
    /// `single_pool_element` and `legacy_single_pool_element`.
    Single(SinglePoolElement),
    /// `list_pool_element`.
    List(ListPoolElement),
    /// `feature_pool_element`.
    Feature(FeaturePoolElement),
    /// `empty_pool_element` (`EmptyPoolElement.INSTANCE`).
    Empty,
}

impl PoolElement {
    /// Parses `StructurePoolElement.CODEC` (dispatched on `element_type`). Inside a list the
    /// list's projection replaces the element's own (`setProjectionOnEachElement`).
    pub(super) fn parse(
        json: &Value,
        projection_override: Option<Projection>,
        parser: &mut StructureParser<'_>,
        pools: &mut PoolsBuilder,
    ) -> Result<Self, Error> {
        let kind = json
            .get("element_type")
            .and_then(Value::as_str)
            .map(qualify)
            .ok_or_else(|| Error::new("pool element without element_type"))?;
        let projection = || -> Result<Projection, Error> {
            let own = field(json, "projection")?
                .as_str()
                .ok_or_else(|| Error::new("projection is not a string"))
                .and_then(Projection::from_name)?;
            Ok(projection_override.unwrap_or(own))
        };
        Ok(match kind.as_str() {
            "minecraft:empty_pool_element" => Self::Empty,
            "minecraft:single_pool_element" => Self::Single(SinglePoolElement::parse(
                json,
                false,
                projection()?,
                parser,
                pools,
            )?),
            "minecraft:legacy_single_pool_element" => Self::Single(SinglePoolElement::parse(
                json,
                true,
                projection()?,
                parser,
                pools,
            )?),
            "minecraft:feature_pool_element" => {
                let feature = field(json, "feature")?.as_str().ok_or_else(|| {
                    Error::new("inline placed features in pools are not supported")
                })?;
                let id = parser
                    .features()
                    .env
                    .registry
                    .placed_id(feature)
                    .ok_or_else(|| Error::new(format!("unknown placed feature {feature}")))?;
                Self::Feature(FeaturePoolElement {
                    feature: id,
                    feature_name: qualify(feature),
                    projection: projection()?,
                    jigsaw: Jigsaw {
                        pos: BlockPos::ZERO,
                        front: Direction::Down,
                        top: Direction::South,
                        rollable: true,
                        name: None,
                        pool: pools.empty_pool(),
                        target: pools.symbol(EMPTY_ID),
                        placement_priority: 0,
                        selection_priority: 0,
                    },
                })
            }
            "minecraft:list_pool_element" => {
                let projection = projection()?;
                let elements = field(json, "elements")?
                    .as_array()
                    .ok_or_else(|| Error::new("list pool element elements is not a list"))?
                    .iter()
                    .map(|e| Self::parse(e, Some(projection), parser, pools))
                    .collect::<Result<Vec<_>, _>>()?;
                if elements.is_empty() {
                    return Err(Error::new("list pool element: elements are empty"));
                }
                // Vanilla throws when the box of a list without a non-empty element is needed.
                if elements.iter().all(|e| matches!(e, Self::Empty)) {
                    return Err(Error::new("list pool element without a non-empty element"));
                }
                Self::List(ListPoolElement {
                    elements,
                    projection,
                })
            }
            other => return Err(Error::new(format!("unknown pool element type {other}"))),
        })
    }

    /// `getType()` as its registry id.
    pub fn element_type(&self) -> &'static str {
        match self {
            Self::Single(single) if single.legacy => "minecraft:legacy_single_pool_element",
            Self::Single(_) => "minecraft:single_pool_element",
            Self::List(_) => "minecraft:list_pool_element",
            Self::Feature(_) => "minecraft:feature_pool_element",
            Self::Empty => "minecraft:empty_pool_element",
        }
    }

    /// `StructurePoolElement.CODEC` encoded to NBT (the `pool_element` of a
    /// `PoolElementStructurePiece`'s saved data).
    pub fn save_data(&self) -> Compound {
        let element = Compound::new().with("element_type", self.element_type());
        match self {
            Self::Single(single) => {
                let mut element = element
                    .with("location", single.location.as_str())
                    .with("processors", single.saved_processors.clone())
                    .with("projection", single.projection.name());
                if let Some(liquid) = single.override_liquid_settings {
                    element.insert("override_liquid_settings", liquid.name());
                }
                element
            }
            Self::List(list) => element
                .with(
                    "elements",
                    List(
                        list.elements
                            .iter()
                            .map(|e| Tag::Compound(e.save_data()))
                            .collect(),
                    ),
                )
                .with("projection", list.projection.name()),
            Self::Feature(feature) => element
                .with("feature", feature.feature_name.as_str())
                .with("projection", feature.projection.name()),
            Self::Empty => element,
        }
    }

    /// `getProjection()`.
    pub fn projection(&self) -> Projection {
        match self {
            Self::Single(single) => single.projection,
            Self::List(list) => list.projection,
            Self::Feature(feature) => feature.projection,
            Self::Empty => Projection::TerrainMatching,
        }
    }

    /// `getGroundLevelDelta()`: 1 for every Vanilla element.
    pub fn ground_level_delta(&self) -> i32 {
        1
    }

    /// `getShuffledJigsawBlocks(manager, position, rotation, random)`, appended to `out`.
    pub fn shuffled_jigsaw_blocks<R: RandomSource + ?Sized>(
        &self,
        position: BlockPos,
        rotation: Rotation,
        random: &mut R,
        out: &mut Vec<Jigsaw>,
    ) {
        match self {
            Self::Single(single) => single.shuffled_jigsaw_blocks(position, rotation, random, out),
            Self::List(list) => {
                list.elements[0].shuffled_jigsaw_blocks(position, rotation, random, out);
            }
            Self::Feature(feature) => out.push(Jigsaw {
                pos: position,
                ..feature.jigsaw
            }),
            Self::Empty => {}
        }
    }

    /// `getBoundingBox(manager, position, rotation)`. Vanilla throws for the empty element
    /// (its callers filter it out); this returns the block at `position` for it.
    pub fn bounding_box(&self, position: BlockPos, rotation: Rotation) -> BoundingBox {
        match self {
            Self::Single(single) => {
                single
                    .template
                    .bounding_box_with(position, rotation, BlockPos::ZERO, Mirror::None)
            }
            Self::List(list) => {
                let mut boxes = list
                    .elements
                    .iter()
                    .filter(|e| !matches!(e, Self::Empty))
                    .map(|e| e.bounding_box(position, rotation));
                // Parsing guarantees a non-empty element.
                let mut bb = boxes.next().unwrap_or(BoundingBox::of_block(position));
                for other in boxes {
                    bb.encapsulate(&other);
                }
                bb
            }
            Self::Feature(_) | Self::Empty => BoundingBox::of_block(position),
        }
    }

    /// `place(manager, level, structureManager, generator, position, referencePos, rotation,
    /// chunkBB, random, liquidSettings, keepJigsaws = false)`.
    #[allow(clippy::too_many_arguments)]
    pub fn place(
        &self,
        ctx: &mut PieceContext<'_, '_>,
        position: BlockPos,
        reference: BlockPos,
        rotation: Rotation,
        chunk_bb: &BoundingBox,
        liquid_settings: LiquidSettings,
    ) -> bool {
        match self {
            Self::Single(single) => single.place(
                ctx,
                position,
                reference,
                rotation,
                chunk_bb,
                liquid_settings,
            ),
            Self::List(list) => list.elements.iter().all(|element| {
                element.place(
                    ctx,
                    position,
                    reference,
                    rotation,
                    chunk_bb,
                    liquid_settings,
                )
            }),
            Self::Feature(feature) => {
                let mut feature_ctx = FeatureContext {
                    level: &mut *ctx.level,
                    env: &ctx.generator.features().env,
                    random: &mut *ctx.random,
                };
                place_placed(&mut feature_ctx, feature.feature, position, None)
            }
            Self::Empty => true,
        }
    }

    /// The template location of single elements (`getTemplateLocation()`).
    pub fn location(&self) -> Option<&str> {
        match self {
            Self::Single(single) => Some(&single.location),
            _ => None,
        }
    }
}

/// `JigsawBlockEntity.EMPTY_ID` (and `Pools.EMPTY`).
pub const EMPTY_ID: &str = "minecraft:empty";

/// `SinglePoolElement` (`legacy`: `LegacySinglePoolElement`).
#[derive(Debug)]
pub struct SinglePoolElement {
    /// `LegacySinglePoolElement`: ignores air instead of only structure blocks.
    pub legacy: bool,
    /// `location`: the template id.
    pub location: String,
    /// `processors` as `StructureProcessorType.LIST_CODEC` writes it to NBT: the registry id,
    /// or the inline list (`{processors: [...]}`) converted from its JSON.
    saved_processors: Tag,
    pub projection: Projection,
    /// `overrideLiquidSettings`.
    pub override_liquid_settings: Option<LiquidSettings>,
    template: Arc<StructureTemplate>,
    /// The processors of `getSettings` in Vanilla's order: the block ignore processor, jigsaw
    /// replacement, the element's list, the projection's (`gravity` for terrain matching); a
    /// legacy element moves the ignore processor (now also ignoring air) to the end.
    processors: Vec<StructureProcessor>,
    /// Per palette: its jigsaw blocks (`Palette.jigsaws()`), template relative and unrotated.
    jigsaws: Vec<Vec<Jigsaw>>,
}

impl SinglePoolElement {
    fn parse(
        json: &Value,
        legacy: bool,
        projection: Projection,
        parser: &mut StructureParser<'_>,
        pools: &mut PoolsBuilder,
    ) -> Result<Self, Error> {
        let location = field(json, "location")?
            .as_str()
            .map(qualify)
            .ok_or_else(|| Error::new("pool element location is not a string"))?;
        let template = parser.template(&location)?;
        let processors_json = field(json, "processors")?;
        let list = parser.processor_list(processors_json)?;
        let override_liquid_settings = match json.get("override_liquid_settings") {
            None => None,
            Some(value) => Some(
                value
                    .as_str()
                    .and_then(LiquidSettings::from_name)
                    .ok_or_else(|| Error::new(format!("invalid liquid settings {value}")))?,
            ),
        };
        let mut processors = Vec::with_capacity(list.processors.len() + 3);
        if !legacy {
            processors.push(StructureProcessor::BlockIgnore(vec![ids::STRUCTURE_BLOCK]));
        }
        processors.push(StructureProcessor::JigsawReplacement);
        processors.extend(list.processors.iter().cloned());
        if projection == Projection::TerrainMatching {
            processors.push(StructureProcessor::Gravity {
                heightmap: vanillite_chunk::HeightmapKind::WorldSurfaceWg,
                offset: -1,
            });
        }
        if legacy {
            processors.push(StructureProcessor::BlockIgnore(vec![
                ids::AIR,
                ids::STRUCTURE_BLOCK,
            ]));
        }
        let jigsaws = template
            .palettes()
            .iter()
            .map(|palette| {
                palette
                    .jigsaws()
                    .iter()
                    .map(|info| {
                        let (front, top) = orientation(info.state).ok_or_else(|| {
                            Error::new(format!("{location}: jigsaw without orientation"))
                        })?;
                        Ok(Jigsaw {
                            pos: info.pos,
                            front,
                            top,
                            rollable: info.joint_type == JointType::Rollable,
                            name: Some(pools.symbol(&info.name)),
                            target: pools.symbol(&info.target),
                            pool: pools.named(&info.pool),
                            placement_priority: info.placement_priority,
                            selection_priority: info.selection_priority,
                        })
                    })
                    .collect::<Result<Vec<_>, Error>>()
            })
            .collect::<Result<Vec<_>, Error>>()?;
        Ok(Self {
            legacy,
            location,
            saved_processors: match processors_json.as_str() {
                Some(id) => Tag::String(qualify(id)),
                None => json_to_tag(processors_json),
            },
            projection,
            override_liquid_settings,
            template,
            processors,
            jigsaws,
        })
    }

    /// `getShuffledJigsawBlocks`: the jigsaws of the palette `getJigsaws` picks (a
    /// `LegacyRandomSource(Mth.getSeed(position))`, as settings without a random do), placed,
    /// shuffled and stably sorted by descending selection priority.
    fn shuffled_jigsaw_blocks<R: RandomSource + ?Sized>(
        &self,
        position: BlockPos,
        rotation: Rotation,
        random: &mut R,
        out: &mut Vec<Jigsaw>,
    ) {
        let palette = match self.jigsaws.len() {
            0 => return,
            1 => &self.jigsaws[0],
            n => {
                let mut palette_random =
                    LegacyRandom::new(get_seed(position.x, position.y, position.z));
                &self.jigsaws[palette_random.next_int_bounded(n as i32) as usize]
            }
        };
        let start = out.len();
        out.extend(palette.iter().map(|j| j.placed(position, rotation)));
        let jigsaws = &mut out[start..];
        shuffle(jigsaws, random);
        jigsaws.sort_by_key(|j| Reverse(j.selection_priority));
    }

    /// `SinglePoolElement.place`: the template with `getSettings(rotation, chunkBB,
    /// liquidSettings, false)` and update flags 18. The data markers it then processes reach
    /// `handleDataMarker`, which does nothing for every Vanilla element (and processing them
    /// has no effect on the level), so they are skipped.
    fn place(
        &self,
        ctx: &mut PieceContext<'_, '_>,
        position: BlockPos,
        reference: BlockPos,
        rotation: Rotation,
        chunk_bb: &BoundingBox,
        liquid_settings: LiquidSettings,
    ) -> bool {
        let mut settings = StructurePlaceSettings::new()
            .with_rotation(rotation)
            .with_bounding_box(*chunk_bb);
        settings.known_shape = true;
        settings.ignore_entities = false;
        settings.finalize_entities = true;
        settings.liquid_settings = self.override_liquid_settings.unwrap_or(liquid_settings);
        settings.add_processors(&self.processors);
        self.template.place_in_world(
            ctx.level,
            position,
            reference,
            &settings,
            &mut *ctx.random,
            18,
        )
    }
}

/// A JSON value as NBT the way `JsonOps` → `NbtOps` converts it (whole numbers as ints or
/// longs, others as doubles, booleans as bytes). Only used to write inline processor lists,
/// which are all empty in the Vanilla data pack (a non-empty list would keep its JSON field
/// order and number forms rather than the processor codecs' encoding).
fn json_to_tag(json: &Value) -> Tag {
    match json {
        Value::Null => Tag::Compound(Compound::new()),
        Value::Bool(b) => Tag::Byte(i8::from(*b)),
        Value::Number(n) => match n.as_i64() {
            Some(v) => i32::try_from(v).map_or(Tag::Long(v), Tag::Int),
            None => Tag::Double(n.as_f64().unwrap_or_default()),
        },
        Value::String(s) => Tag::String(s.clone()),
        Value::Array(items) => Tag::List(List(items.iter().map(json_to_tag).collect())),
        Value::Object(map) => {
            let mut compound = Compound::with_capacity(map.len());
            for (key, value) in map {
                compound.insert(key.as_str(), json_to_tag(value));
            }
            Tag::Compound(compound)
        }
    }
}

/// The front and top of a jigsaw block state (`orientation`, e.g. `down_south`).
fn orientation(state: BlockState) -> Option<(Direction, Direction)> {
    let (front, top) = state.value("orientation")?.split_once('_')?;
    Some((direction_by_name(front)?, direction_by_name(top)?))
}

/// `ListPoolElement`: several elements placed at the same position.
#[derive(Debug)]
pub struct ListPoolElement {
    pub elements: Vec<PoolElement>,
    pub projection: Projection,
}

/// `FeaturePoolElement`: a placed feature at the piece position.
#[derive(Debug)]
pub struct FeaturePoolElement {
    pub feature: PlacedFeatureId,
    /// The placed feature's registry id.
    pub feature_name: String,
    pub projection: Projection,
    /// The one jigsaw it exposes (front down, top south, rollable, no name, pool and target
    /// `minecraft:empty`), at the origin.
    jigsaw: Jigsaw,
}

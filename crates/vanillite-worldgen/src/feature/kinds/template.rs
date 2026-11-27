//! Features placing structure templates: `fossil` (`FossilFeature`) and `template`
//! (`TemplateFeature`).

use std::sync::Arc;

use serde_json::Value;
use vanillite_base::random::RandomSource;
use vanillite_base::{BlockPos, Direction};
use vanillite_chunk::HeightmapKind;
use vanillite_data::block::ids;

use crate::Error;
use crate::carver::providers::WeightedList;
use crate::feature::region::WorldGenRegion;
use crate::feature::{FeatureContext, FeatureParser, FeatureType, field, int_field};
use crate::template::TemplateRef;
use crate::template::{BoundingBox, Mirror, ProcessorList, Rotation, StructurePlaceSettings};

/// A list of template ids (`Identifier.CODEC.listOf()`, non-empty) loaded through the parser.
fn template_list(
    json: &Value,
    name: &str,
    parser: &FeatureParser<'_>,
) -> Result<Vec<TemplateRef>, Error> {
    let items = field(json, name)?
        .as_array()
        .ok_or_else(|| Error::new(format!("{name} is not a list")))?;
    if items.is_empty() {
        return Err(Error::new(format!("{name} must not be empty")));
    }
    items
        .iter()
        .map(|id| {
            let id = id
                .as_str()
                .ok_or_else(|| Error::new(format!("{name}: invalid id {id}")))?;
            Ok(parser.structure_template(id))
        })
        .collect()
}

/// `FossilFeature`: a buried fossil template with an ore overlay template.
#[derive(Debug)]
pub struct FossilFeature {
    /// `fossilStructures` and `overlayStructures`, pairwise.
    fossils: Vec<(TemplateRef, TemplateRef)>,
    fossil_processors: Arc<ProcessorList>,
    overlay_processors: Arc<ProcessorList>,
    max_empty_corners_allowed: i32,
}

impl FeatureType for FossilFeature {
    fn parse(json: &Value, parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        let fossils = template_list(json, "fossil_structures", parser)?;
        let overlays = template_list(json, "overlay_structures", parser)?;
        if fossils.len() != overlays.len() {
            return Err(Error::new("Fossil structure lists must be equal lengths"));
        }
        let max_empty_corners_allowed = int_field(json, "max_empty_corners_allowed")?;
        if !(0..=7).contains(&max_empty_corners_allowed) {
            return Err(Error::new(format!(
                "max_empty_corners_allowed {max_empty_corners_allowed} outside [0, 7]"
            )));
        }
        Ok(Self {
            fossils: fossils.into_iter().zip(overlays).collect(),
            fossil_processors: parser.processor_list(field(json, "fossil_processors")?)?,
            overlay_processors: parser.processor_list(field(json, "overlay_processors")?)?,
            max_empty_corners_allowed,
        })
    }

    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        let level = &mut *ctx.level;
        let random = &mut *ctx.random;
        let rotation = Rotation::random(random);
        let index = random.next_int_bounded(self.fossils.len() as i32) as usize;
        let (fossil, overlay) = &self.fossils[index];
        let (fossil, overlay) = (fossil.get(), overlay.get());
        let (chunk_x, chunk_z) = (origin.x >> 4, origin.z >> 4);
        let bounding_box = BoundingBox::new(
            (chunk_x << 4) - 16,
            level.min_y(),
            (chunk_z << 4) - 16,
            (chunk_x << 4) + 15 + 16,
            level.max_y(),
            (chunk_z << 4) + 15 + 16,
        );
        let mut settings = StructurePlaceSettings::new()
            .with_rotation(rotation)
            .with_bounding_box(bounding_box)
            .with_placement_random();
        let size = fossil.size_rotated(rotation);
        let low_corner = origin.offset(-size.x / 2, 0, -size.z / 2);
        let mut lowest_surface_y = origin.y;
        for x in 0..size.x {
            for z in 0..size.z {
                lowest_surface_y = lowest_surface_y.min(level.height_at(
                    HeightmapKind::OceanFloorWg,
                    low_corner.x + x,
                    low_corner.z + z,
                ));
            }
        }
        let target_y =
            (lowest_surface_y - 15 - random.next_int_bounded(10)).max(level.min_y() + 10);
        let target = fossil.zero_position_with_transform(
            BlockPos::new(low_corner.x, target_y, low_corner.z),
            Mirror::None,
            rotation,
        );
        if count_empty_corners(level, &fossil.bounding_box(&settings, target))
            > self.max_empty_corners_allowed
        {
            return false;
        }
        settings.clear_processors();
        settings.add_processors(&self.fossil_processors.processors);
        fossil.place_in_world(level, target, target, &settings, random, 260);
        settings.clear_processors();
        settings.add_processors(&self.overlay_processors.processors);
        overlay.place_in_world(level, target, target, &settings, random, 260);
        true
    }
}

/// `FossilFeature.countEmptyCorners`: corners of the box that are air, lava or water.
fn count_empty_corners(level: &WorldGenRegion<'_>, bounds: &BoundingBox) -> i32 {
    bounds
        .corners()
        .into_iter()
        .filter(|&pos| {
            let state = level.block(pos);
            state.is_air() || state.block() == ids::LAVA || state.block() == ids::WATER
        })
        .count() as i32
}

/// `TemplateFeature.TemplateEntry`: a template and the rotations it may be placed with.
#[derive(Debug)]
struct TemplateEntry {
    template: TemplateRef,
    rotations: Vec<Rotation>,
}

/// `TemplateFeature`: one template of a weighted list, centred on the origin.
#[derive(Debug)]
pub struct TemplateFeature {
    templates: WeightedList<TemplateEntry>,
    processors: Option<Arc<ProcessorList>>,
}

impl FeatureType for TemplateFeature {
    fn parse(json: &Value, parser: &mut FeatureParser<'_>) -> Result<Self, Error> {
        let templates = WeightedList::from_json(field(json, "templates")?, |entry| {
            let id = field(entry, "id")?
                .as_str()
                .ok_or_else(|| Error::new("template id is not a string"))?;
            let rotations = match entry.get("rotations") {
                Some(list) => list
                    .as_array()
                    .ok_or_else(|| Error::new("rotations is not a list"))?
                    .iter()
                    .map(|r| {
                        r.as_str()
                            .ok_or_else(|| Error::new(format!("invalid rotation {r}")))
                            .and_then(Rotation::from_name)
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                None => Rotation::ALL.to_vec(),
            };
            if rotations.is_empty() {
                // `Util.getRandom` on an empty list would throw while placing.
                return Err(Error::new("template rotations must not be empty"));
            }
            Ok(TemplateEntry {
                template: parser.structure_template(id),
                rotations,
            })
        })?;
        let processors = json
            .get("processors")
            .map(|p| parser.processor_list(p))
            .transpose()?;
        Ok(Self {
            templates,
            processors,
        })
    }

    fn place(&self, ctx: &mut FeatureContext<'_, '_>, origin: BlockPos) -> bool {
        let random = &mut *ctx.random;
        let entry = self.templates.get_random(random);
        let rotation =
            entry.rotations[random.next_int_bounded(entry.rotations.len() as i32) as usize];
        let template = entry.template.get();
        let size = template.size();
        // `getRotatedOffset`: the axis' negative direction, rotated, times half the size.
        let offset = |direction: Direction, half: i32| {
            let [x, y, z] = rotation.rotate(direction).normal();
            (x * half, y * half, z * half)
        };
        let (ax, ay, az) = offset(Direction::West, size.x / 2);
        let (bx, by, bz) = offset(Direction::North, size.z / 2);
        let pos = origin.offset(ax, ay, az).offset(bx, by, bz);
        let mut settings = StructurePlaceSettings::new()
            .with_rotation(rotation)
            .with_placement_random();
        if let Some(processors) = &self.processors {
            settings.add_processors(&processors.processors);
        }
        template.place_in_world(ctx.level, pos, pos, &settings, random, 3)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use vanillite_base::ChunkPos;
    use vanillite_base::random::DecorationRandom;

    use super::*;
    use crate::feature::{FeatureEnv, FeatureId};
    use crate::test_support::packs;
    use crate::{
        DimensionSpec, NeighbourBiomes, ProtoChunk, ReferencedStarts, TerrainGenerator, TerrainJob,
    };

    /// Every `fossil` and `template` configuration in the pack, also those nested inline in
    /// other features.
    fn collect(json: &Value, out: &mut Vec<Value>) {
        match json {
            Value::Object(map) => {
                if matches!(
                    map.get("type").and_then(Value::as_str),
                    Some("minecraft:fossil" | "minecraft:template")
                ) {
                    out.push(json.clone());
                }
                map.values().for_each(|v| collect(v, out));
            }
            Value::Array(items) => items.iter().for_each(|v| collect(v, out)),
            _ => {}
        }
    }

    /// The 3×3 overworld chunks around the origin after the terrain stage.
    fn terrain(generator: &TerrainGenerator) -> Vec<ProtoChunk> {
        let mut biomes: Vec<ProtoChunk> = (0..49)
            .map(|i| generator.new_chunk(ChunkPos::new(i % 7 - 3, i / 7 - 3)))
            .collect();
        generator.generate_biomes(&mut biomes);
        let at = |dx: i32, dz: i32| ((dz + 3) * 7 + dx + 3) as usize;
        let mut jobs = Vec::new();
        for dz in -1..=1 {
            for dx in -1..=1 {
                let neighbours = NeighbourBiomes::collect(|ox, oz| &biomes[at(dx + ox, dz + oz)]);
                jobs.push(TerrainJob {
                    chunk: biomes[at(dx, dz)].clone(),
                    neighbours,
                    structures: ReferencedStarts::empty(),
                });
            }
        }
        generator.generate_terrain(&mut jobs);
        jobs.into_iter().map(|j| j.chunk).collect()
    }

    #[test]
    fn every_template_feature_parses_and_places() {
        let packs = packs();
        let biome_source =
            json!({"type": "minecraft:multi_noise", "preset": "minecraft:overworld"});
        let spec = DimensionSpec {
            noise_settings: "minecraft:overworld",
            biome_source: &biome_source,
            min_y: -64,
            height: 384,
            has_sky_light: true,
        };
        let generator = TerrainGenerator::new(packs, spec, 12345).expect("generator");
        let mut configs = Vec::new();
        for entry in packs
            .registry("minecraft:worldgen/feature")
            .expect("features")
            .entries()
        {
            let json: Value = serde_json::from_str(entry.json.get()).expect("json");
            collect(&json, &mut configs);
        }
        assert!(
            configs.len() >= 5,
            "only {} template features",
            configs.len()
        );

        let mut parser = FeatureParser::new(packs, generator.biomes(), generator.random_state());
        let ids: Vec<FeatureId> = configs
            .iter()
            .map(|c| parser.feature(c).unwrap_or_else(|e| panic!("{c}: {e}")))
            .collect();
        let env = FeatureEnv {
            registry: parser.finish(),
            context: generator.features().env.context,
            biome_feature_sets: Vec::new(),
        };
        let base = terrain(&generator);
        for (config, id) in configs.iter().zip(ids) {
            let mut changed = false;
            for seed in 0..8 {
                let mut chunks = base.clone();
                let mut region = WorldGenRegion::new(
                    chunks.iter_mut().collect(),
                    1,
                    1,
                    generator.random_state(),
                    63,
                );
                let mut random = DecorationRandom::new(seed);
                let origin = BlockPos::new(4 + seed as i32, 40, 8);
                let mut ctx = FeatureContext {
                    level: &mut region,
                    env: &env,
                    random: &mut random,
                };
                ctx.place_feature(id, origin);
                drop(region);
                changed |= chunks.iter().zip(&base).any(|(a, b)| {
                    (-64..320).any(|y| {
                        (0..256).any(|i| {
                            let x = a.pos().min_block_x() + i % 16;
                            let z = a.pos().min_block_z() + i / 16;
                            a.block_or_void(x, y, z) != b.block_or_void(x, y, z)
                        })
                    })
                });
            }
            assert!(changed, "nothing placed by {config}");
        }
    }
}

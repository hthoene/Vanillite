//! Folder layouts of a world's dimensions.
//!
//! Vanilla 26.x (and Paper 26.x) keep every dimension in `<level>/dimensions/<namespace>/<path>/`.
//! Spigot and CraftBukkit 26.x keep the overworld there too, but give the nether and the end
//! world folders of their own next to it: `<level>_nether/dimensions/minecraft/the_nether/` and
//! `<level>_the_end/dimensions/minecraft/the_end/`, each with its own `level.dat`, `uid.dat` and
//! `data/`, which Spigot creates itself when they are missing. Players, `level.dat` and the
//! world-wide saved data stay in `<level>/` in both layouts.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::dimension_dir;

/// Where a world's dimensions are stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum WorldLayout {
    /// Vanilla's and Paper's: every dimension below `<level>/dimensions/`.
    #[default]
    Vanilla,
    /// Spigot's: the nether and the end in `<level>_nether/` and `<level>_the_end/`.
    Spigot,
}

impl WorldLayout {
    /// The world folder holding the `dimensions/` tree of `dimension`.
    pub fn world_dir(self, server_dir: &Path, level_name: &str, dimension: &str) -> PathBuf {
        let suffix = match (self, dimension) {
            (WorldLayout::Spigot, "minecraft:the_nether") => "_nether",
            (WorldLayout::Spigot, "minecraft:the_end") => "_the_end",
            _ => "",
        };
        server_dir.join(format!("{level_name}{suffix}"))
    }

    /// The folder of `dimension` (contains `region/`, `entities/`, `poi/`, `data/`).
    pub fn dimension_dir(self, server_dir: &Path, level_name: &str, dimension: &str) -> PathBuf {
        dimension_dir(
            &self.world_dir(server_dir, level_name, dimension),
            dimension,
        )
    }

    fn other(self) -> WorldLayout {
        match self {
            WorldLayout::Vanilla => WorldLayout::Spigot,
            WorldLayout::Spigot => WorldLayout::Vanilla,
        }
    }
}

/// Moves the dimensions of the world `level_name` that are stored in the other layout into
/// `layout`, one folder rename per dimension (atomic; a crash leaves every dimension complete in
/// one of the two places and the next start goes on). Returns one line per moved folder.
///
/// Fails without moving anything for a dimension that has data in both layouts: which copy is
/// current cannot be told, so the operator has to remove one. Spigot's per-world files
/// (`level.dat`, `uid.dat`, `data/`) are left where they are.
pub fn adopt_layout(
    server_dir: &Path,
    level_name: &str,
    layout: WorldLayout,
    dimensions: &[&str],
) -> io::Result<Vec<String>> {
    let mut moves = Vec::new();
    for &dimension in dimensions {
        let to = layout.dimension_dir(server_dir, level_name, dimension);
        let from = layout
            .other()
            .dimension_dir(server_dir, level_name, dimension);
        if from == to || !has_entries(&from)? {
            continue;
        }
        if has_entries(&to)? {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "{dimension} is stored in both world layouts, {} and {}; \
                     remove the outdated one or set storage.layout to match",
                    to.display(),
                    from.display()
                ),
            ));
        }
        moves.push((from, to));
    }
    let mut report = Vec::with_capacity(moves.len());
    for (from, to) in moves {
        if to.exists() {
            fs::remove_dir(&to)?;
        }
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(&from, &to).map_err(|e| {
            io::Error::new(
                e.kind(),
                format!("moving {} to {}: {e}", from.display(), to.display()),
            )
        })?;
        report.push(format!("moved {} to {}", from.display(), to.display()));
    }
    Ok(report)
}

/// Whether `dir` is a folder with anything in it.
fn has_entries(dir: &Path) -> io::Result<bool> {
    match fs::read_dir(dir) {
        Ok(mut entries) => Ok(entries.next().is_some()),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(false)
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use vanillite_base::ChunkPos;
    use vanillite_chunk::ProtoChunk;
    use vanillite_chunk::slots::{DimensionId, WorldInfo};
    use vanillite_nbt::{Budget, Compound};

    use super::*;
    use crate::chunk_nbt::Codec;
    use crate::region::RegionFile;
    use crate::{AnvilCodec, AnvilStore, Compression, IoPool, StoreSettings, blocking};

    fn store(info: &WorldInfo) -> AnvilStore {
        AnvilStore::new(
            info.dir.clone(),
            StoreSettings::default(),
            IoPool::new(1).unwrap(),
        )
    }

    const DIMENSIONS: [&str; 3] = [
        "minecraft:overworld",
        "minecraft:the_nether",
        "minecraft:the_end",
    ];

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vanillite-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn info(dimension: &str, dir: PathBuf) -> WorldInfo {
        vanillite_chunk::facts::install_vanilla(&[], &[], 1);
        let (min_y, height) = match dimension {
            "minecraft:overworld" => (-64, 384),
            _ => (0, 256),
        };
        WorldInfo {
            dimension: dimension.to_owned(),
            dir,
            min_y,
            height,
            seed: 0,
            has_sky_light: dimension != "minecraft:the_nether",
            biomes: Arc::from(["minecraft:plains".to_owned()]),
            default_biome: 0,
            air: vanillite_data::block::BlockState::AIR.0,
        }
    }

    /// Every chunk stored below `dimension_dir`, as raw NBT.
    fn raw_chunks(dimension_dir: &Path) -> Vec<(ChunkPos, Compound)> {
        let mut out = Vec::new();
        let Ok(files) = fs::read_dir(dimension_dir.join("region")) else {
            return out;
        };
        let mut paths: Vec<_> = files.map(|f| f.unwrap().path()).collect();
        paths.sort();
        for path in paths {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let parts: Vec<&str> = name.split('.').collect();
            let [_, rx, rz, _] = parts[..] else { continue };
            let (rx, rz): (i32, i32) = (rx.parse().unwrap(), rz.parse().unwrap());
            let mut region = RegionFile::open(&path, false).unwrap().unwrap();
            for i in 0..1024 {
                let pos = ChunkPos::new(rx * 32 + i % 32, rz * 32 + i / 32);
                if let Some((compression, data)) = region.read(pos).unwrap() {
                    let bytes = compression.decompress(&data).unwrap();
                    let root =
                        vanillite_nbt::read::read_named(&mut bytes.as_slice(), &mut Budget::file())
                            .unwrap();
                    out.push((pos, root));
                }
            }
        }
        out
    }

    /// Saves `chunks` into `storage`, loads them back and checks that the chunk NBT is the same.
    fn save_and_reload(storage: &AnvilStore, anvil: &AnvilCodec, chunks: Vec<ProtoChunk>) {
        let codec = anvil.nbt();
        let expected: Vec<Compound> = chunks.iter().map(|c| codec.write(c).unwrap()).collect();
        let positions: Vec<ChunkPos> = chunks.iter().map(|c| c.pos).collect();
        let refs: Vec<&ProtoChunk> = chunks.iter().collect();
        let saved = blocking::save(storage, anvil, DimensionId(0), &refs);
        assert!(saved.iter().all(Result::is_ok), "{saved:?}");
        blocking::commit(storage).unwrap();
        let loaded = blocking::load(storage, anvil, DimensionId(0), &positions);
        for (loaded, expected) in loaded.into_iter().zip(expected) {
            let chunk = loaded.unwrap().expect("saved chunk is missing");
            assert_eq!(codec.write(&chunk).unwrap(), expected, "{:?}", chunk.pos);
        }
    }

    #[test]
    fn spigot_layout_paths() {
        let server = Path::new("/srv");
        let dir = |d| WorldLayout::Spigot.dimension_dir(server, "world", d);
        assert_eq!(
            dir("minecraft:overworld"),
            Path::new("/srv/world/dimensions/minecraft/overworld")
        );
        assert_eq!(
            dir("minecraft:the_nether"),
            Path::new("/srv/world_nether/dimensions/minecraft/the_nether")
        );
        assert_eq!(
            dir("minecraft:the_end"),
            Path::new("/srv/world_the_end/dimensions/minecraft/the_end")
        );
        assert_eq!(
            WorldLayout::Vanilla.dimension_dir(server, "world", "minecraft:the_end"),
            Path::new("/srv/world/dimensions/minecraft/the_end")
        );
    }

    #[test]
    fn spigot_layout_round_trip() {
        let server = temp_dir("spigot-layout");
        for dimension in DIMENSIONS {
            let dir = WorldLayout::Spigot.dimension_dir(&server, "world", dimension);
            let info = info(dimension, dir.clone());
            let storage = store(&info);
            let chunks = (-2..2)
                .map(|x| {
                    let mut chunk = ProtoChunk::empty(ChunkPos::new(x, x * 7), &info);
                    chunk.inhabited_time = i64::from(x) * 100;
                    chunk
                })
                .collect();
            save_and_reload(
                &storage,
                &AnvilCodec::new(&info, Compression::Deflate),
                chunks,
            );
            assert!(dir.join("region").join("r.-1.-1.mca").is_file());
        }
        assert!(
            server
                .join("world_nether/dimensions/minecraft/the_nether/region")
                .is_dir()
        );
        assert!(
            server
                .join("world_the_end/dimensions/minecraft/the_end/region")
                .is_dir()
        );
        fs::remove_dir_all(&server).unwrap();
    }

    #[test]
    fn adopt_moves_and_refuses_ambiguous_worlds() {
        let server = temp_dir("adopt-layout");
        let region =
            |layout: WorldLayout, d: &str| layout.dimension_dir(&server, "world", d).join("region");
        for d in DIMENSIONS {
            fs::create_dir_all(region(WorldLayout::Spigot, d)).unwrap();
            fs::write(region(WorldLayout::Spigot, d).join("r.0.0.mca"), d).unwrap();
        }
        fs::write(server.join("world_nether").join("level.dat"), "spigot").unwrap();
        let moved = adopt_layout(&server, "world", WorldLayout::Vanilla, &DIMENSIONS).unwrap();
        assert_eq!(moved.len(), 2, "{moved:?}");
        for d in DIMENSIONS {
            let file = region(WorldLayout::Vanilla, d).join("r.0.0.mca");
            assert_eq!(fs::read_to_string(file).unwrap(), d);
        }
        assert!(server.join("world_nether").join("level.dat").is_file());
        assert!(
            adopt_layout(&server, "world", WorldLayout::Vanilla, &DIMENSIONS)
                .unwrap()
                .is_empty()
        );
        let moved = adopt_layout(&server, "world", WorldLayout::Spigot, &DIMENSIONS).unwrap();
        assert_eq!(moved.len(), 2, "{moved:?}");
        // The same dimension in both layouts: nothing is moved.
        fs::create_dir_all(region(WorldLayout::Vanilla, "minecraft:the_end")).unwrap();
        fs::create_dir_all(region(WorldLayout::Vanilla, "minecraft:the_nether")).unwrap();
        let err = adopt_layout(&server, "world", WorldLayout::Vanilla, &DIMENSIONS).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert!(
            region(WorldLayout::Spigot, "minecraft:the_nether")
                .join("r.0.0.mca")
                .is_file()
        );
        fs::remove_dir_all(&server).unwrap();
    }

    /// A Paper 26.x world (Vanilla layout) saved again in the Spigot layout: every chunk the
    /// codec decodes is written back the same, every tag it does not decode is kept. The world
    /// is given by `VANILLITE_TEST_WORLD` (a copy, it is only read); skipped without it.
    #[test]
    fn paper_world_resaved_in_spigot_layout() {
        let Some(world) = std::env::var_os("VANILLITE_TEST_WORLD").map(PathBuf::from) else {
            return;
        };
        let source = world.parent().unwrap();
        let level = world.file_name().unwrap().to_str().unwrap();
        let server = temp_dir("paper-to-spigot");
        let mut count = 0;
        for dimension in DIMENSIONS {
            let from = WorldLayout::Vanilla.dimension_dir(source, level, dimension);
            let to = WorldLayout::Spigot.dimension_dir(&server, level, dimension);
            let info = info(dimension, to.clone());
            let codec = Codec::new(&info);
            let raw = raw_chunks(&from);
            let chunks = raw
                .iter()
                .map(|(pos, root)| codec.read(*pos, root.clone()).unwrap())
                .collect();
            save_and_reload(
                &store(&info),
                &AnvilCodec::new(&info, Compression::Deflate),
                chunks,
            );
            let resaved: std::collections::HashMap<_, _> = raw_chunks(&to).into_iter().collect();
            for (pos, root) in &raw {
                let again = &resaved[pos];
                for (key, tag) in root.iter() {
                    if !crate::chunk_nbt::DECODED.contains(&key) {
                        assert_eq!(again.get(key), Some(tag), "{dimension} {pos:?} {key}");
                    }
                }
            }
            count += raw.len();
        }
        assert!(count > 0, "no chunks in {}", world.display());
        fs::remove_dir_all(&server).unwrap();
    }
}

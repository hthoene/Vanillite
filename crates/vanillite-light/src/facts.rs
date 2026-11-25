//! Per-state light facts: emission, dampening and the face occlusion shapes.
//!
//! Vanilla: `LightEngine.getOcclusionShape`, `isEmptyShape`, `Shapes.faceShapeOccludes`. A face
//! shape is the occlusion shape's boxes that touch that face, rasterised onto a 16 × 16 grid in
//! world axes, so the faces of two neighbouring blocks on the same plane compare directly.

use std::sync::OnceLock;

use vanillite_base::Direction;
use vanillite_data::block::{BlockState, STATE_COUNT, STATE_LIGHT, flags};

/// A face shape: 16 rows of 16 bits.
type Face = [u16; 16];

/// The light facts of all block states.
#[derive(Debug)]
pub(crate) struct LightFacts {
    /// Index into `faces` per state; 0 is the empty shape (`isEmptyShape`).
    shape: Box<[u16]>,
    /// The six faces (in `Direction` order) per distinct shape.
    faces: Vec<[Face; 6]>,
}

impl LightFacts {
    pub(crate) fn get() -> &'static LightFacts {
        static FACTS: OnceLock<LightFacts> = OnceLock::new();
        FACTS.get_or_init(LightFacts::build)
    }

    fn build() -> LightFacts {
        let mut faces: Vec<[Face; 6]> = vec![[[0; 16]; 6]];
        let mut index = vanillite_base::hash::FxHashMap::default();
        let shape = (0..STATE_COUNT)
            .map(|id| {
                let state = BlockState(u16::try_from(id).unwrap_or(0));
                if !state.has(flags::CAN_OCCLUDE)
                    || !state.has(flags::USE_SHAPE_FOR_LIGHT_OCCLUSION)
                {
                    return 0;
                }
                let face_set = Direction::ALL.map(|dir| face(state.occlusion_shape(), dir));
                *index.entry(face_set).or_insert_with(|| {
                    faces.push(face_set);
                    u16::try_from(faces.len() - 1).unwrap_or(0)
                })
            })
            .collect();
        LightFacts { shape, faces }
    }

    /// `(emission, dampening)` of a state.
    #[inline]
    pub(crate) fn light(state: u16) -> (u8, u8) {
        let packed = STATE_LIGHT.get(usize::from(state)).copied().unwrap_or(0);
        (packed >> 4, packed & 15)
    }

    /// `LightEngine.getOpacity`: at least 1.
    #[inline]
    pub(crate) fn opacity(state: u16) -> u8 {
        Self::light(state).1.max(1)
    }

    /// The occlusion shape of a state, as an index (0: none for light).
    #[inline]
    pub(crate) fn shape_of(&self, state: u16) -> usize {
        usize::from(self.shape.get(usize::from(state)).copied().unwrap_or(0))
    }

    /// Whether the shapes of both states together close the face between them
    /// (`LightEngine.shapeOccludes`); `dir` points from `from` to `to`.
    #[inline]
    pub(crate) fn occludes(&self, from: u16, to: u16, dir: Direction) -> bool {
        let (a, b) = (self.shape_of(from), self.shape_of(to));
        if a == 0 && b == 0 {
            return false;
        }
        let fa = &self.faces[a][dir as usize];
        let fb = &self.faces[b][dir.opposite() as usize];
        fa.iter().zip(fb).all(|(x, y)| x | y == u16::MAX)
    }

    /// `ChunkSkyLightSources.isEdgeOccluded`: whether sky light cannot pass straight down from
    /// `top` into `bottom`.
    #[inline]
    pub(crate) fn edge_occluded(&self, top: u16, bottom: u16) -> bool {
        Self::light(bottom).1 != 0 || self.occludes(top, bottom, Direction::Down)
    }
}

/// The face of a shape on side `dir`, in the two axes other than the face normal.
fn face(shape: &[[f64; 6]], dir: Direction) -> Face {
    let axis = match dir {
        Direction::Down | Direction::Up => 1,
        Direction::North | Direction::South => 2,
        Direction::West | Direction::East => 0,
    };
    let positive = matches!(dir, Direction::Up | Direction::South | Direction::East);
    let (u, v) = match axis {
        0 => (1, 2),
        1 => (0, 2),
        _ => (0, 1),
    };
    let cell = |value: f64| (value * 16.0).round().clamp(0.0, 16.0) as usize;
    let mut out = [0u16; 16];
    for b in shape {
        let touches = if positive {
            b[axis + 3] >= 1.0 - 1e-7
        } else {
            b[axis] <= 1e-7
        };
        if !touches {
            continue;
        }
        for row in &mut out[cell(b[v])..cell(b[v + 3])] {
            for bit in cell(b[u])..cell(b[u + 3]) {
                *row |= 1 << bit;
            }
        }
    }
    out
}

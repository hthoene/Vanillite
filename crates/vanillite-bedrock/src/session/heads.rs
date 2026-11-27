//! Player heads with a skin. Bedrock draws player heads with the default skin only; a skin needs
//! a model, so each head with a profile becomes a small player entity with a head-only model
//! ([`Skin::skull`]) at the block, like Geyser (MIT) does it. Its skin is the profile's Java skin.

use rustc_hash::FxHashMap;
use vanillite_data::block::BlockState;

use super::Session;
use crate::packets::{self as bp, id};
use crate::skin::Skin;

/// Most heads shown at once per player.
const MAX_HEADS: usize = 128;

/// Bedrock runtime ids of head entities start here (Java entity ids stay below 2^32).
const HEAD_IDS: u64 = 1 << 40;

/// A head block shown as an entity.
#[derive(Debug, Clone)]
pub(super) struct Head {
    runtime: u64,
    uuid: u128,
    state: u32,
    url: String,
    shown: bool,
}

/// Heads by block position.
pub(super) type Heads = FxHashMap<[i32; 3], Head>;

/// Position and yaw of a head entity (`SkullBlock` rotation, `WallSkullBlock` facing).
fn placement(pos: [i32; 3], state: u32) -> Option<([f32; 3], f32)> {
    let s = BlockState(u16::try_from(state).ok()?);
    if !s.is_valid() {
        return None;
    }
    let [x, y, z] = pos.map(|v| crate::num::f32_of(f64::from(v)));
    let (mut x, mut y, mut z) = (x + 0.5, y - 0.01, z + 0.5);
    let yaw = match s.value("facing") {
        Some(facing) => {
            y += 0.25;
            let (yaw, dx, dz) = match facing {
                "north" => (180.0, 0.0, 0.24),
                "south" => (0.0, 0.0, -0.24),
                "west" => (90.0, 0.24, 0.0),
                _ => (270.0, -0.24, 0.0),
            };
            x += dx;
            z += dz;
            yaw
        }
        None => {
            let r = s
                .value("rotation")
                .and_then(|r| r.parse::<u8>().ok())
                .unwrap_or(0);
            (180.0 + f32::from(r) * 22.5) % 360.0
        }
    };
    Some(([x, y, z], yaw))
}

impl Head {
    pub(super) fn state(&self) -> u32 {
        self.state
    }
}

impl Session {
    /// A player head with the `textures` value of its profile at `pos`.
    pub(super) fn head_found(&mut self, pos: [i32; 3], state: u32, textures: &str) {
        let Some((url, _)) = crate::skin::texture_of(textures) else {
            self.head_gone(pos);
            return;
        };
        if let Some(h) = self.heads.get(&pos)
            && h.url == url
            && h.state == state
        {
            return;
        }
        self.head_gone(pos);
        if self.heads.len() >= MAX_HEADS {
            return;
        }
        self.next_head = self.next_head.wrapping_add(1);
        let runtime = HEAD_IDS.wrapping_add(self.next_head);
        // A UUID of the version 2 space no real player has.
        let uuid = u128::from(runtime) | 0x0000_0000_0000_2000_8000_0000_0000_0000;
        self.heads.insert(
            pos,
            Head {
                runtime,
                uuid,
                state,
                url: url.clone(),
                shown: false,
            },
        );
        self.fetch_skin(uuid, url, false);
    }

    /// The head at `pos` is gone (another block, or no skin any more).
    pub(super) fn head_gone(&mut self, pos: [i32; 3]) {
        if let Some(h) = self.heads.remove(&pos)
            && h.shown
        {
            self.send(
                id::REMOVE_ACTOR,
                &bp::remove_actor(i64::try_from(h.runtime).unwrap_or(0)),
            );
        }
    }

    /// Removes every head (dimension change).
    pub(super) fn clear_heads(&mut self) {
        let positions: Vec<[i32; 3]> = self.heads.keys().copied().collect();
        for pos in positions {
            self.head_gone(pos);
        }
    }

    /// A loaded skin for a head entity: spawns it (`false`: not a head's skin).
    pub(super) fn head_skin(&mut self, uuid: u128, skin: &Skin) -> bool {
        let Some((&pos, head)) = self.heads.iter_mut().find(|(_, h)| h.uuid == uuid) else {
            return false;
        };
        if head.shown {
            return true;
        }
        head.shown = true;
        let (runtime, state) = (head.runtime, head.state);
        let Some((at, yaw)) = placement(pos, state) else {
            return true;
        };
        let skull = skin.skull();
        let entity_id = i64::try_from(runtime).unwrap_or(0);
        let entry = bp::ListEntry {
            uuid,
            entity_id,
            name: "",
            xuid: "",
            skin: &skull,
        };
        self.send(
            id::PLAYER_LIST,
            &bp::player_list_add(&[entry], self.version),
        );
        let body = bp::add_player(
            &bp::AddPlayer {
                uuid,
                name: "",
                entity_id,
                pos: at,
                pitch: 0.0,
                yaw,
                head_yaw: yaw,
                game_mode: 0,
                sneaking: false,
                skull: true,
            },
            self.version,
        );
        self.send(id::ADD_PLAYER, &body);
        self.send(id::PLAYER_LIST, &bp::player_list_remove(&[uuid]));
        true
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]
mod tests {
    use super::*;

    #[test]
    fn heads_sit_on_their_block() {
        let state = |name: &str, k: &str, v: &str| {
            let b = vanillite_data::block::BlockId::by_name(name).unwrap();
            u32::from(b.default_state().with(k, v).unwrap().0)
        };
        let floor = state("minecraft:player_head", "rotation", "4");
        assert_eq!(placement([1, 2, 3], floor), Some(([1.5, 1.99, 3.5], 270.0)));
        let wall = state("minecraft:player_wall_head", "facing", "north");
        let (at, yaw) = placement([0, 0, 0], wall).unwrap();
        assert_eq!(yaw, 180.0);
        assert!((at[2] - 0.74).abs() < 1e-6 && (at[1] - 0.24).abs() < 1e-6);
    }
}

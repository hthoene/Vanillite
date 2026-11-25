//! Riding: Java passengers as Bedrock entity links, the movement keys, the vehicle the client
//! steers (boats, horses and the like move where the client predicts them, like on Java) and
//! leaving a vehicle.

use vanillite_protocol::packets::play::clientbound as pcb;
use vanillite_protocol::packets::play::serverbound as psb;

use super::{Session, uid};
use crate::num::f32_of;
use crate::packets::{self as bp, id, input};

/// Most passengers of one vehicle tracked (Vanilla vehicles carry at most a few).
const MAX_PASSENGERS: usize = 16;

/// The Java movement keys of a `player_auth_input`.
pub(super) fn keys(i: &bp::AuthInput, jump: bool, shift: bool, sprint: bool) -> psb::Input {
    const DEAD_ZONE: f32 = 0.01;
    let [x, y] = i.move_vector;
    psb::Input {
        forward: y > DEAD_ZONE
            || i.has(input::UP)
            || i.has(input::UP_LEFT)
            || i.has(input::UP_RIGHT),
        backward: y < -DEAD_ZONE || i.has(input::DOWN),
        left: x > DEAD_ZONE || i.has(input::LEFT) || i.has(input::UP_LEFT),
        right: x < -DEAD_ZONE || i.has(input::RIGHT) || i.has(input::UP_RIGHT),
        jump,
        shift,
        sprint,
    }
}

/// Whether the client moves a vehicle of this Java type itself (`isControlledByClient`
/// vehicles: boats and rafts, horses and the other steerable mounts).
fn client_moved(java: &str) -> bool {
    let base = java.strip_prefix("minecraft:").unwrap_or(java);
    base.ends_with("_boat")
        || base.ends_with("_raft")
        || matches!(
            base,
            "horse"
                | "donkey"
                | "mule"
                | "skeleton_horse"
                | "zombie_horse"
                | "camel"
                | "camel_husk"
                | "pig"
                | "strider"
                | "happy_ghast"
                | "nautilus"
                | "zombie_nautilus"
        )
}

fn is_boat(java: &str) -> bool {
    java.ends_with("_boat") || java.ends_with("_raft")
}

impl Session {
    /// Bedrock unique id of a Java entity id (the own player keeps its first id).
    fn unique(&self, java: i32) -> i64 {
        match &self.world {
            Some(w) if java == w.java_id => uid(w.entity_id),
            _ => uid(java),
        }
    }

    /// Whether the own player rides something.
    pub(super) fn riding(&self) -> bool {
        self.world.as_ref().is_some_and(|w| w.vehicle.is_some())
    }

    /// `set_passengers`: links to add and remove; the first passenger steers.
    pub(super) fn java_passengers(&mut self, vehicle: i32, mut passengers: Vec<i32>) {
        passengers.truncate(MAX_PASSENGERS);
        let Some(w) = &mut self.world else {
            return;
        };
        let old = if passengers.is_empty() {
            w.passengers.remove(&vehicle)
        } else {
            w.passengers.insert(vehicle, passengers.clone())
        }
        .unwrap_or_default();
        let own = w.java_id;
        if passengers.contains(&own) {
            w.vehicle = Some(vehicle);
        } else if w.vehicle == Some(vehicle) {
            w.vehicle = None;
            w.predicted_vehicle = None;
        }
        let mut links: Vec<(i32, u8)> = old
            .iter()
            .filter(|p| !passengers.contains(p))
            .map(|&p| (p, 0))
            .collect();
        for (i, &p) in passengers.iter().enumerate() {
            let kind = if i == 0 { 1 } else { 2 };
            let old_kind = old
                .iter()
                .position(|&o| o == p)
                .map(|j| if j == 0 { 1 } else { 2 });
            if old_kind != Some(kind) {
                links.push((p, kind));
            }
        }
        for &(p, kind) in &links {
            if let Some(e) = w.entities.get_mut(&p) {
                e.riding = kind != 0;
            }
        }
        let ridden = self.unique(vehicle);
        for (p, kind) in links {
            let rider = self.unique(p);
            self.send(
                id::SET_ACTOR_LINK,
                &bp::set_actor_link(ridden, rider, kind, false),
            );
            if p != own {
                self.send_actor_flags(p);
            }
        }
    }

    /// The server's position of the vehicle the client moves (a correction).
    pub(super) fn java_move_vehicle(&mut self, m: &pcb::MoveVehicleBack) {
        let Some(vehicle) = self.world.as_ref().and_then(|w| w.vehicle) else {
            return;
        };
        let pos = [m.position.x, m.position.y, m.position.z].map(f32_of);
        let runtime = self.actor(vehicle);
        self.send(
            id::MOVE_ACTOR_ABSOLUTE,
            &bp::move_actor_absolute(runtime, false, true, pos, [m.x_rot, m.y_rot, m.y_rot]),
        );
    }

    /// Whether the server's moves of `java` are left out (the client moves it itself).
    pub(super) fn client_moves(&self, java: i32) -> bool {
        self.world
            .as_ref()
            .is_some_and(|w| w.predicted_vehicle == Some(java))
    }

    /// While riding: the steered vehicle's position and paddles from the client's prediction.
    pub(super) fn steer_vehicle(&mut self, i: &bp::AuthInput, rotation: Option<[f32; 2]>) {
        let Some(w) = &mut self.world else {
            return;
        };
        let Some(vehicle) = w.vehicle else {
            return;
        };
        let steers = w
            .passengers
            .get(&vehicle)
            .and_then(|p| p.first())
            .is_some_and(|&p| p == w.java_id);
        let java = w
            .entities
            .get(&vehicle)
            .map_or("", |e| super::play::java_type_name(e.java_type));
        let predicted = steers && client_moved(java) && i.has(input::CLIENT_PREDICTED_VEHICLE);
        w.predicted_vehicle = predicted.then_some(vehicle);
        let (true, Some([pitch, yaw])) = (predicted, rotation) else {
            return;
        };
        let boat = is_boat(java);
        // In a predicted vehicle the input position is the vehicle's.
        let pos = i.position.map(f64::from);
        if let Some(e) = w.entities.get_mut(&vehicle) {
            e.pos = pos;
        }
        self.java_send(&psb::MoveVehicle {
            x: pos[0],
            y: pos[1],
            z: pos[2],
            // Bedrock boats face 90 degrees off the Java ones.
            y_rot: if boat { yaw - 90.0 } else { yaw },
            x_rot: pitch,
            on_ground: i.has(input::VERTICAL_COLLISION),
        });
        if boat {
            self.java_send(&psb::PaddleBoat {
                left: i.has(input::PADDLING_LEFT),
                right: i.has(input::PADDLING_RIGHT),
            });
        }
    }

    /// `interact` leave vehicle: Java dismounts with the sneak key.
    pub(super) fn leave_vehicle(&mut self) {
        let Some(w) = &mut self.world else {
            return;
        };
        if w.vehicle.is_none() {
            return;
        }
        let keys = psb::Input {
            shift: true,
            ..w.input
        };
        w.input = keys;
        self.java_send(&psb::PlayerInput(keys));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steerable_vehicles() {
        assert!(client_moved("minecraft:oak_boat"));
        assert!(client_moved("minecraft:bamboo_chest_raft"));
        assert!(client_moved("minecraft:horse"));
        assert!(!client_moved("minecraft:minecart"));
        assert!(!client_moved("minecraft:llama"));
        assert!(is_boat("minecraft:spruce_chest_boat"));
    }
}
